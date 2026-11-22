//! The git host (design section 4.2): the pull request of a task's branch. [`Gh`] asks the `gh` CLI;
//! [`FakeGit`] answers from memory for tests.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

use crate::timed::{self, TimedError};

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

    /// The pull request at `url`, as it stands now; the URL's host (github.com or a GitHub Enterprise one) picks where
    /// to ask.
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

/// How long one `gh` call may take before it is ended and reported as a failure, so a hung `gh` never holds up the
/// tick or a close.
pub const GH_TIMEOUT: Duration = Duration::from_secs(60);

/// Turns what `gh pr view` left into the answer: the pull request, none when `gh` says there is none, or an error.
///
/// # Errors
///
/// Returns [`GitHostError::Failed`] when `gh` failed for any other reason and [`GitHostError::Parse`] when its JSON
/// does not read.
pub fn interpret_view(
    shown: &str,
    success: bool,
    stdout: &str,
    stderr: &str,
) -> Result<Option<PrInfo>, GitHostError> {
    if !success {
        let message = stderr.trim().to_owned();
        if message.contains("no pull requests found") || message.contains("Could not resolve") {
            return Ok(None);
        }
        return Err(GitHostError::Failed {
            command: shown.to_owned(),
            message,
        });
    }
    parse_pr(stdout).map_or_else(
        |source| {
            Err(GitHostError::Parse {
                command: shown.to_owned(),
                source,
            })
        },
        |pr| Ok(Some(pr)),
    )
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
        let ran = timed::run(&mut command, GH_TIMEOUT).map_err(|error| match error {
            TimedError::Start { source, .. } => GitHostError::Run(source),
            TimedError::TimedOut { .. } => GitHostError::Failed {
                command: shown.clone(),
                message: error.to_string(),
            },
        })?;
        interpret_view(&shown, ran.success(), &ran.stdout, &ran.stderr)
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

/// `owner/name` of a pull request URL on any host, such as `https://github.com/owner/name/pull/7` or a GitHub
/// Enterprise one, `https://ghe.example.com/owner/name/pull/7`.
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

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "tests")]

    use super::*;

    const URL: &str = "https://github.com/owner/name/pull/7";

    fn viewed(success: bool, stdout: &str, stderr: &str) -> Result<Option<PrInfo>, GitHostError> {
        interpret_view("pr view x", success, stdout, stderr)
    }

    #[test]
    fn gh_output_reads_as_open_merged_closed_missing_or_an_error() {
        for (state, expected) in [
            ("OPEN", PrState::Open),
            ("MERGED", PrState::Merged),
            ("CLOSED", PrState::Closed),
        ] {
            let json = format!(r#"{{"number":7,"state":"{state}","url":"{URL}"}}"#);
            let pr = viewed(true, &json, "").unwrap().unwrap();
            assert_eq!(pr.state, expected, "{state}");
            assert_eq!(pr.number, 7);
        }
        for stderr in [
            "no pull requests found for branch \"x\"\n",
            "GraphQL: Could not resolve to a PullRequest with the number of 9.",
        ] {
            assert_eq!(viewed(false, "", stderr).unwrap(), None, "{stderr}");
        }
        assert!(matches!(
            viewed(false, "", "HTTP 502: Bad Gateway"),
            Err(GitHostError::Failed { message, .. }) if message == "HTTP 502: Bad Gateway"
        ));
        assert!(matches!(
            viewed(true, "not json", ""),
            Err(GitHostError::Parse { .. })
        ));
    }

    #[test]
    fn a_gh_that_outlives_its_deadline_is_a_failure_with_a_clear_message() {
        let mut command = Command::new("sleep");
        command.arg("30");
        let error = timed::run(&mut command, Duration::from_millis(200)).unwrap_err();
        assert!(error.to_string().contains("did not finish within"), "{error}");
    }

    #[test]
    fn a_pull_request_of_any_host_keeps_its_url_and_names_its_repository() {
        for url in [
            "https://github.com/owner/name/pull/7",
            "https://ghe.example.com/owner/name/pull/7",
        ] {
            assert_eq!(repo_of_url(url), "owner/name", "{url}");
            let pr =
                parse_pr(&format!(r#"{{"number":7,"state":"MERGED","url":"{url}"}}"#)).unwrap();
            assert_eq!(
                pr,
                PrInfo {
                    repo: "owner/name".to_owned(),
                    number: 7,
                    url: url.to_owned(),
                    state: PrState::Merged,
                },
                "{url}"
            );
        }
    }
}
