//! The git host (design section 4.2): the pull request of a task's branch. [`Gh`] asks the `gh` CLI;
//! [`FakeGit`] answers from memory for tests.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

/// Why the git host could not answer.
#[derive(Debug, thiserror::Error)]
pub enum GitHostError {
    /// `gh` could not be run.
    #[error("running gh: {0}")]
    Run(#[source] std::io::Error),
    /// `gh` answered with a failure.
    #[error("`gh {command}` failed: {message}")]
    Failed {
        /// The arguments.
        command: String,
        /// What `gh` printed.
        message: String,
    },
    /// `gh`'s answer did not parse.
    #[error("`gh {command}` printed something unexpected: {source}")]
    Parse {
        /// The arguments.
        command: String,
        /// The parser's error.
        #[source]
        source: serde_json::Error,
    },
}

/// Where a pull request is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    /// Open.
    Open,
    /// Merged.
    Merged,
    /// Closed without a merge.
    Closed,
}

impl PrState {
    /// The spelling the store keeps.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Merged => "merged",
            Self::Closed => "closed",
        }
    }
}

/// A pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrInfo {
    /// `owner/name`.
    pub repo: String,
    /// The number.
    pub number: i64,
    /// The URL.
    pub url: String,
    /// Where it is.
    pub state: PrState,
}

/// A git host that knows pull requests.
pub trait GitHost: fmt::Debug {
    /// The newest pull request whose head is `branch`, looked up from the repository checked out in `dir`.
    ///
    /// # Errors
    ///
    /// Returns [`GitHostError`] when the host cannot answer; a branch without a pull request is `Ok(None)`.
    fn pr_for_branch(&self, dir: &Path, branch: &str) -> Result<Option<PrInfo>, GitHostError>;

    /// The pull request at `url`, as it stands now.
    ///
    /// # Errors
    ///
    /// Returns [`GitHostError`] when the host cannot answer; an unknown pull request is `Ok(None)`.
    fn pr_by_url(&self, url: &str) -> Result<Option<PrInfo>, GitHostError>;
}

/// The `gh` CLI.
#[derive(Debug, Clone)]
pub struct Gh {
    program: String,
}

impl Gh {
    /// Runs `program`, normally `gh`.
    #[must_use]
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl Default for Gh {
    fn default() -> Self {
        Self::new("gh")
    }
}

#[derive(Deserialize)]
struct RawPr {
    number: i64,
    state: String,
    url: String,
}

impl Gh {
    fn view(&self, dir: Option<&Path>, target: &str) -> Result<Option<PrInfo>, GitHostError> {
        let args = ["pr", "view", target, "--json", "number,state,url"];
        let shown = args.join(" ");
        let mut command = Command::new(&self.program);
        command.args(args);
        if let Some(dir) = dir {
            command.current_dir(dir);
        }
        let output = command.output().map_err(GitHostError::Run)?;
        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            if message.contains("no pull requests found") || message.contains("Could not resolve") {
                return Ok(None);
            }
            return Err(GitHostError::Failed {
                command: shown,
                message,
            });
        }
        parse_pr(&String::from_utf8_lossy(&output.stdout)).map_or_else(
            |source| {
                Err(GitHostError::Parse {
                    command: shown,
                    source,
                })
            },
            |pr| Ok(Some(pr)),
        )
    }
}

impl GitHost for Gh {
    fn pr_for_branch(&self, dir: &Path, branch: &str) -> Result<Option<PrInfo>, GitHostError> {
        self.view(Some(dir), branch)
    }

    fn pr_by_url(&self, url: &str) -> Result<Option<PrInfo>, GitHostError> {
        self.view(None, url)
    }
}

/// Parses the JSON of `gh pr view --json number,state,url`.
///
/// # Errors
///
/// Returns the parser's error.
pub fn parse_pr(text: &str) -> Result<PrInfo, serde_json::Error> {
    let raw: RawPr = serde_json::from_str(text)?;
    let state = match raw.state.as_str() {
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        _ => PrState::Open,
    };
    Ok(PrInfo {
        repo: repo_of_url(&raw.url),
        number: raw.number,
        url: raw.url,
        state,
    })
}

/// `owner/name` of a pull request URL such as `https://github.com/owner/name/pull/7`.
#[must_use]
pub fn repo_of_url(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split('/')
        .skip(1)
        .take(2)
        .collect::<Vec<_>>()
        .join("/")
}

/// A git host in memory.
#[derive(Debug, Default)]
pub struct FakeGit {
    prs: RefCell<HashMap<String, PrInfo>>,
}

impl FakeGit {
    /// A host without pull requests.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the pull request of a branch.
    pub fn set(&self, branch: &str, pr: PrInfo) {
        self.prs.borrow_mut().insert(branch.to_owned(), pr);
    }
}

impl GitHost for FakeGit {
    fn pr_for_branch(&self, _dir: &Path, branch: &str) -> Result<Option<PrInfo>, GitHostError> {
        Ok(self.prs.borrow().get(branch).cloned())
    }

    fn pr_by_url(&self, url: &str) -> Result<Option<PrInfo>, GitHostError> {
        Ok(self.prs.borrow().values().find(|pr| pr.url == url).cloned())
    }
}
