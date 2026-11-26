//! `headwork gate run` and `headwork gate status`: a heavy command waits for a machine-wide slot of its class, runs
//! holding it and gives it back (`docs/decisions/2026-11-26-gates-in-headwork.md`).
//!
//! The folders are [`headwork_adapters::gate`]; the decisions [`headwork_core::gate`]. A gate never fails a command it
//! could not gate: a slot root it cannot write, or a store it cannot record in, runs the command anyway.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use headwork_adapters::gate::{self, Attempt, ClassView, Held, Slots};
use headwork_core::config::GatesConfig;
use headwork_core::gate::{ClassLine, GateClass, Owner, gates_line, held_for, short_command};
use headwork_core::{Actor, Config, Role};
use headwork_store::{Store, TaskId};
use serde::Serialize;

use crate::home::Home;

/// How often a waiting gate says again what it waits for.
pub const REPORT_EVERY: Duration = Duration::from_secs(30);

/// How long a waiting gate sleeps between attempts.
pub const POLL: Duration = Duration::from_secs(1);

/// The exit code of a gate interrupted while it waited.
pub const INTERRUPTED: u8 = 130;

/// The exit code when the command could not be started.
pub const NOT_STARTED: u8 = 127;

/// Event: a gate has to wait for a slot.
pub const EVENT_GATE_WAIT: &str = "gate-wait";
/// Event: a gate took a slot (`waited_s`).
pub const EVENT_GATE_ACQUIRED: &str = "gate-acquired";
/// Event: a gate gave its slot back (`held_s`, `exit`).
pub const EVENT_GATE_RELEASED: &str = "gate-released";

/// The slot root for this home's config: `$HEADWORK_GATE_DIR`, else `[gates] dir`, else the default.
#[must_use]
pub fn gate_dir(home: &Home, gates: &GatesConfig) -> Option<PathBuf> {
    gate::resolve_dir(gates.dir.as_deref().map(|dir| home.expand(dir)))
}

/// The slot root when it exists as a folder: reading never creates it.
fn existing_dir(home: &Home, gates: &GatesConfig) -> Option<PathBuf> {
    gate_dir(home, gates).filter(|dir| dir.is_dir())
}

/// The `Gates: ...` line of `board`, or `None` when the slot folder does not exist.
#[must_use]
pub fn board_line(home: &Home, gates: &GatesConfig) -> Option<String> {
    let dir = existing_dir(home, gates)?;
    let classes: Vec<ClassLine> = Slots::new(dir)
        .read_all(gates)
        .iter()
        .map(|view| ClassLine {
            class: view.class,
            count: view.count,
            holders: view.live_holders().map(gate::HolderInfo::line).collect(),
            waiting: u32::try_from(view.live_waiters().count()).unwrap_or(u32::MAX),
        })
        .collect();
    Some(format!("Gates: {}\n", gates_line(&classes)))
}

/// The tasks with a live waiter, as `(task id, class)`; empty when the slot folder does not exist.
#[must_use]
pub fn waiting_tasks(home: &Home, gates: &GatesConfig) -> Vec<(i64, GateClass)> {
    let Some(dir) = existing_dir(home, gates) else {
        return Vec::new();
    };
    Slots::new(dir)
        .read_all(gates)
        .iter()
        .flat_map(|view| {
            view.live_waiters()
                .filter_map(|waiter| waiter.owner.as_ref()?.task_id)
                .map(|id| (id, view.class))
        })
        .collect()
}

/// Runs `argv` holding a slot of `class`, waiting for one first; returns the command's exit code.
///
/// Waiting prints `headwork: waiting for cpu (...)` to `err` at once and every [`REPORT_EVERY`]. In CI, with
/// `HEADWORK_GATE_OFF=1`, or when the process tree already holds `class`, the command runs directly. The command gets
/// `HEADWORK_GATE_HELD` with `class` added. An interrupt while waiting gives up with [`INTERRUPTED`]; once the command
/// runs, an interrupt is the command's to handle.
///
/// # Errors
///
/// Fails when `argv` is empty or writing to `err` fails.
pub fn run(home: &Home, class: GateClass, argv: &[OsString], err: &mut impl Write) -> Result<u8> {
    let (program, rest) = argv.split_first().context("no command to run")?;
    let config = home.load_config().ok();
    let gates = config
        .as_ref()
        .map(|config| config.gates.clone())
        .unwrap_or_default();
    let mut command = Command::new(program_path(program, config.as_ref()));
    command.args(rest);
    let env = gate::env_from_process();
    command.env(gate::HELD_VAR, env.held_with(class));
    if env.bypass(class).is_some() {
        return run_child(&mut command, err);
    }
    let text = command_text(argv);
    let Some(dir) = gate_dir(home, &gates) else {
        writeln!(
            err,
            "headwork: no folder for the {class} gate; running without a slot"
        )?;
        return run_child(&mut command, err);
    };
    let task_id = std::env::var(headwork_adapters::runtime::ENV_TASK)
        .ok()
        .and_then(|id| id.trim().parse::<i64>().ok());
    let events = Events {
        home,
        task: task_id.map(TaskId),
        actor: actor_from_env(),
        class,
        command: &text,
    };
    let owner = Owner {
        pid: std::process::id(),
        native: true,
        class: Some(class),
        task: task_id
            .and_then(|id| task_name(home, id))
            .or_else(|| task_id.map(|id| id.to_string()))
            .unwrap_or_default(),
        task_id,
        worktree: worktree(),
        command: text.clone(),
        started: local_now(),
        epoch: Some(gate::epoch_now()),
    };
    let slots = Slots::new(dir);
    let ticket = match slots.enqueue(class, gates.count(class), owner) {
        Ok(ticket) => ticket,
        Err(error) => {
            writeln!(err, "headwork: {error}; running without a slot")?;
            return run_child(&mut command, err);
        }
    };
    let waiting = Instant::now();
    let held = match wait_for_slot(ticket, &events, err)? {
        Waited::Held(held) => Some(held),
        Waited::Ungated => None,
        Waited::Interrupted => {
            writeln!(err, "headwork: interrupted while waiting for {class}")?;
            return Ok(INTERRUPTED);
        }
    };
    if let Some(held) = &held {
        events.record(
            EVENT_GATE_ACQUIRED,
            serde_json::json!({ "waited_s": waiting.elapsed().as_secs(), "slot": held.slot() }),
        );
    }
    let holding = Instant::now();
    let code = run_child(&mut command, err)?;
    if let Some(held) = held {
        if let Err(error) = held.release() {
            writeln!(err, "headwork: {error}")?;
        }
        events.record(
            EVENT_GATE_RELEASED,
            serde_json::json!({ "held_s": holding.elapsed().as_secs(), "exit": code }),
        );
    }
    Ok(code)
}

/// How waiting for a slot ended.
enum Waited {
    /// A slot is held.
    Held(Held),
    /// Trying failed; the command runs without a slot.
    Ungated,
    /// An interrupt came first.
    Interrupted,
}

/// Tries for a slot until one is held, trying fails, or an interrupt comes.
fn wait_for_slot(
    mut ticket: gate::Ticket,
    events: &Events<'_>,
    err: &mut impl Write,
) -> Result<Waited> {
    let interrupts = gate::watch_interrupts();
    let interrupted = || {
        interrupts
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst))
    };
    let mut told: Option<Instant> = None;
    loop {
        match ticket.try_take(&local_now(), gate::epoch_now()) {
            Ok(Attempt::Held(held)) => return Ok(Waited::Held(held)),
            Ok(Attempt::Wait {
                view,
                ahead,
                reap_error,
            }) => {
                if told.is_none() {
                    events.record(EVENT_GATE_WAIT, serde_json::json!({ "ahead": ahead }));
                }
                if told.is_none_or(|at| at.elapsed() >= REPORT_EVERY) {
                    writeln!(err, "headwork: {}", view.wait_message(ahead))?;
                    if let Some(error) = reap_error {
                        writeln!(
                            err,
                            "headwork: a stale {} slot could not be taken away: {error}",
                            view.class
                        )?;
                    }
                    told = Some(Instant::now());
                }
            }
            Err(error) => {
                writeln!(err, "headwork: {error}; running without a slot")?;
                return Ok(Waited::Ungated);
            }
        }
        let until = Instant::now() + POLL;
        while Instant::now() < until {
            if interrupted() {
                return Ok(Waited::Interrupted);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// The program to start: the configured shell (else Git Bash) for a bare `bash` or `sh` or WSL's `bash`, as jobs do.
fn program_path(program: &std::ffi::OsStr, config: Option<&Config>) -> PathBuf {
    let Some(text) = program.to_str() else {
        return PathBuf::from(program);
    };
    let shell = headwork_adapters::shell::resolve(
        config.and_then(|config| config.machine.shell.as_deref()),
    );
    super::jobs::program_for(text, &shell)
}

/// Runs the command with this process's standard streams and returns its exit code: the code itself when it fits a
/// byte, 128 plus the signal for one a signal ended, else 1.
fn run_child(command: &mut Command, err: &mut impl Write) -> Result<u8> {
    let status = match command.status() {
        Ok(status) => status,
        Err(error) => {
            writeln!(
                err,
                "headwork: starting `{}`: {error}",
                command.get_program().to_string_lossy()
            )?;
            return Ok(NOT_STARTED);
        }
    };
    Ok(exit_code(status))
}

fn exit_code(status: std::process::ExitStatus) -> u8 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return u8::try_from(128 + signal).unwrap_or(1);
        }
    }
    status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(1)
}

fn command_text(argv: &[OsString]) -> String {
    argv.iter()
        .map(|word| word.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The git top level of the current folder, else the folder itself.
fn worktree() -> String {
    let top = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|top| !top.is_empty());
    top.unwrap_or_else(|| {
        std::env::current_dir()
            .map(|dir| super::slashed(&dir))
            .unwrap_or_default()
    })
}

/// Local time as `%Y-%m-%d %H:%M:%S`, else UTC in the same shape.
pub(crate) fn local_now() -> String {
    headwork_store::local_time_now().unwrap_or_else(|| {
        let utc = headwork_core::update::format_utc(gate::epoch_now().saturating_mul(1000));
        utc.trim_end_matches('Z').replace('T', " ")
    })
}

fn actor_from_env() -> Actor {
    match std::env::var(headwork_adapters::runtime::ENV_ROLE)
        .ok()
        .and_then(|role| role.parse::<Role>().ok())
    {
        Some(Role::Orchestrator) => Actor::Orchestrator,
        Some(Role::Worker) => Actor::Worker,
        None => Actor::User,
    }
}

/// The store, only when `state.db` already exists: a gate never creates it.
fn existing_store(home: &Home) -> Option<Store> {
    let path = home.store_path();
    if !path.is_file() {
        return None;
    }
    Store::open(&path).ok()
}

fn task_name(home: &Home, id: i64) -> Option<String> {
    let store = existing_store(home)?;
    store
        .task(TaskId(id))
        .ok()
        .map(|task| task.name.to_string())
}

/// Writes the gate's events into an existing store; a failure is ignored, never the gate's.
struct Events<'a> {
    home: &'a Home,
    task: Option<TaskId>,
    actor: Actor,
    class: GateClass,
    command: &'a str,
}

impl Events<'_> {
    fn record(&self, kind: &str, extra: serde_json::Value) {
        let Some(store) = existing_store(self.home) else {
            return;
        };
        let mut data = serde_json::json!({ "class": self.class, "command": self.command });
        if let (Some(data), serde_json::Value::Object(extra)) = (data.as_object_mut(), extra) {
            data.extend(extra);
        }
        store
            .append_event(self.actor, self.task, None, kind, &data)
            .ok();
    }
}

/// `headwork gate status`: each class's count, holders and waiters.
#[derive(Debug, Clone, Serialize)]
pub struct GateStatus {
    /// The slot root, when one is known.
    pub dir: Option<String>,
    /// Each class.
    pub classes: Vec<ClassStatus>,
}

/// One class in [`GateStatus`].
#[derive(Debug, Clone, Serialize)]
pub struct ClassStatus {
    /// The class.
    pub class: GateClass,
    /// Its slot count.
    pub count: u32,
    /// Slots live holders take, legacy ones included.
    pub used: u32,
    /// The holders, the class's own first, then legacy ones.
    pub holders: Vec<HolderStatus>,
    /// The waiters, first come first.
    pub waiters: Vec<WaiterStatus>,
}

/// A holder in [`ClassStatus`].
#[derive(Debug, Clone, Serialize)]
pub struct HolderStatus {
    /// The slot number.
    pub slot: u32,
    /// Whether it is a legacy root slot of the old `heavy.sh`.
    pub legacy: bool,
    /// The holder's pid, when its owner file was read.
    pub pid: Option<u32>,
    /// The task, else empty.
    pub task: String,
    /// The worktree.
    pub worktree: String,
    /// The command line.
    pub command: String,
    /// When it started, as written.
    pub started: String,
    /// Seconds held.
    pub age_s: u64,
    /// Whether its holder is gone.
    pub stale: bool,
}

/// A waiter in [`ClassStatus`].
#[derive(Debug, Clone, Serialize)]
pub struct WaiterStatus {
    /// The waiter's pid.
    pub pid: u32,
    /// The task, else empty.
    pub task: String,
    /// The command line.
    pub command: String,
    /// Seconds waited.
    pub age_s: u64,
    /// Whether the waiter is gone.
    pub stale: bool,
}

impl ClassStatus {
    fn of(view: &ClassView) -> Self {
        let holders = view
            .holders
            .iter()
            .chain(&view.legacy)
            .map(|holder| {
                let owner = holder.owner.clone().unwrap_or_default();
                HolderStatus {
                    slot: holder.slot,
                    legacy: holder.legacy,
                    pid: holder.owner.as_ref().map(|owner| owner.pid),
                    task: owner.task,
                    worktree: owner.worktree,
                    command: owner.command,
                    started: owner.started,
                    age_s: holder.age_s,
                    stale: holder.stale,
                }
            })
            .collect();
        let waiters = view
            .waiters
            .iter()
            .map(|waiter| {
                let owner = waiter.owner.clone().unwrap_or_default();
                WaiterStatus {
                    pid: waiter.pid,
                    task: owner.task,
                    command: owner.command,
                    age_s: waiter.age_s,
                    stale: waiter.stale,
                }
            })
            .collect();
        Self {
            class: view.class,
            count: view.count.get(),
            used: view.used(),
            holders,
            waiters,
        }
    }
}

/// Reads every class's slots and waiters; reads only, never reaps.
#[must_use]
pub fn status(home: &Home) -> GateStatus {
    let gates = home
        .load_config()
        .map(|config| config.gates)
        .unwrap_or_default();
    let Some(dir) = gate_dir(home, &gates) else {
        return GateStatus {
            dir: None,
            classes: Vec::new(),
        };
    };
    let classes = Slots::new(dir.clone())
        .read_all(&gates)
        .iter()
        .map(ClassStatus::of)
        .collect();
    GateStatus {
        dir: Some(super::slashed(&dir)),
        classes,
    }
}

/// Writes [`GateStatus`] for people.
///
/// # Errors
///
/// Fails when writing fails.
pub fn write_status(status: &GateStatus, out: &mut impl Write) -> Result<()> {
    let Some(dir) = &status.dir else {
        writeln!(
            out,
            "gates: no slot folder (set [gates] dir or HEADWORK_GATE_DIR)"
        )?;
        return Ok(());
    };
    writeln!(out, "gates in {dir}")?;
    for class in &status.classes {
        writeln!(out, "{} {}/{}", class.class, class.used, class.count)?;
        for holder in &class.holders {
            let who = if holder.task.is_empty() {
                last_part(&holder.worktree)
            } else {
                holder.task.as_str()
            };
            let mut line = format!("  slot-{}", holder.slot);
            if holder.legacy {
                line.push_str(" (legacy)");
            }
            for part in [who, short_command(&holder.command).as_str()] {
                if !part.is_empty() {
                    line.push(' ');
                    line.push_str(part);
                }
            }
            line.push(' ');
            line.push_str(&held_for(holder.age_s));
            if let Some(pid) = holder.pid {
                write!(line, ", pid {pid}")?;
            }
            if holder.stale {
                line.push_str(", stale");
            }
            writeln!(out, "{line}")?;
        }
        for waiter in &class.waiters {
            let mut line = format!("  waiting: pid {}", waiter.pid);
            for part in [
                waiter.task.as_str(),
                short_command(&waiter.command).as_str(),
            ] {
                if !part.is_empty() {
                    line.push(' ');
                    line.push_str(part);
                }
            }
            line.push(' ');
            line.push_str(&held_for(waiter.age_s));
            if waiter.stale {
                line.push_str(", gone");
            }
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

fn last_part(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
}
