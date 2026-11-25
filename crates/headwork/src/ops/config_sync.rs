//! `config`: the effective `config.toml` generated from a shared file, a machine file and a local file (`docs/config-sync.md`).
//!
//! `sources.toml` names the repository and the machine. [`sync`] pulls the repository, merges the layers with
//! [`headwork_core::config_layers`], validates the result like the update smoke test does, and replaces `config.toml`
//! atomically; any failure leaves the file the machine runs on as it was. [`inspect`] compares the sources and the
//! effective file with what the last sync wrote, for `doctor` and the tick.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use headwork_adapters::timed;
use headwork_core::config::HostKind;
use headwork_core::config_layers::{
    KeyPath, LayerError, LayerInput, LayerName, Merged, is_machine_only, is_secret, merge,
};
use headwork_core::{Actor, Config, Platform};
use headwork_store::Store;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use toml::{Table, Value};

use super::slashed;
use super::update::{Builds, SystemBuilds, sha256_file};
use crate::home::Home;

const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// The event a successful sync logs.
pub const EVENT_SYNCED: &str = "config-synced";
/// The event a failed sync logs.
pub const EVENT_SYNC_FAILED: &str = "config-sync-failed";
/// The event a rollback logs.
pub const EVENT_ROLLED_BACK: &str = "config-rolled-back";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourcesFile {
    repo: String,
    machine: String,
}

/// Where the three layers of a synced config are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The repository folder.
    pub repo: PathBuf,
    /// The machine's name.
    pub machine: String,
    /// `<repo>/headwork/config.toml`.
    pub shared_file: PathBuf,
    /// `<repo>/machines/<machine>/headwork.toml`.
    pub machine_file: PathBuf,
    /// `<home>/config.local.toml`.
    pub local_file: PathBuf,
}

impl Layout {
    fn new(home: &Home, repo: PathBuf, machine: &str) -> Result<Self> {
        let clean = !machine.is_empty()
            && machine
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !clean {
            bail!("machine name `{machine}` must be letters, digits, `-` or `_`");
        }
        Ok(Self {
            shared_file: repo.join("headwork").join("config.toml"),
            machine_file: repo.join("machines").join(machine).join("headwork.toml"),
            local_file: home.local_config_path(),
            repo,
            machine: machine.to_owned(),
        })
    }

    /// Reads `sources.toml`; `None` when the machine is not synced.
    ///
    /// # Errors
    ///
    /// Fails when `sources.toml` exists but does not parse.
    pub fn load(home: &Home) -> Result<Option<Self>> {
        let path = home.sources_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        };
        let sources: SourcesFile =
            toml::from_str(&text).with_context(|| format!("{} is not valid", slashed(&path)))?;
        Self::new(home, home.expand(&sources.repo), &sources.machine)
            .with_context(|| format!("{} is not valid", slashed(&path)))
            .map(Some)
    }
}

/// What kind of trouble a source or a merge result has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProblemKind {
    /// A source file is missing, unreadable or does not merge.
    Source,
    /// A tracked file sets a secret key.
    Secret,
    /// The merged text does not parse as a config.
    Config,
    /// The effective host does not suit this OS.
    Host,
    /// The pull failed.
    Pull,
    /// A check of the candidate that the live config passes fails.
    Doctor,
}

/// One reason a config cannot be written; the message names the file and key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    /// What kind it is.
    pub kind: ProblemKind,
    /// What is wrong.
    pub message: String,
}

impl Problem {
    fn new(kind: ProblemKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// The merged layers, rendered and parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// The effective config as TOML text.
    pub text: String,
    /// The merge, with the origin of each key.
    pub merged: Merged,
    /// The text parsed.
    pub config: Config,
}

fn read_source(path: &Path) -> Result<String, Problem> {
    std::fs::read_to_string(path).map_err(|error| {
        Problem::new(
            ProblemKind::Source,
            format!("{}: cannot be read: {error}", slashed(path)),
        )
    })
}

fn read_optional(path: &Path) -> Result<Option<String>, Problem> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Problem::new(
            ProblemKind::Source,
            format!("{}: cannot be read: {error}", slashed(path)),
        )),
    }
}

/// Whether a terminal host runs on an OS: wmux on Windows, cmux on macOS, either elsewhere.
#[must_use]
pub const fn host_suits(host: HostKind, platform: Platform) -> bool {
    match platform {
        Platform::Windows => matches!(host, HostKind::Wmux),
        Platform::MacOs => matches!(host, HostKind::Cmux),
        Platform::Unix => true,
    }
}

/// Merges the layers and checks the result the way `Config::parse` and the loader do.
///
/// # Errors
///
/// Returns the first [`Problem`], naming the file and key.
pub fn build(home: &Home, layout: &Layout, platform: Platform) -> Result<Candidate, Problem> {
    let shared = read_source(&layout.shared_file)?;
    let machine = read_source(&layout.machine_file)?;
    let local = read_optional(&layout.local_file)?;
    let shared_name = slashed(&layout.shared_file);
    let machine_name = slashed(&layout.machine_file);
    let local_name = slashed(&layout.local_file);
    let merged = merge(
        LayerInput {
            name: LayerName::Shared,
            origin: &shared_name,
            text: &shared,
        },
        LayerInput {
            name: LayerName::Machine,
            origin: &machine_name,
            text: &machine,
        },
        local.as_deref().map(|text| LayerInput {
            name: LayerName::Local,
            origin: &local_name,
            text,
        }),
    )
    .map_err(|error| {
        let kind = if matches!(error, LayerError::SecretInTracked { .. }) {
            ProblemKind::Secret
        } else {
            ProblemKind::Source
        };
        Problem::new(kind, error.to_string())
    })?;
    let text = merged
        .to_toml_string()
        .map_err(|error| Problem::new(ProblemKind::Source, error.to_string()))?;
    let origin = format!("the merge of {shared_name} and {machine_name}");
    let config = Config::parse(&text, &origin)
        .map_err(|error| Problem::new(ProblemKind::Config, format!("{error:#}")))?;
    if !host_suits(config.machine.host, platform) {
        return Err(Problem::new(
            ProblemKind::Host,
            format!(
                "{machine_name}: machine.host = \"{}\" does not suit this OS",
                config.machine.host.as_str()
            ),
        ));
    }
    home.with_overlays(config.clone())
        .map_err(|error| Problem::new(ProblemKind::Config, format!("{error:#}")))?;
    Ok(Candidate {
        text,
        merged,
        config,
    })
}

/// The repository's `HEAD`, when it is a git checkout.
#[must_use]
pub fn head_commit(repo: &Path) -> Option<String> {
    let ran = timed::run(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "HEAD"]),
        Duration::from_secs(30),
    )
    .ok()?;
    ran.success().then(|| ran.stdout.trim().to_owned())
}

fn git_in(repo: &Path, args: &[&str]) -> Result<timed::Ran, String> {
    timed::run(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env(
                "GIT_SSH_COMMAND",
                headwork_adapters::ssh::batch_command_in(repo),
            ),
        GIT_TIMEOUT,
    )
    .map_err(|error| {
        format!(
            "git {} in {} did not run: {error}",
            args.join(" "),
            slashed(repo)
        )
    })
}

fn git_ok(repo: &Path, args: &[&str]) -> Result<String, String> {
    let ran = git_in(repo, args)?;
    if ran.success() {
        Ok(ran.stdout.trim().to_owned())
    } else {
        Err(format!(
            "git {} in {} failed: {}",
            args.join(" "),
            slashed(repo),
            ran.printed()
        ))
    }
}

/// Fetches and fast-forwards the checked-out branch, whatever the working tree holds.
///
/// A plain `git pull` refuses a tree with unstaged changes when `pull.rebase` is set, and a source repository that
/// another tool edits (wmux rewrites its block in `CLAUDE.md`) is never clean. So this fetches, then merges
/// `@{upstream}` with `--ff-only`, which leaves unrelated local changes alone; only when the merge would overwrite a
/// local change does it retry with `--autostash`. A failure leaves `config.toml` untouched.
fn pull(repo: &Path) -> Result<String, String> {
    git_ok(repo, &["fetch", "--quiet"])?;
    git_ok(repo, &["merge", "--ff-only", "--quiet", "@{upstream}"]).or_else(|_plain| {
        git_ok(
            repo,
            &[
                "merge",
                "--ff-only",
                "--autostash",
                "--quiet",
                "@{upstream}",
            ],
        )
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _infallible: fmt::Result = write!(hex, "{byte:02x}");
    }
    hex
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct OriginFile {
    commit: Option<String>,
    machine: String,
    written_sha256: String,
    origins: BTreeMap<String, String>,
}

fn read_origin(home: &Home) -> Option<OriginFile> {
    let text = std::fs::read_to_string(home.origin_path()).ok()?;
    serde_json::from_str(&text).ok()
}

fn origin_file(layout: &Layout, commit: Option<String>, candidate: &Candidate) -> OriginFile {
    OriginFile {
        commit,
        machine: layout.machine.clone(),
        written_sha256: sha256_hex(candidate.text.as_bytes()),
        origins: candidate
            .merged
            .origins
            .iter()
            .map(|(key, layer)| (key.to_string(), layer.to_string()))
            .collect(),
    }
}

/// What a doctor or the tick learns about a synced config.
#[derive(Debug, Clone, PartialEq)]
pub struct Inspection {
    /// Where the layers are.
    pub layout: Layout,
    /// The current merge.
    pub candidate: Result<Candidate, Problem>,
    /// The repository's `HEAD` now.
    pub head: Option<String>,
    origin: Option<OriginFile>,
    live_sha: Option<String>,
}

/// Looks at the sources and the effective file without changing anything; `Ok(None)` when not synced.
///
/// # Errors
///
/// Fails when `sources.toml` does not parse.
pub fn inspect(home: &Home, platform: Platform) -> Result<Option<Inspection>> {
    let Some(layout) = Layout::load(home)? else {
        return Ok(None);
    };
    let candidate = build(home, &layout, platform);
    let head = head_commit(&layout.repo);
    Ok(Some(Inspection {
        candidate,
        head,
        origin: read_origin(home),
        live_sha: sha256_file(&home.config_path()).ok(),
        layout,
    }))
}

impl Inspection {
    /// Why the sources are ahead of the effective file, when they are.
    #[must_use]
    pub fn stale(&self) -> Option<String> {
        let Some(origin) = &self.origin else {
            return Some("no sync is recorded (config.origin.json is missing)".to_owned());
        };
        if origin.commit != self.head {
            return Some(format!(
                "the repository is at {}, the last sync used {}",
                short(self.head.as_deref()),
                short(origin.commit.as_deref())
            ));
        }
        match &self.candidate {
            Err(problem) => Some(format!("the sources do not merge: {problem}")),
            Ok(candidate) if sha256_hex(candidate.text.as_bytes()) != origin.written_sha256 => {
                Some("the merge of the sources differs from what the last sync wrote".to_owned())
            }
            Ok(_) => None,
        }
    }

    /// Why `config.toml` is not what the last sync wrote, when it is not.
    #[must_use]
    pub fn drift(&self, home: &Home) -> Option<String> {
        let origin = self.origin.as_ref()?;
        let live = self.live_sha.as_deref();
        if live == Some(origin.written_sha256.as_str()) {
            return None;
        }
        if live.is_none() {
            return Some("config.toml is missing".to_owned());
        }
        let keys = self
            .candidate
            .as_ref()
            .ok()
            .and_then(|candidate| differing_keys(home, candidate))
            .filter(|keys| !keys.is_empty());
        Some(keys.map_or_else(
            || "config.toml was edited by hand after the last sync wrote it".to_owned(),
            |keys| {
                format!(
                    "config.toml was edited by hand; keys that differ from the sources: {}",
                    keys.join(", ")
                )
            },
        ))
    }
}

fn short(commit: Option<&str>) -> &str {
    commit.map_or("no commit", |commit| commit.get(..10).unwrap_or(commit))
}

fn differing_keys(home: &Home, candidate: &Candidate) -> Option<Vec<String>> {
    let live: Table = toml::from_str(&std::fs::read_to_string(home.config_path()).ok()?).ok()?;
    let mut live_leaves = BTreeMap::new();
    leaves(&live, &mut Vec::new(), &mut live_leaves);
    let mut wanted = BTreeMap::new();
    leaves(&candidate.merged.table, &mut Vec::new(), &mut wanted);
    let mut keys: Vec<String> = live_leaves
        .keys()
        .chain(wanted.keys())
        .filter(|key| live_leaves.get(*key) != wanted.get(*key))
        .cloned()
        .collect();
    keys.sort();
    keys.dedup();
    Some(keys)
}

fn leaves(table: &Table, prefix: &mut Vec<String>, out: &mut BTreeMap<String, String>) {
    for (key, value) in table {
        prefix.push(key.clone());
        match value {
            Value::Table(inner) if !inner.is_empty() => leaves(inner, prefix, out),
            Value::Table(_)
            | Value::String(_)
            | Value::Integer(_)
            | Value::Float(_)
            | Value::Boolean(_)
            | Value::Datetime(_)
            | Value::Array(_) => {
                out.insert(KeyPath::new(prefix.clone()).to_string(), value.to_string());
            }
        }
        prefix.pop();
    }
}

fn not_synced(home: &Home) -> Problem {
    Problem::new(
        ProblemKind::Source,
        format!(
            "not synced: {} does not exist; `headwork config adopt --machine <name>` sets it up",
            slashed(&home.sources_path())
        ),
    )
}

/// The result of `config check`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckReport {
    /// Whether the sources merge into a valid config.
    pub ok: bool,
    /// What is wrong.
    pub problems: Vec<Problem>,
    /// Projects the shared file describes that no machine file gave a `main`.
    pub dropped_projects: Vec<String>,
    /// The repository's `HEAD`.
    pub commit: Option<String>,
}

impl fmt::Display for CheckReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ok {
            writeln!(f, "config sources are valid")?;
        }
        for problem in &self.problems {
            writeln!(f, "problem: {problem}")?;
        }
        for project in &self.dropped_projects {
            writeln!(
                f,
                "dropped project {project}: no machine file gives it a `main`"
            )?;
        }
        Ok(())
    }
}

/// Merges and validates the sources without writing anything.
#[must_use]
pub fn check(home: &Home, platform: Platform) -> CheckReport {
    let layout = match Layout::load(home) {
        Ok(Some(layout)) => layout,
        Ok(None) => return failed_check(not_synced(home)),
        Err(error) => return failed_check(Problem::new(ProblemKind::Source, format!("{error:#}"))),
    };
    let commit = head_commit(&layout.repo);
    match build(home, &layout, platform) {
        Ok(candidate) => CheckReport {
            ok: true,
            problems: Vec::new(),
            dropped_projects: candidate.merged.dropped_projects,
            commit,
        },
        Err(problem) => CheckReport {
            commit,
            ..failed_check(problem)
        },
    }
}

fn failed_check(problem: Problem) -> CheckReport {
    CheckReport {
        ok: false,
        problems: vec![problem],
        dropped_projects: Vec::new(),
        commit: None,
    }
}

/// What `config sync` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncOptions {
    /// Run `git pull --ff-only` in the repository first.
    pub pull: bool,
    /// Validate and report, write nothing.
    pub dry_run: bool,
}

/// What a sync did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "a report of independent facts, serialized as is"
)]
pub struct SyncReport {
    /// Whether the candidate was valid and, unless a dry run, written.
    pub ok: bool,
    /// Whether nothing was written on purpose.
    pub dry_run: bool,
    /// What `git pull` printed, when it ran.
    pub pulled: Option<String>,
    /// The repository's `HEAD`.
    pub commit: Option<String>,
    /// Whether `config.toml` was replaced.
    pub written: bool,
    /// Whether the candidate equals the file it replaces.
    pub unchanged: bool,
    /// Why it failed.
    pub problems: Vec<Problem>,
    /// Projects dropped because no machine file gives them a `main`.
    pub dropped_projects: Vec<String>,
    /// The changed lines, `-` old and `+` new.
    pub diff: Vec<String>,
}

impl SyncReport {
    fn failure(problem: Problem, mut base: Self) -> Self {
        base.ok = false;
        base.problems.push(problem);
        base
    }

    /// One line for a command that ran a sync as a side step.
    #[must_use]
    pub fn summary(&self) -> String {
        if !self.ok {
            let why: Vec<String> = self.problems.iter().map(ToString::to_string).collect();
            return format!(
                "config sync failed, the old config.toml stays: {}",
                why.join("; ")
            );
        }
        if self.unchanged {
            "config sync: config.toml is up to date".to_owned()
        } else {
            format!(
                "config sync: config.toml rewritten ({} changed lines)",
                self.diff.len()
            )
        }
    }
}

impl fmt::Display for SyncReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(pulled) = &self.pulled {
            writeln!(f, "pulled: {pulled}")?;
        }
        for line in &self.diff {
            writeln!(f, "{line}")?;
        }
        for project in &self.dropped_projects {
            writeln!(
                f,
                "dropped project {project}: no machine file gives it a `main`"
            )?;
        }
        for problem in &self.problems {
            writeln!(f, "problem: {problem}")?;
        }
        if self.ok {
            let tail = match (self.dry_run, self.unchanged) {
                (true, true) => "dry run: config.toml is already up to date",
                (true, false) => "dry run: config.toml would change; nothing written",
                (false, true) => "config.toml is up to date",
                (false, false) => "config.toml written; the previous one is config.toml.prev",
            };
            writeln!(f, "{tail}")?;
        } else {
            writeln!(f, "sync failed; config.toml is untouched")?;
        }
        Ok(())
    }
}

/// Where a sync logs and who it acts as.
#[derive(Debug, Clone, Copy)]
pub struct Logging<'a> {
    /// The store, when one is open; events are skipped without it.
    pub store: Option<&'a Store>,
    /// Who syncs.
    pub actor: Actor,
}

impl Logging<'_> {
    fn log(&self, kind: &str, data: &serde_json::Value) {
        if let Some(store) = self.store {
            drop(store.append_event(self.actor, None, None, kind, data));
        }
    }
}

/// Pulls, merges, validates and writes the effective config.
///
/// A failing source, merge or check is reported in [`SyncReport::problems`] and leaves `config.toml` as it was.
///
/// # Errors
///
/// Fails when the machine is not synced or `sources.toml` is invalid, or when a file cannot be written.
pub fn sync(
    home: &Home,
    exe: &Path,
    options: SyncOptions,
    platform: Platform,
    log: Logging<'_>,
) -> Result<SyncReport> {
    let Some(layout) = Layout::load(home)? else {
        bail!("{}", not_synced(home));
    };
    let report = run_sync(home, exe, &layout, options, platform)?;
    if report.ok && !report.dry_run {
        log.log(
            EVENT_SYNCED,
            &serde_json::json!({
                "commit": report.commit, "machine": layout.machine,
                "unchanged": report.unchanged, "changed_lines": report.diff.len(),
                "dropped_projects": report.dropped_projects,
            }),
        );
    } else if !report.ok {
        let reasons: Vec<String> = report.problems.iter().map(ToString::to_string).collect();
        log.log(
            EVENT_SYNC_FAILED,
            &serde_json::json!({ "commit": report.commit, "machine": layout.machine, "reason": reasons.join("; ") }),
        );
    }
    Ok(report)
}

fn run_sync(
    home: &Home,
    exe: &Path,
    layout: &Layout,
    options: SyncOptions,
    platform: Platform,
) -> Result<SyncReport> {
    let mut report = SyncReport {
        ok: true,
        dry_run: options.dry_run,
        pulled: None,
        commit: None,
        written: false,
        unchanged: false,
        problems: Vec::new(),
        dropped_projects: Vec::new(),
        diff: Vec::new(),
    };
    if options.pull {
        match pull(&layout.repo) {
            Ok(printed) => report.pulled = Some(printed),
            Err(message) => {
                return Ok(SyncReport::failure(
                    Problem::new(ProblemKind::Pull, message),
                    report,
                ));
            }
        }
    }
    report.commit = head_commit(&layout.repo);
    let candidate = match build(home, layout, platform) {
        Ok(candidate) => candidate,
        Err(problem) => return Ok(SyncReport::failure(problem, report)),
    };
    report
        .dropped_projects
        .clone_from(&candidate.merged.dropped_projects);
    let live_text = std::fs::read_to_string(home.config_path()).ok();
    if let Err(problem) = smoke(home, exe, &candidate.text, live_text.as_deref()) {
        return Ok(SyncReport::failure(problem, report));
    }
    report.diff = line_diff(live_text.as_deref().unwrap_or(""), &candidate.text);
    report.unchanged = live_text.as_deref() == Some(candidate.text.as_str());
    if options.dry_run {
        return Ok(report);
    }
    write_candidate(
        home,
        layout,
        &candidate,
        report.commit.clone(),
        live_text.as_deref(),
        report.unchanged,
    )?;
    report.written = !report.unchanged;
    Ok(report)
}

fn write_candidate(
    home: &Home,
    layout: &Layout,
    candidate: &Candidate,
    commit: Option<String>,
    live_text: Option<&str>,
    unchanged: bool,
) -> Result<()> {
    if !unchanged {
        if let Some(live) = live_text {
            write_atomic(&home.prev_config_path(), live)?;
        }
        write_atomic(&home.config_path(), &candidate.text)?;
    }
    let origin = origin_file(layout, commit, candidate);
    write_atomic(&home.origin_path(), &serde_json::to_string_pretty(&origin)?)
}

fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let name = path.file_name().map_or_else(
        || "file".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let temp = path.with_file_name(format!("{name}.new"));
    std::fs::write(&temp, text).with_context(|| format!("writing {}", temp.display()))?;
    std::fs::rename(&temp, path)
        .with_context(|| format!("replacing {} with {}", path.display(), temp.display()))
}

fn scratch_failures(
    home: &Home,
    exe: &Path,
    name: &str,
    text: Option<&str>,
) -> Result<Vec<String>> {
    let scratch = home.root().join("config-scratch").join(name);
    if scratch.exists() {
        std::fs::remove_dir_all(&scratch)
            .with_context(|| format!("clearing {}", scratch.display()))?;
    }
    std::fs::create_dir_all(&scratch).with_context(|| format!("creating {}", scratch.display()))?;
    if let Some(text) = text {
        std::fs::write(scratch.join("config.toml"), text)
            .with_context(|| format!("writing the scratch config in {}", scratch.display()))?;
    }
    let checks = SystemBuilds.doctor(exe, &scratch);
    drop(std::fs::remove_dir_all(&scratch));
    Ok(checks?
        .into_iter()
        .filter(|(_, status)| status == "fail")
        .map(|(id, _)| id)
        .collect())
}

/// Runs `doctor --json` on a scratch home with the live config and one with the candidate; a check that fails only with
/// the candidate refuses it.
fn smoke(home: &Home, exe: &Path, candidate: &str, live: Option<&str>) -> Result<(), Problem> {
    let failing = |name: &str, text: Option<&str>| {
        scratch_failures(home, exe, name, text).map_err(|error| {
            Problem::new(
                ProblemKind::Doctor,
                format!("doctor on a scratch home did not run: {error:#}"),
            )
        })
    };
    let before = failing("live", live)?;
    let after = failing("candidate", Some(candidate))?;
    let new: Vec<String> = after
        .into_iter()
        .filter(|id| !before.contains(id))
        .collect();
    if new.is_empty() {
        Ok(())
    } else {
        Err(Problem::new(
            ProblemKind::Doctor,
            format!(
                "doctor fails {} with the candidate config, which the live config passes",
                new.join(", ")
            ),
        ))
    }
}

/// The changed lines of `old` against `new`: `- ` before a removed line, `+ ` before an added one.
#[must_use]
pub fn line_diff(old: &str, new: &str) -> Vec<String> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let mut lcs = vec![vec![0_usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            i += 1;
            j += 1;
        } else if j >= b.len() || (i < a.len() && lcs[i + 1][j] >= lcs[i][j + 1]) {
            out.push(format!("- {}", a[i]));
            i += 1;
        } else {
            out.push(format!("+ {}", b[j]));
            j += 1;
        }
    }
    out
}

/// A line per leaf of the effective config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShownKey {
    /// The dotted key.
    pub key: String,
    /// `shared`, `machine`, `local`, or `default` when the last sync did not write it.
    pub layer: String,
    /// The value as TOML.
    pub value: String,
}

/// The effective config's keys with the layer each came from.
///
/// # Errors
///
/// Fails when `config.toml` is missing or not TOML.
pub fn show_origins(home: &Home) -> Result<Vec<ShownKey>> {
    let path = home.config_path();
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let table: Table =
        toml::from_str(&text).with_context(|| format!("{} is not TOML", path.display()))?;
    let origin = read_origin(home);
    let mut found = BTreeMap::new();
    leaves(&table, &mut Vec::new(), &mut found);
    Ok(found
        .into_iter()
        .map(|(key, value)| ShownKey {
            layer: origin
                .as_ref()
                .and_then(|origin| origin.origins.get(&key))
                .cloned()
                .unwrap_or_else(|| "default".to_owned()),
            key,
            value,
        })
        .collect())
}

/// What `config rollback` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RollbackReport {
    /// Where the replaced file went.
    pub kept_as: PathBuf,
    /// The restored file.
    pub restored: PathBuf,
}

/// Puts `config.toml.prev` back as `config.toml`, keeping the replaced file as `config.toml.rolled-back`.
///
/// # Errors
///
/// Fails when there is no previous file, it is not a valid config, or a file cannot be written.
pub fn rollback(home: &Home, log: Logging<'_>) -> Result<RollbackReport> {
    let prev = home.prev_config_path();
    let text = std::fs::read_to_string(&prev).with_context(|| {
        format!(
            "no previous config at {}; nothing to roll back to",
            prev.display()
        )
    })?;
    Config::parse(&text, &slashed(&prev))?;
    let live = home.config_path();
    let kept = home.root().join("config.toml.rolled-back");
    if live.exists() {
        std::fs::copy(&live, &kept)
            .with_context(|| format!("keeping {} as {}", live.display(), kept.display()))?;
    }
    write_atomic(&live, &text)?;
    log.log(
        EVENT_ROLLED_BACK,
        &serde_json::json!({ "kept_as": slashed(&kept), "restored_from": slashed(&prev) }),
    );
    Ok(RollbackReport {
        kept_as: kept,
        restored: live,
    })
}

/// What `config adopt` takes.
#[derive(Debug, Clone)]
pub struct AdoptOptions {
    /// The machine's name.
    pub machine: String,
    /// The repository folder; the Claude folder when absent.
    pub repo: Option<PathBuf>,
    /// Write the files instead of printing them.
    pub write: bool,
    /// Overwrite existing files.
    pub force: bool,
}

/// What `config adopt` found or wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdoptReport {
    /// The shared file's text.
    pub shared: String,
    /// The machine file's text.
    pub machine: String,
    /// The local file's text.
    pub local: String,
    /// Where each would go, or went.
    pub paths: BTreeMap<String, String>,
    /// Whether the files were written.
    pub written: bool,
}

impl fmt::Display for AdoptReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (name, text) in [
            ("shared", &self.shared),
            ("machine", &self.machine),
            ("local", &self.local),
        ] {
            let path = self.paths.get(name).map_or("", String::as_str);
            writeln!(f, "# {name}: {path}\n{text}")?;
        }
        if let Some(sources) = self.paths.get("sources") {
            writeln!(f, "# sources: {sources}")?;
        }
        if self.written {
            writeln!(
                f,
                "written; merging the three files reproduces the live config"
            )
        } else {
            writeln!(
                f,
                "nothing written; add --write to write these files (merging them reproduces the live config)"
            )
        }
    }
}

#[derive(Default)]
struct Split {
    shared: Table,
    machine: Table,
    local: Table,
}

fn insert_path(table: &mut Table, segments: &[String], value: Value) {
    match segments {
        [] => {}
        [last] => {
            table.insert(last.clone(), value);
        }
        [first, rest @ ..] => {
            let entry = table
                .entry(first.clone())
                .or_insert_with(|| Value::Table(Table::new()));
            if let Value::Table(inner) = entry {
                insert_path(inner, rest, value);
            }
        }
    }
}

fn split(table: &Table, prefix: &mut Vec<String>, out: &mut Split) {
    for (key, value) in table {
        prefix.push(key.clone());
        if let Value::Table(inner) = value
            && !inner.is_empty()
        {
            split(inner, prefix, out);
        } else {
            let path = KeyPath::new(prefix.clone());
            if is_secret(&path) {
                let empty = matches!(value, Value::String(text) if text.is_empty());
                if !empty {
                    insert_path(&mut out.local, prefix, value.clone());
                }
            } else if is_machine_only(&path) {
                insert_path(&mut out.machine, prefix, value.clone());
            } else {
                insert_path(&mut out.shared, prefix, value.clone());
            }
        }
        prefix.pop();
    }
}

fn render(table: &Table) -> Result<String> {
    Ok(toml::to_string(table)?)
}

/// Splits the live `config.toml` into shared, machine and local files, checks that merging them reproduces it, and
/// optionally writes them.
///
/// # Errors
///
/// Fails when `config.toml` is missing or invalid, the merge of the split does not reproduce it, a target file exists
/// without `force`, or a file cannot be written.
pub fn adopt(home: &Home, options: &AdoptOptions, platform: Platform) -> Result<AdoptReport> {
    let repo = options
        .repo
        .clone()
        .unwrap_or_else(|| home.claude_dir().to_path_buf());
    let layout = Layout::new(home, repo, &options.machine)?;
    let live_path = home.config_path();
    let live_text = std::fs::read_to_string(&live_path)
        .with_context(|| format!("reading {}", live_path.display()))?;
    let live = Config::parse(&live_text, &slashed(&live_path))?;
    let table: Table = toml::from_str(&live_text)
        .with_context(|| format!("{} is not TOML", live_path.display()))?;
    let mut parts = Split::default();
    split(&table, &mut Vec::new(), &mut parts);
    let (shared, machine, local) = (
        render(&parts.shared)?,
        render(&parts.machine)?,
        render(&parts.local)?,
    );
    let shared_name = slashed(&layout.shared_file);
    let machine_name = slashed(&layout.machine_file);
    let local_name = slashed(&layout.local_file);
    let merged = merge(
        LayerInput {
            name: LayerName::Shared,
            origin: &shared_name,
            text: &shared,
        },
        LayerInput {
            name: LayerName::Machine,
            origin: &machine_name,
            text: &machine,
        },
        Some(LayerInput {
            name: LayerName::Local,
            origin: &local_name,
            text: &local,
        }),
    )
    .context("the split files do not merge")?;
    let rebuilt = Config::parse(&merged.to_toml_string()?, "the merge of the split files")
        .context("the merge of the split files is not a valid config")?;
    if rebuilt != live {
        bail!(
            "the merge of the split files does not reproduce the live config.toml; nothing was written"
        );
    }
    if !host_suits(rebuilt.machine.host, platform) {
        bail!(
            "machine.host = \"{}\" does not suit this OS; fix config.toml first",
            rebuilt.machine.host.as_str()
        );
    }
    let sources = toml::to_string(&SourcesFile {
        repo: slashed(&layout.repo),
        machine: options.machine.clone(),
    })?;
    let mut paths = BTreeMap::new();
    paths.insert("shared".to_owned(), shared_name);
    paths.insert("machine".to_owned(), machine_name);
    paths.insert("local".to_owned(), local_name);
    paths.insert("sources".to_owned(), slashed(&home.sources_path()));
    let mut files: Vec<(PathBuf, &str)> = vec![
        (layout.shared_file.clone(), &shared),
        (layout.machine_file.clone(), &machine),
        (home.sources_path(), &sources),
    ];
    if !parts.local.is_empty() {
        files.push((layout.local_file, &local));
    }
    if options.write {
        if !options.force
            && let Some((existing, _)) = files.iter().find(|(path, _)| path.exists())
        {
            bail!(
                "{} exists; pass --force to overwrite it",
                existing.display()
            );
        }
        for (path, text) in &files {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
        }
    }
    Ok(AdoptReport {
        shared,
        machine,
        local,
        paths,
        written: options.write,
    })
}

/// Runs a sync after an install or update, without a pull; the one-line outcome, or `None` when not synced. A failure
/// keeps the old config and is reported in the line.
#[must_use]
pub fn after_install(home: &Home, exe: &Path, actor: Actor) -> Option<String> {
    match Layout::load(home) {
        Ok(None) => return None,
        Ok(Some(_)) => {}
        Err(error) => return Some(format!("config sync skipped: {error:#}")),
    }
    let store = home.open_store().ok();
    let log = Logging {
        store: store.as_ref(),
        actor,
    };
    let options = SyncOptions {
        pull: false,
        dry_run: false,
    };
    Some(match sync(home, exe, options, Platform::current(), log) {
        Ok(report) => report.summary(),
        Err(error) => format!("config sync failed, the old config.toml stays: {error:#}"),
    })
}
