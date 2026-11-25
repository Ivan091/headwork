//! Migration notes: the files under a repository's `migrations/` that tell another machine what to do after a change
//! (`docs/migrations.md`).
//!
//! This module parses and checks a note, decides where it applies and which notes a machine has not handled yet. It
//! reads no file and runs nothing; the binary fetches the notes, runs the steps and keeps the record.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Commands that never belong in an `agent` step, which runs without asking.
pub const AGENT_BANNED: &[&str] = &[
    "login",
    "secret",
    "token",
    "schtasks",
    "launchctl",
    "sudo",
    "tick --install",
    "tick --uninstall",
    "--settings",
    "push --force",
    "push -f",
    "reset --hard",
    "rm -r",
];

/// The text of a note that stands for the machine's name.
pub const MACHINE_PLACEHOLDER: &str = "{machine}";

/// Who runs a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    /// The agent, without asking; idempotent.
    Agent,
    /// The agent, after the user's go and a backup.
    Confirm,
    /// The user; the agent only says what to do.
    User,
}

impl StepKind {
    /// The word the note file uses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Confirm => "confirm",
            Self::User => "user",
        }
    }
}

impl fmt::Display for StepKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One step of a note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// Who runs it.
    pub kind: StepKind,
    /// The command, for `agent` and `confirm` steps.
    #[serde(default)]
    pub run: Option<String>,
    /// A command that saves what the step changes, run before a `confirm` step.
    #[serde(default)]
    pub backup: Option<String>,
    /// What the user is to do, for a `user` step.
    #[serde(default)]
    pub say: Option<String>,
}

/// A migration note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Note {
    /// The note's id, which is its file name without `.toml`.
    pub id: String,
    /// One line for lists.
    pub title: String,
    /// The release or commit that brings the change.
    #[serde(default)]
    pub since: Option<String>,
    /// Operating systems the note applies to (`windows`, `macos`, `linux`); empty means all.
    #[serde(default)]
    pub os: Vec<String>,
    /// Machine names the note applies to; empty means every machine.
    #[serde(default)]
    pub machines: Vec<String>,
    /// Projects whose main checkout the machine must have.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Why the change needs a step.
    #[serde(default)]
    pub why: String,
    /// A command that exits 0 when the note is already applied.
    #[serde(default)]
    pub check: Option<String>,
    /// The steps, in order.
    #[serde(default)]
    pub step: Vec<Step>,
}

/// A note file that does not parse.
#[derive(Debug, thiserror::Error)]
#[error("not a valid migration note: {0}")]
pub struct NoteError(#[from] toml::de::Error);

impl Note {
    /// Parses a note file.
    ///
    /// # Errors
    ///
    /// Fails when the text is not TOML of the note's shape.
    pub fn parse(text: &str) -> Result<Self, NoteError> {
        Ok(toml::from_str(text)?)
    }

    /// The note's steps with `{machine}` replaced by `machine` in each command and message.
    #[must_use]
    pub fn steps_for(&self, machine: &str) -> Vec<Step> {
        let expand = |text: &Option<String>| {
            text.as_ref()
                .map(|t| t.replace(MACHINE_PLACEHOLDER, machine))
        };
        self.step
            .iter()
            .map(|step| Step {
                kind: step.kind,
                run: expand(&step.run),
                backup: expand(&step.backup),
                say: expand(&step.say),
            })
            .collect()
    }
}

/// What is wrong with a note.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Problem {
    /// The id differs from the file name.
    #[error("id `{id}` differs from the file name `{stem}`")]
    IdMismatch {
        /// The id in the note.
        id: String,
        /// The file name without its extension.
        stem: String,
    },
    /// Two notes share an id.
    #[error("id `{0}` is used by more than one note")]
    DuplicateId(String),
    /// The id or the title is empty.
    #[error("`{0}` is empty")]
    Empty(&'static str),
    /// A note without a step.
    #[error("the note has no step")]
    NoSteps,
    /// An `agent` or `confirm` step without `run`.
    #[error("step {step}: a {kind} step needs `run`")]
    MissingRun {
        /// The step, counted from 1.
        step: usize,
        /// Its kind.
        kind: StepKind,
    },
    /// A `user` step without `say`.
    #[error("step {step}: a user step needs `say`")]
    MissingSay {
        /// The step, counted from 1.
        step: usize,
    },
    /// A field that the step's kind does not use.
    #[error("step {step}: a {kind} step takes no `{field}`")]
    Unused {
        /// The step, counted from 1.
        step: usize,
        /// Its kind.
        kind: StepKind,
        /// The field.
        field: &'static str,
    },
    /// An `agent` step with a command that needs the user.
    #[error(
        "step {step}: an agent step must not contain `{pattern}`; make it a confirm or user step"
    )]
    BannedInAgent {
        /// The step, counted from 1.
        step: usize,
        /// The banned text.
        pattern: &'static str,
    },
}

/// Checks one note against the name of the file it was read from.
#[must_use]
pub fn validate(note: &Note, stem: &str) -> Vec<Problem> {
    let mut problems = Vec::new();
    if note.id != stem {
        problems.push(Problem::IdMismatch {
            id: note.id.clone(),
            stem: stem.to_owned(),
        });
    }
    if note.id.trim().is_empty() {
        problems.push(Problem::Empty("id"));
    }
    if note.title.trim().is_empty() {
        problems.push(Problem::Empty("title"));
    }
    if note.step.is_empty() {
        problems.push(Problem::NoSteps);
    }
    for (index, step) in note.step.iter().enumerate() {
        step_problems(index + 1, step, &mut problems);
    }
    problems
}

fn step_problems(number: usize, step: &Step, problems: &mut Vec<Problem>) {
    let kind = step.kind;
    let filled = |text: &Option<String>| text.as_deref().is_some_and(|t| !t.trim().is_empty());
    let mut unused = |field: &'static str| {
        problems.push(Problem::Unused {
            step: number,
            kind,
            field,
        });
    };
    let mut missing = Vec::new();
    match kind {
        StepKind::Agent | StepKind::Confirm => {
            if !filled(&step.run) {
                missing.push(Problem::MissingRun { step: number, kind });
            }
            if step.say.is_some() {
                unused("say");
            }
        }
        StepKind::User => {
            if !filled(&step.say) {
                missing.push(Problem::MissingSay { step: number });
            }
            if step.run.is_some() {
                unused("run");
            }
            if step.backup.is_some() {
                unused("backup");
            }
        }
    }
    if kind == StepKind::Agent && step.backup.is_some() {
        unused("backup");
    }
    problems.extend(missing);
    if kind == StepKind::Agent {
        let command = step.run.as_deref().unwrap_or_default().to_lowercase();
        for pattern in AGENT_BANNED {
            if command.contains(pattern) {
                problems.push(Problem::BannedInAgent {
                    step: number,
                    pattern,
                });
            }
        }
    }
}

/// The ids used by more than one note of the set.
#[must_use]
pub fn duplicate_ids<'a>(ids: impl IntoIterator<Item = &'a str>) -> Vec<Problem> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for id in ids {
        *counts.entry(id).or_default() += 1;
    }
    counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(id, _)| Problem::DuplicateId(id.to_owned()))
        .collect()
}

/// What a note's filters are compared with.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Machine {
    /// The operating system, as [`std::env::consts::OS`] spells it.
    pub os: String,
    /// The machine's name from `sources.toml`, when it is synced.
    pub name: Option<String>,
    /// The projects whose main checkout this machine has.
    pub projects: Vec<String>,
}

impl Note {
    /// Whether the note's `os`, `machines` and `requires` filters let it apply on `machine`.
    #[must_use]
    pub fn applies_to(&self, machine: &Machine) -> bool {
        let os = self.os.is_empty()
            || self
                .os
                .iter()
                .any(|os| os.eq_ignore_ascii_case(&machine.os));
        let named = self.machines.is_empty()
            || machine
                .name
                .as_ref()
                .is_some_and(|name| self.machines.contains(name));
        let has = self
            .requires
            .iter()
            .all(|project| machine.projects.contains(project));
        os && named && has
    }
}

/// How a machine settled a note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Settled {
    /// Done, or found already done by its check.
    Applied,
    /// Left out on purpose.
    Skipped,
}

impl Settled {
    /// The word the store keeps.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Skipped => "skipped",
        }
    }

    /// Reads the stored word.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "applied" => Some(Self::Applied),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }
}

/// Where a note stands on a machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Standing {
    /// Applies here and is not settled.
    Pending,
    /// Settled as applied.
    Applied,
    /// Settled as skipped.
    Skipped,
    /// Filtered out for this machine.
    NotApplicable,
}

impl Standing {
    /// The word `migrate list` prints.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applied => "applied",
            Self::Skipped => "skipped",
            Self::NotApplicable => "not-applicable",
        }
    }
}

/// The standing of each note on `machine`, in the notes' order, given what the machine recorded (note id to state).
///
/// A recorded note keeps its recorded state even when the filters would now exclude it.
#[must_use]
pub fn standings(
    notes: &[Note],
    recorded: &BTreeMap<String, Settled>,
    machine: &Machine,
) -> Vec<(String, Standing)> {
    notes
        .iter()
        .map(|note| {
            let standing = match recorded.get(&note.id) {
                Some(Settled::Applied) => Standing::Applied,
                Some(Settled::Skipped) => Standing::Skipped,
                None if note.applies_to(machine) => Standing::Pending,
                None => Standing::NotApplicable,
            };
            (note.id.clone(), standing)
        })
        .collect()
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests fail loudly on a bad fixture")]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
id = "2026-11-25-sample"
title = "Sample"
os = ["windows"]
requires = ["headwork"]
check = "true"

[[step]]
kind = "agent"
run = "headwork config adopt --machine {machine}"

[[step]]
kind = "confirm"
run = "git reset --hard origin/master"
backup = "git branch backup"

[[step]]
kind = "user"
say = "Run `headwork tick --install`."
"#;

    const ID: &str = "2026-11-25-sample";

    fn sample() -> Note {
        Note::parse(SAMPLE).expect("the sample parses")
    }

    fn step(kind: StepKind, run: Option<&str>, say: Option<&str>) -> Step {
        Step {
            kind,
            run: run.map(str::to_owned),
            backup: None,
            say: say.map(str::to_owned),
        }
    }

    fn agent(run: &str) -> Note {
        Note {
            step: vec![step(StepKind::Agent, Some(run), None)],
            ..sample()
        }
    }

    #[test]
    fn a_good_note_has_no_problem() {
        assert_eq!(validate(&sample(), ID), Vec::new(), "sample");
    }

    #[test]
    fn unknown_fields_are_refused() {
        assert!(
            Note::parse("id = \"a\"\ntitle = \"b\"\nbogus = 1\n").is_err(),
            "bogus key"
        );
    }

    #[test]
    fn the_id_must_match_the_file() {
        assert_eq!(
            validate(&sample(), "other"),
            vec![Problem::IdMismatch {
                id: ID.to_owned(),
                stem: "other".to_owned()
            }],
            "mismatch"
        );
    }

    #[test]
    fn steps_need_their_fields() {
        let note = Note {
            step: vec![
                step(StepKind::Agent, None, None),
                step(StepKind::Confirm, Some(" "), None),
                step(StepKind::User, None, None),
            ],
            ..sample()
        };
        assert_eq!(
            validate(&note, ID),
            vec![
                Problem::MissingRun {
                    step: 1,
                    kind: StepKind::Agent
                },
                Problem::MissingRun {
                    step: 2,
                    kind: StepKind::Confirm
                },
                Problem::MissingSay { step: 3 },
            ],
            "missing fields"
        );
    }

    #[test]
    fn a_note_needs_a_step() {
        let note = Note {
            step: Vec::new(),
            ..sample()
        };
        assert_eq!(validate(&note, ID), vec![Problem::NoSteps], "no steps");
    }

    #[test]
    fn fields_of_the_wrong_kind_are_refused() {
        let note = Note {
            step: vec![
                step(StepKind::User, Some("x"), Some("y")),
                step(StepKind::Confirm, Some("x"), Some("y")),
            ],
            ..sample()
        };
        assert_eq!(
            validate(&note, ID),
            vec![
                Problem::Unused {
                    step: 1,
                    kind: StepKind::User,
                    field: "run"
                },
                Problem::Unused {
                    step: 2,
                    kind: StepKind::Confirm,
                    field: "say"
                },
            ],
            "unused fields"
        );
    }

    #[test]
    fn agent_steps_refuse_commands_that_need_the_user() {
        for pattern in AGENT_BANNED {
            let problems = validate(&agent(&format!("echo {pattern} now")), ID);
            assert_eq!(
                problems,
                vec![Problem::BannedInAgent { step: 1, pattern }],
                "{pattern}"
            );
        }
        assert_eq!(
            validate(&agent("headwork TICK --INSTALL"), ID).len(),
            1,
            "case does not matter"
        );
    }

    #[test]
    fn confirm_and_user_steps_may_hold_banned_commands() {
        assert_eq!(
            validate(&sample(), ID),
            Vec::new(),
            "reset --hard in confirm"
        );
    }

    #[test]
    fn duplicate_ids_are_found() {
        assert_eq!(
            duplicate_ids(["a", "b", "a"]),
            vec![Problem::DuplicateId("a".to_owned())],
            "a twice"
        );
        assert_eq!(duplicate_ids(["a", "b"]), Vec::new(), "all different");
    }

    fn machine(os: &str, name: Option<&str>, projects: &[&str]) -> Machine {
        Machine {
            os: os.to_owned(),
            name: name.map(str::to_owned),
            projects: projects.iter().map(|p| (*p).to_owned()).collect(),
        }
    }

    #[test]
    fn filters_pick_the_machines() {
        let note = sample();
        for (machine, applies, why) in [
            (
                machine("windows", None, &["headwork"]),
                true,
                "os and project match",
            ),
            (machine("macos", None, &["headwork"]), false, "other os"),
            (machine("windows", None, &[]), false, "project missing"),
        ] {
            assert_eq!(note.applies_to(&machine), applies, "{why}");
        }
        let named = Note {
            machines: vec!["mac".to_owned()],
            os: Vec::new(),
            requires: Vec::new(),
            ..sample()
        };
        assert!(
            named.applies_to(&machine("macos", Some("mac"), &[])),
            "named machine"
        );
        assert!(
            !named.applies_to(&machine("macos", Some("pc"), &[])),
            "other machine"
        );
        assert!(
            !named.applies_to(&machine("macos", None, &[])),
            "unnamed machine"
        );
    }

    #[test]
    fn standings_follow_the_record_then_the_filters() {
        let with_id = |id: &str| Note {
            id: id.to_owned(),
            ..sample()
        };
        let notes = vec![
            with_id("a"),
            with_id("b"),
            with_id("c"),
            Note {
                os: vec!["linux".to_owned()],
                ..with_id("d")
            },
        ];
        let recorded = BTreeMap::from([
            ("a".to_owned(), Settled::Applied),
            ("b".to_owned(), Settled::Skipped),
        ]);
        let got = standings(&notes, &recorded, &machine("windows", None, &["headwork"]));
        let words: Vec<_> = got
            .iter()
            .map(|(id, s)| format!("{id}:{}", s.as_str()))
            .collect();
        assert_eq!(
            words,
            ["a:applied", "b:skipped", "c:pending", "d:not-applicable"],
            "standings"
        );
    }

    #[test]
    fn the_machine_name_is_expanded() {
        let steps = sample().steps_for("windows-pc");
        assert_eq!(
            steps.first().and_then(|s| s.run.as_deref()),
            Some("headwork config adopt --machine windows-pc"),
            "placeholder"
        );
    }

    #[test]
    fn settled_words_round_trip() {
        for state in [Settled::Applied, Settled::Skipped] {
            assert_eq!(Settled::parse(state.as_str()), Some(state), "{state:?}");
        }
        assert_eq!(Settled::parse("other"), None, "unknown");
    }
}
