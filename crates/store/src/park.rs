//! Parks, recaps and the jobs registry (design sections 2.2 and 6).

use headwork_core::config::ParkPolicy;
use headwork_core::process::ProcessIdentity;
use headwork_core::{Actor, JobState, MachineMode, RecapKind};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::queries::insert_event;
use crate::records::TaskId;
use crate::{NOW, Store, StoreError};

/// A park.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParkRow {
    /// The row id.
    pub id: i64,
    /// When it began.
    pub started_at: String,
    /// When the tool stops waiting and forces what is left.
    pub deadline: String,
    /// When everything was stopped.
    pub finished_at: Option<String>,
    /// When the resume finished.
    pub resumed_at: Option<String>,
    /// The resume's summary file.
    pub recap_path: Option<String>,
}

/// A recap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecapRow {
    /// The row id.
    pub id: i64,
    /// The task, if any.
    pub task_id: Option<TaskId>,
    /// The session that wrote it, or that it is about.
    pub session_id: Option<String>,
    /// The park it belongs to, if any.
    pub park_id: Option<i64>,
    /// Why it was written.
    pub kind: RecapKind,
    /// The text.
    pub body: String,
    /// When.
    pub created_at: String,
}

/// A recap to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRecap<'a> {
    /// The task.
    pub task_id: Option<TaskId>,
    /// The session.
    pub session_id: Option<&'a str>,
    /// Why it is written.
    pub kind: RecapKind,
    /// The text.
    pub body: &'a str,
}

/// A long-running process registered with the tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobRow {
    /// The row id.
    pub id: i64,
    /// The task that owns it.
    pub task_id: Option<TaskId>,
    /// The project.
    pub project: String,
    /// What it is, in a few words.
    pub label: String,
    /// What a park does with it.
    pub park_policy: ParkPolicy,
    /// The command line that starts it again.
    pub argv: Vec<String>,
    /// The folder it runs in.
    pub cwd: String,
    /// Its process id while it runs.
    pub pid: Option<u32>,
    /// What the process under `pid` was when the pid was recorded; absent for jobs recorded before identities were kept.
    pub identity: Option<ProcessIdentity>,
    /// The file a `checkpoint` park waits on.
    pub checkpoint_path: Option<String>,
    /// It holds a heavy slot.
    pub heavy: bool,
    /// Resume starts it again after a park.
    pub restart: bool,
    /// Where it is.
    pub state: JobState,
    /// Its output.
    pub log_path: String,
    /// When it was registered.
    pub started_at: String,
    /// What happened to it at the last park or resume.
    pub note: Option<String>,
}

/// A job to register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewJob<'a> {
    /// The owning task.
    pub task_id: Option<TaskId>,
    /// The registering session.
    pub session_id: Option<&'a str>,
    /// The project.
    pub project: &'a str,
    /// What it is.
    pub label: &'a str,
    /// What a park does with it.
    pub park_policy: ParkPolicy,
    /// The command line that starts it.
    pub argv: &'a [String],
    /// The folder it runs in.
    pub cwd: &'a str,
    /// Its process id.
    pub pid: Option<u32>,
    /// What the process under `pid` is.
    pub identity: Option<&'a ProcessIdentity>,
    /// The checkpoint file.
    pub checkpoint_path: Option<&'a str>,
    /// It holds a heavy slot.
    pub heavy: bool,
    /// Resume starts it again.
    pub restart: bool,
    /// Where its output goes.
    pub log_path: &'a str,
}

const JOB_COLUMNS: &str = "id, task_id, project, label, park_policy, argv, cwd, pid, checkpoint_path, heavy, restart, \
                           state, log_path, started_at, note, pid_image, pid_started";

type RawJob = (
    i64,
    Option<i64>,
    String,
    String,
    String,
    String,
    String,
    Option<i64>,
    Option<String>,
    bool,
    bool,
    String,
    String,
    String,
    Option<String>,
    (Option<String>, Option<i64>),
);

fn read_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawJob> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        (row.get(15)?, row.get(16)?),
    ))
}

fn into_job(raw: RawJob) -> Result<JobRow, StoreError> {
    let (
        id,
        task,
        project,
        label,
        policy,
        argv,
        cwd,
        pid,
        checkpoint_path,
        heavy,
        restart,
        state,
        log_path,
        started_at,
        note,
        (pid_image, pid_started),
    ) = raw;
    Ok(JobRow {
        id,
        task_id: task.map(TaskId),
        project,
        label,
        park_policy: policy.parse()?,
        argv: serde_json::from_str(&argv).map_err(StoreError::Json)?,
        cwd,
        pid: pid
            .map(|n| u32::try_from(n).map_err(|_| StoreError::OutOfRange(n)))
            .transpose()?,
        identity: identity(pid_image, pid_started)?,
        checkpoint_path,
        heavy,
        restart,
        state: state.parse()?,
        log_path,
        started_at,
        note,
    })
}

pub(crate) fn identity(
    image: Option<String>,
    started: Option<i64>,
) -> Result<Option<ProcessIdentity>, StoreError> {
    match (image, started) {
        (Some(image), Some(started)) => Ok(Some(ProcessIdentity {
            image,
            started: u64::try_from(started).map_err(|_| StoreError::OutOfRange(started))?,
        })),
        _ => Ok(None),
    }
}

pub(crate) fn identity_columns(
    identity: Option<&ProcessIdentity>,
) -> Result<(Option<&str>, Option<i64>), StoreError> {
    identity.map_or(Ok((None, None)), |id| {
        let started = i64::try_from(id.started).map_err(|_| StoreError::OutOfRange(i64::MAX))?;
        Ok((Some(id.image.as_str()), Some(started)))
    })
}

const PARK_COLUMNS: &str = "id, started_at, deadline, finished_at, resumed_at, recap_path";

fn read_park(row: &rusqlite::Row<'_>) -> rusqlite::Result<ParkRow> {
    Ok(ParkRow {
        id: row.get(0)?,
        started_at: row.get(1)?,
        deadline: row.get(2)?,
        finished_at: row.get(3)?,
        resumed_at: row.get(4)?,
        recap_path: row.get(5)?,
    })
}

type RawRecap = (
    i64,
    Option<i64>,
    Option<String>,
    Option<i64>,
    String,
    String,
    String,
);

fn into_recap(raw: RawRecap) -> Result<RecapRow, StoreError> {
    let (id, task, session, park, kind, body, created_at) = raw;
    Ok(RecapRow {
        id,
        task_id: task.map(TaskId),
        session_id: session,
        park_id: park,
        kind: kind.parse()?,
        body,
        created_at,
    })
}

const RECAP_COLUMNS: &str = "id, task_id, session_id, park_id, kind, body, created_at";

fn read_recap(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRecap> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
    ))
}

impl Store {
    /// Starts a park: a row with a deadline `deadline_s` seconds ahead, and the machine in `parking`.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn begin_park(&mut self, deadline_s: u64, actor: Actor) -> Result<ParkRow, StoreError> {
        let tx = self.conn.transaction()?;
        let seconds = i64::try_from(deadline_s).map_err(|_| StoreError::OutOfRange(i64::MAX))?;
        tx.execute(
            &format!(
                "INSERT INTO parks (started_at, deadline) VALUES ({NOW}, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+' || ?1 || ' seconds'))"
            ),
            params![seconds],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            &format!(
                "UPDATE machine SET mode = 'parking', park_id = ?1, updated_at = {NOW} WHERE id = 1"
            ),
            params![id],
        )?;
        insert_event(
            &tx,
            actor,
            None,
            None,
            "park-begun",
            &serde_json::json!({ "park": id, "deadline_s": deadline_s }),
        )?;
        let row = tx.query_row(
            &format!("SELECT {PARK_COLUMNS} FROM parks WHERE id = ?1"),
            params![id],
            read_park,
        )?;
        tx.commit()?;
        Ok(row)
    }

    /// A park by id.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn park(&self, id: i64) -> Result<Option<ParkRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {PARK_COLUMNS} FROM parks WHERE id = ?1"),
                params![id],
                read_park,
            )
            .optional()?)
    }

    /// The park the machine is in, or was last in.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn current_park(&self) -> Result<Option<ParkRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {PARK_COLUMNS} FROM parks WHERE id = (SELECT park_id FROM machine WHERE id = 1)"
                ),
                [],
                read_park,
            )
            .optional()?)
    }

    /// Whether the park's deadline passed, by SQLite's clock.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn park_deadline_passed(&self, id: i64) -> Result<bool, StoreError> {
        Ok(self.conn.query_row(
            &format!("SELECT deadline <= {NOW} FROM parks WHERE id = ?1"),
            params![id],
            |row| row.get(0),
        )?)
    }

    /// Marks every session stopped: the park is finished and the machine `parked`.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn finish_park(&mut self, id: i64, actor: Actor) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            &format!("UPDATE parks SET finished_at = {NOW} WHERE id = ?1"),
            params![id],
        )?;
        tx.execute(
            &format!("UPDATE machine SET mode = 'parked', updated_at = {NOW} WHERE id = 1"),
            [],
        )?;
        insert_event(
            &tx,
            actor,
            None,
            None,
            "park-finished",
            &serde_json::json!({ "park": id }),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Marks the resume finished: the machine is `normal` again and the park carries the summary file.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn finish_resume(
        &mut self,
        park: Option<i64>,
        recap_path: &str,
        actor: Actor,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        if let Some(id) = park {
            tx.execute(
                &format!("UPDATE parks SET resumed_at = {NOW}, recap_path = ?2 WHERE id = ?1"),
                params![id, recap_path],
            )?;
        }
        tx.execute(
            &format!("UPDATE machine SET mode = 'normal', updated_at = {NOW} WHERE id = 1"),
            [],
        )?;
        insert_event(
            &tx,
            actor,
            None,
            None,
            "resume-finished",
            &serde_json::json!({ "park": park, "recap": recap_path }),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Records a recap. During a park it belongs to that park.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn add_recap(&mut self, recap: &NewRecap<'_>, actor: Actor) -> Result<i64, StoreError> {
        let tx = self.conn.transaction()?;
        let park: Option<i64> = if recap.kind == RecapKind::Session
            && tx.query_row("SELECT mode FROM machine WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })? == MachineMode::Parking.as_str()
        {
            tx.query_row("SELECT park_id FROM machine WHERE id = 1", [], |row| {
                row.get(0)
            })?
        } else {
            None
        };
        tx.execute(
            &format!(
                "INSERT INTO recaps (task_id, session_id, park_id, kind, body, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, {NOW})"
            ),
            params![
                recap.task_id.map(|id| id.0),
                recap.session_id,
                park,
                recap.kind.as_str(),
                recap.body
            ],
        )?;
        let id = tx.last_insert_rowid();
        insert_event(
            &tx,
            actor,
            recap.task_id,
            recap.session_id,
            "recap",
            &serde_json::json!({ "recap": id, "kind": recap.kind, "park": park, "chars": recap.body.len() }),
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Records a recap of this kind for a park the caller names, without the machine-mode lookup.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn add_park_recap(
        &mut self,
        park: i64,
        recap: &NewRecap<'_>,
        actor: Actor,
    ) -> Result<i64, StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            &format!(
                "INSERT INTO recaps (task_id, session_id, park_id, kind, body, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, {NOW})"
            ),
            params![
                recap.task_id.map(|id| id.0),
                recap.session_id,
                park,
                recap.kind.as_str(),
                recap.body
            ],
        )?;
        let id = tx.last_insert_rowid();
        insert_event(
            &tx,
            actor,
            recap.task_id,
            recap.session_id,
            "recap",
            &serde_json::json!({ "recap": id, "kind": recap.kind, "park": park, "chars": recap.body.len() }),
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// The newest recap of a task.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn latest_recap(&self, task: TaskId) -> Result<Option<RecapRow>, StoreError> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {RECAP_COLUMNS} FROM recaps WHERE task_id = ?1 ORDER BY id DESC LIMIT 1"
                ),
                params![task.0],
                read_recap,
            )
            .optional()?
            .map(into_recap)
            .transpose()
    }

    /// The recap a session wrote during a park, if it did.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn session_park_recap(
        &self,
        park: i64,
        task: TaskId,
    ) -> Result<Option<RecapRow>, StoreError> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {RECAP_COLUMNS} FROM recaps WHERE park_id = ?1 AND task_id = ?2 AND kind = 'session'
                     ORDER BY id DESC LIMIT 1"
                ),
                params![park, task.0],
                read_recap,
            )
            .optional()?
            .map(into_recap)
            .transpose()
    }

    /// Registers a job.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn register_job(&mut self, job: &NewJob<'_>, actor: Actor) -> Result<i64, StoreError> {
        let tx = self.conn.transaction()?;
        let argv = serde_json::to_string(job.argv).map_err(StoreError::Json)?;
        let (image, started) = identity_columns(job.identity)?;
        tx.execute(
            &format!(
                "INSERT INTO jobs (task_id, session_id, project, label, park_policy, argv, cwd, pid, checkpoint_path, heavy,
                                   restart, state, log_path, started_at, pid_image, pid_started)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'running', ?12, {NOW}, ?13, ?14)"
            ),
            params![
                job.task_id.map(|id| id.0),
                job.session_id,
                job.project,
                job.label,
                job.park_policy.as_str(),
                argv,
                job.cwd,
                job.pid.map(i64::from),
                job.checkpoint_path,
                job.heavy,
                job.restart,
                job.log_path,
                image,
                started,
            ],
        )?;
        let id = tx.last_insert_rowid();
        insert_event(
            &tx,
            actor,
            job.task_id,
            job.session_id,
            "job-registered",
            &serde_json::json!({
                "job": id, "label": job.label, "policy": job.park_policy, "pid": job.pid, "image": image, "restart": job.restart,
            }),
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// A job by id.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn job(&self, id: i64) -> Result<Option<JobRow>, StoreError> {
        self.conn
            .query_row(
                &format!("SELECT {JOB_COLUMNS} FROM jobs WHERE id = ?1"),
                params![id],
                read_job,
            )
            .optional()?
            .map(into_job)
            .transpose()
    }

    /// Jobs in a state, oldest first; with a task, only that task's.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn jobs_in(
        &self,
        state: JobState,
        task: Option<TaskId>,
    ) -> Result<Vec<JobRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {JOB_COLUMNS} FROM jobs WHERE state = ?1 AND (?2 IS NULL OR task_id = ?2) ORDER BY id"
        ))?;
        let rows = stmt.query_map(params![state.as_str(), task.map(|id| id.0)], read_job)?;
        rows.map(|raw| into_job(raw?)).collect()
    }

    /// Every job that is not over, oldest first; with a task, only that task's.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn open_jobs(&self, task: Option<TaskId>) -> Result<Vec<JobRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {JOB_COLUMNS} FROM jobs WHERE state IN ('running', 'stopping', 'parked')
             AND (?1 IS NULL OR task_id = ?1) ORDER BY id"
        ))?;
        let rows = stmt.query_map(params![task.map(|id| id.0)], read_job)?;
        rows.map(|raw| into_job(raw?)).collect()
    }

    /// Every job that recorded a process, in any state, oldest first: `reap` never ends one that still runs.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn jobs_with_pid(&self) -> Result<Vec<JobRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {JOB_COLUMNS} FROM jobs WHERE pid IS NOT NULL ORDER BY id"
        ))?;
        let rows = stmt.query_map([], read_job)?;
        rows.map(|raw| into_job(raw?)).collect()
    }

    /// Moves a job to a state with a note, and logs it.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn set_job_state(
        &mut self,
        id: i64,
        state: JobState,
        note: Option<&str>,
        actor: Actor,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        let over = matches!(state, JobState::Done | JobState::Failed);
        tx.execute(
            &format!(
                "UPDATE jobs SET state = ?1, note = COALESCE(?2, note),
                     ended_at = CASE WHEN ?3 THEN {NOW} ELSE ended_at END WHERE id = ?4"
            ),
            params![state.as_str(), note, over, id],
        )?;
        let task: Option<i64> = tx.query_row(
            "SELECT task_id FROM jobs WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        insert_event(
            &tx,
            actor,
            task.map(TaskId),
            None,
            "job-state",
            &serde_json::json!({ "job": id, "state": state, "note": note }),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Records the process a job runs as, after a restart, with what that process is.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn set_job_pid(
        &mut self,
        id: i64,
        pid: u32,
        identity: Option<&ProcessIdentity>,
        actor: Actor,
    ) -> Result<(), StoreError> {
        let (image, started) = identity_columns(identity)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE jobs SET pid = ?1, pid_image = ?2, pid_started = ?3, state = 'running', ended_at = NULL, note = NULL
             WHERE id = ?4",
            params![i64::from(pid), image, started, id],
        )?;
        let task: Option<i64> = tx.query_row(
            "SELECT task_id FROM jobs WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        insert_event(
            &tx,
            actor,
            task.map(TaskId),
            None,
            "job-started",
            &serde_json::json!({ "job": id, "pid": pid, "image": image }),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Marks an open job over without touching its process, and logs why.
    ///
    /// Returns `false` when the job is already over.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error, `QueryReturnedNoRows` when no job has this id.
    pub fn forget_job(&mut self, id: i64, reason: &str, actor: Actor) -> Result<bool, StoreError> {
        let tx = self.conn.transaction()?;
        let note = format!("forgotten: {reason}");
        let changed = tx.execute(
            &format!(
                "UPDATE jobs SET state = 'done', note = ?1, ended_at = {NOW}
                 WHERE id = ?2 AND state IN ('running', 'stopping', 'parked')"
            ),
            params![note, id],
        )?;
        let (task, pid): (Option<i64>, Option<i64>) = tx.query_row(
            "SELECT task_id, pid FROM jobs WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if changed > 0 {
            insert_event(
                &tx,
                actor,
                task.map(TaskId),
                None,
                "job-forgotten",
                &serde_json::json!({ "job": id, "pid": pid, "reason": reason }),
            )?;
        }
        tx.commit()?;
        Ok(changed > 0)
    }

    /// Supersedes every ask of a task that still waits and returns what each one asked.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn supersede_asks(
        &mut self,
        task: TaskId,
        actor: Actor,
    ) -> Result<Vec<(String, String)>, StoreError> {
        let tx = self.conn.transaction()?;
        let asked: Vec<(i64, String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT id, question, options FROM asks WHERE task_id = ?1 AND state IN ('pending', 'open') ORDER BY id",
            )?;
            let rows = stmt.query_map(params![task.0], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
            rows.collect::<Result<_, _>>()?
        };
        for (id, _, _) in &asked {
            tx.execute(
                "UPDATE asks SET state = 'superseded' WHERE id = ?1",
                params![id],
            )?;
            insert_event(
                &tx,
                actor,
                Some(task),
                None,
                "ask-superseded",
                &serde_json::json!({ "ask": id }),
            )?;
        }
        tx.commit()?;
        Ok(asked.into_iter().map(|(_, q, o)| (q, o)).collect())
    }

    /// Records the safety copy a park made of a worktree's uncommitted changes.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn set_park_ref(&mut self, task: TaskId, reference: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE worktrees SET park_ref = ?1 WHERE task_id = ?2 AND helper = 0",
            params![reference, task.0],
        )?;
        Ok(())
    }

    /// Gives a resumed session its pane again: it is `starting` until its `SessionStart` arrives.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn session_resumed(&mut self, id: &str, actor: Actor) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        let task: Option<i64> = tx.query_row(
            "SELECT task_id FROM sessions WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        tx.execute(
            &format!(
                "UPDATE sessions SET state = 'starting', launcher_pid = NULL, claude_pid = NULL, launcher_image = NULL,
                     launcher_started = NULL, claude_image = NULL, claude_started = NULL, pids_at = NULL, ended_at = NULL,
                     exit_code = NULL, started_at = {NOW} WHERE id = ?1"
            ),
            params![id],
        )?;
        insert_event(
            &tx,
            actor,
            task.map(TaskId),
            Some(id),
            "session-resumed",
            &serde_json::json!({}),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Tasks in a state, in the order they were last changed.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn tasks_by_age(&self, state: headwork_core::TaskState) -> Result<Vec<TaskId>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM tasks WHERE state = ?1 ORDER BY updated_at, id")?;
        let rows = stmt.query_map(params![state.as_str()], |row| row.get(0))?;
        Ok(rows
            .collect::<Result<Vec<i64>, _>>()?
            .into_iter()
            .map(TaskId)
            .collect())
    }
}

impl Store {
    /// The questions of a task's asks that still wait on the user, with their options.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn waiting_questions(&self, task: TaskId) -> Result<Vec<(String, String)>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT question, options FROM asks WHERE task_id = ?1 AND state IN ('pending', 'open') ORDER BY id",
        )?;
        let rows = stmt.query_map(params![task.0], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

impl Store {
    /// Records the folder a session runs in, so a resume can start it there again.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn set_session_dir(&mut self, id: &str, dir: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE sessions SET dir = ?1 WHERE id = ?2",
            params![dir, id],
        )?;
        Ok(())
    }

    /// The folder a session runs in, when it was recorded.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn session_dir(&self, id: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT dir FROM sessions WHERE id = ?1",
                params![id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Records the orchestrator session a park found running; the resume after it brings that session back.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn set_park_orchestrator(
        &mut self,
        park: i64,
        session: &str,
        actor: Actor,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE parks SET orchestrator_session = ?1 WHERE id = ?2",
            params![session, park],
        )?;
        insert_event(
            &tx,
            actor,
            None,
            Some(session),
            "park-orchestrator",
            &serde_json::json!({ "park": park }),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The orchestrator session a park found running.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn park_orchestrator(&self, park: i64) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT orchestrator_session FROM parks WHERE id = ?1",
                params![park],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// The newest recap a session without a task (the orchestrator) left for a park, of any kind.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn orchestrator_park_recap(
        &self,
        park: i64,
        session: &str,
    ) -> Result<Option<RecapRow>, StoreError> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {RECAP_COLUMNS} FROM recaps WHERE park_id = ?1 AND session_id = ?2 AND task_id IS NULL
                     ORDER BY id DESC LIMIT 1"
                ),
                params![park, session],
                read_recap,
            )
            .optional()?
            .map(into_recap)
            .transpose()
    }

    /// Takes the recap the orchestrator `session` wrote in the turn that started the park `park` as its park recap:
    /// its newest own recap outside any park, written at or after its latest prompt. The `park` tool asks the
    /// orchestrator to write its recap first, so that recap exists before the park does. Returns the recap's id, or
    /// `None` when there is no such recap (then the park asks the orchestrator for one).
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn adopt_orchestrator_recap(
        &mut self,
        park: i64,
        session: &str,
        actor: Actor,
    ) -> Result<Option<i64>, StoreError> {
        let tx = self.conn.transaction()?;
        let recap: Option<i64> = tx
            .query_row(
                "SELECT id FROM recaps
                 WHERE session_id = ?1 AND task_id IS NULL AND park_id IS NULL AND kind = 'session'
                   AND created_at >= (SELECT MAX(at) FROM events WHERE session_id = ?1 AND type = 'prompt')
                 ORDER BY id DESC LIMIT 1",
                params![session],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = recap {
            tx.execute(
                "UPDATE recaps SET park_id = ?1 WHERE id = ?2",
                params![park, id],
            )?;
            insert_event(
                &tx,
                actor,
                None,
                Some(session),
                "recap-adopted",
                &serde_json::json!({ "park": park, "recap": id }),
            )?;
        }
        tx.commit()?;
        Ok(recap)
    }

    /// The newest recap of a session, of any kind.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn latest_session_recap(&self, session: &str) -> Result<Option<RecapRow>, StoreError> {
        self.conn
            .query_row(
                &format!("SELECT {RECAP_COLUMNS} FROM recaps WHERE session_id = ?1 ORDER BY id DESC LIMIT 1"),
                params![session],
                read_recap,
            )
            .optional()?
            .map(into_recap)
            .transpose()
    }
}
