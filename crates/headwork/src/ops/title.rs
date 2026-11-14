//! The title of a child's pane or tab: `<task name> - <model> <effort>` by default, from `[layout] title`.
//!
//! wmux keeps a title set with `rename-surface` apart from the one a program sets by escape sequence, so Claude's own
//! title never replaces it. It is set when a pane opens (spawn and resume), re-asserted on every tick in case the
//! user renamed or wmux restored a surface, and `headwork retitle` sets it for sessions started before this existed.

use anyhow::Result;
use headwork_adapters::host::SurfaceId;
use headwork_store::{Store, Task};
use serde::Serialize;

use super::{Env, effort};

/// A surface that was given its title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Retitled {
    /// `<project>-<slug>`.
    pub name: String,
    /// The surface.
    pub surface: String,
    /// The title it now has.
    pub title: String,
}

/// The title of a task's pane.
#[must_use]
pub fn title_of(env: &Env<'_>, task: &Task) -> String {
    env.config.layout.title_for(
        &task.name.to_string(),
        task.model.as_str(),
        Some(effort::of_task(env.home, env.config, task).as_str()),
    )
}

/// Gives a task's surface its title.
///
/// # Errors
///
/// Returns the host's failure.
pub fn set(env: &Env<'_>, task: &Task, surface: &SurfaceId) -> Result<String> {
    let title = title_of(env, task);
    env.host.rename_surface(surface, &title)?;
    Ok(title)
}

/// Gives the surface of every live session of a task, or of every task when `only` is `None`, its title.
///
/// # Errors
///
/// Fails when the store fails; a surface the host refuses is skipped and its name listed in the second answer.
pub fn retitle(
    store: &Store,
    env: &Env<'_>,
    only: Option<&Task>,
) -> Result<(Vec<Retitled>, Vec<String>)> {
    let mut done = Vec::new();
    let mut failed = Vec::new();
    for (id, surface) in store.live_task_surfaces()? {
        if only.is_some_and(|task| task.id != id) {
            continue;
        }
        let task = store.task(id)?;
        match set(env, &task, &SurfaceId(surface.clone())) {
            Ok(title) => done.push(Retitled {
                name: task.name.to_string(),
                surface,
                title,
            }),
            Err(error) => failed.push(format!("{}: {error:#}", task.name)),
        }
    }
    Ok((done, failed))
}
