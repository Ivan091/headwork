//! Sessions that wait on the user and run nothing else give up their worker slot (`headwork_core::slots`).
//!
//! The hook that opens an ask (`AskUserQuestion`, a permission prompt) checks the session at once and marks it; the
//! session's next hook call clears the mark, since it works again. The tick checks every session with a waiting ask
//! again, so a background shell that ended while the dialog was open frees the slot then. When a mark frees a slot and
//! a task is queued, the queue runs.

use std::fmt;
use std::path::Path;

use anyhow::Result;
use headwork_adapters::proc;
use headwork_core::Actor;
use headwork_core::reap::Proc;
use headwork_core::slots::{WaitFacts, busy_shells, claude_of};
use headwork_store::{Store, TaskId};

use crate::home::Home;

/// What the slot check reads from the machine and starts on it; tests replace it.
pub trait SlotProbe: fmt::Debug {
    /// The machine's processes, with the parents the system recorded.
    fn processes(&self) -> Vec<Proc>;

    /// Starts `headwork queue` detached, so a freed slot is taken without waiting for the tick.
    ///
    /// # Errors
    ///
    /// Fails when the program does not start.
    fn pump(&self) -> Result<()>;
}

/// The real machine.
#[derive(Debug, Clone, Copy)]
pub struct LiveSlots<'a> {
    /// The home folder the queue runs on.
    pub home: &'a Home,
    /// The `headwork` program.
    pub exe: &'a Path,
}

impl SlotProbe for LiveSlots<'_> {
    fn processes(&self) -> Vec<Proc> {
        proc::processes()
    }

    fn pump(&self) -> Result<()> {
        use super::update::UpdateStarter;
        super::update::Detached {
            home: self.home,
            exe: self.exe,
        }
        .start(&["queue".to_owned()])
        .map(|_| ())
    }
}

/// One session to check.
#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    /// The Claude session id.
    pub session: &'a str,
    /// Its task.
    pub task: TaskId,
    /// Its `claude` process, when recorded; else the `claude` above this process, as for a hook.
    pub claude_pid: Option<u32>,
}

/// The facts of one session; `procs` is read only when the store alone does not settle it.
///
/// # Errors
///
/// Fails when the store fails.
pub fn facts(
    store: &Store,
    candidate: Candidate<'_>,
    procs: &dyn Fn() -> Vec<Proc>,
) -> Result<WaitFacts> {
    let mut facts = WaitFacts {
        waiting_ask: store.dialog_ask_waits(candidate.session)?,
        ..WaitFacts::default()
    };
    if !facts.waiting_ask {
        return Ok(facts);
    }
    facts.running_job = super::jobs::has_running_job(store, candidate.task)?;
    facts.live_part = store.live_parts(candidate.task)? > 0;
    facts.background_agent = store.background_agent_since_prompt(candidate.session)?;
    if facts.frees_slot() {
        let procs = procs();
        let own = std::process::id();
        facts.shell_running = candidate
            .claude_pid
            .or_else(|| claude_of(&procs, own))
            .is_none_or(|claude| !busy_shells(&procs, claude, own).is_empty());
    }
    Ok(facts)
}

/// Checks one session and sets or clears its mark. Returns whether it is marked now and the mark changed.
///
/// # Errors
///
/// Fails when the store fails.
pub fn check(
    store: &mut Store,
    candidate: Candidate<'_>,
    procs: &dyn Fn() -> Vec<Proc>,
    actor: Actor,
) -> Result<bool> {
    let frees = facts(store, candidate, procs)?.frees_slot();
    let changed = store.set_session_waiting(candidate.session, frees, actor)?;
    Ok(frees && changed)
}

/// Runs the queue in a process of its own when a slot was freed and a task waits for one; a failure to start it is
/// logged, never raised, since the tick runs the queue too.
///
/// # Errors
///
/// Fails when the store fails.
pub fn pump_if_queued(store: &Store, probe: &dyn SlotProbe, task: TaskId) -> Result<()> {
    if !store.has_queued()? {
        return Ok(());
    }
    let error = probe.pump().err().map(|error| format!("{error:#}"));
    store.append_event(
        Actor::Hook,
        Some(task),
        None,
        "queue-triggered",
        &serde_json::json!({ "by": "slot-freed", "error": error }),
    )?;
    Ok(())
}

/// Checks every running session that carries the mark or has an ask waiting in its dialog, reading the processes
/// once. Returns how many sessions were newly marked.
///
/// # Errors
///
/// Fails when the store fails.
pub fn recheck_all(store: &mut Store, probe: &dyn SlotProbe, actor: Actor) -> Result<u32> {
    let candidates = store.waiting_candidates()?;
    if candidates.is_empty() {
        return Ok(0);
    }
    let snapshot = std::cell::OnceCell::new();
    let procs = || snapshot.get_or_init(|| probe.processes()).clone();
    let mut freed = 0_u32;
    for candidate in &candidates {
        let marked = check(
            store,
            Candidate {
                session: &candidate.session,
                task: candidate.task,
                claude_pid: candidate.claude_pid,
            },
            &procs,
            actor,
        )?;
        if marked {
            freed = freed.saturating_add(1);
        }
    }
    Ok(freed)
}

/// A machine for tests: processes set by hand, and a count of the queue runs asked for.
#[derive(Debug, Default)]
pub struct FakeSlots {
    procs: std::sync::Mutex<Vec<Proc>>,
    pumps: std::sync::atomic::AtomicU32,
}

impl FakeSlots {
    /// This process as a hook of a `claude` (pid [`FAKE_CLAUDE`]) that runs a shell of its own when `shell` is set.
    #[must_use]
    pub fn hook_of_claude(shell: bool) -> Self {
        let fake = Self::default();
        fake.set_shell(shell);
        fake
    }

    /// Starts or ends the `claude`'s own shell.
    pub fn set_shell(&self, shell: bool) {
        let own = std::process::id();
        let proc = |pid, parent, image: &str| Proc {
            pid,
            parent: Some(parent),
            image: image.to_owned(),
            started: 0,
            cpu_ms: 0,
            cwd: None,
        };
        let mut procs = vec![
            proc(FAKE_CLAUDE, 1, "claude.exe"),
            proc(FAKE_CLAUDE + 1, FAKE_CLAUDE, "bash.exe"),
            proc(own, FAKE_CLAUDE + 1, "headwork.exe"),
            proc(FAKE_CLAUDE + 2, FAKE_CLAUDE, "cmd.exe"),
        ];
        if shell {
            procs.push(proc(FAKE_CLAUDE + 3, FAKE_CLAUDE, "bash.exe"));
        }
        if let Ok(mut held) = self.procs.lock() {
            *held = procs;
        }
    }

    /// How many queue runs were asked for.
    #[must_use]
    pub fn pumps(&self) -> u32 {
        self.pumps.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// The pid of the fake `claude`.
pub const FAKE_CLAUDE: u32 = 4_000_000_000;

impl SlotProbe for FakeSlots {
    fn processes(&self) -> Vec<Proc> {
        self.procs
            .lock()
            .map(|procs| procs.clone())
            .unwrap_or_default()
    }

    fn pump(&self) -> Result<()> {
        self.pumps.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}
