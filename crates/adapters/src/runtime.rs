//! The agent runtime (design section 4.3): how a Claude Code session is started and how it learns who it is.

use headwork_core::{Effort, Model, PermissionMode};

/// The Claude session id, read by hooks and the MCP server.
pub const ENV_SESSION: &str = "HEADWORK_SESSION";
/// `root` or `child`.
pub const ENV_ROLE: &str = "HEADWORK_ROLE";
/// The task's id.
pub const ENV_TASK: &str = "HEADWORK_TASK";
/// The headwork home, so tools started inside the session use the same store.
pub const ENV_HOME: &str = "HEADWORK_HOME";

/// The `headwork` program, so the plugin's hooks can call it without `PATH`.
pub const ENV_EXE: &str = "HEADWORK_EXE";

/// The program a pane starts.
pub const CLAUDE: &str = "claude";

/// How to start one `claude` session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaudeLaunch<'a> {
    /// The fixed session id.
    pub session_id: &'a str,
    /// Continue the session with this id instead of starting it.
    pub resume: bool,
    /// The session's name, also its Remote Control name.
    pub name: &'a str,
    /// Start with Remote Control.
    pub remote_control: bool,
    /// The model.
    pub model: Model,
    /// The effort; claude's own default when absent.
    pub effort: Option<Effort>,
    /// The permission mode.
    pub permission_mode: PermissionMode,
    /// An agent definition, such as `root`.
    pub agent: Option<&'a str>,
    /// A plugin folder to load for this session only (`--plugin-dir`).
    pub plugin_dir: Option<&'a str>,
    /// The first prompt.
    pub prompt: &'a str,
}

impl ClaudeLaunch<'_> {
    /// The argv, program first.
    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        let mut argv = vec![CLAUDE.to_owned()];
        argv.push(
            if self.resume {
                "--resume"
            } else {
                "--session-id"
            }
            .to_owned(),
        );
        argv.push(self.session_id.to_owned());
        if self.remote_control {
            argv.extend(["--remote-control".to_owned(), self.name.to_owned()]);
        }
        argv.extend([
            "-n".to_owned(),
            self.name.to_owned(),
            "--model".to_owned(),
            self.model.as_str().to_owned(),
            "--permission-mode".to_owned(),
            self.permission_mode.as_str().to_owned(),
        ]);
        if let Some(effort) = self.effort {
            argv.extend(["--effort".to_owned(), effort.as_str().to_owned()]);
        }
        if let Some(agent) = self.agent {
            argv.extend(["--agent".to_owned(), agent.to_owned()]);
        }
        if let Some(dir) = self.plugin_dir {
            argv.extend(["--plugin-dir".to_owned(), dir.to_owned()]);
        }
        argv.extend(["--".to_owned(), self.prompt.to_owned()]);
        argv
    }
}
