//! The queue order, and tasks adopted from a setup that ran before headwork.

use headwork_core::{Actor, Config, Model, PermissionMode, ProjectName, Role, Slug, TaskName};
use rusqlite::params;

use crate::queries::{insert_event, load_task, upsert_project};
use crate::records::{Task, TaskId};
use crate::{NOW, Store, StoreError};

/// A running session that another tool started, to adopt as a `working` task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptTask {
    /// The project.
    pub project: ProjectName,
    /// The slug.
    pub slug: Slug,
    /// The kind.
    pub kind: String,
    /// The model it runs.
    pub model: Model,
    /// It runs in the main checkout.
    pub in_main: bool,
    /// A standing role.
    pub keep: bool,
    /// Its brief, if one is known.
    pub brief_path: String,
    /// Its Claude session id, or a stand-in when the host does not know it.
    pub session_id: String,
    /// The terminal host.
    pub host: String,
    /// The host workspace.
    pub host_workspace: Option<String>,
    /// The host surface.
    pub host_surface: Option<String>,
    /// The `claude` process, when known.
    pub claude_pid: Option<u32>,
    /// Its worktree and branch.
    pub worktree: Option<(String, String)>,
    /// Where the facts came from, for the log.
    pub source: String,
}

impl Store {
    /// The queued tasks, in queue order: ranked ones first by rank, then the rest oldest first.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn queued(&self) -> Result<Vec<TaskId>, StoreError> {
        Ok(self
            .conn
            .prepare(
                "SELECT id FROM tasks WHERE state = 'queued'
                 ORDER BY queue_rank IS NULL, queue_rank, queued_at, id",
            )?
            .query_map([], |row| row.get(0).map(TaskId))?
            .collect::<Result<_, _>>()?)
    }

    /// The tasks in one state, oldest first.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn tasks_in(&self, state: headwork_core::TaskState) -> Result<Vec<TaskId>, StoreError> {
        Ok(self
            .conn
            .prepare("SELECT id FROM tasks WHERE state = ?1 ORDER BY id")?
            .query_map(params![state.as_str()], |row| row.get(0).map(TaskId))?
            .collect::<Result<_, _>>()?)
    }

    /// Puts these queued tasks first, in this order; the other queued tasks follow, oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotQueued`] when one of them is not queued, or a write error.
    pub fn set_queue_order(&mut self, order: &[TaskId], actor: Actor) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        for id in order {
            let task = load_task(&tx, *id)?;
            if task.state != headwork_core::TaskState::Queued {
                return Err(StoreError::NotQueued(task.name.to_string()));
            }
        }
        tx.execute(
            "UPDATE tasks SET queue_rank = NULL WHERE state = 'queued'",
            [],
        )?;
        for (rank, id) in (1_i64..).zip(order) {
            tx.execute(
                "UPDATE tasks SET queue_rank = ?1 WHERE id = ?2",
                params![rank, id.0],
            )?;
        }
        let ids: Vec<i64> = order.iter().map(|id| id.0).collect();
        insert_event(
            &tx,
            actor,
            None,
            None,
            "queue-ordered",
            &serde_json::json!({ "order": ids }),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Records a running session as a `working` task with its session and worktree, without the start guard: it
    /// already runs. Logged as `adopted`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NameTaken`] when a live task has the name, or a write error.
    pub fn adopt_task(
        &mut self,
        config: &Config,
        adopt: &AdoptTask,
        actor: Actor,
    ) -> Result<Task, StoreError> {
        let tx = self.conn.transaction()?;
        let name = TaskName::new(adopt.project.clone(), adopt.slug.clone());
        let taken: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM tasks WHERE project = ?1 AND slug = ?2 AND state <> 'closed')",
            params![adopt.project.as_str(), adopt.slug.as_str()],
            |row| row.get(0),
        )?;
        if taken {
            return Err(StoreError::NameTaken(name.to_string()));
        }
        if let Some(project) = config.project(&adopt.project) {
            upsert_project(&tx, &adopt.project, project)?;
        }
        tx.execute(
            &format!(
                "INSERT INTO tasks (project, slug, kind, state, model, keep, in_main, brief_path, queued_at, updated_at,
                     permission_mode)
                 VALUES (?1, ?2, ?3, 'working', ?4, ?5, ?6, ?7, {NOW}, {NOW}, ?8)"
            ),
            params![
                adopt.project.as_str(),
                adopt.slug.as_str(),
                adopt.kind,
                adopt.model.as_str(),
                adopt.keep,
                adopt.in_main,
                adopt.brief_path,
                PermissionMode::Auto.as_str(),
            ],
        )?;
        let id = TaskId(tx.last_insert_rowid());
        tx.execute(
            &format!(
                "INSERT INTO sessions (id, task_id, role, rc_name, runtime, host, host_workspace, host_surface, claude_pid,
                     state, started_at, last_event_at)
                 VALUES (?1, ?2, ?3, ?4, 'pane', ?5, ?6, ?7, ?8, 'live', {NOW}, {NOW})"
            ),
            params![
                adopt.session_id,
                id.0,
                Role::Worker.as_str(),
                name.to_string(),
                adopt.host,
                adopt.host_workspace,
                adopt.host_surface,
                adopt.claude_pid.map(i64::from),
            ],
        )?;
        if let Some((path, branch)) = &adopt.worktree {
            tx.execute(
                "INSERT INTO worktrees (task_id, path, branch, state) VALUES (?1, ?2, ?3, 'present')
                 ON CONFLICT(path) DO UPDATE SET task_id = excluded.task_id, branch = excluded.branch, state = 'present'",
                params![id.0, path, branch],
            )?;
        }
        let data = serde_json::json!({
            "name": name.to_string(), "session": adopt.session_id, "surface": adopt.host_surface,
            "worktree": adopt.worktree.as_ref().map(|(path, _)| path), "keep": adopt.keep, "source": adopt.source,
        });
        insert_event(
            &tx,
            actor,
            Some(id),
            Some(&adopt.session_id),
            "adopted",
            &data,
        )?;
        tx.commit()?;
        load_task(&self.conn, id)
    }
}
