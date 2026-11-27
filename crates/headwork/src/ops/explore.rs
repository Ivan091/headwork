//! Explorations (kind `explore`): the user's `Backlog` ask and the adds an explorer may make after it.
//!
//! An explorer ends in a report whose last section lists backlog candidates, then asks the user one `AskUserQuestion`
//! headed `Backlog`. The `PostToolUse` hook records the picks (`backlog-picked`); the narrow `backlog_add` tool then lets the
//! explorer add as many items as were picked, each with a `Source:` suffix naming its report.

use anyhow::{Result, anyhow, bail};
use headwork_core::guard::grants::{brief_report, explore_report_check};
use headwork_core::guard::{GuardPath, Places};
use headwork_core::{Actor, EXPLORE_KIND, TaskShape};
use headwork_store::{EVENT_BACKLOG_PICKED, Store, Task};
use serde_json::{Value, json};

use super::backlog::{AddRequest, Change, add_for};
use super::{Env, slashed};

/// The header of the explorer's ask about which candidates go on the backlog.
pub const BACKLOG_HEADER: &str = "Backlog";

/// The options that decline every candidate; picking one adds nothing.
pub const DECLINE_LABELS: &[&str] = &["Skip it", "Add none"];

/// The event of an answered `Backlog` ask: `{"count": N, "picks": [labels]}`.
pub const EVENT_PICKED: &str = EVENT_BACKLOG_PICKED;

/// The refusal of an add before the user picked anything.
pub const UNASKED: &str = "backlog-unasked";

/// One asked question: its text, its header and its option labels.
type Question = (String, String, Vec<String>);

/// Whether `task` is an explorer: a task of kind `explore` that is not a part of a lead.
#[must_use]
pub fn is_explorer(task: &Task) -> bool {
    task.kind == EXPLORE_KIND && !matches!(task.shape, TaskShape::Part { .. })
}

fn questions(asked: &Value) -> Vec<Question> {
    let Some(list) = asked.get("questions").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .map(|question| {
            let text = |key: &str| {
                question
                    .get(key)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            let options = question
                .get("options")
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .filter_map(|option| {
                            option
                                .get("label")
                                .and_then(Value::as_str)
                                .or_else(|| option.as_str())
                                .map(str::to_owned)
                        })
                        .collect()
                })
                .unwrap_or_default();
            (text("question"), text("header"), options)
        })
        .collect()
}

/// The labels the user picked in the questions headed `Backlog` of an asked `AskUserQuestion`, or `None` when the ask has
/// no such question.
///
/// `answers` maps a question's text to the answer, where a multi-select answer joins the picked labels with `, `. Only
/// labels that equal an option of the question count, so typed text picks nothing, and the decline labels never count.
#[must_use]
pub fn picked_labels(asked: &Value, answers: Option<&Value>) -> Option<Vec<String>> {
    let mut found = false;
    let mut picks = Vec::new();
    for (question, header, options) in questions(asked) {
        if !header.trim().eq_ignore_ascii_case(BACKLOG_HEADER) {
            continue;
        }
        found = true;
        let Some(answer) = answers
            .and_then(|answers| answers.get(&question))
            .and_then(Value::as_str)
        else {
            continue;
        };
        picks.extend(
            options
                .into_iter()
                .filter(|label| !is_decline(label) && names_label(answer, label)),
        );
    }
    found.then_some(picks)
}

fn is_decline(label: &str) -> bool {
    DECLINE_LABELS
        .iter()
        .any(|decline| decline.eq_ignore_ascii_case(label.trim()))
}

fn names_label(answer: &str, label: &str) -> bool {
    if label.is_empty() {
        return false;
    }
    answer.match_indices(label).any(|(start, _)| {
        let before = answer.get(..start).unwrap_or_default();
        let after = answer.get(start + label.len()..).unwrap_or_default();
        (before.is_empty() || before.ends_with(", "))
            && (after.is_empty() || after.starts_with(", "))
    })
}

/// Records the picks of an answered ask as the event `backlog-picked` when it has a question headed `Backlog`.
///
/// # Errors
///
/// Fails when the store does.
pub fn record_backlog_picks(
    store: &Store,
    task: &Task,
    session: &str,
    asked: &Value,
    response: &Value,
) -> Result<()> {
    if let Some(picks) = picked_labels(asked, response.get("answers")) {
        store.append_event(
            Actor::Hook,
            Some(task.id),
            Some(session),
            EVENT_PICKED,
            &json!({ "count": picks.len(), "picks": picks }),
        )?;
    }
    Ok(())
}

/// Adds an explorer's item, refused with `backlog-unasked` when the user's picks are used up.
///
/// The `backlog-added` event carries the task, and ` Source: <report>, task <name>` is appended when the text names no
/// source.
///
/// # Errors
///
/// Fails on the refusal, or as [`add_for`] does.
pub fn add(
    store: &mut Store,
    env: &Env<'_>,
    task: &Task,
    mut request: AddRequest,
) -> Result<Change> {
    if store.backlog_adds_left(task.id)? == 0 {
        bail!(
            "{UNASKED}: ask the user with one AskUserQuestion headed `{BACKLOG_HEADER}` first and add only what they \
             picked; no pick is left to add"
        );
    }
    request.text = with_source(
        &request.text,
        report_path(env, task).as_deref(),
        &task.name.to_string(),
    );
    add_for(store, env, &request, Some(task.id))
}

/// `text` with ` Source: <report>, task <name>` appended unless it has a `Source:` already.
#[must_use]
pub fn with_source(text: &str, report: Option<&str>, task: &str) -> String {
    if text.to_ascii_lowercase().contains("source:") {
        return text.to_owned();
    }
    let place = report.map_or_else(
        || format!("task {task}"),
        |path| format!("{path}, task {task}"),
    );
    format!("{} Source: {place}", text.trim_end())
}

/// Refuses a top-level exploration whose brief names no report path under `[hooks] brief_writable`.
///
/// # Errors
///
/// The refusal `explore-no-report`.
pub fn check_report(env: &Env<'_>, brief: &str) -> Result<()> {
    let places = Places {
        home: Some(slashed(&env.home.expand("~"))),
        temp: None,
    };
    explore_report_check(brief, &brief_roots(env), &places).map_err(|message| anyhow!(message))
}

fn brief_roots(env: &Env<'_>) -> Vec<GuardPath> {
    env.config
        .hooks
        .brief_writable
        .iter()
        .map(|dir| GuardPath::new(&slashed(&env.home.expand(dir))))
        .collect()
}

fn report_path(env: &Env<'_>, task: &Task) -> Option<String> {
    let brief = std::fs::read_to_string(&task.brief_path).ok()?;
    let places = Places {
        home: Some(slashed(&env.home.expand("~"))),
        temp: None,
    };
    brief_report(&brief, &brief_roots(env), &places).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(header: &str) -> Value {
        json!({ "questions": [{
            "question": "Which go on the backlog?",
            "header": header,
            "multiSelect": true,
            "options": [
                { "label": "Guard x", "description": "d" },
                { "label": "Fix y, then z", "description": "d" },
                { "label": "Add it (Recommended)", "description": "d" },
                { "label": "Skip it", "description": "d" },
                { "label": "Add none", "description": "d" }
            ]
        }] })
    }

    fn answered(answer: &str) -> Value {
        json!({ "Which go on the backlog?": answer })
    }

    #[test]
    fn picks_are_the_option_labels_the_answer_names() {
        let table: &[(&str, &[&str])] = &[
            ("Guard x", &["Guard x"]),
            ("Guard x, Fix y, then z", &["Guard x", "Fix y, then z"]),
            ("Add it (Recommended)", &["Add it (Recommended)"]),
            ("Skip it", &[]),
            ("add none", &[]),
            ("Guard x, Skip it", &["Guard x"]),
            ("do the guard instead", &[]),
            ("Guard xyz", &[]),
            ("", &[]),
        ];
        for (answer, want) in table {
            let got = picked_labels(&ask("Backlog"), Some(&answered(answer)));
            let want: Vec<String> = want.iter().map(|label| (*label).to_owned()).collect();
            assert_eq!(got, Some(want), "{answer}");
        }
    }

    #[test]
    fn only_questions_headed_backlog_count() {
        let guard = || Some(vec!["Guard x".to_owned()]);
        assert_eq!(
            picked_labels(&ask("Close"), Some(&answered("Guard x"))),
            None
        );
        assert_eq!(
            picked_labels(&ask(" backlog "), Some(&answered("Guard x"))),
            guard()
        );
        assert_eq!(
            picked_labels(&ask("Backlog"), None),
            Some(Vec::new()),
            "no answers is zero picks"
        );
        assert_eq!(picked_labels(&json!({}), Some(&answered("x"))), None);
        let mut two = ask("Backlog");
        let second = json!({
            "question": "Second?", "header": "Backlog",
            "options": [{ "label": "Fix y, then z" }, { "label": "Other" }]
        });
        if let Some(list) = two.get_mut("questions").and_then(Value::as_array_mut) {
            list.push(second);
        }
        let answers = json!({ "Which go on the backlog?": "Guard x", "Second?": "Fix y, then z" });
        assert_eq!(
            picked_labels(&two, Some(&answers)).map(|picks| picks.len()),
            Some(2)
        );
    }

    #[test]
    fn a_source_is_appended_once() {
        assert_eq!(
            with_source("Guard x.", Some("S:/Code/notes/r.md"), "headwork-audit"),
            "Guard x. Source: S:/Code/notes/r.md, task headwork-audit"
        );
        assert_eq!(
            with_source(
                "Guard x. Source: notes/o.md",
                Some("S:/Code/notes/r.md"),
                "t"
            ),
            "Guard x. Source: notes/o.md"
        );
        assert_eq!(
            with_source("Guard x.", None, "t"),
            "Guard x. Source: task t"
        );
    }
}
