//! The "Needs you" digest (`docs/todo.md`): what waits on the user, as at most ten lines.
//!
//! The builder is pure. The binary gathers the [`Facts`] from the store; ages arrive in seconds and the clock as
//! [`Facts::today`], so the same facts always give the same digest.

use std::fmt::Write as _;

use serde::Serialize;

use crate::todo::{Todo, TodoDate, TodoOwner, TodoState};

/// The header of a digest that holds lines.
pub const HEADER: &str = "Needs you";

/// The most lines a digest holds, header included.
pub const MAX_LINES: usize = 10;

const TEXT_LIMIT: usize = 80;
const ASK_LIMIT: usize = 60;
const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR: u64 = 3600;
const SECONDS_PER_DAY: u64 = 86_400;
const HOURS_BEFORE_DAYS: u64 = 48;

/// An ask that waits on the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskFact {
    /// The ask's id.
    pub id: i64,
    /// The task's name; none for an ask of the orchestrator.
    pub task: Option<String>,
    /// The question.
    pub question: String,
    /// Seconds since it was asked.
    pub waiting_s: u64,
}

/// A migration note that waits for this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteFact {
    /// `<source>/<id>`.
    pub address: String,
    /// The note's title.
    pub title: String,
}

/// A task whose pull requests wait for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrFact {
    /// The task's name.
    pub task: String,
    /// Its open pull requests.
    pub urls: Vec<String>,
}

/// A tick finding that stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingFact {
    /// The rule's word.
    pub kind: String,
    /// The task's name, when the finding is about one.
    pub task: Option<String>,
    /// Seconds since it was first seen.
    pub open_s: u64,
}

/// Everything a digest is built from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// Today on this machine's clock.
    pub today: TodoDate,
    /// The asks that wait.
    pub asks: Vec<AskFact>,
    /// The to-do items, in any state.
    pub todos: Vec<Todo>,
    /// The migration notes that wait.
    pub notes: Vec<NoteFact>,
    /// The tasks whose pull requests wait.
    pub prs: Vec<PrFact>,
    /// The standing findings.
    pub findings: Vec<FindingFact>,
    /// Hours a finding must stand before it is listed.
    pub finding_hours: u32,
}

/// The digest: its lines in order, and how many more did not fit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Digest {
    /// The lines shown, without the header and the closing "more" line.
    pub lines: Vec<String>,
    /// How many items did not fit.
    pub more: usize,
}

impl Digest {
    /// Whether nothing waits on the user.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The digest as text: the header and a bullet per line; empty when there is nothing.
    #[must_use]
    pub fn render(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut text = format!("{HEADER}\n");
        for line in &self.lines {
            text.push_str("- ");
            text.push_str(line);
            text.push('\n');
        }
        if self.more > 0 {
            let _ = writeln!(text, "... and {} more: headwork digest", self.more);
        }
        text
    }
}

impl Digest {
    /// The digest as text of at most `room` bytes: whole when it fits, else with fewer lines and a longer "more" line,
    /// else a one-line count, else nothing.
    #[must_use]
    pub fn render_within(&self, room: usize) -> String {
        let full = self.render();
        if full.len() <= room {
            return full;
        }
        for keep in (1..self.lines.len()).rev() {
            let shorter = Self {
                lines: self.lines.iter().take(keep).cloned().collect(),
                more: self.more + self.lines.len() - keep,
            };
            let text = shorter.render();
            if text.len() <= room {
                return text;
            }
        }
        let count = format!(
            "{HEADER}: {} waiting, run `headwork digest`\n",
            self.lines.len() + self.more
        );
        if count.len() <= room {
            count
        } else {
            String::new()
        }
    }
}

/// `text` cut to `limit` characters on its first line, with `...` when something was cut.
#[must_use]
pub fn shorten(text: &str, limit: usize) -> String {
    let first = text.lines().next().unwrap_or_default().trim();
    let cut = first.chars().count() > limit || text.trim().lines().count() > 1;
    let kept: String = first.chars().take(limit).collect();
    if cut {
        format!("{}...", kept.trim_end())
    } else {
        kept
    }
}

/// A span as the digest says it: `5m`, `3h` or `2d`.
#[must_use]
pub fn age(seconds: u64) -> String {
    if seconds < SECONDS_PER_HOUR {
        format!("{}m", seconds / SECONDS_PER_MINUTE)
    } else if seconds < HOURS_BEFORE_DAYS * SECONDS_PER_HOUR {
        format!("{}h", seconds / SECONDS_PER_HOUR)
    } else {
        format!("{}d", seconds / SECONDS_PER_DAY)
    }
}

fn ask_line(ask: &AskFact) -> String {
    format!(
        "ask {} {}: {} (waiting {})",
        ask.id,
        ask.task.as_deref().unwrap_or("orchestrator"),
        shorten(&ask.question, ASK_LIMIT),
        age(ask.waiting_s)
    )
}

fn dated_todos(todos: &[Todo], today: &TodoDate) -> Vec<(TodoDate, i64, String)> {
    let mut dated: Vec<(TodoDate, i64, String)> = todos
        .iter()
        .filter(|todo| todo.state == TodoState::Open && todo.task.is_none())
        .filter_map(|todo| {
            let text = shorten(&todo.text, TEXT_LIMIT);
            match (&todo.due, &todo.not_before) {
                (Some(due), _) if due <= today => Some((
                    due.clone(),
                    todo.id,
                    format!("todo #{}: {text} (due {due})", todo.id),
                )),
                (_, Some(from)) if from <= today => Some((
                    from.clone(),
                    todo.id,
                    format!("todo #{}: {text} (from {from})", todo.id),
                )),
                _ => None,
            }
        })
        .collect();
    dated.sort();
    dated
}

/// Builds the digest from `facts`.
///
/// Asks come oldest first, then to-do items due or started, migration notes, pull requests, findings open long enough,
/// and the other open items of the user, trimmed to [`MAX_LINES`] with a closing line for the rest.
#[must_use]
pub fn build(facts: &Facts) -> Digest {
    let mut asks: Vec<&AskFact> = facts.asks.iter().collect();
    asks.sort_by_key(|ask| (std::cmp::Reverse(ask.waiting_s), ask.id));
    let mut lines: Vec<String> = asks.into_iter().map(ask_line).collect();
    let dated = dated_todos(&facts.todos, &facts.today);
    let listed: Vec<i64> = dated.iter().map(|(_, id, _)| *id).collect();
    lines.extend(dated.into_iter().map(|(_, _, line)| line));
    lines.extend(facts.notes.iter().map(|note| {
        format!(
            "migration note {}: {}",
            note.address,
            shorten(&note.title, TEXT_LIMIT)
        )
    }));
    lines.extend(
        facts
            .prs
            .iter()
            .map(|pr| format!("pr {}: {}", pr.task, pr.urls.join(" "))),
    );
    let threshold = u64::from(facts.finding_hours) * SECONDS_PER_HOUR;
    let mut findings: Vec<&FindingFact> = facts
        .findings
        .iter()
        .filter(|finding| finding.open_s >= threshold)
        .collect();
    findings.sort_by_key(|finding| std::cmp::Reverse(finding.open_s));
    lines.extend(findings.into_iter().map(|finding| {
        format!(
            "finding {} {} (open {})",
            finding.kind,
            finding.task.as_deref().unwrap_or("-"),
            age(finding.open_s)
        )
    }));
    let mut rest: Vec<&Todo> = facts
        .todos
        .iter()
        .filter(|todo| {
            todo.state == TodoState::Open
                && todo.owner == TodoOwner::User
                && todo.task.is_none()
                && !listed.contains(&todo.id)
                && todo
                    .not_before
                    .as_ref()
                    .is_none_or(|from| from <= &facts.today)
        })
        .collect();
    rest.sort_by_key(|todo| (todo.position, todo.id));
    lines.extend(
        rest.into_iter()
            .map(|todo| format!("todo #{}: {}", todo.id, shorten(&todo.text, TEXT_LIMIT))),
    );
    trim(lines)
}

fn trim(mut lines: Vec<String>) -> Digest {
    let room = MAX_LINES - 1;
    if lines.len() <= room {
        return Digest { lines, more: 0 };
    }
    let shown = room - 1;
    let more = lines.len() - shown;
    lines.truncate(shown);
    Digest { lines, more }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::todo::LinkedTask;

    fn date(text: &str) -> TodoDate {
        TodoDate::parse(text).unwrap()
    }

    fn todo(id: i64, text: &str, owner: TodoOwner) -> Todo {
        Todo {
            id,
            project: "demo".to_owned(),
            section: None,
            text: text.to_owned(),
            notes: Vec::new(),
            state: TodoState::Open,
            owner,
            added: None,
            not_before: None,
            due: None,
            done_on: None,
            done_note: None,
            task: None,
            position: id,
        }
    }

    fn ask(id: i64, task: Option<&str>, question: &str, waiting_s: u64) -> AskFact {
        AskFact {
            id,
            task: task.map(str::to_owned),
            question: question.to_owned(),
            waiting_s,
        }
    }

    fn facts() -> Facts {
        Facts {
            today: date("2026-11-26"),
            asks: Vec::new(),
            todos: Vec::new(),
            notes: Vec::new(),
            prs: Vec::new(),
            findings: Vec::new(),
            finding_hours: 3,
        }
    }

    #[test]
    fn nothing_waiting_gives_an_empty_digest() {
        let digest = build(&facts());
        assert!(digest.is_empty(), "no lines");
        assert_eq!(digest.render(), "", "no text");
    }

    #[test]
    fn each_kind_has_its_line_shape_in_order() {
        let mut facts = facts();
        facts.asks = vec![
            ask(8, Some("demo-new"), "Close?", 600),
            ask(3, None, "Pick one", 3 * 3600 + 5),
        ];
        let mut due = todo(1, "Pay the invoice", TodoOwner::User);
        due.due = Some(date("2026-11-20"));
        let mut from = todo(2, "Start the survey", TodoOwner::Agent);
        from.not_before = Some(date("2026-11-26"));
        let mut future = todo(3, "Later thing", TodoOwner::User);
        future.not_before = Some(date("2026-12-01"));
        let plain = todo(4, "Review the draft", TodoOwner::User);
        let agent = todo(5, "Agent chore", TodoOwner::Agent);
        facts.todos = vec![plain, future, from, due, agent];
        facts.notes = vec![NoteFact {
            address: "demo/2026-11-26-x".into(),
            title: "Set a key".into(),
        }];
        facts.prs = vec![PrFact {
            task: "demo-fix".into(),
            urls: vec!["https://example.test/pull/1".into()],
        }];
        facts.findings = vec![
            FindingFact {
                kind: "stuck".into(),
                task: Some("demo-fix".into()),
                open_s: 5 * 3600,
            },
            FindingFact {
                kind: "disk-warn".into(),
                task: None,
                open_s: 3600,
            },
        ];
        let digest = build(&facts);
        assert_eq!(
            digest.lines,
            [
                "ask 3 orchestrator: Pick one (waiting 3h)",
                "ask 8 demo-new: Close? (waiting 10m)",
                "todo #1: Pay the invoice (due 2026-11-20)",
                "todo #2: Start the survey (from 2026-11-26)",
                "migration note demo/2026-11-26-x: Set a key",
                "pr demo-fix: https://example.test/pull/1",
                "finding stuck demo-fix (open 5h)",
                "todo #4: Review the draft",
            ],
            "order and shapes"
        );
        assert_eq!(digest.more, 0, "all fit");
        assert!(
            digest.render().starts_with("Needs you\n- ask 3 "),
            "{}",
            digest.render()
        );
    }

    #[test]
    fn more_than_fits_ends_with_a_count() {
        let mut facts = facts();
        facts.todos = (1..=14)
            .map(|id| todo(id, "Item", TodoOwner::User))
            .collect();
        let digest = build(&facts);
        assert_eq!(digest.lines.len(), 8, "eight items");
        assert_eq!(digest.more, 6, "the rest");
        let text = digest.render();
        assert_eq!(text.lines().count(), MAX_LINES, "ten lines with the header");
        assert!(
            text.ends_with("... and 6 more: headwork digest\n"),
            "{text}"
        );
    }

    #[test]
    fn a_digest_that_does_not_fit_loses_lines_then_becomes_a_count() {
        let mut facts = facts();
        facts.todos = (1..=4)
            .map(|id| todo(id, "Item", TodoOwner::User))
            .collect();
        let digest = build(&facts);
        let full = digest.render();
        assert_eq!(digest.render_within(full.len()), full, "fits whole");
        let fewer = digest.render_within(full.len() - 1);
        assert!(fewer.len() < full.len(), "shorter: {fewer}");
        assert!(fewer.ends_with("more: headwork digest\n"), "{fewer}");
        let count = digest.render_within(50);
        assert_eq!(
            count, "Needs you: 4 waiting, run `headwork digest`\n",
            "a count"
        );
        assert_eq!(digest.render_within(5), "", "no room");
    }

    #[test]
    fn exactly_nine_items_all_show() {
        let mut facts = facts();
        facts.todos = (1..=9)
            .map(|id| todo(id, "Item", TodoOwner::User))
            .collect();
        let digest = build(&facts);
        assert_eq!((digest.lines.len(), digest.more), (9, 0), "nine fit");
    }

    #[test]
    fn done_dropped_and_linked_items_are_left_out() {
        let mut facts = facts();
        let mut done = todo(1, "Done one", TodoOwner::User);
        done.state = TodoState::Done;
        let mut dropped = todo(2, "Dropped one", TodoOwner::User);
        dropped.state = TodoState::Dropped;
        let mut linked = todo(3, "Linked one", TodoOwner::User);
        linked.task = Some(LinkedTask {
            id: 1,
            name: "demo-x".into(),
            state: "working".into(),
        });
        facts.todos = vec![done, dropped, linked];
        assert!(build(&facts).is_empty(), "nothing open and free");
    }

    #[test]
    fn long_text_is_cut_and_spans_are_short() {
        let text = "x".repeat(100);
        assert_eq!(shorten(&text, 80).chars().count(), 83, "80 and the dots");
        assert_eq!(shorten("short", 80), "short", "left alone");
        assert_eq!(shorten("one\ntwo", 80), "one...", "first line only");
        assert_eq!(age(59), "0m", "under a minute");
        assert_eq!(age(7200), "2h", "hours");
        assert_eq!(age(3 * 86_400), "3d", "days");
    }
}
