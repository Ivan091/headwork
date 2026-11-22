//! A session's launcher and `claude`, judged by what was recorded with their pids.
//!
//! Windows reuses process ids, so a task that sat in `closing` for days may have its recorded pids held by unrelated
//! programs (wmux, a shell). Every check and kill of a session's processes goes through here: a pid counts as the
//! session's only while [`headwork_core::process::RecordedPid::judge`] says it still names the recorded process, and
//! a pid another program took over counts as gone and is never signalled.

use anyhow::Result;
use headwork_adapters::proc::{self, Liveness, RecordedPid};
use headwork_core::Actor;
use headwork_store::{SessionRow, Store, TaskId};

/// Whether the recorded process still runs as itself.
#[must_use]
pub fn runs(record: &RecordedPid) -> bool {
    proc::recorded_liveness(record).is_alive()
}

/// Whether any of a session's recorded processes still runs as itself.
#[must_use]
pub fn any_runs(session: &SessionRow) -> bool {
    session.processes().iter().any(runs)
}

/// What ending a session's processes did.
#[derive(Debug, Default)]
pub struct Ended {
    /// At least one process still ran as itself and its tree was ended.
    pub any: bool,
    /// The pids another program held, with that program's image; each was left alone.
    pub reused: Vec<(u32, String)>,
}

impl Ended {
    fn add(&mut self, pid: u32, before: Liveness) {
        match before {
            Liveness::Alive => self.any = true,
            Liveness::Reused { now } => self.reused.push((pid, now.image)),
            Liveness::Gone => {}
        }
    }

    /// Logs a `pid-reused` event for each pid another program held.
    ///
    /// # Errors
    ///
    /// Fails when the store fails.
    pub fn log(
        &self,
        store: &Store,
        actor: Actor,
        task: Option<TaskId>,
        session: &str,
    ) -> Result<()> {
        for (pid, image) in &self.reused {
            store.append_event(
                actor,
                task,
                Some(session),
                "pid-reused",
                &serde_json::json!({ "pid": pid, "image": image }),
            )?;
        }
        Ok(())
    }
}

/// Ends the tree of each of `records` that still runs as itself, in order.
///
/// # Errors
///
/// Fails when a process cannot be ended.
pub fn end(records: &[RecordedPid]) -> Result<Ended> {
    let mut ended = Ended::default();
    for record in records {
        ended.add(record.pid, proc::end_recorded_tree(record)?);
    }
    Ok(ended)
}

/// Ends the trees of a session's processes that still run as themselves, `claude` first.
///
/// # Errors
///
/// Fails when a process cannot be ended.
pub fn end_all(session: &SessionRow) -> Result<Ended> {
    end(&session.processes())
}
