//! `reap`: end what ended sessions left running, and orphaned idle `tail` and `bash`; the decisions are
//! [`headwork_core::reap`], this gathers the facts and ends the processes.
//!
//! Each process is ended alone (never its tree, so a protected child is never taken along) and only while it is still
//! the process the snapshot saw: a pid another program took since is left alone. Every end is logged as `reaped`.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use headwork_adapters::proc::{self, ProcessIdentity};
use headwork_core::reap::{EndedSession, Facts, Reason, Root, Spare, Verdict, plan};
use headwork_store::{Store, TaskId};
use serde::Serialize;

use super::Env;

/// How `reap` runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReapOptions {
    /// Report what would be ended and end nothing.
    pub dry_run: bool,
}

/// What became of a process `reap` chose to end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum Outcome {
    /// A dry run: it would be ended.
    WouldEnd,
    /// Ended.
    Ended,
    /// It was gone, or another program held its pid, by the time of the kill.
    Gone,
    /// The kill failed.
    Failed {
        /// Why.
        error: String,
    },
}

/// One process `reap` chose to end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reaped {
    /// Its id.
    pub pid: u32,
    /// Its program.
    pub image: String,
    /// Why it was chosen.
    pub why: Reason,
    /// What became of it.
    pub outcome: Outcome,
}

/// What `reap` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReapReport {
    /// It was a dry run.
    pub dry_run: bool,
    /// The processes chosen, those started last first.
    pub ended: Vec<Reaped>,
    /// The leftovers left running, and why.
    pub spared: Vec<Verdict<Spare>>,
    /// Git Bash's process list was read; without it no shell was judged orphaned.
    pub shells_judged: bool,
}

impl ReapReport {
    /// Some kill failed.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.ended
            .iter()
            .any(|reaped| matches!(reaped.outcome, Outcome::Failed { .. }))
    }
}

/// Ends what ended sessions left running and orphaned idle shells, or with `dry_run` only says what it would end.
///
/// # Errors
///
/// Fails when the store fails.
pub fn reap(store: &mut Store, env: &Env<'_>, options: ReapOptions) -> Result<ReapReport> {
    let now = now();
    let sessions = ended_sessions(store, |_| true)?;
    let jobs = live_jobs(store)?;
    let (procs, shells_known) = proc::snapshot();
    let decided = plan(&Facts {
        now,
        procs: &procs,
        sessions: &sessions,
        jobs: &jobs,
        shells_known,
        parents_kept: cfg!(windows),
    });
    let ended = end(store, env, None, decided.end, options.dry_run)?;
    Ok(ReapReport {
        dry_run: options.dry_run,
        ended,
        spared: decided.spare,
        shells_judged: shells_known,
    })
}

/// Ends what the sessions of a closing task left running once its tree is gone, without waiting for them to settle.
///
/// On unix a `nohup cmd &` from a `Bash` call runs in a session of its own, outlives the tree and is found by its
/// working folder (see [`headwork_core::reap`]).
///
/// # Errors
///
/// Fails when the store fails.
pub fn end_task_leftovers(
    store: &mut Store,
    env: &Env<'_>,
    task: TaskId,
    name: &str,
) -> Result<Vec<Reaped>> {
    let sessions: Vec<EndedSession> = ended_sessions(store, |row| row.name == name)?
        .into_iter()
        .map(|session| EndedSession {
            ended: None,
            ..session
        })
        .collect();
    if sessions.is_empty() {
        return Ok(Vec::new());
    }
    let jobs = live_jobs(store)?;
    let (procs, shells_known) = proc::snapshot();
    let decided = plan(&Facts {
        now: now(),
        procs: &procs,
        sessions: &sessions,
        jobs: &jobs,
        shells_known,
        parents_kept: cfg!(windows),
    });
    let leftovers = decided
        .end
        .into_iter()
        .filter(|verdict| matches!(verdict.why, Reason::Session { .. }))
        .collect();
    end(store, env, Some(task), leftovers, false)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn ended_sessions(
    store: &Store,
    keep: impl Fn(&headwork_store::EndedSession) -> bool,
) -> Result<Vec<EndedSession>> {
    Ok(store
        .ended_sessions()?
        .into_iter()
        .filter(|row| keep(row))
        .filter_map(|row| {
            Some(EndedSession {
                name: row.name,
                roots: [
                    (row.launcher_pid, row.launcher_started),
                    (row.claude_pid, row.claude_started),
                ]
                .into_iter()
                .filter_map(|(pid, started)| pid.map(|pid| Root { pid, started }))
                .collect(),
                started: row.started?,
                ended: row.ended,
                last_seen: row.last_seen,
                folders: with_resolved(row.folders),
            })
        })
        .collect())
}

/// The folders as recorded and as the system resolves them (`/tmp` is `/private/tmp` on macOS), since a process
/// reports its working folder resolved.
fn with_resolved(folders: Vec<String>) -> Vec<String> {
    let resolved: Vec<String> = folders
        .iter()
        .filter_map(|folder| std::fs::canonicalize(folder).ok())
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    let mut all = folders;
    for folder in resolved {
        if !all.contains(&folder) {
            all.push(folder);
        }
    }
    all
}

fn live_jobs(store: &Store) -> Result<Vec<u32>> {
    Ok(store
        .jobs_with_pid()?
        .iter()
        .filter(|job| super::jobs::liveness(job).is_some_and(|now| now.is_alive()))
        .filter_map(|job| job.pid)
        .collect())
}

fn end(
    store: &Store,
    env: &Env<'_>,
    task: Option<TaskId>,
    chosen: Vec<Verdict<Reason>>,
    dry_run: bool,
) -> Result<Vec<Reaped>> {
    let mut ended = Vec::with_capacity(chosen.len());
    for verdict in chosen {
        let outcome = if dry_run {
            Outcome::WouldEnd
        } else {
            let identity = ProcessIdentity {
                image: verdict.image.clone(),
                started: verdict.started,
            };
            match proc::kill_one(verdict.pid, &identity) {
                Ok(true) => Outcome::Ended,
                Ok(false) => Outcome::Gone,
                Err(error) => Outcome::Failed {
                    error: error.to_string(),
                },
            }
        };
        if matches!(outcome, Outcome::Ended | Outcome::Failed { .. }) {
            store.append_event(
                env.actor,
                task,
                None,
                "reaped",
                &serde_json::json!({
                    "pid": verdict.pid,
                    "image": verdict.image,
                    "why": verdict.why,
                    "outcome": outcome,
                }),
            )?;
        }
        ended.push(Reaped {
            pid: verdict.pid,
            image: verdict.image,
            why: verdict.why,
            outcome,
        });
    }
    Ok(ended)
}
