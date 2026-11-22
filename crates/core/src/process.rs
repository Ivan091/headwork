//! Which process a recorded pid still names.
//!
//! Windows reuses process ids, so a pid stored hours ago may now belong to an unrelated program. A job records the
//! identity of its process (image name and start time) when the pid is stored, and every later check compares it with
//! what runs under that pid now. Only a process that matches may be signalled. A session records the same for its
//! launcher and its `claude` ([`RecordedPid`]).

use std::path::Path;

use serde::{Deserialize, Serialize};

/// What tells one process apart from a later one that got the same pid.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProcessIdentity {
    /// The program image, such as `bash.exe`.
    pub image: String,
    /// When it started, in seconds since the unix epoch.
    pub started: u64,
}

/// Start times this far apart, in seconds, still name the same process: they come from one clock, rounded.
pub const START_SLACK_S: u64 = 1;

/// Programs a registered job may run under when its own program is not the one holding the pid: the shells and
/// launchers that wrap the real work.
pub const JOB_HOSTS: [&str; 7] = ["bash", "sh", "cargo", "python", "python3", "py", "uv"];

/// What runs under a recorded pid now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Liveness {
    /// The recorded process still runs.
    Alive,
    /// No process has the pid.
    Gone,
    /// Another program holds the pid; it must never be signalled.
    Reused {
        /// What runs under the pid now.
        now: ProcessIdentity,
    },
}

impl Liveness {
    /// Whether the recorded process still runs.
    #[must_use]
    pub const fn is_alive(&self) -> bool {
        matches!(self, Self::Alive)
    }

    /// Why the recorded process counts as over, for the job's note; `None` while it runs.
    #[must_use]
    pub fn ended_note(&self, pid: u32) -> Option<String> {
        match self {
            Self::Alive => None,
            Self::Gone => Some("the process ended".to_owned()),
            Self::Reused { now } => Some(format!(
                "the process ended; pid {pid} now belongs to another program ({}), left untouched",
                now.image
            )),
        }
    }
}

/// Judges a recorded pid by what runs under it now.
///
/// With a recorded identity the image and the start time must both match. A job recorded before identities were kept
/// (`recorded` absent) is judged conservatively by its command line: the process counts as the job's only when its
/// image is the job's own program or one of [`JOB_HOSTS`].
#[must_use]
pub fn judge(
    recorded: Option<&ProcessIdentity>,
    argv: &[String],
    current: Option<ProcessIdentity>,
) -> Liveness {
    let Some(now) = current else {
        return Liveness::Gone;
    };
    let same = recorded.map_or_else(
        || plausible_for(argv, &now.image),
        |then| {
            same_image(&then.image, &now.image)
                && then.started.abs_diff(now.started) <= START_SLACK_S
        },
    );
    if same {
        Liveness::Alive
    } else {
        Liveness::Reused { now }
    }
}

/// A process a session recorded: its pid, the identity read when it was recorded, and when that was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedPid {
    /// The process id.
    pub pid: u32,
    /// Its image and start time, read when the pid was recorded; absent for a session recorded before identities were
    /// kept, or when the process could not be read.
    pub identity: Option<ProcessIdentity>,
    /// When the pid was recorded, in seconds since the unix epoch.
    pub recorded_at: Option<u64>,
}

impl RecordedPid {
    /// Judges what runs under the pid now.
    ///
    /// With an identity the image and the start time must both match, as for a job. Without one the process counts as
    /// the recorded one only when it started no later than the pid was recorded: the recorded process was running
    /// then, so a program that took its pid over started after. A start time that reads 0 (unknown), or a pid with no
    /// record time, never counts: such a process is left alone.
    #[must_use]
    pub fn judge(&self, current: Option<ProcessIdentity>) -> Liveness {
        if self.identity.is_some() {
            return judge(self.identity.as_ref(), &[], current);
        }
        let Some(now) = current else {
            return Liveness::Gone;
        };
        let started_before = self
            .recorded_at
            .is_some_and(|at| now.started != 0 && now.started <= at.saturating_add(START_SLACK_S));
        if started_before {
            Liveness::Alive
        } else {
            Liveness::Reused { now }
        }
    }
}

fn stem(image: &str) -> String {
    let name = image.rsplit(['/', '\\']).next().unwrap_or(image);
    Path::new(name).file_stem().map_or_else(
        || name.to_ascii_lowercase(),
        |s| s.to_string_lossy().to_ascii_lowercase(),
    )
}

fn same_image(a: &str, b: &str) -> bool {
    stem(a) == stem(b)
}

fn plausible_for(argv: &[String], image: &str) -> bool {
    let image = stem(image);
    argv.first().is_some_and(|program| stem(program) == image)
        || JOB_HOSTS.contains(&image.as_str())
}

#[cfg(test)]
mod tests {
    use super::{Liveness, ProcessIdentity, RecordedPid, judge};

    fn id(image: &str, started: u64) -> ProcessIdentity {
        ProcessIdentity {
            image: image.to_owned(),
            started,
        }
    }

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|&w| w.to_owned()).collect()
    }

    #[test]
    fn a_recorded_identity_must_match_image_and_start() {
        let then = id("bash.exe", 1000);
        let cases = [
            (None, Liveness::Gone),
            (Some(id("bash.exe", 1000)), Liveness::Alive),
            (Some(id("BASH.EXE", 1001)), Liveness::Alive),
            (
                Some(id("bash.exe", 5000)),
                Liveness::Reused {
                    now: id("bash.exe", 5000),
                },
            ),
            (
                Some(id("firefox.exe", 1000)),
                Liveness::Reused {
                    now: id("firefox.exe", 1000),
                },
            ),
        ];
        for (current, expected) in cases {
            assert_eq!(
                judge(Some(&then), &argv(&["bash"]), current.clone()),
                expected,
                "{current:?}"
            );
        }
    }

    #[test]
    fn a_job_without_identity_counts_only_under_its_own_program_or_a_host() {
        let job = argv(&["scripts/cars-rl.exe", "--out", "x"]);
        assert!(
            judge(None, &job, Some(id("cars-rl.exe", 1))).is_alive(),
            "own program"
        );
        assert!(
            judge(None, &job, Some(id("bash.exe", 1))).is_alive(),
            "a shell host"
        );
        assert_eq!(
            judge(None, &job, Some(id("firefox.exe", 1))),
            Liveness::Reused {
                now: id("firefox.exe", 1)
            },
            "an unrelated program"
        );
        assert_eq!(judge(None, &job, None), Liveness::Gone, "nothing runs");
        assert!(
            judge(
                None,
                &argv(&["C:/Program Files/Git/bin/bash.exe", "-c", "x"]),
                Some(id("bash.exe", 1))
            )
            .is_alive(),
            "a full path"
        );
    }

    #[test]
    fn the_note_names_the_program_that_took_the_pid() {
        assert_eq!(Liveness::Alive.ended_note(7), None, "alive");
        assert_eq!(
            Liveness::Gone.ended_note(7).as_deref(),
            Some("the process ended"),
            "gone"
        );
        let note = Liveness::Reused {
            now: id("firefox.exe", 1),
        }
        .ended_note(38260)
        .unwrap_or_default();
        assert!(
            note.contains("38260") && note.contains("firefox.exe"),
            "{note}"
        );
    }

    #[test]
    fn a_recorded_session_pid_is_judged_by_identity_else_by_start_before_the_record() {
        let known = RecordedPid {
            pid: 7,
            identity: Some(id("claude.exe", 1000)),
            recorded_at: Some(1002),
        };
        let bare = RecordedPid {
            identity: None,
            ..known
        };
        let untimed = RecordedPid {
            recorded_at: None,
            ..bare.clone()
        };
        let reused = |image: &str, started| Liveness::Reused {
            now: id(image, started),
        };
        let cases = [
            (&known, None, Liveness::Gone),
            (&known, Some(id("claude.exe", 1000)), Liveness::Alive),
            (
                &known,
                Some(id("claude.exe", 900)),
                reused("claude.exe", 900),
            ),
            (&known, Some(id("wmux.exe", 1000)), reused("wmux.exe", 1000)),
            (&bare, None, Liveness::Gone),
            (&bare, Some(id("anything.exe", 990)), Liveness::Alive),
            (&bare, Some(id("anything.exe", 1003)), Liveness::Alive),
            (&bare, Some(id("wmux.exe", 1004)), reused("wmux.exe", 1004)),
            (&bare, Some(id("idle.exe", 0)), reused("idle.exe", 0)),
            (
                &untimed,
                Some(id("claude.exe", 990)),
                reused("claude.exe", 990),
            ),
            (&untimed, None, Liveness::Gone),
        ];
        for (record, current, expected) in cases {
            assert_eq!(
                record.judge(current.clone()),
                expected,
                "{record:?} {current:?}"
            );
        }
    }
}
