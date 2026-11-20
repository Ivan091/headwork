//! `reap`: end what ended sessions left running, and orphaned idle `tail` and `bash`; the decisions are
//! [`headwork_core::reap`], this gathers the facts and ends the processes.
//!
//! Each process is ended alone (never its tree, so a protected child is never taken along) and only while it is still
//! the process the snapshot saw: a pid another program took since is left alone. Every end is logged as `reaped`.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use headwork_adapters::proc::{self, ProcessIdentity};
use headwork_core::reap::{EndedSession, Facts, Reason, Spare, Verdict, plan};
use headwork_store::Store;
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
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let sessions: Vec<EndedSession> = store
        .ended_sessions()?
        .into_iter()
        .filter_map(|row| {
            Some(EndedSession {
                name: row.name,
                roots: [row.launcher_pid, row.claude_pid]
                    .into_iter()
                    .flatten()
                    .collect(),
                started: row.started?,
                ended: row.ended,
            })
        })
        .collect();
    let jobs: Vec<u32> = store
        .jobs_with_pid()?
        .iter()
        .filter(|job| super::jobs::liveness(job).is_some_and(|now| now.is_alive()))
        .filter_map(|job| job.pid)
        .collect();
    let (procs, shells_known) = proc::snapshot();
    let decided = plan(&Facts {
        now,
        procs: &procs,
        sessions: &sessions,
        jobs: &jobs,
        shells_known,
    });
    let mut ended = Vec::with_capacity(decided.end.len());
    for verdict in decided.end {
        let outcome = if options.dry_run {
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
                None,
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
    Ok(ReapReport {
        dry_run: options.dry_run,
        ended,
        spared: decided.spare,
        shells_judged: shells_known,
    })
}
