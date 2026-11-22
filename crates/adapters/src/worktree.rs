//! Task worktrees, made and removed by the project's configured command or by git itself (`builtin`).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use headwork_core::config::{TaskVars, TemplateProblem, WorktreeCommand};

/// Why a worktree command failed.
#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    /// The template names a placeholder a task has no value for.
    #[error("the worktree command template: {0}")]
    Template(#[from] TemplateProblem),
    /// The program did not start.
    #[error("running `{program}`: {source}")]
    Run {
        /// The program.
        program: String,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// The command ran longer than its timeout and was ended, with whatever it printed by then.
    #[error(
        "`{command}` timed out after {seconds} s and was ended (waiting on a prompt?): {output}"
    )]
    TimedOut {
        /// The command.
        command: String,
        /// The timeout.
        seconds: u64,
        /// What it printed.
        output: String,
    },
    /// The command exited with a failure.
    #[error("`{command}` failed: {output}")]
    Failed {
        /// The command.
        command: String,
        /// What it printed.
        output: String,
    },
}

/// What became of a task's branch once its worktree was removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchEnd {
    /// It held no commits beyond the main checkout's `HEAD` or its upstream, and was deleted.
    Deleted,
    /// There was no such branch, for instance because the remove command deleted it.
    Absent,
    /// It holds commits that neither the main checkout's `HEAD` nor its upstream has, so it stays.
    Kept,
}

/// How long one worktree command may run before it is ended: a script waiting on a prompt fails instead of hanging.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(300);

/// Runs worktree commands for one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktrees {
    shell: PathBuf,
    main: PathBuf,
    timeout: Duration,
}

impl Worktrees {
    /// Commands run in `main` with no input and at most [`COMMAND_TIMEOUT`] each; templates run through `shell -c`.
    #[must_use]
    pub fn new(shell: impl Into<PathBuf>, main: impl Into<PathBuf>) -> Self {
        Self {
            shell: shell.into(),
            main: main.into(),
            timeout: COMMAND_TIMEOUT,
        }
    }

    /// The same, with another timeout per command.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Makes the task's worktree: `git worktree add -b <branch> <worktree>` for `builtin`, else the template.
    ///
    /// A `builtin` worktree reuses a branch of that name left by an earlier task when it is empty (no commits beyond
    /// the main checkout's `HEAD` or its upstream): it is reset to `HEAD` with `-B`. A branch with commits of its own is never reset;
    /// git refuses to make the worktree then.
    ///
    /// # Errors
    ///
    /// Returns a [`WorktreeError`] when the template is bad or the command fails.
    pub fn create(
        &self,
        command: &WorktreeCommand,
        vars: &TaskVars<'_>,
    ) -> Result<(), WorktreeError> {
        match command {
            WorktreeCommand::Builtin => {
                let flag =
                    if self.branch_exists(vars.branch)? && self.branch_is_empty(vars.branch)? {
                        self.git(&["worktree", "prune"])?;
                        "-B"
                    } else {
                        "-b"
                    };
                self.git(&["worktree", "add", flag, vars.branch, vars.worktree])
            }
            WorktreeCommand::Command(template) => self.script(&vars.expand(template)?),
        }
    }

    /// Deletes `branch` when it holds no commits beyond the main checkout's `HEAD` or that branch's upstream, so its
    /// slug can be used again; a branch with work of its own is kept. A merged pull request lands on the remote first,
    /// and the main checkout follows only when someone pulls, so the upstream counts as much as `HEAD`.
    ///
    /// # Errors
    ///
    /// Returns a [`WorktreeError`] when git fails, for instance because the branch is still checked out somewhere.
    pub fn delete_empty_branch(&self, branch: &str) -> Result<BranchEnd, WorktreeError> {
        if !self.branch_exists(branch)? {
            return Ok(BranchEnd::Absent);
        }
        if !self.branch_is_empty(branch)? {
            return Ok(BranchEnd::Kept);
        }
        self.git(&["worktree", "prune"])?;
        self.git(&["branch", "-D", branch])?;
        Ok(BranchEnd::Deleted)
    }

    /// Whether the main checkout's repository has a local branch named `branch`.
    ///
    /// # Errors
    ///
    /// Returns a [`WorktreeError`] when git cannot run.
    pub fn branch_exists(&self, branch: &str) -> Result<bool, WorktreeError> {
        let reference = format!("refs/heads/{branch}");
        self.succeeds(&["rev-parse", "--verify", "--quiet", &reference])
    }

    fn branch_is_empty(&self, branch: &str) -> Result<bool, WorktreeError> {
        self.contains(branch)
    }

    /// Whether the main checkout's `HEAD`, or the upstream of the branch it has checked out, contains `revision`.
    fn contains(&self, revision: &str) -> Result<bool, WorktreeError> {
        if self.succeeds(&["merge-base", "--is-ancestor", revision, "HEAD"])? {
            return Ok(true);
        }
        self.succeeds(&["merge-base", "--is-ancestor", revision, "@{upstream}"])
    }

    /// Fetches the remote the main checkout's branch tracks, so a pull request merged since the last fetch counts as
    /// landed. Returns `false`, fetching nothing, when that branch tracks no remote. The fetch never prompts: ssh runs
    /// in batch mode with the user's own ssh program ([`crate::ssh::batch_command_in`]).
    ///
    /// # Errors
    ///
    /// Returns a [`WorktreeError`] when git fails or times out, for instance without network.
    pub fn fetch_upstream(&self) -> Result<bool, WorktreeError> {
        let head = self.output(
            Command::new("git").args(["symbolic-ref", "-q", "HEAD"]),
            "git symbolic-ref HEAD",
        );
        let Ok(head) = head else {
            return Ok(false);
        };
        let remote = self.output(
            Command::new("git").args([
                "for-each-ref",
                "--format=%(upstream:remotename)",
                head.trim(),
            ]),
            "git for-each-ref",
        )?;
        let remote = remote.trim();
        if remote.is_empty() {
            return Ok(false);
        }
        let shown = format!("git fetch --quiet {remote}");
        self.output(
            Command::new("git")
                .args(["fetch", "--quiet", remote])
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("GIT_SSH_COMMAND", crate::ssh::batch_command_in(&self.main)),
            &shown,
        )?;
        Ok(true)
    }

    fn succeeds(&self, args: &[&str]) -> Result<bool, WorktreeError> {
        let shown = format!("git {}", args.join(" "));
        match self.output(Command::new("git").args(args), &shown) {
            Ok(_) => Ok(true),
            Err(WorktreeError::Failed { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Removes the task's worktree: `git worktree remove <worktree>` for `builtin`, which refuses a dirty one, else
    /// the template.
    ///
    /// # Errors
    ///
    /// Returns a [`WorktreeError`] when the template is bad or the command fails.
    pub fn remove(
        &self,
        command: &WorktreeCommand,
        vars: &TaskVars<'_>,
    ) -> Result<(), WorktreeError> {
        match command {
            WorktreeCommand::Builtin => self.git(&["worktree", "remove", vars.worktree]),
            WorktreeCommand::Command(template) => self.script(&vars.expand(template)?),
        }
    }

    /// Removes the task's worktree with `git worktree remove`, without force, when it holds no work: its branch has no
    /// commits beyond the main checkout's `HEAD` or its upstream, and git refuses a worktree with changes or untracked files. Returns
    /// whether it was removed. This is the fallback for a remove script that keeps worktrees it cannot see merged,
    /// which a task that never started also is.
    ///
    /// # Errors
    ///
    /// Returns a [`WorktreeError`] when git cannot run or times out.
    pub fn remove_if_pristine(&self, vars: &TaskVars<'_>) -> Result<bool, WorktreeError> {
        if !self.branch_exists(vars.branch)? || !self.branch_is_empty(vars.branch)? {
            return Ok(false);
        }
        match self.git(&["worktree", "remove", vars.worktree]) {
            Ok(()) => Ok(true),
            Err(WorktreeError::Failed { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Removes the task's worktree even when it holds changes: `git worktree remove --force` for `builtin`, else the
    /// template, which decides for itself.
    ///
    /// # Errors
    ///
    /// Returns a [`WorktreeError`] when the template is bad or the command fails.
    pub fn remove_forced(
        &self,
        command: &WorktreeCommand,
        vars: &TaskVars<'_>,
    ) -> Result<(), WorktreeError> {
        match command {
            WorktreeCommand::Builtin => self.git(&["worktree", "remove", "--force", vars.worktree]),
            WorktreeCommand::Command(template) => self.script(&vars.expand(template)?),
        }
    }

    /// The worktrees git knows for the main checkout, with their branches.
    ///
    /// # Errors
    ///
    /// Returns a [`WorktreeError`] when git fails.
    pub fn list(&self) -> Result<Vec<(PathBuf, Option<String>)>, WorktreeError> {
        let output = self.output(
            Command::new("git").args(["worktree", "list", "--porcelain"]),
            "git worktree list",
        )?;
        Ok(parse_porcelain(&output))
    }

    /// The worktrees git knows for the main checkout, with their branches and locks, the main checkout first.
    ///
    /// # Errors
    ///
    /// Returns a [`WorktreeError`] when git fails.
    pub fn entries(&self) -> Result<Vec<WorktreeEntry>, WorktreeError> {
        let output = self.output(
            Command::new("git").args(["worktree", "list", "--porcelain"]),
            "git worktree list",
        )?;
        Ok(parse_entries(&output))
    }

    /// Whether the worktree at `path` holds uncommitted changes or untracked files.
    ///
    /// # Errors
    ///
    /// Returns a [`WorktreeError`] when git cannot read it.
    pub fn is_dirty(&self, path: &Path) -> Result<bool, WorktreeError> {
        let output = self.output(
            Command::new("git")
                .arg("-C")
                .arg(path)
                .args(["status", "--porcelain"]),
            "git status --porcelain",
        )?;
        Ok(!output.trim().is_empty())
    }

    /// Whether the main checkout's `HEAD`, or the upstream of its branch, contains `revision`.
    ///
    /// # Errors
    ///
    /// Returns a [`WorktreeError`] when git cannot run.
    pub fn is_merged(&self, revision: &str) -> Result<bool, WorktreeError> {
        self.contains(revision)
    }

    /// Whether every commit `branch` holds beyond the main checkout's `HEAD` has a commit of the same subject in
    /// `HEAD`, as a squash merge or a re-applied commit leaves it.
    ///
    /// # Errors
    ///
    /// Returns a [`WorktreeError`] when git fails.
    pub fn is_superseded(&self, branch: &str) -> Result<bool, WorktreeError> {
        let range = format!("HEAD..{branch}");
        let own = self.output(
            Command::new("git").args(["log", "--format=%s", &range]),
            "git log HEAD..<branch>",
        )?;
        let own: Vec<&str> = own.lines().filter(|line| !line.trim().is_empty()).collect();
        if own.is_empty() {
            return Ok(true);
        }
        let landed = self.output(
            Command::new("git").args(["log", "--format=%s", "HEAD"]),
            "git log HEAD",
        )?;
        let landed: std::collections::HashSet<&str> = landed.lines().collect();
        Ok(own.iter().all(|subject| landed.contains(subject)))
    }

    /// Whether `branch` tracks a remote branch that no longer exists.
    ///
    /// # Errors
    ///
    /// Returns a [`WorktreeError`] when git fails.
    pub fn upstream_gone(&self, branch: &str) -> Result<bool, WorktreeError> {
        let reference = format!("refs/heads/{branch}");
        let output = self.output(
            Command::new("git").args(["for-each-ref", "--format=%(upstream:track)", &reference]),
            "git for-each-ref",
        )?;
        Ok(output.trim() == "[gone]")
    }

    /// Unlocks the worktree at `path`.
    ///
    /// # Errors
    ///
    /// Returns a [`WorktreeError`] when git fails.
    pub fn unlock(&self, path: &str) -> Result<(), WorktreeError> {
        self.git(&["worktree", "unlock", path])
    }

    /// Removes the worktree at `path` without force: git refuses one with changes or untracked files.
    ///
    /// # Errors
    ///
    /// Returns a [`WorktreeError`] when git refuses or fails.
    pub fn remove_path(&self, path: &str) -> Result<(), WorktreeError> {
        self.git(&["worktree", "remove", path])
    }

    /// Forgets the worktrees whose folder is gone.
    ///
    /// # Errors
    ///
    /// Returns a [`WorktreeError`] when git fails.
    pub fn prune(&self) -> Result<(), WorktreeError> {
        self.git(&["worktree", "prune"])
    }

    /// Deletes `branch` when the main checkout's `HEAD` or its upstream contains it ([`Self::is_merged`]); refuses
    /// any other. `git branch -d` would refuse a branch only the upstream contains, so the check is made here.
    ///
    /// # Errors
    ///
    /// Returns a [`WorktreeError`] when the branch is not contained, or git refuses or fails.
    pub fn delete_merged_branch(&self, branch: &str) -> Result<(), WorktreeError> {
        if !self.contains(branch)? {
            return Err(WorktreeError::Failed {
                command: format!("git branch -D {branch}"),
                output: "neither the main checkout nor its upstream contains it".to_owned(),
            });
        }
        self.git(&["branch", "-D", branch])
    }

    /// Runs plain `git gc` in the main checkout, with git's default prune window, and with no timeout of its own.
    ///
    /// # Errors
    ///
    /// Returns a [`WorktreeError`] when git fails.
    pub fn gc(&self) -> Result<(), WorktreeError> {
        let shown = "git gc";
        let output = Command::new("git")
            .args(["gc", "--quiet"])
            .current_dir(&self.main)
            .stdin(Stdio::null())
            .output()
            .map_err(|source| WorktreeError::Run {
                program: "git".to_owned(),
                source,
            })?;
        if output.status.success() {
            return Ok(());
        }
        Err(WorktreeError::Failed {
            command: shown.to_owned(),
            output: format!(
                "{}{}",
                String::from_utf8_lossy(&output.stderr).trim(),
                String::from_utf8_lossy(&output.stdout).trim()
            ),
        })
    }

    /// Whether git counts `path` as a worktree of the main checkout, after forgetting the ones whose folder is gone.
    ///
    /// # Errors
    ///
    /// Returns a [`WorktreeError`] when git fails.
    pub fn is_registered(&self, path: &Path) -> Result<bool, WorktreeError> {
        self.git(&["worktree", "prune"])?;
        Ok(self
            .list()?
            .iter()
            .any(|(listed, _)| same_path(listed, path)))
    }

    fn git(&self, args: &[&str]) -> Result<(), WorktreeError> {
        let shown = format!("git {}", args.join(" "));
        self.output(Command::new("git").args(args), &shown)
            .map(drop)
    }

    fn script(&self, line: &str) -> Result<(), WorktreeError> {
        self.output(Command::new(&self.shell).args(["-c", line]), line)
            .map(drop)
    }

    fn output(&self, command: &mut Command, shown: &str) -> Result<String, WorktreeError> {
        let program = command.get_program().to_string_lossy().into_owned();
        let run_error = |source| WorktreeError::Run {
            program: program.clone(),
            source,
        };
        let mut child = command
            .current_dir(&self.main)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(run_error)?;
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().map_err(run_error)? {
                break Some(status);
            }
            if started.elapsed() >= self.timeout {
                drop(crate::proc::kill_tree(child.id()));
                drop(child.kill());
                drop(child.wait());
                break None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let grace = Duration::from_secs(2);
        let stdout = stdout.recv_timeout(grace).unwrap_or_default();
        let stderr = stderr.recv_timeout(grace).unwrap_or_default();
        let printed = format!("{}{}", stderr.trim(), stdout.trim());
        match status {
            Some(status) if status.success() => Ok(stdout),
            Some(_) => Err(WorktreeError::Failed {
                command: shown.to_owned(),
                output: printed,
            }),
            None => Err(WorktreeError::TimedOut {
                command: shown.to_owned(),
                seconds: self.timeout.as_secs(),
                output: printed,
            }),
        }
    }
}

/// Reads a child's pipe to its end on a thread of its own, so a full pipe never blocks the child, and hands the text
/// over when the pipe closes.
fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<String> {
    let (send, receive) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            drop(pipe.read_to_end(&mut bytes));
            drop(send.send(String::from_utf8_lossy(&bytes).into_owned()));
        });
    }
    receive
}

/// Parses `git worktree list --porcelain` into paths and branch names.
#[must_use]
pub fn parse_porcelain(text: &str) -> Vec<(PathBuf, Option<String>)> {
    let mut out = Vec::new();
    let mut current: Option<(PathBuf, Option<String>)> = None;
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            out.extend(current.take());
            current = Some((PathBuf::from(path), None));
        } else if let (Some(branch), Some((_, slot))) =
            (line.strip_prefix("branch "), current.as_mut())
        {
            *slot = Some(branch.trim_start_matches("refs/heads/").to_owned());
        }
    }
    out.extend(current);
    out
}

/// One worktree as `git worktree list --porcelain` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    /// The folder.
    pub path: PathBuf,
    /// The branch checked out; absent when detached.
    pub branch: Option<String>,
    /// The commit checked out.
    pub head: Option<String>,
    /// The lock's reason, when locked (empty for a lock without one).
    pub locked: Option<String>,
    /// Git found its folder gone.
    pub prunable: bool,
}

/// Parses `git worktree list --porcelain` into entries.
#[must_use]
pub fn parse_entries(text: &str) -> Vec<WorktreeEntry> {
    let mut out = Vec::new();
    let mut current: Option<WorktreeEntry> = None;
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            out.extend(current.take());
            current = Some(WorktreeEntry {
                path: PathBuf::from(path),
                branch: None,
                head: None,
                locked: None,
                prunable: false,
            });
            continue;
        }
        let Some(entry) = current.as_mut() else {
            continue;
        };
        if let Some(branch) = line.strip_prefix("branch ") {
            entry.branch = Some(branch.trim_start_matches("refs/heads/").to_owned());
        } else if let Some(head) = line.strip_prefix("HEAD ") {
            entry.head = Some(head.to_owned());
        } else if line == "locked" {
            entry.locked = Some(String::new());
        } else if let Some(reason) = line.strip_prefix("locked ") {
            entry.locked = Some(reason.to_owned());
        } else if line == "prunable" || line.starts_with("prunable ") {
            entry.prunable = true;
        }
    }
    out.extend(current);
    out
}

/// Whether two paths name the same folder, ignoring case and separators, as Windows does.
///
/// When both exist they are compared in canonical form, so a short `RUNNER~1` name matches its long name.
#[must_use]
pub fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let text = p.to_string_lossy().replace('\\', "/");
        text.trim_start_matches("//?/")
            .trim_end_matches('/')
            .to_lowercase()
    };
    if norm(a) == norm(b) {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => norm(&a) == norm(&b),
        _ => false,
    }
}
