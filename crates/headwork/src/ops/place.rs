//! Where a worker's pane opens in its project's workspace, and how it goes when the worker closes (`[layout]`).
//!
//! In `panes` mode each worker gets a pane of its own, up to `layout.panes` and never smaller than a quarter of the
//! workspace, so at most a grid of two rows of two; later workers stack as tabs. A worker takes a free pane when there
//! is one (a fresh workspace's shell, a pane the user left), else splits one in half: the first pane top and bottom or
//! side by side (`split`), any other side by side (`split_pane`), as wmux allows; no empty pane is cut ahead of time.
//! A closing worker's surface closes, and when it was its pane's last the pane leaves the layout and its neighbours
//! take the space; the workspace closes once
//! nothing in it holds a session. The choice is [`headwork_core::layout::place`]; this module reads the workspace from
//! the host as it is now, so panes the user closed or rearranged by hand count as they stand. Workers already open
//! are never moved, and a pane that still holds a session (another worker's tab, the orchestrator) never closes.
//! `tabs` mode adds tabs. `workspaces` mode gives each worker a workspace of its own, titled with the task's name and
//! holding one full-screen pane: the fresh workspace's shell is replaced, nothing splits or stacks, and the workspace
//! closes with its worker.
//!
//! A lead task (`docs/leads.md`, section 2) opens like `workspaces` mode whatever `[layout] mode` says: a workspace of
//! its own titled with its name, the lead full-screen in it. Its parts open as new tabs of the lead's recorded
//! workspace (found by id, then by the lead's name) and never in a workspace of their own: a part whose lead's
//! workspace is gone is refused. A part's release closes only its tab; the lead's release closes the workspace with the
//! helpers still in it.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Result, bail};
use headwork_adapters::host::{PaneCell, PaneId, SurfaceId, Target, TerminalHost, WorkspaceId};
use headwork_core::layout::{self, Cell, Placement};
use headwork_core::{LayoutMode, PlaceKind, TaskShape};
use headwork_store::{Place, Store, Task, TaskId};

use super::{Env, label, title};

/// The host label of the placeholders older builds left in a free pane.
///
/// Builds before 2026-11-16 started `headwork idle` there; the label lets [`release`] and spawns find and close them.
/// It has no `/`, so the tick and import, which read `<project>/<slug>` labels, never take it for a worker.
pub const IDLE_LABEL: &str = "headwork:free";

/// The host label of a helper session a worker opens as a tab in its own workspace.
///
/// It has no `/`, so the tick and import never take it for a worker, and a workspace closes with its worker whatever
/// helpers still run in it.
pub const HELPER_LABEL: &str = "headwork:helper";

/// A worker's pane to open.
#[derive(Debug, Clone, Copy)]
pub(super) struct Opening<'a> {
    /// The task.
    pub task: &'a Task,
    /// Its session, already recorded.
    pub session: &'a str,
    /// The project's workspace.
    pub workspace: &'a WorkspaceId,
    /// The workspace was just created.
    pub fresh: bool,
    /// Where the program runs.
    pub dir: &'a Path,
    /// The program: `headwork launch …`.
    pub argv: &'a [String],
}

/// Opens the pane where the layout puts it, records the place on the session, and returns the surface.
///
/// # Errors
///
/// Fails when the host or the store fails.
pub(super) fn open(store: &mut Store, env: &Env<'_>, opening: &Opening<'_>) -> Result<String> {
    let (target, kind) = match (opening.task.shape, env.config.layout.mode) {
        (TaskShape::Part { .. }, _) => (Target::Tab, PlaceKind::Tab),
        (TaskShape::Lead, _) | (TaskShape::Plain, LayoutMode::Workspaces) => (
            tab(env.host, opening.workspace, opening.fresh)?,
            PlaceKind::Pane,
        ),
        (TaskShape::Plain, LayoutMode::Tabs) => (
            tab(env.host, opening.workspace, opening.fresh)?,
            PlaceKind::Tab,
        ),
        (TaskShape::Plain, LayoutMode::Panes) => pane(store, env, opening.workspace)?,
    };
    let started = env.host.spawn(
        opening.workspace,
        opening.dir,
        &label(opening.task),
        opening.argv,
        &target,
    )?;
    title::set(store, env, opening.task, &started.surface).ok();
    let pane = match &target {
        Target::Pane { pane, .. } => Some(pane.as_str()),
        Target::Tab => None,
    };
    store.set_session_place(
        opening.session,
        &Place {
            workspace: opening.workspace.as_str(),
            surface: started.surface.as_str(),
            pane,
            kind: Some(kind),
        },
        env.actor,
    )?;
    Ok(started.surface.to_string())
}

/// Closes a session's surface, or its workspace when nothing else there holds a session.
///
/// A lead's part closes only its tab: the lead's tab still holds a session. A lead's workspace, like any in
/// `workspaces` or `panes` mode, closes when no other surface holds a session (helpers do not count); in `tabs` mode a
/// plain task's workspace closes when it holds nothing else. Otherwise only the surface closes: a pane whose last
/// surface it was leaves the layout and the host gives its space to the panes beside it. Placeholders that builds
/// before this one left in the workspace close first.
///
/// # Errors
///
/// Returns the host's failure.
pub fn release(
    env: &Env<'_>,
    shape: TaskShape<TaskId>,
    workspace: Option<&WorkspaceId>,
    surface: &SurfaceId,
) -> Result<()> {
    let host = env.host;
    if let (Some(workspace), false) = (workspace, shape.is_part()) {
        sweep(host, workspace)?;
        let surfaces = host.surfaces(workspace)?;
        let tabs = shape == TaskShape::Plain && env.config.layout.mode == LayoutMode::Tabs;
        let last = if tabs {
            surfaces.iter().all(|s| s == surface)
        } else {
            let mut busy = busy_on_host(host)?;
            for program in host.panes()? {
                if program.label == HELPER_LABEL {
                    busy.remove(&program.surface);
                }
            }
            !surfaces.iter().any(|s| s != surface && busy.contains(s))
        };
        if last {
            host.close_workspace(workspace)?;
            return Ok(());
        }
    }
    host.close_surface(surface)?;
    Ok(())
}

/// The title of the workspace a task opens in: its own name for a lead and in `workspaces` mode, else its project's.
/// A part opens in its lead's workspace, found by [`workspace_for`].
#[must_use]
pub(super) fn workspace_title(env: &Env<'_>, task: &Task) -> String {
    match (task.shape, env.config.layout.mode) {
        (TaskShape::Lead, _) | (TaskShape::Plain, LayoutMode::Workspaces) => task.name.to_string(),
        (TaskShape::Part { .. } | TaskShape::Plain, LayoutMode::Panes | LayoutMode::Tabs)
        | (TaskShape::Part { .. }, LayoutMode::Workspaces) => task.project().as_str().to_owned(),
    }
}

/// The workspace a task's session opens in, and whether it was just created.
///
/// A lead or a plain task gets the workspace [`workspace_title`] names, created in `main` when missing. A part gets
/// its lead's recorded workspace, by id, else the one titled with the lead's name; it never creates one.
///
/// # Errors
///
/// Fails when the host or the store fails, or for a part whose lead's workspace is gone.
pub(super) fn workspace_for(
    store: &Store,
    env: &Env<'_>,
    task: &Task,
    main: &Path,
) -> Result<(WorkspaceId, bool)> {
    let TaskShape::Part { parent } = task.shape else {
        return Ok(env
            .host
            .ensure_workspace(&workspace_title(env, task), main)?);
    };
    Ok((lead_workspace(store, env, parent)?, false))
}

/// The open workspace of the lead `lead`: its newest session's recorded one by id, else the one titled with its name.
///
/// # Errors
///
/// Fails when the host or the store fails, or when neither is open.
pub(super) fn lead_workspace(store: &Store, env: &Env<'_>, lead: TaskId) -> Result<WorkspaceId> {
    let title = store.task(lead)?.name.to_string();
    let recorded = store
        .session_of(lead)?
        .and_then(|session| session.host_workspace)
        .map(WorkspaceId);
    let open = env.host.workspaces()?;
    let found = recorded
        .as_ref()
        .and_then(|id| open.iter().find(|w| &w.id == id))
        .or_else(|| open.iter().find(|w| w.title == title));
    match found {
        Some(workspace) => Ok(workspace.id.clone()),
        None => bail!(
            "the workspace of the lead {title} is gone (recorded: {}); a part or helper opens only in its lead's workspace",
            recorded.as_ref().map_or("none", WorkspaceId::as_str)
        ),
    }
}

/// The workspace a helper of `task` opens in: its lead's for a lead or a part ([`lead_workspace`]), else the one its
/// session recorded.
///
/// # Errors
///
/// Fails when the host or the store fails, or when the lead's workspace is gone.
pub(super) fn helper_workspace(
    store: &Store,
    env: &Env<'_>,
    task: &Task,
    recorded: Option<WorkspaceId>,
) -> Result<WorkspaceId> {
    let lead = match task.shape {
        TaskShape::Lead => task.id,
        TaskShape::Part { parent } => parent,
        TaskShape::Plain => {
            return recorded
                .ok_or_else(|| anyhow::anyhow!("this session has no workspace recorded"));
        }
    };
    lead_workspace(store, env, lead)
}

/// Opens `argv` as a new tab of the workspace a worker runs in, and returns the new surface.
///
/// # Errors
///
/// Fails when the host fails.
pub fn open_helper(
    env: &Env<'_>,
    workspace: &WorkspaceId,
    dir: &Path,
    argv: &[String],
) -> Result<SurfaceId> {
    let started = env
        .host
        .spawn(workspace, dir, HELPER_LABEL, argv, &Target::Tab)?;
    Ok(started.surface)
}

/// Closes the `headwork idle` placeholders still open in `workspace`. Builds before 2026-11-16 left
/// one in a pane its worker closed, to hold the pane for the next worker; panes now close with their worker instead.
fn sweep(host: &dyn TerminalHost, workspace: &WorkspaceId) -> Result<()> {
    let open: HashSet<SurfaceId> = host.surfaces(workspace)?.into_iter().collect();
    for program in host.panes()? {
        if program.label == IDLE_LABEL && open.contains(&program.surface) {
            host.close_surface(&program.surface)?;
        }
    }
    Ok(())
}

/// Today's tabs: a fresh workspace's shell is replaced, else the host picks the pane.
pub(super) fn tab(host: &dyn TerminalHost, workspace: &WorkspaceId, fresh: bool) -> Result<Target> {
    if !fresh {
        return Ok(Target::Tab);
    }
    Ok(host
        .layout(workspace)?
        .cells()
        .into_iter()
        .next()
        .map_or(Target::Tab, |cell| Target::Pane {
            pane: cell.pane,
            replace_tab: true,
        }))
}

fn pane(store: &Store, env: &Env<'_>, workspace: &WorkspaceId) -> Result<(Target, PlaceKind)> {
    sweep(env.host, workspace)?;
    let cells = env.host.layout(workspace)?.cells();
    let mut busy = busy_on_host(env.host)?;
    busy.extend(store.live_session_surfaces()?.into_iter().map(SurfaceId));
    let measured: Vec<Cell> = cells
        .iter()
        .map(|cell| Cell {
            width: cell.width,
            height: cell.height,
            surfaces: cell.surfaces.len(),
            busy: cell.surfaces.iter().filter(|s| busy.contains(*s)).count(),
        })
        .collect();
    let into = |pane: &PaneCell, replace_tab: bool| Target::Pane {
        pane: pane.pane.clone(),
        replace_tab,
    };
    let own = |pane: PaneId| {
        (
            Target::Pane {
                pane,
                replace_tab: true,
            },
            PlaceKind::Pane,
        )
    };
    let stacked = |cell: Option<&PaneCell>| {
        (
            cell.map_or(Target::Tab, |found| into(found, false)),
            PlaceKind::Tab,
        )
    };
    Ok(match layout::place(&measured, env.config.layout.panes) {
        Some(Placement::Free { cell, replace_tab }) => cells.get(cell).map_or_else(
            || stacked(None),
            |free| (into(free, replace_tab), PlaceKind::Pane),
        ),
        Some(Placement::Split { cell: 0, down }) => {
            let first = cells.first();
            let anchor = first.and_then(|cell| cell.surfaces.first());
            match anchor {
                Some(anchor) => split_first(env.host, workspace, anchor, down)?
                    .map_or_else(|| stacked(first), own),
                None => stacked(first),
            }
        }
        Some(Placement::Split { cell, .. }) => match cells.get(cell) {
            Some(found) => {
                let (pane, _shell) = env.host.split_pane(workspace, &found.pane)?;
                own(pane)
            }
            None => stacked(None),
        },
        Some(Placement::Stack { cell }) => stacked(cells.get(cell)),
        None => stacked(None),
    })
}

/// Splits the first pane of `workspace` and returns the new pane. wmux splits in the workspace of the surface it is
/// run from and, for a surface it does not know, in whatever workspace the user looks at; so a new pane that did not
/// land in `workspace` is closed again and `None` returned.
fn split_first(
    host: &dyn TerminalHost,
    workspace: &WorkspaceId,
    anchor: &SurfaceId,
    down: bool,
) -> Result<Option<PaneId>> {
    let (pane, _shell) = host.split(anchor, down)?;
    let landed = host
        .layout(workspace)?
        .cells()
        .iter()
        .any(|found| found.pane == pane);
    if landed {
        Ok(Some(pane))
    } else {
        host.close_pane(&pane)?;
        Ok(None)
    }
}

/// The surfaces the host sees holding a session: a program it runs other than a placeholder, or a Claude session it
/// saw in a surface it did not start (one restored after a restart).
fn busy_on_host(host: &dyn TerminalHost) -> Result<HashSet<SurfaceId>> {
    let mut busy: HashSet<SurfaceId> = host
        .panes()?
        .into_iter()
        .filter(|program| program.running && program.label != IDLE_LABEL)
        .map(|program| program.surface)
        .collect();
    busy.extend(
        host.surface_states()?
            .into_iter()
            .filter(|state| state.session_id.is_some())
            .map(|state| state.surface),
    );
    Ok(busy)
}
