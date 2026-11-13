//! The terminal host (design section 4.1): workspaces, panes, screens, keys, blocked states and notifications.
//!
//! [`TerminalHost`] is all the rest of headwork sees. [`wmux::Wmux`] drives wmux on Windows; [`fake::FakeHost`]
//! records calls for tests.

pub mod fake;
pub mod wmux;

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Why a host operation failed.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// The host program could not be run.
    #[error("running {program}: {source}")]
    Run {
        /// The program.
        program: String,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// The host answered with a failure.
    #[error("`{command}` failed: {message}")]
    Failed {
        /// The command line, without the program.
        command: String,
        /// What the host printed.
        message: String,
    },
    /// The host's answer did not parse.
    #[error("`{command}` printed something unexpected: {source}")]
    Parse {
        /// The command line, without the program.
        command: String,
        /// The parser's error.
        #[source]
        source: serde_json::Error,
    },
    /// The host is not running.
    #[error("the terminal host is not running")]
    NotRunning,
    /// Something the host should have made cannot be found.
    #[error("{0}")]
    Missing(String),
}

macro_rules! host_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// The host's spelling.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

host_id!(
    /// A workspace, such as wmux's `ws-…`.
    WorkspaceId
);
host_id!(
    /// A surface: one terminal tab in a pane, such as wmux's `surf-…`.
    SurfaceId
);
host_id!(
    /// A program the host started and tracks, such as wmux's `agent-…`.
    AgentId
);

/// A workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Workspace {
    /// The id.
    pub id: WorkspaceId,
    /// The title; headwork names workspaces after projects.
    pub title: String,
}

/// A program the host started in a surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pane {
    /// The host's id for the program.
    pub agent: AgentId,
    /// The surface it runs in.
    pub surface: SurfaceId,
    /// The workspace, when the host says.
    pub workspace: Option<WorkspaceId>,
    /// The label it was started with; headwork uses `<project>/<slug>`.
    pub label: String,
    /// The command line it runs.
    pub command: String,
    /// Whether it still runs.
    pub running: bool,
    /// Its process id.
    pub pid: Option<u32>,
    /// When it started, in milliseconds since the epoch.
    pub started_ms: Option<u64>,
}

/// What a surface's program is doing, as the host sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SurfaceState {
    /// The surface.
    pub surface: SurfaceId,
    /// The host's word, such as `working`, `idle` or `blocked`.
    pub state: String,
    /// The Claude session the host identified in it.
    pub session_id: Option<String>,
    /// Why it is blocked.
    pub blocked_reason: Option<String>,
}

/// A key to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Enter.
    Enter,
    /// Escape.
    Escape,
    /// A printable key.
    Char(char),
    /// A key with Ctrl held.
    Ctrl(char),
}

/// One answer of a blocked state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Choice {
    /// A stable id.
    pub id: String,
    /// What the button says.
    pub label: String,
    /// The key the button sends, such as `1`.
    pub key: String,
}

/// A terminal multiplexer.
pub trait TerminalHost: fmt::Debug {
    /// Fails when the host is not running.
    ///
    /// # Errors
    ///
    /// Returns [`HostError::NotRunning`] or the failure.
    fn ping(&self) -> Result<(), HostError>;

    /// Every workspace.
    ///
    /// # Errors
    ///
    /// Returns the host's failure.
    fn workspaces(&self) -> Result<Vec<Workspace>, HostError>;

    /// The workspace with this title, created in `cwd` when missing. Returns the id and whether it was created.
    ///
    /// # Errors
    ///
    /// Returns the host's failure.
    fn ensure_workspace(&self, title: &str, cwd: &Path) -> Result<(WorkspaceId, bool), HostError>;

    /// Starts `argv` in a new surface of the workspace, labelled `label`. The program runs directly, not through a
    /// shell. In a `fresh_workspace` it replaces the shell the workspace was created with, so the workspace holds only
    /// the program and closes with it.
    ///
    /// # Errors
    ///
    /// Returns the host's failure, or [`HostError::Missing`] when the started program cannot be found afterwards.
    fn spawn(
        &self,
        workspace: &WorkspaceId,
        cwd: &Path,
        label: &str,
        argv: &[String],
        fresh_workspace: bool,
    ) -> Result<Pane, HostError>;

    /// Every program the host started, running or exited.
    ///
    /// # Errors
    ///
    /// Returns the host's failure.
    fn panes(&self) -> Result<Vec<Pane>, HostError>;

    /// The surfaces of a workspace.
    ///
    /// # Errors
    ///
    /// Returns the host's failure.
    fn surfaces(&self, workspace: &WorkspaceId) -> Result<Vec<SurfaceId>, HostError>;

    /// What every surface's program is doing.
    ///
    /// # Errors
    ///
    /// Returns the host's failure.
    fn surface_states(&self) -> Result<Vec<SurfaceState>, HostError>;

    /// The last `lines` lines of a surface's screen.
    ///
    /// # Errors
    ///
    /// Returns the host's failure.
    fn read_screen(&self, surface: &SurfaceId, lines: u32) -> Result<String, HostError>;

    /// Types text into a surface, without Enter.
    ///
    /// # Errors
    ///
    /// Returns the host's failure.
    fn send_text(&self, surface: &SurfaceId, text: &str) -> Result<(), HostError>;

    /// Sends one key to a surface.
    ///
    /// # Errors
    ///
    /// Returns the host's failure.
    fn send_key(&self, surface: &SurfaceId, key: Key) -> Result<(), HostError>;

    /// Closes a surface.
    ///
    /// # Errors
    ///
    /// Returns the host's failure.
    fn close_surface(&self, surface: &SurfaceId) -> Result<(), HostError>;

    /// Closes a workspace with everything in it.
    ///
    /// # Errors
    ///
    /// Returns the host's failure.
    fn close_workspace(&self, workspace: &WorkspaceId) -> Result<(), HostError>;

    /// Ends a program the host started.
    ///
    /// # Errors
    ///
    /// Returns the host's failure.
    fn kill_agent(&self, agent: &AgentId) -> Result<(), HostError>;

    /// Marks a surface as waiting on the user, with buttons that send keys.
    ///
    /// # Errors
    ///
    /// Returns the host's failure.
    fn show_blocked(
        &self,
        surface: &SurfaceId,
        question: &str,
        choices: &[Choice],
    ) -> Result<(), HostError>;

    /// Clears a surface's blocked state.
    ///
    /// # Errors
    ///
    /// Returns the host's failure.
    fn clear_blocked(&self, surface: &SurfaceId) -> Result<(), HostError>;

    /// Shows a notification.
    ///
    /// # Errors
    ///
    /// Returns the host's failure.
    fn notify(&self, text: &str) -> Result<(), HostError>;
}

/// Joins an argv into one Windows command line, quoting arguments with spaces or quotes the way the C runtime parses
/// them back.
#[must_use]
pub fn command_line(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_owned();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0_usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            other => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(other);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}
