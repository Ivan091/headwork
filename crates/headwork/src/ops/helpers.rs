//! Helper worktrees for the parts of a lead's task.
//!
//! Each is made by the project's worktree command beside the task's own, recorded under the task, writable for it, and
//! removed when it closes.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail, ensure};
use headwork_core::Slug;
use headwork_core::config::TaskVars;
use headwork_store::{Store, Task, TaskId};
use serde::Serialize;

use super::{Env, slashed};

/// A helper worktree made or found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Helper {
    /// The folder.
    pub path: String,
    /// The branch.
    pub branch: String,
    /// It existed already as a helper of this task.
    pub existing: bool,
}

/// Makes a helper worktree for `task` with slug and branch `slug`, at the folder the project's `worktree_path` gives
/// it, and records it under the task.
///
/// # Errors
///
/// Fails for a bad slug, a folder that exists and is not this task's helper, a task in the main checkout, or a
/// worktree command that fails or makes no folder.
pub fn add(store: &mut Store, env: &Env<'_>, task: &Task, slug: &str) -> Result<Helper> {
    let slug = Slug::new(slug)
        .map_err(|error| anyhow::anyhow!("`{slug}` is not a kebab-case slug: {error}"))?;
    if slug == *task.slug() {
        bail!("`{slug}` is this task's own slug: name the part");
    }
    let dir = env.worktree_dir(task, slug.as_str())?;
    let path = slashed(&dir);
    if let Some(known) = store
        .helper_worktrees(task.id)?
        .into_iter()
        .find(|helper| same(&helper.path, &dir))
    {
        return Ok(Helper {
            path: known.path,
            branch: known.branch,
            existing: true,
        });
    }
    if task.in_main {
        bail!("a task in the main checkout makes no worktrees");
    }
    if dir.exists() {
        bail!("{path} exists already and is not a helper of this task: pick another slug");
    }
    let main = slashed(&env.main_path(task)?);
    let vars = TaskVars {
        slug: slug.as_str(),
        branch: slug.as_str(),
        main: &main,
        worktree: &path,
    };
    env.worktrees(task)?
        .create(&env.project(task)?.worktree_new, &vars)?;
    ensure!(dir.is_dir(), "the worktree command did not create {path}");
    store.record_helper_worktree(task.id, &path, slug.as_str(), env.actor)?;
    Ok(Helper {
        path,
        branch: slug.as_str().to_owned(),
        existing: false,
    })
}

/// Removes a closing task's helper worktrees.
///
/// Each goes with the project's remove command, or without force when it holds no work. One that holds work stays,
/// and is logged `helper-worktree-refused` for the user; it never holds up the close.
///
/// # Errors
///
/// Fails when the store fails.
pub fn remove_all(store: &mut Store, env: &Env<'_>, id: TaskId) -> Result<Vec<PathBuf>> {
    let task = store.task(id)?;
    let mut left = Vec::new();
    for helper in store.helper_worktrees(id)? {
        let dir = PathBuf::from(&helper.path);
        if helper.state == "kept" {
            left.push(dir);
            continue;
        }
        if !dir.exists() {
            store.set_helper_worktree_state(id, &helper.path, "removed", env.actor)?;
            continue;
        }
        let main = slashed(&env.main_path(&task)?);
        let vars = TaskVars {
            slug: &helper.branch,
            branch: &helper.branch,
            main: &main,
            worktree: &helper.path,
        };
        let worktrees = env.worktrees(&task)?;
        let error = match worktrees.remove(&env.project(&task)?.worktree_remove, &vars) {
            Ok(()) if !dir.exists() => None,
            Ok(()) => Some("the remove command left the folder in place".to_owned()),
            Err(error) => Some(error.to_string()),
        };
        let removed = error.is_none() || (worktrees.remove_if_pristine(&vars)? && !dir.exists());
        if removed {
            store.set_helper_worktree_state(id, &helper.path, "removed", env.actor)?;
            drop(worktrees.delete_empty_branch(&helper.branch));
        } else {
            store.append_event(
                env.actor,
                Some(id),
                None,
                "helper-worktree-refused",
                &serde_json::json!({ "path": helper.path, "error": error }),
            )?;
            left.push(dir);
        }
    }
    Ok(left)
}

fn same(recorded: &str, dir: &Path) -> bool {
    headwork_adapters::worktree::same_path(Path::new(recorded), dir)
}
