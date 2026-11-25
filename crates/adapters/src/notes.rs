//! A repository checkout read for its migration notes (`docs/migrations.md`).
//!
//! The notes are read from the default branch's remote ref with `git show`, never from the working tree, so a machine
//! sees a note before it pulls and whatever branch its checkout is on. The fetch never prompts: ssh runs in batch mode
//! ([`crate::ssh::batch_command_in`]).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::timed::{self, TimedError};

/// The folder of notes, at a repository's root.
pub const NOTES_DIR: &str = "migrations";

/// How long a git call may run before it is ended.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a note's command may run before it is ended.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(600);

/// Why a repository could not be read.
#[derive(Debug, thiserror::Error)]
pub enum NotesError {
    /// The program did not start or ran too long.
    #[error(transparent)]
    Timed(#[from] TimedError),
    /// A command exited with a failure.
    #[error("`{command}` failed: {output}")]
    Failed {
        /// The command.
        command: String,
        /// What it printed.
        output: String,
    },
    /// The repository has no remote default branch to read.
    #[error("{} has no origin/main or origin/master to read the notes from", .0.display())]
    NoDefaultBranch(PathBuf),
}

/// What a note's command printed and how it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRun {
    /// Whether it exited with 0.
    pub success: bool,
    /// Standard error and output, trimmed.
    pub output: String,
}

/// A checkout.
#[derive(Debug, Clone)]
pub struct NoteRepo {
    dir: PathBuf,
}

impl NoteRepo {
    /// The checkout at `dir`.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The checkout's folder.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether the folder is a git checkout.
    #[must_use]
    pub fn exists(&self) -> bool {
        self.dir.join(".git").exists()
    }

    /// Whether the checkout has a remote named `origin`; one without has no notes to read.
    #[must_use]
    pub fn has_origin(&self) -> bool {
        self.git(&["remote", "get-url", "origin"]).is_ok()
    }

    /// The remote ref of the default branch, such as `origin/master`: what `origin/HEAD` points at, else `origin/main`
    /// or `origin/master`, whichever exists.
    ///
    /// # Errors
    ///
    /// Fails when git cannot run or no such ref exists.
    pub fn default_ref(&self) -> Result<String, NotesError> {
        if let Ok(head) = self.git(&["symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"]) {
            let head = head.trim();
            if !head.is_empty() {
                return Ok(head.to_owned());
            }
        }
        for candidate in ["origin/main", "origin/master"] {
            if self
                .git(&["rev-parse", "--verify", "--quiet", candidate])
                .is_ok()
            {
                return Ok(candidate.to_owned());
            }
        }
        Err(NotesError::NoDefaultBranch(self.dir.clone()))
    }

    /// Fetches the default branch from `origin`.
    ///
    /// # Errors
    ///
    /// Fails when git fails or times out, for instance without network.
    pub fn fetch(&self) -> Result<(), NotesError> {
        let reference = self.default_ref()?;
        let branch = reference.strip_prefix("origin/").unwrap_or(&reference);
        let mut command = Command::new("git");
        command
            .args(["fetch", "--quiet", "origin", branch])
            .current_dir(&self.dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_SSH_COMMAND", crate::ssh::batch_command_in(&self.dir));
        let ran = timed::run(&mut command, GIT_TIMEOUT)?;
        if ran.success() {
            Ok(())
        } else {
            Err(NotesError::Failed {
                command: format!("git fetch origin {branch}"),
                output: ran.printed(),
            })
        }
    }

    /// The paths of the `*.toml` files in `migrations/` at `reference`, sorted.
    ///
    /// # Errors
    ///
    /// Fails when git fails.
    pub fn list(&self, reference: &str) -> Result<Vec<String>, NotesError> {
        let tree = format!("{NOTES_DIR}/");
        let listing = self.git(&["ls-tree", "--name-only", reference, &tree])?;
        let mut files: Vec<String> = listing
            .lines()
            .map(str::trim)
            .filter(|line| {
                Path::new(line)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
            })
            .map(str::to_owned)
            .collect();
        files.sort();
        Ok(files)
    }

    /// The text of `path` at `reference`.
    ///
    /// # Errors
    ///
    /// Fails when git fails.
    pub fn show(&self, reference: &str, path: &str) -> Result<String, NotesError> {
        self.git(&["show", &format!("{reference}:{path}")])
    }

    /// Whether `path` exists in the checkout's `HEAD`.
    #[must_use]
    pub fn in_head(&self, path: &str) -> bool {
        self.git(&["cat-file", "-e", &format!("HEAD:{path}")])
            .is_ok()
    }

    /// Whether the working tree has changes.
    ///
    /// # Errors
    ///
    /// Fails when git fails.
    pub fn dirty(&self) -> Result<bool, NotesError> {
        Ok(!self.git(&["status", "--porcelain"])?.trim().is_empty())
    }

    /// The checked-out branch's name; `HEAD` when detached.
    ///
    /// # Errors
    ///
    /// Fails when git fails.
    pub fn branch(&self) -> Result<String, NotesError> {
        Ok(self
            .git(&["rev-parse", "--abbrev-ref", "HEAD"])?
            .trim()
            .to_owned())
    }

    /// Runs `line` with `shell -c` in the checkout.
    ///
    /// # Errors
    ///
    /// Fails when the shell does not start or the command runs past [`STEP_TIMEOUT`].
    pub fn run_line(&self, shell: &Path, line: &str) -> Result<StepRun, NotesError> {
        let mut command = Command::new(shell);
        command.args(["-c", line]).current_dir(&self.dir);
        let ran = timed::run(&mut command, STEP_TIMEOUT)?;
        Ok(StepRun {
            success: ran.success(),
            output: ran.printed(),
        })
    }

    fn git(&self, args: &[&str]) -> Result<String, NotesError> {
        let mut command = Command::new("git");
        command.args(args).current_dir(&self.dir);
        let ran = timed::run(&mut command, GIT_TIMEOUT)?;
        if ran.success() {
            Ok(ran.stdout)
        } else {
            Err(NotesError::Failed {
                command: format!("git {}", args.join(" ")),
                output: ran.printed(),
            })
        }
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests fail loudly on a bad fixture")]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let done = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(done.status.success(), "git {args:?}: {done:?}");
    }

    fn fixture() -> (tempfile::TempDir, NoteRepo) {
        let root = tempfile::tempdir().unwrap();
        let origin = root.path().join("origin");
        std::fs::create_dir_all(origin.join("migrations")).unwrap();
        git(&origin, &["init", "-q", "-b", "main"]);
        std::fs::write(origin.join("migrations/a.toml"), "id = \"a\"\n").unwrap();
        std::fs::write(origin.join("migrations/readme.md"), "x").unwrap();
        git(&origin, &["add", "."]);
        git(&origin, &["commit", "-q", "-m", "first"]);
        let clone = root.path().join("clone");
        git(
            root.path(),
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        (root, NoteRepo::new(clone))
    }

    #[test]
    fn notes_are_listed_and_read_from_the_remote_ref() {
        let (_root, repo) = fixture();
        let reference = repo.default_ref().unwrap();
        assert_eq!(reference, "origin/main", "default ref");
        assert_eq!(
            repo.list(&reference).unwrap(),
            ["migrations/a.toml"],
            "only toml"
        );
        assert_eq!(
            repo.show(&reference, "migrations/a.toml").unwrap(),
            "id = \"a\"\n",
            "content"
        );
    }

    #[test]
    fn a_note_added_upstream_is_seen_after_the_fetch_and_before_the_pull() {
        let (root, repo) = fixture();
        let origin = root.path().join("origin");
        std::fs::write(origin.join("migrations/b.toml"), "id = \"b\"\n").unwrap();
        git(&origin, &["add", "."]);
        git(&origin, &["commit", "-q", "-m", "second"]);
        repo.fetch().unwrap();
        let reference = repo.default_ref().unwrap();
        assert_eq!(
            repo.list(&reference).unwrap(),
            ["migrations/a.toml", "migrations/b.toml"],
            "both notes"
        );
        assert!(repo.in_head("migrations/a.toml"), "a is in HEAD");
        assert!(!repo.in_head("migrations/b.toml"), "b is not pulled yet");
        assert!(!repo.dirty().unwrap(), "clean");
        assert_eq!(repo.branch().unwrap(), "main", "branch");
    }

    #[test]
    fn a_folder_without_origin_has_no_default_ref() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q", "-b", "main"]);
        let repo = NoteRepo::new(root.path());
        assert!(
            matches!(repo.default_ref(), Err(NotesError::NoDefaultBranch(_))),
            "no origin"
        );
    }
}
