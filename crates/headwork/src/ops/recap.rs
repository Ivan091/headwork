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
use headwork_store::{NewRecap, Store, Task};

use super::Env;

/// How many changed files a recap lists.
const FILE_LIMIT: usize = 20;

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
    for job in store.open_jobs(Some(task.id))? {
        let _ = writeln!(
            text,
            "- job `{}` ({}, {}): {}",
            job.label,
            job.park_policy,
            job.state,
            job.argv.join(" ")
        );
    }
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
