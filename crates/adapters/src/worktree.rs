//! Task worktrees, made and removed by the project's configured command or by git itself (`builtin`).

use std::path::{Path, PathBuf};
use std::process::Command;

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
    /// The command exited with a failure.
    #[error("`{command}` failed: {output}")]
    Failed {
        /// The command.
        command: String,
        /// What it printed.
        output: String,
    },
}

/// Runs worktree commands for one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktrees {
    shell: PathBuf,
    main: PathBuf,
}

impl Worktrees {
    /// Commands run in `main`; templates run through `shell -c`.
    #[must_use]
    pub fn new(shell: impl Into<PathBuf>, main: impl Into<PathBuf>) -> Self {
        Self {
            shell: shell.into(),
            main: main.into(),
        }
    }

    /// Makes the task's worktree: `git worktree add -b <branch> <worktree>` for `builtin`, else the template.
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
                self.git(&["worktree", "add", "-b", vars.branch, vars.worktree])
            }
            WorktreeCommand::Command(template) => self.script(&vars.expand(template)?),
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
        let output =
            command
                .current_dir(&self.main)
                .output()
                .map_err(|source| WorktreeError::Run {
                    program: command.get_program().to_string_lossy().into_owned(),
                    source,
                })?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if output.status.success() {
            Ok(stdout)
        } else {
            Err(WorktreeError::Failed {
                command: shown.to_owned(),
                output: format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stderr).trim(),
                    stdout.trim()
                ),
            })
        }
    }
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
