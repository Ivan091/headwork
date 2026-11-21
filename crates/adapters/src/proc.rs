//! Process trees: start a program so that the tool owns everything it starts, ask whether a process lives, end a
//! whole tree.
//!
//! On Windows a child runs inside a Job Object with kill-on-close, so closing the job, or the death of the process
//! holding it, ends every descendant. On unix the child leads its own process group, and ending the group ends the
//! tree.

use std::io;
use std::process::{Child, Command, ExitStatus};

pub use headwork_core::process::{Liveness, ProcessIdentity};

/// Why a process operation failed.
#[derive(Debug, thiserror::Error)]
pub enum ProcError {
    /// The program did not start.
    #[error("starting `{program}`: {source}")]
    Spawn {
        /// The program.
        program: String,
        /// The OS error.
        #[source]
        source: io::Error,
    },
    /// Waiting for the child failed.
    #[error("waiting for process {pid}: {source}")]
    Wait {
        /// The child's process id.
        pid: u32,
        /// The OS error.
        #[source]
        source: io::Error,
    },
    /// A system tool such as `taskkill` or `kill` did not run.
    #[error("running `{tool}`: {source}")]
    Tool {
        /// The tool.
        tool: &'static str,
        /// The OS error.
        #[source]
        source: io::Error,
    },
    /// The interrupt handler could not be installed.
    #[error("ignoring Ctrl+C: {0}")]
    Interrupts(String),
}

/// How the tool holds a child's tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ownership {
    /// A Windows Job Object with kill-on-close.
    Job,
    /// A unix process group led by the child.
    ProcessGroup,
    /// Not owned; the reason says why. The child still runs.
    Unowned(String),
}

/// A child process whose whole tree the tool owns.
#[derive(Debug)]
pub struct OwnedChild {
    child: Child,
    tree: imp::Tree,
    ownership: Ownership,
}

impl OwnedChild {
    /// Starts `command` inside a new tree it cannot leave.
    ///
    /// The tree is joined right after the start, so a grandchild started in the first instant may escape it; `claude`
    /// takes far longer than that before it starts anything.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError::Spawn`] when the program does not start. Failing to own the tree is not an error; it is
    /// reported by [`OwnedChild::ownership`].
    pub fn spawn(command: &mut Command) -> Result<Self, ProcError> {
        imp::prepare(command);
        let child = command.spawn().map_err(|source| ProcError::Spawn {
            program: command.get_program().to_string_lossy().into_owned(),
            source,
        })?;
        let (tree, ownership) = imp::adopt(&child);
        Ok(Self {
            child,
            tree,
            ownership,
        })
    }

    /// The child's process id.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// How the tree is held.
    #[must_use]
    pub const fn ownership(&self) -> &Ownership {
        &self.ownership
    }

    /// Waits for the child to exit, then ends whatever is left of its tree.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError::Wait`] when waiting fails; the tree is ended anyway.
    pub fn wait(mut self) -> Result<ExitStatus, ProcError> {
        let pid = self.child.id();
        let status = self
            .child
            .wait()
            .map_err(|source| ProcError::Wait { pid, source });
        imp::end(self.tree);
        status
    }
}

/// Starts `command` outside every job, so it outlives the pane and the tree that started it; returns its pid.
///
/// Nothing waits for it. On Windows it gets a hidden console of its own (`CREATE_NO_WINDOW`), so the console programs
/// it runs (`tasklist`, `wmux`, `git`) open no window, which they would without any console (`DETACHED_PROCESS`).
///
/// On Windows it asks to leave the enclosing jobs; when a job forbids that it starts inside them, and the reason is
/// returned beside the pid.
///
/// # Errors
///
/// Returns [`ProcError::Spawn`] when the program does not start.
pub fn spawn_detached(command: &mut Command) -> Result<(u32, Option<String>), ProcError> {
    imp::spawn_detached(
        command,
        std::process::Stdio::null(),
        std::process::Stdio::null(),
    )
}

/// Like [`spawn_detached`], with the program's output going to `stdout` and `stderr`, such as a log file.
///
/// # Errors
///
/// Returns [`ProcError::Spawn`] when the program does not start.
pub fn spawn_detached_to(
    command: &mut Command,
    stdout: std::process::Stdio,
    stderr: std::process::Stdio,
) -> Result<(u32, Option<String>), ProcError> {
    imp::spawn_detached(command, stdout, stderr)
}

/// Ends the process `pid` and all its descendants.
///
/// Returns `false` when no such process was running.
///
/// # Errors
///
/// Returns [`ProcError::Tool`] when the system tool cannot run.
pub fn kill_tree(pid: u32) -> Result<bool, ProcError> {
    imp::kill_tree(pid)
}

/// Whether a process with this id runs.
///
/// # Errors
///
/// Returns [`ProcError::Tool`] when the system tool cannot run.
pub fn is_alive(pid: u32) -> Result<bool, ProcError> {
    imp::is_alive(pid)
}

/// The image name and start time of the process with this id, or `None` when none runs.
#[must_use]
pub fn identify(pid: u32) -> Option<ProcessIdentity> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    system.process(pid).map(|process| ProcessIdentity {
        image: process.name().to_string_lossy().into_owned(),
        started: process.start_time(),
    })
}

/// What runs under a recorded pid now, judged against the identity recorded with it (see [`headwork_core::process`]).
#[must_use]
pub fn liveness(pid: u32, recorded: Option<&ProcessIdentity>, argv: &[String]) -> Liveness {
    headwork_core::process::judge(recorded, argv, identify(pid))
}

/// Every running process as `headwork reap` judges them, and whether Git Bash's process list was read.
///
/// A Git Bash process gets its Git Bash parent (see [`git_bash_parents`]); every other process, and a Git Bash
/// process whose Git Bash parent is not a Git Bash process, keeps the parent the system recorded. On unix each process
/// carries its working folder, by which `reap` knows an adopted orphan's task.
#[must_use]
pub fn snapshot() -> (Vec<headwork_core::reap::Proc>, bool) {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let shells = git_bash_parents();
    let mut system = System::new();
    let kind = ProcessRefreshKind::nothing().with_cpu();
    let kind = if cfg!(unix) {
        kind.with_cwd(UpdateKind::Always)
    } else {
        kind
    };
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    let procs = system
        .processes()
        .values()
        .map(|process| {
            let pid = process.pid().as_u32();
            let recorded = process.parent().map(sysinfo::Pid::as_u32);
            headwork_core::reap::Proc {
                pid,
                parent: shells
                    .as_ref()
                    .and_then(|shells| shells.get(&pid).copied().flatten())
                    .or(recorded),
                image: process.name().to_string_lossy().into_owned(),
                started: process.start_time(),
                cpu_ms: process.accumulated_cpu_time(),
                cwd: process.cwd().map(|cwd| cwd.to_string_lossy().into_owned()),
            }
        })
        .collect();
    (procs, shells.is_some())
}

/// Where Git Bash's `ps` may be when it is not on `PATH`, as for a scheduled task.
const GIT_BASH_PS: [&str; 3] = [
    "ps",
    "C:/Program Files/Git/usr/bin/ps.exe",
    "C:/Program Files (x86)/Git/usr/bin/ps.exe",
];

/// Git Bash's own parent links, by Windows pid; `None` when Git Bash's `ps` cannot be run.
///
/// Each Git Bash process maps to the Windows pid of its Git Bash parent, or to `None` when that parent is not a Git
/// Bash process (it started from a Windows program, or its parent ended).
///
/// Git Bash's `fork` and `exec` start a shell from a Windows process that exits at once, so the parent Windows
/// records is gone while the shell's real parent still runs.
///
/// Elsewhere the system's parents are right, and the map is empty.
#[must_use]
pub fn git_bash_parents() -> Option<std::collections::HashMap<u32, Option<u32>>> {
    if !cfg!(windows) {
        return Some(std::collections::HashMap::new());
    }
    GIT_BASH_PS.iter().find_map(|program| {
        let output = Command::new(program).arg("-el").output().ok()?;
        output
            .status
            .success()
            .then(|| parse_git_bash_ps(&String::from_utf8_lossy(&output.stdout)))
    })
}

/// Reads `ps -el` of Git Bash: `PID PPID PGID WINPID TTY UID STIME COMMAND`, a line perhaps led by a status letter.
#[must_use]
pub fn parse_git_bash_ps(text: &str) -> std::collections::HashMap<u32, Option<u32>> {
    let rows: Vec<(u32, u32, u32)> = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace().peekable();
            if fields.peek()?.parse::<u32>().is_err() {
                fields.next();
            }
            let own = fields.next()?.parse().ok()?;
            let parent = fields.next()?.parse().ok()?;
            let _group: u32 = fields.next()?.parse().ok()?;
            let windows = fields.next()?.parse().ok()?;
            Some((own, parent, windows))
        })
        .collect();
    let windows: std::collections::HashMap<u32, u32> = rows
        .iter()
        .map(|&(own, _, windows)| (own, windows))
        .collect();
    rows.iter()
        .map(|&(_, parent, own)| (own, windows.get(&parent).copied()))
        .collect()
}

/// Ends the one process `pid`, not its descendants, and only while it is still the process `expected` names; a pid
/// another program took since is left alone.
///
/// Returns `false` when it no longer ran, or ran as another program.
///
/// # Errors
///
/// Returns [`ProcError::Tool`] when the system tool cannot run.
pub fn kill_one(pid: u32, expected: &ProcessIdentity) -> Result<bool, ProcError> {
    if !headwork_core::process::judge(Some(expected), &[], identify(pid)).is_alive() {
        return Ok(false);
    }
    imp::kill_one(pid)
}

/// The ids of the processes running a program image, such as `claude.exe` on Windows or `claude` elsewhere.
///
/// # Errors
///
/// Returns [`ProcError::Tool`] when the system tool cannot run.
pub fn pids_named(image: &str) -> Result<Vec<u32>, ProcError> {
    imp::pids_named(image)
}

/// Makes this process ignore Ctrl+C and Ctrl+Break, so that an interrupt typed into a pane reaches the program in it
/// and does not end the launcher that owns the program's tree.
///
/// # Errors
///
/// Returns [`ProcError::Interrupts`] when the handler cannot be installed.
pub fn ignore_interrupts() -> Result<(), ProcError> {
    ctrlc::set_handler(|| {}).map_err(|error| ProcError::Interrupts(error.to_string()))
}

#[cfg(windows)]
mod imp {
    use std::os::windows::io::AsRawHandle;
    use std::process::{Child, Command};

    use win32job::{ExtendedLimitInfo, Job};

    use super::{Ownership, ProcError};

    /// The job holding the tree; closing it ends every process in it.
    pub(super) type Tree = Option<Job>;

    /// Taskkill's exit code when no process matched.
    const TASKKILL_NOT_FOUND: i32 = 128;

    pub(super) const fn prepare(_command: &mut Command) {}

    pub(super) fn adopt(child: &Child) -> (Tree, Ownership) {
        match job_for(child) {
            Ok(job) => (Some(job), Ownership::Job),
            Err(reason) => (None, Ownership::Unowned(reason)),
        }
    }

    fn job_for(child: &Child) -> Result<Job, String> {
        let mut info = ExtendedLimitInfo::new();
        info.limit_kill_on_job_close();
        let job = Job::create_with_limit_info(&info).map_err(|error| error.to_string())?;
        let handle = isize::try_from(child.as_raw_handle().addr())
            .map_err(|_| "the process handle does not fit".to_owned())?;
        job.assign_process(handle)
            .map_err(|error| format!("assigning the job: {}", std::io::Error::from(error)))?;
        Ok(job)
    }

    pub(super) fn end(tree: Tree) {
        drop(tree);
    }

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

    pub(super) fn spawn_detached(
        command: &mut Command,
        stdout: std::process::Stdio,
        stderr: std::process::Stdio,
    ) -> Result<(u32, Option<String>), ProcError> {
        use std::os::windows::process::CommandExt;
        let program = command.get_program().to_string_lossy().into_owned();
        command
            .stdin(std::process::Stdio::null())
            .stdout(stdout)
            .stderr(stderr);
        command.creation_flags(
            CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB,
        );
        match command.spawn() {
            Ok(child) => Ok((child.id(), None)),
            Err(breakaway) => {
                command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
                let child = command
                    .spawn()
                    .map_err(|source| ProcError::Spawn { program, source })?;
                Ok((
                    child.id(),
                    Some(format!("could not leave the job: {breakaway}")),
                ))
            }
        }
    }

    pub(super) fn kill_tree(pid: u32) -> Result<bool, ProcError> {
        let status = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output()
            .map_err(|source| ProcError::Tool {
                tool: "taskkill",
                source,
            })?
            .status;
        Ok(status.code() != Some(TASKKILL_NOT_FOUND) && status.success())
    }

    pub(super) fn kill_one(pid: u32) -> Result<bool, ProcError> {
        let status = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output()
            .map_err(|source| ProcError::Tool {
                tool: "taskkill",
                source,
            })?
            .status;
        Ok(status.code() != Some(TASKKILL_NOT_FOUND) && status.success())
    }

    pub(super) fn is_alive(pid: u32) -> Result<bool, ProcError> {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .map_err(|source| ProcError::Tool {
                tool: "tasklist",
                source,
            })?;
        let needle = format!("\"{pid}\"");
        Ok(String::from_utf8_lossy(&output.stdout).contains(&needle))
    }

    pub(super) fn pids_named(image: &str) -> Result<Vec<u32>, ProcError> {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("IMAGENAME eq {image}"), "/NH", "/FO", "CSV"])
            .output()
            .map_err(|source| ProcError::Tool {
                tool: "tasklist",
                source,
            })?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.split(',').nth(1))
            .filter_map(|field| field.trim_matches('"').parse().ok())
            .collect())
    }
}

#[cfg(unix)]
mod imp {
    use std::io::IsTerminal;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};

    use rustix::io::Errno;
    use rustix::process::{Pid, Signal};

    use super::{Ownership, ProcError};

    /// The process group id, which is the child's pid.
    pub(super) type Tree = u32;

    pub(super) fn prepare(command: &mut Command) {
        command.process_group(0);
    }

    /// Takes the child's group as the tree and, when this process holds the terminal, hands the terminal to that
    /// group: a group that is not the terminal's foreground group is stopped (`SIGTTIN`) when it reads its input, as
    /// `claude` does at once. The group is then continued, in case it read before the handover.
    pub(super) fn adopt(child: &Child) -> (Tree, Ownership) {
        let group = child.id();
        if let Some(pid) = pid_of(group) {
            hand_terminal(pid);
        }
        (group, Ownership::ProcessGroup)
    }

    fn hand_terminal(group: Pid) {
        let stdin = std::io::stdin();
        if !stdin.is_terminal() {
            return;
        }
        let ours = rustix::process::getpgrp();
        if rustix::termios::tcgetpgrp(&stdin).ok() != Some(ours) {
            return;
        }
        if rustix::termios::tcsetpgrp(&stdin, group).is_ok() {
            rustix::process::kill_process_group(group, Signal::CONT).ok();
        }
    }

    fn pid_of(pid: u32) -> Option<Pid> {
        i32::try_from(pid).ok().and_then(Pid::from_raw)
    }

    fn tool(error: Errno) -> ProcError {
        ProcError::Tool {
            tool: "kill",
            source: error.into(),
        }
    }

    pub(super) fn spawn_detached(
        command: &mut Command,
        stdout: std::process::Stdio,
        stderr: std::process::Stdio,
    ) -> Result<(u32, Option<String>), ProcError> {
        let program = command.get_program().to_string_lossy().into_owned();
        command
            .stdin(std::process::Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .process_group(0);
        let child = command
            .spawn()
            .map_err(|source| ProcError::Spawn { program, source })?;
        Ok((child.id(), None))
    }

    pub(super) fn end(group: Tree) {
        if let Some(group) = pid_of(group) {
            rustix::process::kill_process_group(group, Signal::KILL).ok();
        }
    }

    /// The process and its descendants, by the parent links the system reports.
    fn descendants(pid: u32) -> Vec<u32> {
        let mut found = vec![pid];
        let links: Vec<(u32, u32)> = super::snapshot()
            .0
            .into_iter()
            .filter_map(|process| Some((process.pid, process.parent?)))
            .collect();
        let mut next = 0;
        while let Some(&parent) = found.get(next) {
            for &(child, of) in &links {
                if of == parent && !found.contains(&child) {
                    found.push(child);
                }
            }
            next += 1;
        }
        found
    }

    /// Ends the process, its group when it leads one, and every descendant: a child that was not started in a group
    /// of its own (a plain `Command::spawn`) is still ended with its tree.
    pub(super) fn kill_tree(pid: u32) -> Result<bool, ProcError> {
        if !is_alive(pid)? {
            return Ok(false);
        }
        let Some(target) = pid_of(pid) else {
            return Ok(false);
        };
        let tree = descendants(pid);
        if rustix::process::getpgid(Some(target)).ok() == Some(target) {
            rustix::process::kill_process_group(target, Signal::KILL).ok();
        }
        for member in tree.into_iter().filter_map(pid_of) {
            match rustix::process::kill_process(member, Signal::KILL) {
                Ok(()) | Err(Errno::SRCH) => {}
                Err(error) => return Err(tool(error)),
            }
        }
        Ok(true)
    }

    pub(super) fn kill_one(pid: u32) -> Result<bool, ProcError> {
        let Some(target) = pid_of(pid) else {
            return Ok(false);
        };
        match rustix::process::kill_process(target, Signal::KILL) {
            Ok(()) => Ok(true),
            Err(Errno::SRCH) => Ok(false),
            Err(error) => Err(tool(error)),
        }
    }

    pub(super) fn pids_named(image: &str) -> Result<Vec<u32>, ProcError> {
        let output = Command::new("pgrep")
            .args(["-x", image])
            .output()
            .map_err(|source| ProcError::Tool {
                tool: "pgrep",
                source,
            })?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect())
    }

    /// Whether the process runs. A zombie, which ended but was not yet waited for (a detached child no one waits
    /// on), does not.
    pub(super) fn is_alive(pid: u32) -> Result<bool, ProcError> {
        let Some(target) = pid_of(pid) else {
            return Ok(false);
        };
        match rustix::process::test_kill_process(target) {
            Ok(()) => Ok(!is_zombie(pid)),
            Err(Errno::SRCH) => Ok(false),
            Err(Errno::PERM) => Ok(true),
            Err(error) => Err(tool(error)),
        }
    }

    fn is_zombie(pid: u32) -> bool {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};
        let pid = Pid::from_u32(pid);
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        system
            .process(pid)
            .is_none_or(|process| process.status() == ProcessStatus::Zombie)
    }
}
