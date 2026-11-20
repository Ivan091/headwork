//! What `headwork clean` does with each worktree of a main checkout and whether it may run `git gc` there: pure
//! decisions over facts the binary gathers.
//!
//! Nothing is removed while a session or an agent may still use it, while it holds uncommitted changes or untracked
//! files, or while its branch holds work the main checkout lacks. A branch is only ever deleted when the main
//! checkout's `HEAD` already contains it; a worktree whose work landed another way (a squash, a re-applied commit, a
//! deleted remote branch) is removed and its branch kept.

use serde::Serialize;

use crate::TaskState;

/// Where a worktree's branch stands against the main checkout's `HEAD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchFate {
    /// `HEAD` contains its tip (a detached worktree counts by its commit).
    Merged,
    /// Every commit beyond `HEAD` has a commit of the same subject in `HEAD`: squashed or re-applied.
    Superseded,
    /// Its upstream branch was deleted on the remote, as GitHub does after a merge.
    UpstreamGone,
    /// It holds commits `HEAD` lacks.
    Unmerged,
}

/// A worktree's git lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Lock {
    /// Not locked.
    Unlocked,
    /// Locked by a Claude Code agent (`claude agent <name> (pid N)`).
    Agent {
        /// The agent's process.
        pid: u32,
        /// That process still runs.
        alive: bool,
    },
    /// Locked for another reason, by someone else.
    Other {
        /// The lock's reason.
        reason: String,
    },
}

impl Lock {
    /// Reads `git worktree list --porcelain`'s `locked` reason, given whether a process runs.
    #[must_use]
    pub fn from_reason(reason: &str, alive: impl Fn(u32) -> bool) -> Self {
        agent_pid(reason).map_or_else(
            || Self::Other {
                reason: reason.trim().to_owned(),
            },
            |pid| Self::Agent {
                pid,
                alive: alive(pid),
            },
        )
    }
}

/// The process a Claude Code agent lock names: `claude agent agent-1234 (pid 4711)`.
#[must_use]
pub fn agent_pid(reason: &str) -> Option<u32> {
    let rest = reason.trim().strip_prefix("claude agent ")?;
    let (_, pid) = rest.rsplit_once("(pid ")?;
    pid.strip_suffix(')')?.trim().parse().ok()
}

/// What is known of a Claude Code agent worktree inside a main checkout (`.claude/worktrees/agent-*`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentWorktree {
    /// Its folder exists.
    pub folder_exists: bool,
    /// Its lock.
    pub lock: Lock,
    /// It holds uncommitted changes or untracked files.
    pub dirty: bool,
    /// Its branch; absent when the folder is gone.
    pub branch: Option<BranchFate>,
}

/// What is known of a worktree the store records for a task, its own or a helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TaskWorktree {
    /// The task's state.
    pub state: TaskState,
    /// The task's latest session still runs.
    pub session_live: bool,
    /// It holds uncommitted changes or untracked files.
    pub dirty: bool,
}

/// What to do with one worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "do")]
pub enum Plan {
    /// Remove it, without force.
    Remove {
        /// Unlock it first: its agent is gone.
        unlock: bool,
        /// Delete its branch afterwards with `git branch -d`: `HEAD` contains it.
        delete_branch: bool,
    },
    /// Its folder is gone: forget it with `git worktree prune`.
    Prune {
        /// Unlock it first: its agent is gone.
        unlock: bool,
    },
    /// Leave it, for this reason.
    Leave {
        /// Why.
        reason: Leave,
    },
}

/// Why a worktree is left.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "why")]
pub enum Leave {
    /// It is the main checkout itself.
    MainCheckout,
    /// No task of headwork owns it and it is no agent worktree.
    NoTask,
    /// Its task is neither merged nor closed.
    TaskActive {
        /// The task's state.
        state: TaskState,
    },
    /// Its task is merged but its session still runs.
    SessionLive,
    /// It holds uncommitted changes or untracked files.
    Dirty,
    /// The agent that locked it still runs.
    AgentRunning {
        /// The agent's process.
        pid: u32,
    },
    /// Someone else locked it.
    Locked {
        /// The lock's reason.
        reason: String,
    },
    /// Its branch holds commits the main checkout lacks.
    Unmerged,
}

impl std::fmt::Display for Leave {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MainCheckout => f.write_str("the main checkout"),
            Self::NoTask => f.write_str("no headwork task owns it"),
            Self::TaskActive { state } => write!(f, "its task is {state}"),
            Self::SessionLive => f.write_str("its task is merged but its session still runs"),
            Self::Dirty => f.write_str("it holds uncommitted changes or untracked files"),
            Self::AgentRunning { pid } => write!(f, "its agent (pid {pid}) still runs"),
            Self::Locked { reason } => write!(f, "locked: {reason}"),
            Self::Unmerged => f.write_str("its branch holds commits the main checkout lacks"),
        }
    }
}

/// A task's worktree goes when the task is closed, or merged with its session ended, and the tree is clean. The
/// project's own remove command still has the last word (me-sim's refuses unmerged work).
#[must_use]
pub const fn plan_task(facts: TaskWorktree) -> Plan {
    let reason = match (facts.state, facts.session_live) {
        (TaskState::Closed, _) | (TaskState::Merged, false) => {
            if facts.dirty {
                Leave::Dirty
            } else {
                return Plan::Remove {
                    unlock: false,
                    delete_branch: false,
                };
            }
        }
        (TaskState::Merged, true) => Leave::SessionLive,
        (state, _) => Leave::TaskActive { state },
    };
    Plan::Leave { reason }
}

/// An agent worktree goes when its agent is gone, the tree is clean and its branch is merged, superseded or gone
/// upstream; one whose folder is gone is pruned.
#[must_use]
pub fn plan_agent(facts: &AgentWorktree) -> Plan {
    let unlock = match &facts.lock {
        Lock::Unlocked => false,
        Lock::Agent { alive: false, .. } => true,
        Lock::Agent { pid, alive: true } => {
            return Plan::Leave {
                reason: Leave::AgentRunning { pid: *pid },
            };
        }
        Lock::Other { reason } => {
            return Plan::Leave {
                reason: Leave::Locked {
                    reason: reason.clone(),
                },
            };
        }
    };
    if !facts.folder_exists {
        return Plan::Prune { unlock };
    }
    if facts.dirty {
        return Plan::Leave {
            reason: Leave::Dirty,
        };
    }
    match facts.branch {
        Some(BranchFate::Merged) => Plan::Remove {
            unlock,
            delete_branch: true,
        },
        Some(BranchFate::Superseded | BranchFate::UpstreamGone) => Plan::Remove {
            unlock,
            delete_branch: false,
        },
        Some(BranchFate::Unmerged) | None => Plan::Leave {
            reason: Leave::Unmerged,
        },
    }
}

/// Why `git gc` does not run in a main checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "why")]
pub enum GcBlock {
    /// A registered job runs there.
    Job {
        /// The job's label.
        label: String,
    },
    /// A main-only task runs there.
    MainTask {
        /// The task.
        task: String,
    },
}

impl std::fmt::Display for GcBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Job { label } => write!(f, "job `{label}` runs in the main checkout"),
            Self::MainTask { task } => write!(f, "main-only task {task} runs in the main checkout"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLEAN_MERGED: AgentWorktree = AgentWorktree {
        folder_exists: true,
        lock: Lock::Unlocked,
        dirty: false,
        branch: Some(BranchFate::Merged),
    };

    #[test]
    fn agent_lock_reasons_name_their_pid() {
        let cases = [
            (
                "claude agent agent-af5756f0552673c99 (pid 47132)",
                Some(47132),
            ),
            ("claude agent agent-x (pid 7)\n", Some(7)),
            ("claude agent agent-x", None),
            ("kept by hand (pid 7)", None),
            ("", None),
        ];
        for (reason, pid) in cases {
            assert_eq!(agent_pid(reason), pid, "{reason:?}");
        }
        assert_eq!(
            Lock::from_reason("on a USB disk", |_| true),
            Lock::Other {
                reason: "on a USB disk".to_owned()
            },
            "another lock"
        );
        assert_eq!(
            Lock::from_reason("claude agent a (pid 9)", |pid| pid == 9),
            Lock::Agent {
                pid: 9,
                alive: true
            },
            "alive"
        );
    }

    #[test]
    fn task_worktrees_go_only_when_the_task_is_over_and_the_tree_clean() {
        let remove = Plan::Remove {
            unlock: false,
            delete_branch: false,
        };
        let leave = |reason| Plan::Leave { reason };
        let cases = [
            (TaskState::Closed, false, false, remove.clone()),
            (TaskState::Closed, true, false, remove.clone()),
            (TaskState::Merged, false, false, remove),
            (TaskState::Merged, true, false, leave(Leave::SessionLive)),
            (TaskState::Closed, false, true, leave(Leave::Dirty)),
            (TaskState::Merged, false, true, leave(Leave::Dirty)),
            (
                TaskState::Working,
                false,
                false,
                leave(Leave::TaskActive {
                    state: TaskState::Working,
                }),
            ),
            (
                TaskState::InReview,
                false,
                false,
                leave(Leave::TaskActive {
                    state: TaskState::InReview,
                }),
            ),
            (
                TaskState::Interrupted,
                false,
                false,
                leave(Leave::TaskActive {
                    state: TaskState::Interrupted,
                }),
            ),
            (
                TaskState::Closing,
                false,
                false,
                leave(Leave::TaskActive {
                    state: TaskState::Closing,
                }),
            ),
        ];
        for (state, session_live, dirty, plan) in cases {
            let facts = TaskWorktree {
                state,
                session_live,
                dirty,
            };
            assert_eq!(plan_task(facts), plan, "{facts:?}");
        }
    }

    #[test]
    fn agent_worktrees_go_when_their_agent_is_gone_and_their_work_landed() {
        let dead = Lock::Agent {
            pid: 3,
            alive: false,
        };
        let cases = [
            (
                CLEAN_MERGED,
                Plan::Remove {
                    unlock: false,
                    delete_branch: true,
                },
            ),
            (
                AgentWorktree {
                    lock: dead,
                    branch: Some(BranchFate::Superseded),
                    ..CLEAN_MERGED
                },
                Plan::Remove {
                    unlock: true,
                    delete_branch: false,
                },
            ),
            (
                AgentWorktree {
                    branch: Some(BranchFate::UpstreamGone),
                    ..CLEAN_MERGED
                },
                Plan::Remove {
                    unlock: false,
                    delete_branch: false,
                },
            ),
            (
                AgentWorktree {
                    branch: Some(BranchFate::Unmerged),
                    ..CLEAN_MERGED
                },
                Plan::Leave {
                    reason: Leave::Unmerged,
                },
            ),
            (
                AgentWorktree {
                    dirty: true,
                    ..CLEAN_MERGED
                },
                Plan::Leave {
                    reason: Leave::Dirty,
                },
            ),
        ];
        for (facts, plan) in cases {
            assert_eq!(plan_agent(&facts), plan, "{facts:?}");
        }
    }

    #[test]
    fn agent_worktrees_stay_while_locked_by_a_live_agent_or_someone_else() {
        let dead = Lock::Agent {
            pid: 3,
            alive: false,
        };
        let cases = [
            (
                AgentWorktree {
                    lock: dead.clone(),
                    ..CLEAN_MERGED
                },
                Plan::Remove {
                    unlock: true,
                    delete_branch: true,
                },
            ),
            (
                AgentWorktree {
                    lock: Lock::Agent {
                        pid: 4,
                        alive: true,
                    },
                    ..CLEAN_MERGED
                },
                Plan::Leave {
                    reason: Leave::AgentRunning { pid: 4 },
                },
            ),
            (
                AgentWorktree {
                    lock: Lock::Other {
                        reason: "mine".to_owned(),
                    },
                    ..CLEAN_MERGED
                },
                Plan::Leave {
                    reason: Leave::Locked {
                        reason: "mine".to_owned(),
                    },
                },
            ),
            (
                AgentWorktree {
                    folder_exists: false,
                    lock: dead,
                    dirty: false,
                    branch: None,
                },
                Plan::Prune { unlock: true },
            ),
            (
                AgentWorktree {
                    folder_exists: false,
                    lock: Lock::Agent {
                        pid: 5,
                        alive: true,
                    },
                    dirty: false,
                    branch: None,
                },
                Plan::Leave {
                    reason: Leave::AgentRunning { pid: 5 },
                },
            ),
        ];
        for (facts, plan) in cases {
            assert_eq!(plan_agent(&facts), plan, "{facts:?}");
        }
    }
}
