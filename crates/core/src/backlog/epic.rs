//! The epic explorer's spin-up (`docs/backlog.md`, section 12): the slug of its task, the name of its plan notes file and
//! the brief it starts from.
//!
//! Pure: the binary reads the epic from the store, writes the brief to a file and spawns the task.

use std::fmt::Write as _;

/// How many characters of the epic's text the short slug keeps.
const SHORT_CHARS: usize = 24;

/// How many words of the epic's text the short slug keeps.
const SHORT_WORDS: usize = 4;

/// The first words of `text` in kebab case: ASCII letters and digits, lower case, at most four words and 24 characters,
/// never ending in a hyphen; empty when the text has no such word.
#[must_use]
pub fn short_slug(text: &str) -> String {
    let mut out = String::new();
    let words = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .take(SHORT_WORDS);
    for word in words {
        let word = word.to_ascii_lowercase();
        let extra = usize::from(!out.is_empty());
        if out.len() + extra + word.len() > SHORT_CHARS {
            if out.is_empty() {
                out.extend(word.chars().take(SHORT_CHARS));
            }
            break;
        }
        if extra == 1 {
            out.push('-');
        }
        out.push_str(&word);
    }
    out
}

/// The slug of the explorer task of epic `id`: `epic-<id>-<short slug of the text>`, or `epic-<id>` when the text gives
/// no word.
#[must_use]
pub fn epic_slug(id: i64, text: &str) -> String {
    match short_slug(text) {
        short if short.is_empty() => format!("epic-{id}"),
        short => format!("epic-{id}-{short}"),
    }
}

/// The file name of epic `id`'s plan notes, under `<brief_writable root>/epics/`: `<id>-<short slug>.md`, or `<id>.md`.
#[must_use]
pub fn plan_file_name(id: i64, text: &str) -> String {
    match short_slug(text) {
        short if short.is_empty() => format!("{id}.md"),
        short => format!("{id}-{short}.md"),
    }
}

/// What the explorer's brief is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpicBrief<'a> {
    /// The epic's id.
    pub id: i64,
    /// The epic's text.
    pub text: &'a str,
    /// The epic's notes, one line each.
    pub notes: &'a [String],
    /// The epic's project.
    pub project: &'a str,
    /// The plan notes file, on the brief's `Writes:` line.
    pub plan_path: &'a str,
}

/// The brief of epic `brief.id`'s explorer: a `Goal:` line, the epic's text and notes, the project and a `Writes:` line
/// naming the plan notes file.
#[must_use]
pub fn epic_brief(brief: &EpicBrief<'_>) -> String {
    let EpicBrief {
        id,
        text,
        notes,
        project,
        plan_path,
    } = *brief;
    let mut out = format!(
        "Goal: Plan epic #{id}: {text}\n\n\
         You explore epic #{id} of project {project}, read only. Plan it as your rules say (\"You explore an epic\"): write \
         the plan to the file below, create the epic's children with `plan_add`, order them, and check them with \
         `plan_list`. The user's spin-up of the epic is the go for the whole plan: no `Backlog` ask.\n\n\
         ## The epic\n\n#{id} {text}\n"
    );
    if !notes.is_empty() {
        out.push('\n');
        for note in notes {
            let _infallible = writeln!(out, "- {note}");
        }
    }
    let _infallible = write!(out, "\nProject: {project}\n\nWrites: {plan_path}\n");
    out
}
