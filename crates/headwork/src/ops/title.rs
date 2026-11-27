//! The title of a worker's pane or tab: the task name by default, from `[layout] title`.
//!
//! wmux keeps a title set with `rename-surface` apart from the one a program sets by escape sequence, so Claude's own
//! title never replaces it. It is set when a pane opens (spawn and resume), re-asserted on every tick in case the
//! user renamed or wmux restored a surface, and `headwork retitle` sets it for sessions started before this existed.
//!
//! A lead's part is a tab in its lead's workspace, whose title already names the task, so its tab follows
//! `[leads] part_title` instead (default `{part}`, the part's slug after its lead's).

use anyhow::Result;
use headwork_adapters::host::SurfaceId;
use headwork_core::{REVIEW_KIND, TaskShape};
use headwork_core::lead::part_label;
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

/// The title of a task's pane: `[layout] title`, or `[leads] part_title` for a lead's part.
///
/// # Errors
///
/// Fails when the store cannot read a part's lead.
pub fn title_of(store: &Store, env: &Env<'_>, task: &Task) -> Result<String> {
    let name = task.name.to_string();
    let effort = effort::of_task(env.home, env.config, task);
    let role = role_of(task);
    Ok(match task.shape {
        TaskShape::Part { parent } => {
            let lead = store.task(parent)?;
            env.config.leads.title_for(
                part_label(task.slug().as_str(), lead.slug().as_str()),
                &name,
                role,
                task.model.as_str(),
                Some(effort.as_str()),
            )
        }
        TaskShape::Lead | TaskShape::Plain => {
            env.config
                .layout
                .title_for(&name, role, task.model.as_str(), Some(effort.as_str()))
        }
    })
}

/// The kind of session a task is, for `{role}`: `reviewer` for a review part, else `lead`, `part` or `worker`.
#[must_use]
pub fn role_of(task: &Task) -> &'static str {
    if task.kind == REVIEW_KIND {
        return "reviewer";
    }
    match task.shape {
        TaskShape::Lead => "lead",
        TaskShape::Part { .. } => "part",
        TaskShape::Plain => "worker",
    }
}

/// Gives a task's surface its title.
///
/// # Errors
///
/// Returns the host's or the store's failure.
pub fn set(store: &Store, env: &Env<'_>, task: &Task, surface: &SurfaceId) -> Result<String> {
    let title = title_of(store, env, task)?;
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
        match set(store, env, &task, &SurfaceId(surface.clone())) {
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
