//! Recaps (design section 6): what a session was doing, its next step and what runs, kept in the store and written to
//! `handoffs/<name>.md`, where the resumed session reads it.
//!
//! A session writes its own with the MCP `recap` tool. When it cannot (a park deadline, a crash, a close), the tool
//! writes a mechanical one from the store and the worktree.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use headwork_core::{Actor, RecapKind};
use headwork_store::{BoardTask, EventFilter, EventRecord, NewRecap, Store, Task};

use super::Env;

/// How many changed files a recap lists.
const FILE_LIMIT: usize = 20;

/// How many log events a recap lists.
const EVENT_LIMIT: u32 = 40;

/// How many characters of an event's details a recap keeps.
const DETAIL_LIMIT: usize = 160;

/// Event types too frequent to say anything about where a session stood.
const NOISY_EVENTS: [&str; 2] = ["tool-used", "usage-read"];

/// The log lines of `events`, newest last, noisy types left out and details cut short.
fn event_lines(events: &[EventRecord]) -> String {
    let mut text = String::new();
    for event in events
        .iter()
        .filter(|event| !NOISY_EVENTS.contains(&event.kind.as_str()))
    {
        let mut detail = event.data.to_string();
        if detail.chars().count() > DETAIL_LIMIT {
            detail = detail.chars().take(DETAIL_LIMIT).collect::<String>() + "...";
        }
        let detail = if detail == "null" || detail == "{}" {
            String::new()
        } else {
            format!(" {detail}")
        };
        let _ = writeln!(
            text,
            "  - {} {} {}{detail}",
            event.at, event.actor, event.kind
        );
    }
    text
}

/// What `git` prints for `args` in `dir`, trimmed; none when git fails or prints nothing.
#[must_use]
pub fn git_text(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// A recap built from the store and the worktree, with `reason` as its first line.
///
/// # Errors
///
/// Fails when the task's folder cannot be worked out or the store fails.
pub fn mechanical(store: &Store, env: &Env<'_>, task: &Task, reason: &str) -> Result<String> {
    let dir = env.task_dir(task)?;
    let mut text = format!("# Recap of {} ({reason})\n\n", task.name);
    let _ = writeln!(
        text,
        "- state: {}",
        task.state_before_park.unwrap_or(task.state)
    );
    let _ = writeln!(
        text,
        "- model: {}, {}, mode: {}",
        task.model,
        task.effort_label(),
        task.permission_mode
    );
    let _ = writeln!(text, "- brief: {}", task.brief_path);
    let _ = writeln!(text, "- folder: {}", super::slashed(&dir));
    if dir.is_dir() {
        if let Some(branch) = git_text(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]) {
            let _ = writeln!(text, "- branch: {branch}");
        }
        if let Some(head) = git_text(&dir, &["log", "-1", "--format=%h %s"]) {
            let _ = writeln!(text, "- head: {head}");
        }
        match git_text(&dir, &["status", "--short"]) {
            Some(status) => {
                let lines: Vec<&str> = status.lines().collect();
                let _ = writeln!(text, "- uncommitted files: {}", lines.len());
                for line in lines.iter().take(FILE_LIMIT) {
                    let _ = writeln!(text, "  - `{}`", line.trim());
                }
                if lines.len() > FILE_LIMIT {
                    let _ = writeln!(text, "  - and {} more", lines.len() - FILE_LIMIT);
                }
            }
            None => text.push_str("- uncommitted files: none\n"),
        }
    }
    match &task.last_report {
        Some(report) => {
            let _ = writeln!(text, "- last report: {report}");
        }
        None => text.push_str("- last report: none\n"),
    }
    for (question, options) in store.waiting_questions(task.id)? {
        let _ = writeln!(
            text,
            "- open question to the user: {question} (options: {options})"
        );
    }
    for pr in store.recorded_prs(task.id)? {
        let _ = writeln!(text, "- pull request {} ({})", pr.url, pr.state);
    }
    for part in store.parts(task.id)? {
        let part = store.task(part)?;
        let _ = writeln!(text, "- part {} ({})", part.name, part.state);
    }
    for job in store.open_jobs(Some(task.id))? {
        let checkpoint = job
            .checkpoint_path
            .as_deref()
            .map_or_else(String::new, |path| format!(", checkpoint {path}"));
        let _ = writeln!(
            text,
            "- job `{}` ({}, {}, restart {}): `{}` in {}{checkpoint}",
            job.label,
            job.park_policy,
            job.state,
            if job.restart { "yes" } else { "no" },
            job.argv.join(" "),
            job.cwd
        );
    }
    let events = store.events(EventFilter {
        task: Some(task.id),
        limit: EVENT_LIMIT,
    })?;
    let lines = event_lines(&events);
    if !lines.is_empty() {
        text.push_str("- recent log of this task, oldest first:\n");
        text.push_str(&lines);
    }
    text.push_str(
        "- this is a mechanical recap: the session wrote none of its own, so its decisions and reasons are not here; \
         read the brief, the branch's commits and its pull requests, and ask the user when something is unclear\n",
    );
    Ok(text)
}

/// One task of the board as a section of the orchestrator's recap.
fn task_section(store: &Store, entry: &BoardTask, text: &mut String) -> Result<()> {
    let task = &entry.task;
    let _ = writeln!(
        text,
        "- {} ({}, {}, model {}): {}",
        task.name,
        task.kind,
        task.state_before_park.unwrap_or(task.state),
        task.model,
        task.last_report.as_deref().unwrap_or("no report yet")
    );
    let _ = writeln!(text, "  - brief: {}", task.brief_path);
    if let Some(parent) = task.shape.parent() {
        let _ = writeln!(text, "  - part of {}", store.task(parent)?.name);
    }
    for pr in store.recorded_prs(task.id)? {
        let _ = writeln!(text, "  - pull request {} ({})", pr.url, pr.state);
    }
    for ask in &entry.asks {
        let _ = writeln!(
            text,
            "  - waits on the user: {} (options: {})",
            ask.question,
            ask.options.join(" | ")
        );
    }
    for job in store.open_jobs(Some(task.id))? {
        let _ = writeln!(
            text,
            "  - job `{}` ({}): `{}`",
            job.label,
            job.state,
            job.argv.join(" ")
        );
    }
    Ok(())
}

/// A recap of the orchestrator built from the store: the board, the queue, the asks, the added rules, the jobs and the log.
///
/// The orchestrator keeps no worktree and so no facts of its own; this is what a new orchestrator session needs to carry
/// on alone when the old one wrote no recap or never could (one started outside headwork).
///
/// # Errors
///
/// Fails when the store fails.
pub fn orchestrator_mechanical(store: &Store, reason: &str) -> Result<String> {
    let board = store.board(false)?;
    let mut text = format!("# Recap of the orchestrator ({reason})\n\n");
    let _ = writeln!(
        text,
        "- machine: {}, {} worker(s) running, {} waiting on the user",
        board.machine, board.live_workers, board.waiting_workers
    );
    text.push_str("\n## Tasks\n\n");
    if board.tasks.is_empty() {
        text.push_str("none\n");
    }
    for entry in &board.tasks {
        task_section(store, entry, &mut text)?;
    }
    let queued = store.queued()?;
    if !queued.is_empty() {
        text.push_str("\n## Queue, in start order\n\n");
        for id in queued {
            let _ = writeln!(text, "- {}", store.task(id)?.name);
        }
    }
    if !board.other_asks.is_empty() {
        text.push_str("\n## Asks without a task\n\n");
        for ask in &board.other_asks {
            let _ = writeln!(
                text,
                "- {} (options: {})",
                ask.question,
                ask.options.join(" | ")
            );
        }
    }
    let rules = store.active_rules()?;
    if !rules.is_empty() {
        text.push_str("\n## Rules the user added at run time (still active)\n\n");
        for rule in rules {
            let _ = writeln!(text, "- [{}] {}", rule.scope, rule.body.trim());
        }
    }
    let jobs = store.open_jobs(None)?;
    if !jobs.is_empty() {
        text.push_str("\n## Registered jobs not over\n\n");
        for job in jobs {
            let _ = writeln!(
                text,
                "- `{}` ({}, {}): `{}` in {}",
                job.label,
                job.park_policy,
                job.state,
                job.argv.join(" "),
                job.cwd
            );
        }
    }
    let events = store.events(EventFilter {
        task: None,
        limit: EVENT_LIMIT,
    })?;
    let lines = event_lines(&events);
    if !lines.is_empty() {
        text.push_str("\n## Recent log, oldest first\n\n");
        text.push_str(&lines);
    }
    text.push_str(
        "\nThis recap is mechanical: the orchestrator's own reasoning is not here. Read `board`, `inbox` and `headwork log` \
         for more, and ask the user about anything the user had asked for that no task shows.\n",
    );
    Ok(text)
}

/// Records a mechanical recap of a task. A failure is logged as an event and never stops the caller.
pub fn note(store: &mut Store, env: &Env<'_>, task: &Task, kind: RecapKind, reason: &str) {
    let session = store.session_of(task.id).ok().flatten().map(|s| s.id);
    let result = mechanical(store, env, task, reason).and_then(|body| {
        store
            .add_recap(
                &NewRecap {
                    task_id: Some(task.id),
                    session_id: session.as_deref(),
                    kind,
                    body: &body,
                },
                Actor::Tick,
            )
            .map_err(Into::into)
    });
    if let Err(error) = result {
        let data = serde_json::json!({ "error": format!("{error:#}") });
        drop(store.append_event(
            env.actor,
            Some(task.id),
            session.as_deref(),
            "recap-error",
            &data,
        ));
    }
}

/// Writes `handoffs/<name>.md`: the session's own recap when it wrote one, then the facts the tool recorded.
///
/// # Errors
///
/// Fails when the folder or file cannot be written.
pub fn write_handoff(
    env: &Env<'_>,
    task: &Task,
    session_recap: Option<&str>,
    facts: &str,
) -> Result<std::path::PathBuf> {
    let path = env.home.handoff_path(&task.name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut text = String::new();
    if let Some(recap) = session_recap {
        text.push_str("# Your own recap\n\n");
        text.push_str(recap.trim_end());
        text.push_str("\n\n");
    }
    text.push_str(facts.trim_end());
    text.push('\n');
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}
