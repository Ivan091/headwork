//! The wmux host (amirlehmam/wmux 2.60), driven through its command line.
//!
//! wmux's `wmux` and `wmux.cmd` are shims around a Node script, so the adapter runs that script with Node directly:
//! no shell, no `cmd` quoting. The program and script come from `WMUX_NODE` and `WMUX_CLI`, which wmux sets in its
//! panes, else from `node` on `PATH` and the default install folder.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use super::{
    AgentId, Choice, HostError, Key, Pane, PaneId, PaneTree, SurfaceId, SurfaceState, Target,
    TerminalHost, Workspace, WorkspaceId, command_line,
};

/// Runs one wmux command and returns what it printed.
pub trait Runner: std::fmt::Debug {
    /// Runs `wmux <args>` as if from the surface `caller` (wmux's `WMUX_SURFACE_ID`), else from wherever headwork runs.
    ///
    /// # Errors
    ///
    /// Returns [`HostError::Run`] when the program cannot start and [`HostError::Failed`] when it exits with a failure.
    fn run_as(&self, caller: Option<&str>, args: &[&str]) -> Result<String, HostError>;

    /// Runs `wmux <args>`.
    ///
    /// # Errors
    ///
    /// As [`Runner::run_as`].
    fn run(&self, args: &[&str]) -> Result<String, HostError> {
        self.run_as(None, args)
    }
}

/// The real wmux command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cli {
    node: PathBuf,
    script: PathBuf,
    electron: bool,
}

impl Cli {
    /// Finds the wmux script: `script` when given, else `WMUX_CLI`, else the default install folder under
    /// `LOCALAPPDATA`.
    #[must_use]
    pub fn locate(script: Option<&Path>) -> Self {
        let env = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        let script = script
            .map(Path::to_path_buf)
            .or_else(|| env("WMUX_CLI").map(PathBuf::from))
            .unwrap_or_else(|| {
                env("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_default()
                    .join("Programs/wmux/resources/cli/wmux.js")
            });
        let node = env("WMUX_NODE").map_or_else(|| PathBuf::from("node"), PathBuf::from);
        Self {
            node,
            script,
            electron: env("WMUX_NODE_ELECTRON").is_some(),
        }
    }
}

impl Runner for Cli {
    fn run_as(&self, caller: Option<&str>, args: &[&str]) -> Result<String, HostError> {
        let mut command = Command::new(&self.node);
        command
            .arg(&self.script)
            .args(args)
            .env("MSYS_NO_PATHCONV", "1");
        if let Some(caller) = caller {
            command.env("WMUX_SURFACE_ID", caller);
        }
        if self.electron {
            command.env("ELECTRON_RUN_AS_NODE", "1");
        }
        let output = command.output().map_err(|source| HostError::Run {
            program: self.node.display().to_string(),
            source,
        })?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if output.status.success() {
            Ok(stdout)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(HostError::Failed {
                command: args.join(" "),
                message: format!("{}{}", stderr.trim(), stdout.trim()),
            })
        }
    }
}

/// wmux behind a [`Runner`].
#[derive(Debug)]
pub struct Wmux<R> {
    runner: R,
}

impl<R: Runner> Wmux<R> {
    /// A host that runs commands through `runner`.
    pub const fn new(runner: R) -> Self {
        Self { runner }
    }

    fn quiet(&self, args: &[&str]) -> Result<(), HostError> {
        self.runner.run(args).map(drop)
    }
}

#[derive(Deserialize)]
struct TreeAnswer {
    tree: RawNode,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum RawNode {
    Leaf {
        #[serde(rename = "paneId")]
        pane_id: String,
        #[serde(default)]
        surfaces: Vec<RawSurface>,
    },
    Branch {
        direction: String,
        #[serde(default = "half")]
        ratio: f64,
        children: Vec<Self>,
    },
}

const fn half() -> f64 {
    0.5
}

impl From<RawNode> for PaneTree {
    fn from(raw: RawNode) -> Self {
        match raw {
            RawNode::Leaf { pane_id, surfaces } => Self::Leaf {
                pane: PaneId(pane_id),
                surfaces: surfaces.into_iter().map(|s| SurfaceId(s.id)).collect(),
            },
            RawNode::Branch {
                direction,
                ratio,
                children,
            } => Self::Split {
                down: direction == "vertical",
                ratio,
                parts: children.into_iter().map(Self::from).collect(),
            },
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GridAnswer {
    #[serde(default)]
    new_pane_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SplitAnswer {
    pane_id: String,
    surface_id: String,
}

/// Parses `wmux tree --workspace`. wmux calls a split whose parts sit side by side `horizontal` and one whose parts
/// are stacked `vertical`.
///
/// # Errors
///
/// Returns the parser's error.
pub fn parse_tree(text: &str) -> Result<PaneTree, serde_json::Error> {
    let answer: TreeAnswer = serde_json::from_str(text)?;
    Ok(answer.tree.into())
}

#[derive(Deserialize)]
struct AgentList {
    agents: Vec<RawAgent>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAgent {
    agent_id: String,
    surface_id: String,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    cmd: Option<String>,
    status: String,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    spawn_time: Option<u64>,
}

impl From<RawAgent> for Pane {
    fn from(raw: RawAgent) -> Self {
        Self {
            agent: AgentId(raw.agent_id),
            surface: SurfaceId(raw.surface_id),
            workspace: raw.workspace_id.map(WorkspaceId),
            label: raw.label.unwrap_or_default(),
            command: raw.cmd.unwrap_or_default(),
            running: raw.status == "running",
            pid: raw.pid,
            started_ms: raw.spawn_time,
        }
    }
}

#[derive(Deserialize)]
struct WorkspaceList {
    workspaces: Vec<RawWorkspace>,
}

#[derive(Deserialize)]
struct RawWorkspace {
    id: String,
    title: String,
}

#[derive(Deserialize)]
struct PaneList {
    panes: Vec<RawPane>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPane {
    #[serde(default)]
    surfaces: Vec<RawSurface>,
}

#[derive(Deserialize)]
struct RawSurface {
    id: String,
}

#[derive(Deserialize)]
struct StateList {
    states: Vec<RawState>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawState {
    surface_id: String,
    state: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    blocked_reason: Option<String>,
}

/// Parses `wmux agent list`.
///
/// # Errors
///
/// Returns the parser's error.
pub fn parse_agents(text: &str) -> Result<Vec<Pane>, serde_json::Error> {
    let list: AgentList = serde_json::from_str(text)?;
    Ok(list.agents.into_iter().map(Pane::from).collect())
}

/// Parses `wmux list-workspaces`.
///
/// # Errors
///
/// Returns the parser's error.
pub fn parse_workspaces(text: &str) -> Result<Vec<Workspace>, serde_json::Error> {
    let list: WorkspaceList = serde_json::from_str(text)?;
    Ok(list
        .workspaces
        .into_iter()
        .map(|raw| Workspace {
            id: WorkspaceId(raw.id),
            title: raw.title,
        })
        .collect())
}

/// Parses `wmux list-panes` into the surfaces of every pane.
///
/// # Errors
///
/// Returns the parser's error.
pub fn parse_surfaces(text: &str) -> Result<Vec<SurfaceId>, serde_json::Error> {
    let list: PaneList = serde_json::from_str(text)?;
    Ok(list
        .panes
        .into_iter()
        .flat_map(|pane| pane.surfaces)
        .map(|surface| SurfaceId(surface.id))
        .collect())
}

/// Parses `wmux agent-state` without a surface.
///
/// # Errors
///
/// Returns the parser's error.
pub fn parse_states(text: &str) -> Result<Vec<SurfaceState>, serde_json::Error> {
    let list: StateList = serde_json::from_str(text)?;
    Ok(list
        .states
        .into_iter()
        .map(|raw| SurfaceState {
            surface: SurfaceId(raw.surface_id),
            state: raw.state,
            session_id: raw.session_id,
            blocked_reason: raw.blocked_reason,
        })
        .collect())
}

impl<R: Runner> TerminalHost for Wmux<R> {
    fn ping(&self) -> Result<(), HostError> {
        match self.runner.run(&["ping"]) {
            Ok(text) if text.trim() == "pong" => Ok(()),
            Ok(_) | Err(HostError::Failed { .. }) => Err(HostError::NotRunning),
            Err(error) => Err(error),
        }
    }

    fn workspaces(&self) -> Result<Vec<Workspace>, HostError> {
        let text = self.runner.run(&["list-workspaces"])?;
        parse_workspaces(&text).map_err(|source| HostError::Parse {
            command: "list-workspaces".to_owned(),
            source,
        })
    }

    fn ensure_workspace(&self, title: &str, cwd: &Path) -> Result<(WorkspaceId, bool), HostError> {
        let find = |host: &Self| {
            host.workspaces().map(|all| {
                all.into_iter()
                    .find(|workspace| workspace.title == title)
                    .map(|workspace| workspace.id)
            })
        };
        if let Some(id) = find(self)? {
            return Ok((id, false));
        }
        let cwd = cwd.display().to_string();
        self.quiet(&[
            "new-workspace",
            "--title",
            title,
            "--cwd",
            &cwd,
            "--panes",
            "1",
            "--layout",
            "single",
        ])?;
        find(self)?
            .map(|id| (id, true))
            .ok_or_else(|| HostError::Missing(format!("workspace `{title}` was not created")))
    }

    fn spawn(
        &self,
        workspace: &WorkspaceId,
        cwd: &Path,
        label: &str,
        argv: &[String],
        target: &Target,
    ) -> Result<Pane, HostError> {
        let before: Vec<AgentId> = self.panes()?.into_iter().map(|pane| pane.agent).collect();
        let cwd = cwd.display().to_string();
        let cmd = command_line(argv);
        let mut command = vec![
            "agent",
            "spawn",
            "--workspace",
            workspace.as_str(),
            "--cwd",
            &cwd,
            "--label",
            label,
            "--cmd",
            &cmd,
        ];
        if let Target::Pane { pane, replace_tab } = target {
            command.extend(["--pane", pane.as_str()]);
            if *replace_tab {
                command.push("--replace-tab");
            }
        }
        self.quiet(&command)?;
        self.panes()?
            .into_iter()
            .filter(|pane| pane.label == label && !before.contains(&pane.agent))
            .max_by_key(|pane| pane.started_ms)
            .ok_or_else(|| HostError::Missing(format!("the pane `{label}` did not appear")))
    }

    fn layout(&self, workspace: &WorkspaceId) -> Result<PaneTree, HostError> {
        let args = ["tree", "--workspace", workspace.as_str()];
        let text = self.runner.run(&args)?;
        parse_tree(&text).map_err(|source| HostError::Parse {
            command: args.join(" "),
            source,
        })
    }

    fn split(&self, beside: &SurfaceId, down: bool) -> Result<(PaneId, SurfaceId), HostError> {
        let args: &[&str] = if down {
            &["split", "--down"]
        } else {
            &["split"]
        };
        let text = self.runner.run_as(Some(beside.as_str()), args)?;
        let answer: SplitAnswer =
            serde_json::from_str(&text).map_err(|source| HostError::Parse {
                command: args.join(" "),
                source,
            })?;
        Ok((PaneId(answer.pane_id), SurfaceId(answer.surface_id)))
    }

    fn grid(
        &self,
        workspace: &WorkspaceId,
        anchor: &PaneId,
        count: u32,
    ) -> Result<Vec<PaneId>, HostError> {
        let count = count.to_string();
        let args = [
            "layout",
            "grid",
            "--count",
            &count,
            "--anchor-pane",
            anchor.as_str(),
            "--workspace",
            workspace.as_str(),
        ];
        let text = self.runner.run(&args)?;
        let answer: GridAnswer =
            serde_json::from_str(&text).map_err(|source| HostError::Parse {
                command: args.join(" "),
                source,
            })?;
        Ok(answer.new_pane_ids.into_iter().map(PaneId).collect())
    }

    fn close_pane(&self, pane: &PaneId) -> Result<(), HostError> {
        self.quiet(&["close-pane", pane.as_str()])
    }

    fn panes(&self) -> Result<Vec<Pane>, HostError> {
        let text = self.runner.run(&["agent", "list"])?;
        parse_agents(&text).map_err(|source| HostError::Parse {
            command: "agent list".to_owned(),
            source,
        })
    }

    fn surfaces(&self, workspace: &WorkspaceId) -> Result<Vec<SurfaceId>, HostError> {
        let args = ["list-panes", "--workspace", workspace.as_str()];
        let text = self.runner.run(&args)?;
        parse_surfaces(&text).map_err(|source| HostError::Parse {
            command: args.join(" "),
            source,
        })
    }

    fn surface_states(&self) -> Result<Vec<SurfaceState>, HostError> {
        let text = self.runner.run(&["agent-state"])?;
        parse_states(&text).map_err(|source| HostError::Parse {
            command: "agent-state".to_owned(),
            source,
        })
    }

    fn read_screen(&self, surface: &SurfaceId, lines: u32) -> Result<String, HostError> {
        let lines = lines.to_string();
        self.runner.run(&[
            "read-screen",
            "--lines",
            &lines,
            "--surface",
            surface.as_str(),
        ])
    }

    fn send_text(&self, surface: &SurfaceId, text: &str) -> Result<(), HostError> {
        self.quiet(&["send", "--surface", surface.as_str(), text])
    }

    fn send_key(&self, surface: &SurfaceId, key: Key) -> Result<(), HostError> {
        let (name, ctrl) = match key {
            Key::Enter => ("enter".to_owned(), false),
            Key::Escape => ("esc".to_owned(), false),
            Key::Char(c) => (c.to_string(), false),
            Key::Ctrl(c) => (c.to_string(), true),
        };
        let mut args = vec!["send-key", name.as_str(), "--surface", surface.as_str()];
        if ctrl {
            args.push("--ctrl");
        }
        self.quiet(&args)
    }

    fn rename_surface(&self, surface: &SurfaceId, title: &str) -> Result<(), HostError> {
        self.quiet(&["rename-surface", surface.as_str(), title])
    }

    fn close_surface(&self, surface: &SurfaceId) -> Result<(), HostError> {
        self.quiet(&["close-surface", surface.as_str()])
    }

    fn close_workspace(&self, workspace: &WorkspaceId) -> Result<(), HostError> {
        self.quiet(&["close-workspace", workspace.as_str()])
    }

    fn kill_agent(&self, agent: &AgentId) -> Result<(), HostError> {
        self.quiet(&["agent", "kill", agent.as_str()])
    }

    fn show_blocked(
        &self,
        surface: &SurfaceId,
        question: &str,
        choices: &[Choice],
    ) -> Result<(), HostError> {
        let choices = serde_json::to_string(choices).map_err(|source| HostError::Parse {
            command: "report-agent".to_owned(),
            source,
        })?;
        self.quiet(&[
            "report-agent",
            "--blocked",
            question,
            "--choices",
            &choices,
            "--surface",
            surface.as_str(),
        ])
    }

    fn clear_blocked(&self, surface: &SurfaceId) -> Result<(), HostError> {
        self.quiet(&["report-agent", "--unblocked", "--surface", surface.as_str()])
    }

    fn notify(&self, text: &str) -> Result<(), HostError> {
        self.quiet(&["notify", text])
    }
}
