//! Reads and writes of tasks, sessions, asks, projects and events.

use headwork_core::lifecycle::{Dependency, FableGate, StartFacts, droppable};
use headwork_core::{
    Actor, AskKind, AskState, Config, Effort, MachineMode, PartSlots, PermissionMode,
    ProjectConfig, ProjectName, Refusal, Role, SessionState, Slug, Step, TaskName, TaskRequest,
    TaskShape, TaskState, Trigger, admit, apply,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use crate::records::{
    AskId, AskRow, Board, BoardTask, EventRecord, PrRow, ProjectRow, Task, TaskId,
};
use crate::waiting::DIALOG_ASK_KINDS;
use crate::{NOW, Store, StoreError};

/// The answer to a Fable approval ask that lets the task start.
pub const FABLE_ALLOW: &str = "Allow Fable";

const FABLE_DECLINE: &str = "Use Opus instead";

const TASK_COLUMNS: &str = "id, project, slug, kind, state, state_before_park, model, keep, in_main, depends_on, \
                            fable_ask, brief_path, last_report, queued_at, updated_at, permission_mode, effort, parent_id, lead";

/// A task to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTask {
    /// What was asked for.
    pub request: TaskRequest,
    /// The brief file.
    pub brief_path: String,
    /// The task it waits for.
    pub depends_on: Option<TaskId>,
    /// The permission mode its sessions start in.
    pub permission_mode: PermissionMode,
    /// A lead, a part of a lead, or neither.
    pub shape: TaskShape<TaskId>,
}

/// A session to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    /// The Claude session id.
    pub id: String,
    /// The task, none for the orchestrator.
    pub task_id: Option<TaskId>,
    /// The role.
    pub role: Role,
    /// The Remote Control name.
    pub rc_name: String,
    /// The terminal host.
    pub host: String,
    /// The host workspace.
    pub host_workspace: Option<String>,
    /// The host pane.
    pub host_surface: Option<String>,
}

/// An ask to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAsk {
    /// The task, if any.
    pub task_id: Option<TaskId>,
    /// The asking session, if any.
    pub session_id: Option<String>,
    /// What it waits on.
    pub kind: AskKind,
    /// The question.
    pub question: String,
    /// The options, in order.
    pub options: Vec<String>,
}

/// Which events to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventFilter {
    /// Only this task's events.
    pub task: Option<TaskId>,
    /// At most this many, the newest, returned oldest first.
    pub limit: u32,
}

struct RawTask {
    id: i64,
    project: String,
    slug: String,
    kind: String,
    state: String,
    state_before_park: Option<String>,
    model: String,
    keep: bool,
    in_main: bool,
    depends_on: Option<i64>,
    fable_ask: Option<i64>,
    brief_path: String,
    last_report: Option<String>,
    queued_at: String,
    updated_at: String,
    permission_mode: String,
    effort: Option<String>,
    parent_id: Option<i64>,
    lead: bool,
}

impl RawTask {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            project: row.get(1)?,
            slug: row.get(2)?,
            kind: row.get(3)?,
            state: row.get(4)?,
            state_before_park: row.get(5)?,
            model: row.get(6)?,
            keep: row.get(7)?,
            in_main: row.get(8)?,
            depends_on: row.get(9)?,
            fable_ask: row.get(10)?,
            brief_path: row.get(11)?,
            last_report: row.get(12)?,
            queued_at: row.get(13)?,
            updated_at: row.get(14)?,
            permission_mode: row.get(15)?,
            effort: row.get(16)?,
            parent_id: row.get(17)?,
            lead: row.get(18)?,
        })
    }

    fn into_task(self) -> Result<Task, StoreError> {
        Ok(Task {
            id: TaskId(self.id),
            name: TaskName::new(ProjectName::new(self.project)?, Slug::new(self.slug)?),
            kind: self.kind,
            state: self.state.parse()?,
            state_before_park: self
                .state_before_park
                .map(|state| state.parse())
                .transpose()?,
            model: self.model.parse()?,
            permission_mode: self.permission_mode.parse()?,
            effort: self.effort.map(|effort| effort.parse()).transpose()?,
            keep: self.keep,
            in_main: self.in_main,
            depends_on: self.depends_on.map(TaskId),
            shape: TaskShape::from_columns(self.parent_id.map(TaskId), self.lead)?,
            fable_ask: self.fable_ask.map(AskId),
            brief_path: self.brief_path,
            last_report: self.last_report,
            queued_at: self.queued_at,
            updated_at: self.updated_at,
        })
    }
}

pub(crate) fn insert_event(
    conn: &Connection,
    actor: Actor,
    task: Option<TaskId>,
    session: Option<&str>,
    kind: &str,
    data: &serde_json::Value,
) -> Result<(), StoreError> {
    conn.execute(
        &format!("INSERT INTO events (at, actor, task_id, session_id, type, data) VALUES ({NOW}, ?1, ?2, ?3, ?4, ?5)"),
        params![actor.as_str(), task.map(|id| id.0), session, kind, data.to_string()],
    )?;
    Ok(())
}

fn parts_of(conn: &Connection, lead: TaskId) -> Result<Vec<TaskId>, StoreError> {
    Ok(conn
        .prepare("SELECT id FROM tasks WHERE parent_id = ?1 ORDER BY id")?
        .query_map(params![lead.0], |row| row.get(0).map(TaskId))?
        .collect::<Result<_, _>>()?)
}

/// Applies a trigger inside a transaction and logs the transition; a refusal comes back with the task, unlogged,
/// for the caller to log once the transaction is rolled back.
fn apply_in(
    tx: &Transaction<'_>,
    id: TaskId,
    trigger: &Trigger,
    actor: Actor,
) -> Result<Result<Step, (Task, Refusal)>, StoreError> {
    let task = load_task(tx, id)?;
    match apply(&task.snapshot(), trigger) {
        Ok(step) => {
            tx.execute(
                &format!("UPDATE tasks SET state = ?1, state_before_park = ?2, updated_at = {NOW} WHERE id = ?3"),
                params![step.to.as_str(), step.state_before_park.map(TaskState::as_str), id.0],
            )?;
            let data =
                serde_json::json!({ "trigger": trigger.name(), "from": step.from, "to": step.to });
            insert_event(tx, actor, Some(id), None, "transition", &data)?;
            Ok(Ok(step))
        }
        Err(refusal) => Ok(Err((task, refusal))),
    }
}

/// Logs a refused transition and returns the error to give back.
fn log_refusal(
    conn: &Connection,
    task: &Task,
    trigger: &Trigger,
    actor: Actor,
    refusal: Refusal,
) -> Result<StoreError, StoreError> {
    let data = serde_json::json!({
        "trigger": trigger.name(), "state": task.state, "reason": refusal.to_string(),
    });
    insert_event(conn, actor, Some(task.id), None, "refused", &data)?;
    Ok(StoreError::Refused {
        task: task.name.to_string(),
        refusal,
    })
}

pub(crate) fn load_task(conn: &Connection, id: TaskId) -> Result<Task, StoreError> {
    conn.query_row(
        &format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1"),
        params![id.0],
        RawTask::read,
    )
    .optional()?
    .ok_or(StoreError::UnknownTask(id.0))?
    .into_task()
}

fn machine_mode(conn: &Connection) -> Result<MachineMode, StoreError> {
    let mode: String = conn.query_row("SELECT mode FROM machine WHERE id = 1", [], |row| {
        row.get(0)
    })?;
    Ok(mode.parse()?)
}

/// A stable FNV-1a hash of a project's config, to see when it changed.
fn config_hash(project: &ProjectConfig) -> Result<String, StoreError> {
    let text = serde_json::to_string(project)?;
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    Ok(format!("{hash:016x}"))
}

pub(crate) fn upsert_project(
    tx: &Transaction<'_>,
    name: &ProjectName,
    project: &ProjectConfig,
) -> Result<bool, StoreError> {
    let hash = config_hash(project)?;
    let enabled = project.enabled;
    let changed = tx.execute(
        "INSERT INTO projects (name, main_path, config_hash, enabled) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(name) DO UPDATE SET main_path = excluded.main_path, config_hash = excluded.config_hash,
             enabled = excluded.enabled
         WHERE projects.main_path <> excluded.main_path OR projects.config_hash <> excluded.config_hash
             OR projects.enabled <> excluded.enabled",
        params![name.as_str(), project.main, hash, enabled],
    )?;
    Ok(changed > 0)
}

struct LiveTask {
    id: i64,
    parent_id: Option<i64>,
    project: String,
    kind: String,
    state: TaskState,
    in_main: bool,
    session_over: bool,
    waiting: bool,
}

impl LiveTask {
    /// Its session is starting, running or closing, waiting on the user or not: it holds its pane and checkout.
    const fn holds_slot(&self) -> bool {
        self.state.holds_slot(self.session_over)
    }

    /// It holds a slot and works: its session does not wait on the user with nothing else running.
    const fn works(&self) -> bool {
        self.holds_slot() && !self.waiting
    }

    /// It holds a slot only to wait on the user, running nothing else.
    const fn waits(&self) -> bool {
        self.holds_slot() && self.waiting
    }
}

fn live_tasks(conn: &Connection) -> Result<Vec<LiveTask>, StoreError> {
    let mut statement = conn.prepare(
        &format!(
            "SELECT t.id, t.parent_id, t.project, t.kind, t.state, t.in_main,
                    COALESCE((SELECT s.state IN ('ended', 'gone') FROM sessions s WHERE s.task_id = t.id
                              ORDER BY s.started_at DESC, s.rowid DESC LIMIT 1), 1),
                    COALESCE((SELECT s.waiting_since IS NOT NULL AND EXISTS (SELECT 1 FROM asks a
                                  WHERE a.session_id = s.id AND a.state IN ('pending', 'open')
                                    AND a.kind IN {DIALOG_ASK_KINDS})
                              FROM sessions s WHERE s.task_id = t.id
                              ORDER BY s.started_at DESC, s.rowid DESC LIMIT 1), 0)
             FROM tasks t WHERE t.state <> 'closed'"
        ),
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                (row.get(0)?, row.get(1)?),
                row.get::<_, String>(2)?,
                row.get(3)?,
                row.get::<_, String>(4)?,
                row.get(5)?,
                (row.get(6)?, row.get(7)?),
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(
            |((id, parent_id), project, kind, state, in_main, (session_over, waiting))| {
                Ok(LiveTask {
                    id,
                    parent_id,
                    project,
                    kind,
                    state: state.parse()?,
                    in_main,
                    session_over,
                    waiting,
                })
            },
        )
        .collect()
}

/// A task ending (`closing` or `closed`) whose recorded pull requests are all merged, there being at least one: it
/// delivered even when it closed without passing through `merged`, as a forced close from `in_review` does. `?1` is the
/// task's id.
const DELIVERED: &str = "state IN ('closing', 'closed')
    AND EXISTS (SELECT 1 FROM prs WHERE task_id = ?1)
    AND NOT EXISTS (SELECT 1 FROM prs WHERE task_id = ?1 AND state <> 'merged')";

/// Checks that a new part may go under `lead`: a live lead of the same project.
fn check_lead(conn: &Connection, lead: TaskId, project: &ProjectName) -> Result<(), StoreError> {
    let task = load_task(conn, lead)?;
    let refuse = |reason| StoreError::NotALead {
        lead: task.name.to_string(),
        reason,
    };
    if !task.shape.is_lead() {
        return Err(refuse("it is not a lead"));
    }
    if task.project() != project {
        return Err(refuse(
            "a part runs in its lead's project; another project's work is a new task",
        ));
    }
    if !droppable(task.state) {
        return Err(refuse("the lead is closing or over"));
    }
    Ok(())
}

fn count(n: usize) -> Result<u32, StoreError> {
    u32::try_from(n).map_err(|_| StoreError::OutOfRange(i64::MAX))
}

impl Store {
    /// Records the configured projects: new and changed ones are written, ones gone from the config are disabled.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn sync_projects(&mut self, config: &Config) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        let mut changed = Vec::new();
        for (name, project) in &config.projects {
            if upsert_project(&tx, name, project)? {
                changed.push(name.as_str().to_owned());
            }
        }
        let known: Vec<String> = tx
            .prepare("SELECT name FROM projects WHERE enabled = 1")?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        for name in known {
            if !config
                .projects
                .keys()
                .any(|configured| configured.as_str() == name)
            {
                tx.execute(
                    "UPDATE projects SET enabled = 0 WHERE name = ?1",
                    params![name],
                )?;
                changed.push(name);
            }
        }
        if !changed.is_empty() {
            insert_event(
                &tx,
                Actor::User,
                None,
                None,
                "projects-synced",
                &serde_json::json!({ "changed": changed }),
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Creates a task in `queued` after the admission guard. A Fable task that needs the user's approval gets a tool
    /// ask, which must be answered with [`FABLE_ALLOW`] before the task may start.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotAdmitted`] when the guard refuses, or the SQLite error.
    pub fn create_task(
        &mut self,
        config: &Config,
        new: &NewTask,
        actor: Actor,
    ) -> Result<Task, StoreError> {
        let request = &new.request;
        let tx = self.conn.transaction()?;
        let name_taken: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM tasks WHERE project = ?1 AND slug = ?2 AND state <> 'closed')",
            params![request.project.as_str(), request.slug.as_str()],
            |row| row.get(0),
        )?;
        let admitted = admit(config, request, name_taken, machine_mode(&tx)?)?;
        if let TaskShape::Part { parent } = new.shape {
            check_lead(&tx, parent, &request.project)?;
        }
        if let Some(project) = config.project(&request.project) {
            upsert_project(&tx, &request.project, project)?;
        }
        tx.execute(
            &format!(
                "INSERT INTO tasks (project, slug, kind, state, model, keep, in_main, depends_on, brief_path, queued_at,
                     updated_at, permission_mode, effort, parent_id, lead)
                 VALUES (?1, ?2, ?3, 'queued', ?4, ?5, ?6, ?7, ?8, {NOW}, {NOW}, ?9, ?10, ?11, ?12)"
            ),
            params![
                request.project.as_str(),
                request.slug.as_str(),
                request.kind,
                admitted.model.as_str(),
                request.keep,
                request.in_main,
                new.depends_on.map(|id| id.0),
                new.brief_path,
                new.permission_mode.as_str(),
                request.effort.map(Effort::as_str),
                new.shape.parent().map(|id| id.0),
                new.shape.is_lead(),
            ],
        )?;
        let id = TaskId(tx.last_insert_rowid());
        let name = TaskName::new(request.project.clone(), request.slug.clone());
        if admitted.needs_fable_approval {
            let ask = NewAsk {
                task_id: Some(id),
                session_id: None,
                kind: AskKind::Tool,
                question: format!("Allow Fable for {name}?"),
                options: vec![FABLE_ALLOW.to_owned(), FABLE_DECLINE.to_owned()],
            };
            let ask_id = insert_ask(&tx, &ask, AskState::Pending)?;
            tx.execute(
                "UPDATE tasks SET fable_ask = ?1 WHERE id = ?2",
                params![ask_id.0, id.0],
            )?;
        }
        let data = serde_json::json!({
            "name": name.to_string(), "kind": request.kind, "model": admitted.model, "effort": admitted.effort, "in_main": request.in_main,
            "keep": request.keep, "shape": new.shape,
        });
        insert_event(&tx, actor, Some(id), None, "task-created", &data)?;
        tx.commit()?;
        load_task(&self.conn, id)
    }

    /// The task with this id.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::UnknownTask`] or a read error.
    pub fn task(&self, id: TaskId) -> Result<Task, StoreError> {
        load_task(&self.conn, id)
    }

    /// Finds a task by `<project>-<slug>` or `<project>/<slug>`: the live one, else the newest closed one.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn find_task(&self, name: &str) -> Result<Option<Task>, StoreError> {
        let projects = self.projects()?;
        let Some(name) = TaskName::resolve(name, projects.iter().map(|project| &project.name))
        else {
            return Ok(None);
        };
        self.conn
            .query_row(
                &format!(
                    "SELECT {TASK_COLUMNS} FROM tasks WHERE project = ?1 AND slug = ?2
                     ORDER BY state <> 'closed' DESC, id DESC LIMIT 1"
                ),
                params![name.project().as_str(), name.slug().as_str()],
                RawTask::read,
            )
            .optional()?
            .map(RawTask::into_task)
            .transpose()
    }

    /// Moves a queued task to `spawning` when the start guard passes, with the facts read from the store: slots,
    /// the project cap, main-only kinds, the dependency and the Fable approval.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Refused`] when the guard refuses, or a read or write error.
    pub fn start_task(
        &mut self,
        config: &Config,
        id: TaskId,
        actor: Actor,
    ) -> Result<Step, StoreError> {
        let facts = self.start_facts(config, id)?;
        self.transition(id, &Trigger::Start(facts), actor)
    }

    fn start_facts(&self, config: &Config, id: TaskId) -> Result<StartFacts, StoreError> {
        let task = load_task(&self.conn, id)?;
        let live = live_tasks(&self.conn)?;
        let slots = live.iter().filter(|other| other.holds_slot());
        let working = live.iter().filter(|other| other.works());
        let project = task.project().as_str();
        let project_config = config.project(task.project());
        Ok(StartFacts {
            live_workers: count(working.clone().count())?,
            waiting_workers: count(live.iter().filter(|other| other.waits()).count())?,
            max_workers: config.machine.max_workers.get(),
            idle_overflow: config.machine.idle_overflow,
            project_live: count(working.filter(|other| other.project == project).count())?,
            project_cap: project_config
                .and_then(|p| p.max_workers)
                .map(std::num::NonZeroU32::get),
            main_only_kind_busy: task.in_main
                && slots
                    .filter(|other| {
                        other.project == project && other.in_main && other.kind == task.kind
                    })
                    .count()
                    > 0,
            part_slots: match task.shape {
                TaskShape::Part { parent } => Some(PartSlots {
                    live: count(
                        live.iter()
                            .filter(|other| {
                                other.parent_id == Some(parent.0)
                                    && other.id != id.0
                                    && other.holds_slot()
                            })
                            .count(),
                    )?,
                    max_parts: config.leads.max_parts.get(),
                }),
                TaskShape::Lead | TaskShape::Plain => None,
            },
            dependency: self.dependency(task.depends_on)?,
            fable: self.fable_gate(&task)?,
            machine: machine_mode(&self.conn)?,
        })
    }

    fn dependency(&self, depends_on: Option<TaskId>) -> Result<Dependency, StoreError> {
        let Some(id) = depends_on else {
            return Ok(Dependency::None);
        };
        let merged: bool = self.conn.query_row(
            &format!(
                "SELECT state = 'merged' OR EXISTS (SELECT 1 FROM events WHERE task_id = ?1 AND type = 'transition'
                     AND json_extract(data, '$.to') = 'merged') OR ({DELIVERED})
                 FROM tasks WHERE id = ?1"
            ),
            params![id.0],
            |row| row.get(0),
        )?;
        Ok(if merged {
            Dependency::Merged
        } else {
            Dependency::Waiting
        })
    }

    /// The ending dependencies (`closing` or `closed`) of queued tasks that the store does not count merged yet: their
    /// recorded pull requests may be stale, so the queue asks the git host again before it starts anything.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn unsettled_dependencies(&self) -> Result<Vec<TaskId>, StoreError> {
        let ids: Vec<i64> = self
            .conn
            .prepare(
                "SELECT DISTINCT d.id FROM tasks q JOIN tasks d ON d.id = q.depends_on
                 WHERE q.state = 'queued' AND d.state IN ('closing', 'closed') AND NOT EXISTS (
                     SELECT 1 FROM events WHERE task_id = d.id AND type = 'transition'
                         AND json_extract(data, '$.to') = 'merged')
                 ORDER BY d.id",
            )?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(ids.into_iter().map(TaskId).collect())
    }

    fn fable_gate(&self, task: &Task) -> Result<FableGate, StoreError> {
        let Some(ask) = task.fable_ask else {
            return Ok(FableGate::NotNeeded);
        };
        let answer: Option<String> = self
            .conn
            .query_row(
                "SELECT answer FROM asks WHERE id = ?1 AND state = 'answered'",
                params![ask.0],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok(if answer.as_deref() == Some(FABLE_ALLOW) {
            FableGate::Allowed
        } else {
            FableGate::Waiting
        })
    }

    /// Applies a trigger to a task and logs the transition, or logs the refusal and returns it.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Refused`] when the lifecycle refuses, or a read or write error.
    pub fn transition(
        &mut self,
        id: TaskId,
        trigger: &Trigger,
        actor: Actor,
    ) -> Result<Step, StoreError> {
        let tx = self.conn.transaction()?;
        match apply_in(&tx, id, trigger, actor)? {
            Ok(step) => {
                tx.commit()?;
                Ok(step)
            }
            Err((task, refusal)) => {
                drop(tx);
                Err(log_refusal(&self.conn, &task, trigger, actor, refusal)?)
            }
        }
    }

    /// Drops a task, and for a lead every part of it that may still be dropped, in one transaction: a lead's drop
    /// cascades to its live parts. Returns the parts dropped, oldest first; each still has to be closed.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Refused`] when the task itself may not be dropped, changing nothing, or a read or write
    /// error.
    pub fn drop_task(&mut self, id: TaskId, actor: Actor) -> Result<Vec<TaskId>, StoreError> {
        let tx = self.conn.transaction()?;
        let task = match apply_in(&tx, id, &Trigger::Drop, actor)? {
            Ok(_) => load_task(&tx, id)?,
            Err((task, refusal)) => {
                drop(tx);
                return Err(log_refusal(
                    &self.conn,
                    &task,
                    &Trigger::Drop,
                    actor,
                    refusal,
                )?);
            }
        };
        let mut dropped = Vec::new();
        if task.shape.is_lead() {
            for part in parts_of(&tx, id)? {
                if droppable(load_task(&tx, part)?.state) {
                    apply_in(&tx, part, &Trigger::Drop, actor)?.map_err(|(part, refusal)| {
                        StoreError::Refused {
                            task: part.name.to_string(),
                            refusal,
                        }
                    })?;
                    insert_event(
                        &tx,
                        actor,
                        Some(part),
                        None,
                        "drop-cascade",
                        &serde_json::json!({ "lead": task.name.to_string() }),
                    )?;
                    dropped.push(part);
                }
            }
        }
        tx.commit()?;
        Ok(dropped)
    }

    /// The parts of a lead, oldest first, closed ones included.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn parts(&self, lead: TaskId) -> Result<Vec<TaskId>, StoreError> {
        parts_of(&self.conn, lead)
    }

    /// How many parts of a lead are not closed yet: while any is, the lead may not finish (`lead-parts-live`).
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn live_parts(&self, lead: TaskId) -> Result<u32, StoreError> {
        let live: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE parent_id = ?1 AND state <> 'closed'",
            params![lead.0],
            |row| row.get(0),
        )?;
        u32::try_from(live).map_err(|_| StoreError::OutOfRange(live))
    }

    /// Records a new session in `starting`.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn record_session(&mut self, session: &NewSession, actor: Actor) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            &format!(
                "INSERT INTO sessions (id, task_id, role, rc_name, runtime, host, host_workspace, host_surface, state, started_at)
                 VALUES (?1, ?2, ?3, ?4, 'pane', ?5, ?6, ?7, 'starting', {NOW})"
            ),
            params![
                session.id,
                session.task_id.map(|id| id.0),
                session.role.as_str(),
                session.rc_name,
                session.host,
                session.host_workspace,
                session.host_surface,
            ],
        )?;
        let data = serde_json::json!({ "rc_name": session.rc_name, "role": session.role });
        insert_event(
            &tx,
            actor,
            session.task_id,
            Some(&session.id),
            "session-recorded",
            &data,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Sets a session's state; a move to `ended` or `gone` records the end unless one is recorded already.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn set_session_state(
        &mut self,
        id: &str,
        state: SessionState,
        actor: Actor,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        let task: Option<i64> = tx.query_row(
            "SELECT task_id FROM sessions WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        tx.execute(
            &format!(
                "UPDATE sessions SET state = ?1, last_event_at = {NOW},
                     ended_at = CASE WHEN ?1 IN ('ended', 'gone') THEN COALESCE(ended_at, {NOW}) ELSE ended_at END
                 WHERE id = ?2"
            ),
            params![state.as_str(), id],
        )?;
        insert_event(
            &tx,
            actor,
            task.map(TaskId),
            Some(id),
            "session-state",
            &serde_json::json!({ "state": state }),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Records an ask put to the user.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn open_ask(&mut self, ask: &NewAsk, actor: Actor) -> Result<AskId, StoreError> {
        let tx = self.conn.transaction()?;
        let id = insert_ask(&tx, ask, AskState::Open)?;
        let data = serde_json::json!({ "ask": id.0, "kind": ask.kind, "question": ask.question });
        insert_event(
            &tx,
            actor,
            ask.task_id,
            ask.session_id.as_deref(),
            "ask-opened",
            &data,
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Records the answer to an ask.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn answer_ask(&mut self, id: AskId, answer: &str, actor: Actor) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        let (task, session): (Option<i64>, Option<String>) = tx.query_row(
            "SELECT task_id, session_id FROM asks WHERE id = ?1",
            params![id.0],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        tx.execute(
            &format!(
                "UPDATE asks SET state = 'answered', answer = ?1, answered_at = {NOW} WHERE id = ?2"
            ),
            params![answer, id.0],
        )?;
        let data = serde_json::json!({ "ask": id.0, "answer": answer });
        insert_event(
            &tx,
            actor,
            task.map(TaskId),
            session.as_deref(),
            "ask-answered",
            &data,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The known projects, by name.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn projects(&self) -> Result<Vec<ProjectRow>, StoreError> {
        let rows: Vec<(String, String, bool)> = self
            .conn
            .prepare("SELECT name, main_path, enabled FROM projects ORDER BY name")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<Result<_, _>>()?;
        rows.into_iter()
            .map(|(name, main_path, enabled)| {
                Ok(ProjectRow {
                    name: ProjectName::new(name)?,
                    main_path,
                    enabled,
                })
            })
            .collect()
    }

    /// The board: the machine mode, projects, and every task that is not closed (all tasks with `all`), each with
    /// its latest session, waiting asks and latest pull request.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn board(&self, all: bool) -> Result<Board, StoreError> {
        let filter = if all { "" } else { "WHERE state <> 'closed'" };
        let tasks: Vec<RawTask> = self
            .conn
            .prepare(&format!(
                "SELECT {TASK_COLUMNS} FROM tasks {filter} ORDER BY project, id"
            ))?
            .query_map([], RawTask::read)?
            .collect::<Result<_, _>>()?;
        let asks = self.waiting_asks()?;
        let tokens = self.usage_task_tokens()?;
        let mut board_tasks = Vec::with_capacity(tasks.len());
        for raw in tasks {
            let task = raw.into_task()?;
            let task_asks = asks
                .iter()
                .filter(|ask| ask.task_id == Some(task.id))
                .cloned()
                .collect();
            let models = if task.shape.is_lead() {
                tokens.tree(task.id)
            } else {
                tokens.own(task.id)
            };
            board_tasks.push(BoardTask {
                tokens: crate::usage::all_models(&models).total(),
                models,
                session: self.session_of(task.id)?,
                pr: self.latest_pr(task.id)?,
                review: self.review_summary(task.id)?,
                asks: task_asks,
                task,
            });
        }
        let live = live_tasks(&self.conn)?;
        Ok(Board {
            machine: machine_mode(&self.conn)?,
            live_workers: count(live.iter().filter(|task| task.works()).count())?,
            waiting_workers: count(live.iter().filter(|task| task.waits()).count())?,
            projects: self.projects()?,
            tasks: board_tasks,
            other_asks: asks
                .into_iter()
                .filter(|ask| ask.task_id.is_none())
                .collect(),
        })
    }

    fn waiting_asks(&self) -> Result<Vec<AskRow>, StoreError> {
        type RawAsk = (
            i64,
            Option<i64>,
            String,
            String,
            String,
            String,
            Option<String>,
            String,
        );
        let rows: Vec<RawAsk> = self
            .conn
            .prepare(
                "SELECT id, task_id, kind, question, options, state, answer, created_at FROM asks
                 WHERE state IN ('pending', 'open') ORDER BY id",
            )?
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        rows.into_iter()
            .map(
                |(id, task_id, kind, question, options, state, answer, created_at)| {
                    Ok(AskRow {
                        id: AskId(id),
                        task_id: task_id.map(TaskId),
                        kind: kind.parse()?,
                        question,
                        options: serde_json::from_str(&options)?,
                        state: state.parse()?,
                        answer,
                        created_at,
                    })
                },
            )
            .collect()
    }

    fn latest_pr(&self, task: TaskId) -> Result<Option<PrRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT url, state FROM prs WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
                params![task.0],
                |row| {
                    Ok(PrRow {
                        url: row.get(0)?,
                        state: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    /// The newest events, oldest first.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn events(&self, filter: EventFilter) -> Result<Vec<EventRecord>, StoreError> {
        type RawEvent = (
            i64,
            String,
            String,
            Option<i64>,
            Option<String>,
            Option<String>,
            String,
            String,
        );
        let rows: Vec<RawEvent> = self
            .conn
            .prepare(
                "SELECT * FROM (
                     SELECT e.id, e.at, e.actor, e.task_id, t.project || '-' || t.slug, e.session_id, e.type, e.data
                     FROM events e LEFT JOIN tasks t ON t.id = e.task_id
                     WHERE ?1 IS NULL OR e.task_id = ?1
                     ORDER BY e.id DESC LIMIT ?2
                 ) ORDER BY id",
            )?
            .query_map(params![filter.task.map(|id| id.0), filter.limit], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        rows.into_iter()
            .map(|(id, at, actor, task_id, task, session_id, kind, data)| {
                Ok(EventRecord {
                    id,
                    at,
                    actor: actor.parse()?,
                    task_id: task_id.map(TaskId),
                    task,
                    session_id,
                    kind,
                    data: serde_json::from_str(&data)?,
                })
            })
            .collect()
    }
}

fn insert_ask(tx: &Transaction<'_>, ask: &NewAsk, state: AskState) -> Result<AskId, StoreError> {
    tx.execute(
        &format!(
            "INSERT INTO asks (task_id, session_id, kind, question, options, state, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, {NOW})"
        ),
        params![
            ask.task_id.map(|id| id.0),
            ask.session_id,
            ask.kind.as_str(),
            ask.question,
            serde_json::to_string(&ask.options)?,
            state.as_str(),
        ],
    )?;
    Ok(AskId(tx.last_insert_rowid()))
}
