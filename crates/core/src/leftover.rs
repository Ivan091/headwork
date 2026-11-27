//! Remote Control leftovers: ended sessions that did not leave through `/exit`.
//!
//! Every session runs `claude --remote-control <name>`. Claude Code archives the session in the Claude app only when it
//! exits on `/exit`; a killed one stays listed there as unreachable, and nothing but the user can archive it. This
//! module is the pure part: the facts known about an ended session, the verdict on how it ended, and the selection of
//! the leftovers still to list (`docs/decisions/2026-11-27-rc-leftovers.md`).

use std::fmt;

use serde::Serialize;

/// The `SessionEnd` reason Claude Code gives when the user typed `/exit`.
pub const GRACEFUL_END_REASON: &str = "prompt_input_exit";

/// What the event log holds about how a session's exit went.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExitTrail {
    /// A `session-end` event with the reason `prompt_input_exit` exists.
    pub exited_on_command: bool,
    /// An `exit-asked` event exists.
    pub asked: bool,
    /// An `exit-ignored` event exists.
    pub ignored: bool,
}

/// What the store knows about one ended session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndedFacts {
    /// The Claude session id.
    pub session: String,
    /// The Remote Control name.
    pub rc_name: String,
    /// The task name, none for the orchestrator.
    pub task: Option<String>,
    /// The role word.
    pub role: String,
    /// When the session ended (or was last seen), as the store writes times.
    pub ended_at: String,
    /// The exit code of `claude`, when known.
    pub exit_code: Option<i32>,
    /// What the event log says about the exit.
    pub trail: ExitTrail,
    /// An `rc-archived` event exists: the user archived it in the app.
    pub archived: bool,
}

/// Why a session counts as a leftover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// `/exit` was typed and `claude` ignored it, so it was killed.
    ExitIgnored,
    /// `claude` ended with a failing code and nobody asked it to exit.
    EndedWithoutExit,
    /// No exit code and no graceful end: a crash, a restart, or a session found gone.
    EndUnknown,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ExitIgnored => "killed after /exit was ignored",
            Self::EndedWithoutExit => "ended without /exit",
            Self::EndUnknown => "end unknown",
        })
    }
}

/// How an ended session left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// It exited and Claude Code archived its Remote Control session.
    Graceful,
    /// Its Remote Control session may still be listed in the app.
    Leftover(Reason),
}

/// The verdict on one ended session: graceful when a `SessionEnd` came from `/exit`, or `claude` exited with code 0 and
/// was not killed after ignoring `/exit`.
#[must_use]
pub fn classify(facts: &EndedFacts) -> Verdict {
    if facts.trail.exited_on_command || (facts.exit_code == Some(0) && !facts.trail.ignored) {
        return Verdict::Graceful;
    }
    if facts.trail.ignored {
        return Verdict::Leftover(Reason::ExitIgnored);
    }
    match facts.exit_code {
        Some(code) if code != 0 && !facts.trail.asked => {
            Verdict::Leftover(Reason::EndedWithoutExit)
        }
        Some(_) | None => Verdict::Leftover(Reason::EndUnknown),
    }
}

/// A leftover to list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Leftover {
    /// The Claude session id.
    pub session: String,
    /// The Remote Control name, as the app lists it.
    pub rc_name: String,
    /// The task name, none for the orchestrator.
    pub task: Option<String>,
    /// The role word.
    pub role: String,
    /// When it ended.
    pub ended_at: String,
    /// Why it is a leftover.
    pub reason: Reason,
}

/// The leftovers not yet archived, newest first; with `since`, only those that ended at or after that instant.
#[must_use]
pub fn select(facts: &[EndedFacts], since: Option<&str>) -> Vec<Leftover> {
    let mut found: Vec<Leftover> = facts
        .iter()
        .filter(|fact| !fact.archived)
        .filter(|fact| since.is_none_or(|since| fact.ended_at.as_str() >= since))
        .filter_map(|fact| match classify(fact) {
            Verdict::Graceful => None,
            Verdict::Leftover(reason) => Some(Leftover {
                session: fact.session.clone(),
                rc_name: fact.rc_name.clone(),
                task: fact.task.clone(),
                role: fact.role.clone(),
                ended_at: fact.ended_at.clone(),
                reason,
            }),
        })
        .collect();
    found.sort_by(|a, b| {
        b.ended_at
            .cmp(&a.ended_at)
            .then_with(|| a.session.cmp(&b.session))
    });
    found
}

/// The seconds a window of `days` days spans; none for 0, which means all time.
#[must_use]
pub fn window_seconds(days: u32) -> Option<u64> {
    (days > 0).then(|| u64::from(days) * 86_400)
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]
mod tests {
    use super::*;

    fn facts(session: &str, ended_at: &str) -> EndedFacts {
        EndedFacts {
            session: session.to_owned(),
            rc_name: format!("rc-{session}"),
            task: Some("headwork-demo".to_owned()),
            role: "worker".to_owned(),
            ended_at: ended_at.to_owned(),
            exit_code: None,
            trail: ExitTrail::default(),
            archived: false,
        }
    }

    #[test]
    fn classification_table() {
        let cases = [
            (
                "exit on command",
                true,
                Some(1),
                false,
                false,
                Verdict::Graceful,
            ),
            ("clean code", false, Some(0), true, false, Verdict::Graceful),
            (
                "clean code unasked",
                false,
                Some(0),
                false,
                false,
                Verdict::Graceful,
            ),
            (
                "ignored then killed",
                false,
                Some(1),
                true,
                true,
                Verdict::Leftover(Reason::ExitIgnored),
            ),
            (
                "ignored but code 0",
                false,
                Some(0),
                true,
                true,
                Verdict::Leftover(Reason::ExitIgnored),
            ),
            (
                "failing code, unasked",
                false,
                Some(1),
                false,
                false,
                Verdict::Leftover(Reason::EndedWithoutExit),
            ),
            (
                "failing code, asked",
                false,
                Some(1),
                true,
                false,
                Verdict::Leftover(Reason::EndUnknown),
            ),
            (
                "no code",
                false,
                None,
                false,
                false,
                Verdict::Leftover(Reason::EndUnknown),
            ),
        ];
        for (name, command, code, asked, ignored, want) in cases {
            let mut fact = facts("a", "2026-11-20T00:00:00.000Z");
            fact.trail = ExitTrail {
                exited_on_command: command,
                asked,
                ignored,
            };
            fact.exit_code = code;
            assert_eq!(classify(&fact), want, "{name}");
        }
    }

    #[test]
    fn select_drops_graceful_archived_and_old_ones() {
        let mut graceful = facts("g", "2026-11-26T00:00:00.000Z");
        graceful.exit_code = Some(0);
        let mut archived = facts("a", "2026-11-26T00:00:00.000Z");
        archived.archived = true;
        let old = facts("o", "2026-11-01T00:00:00.000Z");
        let recent = facts("r", "2026-11-25T00:00:00.000Z");
        let newest = facts("n", "2026-11-26T00:00:01.000Z");
        let all = [graceful, archived, old, recent, newest];
        let names =
            |since| -> Vec<String> { select(&all, since).into_iter().map(|l| l.session).collect() };
        assert_eq!(names(None), ["n", "r", "o"], "all time, newest first");
        assert_eq!(
            names(Some("2026-11-20T00:00:00.000Z")),
            ["n", "r"],
            "window"
        );
        assert_eq!(select(&all, None).len(), 3, "count");
        assert_eq!(
            select(&all, None)
                .first()
                .map(|l| l.reason.to_string())
                .unwrap(),
            "end unknown",
            "reason text"
        );
    }

    #[test]
    fn a_window_of_zero_days_is_all_time() {
        assert_eq!(window_seconds(0), None, "all time");
        assert_eq!(window_seconds(7), Some(604_800), "a week");
    }
}
