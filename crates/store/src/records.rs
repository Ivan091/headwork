//! Rows read back from the store.

use std::fmt;

use headwork_core::{
    Actor, AskKind, AskState, Effort, MachineMode, Model, PermissionMode, ProjectName,
    SessionState, Slug, TaskName, TaskSnapshot, TaskState,
};
use serde::{Serialize, Serializer};

/// A task's row id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct TaskId(pub i64);

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// An ask's row id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct AskId(pub i64);

fn serialize_display<T: fmt::Display, S: Serializer>(
    value: &T,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}

/// A task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Task {
    /// The row id.
    pub id: TaskId,
    /// `<project>-<slug>`.
    #[serde(serialize_with = "serialize_display")]
    pub name: TaskName,
    /// The task kind.
    pub kind: String,
    /// The lifecycle state.
    pub state: TaskState,
    /// Where a parked or interrupted task returns.
    pub state_before_park: Option<TaskState>,
    /// The model.
    pub model: Model,
    /// The effort; the machine default when absent.
    pub effort: Option<Effort>,
    /// The permission mode its sessions start in.
    pub permission_mode: PermissionMode,
    /// A standing role.
    pub keep: bool,
    /// Runs in the main checkout.
    pub in_main: bool,
    /// The task it waits for.
    pub depends_on: Option<TaskId>,
    /// The Fable approval ask.
    pub fable_ask: Option<AskId>,
    /// The brief file.
    pub brief_path: String,
    /// The last report.
    pub last_report: Option<String>,
    /// When it was queued.
    pub queued_at: String,
    /// When it last changed.
    pub updated_at: String,
}

impl Task {
    /// The effort as shown next to the model: its word, or `default effort` when the machine default applies.
    #[must_use]
    pub fn effort_label(&self) -> String {
        self.effort.map_or_else(
            || "default effort".to_owned(),
            |effort| format!("{effort} effort"),
        )
    }

    /// The parts the lifecycle reads.
    #[must_use]
    pub const fn snapshot(&self) -> TaskSnapshot {
        TaskSnapshot {
            state: self.state,
            state_before_park: self.state_before_park,
            keep: self.keep,
        }
    }

    /// The project.
    #[must_use]
    pub const fn project(&self) -> &ProjectName {
        self.name.project()
    }

    /// The slug.
    #[must_use]
    pub const fn slug(&self) -> &Slug {
        self.name.slug()
    }
}

/// A configured project as the store knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectRow {
    /// The name.
    pub name: ProjectName,
    /// The main checkout.
    pub main_path: String,
    /// Still in the config and enabled.
    pub enabled: bool,
}

/// A task's latest session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionRow {
    /// The Claude session id.
    pub id: String,
    /// The process state.
    pub state: SessionState,
    /// The Remote Control name.
    pub rc_name: String,
    /// The host workspace.
    pub host_workspace: Option<String>,
    /// The host pane.
    pub host_surface: Option<String>,
    /// The launcher process.
    pub launcher_pid: Option<u32>,
    /// The `claude` process.
    pub claude_pid: Option<u32>,
    /// The last hook event.
    pub last_event_at: Option<String>,
    /// The exit code of `claude`, once it ended.
    pub exit_code: Option<i32>,
}

/// An ask.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AskRow {
    /// The row id.
    pub id: AskId,
    /// The task, if any.
    pub task_id: Option<TaskId>,
    /// What it waits on.
    pub kind: AskKind,
    /// The question.
    pub question: String,
    /// The options, in order.
    pub options: Vec<String>,
    /// Where it is.
    pub state: AskState,
    /// The answer.
    pub answer: Option<String>,
    /// When it was created.
    pub created_at: String,
}

/// A task's pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrRow {
    /// The URL.
    pub url: String,
    /// `open`, `merged` or `closed`.
    pub state: String,
}

/// One task on the board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoardTask {
    /// The task.
    #[serde(flatten)]
    pub task: Task,
    /// Its latest session.
    pub session: Option<SessionRow>,
    /// Its asks that still wait on the user.
    pub asks: Vec<AskRow>,
    /// Its latest pull request.
    pub pr: Option<PrRow>,
}

/// The whole board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Board {
    /// The machine mode.
    pub machine: MachineMode,
    /// Tasks that take a child slot.
    pub live_children: u32,
    /// Known projects.
    pub projects: Vec<ProjectRow>,
    /// Tasks, by project and queue order.
    pub tasks: Vec<BoardTask>,
    /// Asks without a task, such as the root's.
    pub other_asks: Vec<AskRow>,
}

/// An event from the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventRecord {
    /// The row id.
    pub id: i64,
    /// When.
    pub at: String,
    /// Who.
    pub actor: Actor,
    /// The task's row id.
    pub task_id: Option<TaskId>,
    /// The task's name.
    pub task: Option<String>,
    /// The session.
    pub session_id: Option<String>,
    /// The event type.
    #[serde(rename = "type")]
    pub kind: String,
    /// The details.
    pub data: serde_json::Value,
}

/// A task's worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorktreeRow {
    /// The folder.
    pub path: String,
    /// The branch.
    pub branch: String,
    /// `present`, `removing`, `removed` or `kept`.
    pub state: String,
}
