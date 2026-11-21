//! What `headwork reap` ends: pure decisions over a snapshot of the machine's processes.
//!
//! Two kinds of leftovers are ended. The processes a session's launcher and `claude` left behind once the session
//! ended (what its Job Object or process group held), found by their parent links and start times, because a Job Object of another
//! process cannot be asked for its members. And `tail` and `bash` whose parent is gone and which have been idle for
//! [`IDLE_AFTER_S`]: the log tails and shells of sessions that died without a job.
//!
//! On Windows, Git Bash's `fork` and `exec` leave a shell's Windows parent pointing at a process that already exited, so the
//! snapshot links each Git Bash process to its parent through Git Bash's own process list. When that list cannot be
//! read, no shell is judged orphaned.
//!
//! Some processes are never ended, nor anything they started, nor anything that started them: `claude`, `me-studio`,
//! `headwork` itself (launchers, finishers, MCP servers) and every registered job.

use std::collections::{BTreeSet, HashMap};

use serde::Serialize;

use crate::process::START_SLACK_S;

/// How long an orphaned shell must have run, in seconds, before it counts as idle.
pub const IDLE_AFTER_S: u64 = 600;

/// The share of one core, in thousandths, an orphaned shell may have used over its life and still count as idle.
pub const IDLE_CPU_PERMILLE: u64 = 10;

/// How long after a session ended, in seconds, its leftovers are left alone, so a closing launcher can finish.
pub const SETTLE_S: u64 = 60;

/// How far, in seconds, a session's launcher or `claude` may have started from the session's recorded start.
pub const ROOT_START_WINDOW_S: u64 = 120;

/// Programs never ended, with everything they started and everything that started them.
pub const PROTECTED_IMAGES: [&str; 3] = ["claude", "me-studio", "headwork"];

/// Programs ended when orphaned and idle.
pub const ORPHAN_IMAGES: [&str; 2] = ["tail", "bash"];

/// One process of the snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Proc {
    /// Its id.
    pub pid: u32,
    /// The id of the process that started it: for a Git Bash process its Git Bash parent, else the parent the system
    /// recorded. That process may be gone, and its id reused.
    pub parent: Option<u32>,
    /// The program image, such as `bash.exe` on Windows or `bash` elsewhere.
    pub image: String,
    /// When it started, in seconds since the unix epoch.
    pub started: u64,
    /// The CPU time it used, in milliseconds.
    pub cpu_ms: u64,
}

/// A session that ended, with the processes that held its tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndedSession {
    /// Its name: the task's, or the orchestrator's.
    pub name: String,
    /// The launcher's and `claude`'s ids, as recorded.
    pub roots: Vec<u32>,
    /// When it started, in seconds since the unix epoch.
    pub started: u64,
    /// When it ended, in seconds since the unix epoch; absent when it was found gone without an end.
    pub ended: Option<u64>,
}

/// Everything `reap` decides on.
#[derive(Debug, Clone, Copy)]
pub struct Facts<'a> {
    /// Now, in seconds since the unix epoch.
    pub now: u64,
    /// Every running process.
    pub procs: &'a [Proc],
    /// The sessions that ended.
    pub sessions: &'a [EndedSession],
    /// The processes of registered jobs that still run, checked against their recorded identities.
    pub jobs: &'a [u32],
    /// Git Bash's process list was read, so the parents of its shells are known; without it no shell counts as
    /// orphaned.
    pub shells_known: bool,
}

/// Why a process is ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Reason {
    /// It belonged to the tree of a session that ended.
    Session {
        /// The session's name.
        name: String,
    },
    /// A `tail` or `bash` whose parent is gone, idle with everything it started.
    Orphan,
}

/// Why a process that looked like a leftover is left running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Spare {
    /// It is, runs under, or started a protected program or a registered job.
    Protected {
        /// The session whose tree it belongs to, if any.
        session: Option<String>,
    },
    /// An orphaned shell that started a program other than `tail` or `bash`.
    RunsOthers,
    /// An orphaned shell that ran for less than [`IDLE_AFTER_S`] or used the CPU.
    Busy,
}

/// A process with what becomes of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verdict<T> {
    /// Its id.
    pub pid: u32,
    /// Its program.
    pub image: String,
    /// When it started, which the kill checks again.
    pub started: u64,
    /// Why.
    pub why: T,
}

/// What `reap` does.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Plan {
    /// The processes to end, those started last first.
    pub end: Vec<Verdict<Reason>>,
    /// The leftovers left running, and why.
    pub spare: Vec<Verdict<Spare>>,
}

/// The program's name without folder or extension, lower case.
#[must_use]
pub fn stem(image: &str) -> String {
    let name = image.rsplit(['/', '\\']).next().unwrap_or(image);
    let name = name.to_ascii_lowercase();
    name.strip_suffix(".exe")
        .map_or_else(|| name.clone(), str::to_owned)
}

struct Tree<'a> {
    by_pid: HashMap<u32, &'a Proc>,
    children: HashMap<u32, Vec<u32>>,
}

impl<'a> Tree<'a> {
    fn new(procs: &'a [Proc]) -> Self {
        let by_pid: HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
        let mut tree = Self {
            by_pid,
            children: HashMap::new(),
        };
        for proc in procs {
            if let Some(parent) = tree.parent(proc) {
                tree.children.entry(parent.pid).or_default().push(proc.pid);
            }
        }
        tree
    }

    /// The live process that started `proc`: the recorded parent, unless it started after `proc` and so only took
    /// over the id.
    fn parent(&self, proc: &Proc) -> Option<&'a Proc> {
        let parent = self.by_pid.get(&proc.parent?)?;
        (parent.pid != proc.pid && parent.started <= proc.started.saturating_add(START_SLACK_S))
            .then_some(*parent)
    }

    fn descendants(&self, root: u32, into: &mut BTreeSet<u32>) {
        let mut stack = vec![root];
        while let Some(pid) = stack.pop() {
            if into.insert(pid)
                && let Some(children) = self.children.get(&pid)
            {
                stack.extend(children);
            }
        }
    }

    fn ancestors(&self, pid: u32, into: &mut BTreeSet<u32>) {
        let mut seen = BTreeSet::new();
        let mut current = self.by_pid.get(&pid).copied();
        while let Some(proc) = current {
            if !seen.insert(proc.pid) {
                break;
            }
            into.insert(proc.pid);
            current = self.parent(proc);
        }
    }

    fn protected(&self, jobs: &[u32]) -> BTreeSet<u32> {
        let roots: Vec<u32> = self
            .by_pid
            .values()
            .filter(|p| {
                PROTECTED_IMAGES.contains(&stem(&p.image).as_str()) || jobs.contains(&p.pid)
            })
            .map(|p| p.pid)
            .collect();
        let mut out = BTreeSet::new();
        for root in roots {
            self.descendants(root, &mut out);
            self.ancestors(root, &mut out);
        }
        out
    }

    /// The processes of an ended session's tree that still run.
    fn session_members(&self, session: &EndedSession, now: u64) -> BTreeSet<u32> {
        let end = session.ended.unwrap_or(now);
        let from = session.started.saturating_sub(START_SLACK_S);
        let until = end.saturating_add(START_SLACK_S);
        let mut members = BTreeSet::new();
        for &root in &session.roots {
            let holder = self.by_pid.get(&root).copied();
            let root_alive = holder.is_some_and(|p| {
                p.started.abs_diff(session.started) <= ROOT_START_WINDOW_S && p.started <= until
            });
            if root_alive {
                self.descendants(root, &mut members);
                continue;
            }
            let before = holder.map_or(u64::MAX, |p| p.started);
            let orphans: Vec<u32> = self
                .by_pid
                .values()
                .filter(|p| {
                    p.parent == Some(root)
                        && p.pid != root
                        && (from..=until).contains(&p.started)
                        && p.started < before
                })
                .map(|p| p.pid)
                .collect();
            for pid in orphans {
                self.descendants(pid, &mut members);
            }
        }
        members
    }

    fn verdict<T>(&self, pid: u32, why: T) -> Option<Verdict<T>> {
        self.by_pid.get(&pid).map(|p| Verdict {
            pid,
            image: p.image.clone(),
            started: p.started,
            why,
        })
    }

    fn idle(&self, pid: u32, now: u64) -> bool {
        self.by_pid.get(&pid).is_some_and(|p| {
            let age = now.saturating_sub(p.started);
            age >= IDLE_AFTER_S && p.cpu_ms <= age.saturating_mul(IDLE_CPU_PERMILLE)
        })
    }
}

/// Decides which processes `reap` ends and which leftovers it leaves, and why.
#[must_use]
pub fn plan(facts: &Facts<'_>) -> Plan {
    let tree = Tree::new(facts.procs);
    let protected = tree.protected(facts.jobs);
    let mut taken = BTreeSet::new();
    let mut plan = Plan::default();
    for session in facts.sessions {
        let settled = session
            .ended
            .is_none_or(|ended| facts.now >= ended.saturating_add(SETTLE_S));
        for pid in tree.session_members(session, facts.now) {
            if !taken.insert(pid) || !settled {
                continue;
            }
            if protected.contains(&pid) {
                plan.spare.extend(tree.verdict(
                    pid,
                    Spare::Protected {
                        session: Some(session.name.clone()),
                    },
                ));
            } else {
                plan.end.extend(tree.verdict(
                    pid,
                    Reason::Session {
                        name: session.name.clone(),
                    },
                ));
            }
        }
    }
    let orphan_roots: Vec<&Proc> = facts
        .procs
        .iter()
        .filter(|_| facts.shells_known)
        .filter(|p| ORPHAN_IMAGES.contains(&stem(&p.image).as_str()) && tree.parent(p).is_none())
        .filter(|p| !taken.contains(&p.pid) && !protected.contains(&p.pid))
        .collect();
    for root in orphan_roots {
        let mut group = BTreeSet::new();
        tree.descendants(root.pid, &mut group);
        if group.iter().any(|pid| taken.contains(pid)) {
            continue;
        }
        taken.extend(group.iter().copied());
        let others = group.iter().any(|pid| {
            tree.by_pid
                .get(pid)
                .is_none_or(|p| !ORPHAN_IMAGES.contains(&stem(&p.image).as_str()))
        });
        let why = if group.iter().any(|pid| protected.contains(pid)) {
            Some(Spare::Protected { session: None })
        } else if others {
            Some(Spare::RunsOthers)
        } else if group.iter().all(|pid| tree.idle(*pid, facts.now)) {
            None
        } else {
            Some(Spare::Busy)
        };
        match why {
            None => plan.end.extend(
                group
                    .iter()
                    .filter_map(|pid| tree.verdict(*pid, Reason::Orphan)),
            ),
            Some(spare) => plan.spare.extend(tree.verdict(root.pid, spare)),
        }
    }
    plan.end
        .sort_by(|a, b| b.started.cmp(&a.started).then(b.pid.cmp(&a.pid)));
    plan.spare.sort_by_key(|v| v.pid);
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 100_000;

    fn p(pid: u32, parent: Option<u32>, image: &str, started: u64) -> Proc {
        Proc {
            pid,
            parent,
            image: image.to_owned(),
            started,
            cpu_ms: 0,
        }
    }

    fn session(roots: &[u32], started: u64, ended: Option<u64>) -> EndedSession {
        EndedSession {
            name: "me-sim-x".to_owned(),
            roots: roots.to_vec(),
            started,
            ended,
        }
    }

    fn ended(plan: &Plan) -> Vec<u32> {
        plan.end.iter().map(|v| v.pid).collect()
    }

    #[test]
    fn stems_drop_folder_extension_and_case() {
        for (image, expected) in [
            ("bash.exe", "bash"),
            ("C:\\Program Files\\Git\\usr\\bin\\BASH.EXE", "bash"),
            ("/usr/bin/tail", "tail"),
            ("me-studio.exe", "me-studio"),
        ] {
            assert_eq!(stem(image), expected, "{image}");
        }
    }

    #[test]
    fn the_leftovers_of_an_ended_session_are_ended_when_it_settled() {
        let procs = [
            p(10, Some(1), "bash.exe", 50_100),
            p(11, Some(10), "cargo.exe", 50_200),
            p(12, Some(2), "tail.exe", 50_300),
            p(13, Some(1), "node.exe", 40_000),
            p(14, Some(1), "node.exe", 70_000),
        ];
        let sessions = [session(&[1, 2], 50_000, Some(60_000))];
        let facts = Facts {
            now: NOW,
            procs: &procs,
            sessions: &sessions,
            jobs: &[],
            shells_known: true,
        };
        let plan = plan(&facts);
        assert_eq!(ended(&plan), vec![12, 11, 10], "{plan:?}");
        assert!(
            plan.end.iter().all(|v| v.why
                == Reason::Session {
                    name: "me-sim-x".to_owned()
                }),
            "{plan:?}"
        );
        let early = Facts {
            now: 60_000 + SETTLE_S - 1,
            ..facts
        };
        assert!(super::plan(&early).end.is_empty(), "not settled yet");
    }

    #[test]
    fn a_reused_root_pid_keeps_its_own_children() {
        let procs = [
            p(1, None, "explorer.exe", 80_000),
            p(20, Some(1), "bash.exe", 80_100),
        ];
        let sessions = [session(&[1], 50_000, None)];
        let plan = plan(&Facts {
            now: NOW,
            procs: &procs,
            sessions: &sessions,
            jobs: &[],
            shells_known: true,
        });
        assert!(plan.end.is_empty(), "{plan:?}");
    }

    #[test]
    fn protected_programs_and_jobs_are_spared_with_their_trees() {
        let procs = [
            p(10, Some(1), "bash.exe", 50_100),
            p(11, Some(10), "claude.exe", 50_200),
            p(12, Some(11), "bash.exe", 50_300),
            p(20, Some(1), "bash.exe", 50_400),
            p(21, Some(20), "cars-rl.exe", 50_500),
            p(22, Some(21), "python.exe", 50_600),
            p(30, Some(1), "me-studio.exe", 50_700),
            p(40, Some(1), "headwork.exe", 50_800),
            p(41, Some(40), "git.exe", 50_900),
            p(50, Some(1), "tail.exe", 51_000),
        ];
        let sessions = [session(&[1], 50_000, Some(60_000))];
        let plan = plan(&Facts {
            now: NOW,
            procs: &procs,
            sessions: &sessions,
            jobs: &[21],
            shells_known: true,
        });
        assert_eq!(ended(&plan), vec![50], "{plan:?}");
        let spared: Vec<u32> = plan.spare.iter().map(|v| v.pid).collect();
        assert_eq!(spared, vec![10, 11, 12, 20, 21, 22, 30, 40, 41], "{plan:?}");
    }

    #[test]
    fn a_live_root_counts_only_near_the_session_start() {
        let procs = [
            p(1, None, "node.exe", 50_050),
            p(2, Some(1), "bash.exe", 50_100),
        ];
        let sessions = [session(&[1], 50_000, Some(60_000))];
        let plan = plan(&Facts {
            now: NOW,
            procs: &procs,
            sessions: &sessions,
            jobs: &[],
            shells_known: true,
        });
        assert_eq!(ended(&plan), vec![2, 1], "{plan:?}");
        let late = [p(1, None, "node.exe", 50_000 + ROOT_START_WINDOW_S + 1)];
        let plan = super::plan(&Facts {
            now: NOW,
            procs: &late,
            sessions: &sessions,
            jobs: &[],
            shells_known: true,
        });
        assert!(plan.end.is_empty(), "{plan:?}");
    }

    #[test]
    fn orphaned_idle_shells_are_ended_and_busy_or_young_ones_spared() {
        let old = NOW - IDLE_AFTER_S - 100;
        let busy = Proc {
            cpu_ms: IDLE_AFTER_S * IDLE_CPU_PERMILLE * 10,
            ..p(30, None, "bash.exe", old)
        };
        let procs = [
            p(1, None, "explorer.exe", 1),
            p(10, Some(999), "bash.exe", old),
            p(11, Some(10), "tail.exe", old + 1),
            p(12, None, "tail.exe", old - 50),
            p(20, Some(998), "bash.exe", NOW - IDLE_AFTER_S + 1),
            busy,
            p(40, Some(997), "bash.exe", old),
            p(41, Some(40), "cargo.exe", old),
            p(50, Some(1), "bash.exe", old),
            p(60, Some(61), "tail.exe", old),
            p(61, None, "explorer.exe", old + 100),
            p(70, Some(996), "bash.exe", old),
            p(71, Some(70), "claude.exe", old),
        ];
        let plan = plan(&Facts {
            now: NOW,
            procs: &procs,
            sessions: &[],
            jobs: &[],
            shells_known: true,
        });
        assert_eq!(ended(&plan), vec![11, 60, 10, 12], "{plan:?}");
        let blind = super::plan(&Facts {
            now: NOW,
            procs: &procs,
            sessions: &[],
            jobs: &[],
            shells_known: false,
        });
        assert_eq!(
            blind,
            Plan::default(),
            "no shell is judged without Git Bash's list"
        );
        let spared: Vec<(u32, Spare)> = plan.spare.iter().map(|v| (v.pid, v.why.clone())).collect();
        assert_eq!(
            spared,
            vec![
                (20, Spare::Busy),
                (30, Spare::Busy),
                (40, Spare::RunsOthers),
            ],
            "{plan:?}"
        );
    }

    #[test]
    fn a_job_reached_through_git_bash_links_shields_its_forked_shells() {
        let old = NOW - IDLE_AFTER_S - 100;
        let procs = [
            p(27360, None, "headwork.exe", 1),
            p(4852, Some(27360), "bash.exe", old),
            p(37324, Some(4852), "bash.exe", old),
            p(45556, Some(37324), "bash.exe", old),
            p(18632, Some(45556), "nice.exe", old + 1),
            p(52564, Some(18632), "cars-rl-gpu.exe", old + 1),
        ];
        let plan = plan(&Facts {
            now: NOW,
            procs: &procs,
            sessions: &[],
            jobs: &[4852],
            shells_known: true,
        });
        assert_eq!(plan, Plan::default(), "{plan:?}");
    }

    #[test]
    fn a_process_in_two_trees_is_decided_once() {
        let procs = [p(10, Some(1), "bash.exe", 50_100)];
        let sessions = [
            session(&[1], 50_000, Some(60_000)),
            session(&[1], 50_000, Some(60_000)),
        ];
        let plan = plan(&Facts {
            now: NOW,
            procs: &procs,
            sessions: &sessions,
            jobs: &[],
            shells_known: true,
        });
        assert_eq!(ended(&plan), vec![10], "{plan:?}");
    }
}
