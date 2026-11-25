//! `headwork clean`: removes the worktrees no task or agent needs any more, lists the ones it leaves and why, and with
//! `gc` runs plain `git gc` in each main checkout where no main-only work runs.
//!
//! A task's worktree goes through the project's own remove command (me-sim's `scripts/worktree.sh remove` refuses
//! unmerged work); a Claude Code agent worktree inside a main checkout (`.claude/worktrees/agent-*`) through git. The
//! decisions are `headwork_core::clean`'s.

use std::path::Path;

use anyhow::Result;
use headwork_adapters::worktree::{WorktreeEntry, Worktrees, same_path};
use headwork_core::clean::{
    AgentWorktree, BranchFate, GcBlock, Lock, Plan, TaskWorktree, plan_agent, plan_task,
};
use headwork_core::lifecycle::WorktreeEnd;
use headwork_core::{JobState, ProjectConfig, ProjectName, SessionState};
use headwork_store::{Store, TaskId, WorktreeRow};
use serde::Serialize;

use super::close::RemovalMode;
use super::{Env, slashed};

/// The folder, inside a main checkout, where Claude Code puts its agents' worktrees.
pub const AGENT_WORKTREES: &str = ".claude/worktrees";

/// What `clean` is asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CleanOptions {
    /// Print the plan and change nothing.
    pub dry_run: bool,
    /// Also run `git gc` in each quiet main checkout.
    pub gc: bool,
}

/// What `clean` did, or would do, per project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CleanReport {
    /// Nothing was changed.
    pub dry_run: bool,
    /// Each configured project.
    pub projects: Vec<ProjectClean>,
}

impl CleanReport {
    /// Whether a removal or `git gc` failed.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.projects.iter().any(|project| {
            project.error.is_some()
                || matches!(project.gc, Some(GcOutcome::Failed { .. }))
                || project
                    .worktrees
                    .iter()
                    .any(|worktree| matches!(worktree.outcome, Outcome::Failed { .. }))
                || project
                    .branches
                    .iter()
                    .any(|branch| matches!(branch.outcome, BranchOutcome::Failed { .. }))
        })
    }
}

/// One project's worktrees and `git gc`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectClean {
    /// The project.
    pub project: String,
    /// Its main checkout.
    pub main: String,
    /// Why the project could not be read at all.
    pub error: Option<String>,
    /// Why fetching the upstream of the main checkout's branch failed, when it did: what counts as merged is then
    /// judged on what was fetched before.
    pub fetch_error: Option<String>,
    /// Its worktrees besides the main checkout.
    pub worktrees: Vec<WorktreeClean>,
    /// The branches closed tasks left behind because they looked unmerged at close.
    pub branches: Vec<BranchClean>,
    /// `git gc`, when asked for.
    pub gc: Option<GcOutcome>,
}

/// One worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorktreeClean {
    /// The folder.
    pub path: String,
    /// The branch checked out there.
    pub branch: Option<String>,
    /// Whose it is: `task <name>`, `helper of <name>`, `agent`, or `nobody`.
    pub owner: String,
    /// What became of it.
    pub outcome: Outcome,
    /// What else happened, such as the branch kept or deleted.
    pub note: Option<String>,
}

/// A branch a closed task left behind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BranchClean {
    /// The branch.
    pub branch: String,
    /// The task that left it: `task <name>`.
    pub owner: String,
    /// What became of it.
    pub outcome: BranchOutcome,
}

/// What became of a branch a closed task left behind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum BranchOutcome {
    /// A dry run: the main checkout or its upstream contains it, so it would be deleted.
    WouldDelete,
    /// Deleted: the main checkout or its upstream contains it.
    Deleted,
    /// Left in place.
    Left {
        /// Why.
        reason: String,
    },
    /// The deletion was tried and failed.
    Failed {
        /// Why.
        error: String,
    },
}

/// Why a branch a closed task left stays.
const BRANCH_UNMERGED: &str = "neither the main checkout nor its upstream contains it";

/// What became of one worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum Outcome {
    /// A dry run: it would be removed.
    WouldRemove,
    /// Removed.
    Removed,
    /// A dry run: its folder is gone and git would forget it.
    WouldPrune,
    /// Its folder was gone; git forgot it.
    Pruned,
    /// Left in place.
    Left {
        /// Why.
        reason: String,
    },
    /// The removal was tried and failed.
    Failed {
        /// Why.
        error: String,
    },
}

/// What became of `git gc` in a main checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "gc")]
pub enum GcOutcome {
    /// A dry run: it would run.
    WouldRun,
    /// It ran.
    Ran,
    /// It did not run.
    Skipped {
        /// Why.
        reason: String,
    },
    /// It ran and failed.
    Failed {
        /// Why.
        error: String,
    },
}

/// Cleans every configured project.
///
/// # Errors
///
/// Fails when the store cannot be read. A project or worktree git cannot handle is reported, never fatal.
pub fn clean(store: &mut Store, env: &Env<'_>, options: CleanOptions) -> Result<CleanReport> {
    let mut projects = Vec::new();
    for (name, project) in &env.config.projects {
        projects.push(clean_project(store, env, name, project, options)?);
    }
    Ok(CleanReport {
        dry_run: options.dry_run,
        projects,
    })
}

fn clean_project(
    store: &mut Store,
    env: &Env<'_>,
    name: &ProjectName,
    project: &ProjectConfig,
    options: CleanOptions,
) -> Result<ProjectClean> {
    let main = env.home.expand(&project.main);
    let mut report = ProjectClean {
        project: name.to_string(),
        main: slashed(&main),
        error: None,
        fetch_error: None,
        worktrees: Vec::new(),
        branches: Vec::new(),
        gc: None,
    };
    if !main.is_dir() {
        report.error = Some("the main checkout does not exist".to_owned());
        return Ok(report);
    }
    let worktrees = Worktrees::new(env.shell(), &main);
    let entries = match worktrees.entries() {
        Ok(entries) => entries,
        Err(error) => {
            report.error = Some(error.to_string());
            return Ok(report);
        }
    };
    report.fetch_error = worktrees
        .fetch_upstream()
        .err()
        .map(|error| error.to_string());
    let recorded = store.recorded_worktrees()?;
    for entry in entries
        .iter()
        .filter(|entry| !same_path(&entry.path, &main))
    {
        let owner = recorded
            .iter()
            .find(|(_, row, _)| same_path(Path::new(&row.path), &entry.path));
        let path = slashed(&entry.path);
        let result = match owner {
            _ if is_agent_worktree(&main, &entry.path) => {
                agent_worktree(store, env, &worktrees, entry, options.dry_run)
            }
            Some((task, row, helper)) => task_worktree(
                store,
                env,
                &worktrees,
                entry,
                &Recorded {
                    task: *task,
                    row,
                    helper: *helper,
                },
                options.dry_run,
            ),
            None => Ok(WorktreeClean {
                path: path.clone(),
                branch: entry.branch.clone(),
                owner: "nobody".to_owned(),
                outcome: Outcome::Left {
                    reason: headwork_core::clean::Leave::NoTask.to_string(),
                },
                note: None,
            }),
        };
        report
            .worktrees
            .push(result.unwrap_or_else(|error| WorktreeClean {
                path,
                branch: entry.branch.clone(),
                owner: "unknown".to_owned(),
                outcome: Outcome::Failed {
                    error: format!("{error:#}"),
                },
                note: None,
            }));
    }
    report.branches = kept_branches(store, env, name, &worktrees, options.dry_run)?;
    if options.gc {
        report.gc = Some(gc(store, env, name, &main, &worktrees, options.dry_run)?);
    }
    Ok(report)
}

/// Deletes the branches closed tasks of the project kept because they looked unmerged at close, once the main
/// checkout or its upstream contains them; lists the rest. A branch that is gone is not listed.
fn kept_branches(
    store: &Store,
    env: &Env<'_>,
    project: &ProjectName,
    worktrees: &Worktrees,
    dry_run: bool,
) -> Result<Vec<BranchClean>> {
    let mut out = Vec::new();
    for (task, branch) in store.kept_branches(project.as_str())? {
        if !worktrees.branch_exists(&branch)? {
            continue;
        }
        let owner = format!("task {}", store.task(task)?.name);
        let outcome = if !worktrees.is_merged(&branch)? {
            BranchOutcome::Left {
                reason: BRANCH_UNMERGED.to_owned(),
            }
        } else if dry_run {
            BranchOutcome::WouldDelete
        } else {
            match worktrees.delete_merged_branch(&branch) {
                Ok(()) => BranchOutcome::Deleted,
                Err(error) => BranchOutcome::Failed {
                    error: error.to_string(),
                },
            }
        };
        if !dry_run && !matches!(outcome, BranchOutcome::Left { .. }) {
            store.append_event(
                env.actor,
                Some(task),
                None,
                "branch-cleaned",
                &serde_json::json!({ "branch": branch, "outcome": outcome }),
            )?;
        }
        out.push(BranchClean {
            branch,
            owner,
            outcome,
        });
    }
    Ok(out)
}

/// Whether `path` is a Claude Code agent worktree inside the main checkout.
#[must_use]
pub fn is_agent_worktree(main: &Path, path: &Path) -> bool {
    path.parent()
        .is_some_and(|parent| same_path(parent, &main.join(AGENT_WORKTREES)))
}

struct Recorded<'a> {
    task: TaskId,
    row: &'a WorktreeRow,
    helper: bool,
}

fn task_worktree(
    store: &mut Store,
    env: &Env<'_>,
    worktrees: &Worktrees,
    entry: &WorktreeEntry,
    recorded: &Recorded<'_>,
    dry_run: bool,
) -> Result<WorktreeClean> {
    let task = store.task(recorded.task)?;
    let exists = entry.path.is_dir();
    let session_live = store.session_of(task.id)?.is_some_and(|session| {
        matches!(session.state, SessionState::Live | SessionState::Starting)
    });
    let dirty = exists && worktrees.is_dirty(&entry.path)?;
    let owner = if recorded.helper {
        format!("helper of {}", task.name)
    } else {
        format!("task {}", task.name)
    };
    let mut report = WorktreeClean {
        path: slashed(&entry.path),
        branch: entry.branch.clone(),
        owner,
        outcome: Outcome::Removed,
        note: None,
    };
    let plan = plan_task(TaskWorktree {
        state: task.state,
        session_live,
        dirty,
    });
    if let Plan::Leave { reason } = plan {
        report.outcome = Outcome::Left {
            reason: reason.to_string(),
        };
        return Ok(report);
    }
    report.outcome = match (dry_run, exists) {
        (true, true) => Outcome::WouldRemove,
        (true, false) => Outcome::WouldPrune,
        (false, false) => {
            worktrees.prune()?;
            mark_removed(store, env, recorded)?;
            Outcome::Pruned
        }
        (false, true) if recorded.helper => {
            super::helpers::remove_one(store, env, &task, recorded.row)?
                .map_or(Outcome::Removed, |error| Outcome::Failed { error })
        }
        (false, true) => {
            let was = recorded.row.state.as_str();
            if was != "present" {
                store.set_worktree_state(task.id, "present", env.actor)?;
            }
            match super::close::remove_worktree(store, env, task.id, RemovalMode::Now)? {
                WorktreeEnd::Removed | WorktreeEnd::NoWorktree => Outcome::Removed,
                WorktreeEnd::Present | WorktreeEnd::Kept => {
                    let error = store
                        .removal_attempts(task.id)?
                        .last_error
                        .unwrap_or_else(|| "the remove command refused".to_owned());
                    if was == "kept" {
                        store.set_worktree_state(task.id, "kept", env.actor)?;
                    }
                    Outcome::Failed { error }
                }
            }
        }
    };
    if !dry_run {
        log(store, env, Some(task.id), &report)?;
    }
    Ok(report)
}

fn mark_removed(store: &mut Store, env: &Env<'_>, recorded: &Recorded<'_>) -> Result<()> {
    if recorded.helper {
        store.set_helper_worktree_state(recorded.task, &recorded.row.path, "removed", env.actor)?;
    } else {
        store.set_worktree_state(recorded.task, "removed", env.actor)?;
    }
    Ok(())
}

fn agent_worktree(
    store: &Store,
    env: &Env<'_>,
    worktrees: &Worktrees,
    entry: &WorktreeEntry,
    dry_run: bool,
) -> Result<WorktreeClean> {
    let exists = entry.path.is_dir();
    let lock = entry.locked.as_deref().map_or(Lock::Unlocked, |reason| {
        Lock::from_reason(reason, |pid| {
            headwork_adapters::proc::is_alive(pid).unwrap_or(true)
        })
    });
    let dirty = exists && worktrees.is_dirty(&entry.path)?;
    let branch = if exists {
        Some(fate(worktrees, entry)?)
    } else {
        None
    };
    let path = slashed(&entry.path);
    let mut report = WorktreeClean {
        path: path.clone(),
        branch: entry.branch.clone(),
        owner: "agent".to_owned(),
        outcome: Outcome::Removed,
        note: None,
    };
    let plan = plan_agent(&AgentWorktree {
        folder_exists: exists,
        lock,
        dirty,
        branch,
    });
    match plan {
        Plan::Leave { reason } => {
            report.outcome = Outcome::Left {
                reason: reason.to_string(),
            };
            return Ok(report);
        }
        Plan::Prune { unlock } => {
            report.outcome = Outcome::WouldPrune;
            if !dry_run {
                if unlock {
                    worktrees.unlock(&path)?;
                }
                worktrees.prune()?;
                report.outcome = Outcome::Pruned;
            }
        }
        Plan::Remove {
            unlock,
            delete_branch,
        } => {
            report.note = branch_note(entry.branch.as_deref(), delete_branch, dry_run);
            report.outcome = Outcome::WouldRemove;
            if !dry_run {
                if unlock {
                    worktrees.unlock(&path)?;
                }
                report.outcome = match worktrees.remove_path(&path) {
                    Ok(()) => Outcome::Removed,
                    Err(error) => Outcome::Failed {
                        error: error.to_string(),
                    },
                };
                if let (Outcome::Removed, true, Some(branch)) =
                    (&report.outcome, delete_branch, entry.branch.as_deref())
                    && let Err(error) = worktrees.delete_merged_branch(branch)
                {
                    report.note = Some(format!("branch {branch} kept: {error}"));
                }
            }
        }
    }
    if !dry_run {
        log(store, env, None, &report)?;
    }
    Ok(report)
}

fn branch_note(branch: Option<&str>, delete: bool, dry_run: bool) -> Option<String> {
    let branch = branch?;
    Some(match (delete, dry_run) {
        (true, true) => format!("branch {branch} would be deleted: the main checkout contains it"),
        (true, false) => format!("branch {branch} deleted: the main checkout contains it"),
        (false, _) => {
            format!("branch {branch} kept: its work landed without a merge of the branch")
        }
    })
}

fn fate(worktrees: &Worktrees, entry: &WorktreeEntry) -> Result<BranchFate> {
    let revision = entry.branch.as_deref().or(entry.head.as_deref());
    let Some(revision) = revision else {
        return Ok(BranchFate::Unmerged);
    };
    if worktrees.is_merged(revision)? {
        return Ok(BranchFate::Merged);
    }
    let Some(branch) = entry.branch.as_deref() else {
        return Ok(BranchFate::Unmerged);
    };
    if worktrees.is_superseded(branch)? {
        return Ok(BranchFate::Superseded);
    }
    if worktrees.upstream_gone(branch)? {
        return Ok(BranchFate::UpstreamGone);
    }
    Ok(BranchFate::Unmerged)
}

fn log(store: &Store, env: &Env<'_>, task: Option<TaskId>, report: &WorktreeClean) -> Result<()> {
    if matches!(report.outcome, Outcome::Left { .. }) {
        return Ok(());
    }
    store.append_event(
        env.actor,
        task,
        None,
        "worktree-cleaned",
        &serde_json::to_value(report)?,
    )?;
    Ok(())
}

/// What keeps `git gc` out of a main checkout: a registered job running there, or a live main-only task.
///
/// # Errors
///
/// Fails when the store fails.
pub fn gc_block(store: &Store, project: &ProjectName, main: &Path) -> Result<Option<GcBlock>> {
    for job in store.open_jobs(None)? {
        if job.project != project.as_str()
            || !matches!(job.state, JobState::Running | JobState::Stopping)
        {
            continue;
        }
        let task_in_main = match job.task_id {
            Some(id) => store.task(id)?.in_main,
            None => false,
        };
        if task_in_main || same_path(Path::new(&job.cwd), main) {
            return Ok(Some(GcBlock::Job { label: job.label }));
        }
    }
    let board = store.board(false)?;
    Ok(board
        .tasks
        .iter()
        .map(|entry| &entry.task)
        .find(|task| task.project() == project && task.in_main && task.state.takes_slot())
        .map(|task| GcBlock::MainTask {
            task: task.name.to_string(),
        }))
}

fn gc(
    store: &Store,
    env: &Env<'_>,
    project: &ProjectName,
    main: &Path,
    worktrees: &Worktrees,
    dry_run: bool,
) -> Result<GcOutcome> {
    if let Some(block) = gc_block(store, project, main)? {
        return Ok(GcOutcome::Skipped {
            reason: block.to_string(),
        });
    }
    if dry_run {
        return Ok(GcOutcome::WouldRun);
    }
    let outcome = match worktrees.gc() {
        Ok(()) => GcOutcome::Ran,
        Err(error) => GcOutcome::Failed {
            error: error.to_string(),
        },
    };
    store.append_event(
        env.actor,
        None,
        None,
        "git-gc",
        &serde_json::json!({ "project": project.as_str(), "outcome": outcome }),
    )?;
    Ok(outcome)
}
