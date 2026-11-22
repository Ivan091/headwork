//! The cmux host (manaflow-ai/cmux 0.65, macOS), driven through its command line.
//!
//! Nearly every call goes through `cmux rpc <method> <json>`, the CLI's passthrough to the v2 socket API, which takes
//! and prints JSON with stable UUIDs. The CLI's own sidebar commands (`set-status`, `clear-status`) and `sessions list`,
//! which reads cmux's Claude hook records, have no socket method and run as plain commands.
//!
//! cmux keeps no registry of the programs it started, so [`Cmux::spawn`] writes the label into the surface's initial
//! command, which `surface.list` gives back, and [`Cmux::panes`] reads it from there. A surface closes when its program
//! exits, so every program found is running. The table of every trait method and the cmux call behind it is in
//! `docs/cmux.md`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

use crate::timed::{self, TimedError};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::{
    AgentId, Choice, HostError, Key, Pane, PaneId, PaneTree, SurfaceId, SurfaceState, Target,
    TerminalHost, Workspace, WorkspaceId,
};

/// Runs one cmux command and returns what it printed.
pub trait Runner: std::fmt::Debug {
    /// Runs `cmux <args>`.
    ///
    /// # Errors
    ///
    /// Returns [`HostError::Run`] when the program cannot start and [`HostError::Failed`] when it exits with a failure.
    fn run(&self, args: &[&str]) -> Result<String, HostError>;
}

/// Where the cmux app keeps its command line.
pub const BUNDLED_CLI: &str = "/Applications/cmux.app/Contents/Resources/bin/cmux";

/// The real cmux command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cli {
    program: PathBuf,
}

impl Cli {
    /// Finds the cmux program: `program` when given, else `CMUX_BUNDLED_CLI_PATH`, which cmux sets in its terminals,
    /// else the one inside `/Applications/cmux.app`, else `cmux` on `PATH`.
    #[must_use]
    pub fn locate(program: Option<&Path>) -> Self {
        let program = program
            .map(Path::to_path_buf)
            .or_else(|| {
                std::env::var_os("CMUX_BUNDLED_CLI_PATH")
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
            })
            .or_else(|| Some(PathBuf::from(BUNDLED_CLI)).filter(|path| path.is_file()))
            .unwrap_or_else(|| PathBuf::from("cmux"));
        Self { program }
    }
}

/// How long one cmux call may take before it is ended and reported as a failure.
pub const CMUX_TIMEOUT: Duration = Duration::from_secs(60);

impl Runner for Cli {
    fn run(&self, args: &[&str]) -> Result<String, HostError> {
        let ran = timed::run(
            Command::new(&self.program)
                .args(args)
                .env("CMUX_QUIET", "1"),
            CMUX_TIMEOUT,
        )
        .map_err(|error| match error {
            TimedError::Start { source, .. } => HostError::Run {
                program: self.program.display().to_string(),
                source,
            },
            TimedError::TimedOut { .. } => HostError::Failed {
                command: args.join(" "),
                message: error.to_string(),
            },
        })?;
        if ran.success() {
            Ok(ran.stdout)
        } else {
            Err(HostError::Failed {
                command: args.join(" "),
                message: format!("{}{}", ran.stderr.trim(), ran.stdout.trim()),
            })
        }
    }
}

/// The variable a spawned program finds its label in, as the first word of the surface's initial command.
const LABEL_VAR: &str = "HEADWORK_HOST_LABEL";

/// What cmux prints for a handle that does not exist.
const NOT_FOUND: &str = "not_found";

/// What cmux prints when asked to close a workspace's only surface, which only closing the workspace does.
const LAST_SURFACE: &str = "Cannot close the last surface";

/// The sidebar status key of a surface's blocked state.
fn status_key(surface: &SurfaceId) -> String {
    format!("headwork-{surface}")
}

/// cmux behind a [`Runner`].
#[derive(Debug)]
pub struct Cmux<R> {
    runner: R,
    search_path: Option<String>,
}

impl<R: Runner> Cmux<R> {
    /// A host that runs commands through `runner` and gives the programs it starts this process's `PATH`.
    pub fn new(runner: R) -> Self {
        let search_path = std::env::var("PATH").ok().map(|path| without_shims(&path));
        Self {
            runner,
            search_path,
        }
    }

    /// The same host giving the programs it starts `search_path` instead.
    #[must_use]
    pub fn with_search_path(self, search_path: Option<String>) -> Self {
        Self {
            search_path,
            ..self
        }
    }

    fn rpc<T: DeserializeOwned>(&self, method: &str, params: &Value) -> Result<T, HostError> {
        let params = params.to_string();
        let text = self.runner.run(&["rpc", method, &params])?;
        serde_json::from_str(&text).map_err(|source| HostError::Parse {
            command: format!("rpc {method}"),
            source,
        })
    }

    fn rpc_quiet(&self, method: &str, params: &Value) -> Result<(), HostError> {
        let params = params.to_string();
        self.runner.run(&["rpc", method, &params]).map(drop)
    }

    fn tree(&self, workspace: Option<&WorkspaceId>) -> Result<Vec<RawWorkspace>, HostError> {
        let params = workspace.map_or_else(
            || json!({ "all": true }),
            |workspace| json!({ "workspace_id": workspace.as_str() }),
        );
        let tree: TreeAnswer = self.rpc("system.tree", &params)?;
        Ok(tree
            .windows
            .into_iter()
            .flat_map(|window| window.workspaces)
            .collect())
    }

    fn workspace(&self, workspace: &WorkspaceId) -> Result<RawWorkspace, HostError> {
        self.tree(Some(workspace))?
            .into_iter()
            .find(|raw| raw.id == workspace.as_str())
            .ok_or_else(|| HostError::Missing(format!("no workspace `{workspace}`")))
    }

    fn holding(&self, surface: &SurfaceId) -> Result<RawWorkspace, HostError> {
        self.tree(None)?
            .into_iter()
            .find(|raw| raw.panes.iter().any(|pane| pane.holds(surface)))
            .ok_or_else(|| HostError::Missing(format!("no workspace holds `{surface}`")))
    }

    fn landed_in(&self, created: &Created, asked: &WorkspaceId) -> Result<WorkspaceId, HostError> {
        let landed = match &created.workspace {
            Some(id) => id.clone(),
            None => self.holding(&SurfaceId(created.surface.clone()))?.id,
        };
        if landed == asked.as_str() {
            return Ok(asked.clone());
        }
        tolerate_missing(self.close_surface(&SurfaceId(created.surface.clone())))?;
        Err(HostError::Missing(format!(
            "cmux opened surface `{}` in workspace `{landed}`, not in the asked workspace `{asked}`; it was closed",
            created.surface
        )))
    }

    fn split_at(
        &self,
        workspace: &WorkspaceId,
        surface: &str,
        down: bool,
    ) -> Result<(PaneId, SurfaceId), HostError> {
        let answer: Created = self.rpc(
            "surface.split",
            &json!({
                "surface_id": surface,
                "direction": if down { "down" } else { "right" },
                "focus": false,
            }),
        )?;
        let pane = answer
            .pane
            .clone()
            .ok_or_else(|| HostError::Missing(format!("splitting `{surface}` opened no pane")))?;
        self.landed_in(&answer, workspace)?;
        Ok((PaneId(pane), SurfaceId(answer.surface)))
    }

    fn initial_command(&self, label: &str, argv: &[String]) -> String {
        let mut words = vec![
            "/usr/bin/env".to_owned(),
            format!("{LABEL_VAR}={}", quote(label)),
        ];
        if let Some(path) = &self.search_path {
            words.push(format!("\"PATH=${{PATH%%:*}}:\"{}", quote(path)));
        }
        words.extend(argv.iter().map(|arg| quote(arg)));
        words.join(" ")
    }
}

/// Drops cmux's per-surface shim folders from a `PATH`: a program cmux starts gets its own surface's shims in front.
fn without_shims(path: &str) -> String {
    path.split(':')
        .filter(|dir| !dir.contains("/.cmuxterm/cmux-cli-shims/"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Quotes a word for a POSIX shell: as it is when it holds only safe characters, else in single quotes.
#[must_use]
pub fn quote(word: &str) -> String {
    let safe = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c));
    if safe {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// Joins an argv into one POSIX shell command line.
#[must_use]
pub fn shell_line(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Splits a POSIX shell command line into its words: single and double quotes and backslashes are honoured, nothing
/// is expanded.
#[must_use]
pub fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                started = true;
                word.extend(chars.by_ref().take_while(|&c| c != '\''));
            }
            '"' => {
                started = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some(next @ ('"' | '\\' | '$' | '`')) => word.push(next),
                            Some(next) => {
                                word.push('\\');
                                word.push(next);
                            }
                            None => word.push('\\'),
                        },
                        other => word.push(other),
                    }
                }
            }
            '\\' => {
                started = true;
                word.extend(chars.next());
            }
            c if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            other => {
                started = true;
                word.push(other);
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// The label and argv of a program [`Cmux::spawn`] started, read back from the surface's initial command.
#[must_use]
pub fn parse_initial_command(line: &str) -> Option<(String, Vec<String>)> {
    let mut words = shell_words(line).into_iter();
    if words.next()? != "/usr/bin/env" {
        return None;
    }
    let label = words
        .next()?
        .strip_prefix(LABEL_VAR)?
        .strip_prefix('=')?
        .to_owned();
    let argv: Vec<String> = words.skip_while(|word| word.starts_with("PATH=")).collect();
    Some((label, argv))
}

#[derive(Deserialize)]
struct TreeAnswer {
    #[serde(default)]
    windows: Vec<RawWindow>,
}

#[derive(Deserialize)]
struct RawWindow {
    #[serde(default)]
    workspaces: Vec<RawWorkspace>,
}

#[derive(Deserialize)]
struct RawWorkspace {
    id: String,
    #[serde(default)]
    title: String,
    layout: Option<RawNode>,
    #[serde(default)]
    panes: Vec<RawPane>,
}

impl RawWorkspace {
    fn pane(&self, pane: &str) -> Option<&RawPane> {
        self.panes.iter().find(|raw| raw.id == pane)
    }

    fn first_pane(&self) -> Option<&RawPane> {
        self.layout
            .as_ref()
            .and_then(RawNode::first)
            .and_then(|id| self.pane(id))
            .or_else(|| self.panes.first())
    }

    fn surfaces(&self) -> Vec<SurfaceId> {
        self.panes
            .iter()
            .flat_map(|pane| pane.surface_ids.iter().cloned().map(SurfaceId))
            .collect()
    }
}

#[derive(Deserialize)]
struct RawPane {
    id: String,
    #[serde(default)]
    selected_surface_id: Option<String>,
    #[serde(default)]
    surface_ids: Vec<String>,
}

impl RawPane {
    fn holds(&self, surface: &SurfaceId) -> bool {
        self.surface_ids.iter().any(|id| id == surface.as_str())
    }

    fn active(&self) -> Option<&str> {
        self.selected_surface_id
            .as_deref()
            .or_else(|| self.surface_ids.first().map(String::as_str))
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawNode {
    Leaf {
        pane: RawPaneRef,
    },
    Split {
        direction: String,
        #[serde(default = "half")]
        split: f64,
        children: Vec<Self>,
    },
}

#[derive(Deserialize)]
struct RawPaneRef {
    id: String,
}

const fn half() -> f64 {
    0.5
}

impl RawNode {
    fn first(&self) -> Option<&str> {
        match self {
            Self::Leaf { pane } => Some(pane.id.as_str()),
            Self::Split { children, .. } => children.first().and_then(Self::first),
        }
    }

    fn into_tree(self, workspace: &RawWorkspace) -> PaneTree {
        match self {
            Self::Leaf { pane } => {
                let surfaces = workspace
                    .pane(&pane.id)
                    .map(|raw| raw.surface_ids.iter().cloned().map(SurfaceId).collect())
                    .unwrap_or_default();
                PaneTree::Leaf {
                    pane: PaneId(pane.id),
                    surfaces,
                }
            }
            Self::Split {
                direction,
                split,
                children,
            } => PaneTree::Split {
                down: direction == "vertical",
                ratio: split,
                parts: children
                    .into_iter()
                    .map(|child| child.into_tree(workspace))
                    .collect(),
            },
        }
    }
}

#[derive(Deserialize)]
struct Created {
    #[serde(rename = "surface_id")]
    surface: String,
    #[serde(default, rename = "pane_id")]
    pane: Option<String>,
    #[serde(default, rename = "workspace_id")]
    workspace: Option<String>,
}

#[derive(Deserialize)]
struct SurfaceList {
    #[serde(default)]
    surfaces: Vec<RawSurface>,
}

#[derive(Deserialize)]
struct RawSurface {
    id: String,
    #[serde(default)]
    initial_command: Option<String>,
}

#[derive(Deserialize)]
struct SessionList {
    #[serde(default)]
    sessions: Vec<RawSession>,
}

#[derive(Deserialize)]
struct RawSession {
    #[serde(default)]
    surface_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    agent_lifecycle: Option<String>,
    #[serde(default)]
    active_for_surface: bool,
}

#[derive(Deserialize)]
struct ScreenText {
    #[serde(default)]
    text: String,
}

/// Parses `cmux rpc system.tree` into its workspaces.
///
/// # Errors
///
/// Returns the parser's error.
pub fn parse_workspaces(text: &str) -> Result<Vec<Workspace>, serde_json::Error> {
    let tree: TreeAnswer = serde_json::from_str(text)?;
    Ok(tree
        .windows
        .into_iter()
        .flat_map(|window| window.workspaces)
        .map(|raw| Workspace {
            id: WorkspaceId(raw.id),
            title: raw.title,
        })
        .collect())
}

/// Parses the layout of `workspace` out of `cmux rpc system.tree`; `None` when the tree does not hold it. cmux calls a
/// split whose parts sit side by side `horizontal` and one whose parts are stacked `vertical`.
///
/// # Errors
///
/// Returns the parser's error.
pub fn parse_tree(
    text: &str,
    workspace: &WorkspaceId,
) -> Result<Option<PaneTree>, serde_json::Error> {
    let tree: TreeAnswer = serde_json::from_str(text)?;
    Ok(tree
        .windows
        .into_iter()
        .flat_map(|window| window.workspaces)
        .find(|raw| raw.id == workspace.as_str())
        .and_then(|mut raw| {
            let layout = raw.layout.take()?;
            Some(layout.into_tree(&raw))
        }))
}

/// Parses `cmux sessions list --json` into the state of every surface with an active Claude session.
///
/// cmux's own Claude hooks record the sessions. `running` becomes `working`, `needsInput` becomes `blocked`, and other
/// words stay as cmux spells them.
///
/// # Errors
///
/// Returns the parser's error.
pub fn parse_states(text: &str) -> Result<Vec<SurfaceState>, serde_json::Error> {
    let list: SessionList = serde_json::from_str(text)?;
    Ok(list
        .sessions
        .into_iter()
        .filter(|raw| raw.active_for_surface)
        .filter_map(|raw| {
            let state = match raw.agent_lifecycle.as_deref() {
                Some("running") => "working".to_owned(),
                Some("needsInput") => "blocked".to_owned(),
                Some(other) => other.to_owned(),
                None => "unknown".to_owned(),
            };
            Some(SurfaceState {
                surface: SurfaceId(raw.surface_id?),
                state,
                session_id: raw.session_id,
                blocked_reason: None,
            })
        })
        .collect())
}

/// Parses `cmux rpc surface.list` into the programs [`Cmux::spawn`] started in a workspace.
///
/// # Errors
///
/// Returns the parser's error.
pub fn parse_programs(text: &str, workspace: &WorkspaceId) -> Result<Vec<Pane>, serde_json::Error> {
    let list: SurfaceList = serde_json::from_str(text)?;
    Ok(list
        .surfaces
        .into_iter()
        .filter_map(|raw| {
            let (label, argv) = parse_initial_command(raw.initial_command.as_deref()?)?;
            Some(Pane {
                agent: AgentId(raw.id.clone()),
                surface: SurfaceId(raw.id),
                workspace: Some(workspace.clone()),
                label,
                command: shell_line(&argv),
                running: true,
                pid: None,
                started_ms: None,
            })
        })
        .collect())
}

fn now_ms() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|since| u64::try_from(since.as_millis()).ok())
}

fn tolerate_missing(result: Result<(), HostError>) -> Result<(), HostError> {
    match result {
        Err(HostError::Failed { message, .. }) if message.contains(NOT_FOUND) => Ok(()),
        other => other,
    }
}

/// The byte a terminal receives for `c` with Ctrl held, such as `\u{1}` for `a`.
fn control(c: char) -> Option<char> {
    let code = u32::from(c.to_ascii_lowercase());
    matches!(code, 0x40..=0x7f)
        .then(|| char::from_u32(code & 0x1f))
        .flatten()
}

impl<R: Runner> TerminalHost for Cmux<R> {
    fn ping(&self) -> Result<(), HostError> {
        match self.runner.run(&["rpc", "system.ping", "{}"]) {
            Ok(text) if text.contains("\"pong\"") => Ok(()),
            Ok(_) | Err(HostError::Failed { .. }) => Err(HostError::NotRunning),
            Err(error) => Err(error),
        }
    }

    fn workspaces(&self) -> Result<Vec<Workspace>, HostError> {
        Ok(self
            .tree(None)?
            .into_iter()
            .map(|raw| Workspace {
                id: WorkspaceId(raw.id),
                title: raw.title,
            })
            .collect())
    }

    fn ensure_workspace(&self, title: &str, cwd: &Path) -> Result<(WorkspaceId, bool), HostError> {
        if let Some(found) = self.workspaces()?.into_iter().find(|w| w.title == title) {
            return Ok((found.id, false));
        }
        let created: Created = self.rpc(
            "workspace.create",
            &json!({ "title": title, "cwd": cwd.display().to_string(), "focus": false }),
        )?;
        created
            .workspace
            .map(|id| (WorkspaceId(id), true))
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
        let mut params = json!({
            "workspace_id": workspace.as_str(),
            "working_directory": cwd.display().to_string(),
            "initial_command": self.initial_command(label, argv),
            "focus": false,
        });
        let mut replaced = None;
        if let Target::Pane { pane, replace_tab } = target {
            params["pane_id"] = json!(pane.as_str());
            if *replace_tab {
                replaced = self
                    .workspace(workspace)?
                    .pane(pane.as_str())
                    .and_then(RawPane::active)
                    .map(|surface| SurfaceId(surface.to_owned()));
            }
        }
        let created: Created = self.rpc("surface.create", &params)?;
        let landed = self.landed_in(&created, workspace)?;
        if let Some(old) = replaced {
            tolerate_missing(self.close_surface(&old))?;
        }
        Ok(Pane {
            agent: AgentId(created.surface.clone()),
            surface: SurfaceId(created.surface),
            workspace: Some(landed),
            label: label.to_owned(),
            command: shell_line(argv),
            running: true,
            pid: None,
            started_ms: now_ms(),
        })
    }

    fn layout(&self, workspace: &WorkspaceId) -> Result<PaneTree, HostError> {
        let mut raw = self.workspace(workspace)?;
        let layout = raw
            .layout
            .take()
            .ok_or_else(|| HostError::Missing(format!("workspace `{workspace}` has no layout")))?;
        Ok(layout.into_tree(&raw))
    }

    fn split(&self, beside: &SurfaceId, down: bool) -> Result<(PaneId, SurfaceId), HostError> {
        let workspace = self.holding(beside)?;
        let first = workspace
            .first_pane()
            .and_then(RawPane::active)
            .ok_or_else(|| {
                HostError::Missing(format!("workspace `{}` has no pane", workspace.id))
            })?;
        self.split_at(&WorkspaceId(workspace.id.clone()), first, down)
    }

    fn split_pane(
        &self,
        workspace: &WorkspaceId,
        pane: &PaneId,
    ) -> Result<(PaneId, SurfaceId), HostError> {
        let raw = self.workspace(workspace)?;
        let surface = raw
            .pane(pane.as_str())
            .and_then(RawPane::active)
            .ok_or_else(|| HostError::Missing(format!("no pane `{pane}` in `{workspace}`")))?;
        self.split_at(workspace, surface, false)
    }

    fn close_pane(&self, pane: &PaneId) -> Result<(), HostError> {
        let surfaces: Vec<SurfaceId> = self
            .tree(None)?
            .iter()
            .flat_map(|raw| raw.panes.iter())
            .find(|raw| raw.id == pane.as_str())
            .map(|raw| raw.surface_ids.iter().cloned().map(SurfaceId).collect())
            .ok_or_else(|| HostError::Missing(format!("no pane `{pane}`")))?;
        for surface in &surfaces {
            tolerate_missing(self.close_surface(surface))?;
        }
        Ok(())
    }

    fn panes(&self) -> Result<Vec<Pane>, HostError> {
        let mut programs = Vec::new();
        for raw in self.tree(None)? {
            let workspace = WorkspaceId(raw.id);
            let params = json!({ "workspace_id": workspace.as_str() }).to_string();
            let text = self.runner.run(&["rpc", "surface.list", &params])?;
            programs.extend(parse_programs(&text, &workspace).map_err(|source| {
                HostError::Parse {
                    command: "rpc surface.list".to_owned(),
                    source,
                }
            })?);
        }
        Ok(programs)
    }

    fn surfaces(&self, workspace: &WorkspaceId) -> Result<Vec<SurfaceId>, HostError> {
        Ok(self.workspace(workspace)?.surfaces())
    }

    fn surface_states(&self) -> Result<Vec<SurfaceState>, HostError> {
        let args = ["sessions", "list", "--json", "--agent", "claude"];
        let text = self.runner.run(&args)?;
        let states = parse_states(&text).map_err(|source| HostError::Parse {
            command: args.join(" "),
            source,
        })?;
        let open: Vec<SurfaceId> = self
            .tree(None)?
            .iter()
            .flat_map(RawWorkspace::surfaces)
            .collect();
        Ok(states
            .into_iter()
            .filter(|state| open.contains(&state.surface))
            .collect())
    }

    fn read_screen(&self, surface: &SurfaceId, lines: u32) -> Result<String, HostError> {
        let screen: ScreenText = self.rpc(
            "surface.read_text",
            &json!({ "surface_id": surface.as_str(), "lines": lines, "scrollback": true }),
        )?;
        Ok(screen.text)
    }

    fn send_text(&self, surface: &SurfaceId, text: &str) -> Result<(), HostError> {
        self.rpc_quiet(
            "surface.send_text",
            &json!({ "surface_id": surface.as_str(), "text": text }),
        )
    }

    fn send_key(&self, surface: &SurfaceId, key: Key) -> Result<(), HostError> {
        let name = match key {
            Key::Enter => "enter",
            Key::Escape => "escape",
            Key::Char(c) => return self.send_text(surface, &c.to_string()),
            Key::Ctrl(c) => {
                let byte = control(c)
                    .ok_or_else(|| HostError::Missing(format!("no Ctrl key for `{c}`")))?;
                return self.send_text(surface, &byte.to_string());
            }
        };
        self.rpc_quiet(
            "surface.send_key",
            &json!({ "surface_id": surface.as_str(), "key": name }),
        )
    }

    fn rename_surface(&self, surface: &SurfaceId, title: &str) -> Result<(), HostError> {
        self.rpc_quiet(
            "tab.action",
            &json!({ "action": "rename", "surface_id": surface.as_str(), "title": title }),
        )
    }

    fn close_surface(&self, surface: &SurfaceId) -> Result<(), HostError> {
        self.rpc_quiet(
            "surface.close",
            &json!({ "surface_id": surface.as_str(), "force": true }),
        )
    }

    fn close_workspace(&self, workspace: &WorkspaceId) -> Result<(), HostError> {
        self.rpc_quiet(
            "workspace.close",
            &json!({ "workspace_id": workspace.as_str(), "force": true }),
        )
    }

    fn kill_agent(&self, agent: &AgentId) -> Result<(), HostError> {
        let surface = SurfaceId(agent.as_str().to_owned());
        match self.close_surface(&surface) {
            Err(HostError::Failed { message, .. }) if message.contains(LAST_SURFACE) => {
                let workspace = WorkspaceId(self.holding(&surface)?.id);
                tolerate_missing(self.close_workspace(&workspace))
            }
            other => tolerate_missing(other),
        }
    }

    fn show_blocked(
        &self,
        surface: &SurfaceId,
        question: &str,
        choices: &[Choice],
    ) -> Result<(), HostError> {
        let answers = choices
            .iter()
            .map(|choice| {
                if choice.label.starts_with(&choice.key) {
                    choice.label.clone()
                } else {
                    format!("{} {}", choice.key, choice.label)
                }
            })
            .collect::<Vec<_>>()
            .join(" · ");
        let shown = if answers.is_empty() {
            question.to_owned()
        } else {
            format!("{question} ({answers})")
        };
        let workspace = self.holding(surface)?;
        self.runner.run(&[
            "set-status",
            &status_key(surface),
            &shown,
            "--workspace",
            &workspace.id,
            "--icon",
            "bell.fill",
            "--color",
            "#FF9500",
            "--priority",
            "100",
        ])?;
        self.rpc_quiet(
            "notification.create_for_surface",
            &json!({
                "surface_id": surface.as_str(),
                "title": "headwork",
                "subtitle": "Needs you",
                "body": shown,
            }),
        )
    }

    fn clear_blocked(&self, surface: &SurfaceId) -> Result<(), HostError> {
        let workspace = match self.holding(surface) {
            Ok(workspace) => workspace,
            Err(HostError::Missing(_)) => return Ok(()),
            Err(error) => return Err(error),
        };
        self.runner
            .run(&[
                "clear-status",
                &status_key(surface),
                "--workspace",
                &workspace.id,
            ])
            .map(drop)
    }

    fn notify(&self, text: &str) -> Result<(), HostError> {
        self.rpc_quiet(
            "notification.create",
            &json!({ "title": "headwork", "body": text }),
        )
    }
}
