//! Where headwork's own releases come from: GitHub's release API and the release files, fetched with `curl`
//! (`docs/autoupdate.md`, "Installing releases").
//!
//! `curl` ships with Windows 10 and later and with macOS, so the binary needs no TLS library of its own. Each call has a
//! connect timeout and a total timeout. The release API is asked with the last `ETag`: an unchanged answer (`304`) does
//! not count against the rate limit. It is asked with a GitHub token when one is at hand ([`GithubToken::discover`]:
//! `GH_TOKEN`, `GITHUB_TOKEN`, then `gh auth token --hostname github.com`), which raises the limit from 60 requests an
//! hour per address to 5,000 per user, and without a login otherwise or when GitHub refuses the token (`401`). The token
//! goes to `curl` on its standard input, never on its command line, and is never printed. Release files download
//! without it: they are not counted against the API's limit, and the token never reaches the file host.
//! [`FakeReleases`] stands in for GitHub in tests.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::timed::{self, TimedError};

/// The checksum file every release carries beside its programs.
pub const SUMS_NAME: &str = "SHA256SUMS";

/// Why asking for a release or downloading a file failed.
#[derive(Debug, thiserror::Error)]
pub enum ReleaseError {
    /// `curl` did not run, failed, or ran out of time.
    #[error("{what}: {detail}")]
    Transfer {
        /// What was being fetched.
        what: String,
        /// What went wrong.
        detail: String,
    },
    /// GitHub refused for the rate limit (`403` or `429`).
    #[error("{what}: GitHub's rate limit (HTTP {status})")]
    RateLimited {
        /// What was being fetched.
        what: String,
        /// The HTTP status.
        status: u16,
    },
    /// GitHub has no such release (`404`).
    #[error("{what}: no such release")]
    NotFound {
        /// What was being fetched.
        what: String,
    },
    /// Another HTTP status.
    #[error("{what}: HTTP {status}")]
    Http {
        /// What was being fetched.
        what: String,
        /// The HTTP status.
        status: u16,
    },
    /// The answer was not a release.
    #[error("{what}: the answer is not a release: {detail}")]
    Parse {
        /// What was being fetched.
        what: String,
        /// What did not parse.
        detail: String,
    },
    /// No release program is built for this system.
    #[error(
        "no headwork release is built for {os} on {arch}; only Windows x86_64 and macOS arm64 are"
    )]
    Unsupported {
        /// The operating system.
        os: String,
        /// The processor.
        arch: String,
    },
}

impl ReleaseError {
    /// Whether trying again later may work: the network, the rate limit, a server error or a garbled answer, but not a
    /// missing release or an unsupported system.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Transfer { .. }
            | Self::RateLimited { .. }
            | Self::Http { .. }
            | Self::Parse { .. } => true,
            Self::NotFound { .. } | Self::Unsupported { .. } => false,
        }
    }
}

/// One file of a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    /// Its file name, such as `headwork-x86_64-pc-windows-msvc.exe`.
    pub name: String,
    /// Where it downloads from.
    pub url: String,
}

/// A release as GitHub describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseInfo {
    /// Its tag, such as `v0.1.214`.
    pub tag: String,
    /// When it was published, as GitHub writes it (`2026-11-22T10:15:30Z`).
    pub published_at: String,
    /// Its files.
    pub assets: Vec<Asset>,
}

impl ReleaseInfo {
    /// The file with this name.
    #[must_use]
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|asset| asset.name == name)
    }
}

/// What asking for the newest release gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// The release, with the `ETag` to send next time.
    Fresh {
        /// The release.
        release: ReleaseInfo,
        /// Its `ETag`, when GitHub sent one.
        etag: Option<String>,
    },
    /// Unchanged since the `ETag` sent.
    NotModified,
}

/// A source of releases: GitHub through `curl`, or a fake.
pub trait ReleaseSource: fmt::Debug {
    /// The newest release of `repo` (`owner/name`), or [`Fetched::NotModified`] when `etag` still matches.
    ///
    /// # Errors
    ///
    /// Returns a [`ReleaseError`] when the request fails or the answer is not a release.
    fn latest(&self, repo: &str, etag: Option<&str>) -> Result<Fetched, ReleaseError>;

    /// The release of `repo` tagged `tag`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::NotFound`] when there is none, and another [`ReleaseError`] when the request fails.
    fn tagged(&self, repo: &str, tag: &str) -> Result<ReleaseInfo, ReleaseError>;

    /// Downloads `url` to the file `to`.
    ///
    /// # Errors
    ///
    /// Returns a [`ReleaseError`] when the download fails.
    fn download(&self, url: &str, to: &Path) -> Result<(), ReleaseError>;
}

/// The release program's file name for this system.
///
/// # Errors
///
/// Returns [`ReleaseError::Unsupported`] on a system no release is built for.
pub fn asset_name() -> Result<&'static str, ReleaseError> {
    asset_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// The release program's file name for `os` on `arch`, as `std::env::consts` spells them.
///
/// # Errors
///
/// Returns [`ReleaseError::Unsupported`] for any pair but Windows `x86_64` and macOS `aarch64`.
pub fn asset_for(os: &str, arch: &str) -> Result<&'static str, ReleaseError> {
    match (os, arch) {
        ("windows", "x86_64") => Ok("headwork-x86_64-pc-windows-msvc.exe"),
        ("macos", "aarch64") => Ok("headwork-aarch64-apple-darwin"),
        _ => Err(ReleaseError::Unsupported {
            os: os.to_owned(),
            arch: arch.to_owned(),
        }),
    }
}

/// A GitHub login token for the release API. It prints as `GithubToken(***)` and has no `Display`, so no log or error
/// message can carry it.
#[derive(Clone, PartialEq, Eq)]
pub struct GithubToken(String);

impl fmt::Debug for GithubToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GithubToken(***)")
    }
}

impl GithubToken {
    /// `text` trimmed, when it can be a token: not empty, not too long, no whitespace or control characters (which
    /// could inject a header).
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let token = text.trim();
        let usable = !token.is_empty()
            && token.len() <= 1024
            && !token.chars().any(|c| c.is_whitespace() || c.is_control());
        usable.then(|| Self(token.to_owned()))
    }

    /// The first usable token of `GH_TOKEN`, `GITHUB_TOKEN`, and what `gh auth token --hostname github.com` prints
    /// (`gh` at `gh`, given at most 10 seconds); `None` when there is none, so the API is asked without a login.
    #[must_use]
    pub fn discover(gh: &Path) -> Option<Self> {
        Self::first_of(
            std::env::var("GH_TOKEN").ok(),
            std::env::var("GITHUB_TOKEN").ok(),
            || Self::from_gh(gh),
        )
    }

    /// The first usable of `gh_token`, `github_token` and what `from_gh` gives, asked only when the first two are not.
    #[must_use]
    pub fn first_of(
        gh_token: Option<String>,
        github_token: Option<String>,
        from_gh: impl FnOnce() -> Option<String>,
    ) -> Option<Self> {
        [gh_token, github_token]
            .into_iter()
            .flatten()
            .find_map(|text| Self::parse(&text))
            .or_else(|| from_gh().and_then(|text| Self::parse(&text)))
    }

    fn from_gh(gh: &Path) -> Option<String> {
        let mut command = Command::new(gh);
        command.args(["auth", "token", "--hostname", "github.com"]);
        let ran = timed::run(&mut command, Duration::from_secs(10)).ok()?;
        ran.success().then_some(ran.stdout)
    }

    fn header_line(&self) -> Vec<u8> {
        format!("Authorization: Bearer {}\n", self.0).into_bytes()
    }
}

/// GitHub through `curl`.
#[derive(Debug, Clone)]
pub struct Curl {
    program: PathBuf,
    connect: Duration,
    api: Duration,
    download: Duration,
    token: Option<GithubToken>,
}

impl Default for Curl {
    fn default() -> Self {
        Self::new("curl")
    }
}

impl Curl {
    /// `curl` at `program`, with a 10-second connect timeout, 60 seconds for an API request and 10 minutes for a file.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            connect: Duration::from_secs(10),
            api: Duration::from_secs(60),
            download: Duration::from_secs(600),
            token: None,
        }
    }

    /// The same `curl`, asking the release API with `token` when there is one.
    #[must_use]
    pub fn with_token(self, token: Option<GithubToken>) -> Self {
        Self { token, ..self }
    }

    /// `curl` on the `PATH`, with the token [`GithubToken::discover`] finds through `gh` on the `PATH`.
    #[must_use]
    pub fn discovered() -> Self {
        Self::default().with_token(GithubToken::discover(Path::new("gh")))
    }

    /// Whether the release API is asked with a token.
    #[must_use]
    pub const fn authenticated(&self) -> bool {
        self.token.is_some()
    }

    fn command(&self, total: Duration) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(["--silent", "--show-error", "--location"])
            .args(["--connect-timeout", &self.connect.as_secs().to_string()])
            .args(["--max-time", &total.as_secs().to_string()])
            .args(["--user-agent", "headwork"]);
        command
    }

    fn run(
        command: &mut Command,
        input: Option<Vec<u8>>,
        total: Duration,
        what: &str,
    ) -> Result<String, ReleaseError> {
        let transfer = |detail: String| ReleaseError::Transfer {
            what: what.to_owned(),
            detail,
        };
        let deadline = total + Duration::from_secs(15);
        let ran = match input {
            Some(input) => timed::run_with_input(command, input, deadline),
            None => timed::run(command, deadline),
        };
        let ran = ran.map_err(|error| match error {
            TimedError::Start { .. } | TimedError::TimedOut { .. } => transfer(error.to_string()),
        })?;
        if ran.success() {
            Ok(ran.stdout)
        } else {
            Err(transfer(format!(
                "curl failed ({}): {}",
                ran.status,
                ran.printed()
            )))
        }
    }

    fn api(&self, url: &str, etag: Option<&str>, what: &str) -> Result<Fetched, ReleaseError> {
        match self.api_as(self.token.as_ref(), url, etag, what) {
            Err(ReleaseError::Http { status: 401, .. }) if self.token.is_some() => {
                self.api_as(None, url, etag, what)
            }
            answer => answer,
        }
    }

    fn api_as(
        &self,
        token: Option<&GithubToken>,
        url: &str,
        etag: Option<&str>,
        what: &str,
    ) -> Result<Fetched, ReleaseError> {
        let mut command = self.command(self.api);
        command
            .arg("--include")
            .args(["--header", "Accept: application/vnd.github+json"])
            .args(["--header", "X-GitHub-Api-Version: 2022-11-28"]);
        if let Some(etag) = etag.filter(|etag| usable_etag(etag)) {
            command.args(["--header", &format!("If-None-Match: {etag}")]);
        }
        if token.is_some() {
            command.args(["--header", "@-"]);
        }
        command.arg(url);
        let input = token.map(GithubToken::header_line);
        let text = Self::run(&mut command, input, self.api, what)?;
        let response = Response::parse(&text).ok_or_else(|| ReleaseError::Parse {
            what: what.to_owned(),
            detail: "no HTTP status line".to_owned(),
        })?;
        match response.status {
            200 => Ok(Fetched::Fresh {
                release: parse_release(response.body, what)?,
                etag: response
                    .header("etag")
                    .filter(|etag| usable_etag(etag))
                    .map(str::to_owned),
            }),
            304 => Ok(Fetched::NotModified),
            404 => Err(ReleaseError::NotFound {
                what: what.to_owned(),
            }),
            status @ (403 | 429) => Err(ReleaseError::RateLimited {
                what: what.to_owned(),
                status,
            }),
            status => Err(ReleaseError::Http {
                what: what.to_owned(),
                status,
            }),
        }
    }
}

impl ReleaseSource for Curl {
    fn latest(&self, repo: &str, etag: Option<&str>) -> Result<Fetched, ReleaseError> {
        self.api(
            &format!("https://api.github.com/repos/{repo}/releases/latest"),
            etag,
            &format!("the newest release of {repo}"),
        )
    }

    fn tagged(&self, repo: &str, tag: &str) -> Result<ReleaseInfo, ReleaseError> {
        let what = format!("release {tag} of {repo}");
        match self.api(
            &format!("https://api.github.com/repos/{repo}/releases/tags/{tag}"),
            None,
            &what,
        )? {
            Fetched::Fresh { release, .. } => Ok(release),
            Fetched::NotModified => Err(ReleaseError::Parse {
                what,
                detail: "304 without an ETag sent".to_owned(),
            }),
        }
    }

    fn download(&self, url: &str, to: &Path) -> Result<(), ReleaseError> {
        let mut command = self.command(self.download);
        command.arg("--fail").arg("--output").arg(to).arg(url);
        Self::run(
            &mut command,
            None,
            self.download,
            &format!("downloading {url}"),
        )
        .map(drop)
    }
}

fn usable_etag(etag: &str) -> bool {
    !etag.is_empty() && etag.len() < 200 && !etag.chars().any(char::is_control)
}

#[derive(Debug, Deserialize)]
struct ReleaseJson {
    tag_name: String,
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<AssetJson>,
}

#[derive(Debug, Deserialize)]
struct AssetJson {
    name: String,
    browser_download_url: String,
}

/// Reads GitHub's release JSON.
///
/// # Errors
///
/// Returns [`ReleaseError::Parse`] when it is not a release.
pub fn parse_release(body: &str, what: &str) -> Result<ReleaseInfo, ReleaseError> {
    let json: ReleaseJson = serde_json::from_str(body).map_err(|error| ReleaseError::Parse {
        what: what.to_owned(),
        detail: error.to_string(),
    })?;
    Ok(ReleaseInfo {
        tag: json.tag_name,
        published_at: json.published_at.unwrap_or_default(),
        assets: json
            .assets
            .into_iter()
            .map(|asset| Asset {
                name: asset.name,
                url: asset.browser_download_url,
            })
            .collect(),
    })
}

/// An HTTP response as `curl --include` prints it: one header block per hop of a redirect, then the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response<'a> {
    /// The last hop's status.
    pub status: u16,
    /// The last hop's headers, names in lower case.
    pub headers: Vec<(String, &'a str)>,
    /// The body.
    pub body: &'a str,
}

impl<'a> Response<'a> {
    /// Splits `curl --include` output; `None` when it does not start with a status line.
    #[must_use]
    pub fn parse(text: &'a str) -> Option<Self> {
        let mut rest = text;
        let mut last = None;
        while rest.starts_with("HTTP/") {
            let (block, after) = split_block(rest);
            last = Some(block);
            rest = after;
        }
        let block = last?;
        let mut lines = block.lines();
        let status = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim()))
            .collect();
        Some(Self {
            status,
            headers,
            body: rest,
        })
    }

    /// The value of the header `name` (lower case).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&'a str> {
        self.headers
            .iter()
            .find(|(found, _)| found == name)
            .map(|(_, value)| *value)
    }
}

fn split_block(text: &str) -> (&str, &str) {
    for separator in ["\r\n\r\n", "\n\n"] {
        if let Some((block, rest)) = text.split_once(separator) {
            return (block, rest);
        }
    }
    (text, "")
}

/// Releases and files held in memory, for tests; the newest release is the one published last.
#[derive(Debug, Default)]
pub struct FakeReleases {
    state: Mutex<FakeState>,
}

#[derive(Debug, Default)]
struct FakeState {
    releases: Vec<ReleaseInfo>,
    files: BTreeMap<String, Vec<u8>>,
    failures: VecDeque<ReleaseError>,
    requests: u32,
    downloads: Vec<String>,
}

impl FakeReleases {
    /// No releases yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn with<T>(&self, act: impl FnOnce(&mut FakeState) -> T) -> T {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        act(&mut state)
    }

    /// Publishes `release` with the files its assets point at, given as `(url, bytes)`.
    pub fn publish(&self, release: ReleaseInfo, files: Vec<(String, Vec<u8>)>) {
        self.with(|state| {
            state.releases.push(release);
            state.files.extend(files);
        });
    }

    /// Makes the next request or download fail with `error`.
    pub fn fail_next(&self, error: ReleaseError) {
        self.with(|state| state.failures.push_back(error));
    }

    /// How many release API requests were made.
    #[must_use]
    pub fn requests(&self) -> u32 {
        self.with(|state| state.requests)
    }

    /// The URLs downloaded, in order.
    #[must_use]
    pub fn downloads(&self) -> Vec<String> {
        self.with(|state| state.downloads.clone())
    }

    fn etag(release: &ReleaseInfo) -> String {
        format!("\"{}\"", release.tag)
    }
}

impl ReleaseSource for FakeReleases {
    fn latest(&self, repo: &str, etag: Option<&str>) -> Result<Fetched, ReleaseError> {
        self.with(|state| {
            state.requests += 1;
            if let Some(error) = state.failures.pop_front() {
                return Err(error);
            }
            let release = state
                .releases
                .last()
                .ok_or_else(|| ReleaseError::NotFound {
                    what: format!("the newest release of {repo}"),
                })?;
            let tag = Self::etag(release);
            if etag == Some(tag.as_str()) {
                return Ok(Fetched::NotModified);
            }
            Ok(Fetched::Fresh {
                release: release.clone(),
                etag: Some(tag),
            })
        })
    }

    fn tagged(&self, repo: &str, tag: &str) -> Result<ReleaseInfo, ReleaseError> {
        self.with(|state| {
            state.requests += 1;
            if let Some(error) = state.failures.pop_front() {
                return Err(error);
            }
            state
                .releases
                .iter()
                .find(|release| release.tag == tag)
                .cloned()
                .ok_or_else(|| ReleaseError::NotFound {
                    what: format!("release {tag} of {repo}"),
                })
        })
    }

    fn download(&self, url: &str, to: &Path) -> Result<(), ReleaseError> {
        self.with(|state| {
            if let Some(error) = state.failures.pop_front() {
                return Err(error);
            }
            state.downloads.push(url.to_owned());
            let bytes = state.files.get(url).ok_or_else(|| ReleaseError::Http {
                what: format!("downloading {url}"),
                status: 404,
            })?;
            std::fs::write(to, bytes).map_err(|error| ReleaseError::Transfer {
                what: format!("writing {}", to.display()),
                detail: error.to_string(),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "tests fail loudly on an unexpected error"
    )]

    use super::*;

    #[test]
    fn each_supported_system_has_its_program() {
        assert_eq!(
            asset_for("windows", "x86_64").unwrap(),
            "headwork-x86_64-pc-windows-msvc.exe",
            "windows"
        );
        assert_eq!(
            asset_for("macos", "aarch64").unwrap(),
            "headwork-aarch64-apple-darwin",
            "mac"
        );
        for (os, arch) in [
            ("linux", "x86_64"),
            ("macos", "x86_64"),
            ("windows", "aarch64"),
        ] {
            let error = asset_for(os, arch).unwrap_err();
            assert!(!error.is_transient(), "{os} {arch}");
            assert!(error.to_string().contains(os), "{error}");
        }
    }

    #[test]
    fn a_response_after_a_redirect_takes_the_last_status_and_headers() {
        let text = "HTTP/1.1 302 Found\r\nLocation: x\r\n\r\nHTTP/2 200\r\nETag: W/\"abc\"\r\ncontent-type: json\r\n\r\n{\"a\":1}";
        let response = Response::parse(text).unwrap();
        assert_eq!(response.status, 200, "last status");
        assert_eq!(response.header("etag"), Some("W/\"abc\""), "etag, any case");
        assert_eq!(response.body, "{\"a\":1}", "body");
        let not_modified = Response::parse("HTTP/2 304\r\netag: \"x\"\r\n\r\n").unwrap();
        assert_eq!(not_modified.status, 304, "304");
        assert_eq!(not_modified.body, "", "no body");
        assert_eq!(Response::parse("{}"), None, "no status line");
    }

    #[test]
    fn a_release_reads_its_tag_time_and_files() {
        let body = r#"{"tag_name":"v0.1.7","published_at":"2026-11-22T10:00:00Z","draft":false,
            "assets":[{"name":"SHA256SUMS","browser_download_url":"https://x/SHA256SUMS","size":1}]}"#;
        let release = parse_release(body, "test").unwrap();
        assert_eq!(release.tag, "v0.1.7", "tag");
        assert_eq!(release.published_at, "2026-11-22T10:00:00Z", "time");
        assert_eq!(
            release.asset(SUMS_NAME).map(|asset| asset.url.as_str()),
            Some("https://x/SHA256SUMS"),
            "asset"
        );
        let error = parse_release("{\"message\":\"Not Found\"}", "test").unwrap_err();
        assert!(error.is_transient(), "a garbled answer is retried");
    }

    #[test]
    fn a_token_comes_from_gh_token_then_github_token_then_gh() {
        let token = |text: &str| GithubToken::parse(text);
        let asked = std::cell::Cell::new(false);
        let gh = || {
            asked.set(true);
            Some("gho_fromgh\n".to_owned())
        };
        assert_eq!(
            GithubToken::first_of(Some("a".to_owned()), Some("b".to_owned()), gh),
            token("a"),
            "GH_TOKEN first"
        );
        assert!(!asked.get(), "gh not asked when the environment has one");
        assert_eq!(
            GithubToken::first_of(Some("  ".to_owned()), Some("b".to_owned()), || None),
            token("b"),
            "an empty GH_TOKEN is passed over"
        );
        assert_eq!(
            GithubToken::first_of(None, None, gh),
            token("gho_fromgh"),
            "gh's token, trimmed"
        );
        assert_eq!(
            GithubToken::first_of(None, None, || None),
            None,
            "anonymous"
        );
    }

    #[test]
    fn a_token_never_prints_and_never_injects_a_header() {
        let token = GithubToken::parse("ghp_secret").unwrap();
        assert_eq!(format!("{token:?}"), "GithubToken(***)", "redacted");
        let curl = Curl::default().with_token(Some(token));
        assert!(
            !format!("{curl:?}").contains("ghp_secret"),
            "curl redacts it"
        );
        assert!(curl.authenticated(), "authenticated");
        assert_eq!(GithubToken::parse("a\r\nX-Injected: yes"), None, "newline");
        assert_eq!(GithubToken::parse("a b"), None, "space");
        assert_eq!(GithubToken::parse(""), None, "empty");
    }

    #[test]
    fn etags_with_control_characters_are_never_sent() {
        assert!(usable_etag("W/\"abc\""), "plain");
        assert!(!usable_etag(""), "empty");
        assert!(!usable_etag("a\r\nInjected: yes"), "newline");
    }

    #[test]
    fn the_fake_answers_not_modified_for_its_etag() {
        let fake = FakeReleases::new();
        assert!(
            matches!(fake.latest("a/b", None), Err(ReleaseError::NotFound { .. })),
            "no release yet"
        );
        fake.publish(
            ReleaseInfo {
                tag: "v0.1.1".to_owned(),
                published_at: String::new(),
                assets: Vec::new(),
            },
            Vec::new(),
        );
        let Ok(Fetched::Fresh { etag, .. }) = fake.latest("a/b", None) else {
            unreachable!("a release is published")
        };
        assert_eq!(
            fake.latest("a/b", etag.as_deref()).unwrap(),
            Fetched::NotModified,
            "unchanged"
        );
        assert_eq!(fake.requests(), 3, "requests counted");
    }
}
