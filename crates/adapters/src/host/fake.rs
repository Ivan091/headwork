//! A terminal host in memory that records every call, for tests of spawn, close and import.

use std::cell::RefCell;
use std::path::Path;

use super::{
    AgentId, Choice, HostError, Key, Pane, SurfaceId, SurfaceState, TerminalHost, Workspace,
    WorkspaceId, command_line,
};

#[derive(Debug, Default)]
struct State {
    next: u32,
    workspaces: Vec<Workspace>,
    surfaces: Vec<(WorkspaceId, SurfaceId)>,
    panes: Vec<Pane>,
    states: Vec<SurfaceState>,
    calls: Vec<String>,
}

impl State {
    fn id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}-{}", self.next)
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

    /// Every call so far, one line each, such as `spawn ws-1 me-sim/sand`.
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
        state.surfaces.push((ws, surface));
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
        state.surfaces.push((ws, surface.clone()));
        surface
    }

    /// Adds a surface state, as `agent-state` reports it.
    pub fn add_state(&self, state: SurfaceState) {
        self.state.borrow_mut().states.push(state);
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
        state.workspaces.push(Workspace {
            id: id.clone(),
            title: title.to_owned(),
        });
        state.calls.push(format!("new-workspace {title}"));
        Ok((id, true))
    }

    fn spawn(
        &self,
        workspace: &WorkspaceId,
        cwd: &Path,
        label: &str,
        argv: &[String],
        fresh_workspace: bool,
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
        state.surfaces.push((workspace.clone(), surface));
        state.panes.push(pane.clone());
        state.calls.push(format!(
            "spawn {workspace} {label} in {} fresh={fresh_workspace}",
            cwd.display()
        ));
        Ok(pane)
    }

    fn panes(&self) -> Result<Vec<Pane>, HostError> {
        Ok(self.state.borrow().panes.clone())
    }

    fn surfaces(&self, workspace: &WorkspaceId) -> Result<Vec<SurfaceId>, HostError> {
        Ok(self
            .state
            .borrow()
            .surfaces
            .iter()
            .filter(|(ws, _)| ws == workspace)
            .map(|(_, surface)| surface.clone())
            .collect())
    }

    fn surface_states(&self) -> Result<Vec<SurfaceState>, HostError> {
        Ok(self.state.borrow().states.clone())
    }

    fn read_screen(&self, surface: &SurfaceId, lines: u32) -> Result<String, HostError> {
        self.record(format!("read-screen {surface} {lines}"));
        Ok(String::new())
    }

    fn send_text(&self, surface: &SurfaceId, text: &str) -> Result<(), HostError> {
        self.record(format!("send {surface} {text}"));
        Ok(())
    }

    fn send_key(&self, surface: &SurfaceId, key: Key) -> Result<(), HostError> {
        self.record(format!("send-key {surface} {key:?}"));
        Ok(())
    }

    fn close_surface(&self, surface: &SurfaceId) -> Result<(), HostError> {
        let mut state = self.state.borrow_mut();
        state.surfaces.retain(|(_, s)| s != surface);
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
        state.surfaces.retain(|(ws, _)| ws != workspace);
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
