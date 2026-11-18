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
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};

    use super::{Ownership, ProcError};

    /// The process group id, which is the child's pid.
    pub(super) type Tree = u32;

    pub(super) fn prepare(command: &mut Command) {
        command.process_group(0);
    }

    pub(super) fn adopt(child: &Child) -> (Tree, Ownership) {
        (child.id(), Ownership::ProcessGroup)
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
        let ended = signal_group(group, "-KILL");
        drop(ended);
    }

    fn signal_group(group: u32, signal: &str) -> Result<bool, ProcError> {
        Command::new("kill")
            .args([signal, "--", &format!("-{group}")])
            .output()
            .map(|output| output.status.success())
            .map_err(|source| ProcError::Tool {
                tool: "kill",
                source,
            })
    }

    pub(super) fn kill_tree(pid: u32) -> Result<bool, ProcError> {
        signal_group(pid, "-KILL")
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

    pub(super) fn is_alive(pid: u32) -> Result<bool, ProcError> {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|output| output.status.success())
            .map_err(|source| ProcError::Tool {
                tool: "kill",
                source,
            })
    }
}
