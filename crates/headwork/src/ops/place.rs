//! Where a child's pane opens in its project's workspace, and what a closing child leaves behind (`[layout]`).
//!
//! In `panes` mode each child gets a pane of its own, up to `layout.panes`; later children stack as tabs. The second
//! child cuts the workspace's lone pane into the whole grid at once, and later ones take the free panes. A child that
//! closes while it is alone in its pane first starts `headwork idle` there as a placeholder, so the pane stays for the
//! next child, and the workspace closes once nothing in it holds a session. The choice is
//! [`headwork_core::layout::place`]; this module reads the workspace from the host as it is now, so panes the user
//! closed or rearranged by hand count as they stand. Children already open are never moved. `tabs` mode adds tabs.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use headwork_adapters::host::{PaneCell, PaneId, SurfaceId, Target, TerminalHost, WorkspaceId};
use headwork_core::layout::{self, Cell, Placement};
use headwork_core::{LayoutMode, PlaceKind};
use headwork_store::{Place, Store, Task};

use super::{Env, label, title};

/// The host label of a free pane's placeholder. It has no `/`, so the tick and import, which read `<project>/<slug>`
/// labels, never take it for a child.
pub const IDLE_LABEL: &str = "headwork:free";

/// A child's pane to open.
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
    let (target, kind) = match env.config.layout.mode {
        LayoutMode::Tabs => (
            tab(env.host, opening.workspace, opening.fresh)?,
            PlaceKind::Tab,
        ),
        LayoutMode::Panes => pane(store, env, opening.workspace)?,
    };
    let started = env.host.spawn(
        opening.workspace,
        opening.dir,
        &label(opening.task),
        opening.argv,
        &target,
    )?;
    title::set(env, opening.task, &started.surface).ok();
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
/// In `tabs` mode the workspace closes when it holds nothing else.
/// In `panes` mode a surface alone in its pane first gets a placeholder beside it, so
/// the pane stays for the next child.
///
/// # Errors
///
/// Returns the host's failure.
pub fn release(env: &Env<'_>, workspace: Option<&WorkspaceId>, surface: &SurfaceId) -> Result<()> {
    let host = env.host;
    if let Some(workspace) = workspace {
        let surfaces = host.surfaces(workspace)?;
        let last = match env.config.layout.mode {
            LayoutMode::Tabs => surfaces.iter().all(|s| s == surface),
            LayoutMode::Panes => {
                let busy = busy_on_host(host)?;
                !surfaces.iter().any(|s| s != surface && busy.contains(s))
            }
        };
        if last {
            host.close_workspace(workspace)?;
            return Ok(());
        }
        if env.config.layout.mode == LayoutMode::Panes {
            keep_pane(env, workspace, surface)?;
        }
    }
    host.close_surface(surface)?;
    Ok(())
}

fn keep_pane(env: &Env<'_>, workspace: &WorkspaceId, surface: &SurfaceId) -> Result<()> {
    let cells = env.host.layout(workspace)?.cells();
    let Some(alone) = cells
        .iter()
        .find(|cell| cell.surfaces.len() == 1 && cell.surfaces.contains(surface))
    else {
        return Ok(());
    };
    let title = env
        .host
        .workspaces()?
        .into_iter()
        .find(|w| &w.id == workspace)
        .map_or_else(String::new, |w| w.title);
    let argv = [env.exe.display().to_string(), "idle".to_owned(), title];
    env.host.spawn(
        workspace,
        env.home.root(),
        IDLE_LABEL,
        &argv,
        &Target::Pane {
            pane: alone.pane.clone(),
            replace_tab: false,
        },
    )?;
    Ok(())
}

/// Today's tabs: a fresh workspace's shell is replaced, else the host picks the pane.
fn tab(host: &dyn TerminalHost, workspace: &WorkspaceId, fresh: bool) -> Result<Target> {
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
        Some(Placement::Grid { cell, count }) => match cells.get(cell) {
            Some(lone) => env
                .host
                .grid(workspace, &lone.pane, count.get())?
                .into_iter()
                .next()
                .map_or_else(|| stacked(Some(lone)), own),
            None => stacked(None),
        },
        Some(Placement::Split { down }) => {
            let first = cells.first();
            let anchor = first.and_then(|cell| cell.surfaces.first());
            match anchor {
                Some(anchor) => split_beside(env.host, workspace, anchor, down)?
                    .map_or_else(|| stacked(first), own),
                None => stacked(first),
            }
        }
        Some(Placement::Stack { cell }) => stacked(cells.get(cell)),
        None => stacked(None),
    })
}

/// Splits the first pane of `workspace` and returns the new pane. wmux splits in the workspace of the surface it is
/// run from and, for a surface it does not know, in whatever workspace the user looks at; so a new pane that did not
/// land in `workspace` is closed again and `None` returned.
fn split_beside(
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
