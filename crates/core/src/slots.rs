//! When a worker session gives up its slot: it waits on the user and runs nothing else.
//!
//! A session that waits in a dialog (an `AskUserQuestion`, a permission prompt, a lead's `Close` question) and runs no
//! background shell, job, background subagent or part does no work, so it stops counting against `[machine]
//! max_workers` and a queued task may start in its place. All sessions together, waiting ones included, stay within
//! `max_workers` plus `[machine] idle_overflow`; past that, a waiting session keeps its slot. When the user answers, the
//! session counts again at once, even over the cap, and the queue starts nothing until the count is back under it
//! (`docs/decisions.md`, 2026-11-22, waiting sessions free their slot).

use std::collections::HashSet;

use crate::reap::{Proc, stem};

/// Programs that are a shell of `claude`'s own when it runs them straight.
///
/// Such a shell is a `Bash` or `PowerShell` command, perhaps running in the background, or a `Monitor`. `cmd` is not
/// one: on Windows `claude` starts MCP servers through `cmd /c`.
pub const SHELL_IMAGES: [&str; 6] = ["bash", "sh", "zsh", "fish", "pwsh", "powershell"];

/// What tells whether a session waits on the user and runs nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent fact the rule weighs"
)]
pub struct WaitFacts {
    /// An ask of the session waits on the user in a dialog.
    pub waiting_ask: bool,
    /// A registered job of the task runs.
    pub running_job: bool,
    /// A part of the lead is not closed.
    pub live_part: bool,
    /// Background work of this turn (a background `Bash`, `Monitor`, `Agent` or `Workflow`) has not sent its
    /// notification yet; an `Agent` or `Workflow` runs inside `claude`, so no process shows it.
    pub background_work: bool,
    /// `claude` runs a shell of its own ([`busy_shells`]).
    pub shell_running: bool,
}

impl WaitFacts {
    /// Whether the session runs nothing: no job, live part, background work or shell.
    #[must_use]
    pub const fn runs_nothing(self) -> bool {
        !self.running_job && !self.live_part && !self.background_work && !self.shell_running
    }

    /// Whether the session frees its slot.
    #[must_use]
    pub const fn frees_slot(self) -> bool {
        self.waiting_ask && self.runs_nothing()
    }
}

/// The shells `claude` (pid `claude`) runs straight, leaving out the one that leads to `own`: a hook runs in a shell of
/// `claude`, so the process asking is never counted as the session's work.
#[must_use]
pub fn busy_shells(procs: &[Proc], claude: u32, own: u32) -> Vec<u32> {
    let parents: std::collections::HashMap<u32, Option<u32>> =
        procs.iter().map(|p| (p.pid, p.parent)).collect();
    let mut mine = HashSet::new();
    let mut at = Some(own);
    while let Some(pid) = at {
        if !mine.insert(pid) {
            break;
        }
        at = parents.get(&pid).copied().flatten();
    }
    procs
        .iter()
        .filter(|p| p.parent == Some(claude) && p.pid != claude && !mine.contains(&p.pid))
        .filter(|p| SHELL_IMAGES.contains(&stem(&p.image).as_str()))
        .map(|p| p.pid)
        .collect()
}

/// The nearest `claude` above `own`: the session a hook runs for.
#[must_use]
pub fn claude_of(procs: &[Proc], own: u32) -> Option<u32> {
    let by_pid: std::collections::HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut seen = HashSet::new();
    let mut at = by_pid.get(&own).and_then(|p| p.parent);
    while let Some(pid) = at {
        if !seen.insert(pid) {
            return None;
        }
        let process = by_pid.get(&pid)?;
        if stem(&process.image) == "claude" {
            return Some(pid);
        }
        at = process.parent;
    }
    None
}

/// Whether a queued task may take a slot: fewer than `max_workers` sessions work, and all sessions, waiting ones too,
/// are fewer than `max_workers` plus `idle_overflow`.
#[must_use]
pub const fn has_room(working: u32, waiting: u32, max_workers: u32, idle_overflow: u32) -> bool {
    let open = working.saturating_add(waiting);
    let limit = max_workers.saturating_add(idle_overflow);
    working < max_workers && open < limit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, parent: Option<u32>, image: &str) -> Proc {
        Proc {
            pid,
            parent,
            image: image.to_owned(),
            started: 0,
            cpu_ms: 0,
            cwd: None,
        }
    }

    #[test]
    fn frees_only_when_waiting_and_idle() {
        let idle = WaitFacts {
            waiting_ask: true,
            ..WaitFacts::default()
        };
        assert!(
            idle.frees_slot(),
            "a waiting session that runs nothing frees its slot"
        );
        assert!(
            !WaitFacts::default().frees_slot(),
            "a session that does not wait keeps it"
        );
        for busy in [
            WaitFacts {
                running_job: true,
                ..idle
            },
            WaitFacts {
                live_part: true,
                ..idle
            },
            WaitFacts {
                background_work: true,
                ..idle
            },
            WaitFacts {
                shell_running: true,
                ..idle
            },
        ] {
            assert!(!busy.frees_slot(), "{busy:?} keeps its slot");
            assert!(!busy.runs_nothing(), "{busy:?} runs something");
        }
        assert!(WaitFacts::default().runs_nothing(), "nothing runs");
    }

    #[test]
    fn counts_claudes_own_shells_but_not_the_hooks_or_mcp_servers() {
        let procs = [
            proc(10, Some(1), "claude.exe"),
            proc(11, Some(10), "cmd.exe"),
            proc(12, Some(11), "node.exe"),
            proc(13, Some(10), "headwork.exe"),
            proc(14, Some(10), "bash.exe"),
            proc(15, Some(14), "headwork.exe"),
            proc(16, Some(10), "bash.exe"),
            proc(17, Some(16), "cargo.exe"),
            proc(18, Some(12), "bash.exe"),
        ];
        assert_eq!(
            busy_shells(&procs, 10, 15),
            vec![16],
            "the background shell, not the hook's shell"
        );
        assert_eq!(
            busy_shells(&procs, 10, 99),
            vec![14, 16],
            "an unknown caller hides nothing"
        );
        assert!(
            busy_shells(&procs[..4], 10, 15).is_empty(),
            "MCP servers are no work"
        );
        assert_eq!(claude_of(&procs, 15), Some(10), "the hook finds its claude");
        assert_eq!(
            claude_of(&procs, 10),
            None,
            "claude itself has none above it"
        );
        assert_eq!(claude_of(&procs, 99), None, "an unknown process has none");
    }

    #[test]
    fn room_counts_working_under_the_cap_and_all_under_the_overflow() {
        assert!(has_room(7, 0, 8, 4), "a free slot");
        assert!(!has_room(8, 0, 8, 4), "every slot works");
        assert!(has_room(5, 6, 8, 4), "waiting sessions gave up their slots");
        assert!(!has_room(5, 7, 8, 4), "the overflow is full");
        assert!(
            !has_room(5, 3, 8, 0),
            "no overflow: waiting sessions keep their slots"
        );
        assert!(!has_room(9, 0, 8, 4), "answered sessions run over the cap");
    }
}
