//! A terminal host in memory that records every call, for tests of spawn, close and import.
//!
//! Each workspace holds a [`PaneTree`]: a new workspace is one empty pane, a split halves a pane and opens a shell in
//! the new half, and a pane whose last surface closes leaves the tree, its sibling taking its place, as in wmux.

use std::cell::RefCell;
use std::path::Path;

use super::{
    AgentId, Choice, HostError, Key, Pane, PaneId, PaneTree, SurfaceId, SurfaceState, Target,
    TerminalHost, Workspace, WorkspaceId, command_line,
};

#[derive(Debug, Default)]
struct State {
    next: u32,
    workspaces: Vec<Workspace>,
    trees: Vec<(WorkspaceId, PaneTree)>,
    panes: Vec<Pane>,
    states: Vec<SurfaceState>,
    calls: Vec<String>,
    screens: Vec<(SurfaceId, String)>,
    stray_splits: bool,
}

impl State {
    fn id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}-{}", self.next)
    }

    fn tree(&mut self, workspace: &WorkspaceId) -> Option<&mut PaneTree> {
        self.trees
            .iter_mut()
            .find(|(ws, _)| ws == workspace)
            .map(|(_, tree)| tree)
    }

    fn add_to_first_pane(&mut self, workspace: &WorkspaceId, surface: SurfaceId) {
        if let Some(surfaces) = self.tree(workspace).and_then(first_leaf) {
            surfaces.push(surface);
        }
    }

    fn remove_surface(&mut self, surface: &SurfaceId) {
        for (_, tree) in &mut self.trees {
            remove(tree, surface);
        }
    }
}

fn surfaces_of(tree: &PaneTree) -> Vec<SurfaceId> {
    tree.cells()
        .into_iter()
        .flat_map(|cell| cell.surfaces)
        .collect()
}

fn first_leaf(tree: &mut PaneTree) -> Option<&mut Vec<SurfaceId>> {
    match tree {
        PaneTree::Leaf { surfaces, .. } => Some(surfaces),
        PaneTree::Split { parts, .. } => parts.first_mut().and_then(first_leaf),
    }
}

fn leaf_mut<'a>(
    tree: &'a mut PaneTree,
    wanted: &dyn Fn(&PaneId, &[SurfaceId]) -> bool,
) -> Option<&'a mut PaneTree> {
    let hit = match &*tree {
        PaneTree::Leaf { pane, surfaces } => Some(wanted(pane, surfaces)),
        PaneTree::Split { .. } => None,
    };
    match hit {
        Some(true) => Some(tree),
        Some(false) => None,
        None => match tree {
            PaneTree::Split { parts, .. } => {
                parts.iter_mut().find_map(|part| leaf_mut(part, wanted))
            }
            PaneTree::Leaf { .. } => None,
        },
    }
}

fn remove(tree: &mut PaneTree, surface: &SurfaceId) {
    match tree {
        PaneTree::Leaf { surfaces, .. } => surfaces.retain(|s| s != surface),
        PaneTree::Split { parts, .. } => {
            for part in parts.iter_mut() {
                remove(part, surface);
            }
            parts.retain(
                |part| !matches!(part, PaneTree::Leaf { surfaces, .. } if surfaces.is_empty()),
            );
            if parts.len() == 1
                && let Some(only) = parts.pop()
            {
                *tree = only;
            } else if parts.is_empty() {
                *tree = PaneTree::Leaf {
                    pane: PaneId(String::new()),
                    surfaces: Vec::new(),
                };
            }
        }
    }
}

/// The base of the pids the fake host makes up: odd and above any real Windows pid, which are multiples of four, so a
/// test that ends a made-up pid can never hit a real process.
pub const FAKE_PID_BASE: u32 = 4_000_000_001;

/// A host in memory. Spawned panes run until they are killed or their surface is closed.
#[derive(Debug, Default)]
pub struct FakeHost {
    state: RefCell<State>,
}

impl FakeHost {
    /// An empty host.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every call so far, one line each, such as `spawn ws-1 me-sim/sand in S:/w tab`.
    #[must_use]
    pub fn calls(&self) -> Vec<String> {
        self.state.borrow().calls.clone()
    }

    /// Adds a running pane that some other tool started, in a workspace with this title.
    pub fn add_foreign_pane(&self, workspace: &str, label: &str, pid: u32) -> Pane {
        let (ws, _) = self
            .ensure_workspace(workspace, Path::new("."))
            .unwrap_or_else(|_| (WorkspaceId(String::new()), false));
        let mut state = self.state.borrow_mut();
        let surface = SurfaceId(state.id("surf"));
        let pane = Pane {
            agent: AgentId(state.id("agent")),
            surface: surface.clone(),
            workspace: Some(ws.clone()),
            label: label.to_owned(),
            command: "claude".to_owned(),
            running: true,
            pid: Some(pid),
            started_ms: Some(u64::from(state.next)),
        };
        state.add_to_first_pane(&ws, surface);
        state.panes.push(pane.clone());
        pane
    }

    /// Adds a surface that holds no host agent, such as a Claude pane the host restored after a restart.
    pub fn add_restored_surface(&self, workspace: &str) -> SurfaceId {
        let (ws, _) = self
            .ensure_workspace(workspace, Path::new("."))
            .unwrap_or_else(|_| (WorkspaceId(String::new()), false));
        let mut state = self.state.borrow_mut();
        let surface = SurfaceId(state.id("surf"));
        state.add_to_first_pane(&ws, surface.clone());
        surface
    }

    /// Adds a surface state, as `agent-state` reports it.
    pub fn add_state(&self, state: SurfaceState) {
        self.state.borrow_mut().states.push(state);
    }

    /// Sets what a surface's screen shows from now on; a surface never set shows nothing.
    pub fn set_screen(&self, surface: &SurfaceId, text: &str) {
        let mut state = self.state.borrow_mut();
        state.screens.retain(|(s, _)| s != surface);
        state.screens.push((surface.clone(), text.to_owned()));
    }

    /// Marks a pane's program as exited, as if it ended by itself.
    pub fn exit(&self, agent: &AgentId) {
        let mut state = self.state.borrow_mut();
        state
            .panes
            .iter_mut()
            .filter(|pane| &pane.agent == agent)
            .for_each(|pane| pane.running = false);
    }

    /// Makes every later split land outside the workspace it was meant for, as wmux does when it does not know the
    /// surface it is run from: the new pane then belongs to a workspace no one listed.
    pub fn stray_splits(&self) {
        self.state.borrow_mut().stray_splits = true;
    }

    fn record(&self, call: String) {
        self.state.borrow_mut().calls.push(call);
    }
}

impl TerminalHost for FakeHost {
    fn ping(&self) -> Result<(), HostError> {
        self.record("ping".to_owned());
        Ok(())
    }

    fn workspaces(&self) -> Result<Vec<Workspace>, HostError> {
        Ok(self.state.borrow().workspaces.clone())
    }

    fn ensure_workspace(&self, title: &str, _cwd: &Path) -> Result<(WorkspaceId, bool), HostError> {
        let mut state = self.state.borrow_mut();
        if let Some(found) = state.workspaces.iter().find(|w| w.title == title) {
            return Ok((found.id.clone(), false));
        }
        let id = WorkspaceId(state.id("ws"));
        let pane = PaneId(state.id("pane"));
        state.workspaces.push(Workspace {
            id: id.clone(),
            title: title.to_owned(),
        });
        state.trees.push((
            id.clone(),
            PaneTree::Leaf {
                pane,
                surfaces: Vec::new(),
            },
        ));
        state.calls.push(format!("new-workspace {title}"));
        Ok((id, true))
    }

    fn spawn(
        &self,
        workspace: &WorkspaceId,
        cwd: &Path,
        label: &str,
        argv: &[String],
        target: &Target,
    ) -> Result<Pane, HostError> {
        let mut state = self.state.borrow_mut();
        let surface = SurfaceId(state.id("surf"));
        let pane = Pane {
            agent: AgentId(state.id("agent")),
            surface: surface.clone(),
            workspace: Some(workspace.clone()),
            label: label.to_owned(),
            command: command_line(argv),
            running: true,
            pid: Some(FAKE_PID_BASE + state.next * 2),
            started_ms: Some(u64::from(state.next)),
        };
        let place = match target {
            Target::Tab => {
                state.add_to_first_pane(workspace, surface);
                "tab".to_owned()
            }
            Target::Pane {
                pane: id,
                replace_tab,
            } => {
                let tree = state
                    .tree(workspace)
                    .ok_or_else(|| HostError::Missing(format!("no workspace {workspace}")))?;
                let leaf = leaf_mut(tree, &|p, _| p == id)
                    .ok_or_else(|| HostError::Missing(format!("no pane {id} in {workspace}")))?;
                if let PaneTree::Leaf { surfaces, .. } = leaf {
                    if *replace_tab {
                        surfaces.pop();
                    }
                    surfaces.push(surface);
                }
                if *replace_tab {
                    format!("pane {id} replace")
                } else {
                    format!("pane {id}")
                }
            }
        };
        state.panes.push(pane.clone());
        state.calls.push(format!(
            "spawn {workspace} {label} in {} {place}",
            cwd.display()
        ));
        Ok(pane)
    }

    fn layout(&self, workspace: &WorkspaceId) -> Result<PaneTree, HostError> {
        self.state
            .borrow_mut()
            .tree(workspace)
            .cloned()
            .ok_or_else(|| HostError::Missing(format!("no workspace {workspace}")))
    }

    fn split(&self, beside: &SurfaceId, down: bool) -> Result<(PaneId, SurfaceId), HostError> {
        let mut state = self.state.borrow_mut();
        let pane = PaneId(state.id("pane"));
        let shell = SurfaceId(state.id("surf"));
        if state.stray_splits {
            state.trees.push((
                WorkspaceId("ws-elsewhere".to_owned()),
                PaneTree::Leaf {
                    pane: pane.clone(),
                    surfaces: vec![shell.clone()],
                },
            ));
            state.calls.push(format!("split {beside} elsewhere"));
            return Ok((pane, shell));
        }
        let tree = state
            .trees
            .iter_mut()
            .map(|(_, tree)| tree)
            .find(|tree| surfaces_of(tree).contains(beside))
            .ok_or_else(|| HostError::Missing(format!("no surface {beside}")))?;
        let found = leaf_mut(tree, &|_, _| true)
            .ok_or_else(|| HostError::Missing(format!("no pane beside {beside}")))?;
        let old = std::mem::replace(
            found,
            PaneTree::Leaf {
                pane: PaneId(String::new()),
                surfaces: Vec::new(),
            },
        );
        *found = PaneTree::Split {
            down,
            ratio: 0.5,
            parts: vec![
                old,
                PaneTree::Leaf {
                    pane: pane.clone(),
                    surfaces: vec![shell.clone()],
                },
            ],
        };
        let direction = if down { "down" } else { "right" };
        state.calls.push(format!("split {beside} {direction}"));
        Ok((pane, shell))
    }

    fn close_pane(&self, pane: &PaneId) -> Result<(), HostError> {
        let mut state = self.state.borrow_mut();
        let doomed: Vec<SurfaceId> = state
            .trees
            .iter()
            .flat_map(|(_, tree)| tree.cells())
            .filter(|cell| &cell.pane == pane)
            .flat_map(|cell| cell.surfaces)
            .collect();
        for surface in &doomed {
            state.remove_surface(surface);
        }
        state.calls.push(format!("close-pane {pane}"));
        Ok(())
    }

    fn panes(&self) -> Result<Vec<Pane>, HostError> {
        Ok(self.state.borrow().panes.clone())
    }

    fn surfaces(&self, workspace: &WorkspaceId) -> Result<Vec<SurfaceId>, HostError> {
        Ok(self
            .state
            .borrow()
            .trees
            .iter()
            .filter(|(ws, _)| ws == workspace)
            .flat_map(|(_, tree)| surfaces_of(tree))
            .collect())
    }

    fn surface_states(&self) -> Result<Vec<SurfaceState>, HostError> {
        Ok(self.state.borrow().states.clone())
    }

    fn read_screen(&self, surface: &SurfaceId, lines: u32) -> Result<String, HostError> {
        self.record(format!("read-screen {surface} {lines}"));
        Ok(self
            .state
            .borrow()
            .screens
            .iter()
            .find(|(s, _)| s == surface)
            .map(|(_, text)| text.clone())
            .unwrap_or_default())
    }

    fn send_text(&self, surface: &SurfaceId, text: &str) -> Result<(), HostError> {
        self.record(format!("send {surface} {text}"));
        Ok(())
    }

    fn send_key(&self, surface: &SurfaceId, key: Key) -> Result<(), HostError> {
        self.record(format!("send-key {surface} {key:?}"));
        Ok(())
    }

    fn rename_surface(&self, surface: &SurfaceId, title: &str) -> Result<(), HostError> {
        let mut state = self.state.borrow_mut();
        state
            .calls
            .push(format!("rename-surface {surface} {title}"));
        Ok(())
    }

    fn close_surface(&self, surface: &SurfaceId) -> Result<(), HostError> {
        let mut state = self.state.borrow_mut();
        state.remove_surface(surface);
        state
            .panes
            .iter_mut()
            .filter(|pane| &pane.surface == surface)
            .for_each(|pane| pane.running = false);
        state.calls.push(format!("close-surface {surface}"));
        Ok(())
    }

    fn close_workspace(&self, workspace: &WorkspaceId) -> Result<(), HostError> {
        let mut state = self.state.borrow_mut();
        state.workspaces.retain(|w| &w.id != workspace);
        state.trees.retain(|(ws, _)| ws != workspace);
        state
            .panes
            .iter_mut()
            .filter(|pane| pane.workspace.as_ref() == Some(workspace))
            .for_each(|pane| pane.running = false);
        state.calls.push(format!("close-workspace {workspace}"));
        Ok(())
    }

    fn kill_agent(&self, agent: &AgentId) -> Result<(), HostError> {
        let mut state = self.state.borrow_mut();
        state
            .panes
            .iter_mut()
            .filter(|pane| &pane.agent == agent)
            .for_each(|pane| pane.running = false);
        state.calls.push(format!("kill {agent}"));
        Ok(())
    }

    fn show_blocked(
        &self,
        surface: &SurfaceId,
        question: &str,
        choices: &[Choice],
    ) -> Result<(), HostError> {
        self.record(format!(
            "blocked {surface} {question} ({} choices)",
            choices.len()
        ));
        Ok(())
    }

    fn clear_blocked(&self, surface: &SurfaceId) -> Result<(), HostError> {
        self.record(format!("unblocked {surface}"));
        Ok(())
    }

    fn notify(&self, text: &str) -> Result<(), HostError> {
        self.record(format!("notify {text}"));
        Ok(())
    }
}
