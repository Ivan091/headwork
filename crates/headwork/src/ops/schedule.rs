//! `tick --install` and `tick --uninstall`: the one Windows scheduled task that runs `headwork tick`. Elsewhere they
//! refuse for now ([`Platform::no_scheduler`]).
//!
//! The task is fixed: named [`TASK_NAME`], every [`EVERY_MINUTES`] minutes, for the current user only while they are
//! logged on, so creating it needs no admin rights. It runs the installed `headwork` (`~/.local/bin`), which hot reload
//! replaces in place, so a new build needs no new task. Sessions run these only after the user's `Allow` tap.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use headwork_core::Platform;
use serde::Serialize;

/// The scheduled task's name.
pub const TASK_NAME: &str = "headwork tick";

/// How often the task runs, in minutes.
pub const EVERY_MINUTES: u32 = 5;

/// What `tick --install` or `--uninstall` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    /// The task was created.
    Created,
    /// The task existed and was replaced.
    Replaced,
    /// The task was removed.
    Removed,
    /// There was no task to remove.
    Absent,
}

/// What a scheduler change reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScheduleReport {
    /// The task's name.
    pub task: &'static str,
    /// What happened.
    pub change: Change,
    /// The command the task runs, when installed.
    pub command: Option<String>,
}

/// Runs `schtasks`, or a fake of it in tests.
pub trait Scheduler {
    /// Runs `schtasks` with `args`; returns whether it succeeded and what it printed.
    ///
    /// # Errors
    ///
    /// Fails when `schtasks` cannot start.
    fn schtasks(&self, args: &[String]) -> Result<(bool, String)>;
}

/// The system's `schtasks`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Schtasks;

impl Scheduler for Schtasks {
    fn schtasks(&self, args: &[String]) -> Result<(bool, String)> {
        if let Some(reason) = Platform::current().no_scheduler() {
            bail!("{reason}");
        }
        let output = Command::new("schtasks")
            .args(args)
            .output()
            .context("running `schtasks`")?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        Ok((output.status.success(), text.trim().to_owned()))
    }
}

/// The program the task runs: the installed `headwork` when there is one, else this program.
#[must_use]
pub fn program(installed: &Path, current: &Path) -> PathBuf {
    if installed.is_file() {
        installed.to_path_buf()
    } else {
        current.to_path_buf()
    }
}

/// The command line the task runs: the program, quoted, with Windows separators, then `tick`.
#[must_use]
pub fn task_command(exe: &Path) -> String {
    format!("\"{}\" tick", exe.display().to_string().replace('/', "\\"))
}

/// The `schtasks` arguments that create or replace the task.
#[must_use]
pub fn create_args(exe: &Path) -> Vec<String> {
    [
        "/Create",
        "/SC",
        "MINUTE",
        "/MO",
        &EVERY_MINUTES.to_string(),
        "/TN",
        TASK_NAME,
        "/TR",
        &task_command(exe),
        "/F",
    ]
    .map(str::to_owned)
    .to_vec()
}

fn query_args() -> Vec<String> {
    ["/Query", "/TN", TASK_NAME].map(str::to_owned).to_vec()
}

fn delete_args() -> Vec<String> {
    ["/Delete", "/TN", TASK_NAME, "/F"]
        .map(str::to_owned)
        .to_vec()
}

/// Creates the task, or replaces it when it exists.
///
/// # Errors
///
/// Fails when `schtasks` cannot run or refuses.
pub fn install(scheduler: &dyn Scheduler, exe: &Path) -> Result<ScheduleReport> {
    let (existed, _) = scheduler.schtasks(&query_args())?;
    let (created, text) = scheduler.schtasks(&create_args(exe))?;
    if !created {
        bail!("`schtasks /Create` refused: {text}");
    }
    Ok(ScheduleReport {
        task: TASK_NAME,
        change: if existed {
            Change::Replaced
        } else {
            Change::Created
        },
        command: Some(task_command(exe)),
    })
}

/// Removes the task; nothing to do when it does not exist.
///
/// # Errors
///
/// Fails when `schtasks` cannot run or refuses.
pub fn uninstall(scheduler: &dyn Scheduler) -> Result<ScheduleReport> {
    let (exists, _) = scheduler.schtasks(&query_args())?;
    if !exists {
        return Ok(ScheduleReport {
            task: TASK_NAME,
            change: Change::Absent,
            command: None,
        });
    }
    let (removed, text) = scheduler.schtasks(&delete_args())?;
    if !removed {
        bail!("`schtasks /Delete` refused: {text}");
    }
    Ok(ScheduleReport {
        task: TASK_NAME,
        change: Change::Removed,
        command: None,
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "tests")]

    use std::cell::RefCell;

    use super::*;

    struct Fake {
        exists: bool,
        refuse: bool,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl Fake {
        fn new(exists: bool, refuse: bool) -> Self {
            Self {
                exists,
                refuse,
                calls: RefCell::new(Vec::new()),
            }
        }

        fn verbs(&self) -> Vec<String> {
            self.calls
                .borrow()
                .iter()
                .map(|call| call[0].clone())
                .collect()
        }
    }

    impl Scheduler for Fake {
        fn schtasks(&self, args: &[String]) -> Result<(bool, String)> {
            self.calls.borrow_mut().push(args.to_vec());
            Ok(match args[0].as_str() {
                "/Query" => (self.exists, String::new()),
                _ => (!self.refuse, "ERROR: Access is denied.".to_owned()),
            })
        }
    }

    #[test]
    fn the_task_runs_the_quoted_program_every_five_minutes() {
        let args = create_args(Path::new("C:/Users/me/.local/bin/headwork.exe"));
        assert_eq!(
            args,
            [
                "/Create",
                "/SC",
                "MINUTE",
                "/MO",
                "5",
                "/TN",
                "headwork tick",
                "/TR",
                "\"C:\\Users\\me\\.local\\bin\\headwork.exe\" tick",
                "/F",
            ],
            "no /RU: the current user, no admin"
        );
    }

    #[test]
    fn install_creates_or_replaces_and_reports_which() {
        let exe = Path::new("C:/h.exe");
        let fresh = Fake::new(false, false);
        assert_eq!(
            install(&fresh, exe).unwrap().change,
            Change::Created,
            "fresh"
        );
        assert_eq!(fresh.verbs(), ["/Query", "/Create"], "calls");
        let again = Fake::new(true, false);
        let report = install(&again, exe).unwrap();
        assert_eq!(report.change, Change::Replaced, "again");
        assert_eq!(
            report.command.as_deref(),
            Some("\"C:\\h.exe\" tick"),
            "command"
        );
        let refused = install(&Fake::new(false, true), exe).unwrap_err();
        assert!(
            refused.to_string().contains("Access is denied"),
            "{refused}"
        );
    }

    #[test]
    fn uninstall_removes_only_an_existing_task() {
        let absent = Fake::new(false, false);
        assert_eq!(uninstall(&absent).unwrap().change, Change::Absent, "absent");
        assert_eq!(absent.verbs(), ["/Query"], "nothing deleted");
        let present = Fake::new(true, false);
        assert_eq!(
            uninstall(&present).unwrap().change,
            Change::Removed,
            "present"
        );
        assert_eq!(present.verbs(), ["/Query", "/Delete"], "deleted");
        assert!(uninstall(&Fake::new(true, true)).is_err(), "refused");
    }

    #[test]
    fn the_installed_program_wins_when_it_exists() {
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("headwork.exe");
        let current = Path::new("S:/build/headwork.exe");
        assert_eq!(program(&installed, current), current, "not installed");
        std::fs::write(&installed, b"").unwrap();
        assert_eq!(program(&installed, current), installed, "installed");
    }
}
