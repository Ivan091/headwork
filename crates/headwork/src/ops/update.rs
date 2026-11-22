//! `update`: installs headwork's newest release from GitHub, smoke-tested against a copy of the store first
//! (`docs/autoupdate.md`, "Installing releases: `headwork update`").
//!
//! One update runs at a time, under `<home>/update.lock`; a second one touches `<home>/update.pending` and leaves, and
//! the holder runs again for it. The decision is [`headwork_core::update::decide`]; this module gathers its facts and
//! carries out the step: ask GitHub (through [`ReleaseSource`]), download this system's program and `SHA256SUMS` into
//! `<home>/updates/<tag>/`, check the hash, run both builds' `doctor --json` on a scratch home holding a copy of the
//! config and the store, install through [`install::install`], and hand over to the new build with
//! `update --after <old version>`, which forwards the main checkout and records the install. The old process never
//! opens the store after the swap: the new build may have migrated it.
//!
//! The working memory is `<home>/update.json` ([`UpdateState`]), written atomically; the history is in the event log.
//!
//! What starts an update (`docs/autoupdate.md`, "What starts it") goes through [`trigger`]: the close of a task of
//! `[update] project` (`update --wait 30`), the tick when a poll is due ([`headwork_core::update::poll_due`]) and the
//! orchestrator's MCP tool `update`. Each starts a detached `headwork update` and logs `update-triggered`.
//!
//! A failure worth telling (deterministic at once, transient from its third attempt) raises the standing finding
//! `update-failed`, sends a host notification and leaves a notice for the orchestrator's next prompt
//! ([`take_notices`]); an install that changed plugin files or migrated the store leaves a notice too. Each is told
//! once; a later install clears the finding.

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use headwork_adapters::disk::Disks;
use headwork_adapters::host::TerminalHost;
use headwork_adapters::release::{Fetched, ReleaseError, ReleaseInfo, ReleaseSource, SUMS_NAME};
use headwork_adapters::{proc, timed};
use headwork_core::update::{
    self, BuildLine, Failure, FailureKind, Installed, Release, SkipReason, UpdateFacts, UpdateStep,
    Version,
};
use headwork_core::watch::{DiskLevel, Limits, disk_level};
use headwork_core::{Actor, Config, FindingId};
use headwork_store::{Store, TaskId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::install;
use crate::home::{CLAUDE_JSON_VAR, HOME_VAR, Home};

/// How long the close trigger's update waits for the release of the merge that closed the task, in minutes.
pub const CLOSE_WAIT_MIN: u64 = 30;
/// The folder holding the installed `headwork` unless `--dir` names another.
pub const DEFAULT_DIR: &str = "~/.local/bin";
/// How many notices `update.json` keeps for the orchestrator; older ones are dropped.
pub const KEPT_NOTICES: usize = 10;

/// The lock one update holds while it runs; `headwork install` waits for it too.
pub const LOCK_FILE: &str = "update.lock";

/// How long a run waits for the lock before it reports that an update is running.
///
/// A real update holds it for minutes; the grace covers a lock that was just released but is still shared with a
/// child process between its `fork` and `exec` (an `flock` on unix belongs to the open file, which a forked child
/// shares until `exec` closes it).
pub const LOCK_GRACE: Duration = Duration::from_secs(2);
/// Touched by an update that found the lock taken, so the holder runs again.
pub const PENDING_FILE: &str = "update.pending";
/// The working memory: last poll, `ETag`, the newest release seen, the last failure and result.
pub const STATE_FILE: &str = "update.json";
/// The folder holding one folder of downloads per release.
pub const UPDATES_DIR: &str = "updates";
/// How many release folders are kept under [`UPDATES_DIR`].
pub const KEPT_RELEASES: usize = 3;
/// How long `headwork install` and `update --after` wait for the lock.
pub const LOCK_WAIT: Duration = Duration::from_secs(60);
/// How often `--wait` asks GitHub.
pub const POLL_EVERY: Duration = Duration::from_secs(60);
/// A transient failure is reported (event, `doctor` warning) from this many attempts in a row on.
pub const REPORT_TRANSIENT_FROM: u32 = 3;

/// The lock file, held while an update runs; released when dropped.
#[derive(Debug)]
pub struct UpdateLock {
    _file: File,
}

impl UpdateLock {
    /// Takes the lock when it is free; `None` when another process holds it.
    ///
    /// # Errors
    ///
    /// Fails when the lock file cannot be opened or locked for another reason.
    pub fn try_take(home: &Home) -> Result<Option<Self>> {
        std::fs::create_dir_all(home.root())
            .with_context(|| format!("creating {}", home.root().display()))?;
        let path = home.root().join(LOCK_FILE);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => {
                Err(error).with_context(|| format!("locking {}", path.display()))
            }
        }
    }

    /// Takes the lock, waiting up to `wait` for its holder; `None` when it stayed taken.
    ///
    /// # Errors
    ///
    /// Fails when the lock file cannot be opened or locked for another reason.
    pub fn take_waiting(home: &Home, wait: Duration) -> Result<Option<Self>> {
        let started = Instant::now();
        loop {
            if let Some(lock) = Self::try_take(home)? {
                return Ok(Some(lock));
            }
            if started.elapsed() >= wait {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

/// A failure as `update.json` keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredFailure {
    /// The release tried, when known.
    pub tag: Option<Version>,
    /// Deterministic (latched on the release) or transient (backed off).
    pub kind: FailureKind,
    /// The step that failed: `ask`, `download`, `hash`, `smoke`, `schema` or `install`.
    pub step: String,
    /// Failures in a row of this kind (and release, when deterministic).
    pub attempts: u32,
    /// When, in UTC.
    pub at: String,
    /// What went wrong.
    pub error: String,
}

/// The newest outcome, for `doctor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastResult {
    /// When, in UTC.
    pub at: String,
    /// What happened, in a few words.
    pub outcome: String,
}

/// Something the orchestrator is told at its next prompt, once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    /// When, in UTC.
    pub at: String,
    /// What to tell.
    pub text: String,
}

/// `<home>/update.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateState {
    /// When GitHub was last asked, in UTC.
    #[serde(default)]
    pub last_poll: Option<String>,
    /// The `ETag` of the last answer about the newest release.
    #[serde(default)]
    pub etag: Option<String>,
    /// The newest release seen, used again when GitHub answers "not modified".
    #[serde(default)]
    pub release: Option<ReleaseInfo>,
    /// The last failure; a success clears it.
    #[serde(default)]
    pub last_failure: Option<StoredFailure>,
    /// The newest outcome.
    #[serde(default)]
    pub last_result: Option<LastResult>,
    /// Notices waiting for the orchestrator's next prompt, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<Notice>,
}

impl UpdateState {
    /// Reads `update.json`; a missing or unreadable file is a fresh state.
    #[must_use]
    pub fn load(home: &Home) -> Self {
        std::fs::read_to_string(home.root().join(STATE_FILE))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Writes `update.json` atomically: a temporary file renamed over it.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be written.
    pub fn save(&self, home: &Home) -> Result<()> {
        let path = home.root().join(STATE_FILE);
        let temp = home
            .root()
            .join(format!("{STATE_FILE}.{}.tmp", std::process::id()));
        let mut text = serde_json::to_vec_pretty(self)?;
        text.push(b'\n');
        std::fs::write(&temp, text).with_context(|| format!("writing {}", temp.display()))?;
        std::fs::rename(&temp, &path).with_context(|| format!("replacing {}", path.display()))
    }

    /// The last failure as the decision reads it.
    #[must_use]
    pub fn failure(&self) -> Option<Failure> {
        let stored = self.last_failure.as_ref()?;
        Some(Failure {
            release: stored.tag,
            kind: stored.kind,
            attempts: stored.attempts,
            at_ms: update::parse_utc(&stored.at)?,
        })
    }

    /// Whether the last failure is one the user should see: deterministic (latched), or transient from the third
    /// attempt on.
    #[must_use]
    pub fn failure_to_report(&self) -> Option<&StoredFailure> {
        self.last_failure.as_ref().filter(|failure| {
            failure.kind == FailureKind::Deterministic || failure.attempts >= REPORT_TRANSIENT_FROM
        })
    }

    fn notice(&mut self, now_ms: u64, text: String) {
        self.notices.push(Notice {
            at: update::format_utc(now_ms),
            text,
        });
        let extra = self.notices.len().saturating_sub(KEPT_NOTICES);
        self.notices.drain(..extra);
    }

    fn result(&mut self, now_ms: u64, outcome: impl Into<String>) {
        self.last_result = Some(LastResult {
            at: update::format_utc(now_ms),
            outcome: outcome.into(),
        });
    }
}

/// A program a detached start began.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Started {
    /// Its process id.
    pub pid: u32,
    /// Why it could not leave the enclosing job, when it could not.
    pub note: Option<String>,
}

/// The builds an update runs: the installed one and the downloaded one.
pub trait Builds: fmt::Debug {
    /// What `<program> --version` printed.
    ///
    /// # Errors
    ///
    /// Fails when the program does not run, fails or runs too long.
    fn version(&self, program: &Path) -> Result<String>;

    /// The checks `<program> doctor --json` reports with `HEADWORK_HOME` at `home`, as `(id, status)`.
    ///
    /// # Errors
    ///
    /// Fails when the program does not run or prints no report.
    fn doctor(&self, program: &Path, home: &Path) -> Result<Vec<(String, String)>>;

    /// Starts `<program> <args>` detached, so it outlives this process.
    ///
    /// # Errors
    ///
    /// Fails when the program does not start.
    fn start(&self, program: &Path, args: &[String]) -> Result<Started>;
}

/// The real programs.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemBuilds;

impl Builds for SystemBuilds {
    fn version(&self, program: &Path) -> Result<String> {
        let ran = timed::run(
            Command::new(program).arg("--version"),
            Duration::from_secs(30),
        )?;
        if ran.success() {
            Ok(ran.stdout)
        } else {
            bail!(
                "{} --version failed ({}): {}",
                program.display(),
                ran.status,
                ran.printed()
            )
        }
    }

    fn doctor(&self, program: &Path, home: &Path) -> Result<Vec<(String, String)>> {
        let ran = timed::run(
            Command::new(program)
                .args(["doctor", "--json"])
                .env(HOME_VAR, home)
                .env(CLAUDE_JSON_VAR, home.join(".claude.json")),
            Duration::from_secs(180),
        )?;
        let checks = doctor_checks(&ran.stdout).with_context(|| {
            format!(
                "{} doctor printed no report ({}): {}",
                program.display(),
                ran.status,
                ran.printed()
            )
        })?;
        Ok(checks)
    }

    fn start(&self, program: &Path, args: &[String]) -> Result<Started> {
        let (pid, note) = proc::spawn_detached(Command::new(program).args(args))?;
        Ok(Started { pid, note })
    }
}

/// Reads the checks out of `doctor --json`.
///
/// # Errors
///
/// Fails when the text is not a doctor report.
pub fn doctor_checks(text: &str) -> Result<Vec<(String, String)>> {
    #[derive(Deserialize)]
    struct Report {
        checks: Vec<CheckJson>,
    }
    #[derive(Deserialize)]
    struct CheckJson {
        id: String,
        status: String,
    }
    let report: Report = serde_json::from_str(text.trim())?;
    Ok(report
        .checks
        .into_iter()
        .map(|check| (check.id, check.status))
        .collect())
}

/// Time, which tests replace.
pub trait Clock: fmt::Debug {
    /// Now, in milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;
    /// Waits.
    fn sleep(&self, duration: Duration);
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        millis_of(SystemTime::now())
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

fn millis_of(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since| u64::try_from(since.as_millis()).ok())
        .unwrap_or(0)
}

/// Everything an update acts with.
#[derive(Debug, Clone, Copy)]
pub struct UpdateEnv<'a> {
    /// The home folder.
    pub home: &'a Home,
    /// The machine config.
    pub config: &'a Config,
    /// GitHub, or a fake.
    pub releases: &'a dyn ReleaseSource,
    /// The installed and downloaded programs.
    pub builds: &'a dyn Builds,
    /// Free disk space.
    pub disks: &'a dyn Disks,
    /// Time.
    pub clock: &'a dyn Clock,
    /// The folder holding the installed `headwork`, `~/.local/bin` by default.
    pub install_dir: &'a Path,
    /// This system's release program, such as `headwork-x86_64-pc-windows-msvc.exe`.
    pub asset: &'a str,
    /// Who acts, for the event log.
    pub actor: Actor,
    /// The terminal host, for the notification of a failure; `None` when there is none.
    pub host: Option<&'a dyn TerminalHost>,
}

impl UpdateEnv<'_> {
    /// The installed program.
    #[must_use]
    pub fn installed(&self) -> PathBuf {
        self.install_dir.join(install::program_name())
    }

    fn updates_dir(&self) -> PathBuf {
        self.home.root().join(UPDATES_DIR)
    }

    fn handover_args(&self, mut args: Vec<String>) -> Vec<String> {
        args.push("--dir".to_owned());
        args.push(self.install_dir.display().to_string());
        args
    }
}

/// What `update` was asked to do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UpdateOptions {
    /// Print the decision and change nothing.
    pub dry_run: bool,
    /// Poll once a minute for up to this many minutes until a newer release appears.
    pub wait_min: Option<u64>,
    /// Install this release, newer or older.
    pub to: Option<Version>,
}

/// What `update` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum UpdateReport {
    /// Another update holds the lock; it runs again for this one.
    Pending,
    /// The installed build is the newest release, or none is published.
    UpToDate {
        /// The installed build, as `--version` prints it after the name.
        installed: String,
        /// The newest release's tag, when there is one.
        newest: Option<String>,
    },
    /// Something ruled the update out.
    Skipped {
        /// The short reason word.
        reason: String,
        /// The reason in words.
        detail: String,
    },
    /// A transient failure backs off.
    Waiting {
        /// When the next try may run, in UTC.
        until: String,
    },
    /// A dry run that would install this release.
    WouldInstall {
        /// The release.
        tag: String,
        /// The installed build.
        installed: String,
    },
    /// The release is installed and the new build took over.
    Installed {
        /// The release.
        tag: String,
        /// The build it replaced.
        from: String,
        /// The new build.
        to: String,
        /// The process of `update --after` the new build runs, when it started.
        handed_over: Option<Started>,
        /// Why the hand-over did not start, when it did not.
        handover_error: Option<String>,
    },
    /// The update failed; nothing was installed and the old build runs on.
    Failed {
        /// The release tried, when known.
        tag: Option<String>,
        /// The step that failed.
        step: String,
        /// Deterministic or transient.
        kind: FailureKind,
        /// Failures in a row.
        attempts: u32,
        /// What went wrong.
        error: String,
    },
    /// The release asked for with `--to` cannot be installed.
    Refused {
        /// The release.
        tag: String,
        /// Why.
        reason: String,
    },
}

impl UpdateReport {
    /// Whether the command should exit non-zero.
    #[must_use]
    pub const fn failed(&self) -> bool {
        matches!(self, Self::Failed { .. } | Self::Refused { .. })
    }
}

impl fmt::Display for UpdateReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => f.write_str("an update is running; it runs again when it is done"),
            Self::UpToDate { installed, newest } => match newest {
                Some(newest) => write!(
                    f,
                    "up to date: headwork {installed}, newest release {newest}"
                ),
                None => write!(
                    f,
                    "up to date: headwork {installed}, no release is published"
                ),
            },
            Self::Skipped { detail, .. } => write!(f, "skipped: {detail}"),
            Self::Waiting { until } => write!(
                f,
                "waiting after a transient failure: the next try runs after {until}"
            ),
            Self::WouldInstall { tag, installed } => {
                write!(
                    f,
                    "would install {tag} over headwork {installed} (dry run, nothing changed)"
                )
            }
            Self::Installed {
                tag,
                from,
                to,
                handed_over,
                handover_error,
            } => {
                write!(f, "installed {tag}: headwork {to}, replacing {from}")?;
                match (handed_over, handover_error) {
                    (Some(started), _) => {
                        write!(
                            f,
                            "; the new build finishes the update (pid {})",
                            started.pid
                        )?;
                        if let Some(note) = &started.note {
                            write!(f, ", inside the caller's job: {note}")?;
                        }
                        Ok(())
                    }
                    (None, Some(error)) => write!(
                        f,
                        "; the new build did not start to finish the update ({error}); run `headwork update` again"
                    ),
                    (None, None) => Ok(()),
                }
            }
            Self::Failed {
                tag,
                step,
                kind,
                attempts,
                error,
            } => {
                let release = tag.as_deref().unwrap_or("the newest release");
                write!(
                    f,
                    "update to {release} failed at {step} ({}, attempt {attempts}): {error}; the installed build runs on",
                    kind.as_str()
                )
            }
            Self::Refused { tag, reason } => write!(f, "{tag} is not installed: {reason}"),
        }
    }
}

/// A failure while installing, before it is recorded.
#[derive(Debug)]
struct StepFailure {
    step: &'static str,
    kind: FailureKind,
    error: String,
}

impl StepFailure {
    fn deterministic(step: &'static str, error: impl Into<String>) -> Self {
        Self {
            step,
            kind: FailureKind::Deterministic,
            error: error.into(),
        }
    }

    fn transient(step: &'static str, error: impl Into<String>) -> Self {
        Self {
            step,
            kind: FailureKind::Transient,
            error: error.into(),
        }
    }

    fn of_release(step: &'static str, error: &ReleaseError) -> Self {
        let kind = if error.is_transient() {
            FailureKind::Transient
        } else {
            FailureKind::Deterministic
        };
        Self {
            step,
            kind,
            error: error.to_string(),
        }
    }
}

/// The facts gathered before GitHub is asked.
#[derive(Debug)]
struct Local {
    installed_line: BuildLine,
    facts: UpdateFacts,
    store_schema: u32,
}

/// Runs one update.
///
/// # Errors
///
/// Fails when the installed build cannot be read, the store cannot be opened, or the state cannot be written; a
/// failed download, check or install is a [`UpdateReport::Failed`], not an error.
pub fn update(env: &UpdateEnv<'_>, opts: UpdateOptions) -> Result<UpdateReport> {
    let lock = if opts.dry_run {
        None
    } else {
        let Some(lock) = UpdateLock::take_waiting(env.home, LOCK_GRACE)? else {
            touch(&env.home.root().join(PENDING_FILE))?;
            return Ok(UpdateReport::Pending);
        };
        drop(std::fs::remove_file(env.home.root().join(PENDING_FILE)));
        Some(lock)
    };
    let mut state = UpdateState::load(env.home);
    let mut store = env.home.open_store()?;
    let local = gather(env, &store, &state, opts)?;
    let installed_text = text_of(&local.installed_line);
    let first = update::decide(&local.facts);
    if first != UpdateStep::UpToDate {
        return finish_without_install(
            env,
            &mut store,
            &mut state,
            opts,
            first,
            &installed_text,
            None,
        );
    }
    let repo = env
        .config
        .update_repo()
        .with_context(|| {
            format!(
                "[update] project `{}` is not configured with an allowed_repos entry to take releases from",
                env.config.update.project
            )
        })?
        .as_str()
        .to_owned();
    let mut facts = local.facts;
    let release = match ask(env, &repo, &mut state, opts.to, opts.dry_run) {
        Ok(Some(release)) => release,
        Ok(None) => {
            return finish_without_install(
                env,
                &mut store,
                &mut state,
                opts,
                UpdateStep::UpToDate,
                &installed_text,
                None,
            );
        }
        Err(Asked::Refused(reason)) => {
            let tag = opts.to.map_or_else(String::new, Version::tag);
            return Ok(UpdateReport::Refused { tag, reason });
        }
        Err(Asked::Failed(failure)) => {
            if opts.dry_run {
                bail!("asking GitHub failed: {}", failure.error);
            }
            return record_failure(env, Some(&mut store), &mut state, None, failure);
        }
    };
    let Some(mut known) = known_release(&release) else {
        let failure = StepFailure::deterministic(
            "ask",
            format!("`{}` is not a release tag such as v0.1.7", release.tag),
        );
        if opts.dry_run {
            bail!("{}", failure.error);
        }
        return record_failure(env, Some(&mut store), &mut state, None, failure);
    };
    facts.release = Some(known);
    let mut step = update::decide(&facts);
    if let Some(minutes) = opts.wait_min.filter(|_| opts.to.is_none()) {
        let deadline = env
            .clock
            .now_ms()
            .saturating_add(minutes.saturating_mul(60_000));
        let mut current = release.clone();
        while waits_for_newer(step) && env.clock.now_ms() < deadline {
            env.clock.sleep(POLL_EVERY);
            facts.now_ms = env.clock.now_ms();
            if let Ok(Some(newer)) = ask(env, &repo, &mut state, None, opts.dry_run)
                && let Some(seen) = known_release(&newer)
            {
                known = seen;
                current = newer;
            }
            facts.release = Some(known);
            step = update::decide(&facts);
        }
        return carry_out(env, store, lock, &mut state, opts, &local, step, &current);
    }
    carry_out(env, store, lock, &mut state, opts, &local, step, &release)
}

const fn waits_for_newer(step: UpdateStep) -> bool {
    matches!(
        step,
        UpdateStep::UpToDate | UpdateStep::Skip(SkipReason::DevBuild)
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the update's parts, carried from one step to the next"
)]
fn carry_out(
    env: &UpdateEnv<'_>,
    mut store: Store,
    lock: Option<UpdateLock>,
    state: &mut UpdateState,
    opts: UpdateOptions,
    local: &Local,
    step: UpdateStep,
    release: &ReleaseInfo,
) -> Result<UpdateReport> {
    let installed_text = text_of(&local.installed_line);
    match step {
        UpdateStep::Install { tag } if opts.dry_run => Ok(UpdateReport::WouldInstall {
            tag: tag.tag(),
            installed: installed_text,
        }),
        UpdateStep::Install { tag } => {
            install_release(env, store, lock, state, opts, local, tag, release)
        }
        other @ (UpdateStep::UpToDate | UpdateStep::Skip(_) | UpdateStep::Wait { .. }) => {
            finish_without_install(
                env,
                &mut store,
                state,
                opts,
                other,
                &installed_text,
                Some(&release.tag),
            )
        }
    }
}

fn gather(
    env: &UpdateEnv<'_>,
    store: &Store,
    state: &UpdateState,
    opts: UpdateOptions,
) -> Result<Local> {
    let installed_path = env.installed();
    let printed = env.builds.version(&installed_path).with_context(|| {
        format!(
            "reading the installed build at {}",
            installed_path.display()
        )
    })?;
    let installed_line = BuildLine::parse(&printed).with_context(|| {
        format!(
            "{} --version printed {printed:?}, not `headwork <version> (<commit>, schema <n>)`",
            installed_path.display()
        )
    })?;
    let installed = installed_line.release().map_or_else(
        || Installed::Dev {
            installed_ms: std::fs::metadata(&installed_path)
                .and_then(|meta| meta.modified())
                .map_or(0, millis_of),
        },
        Installed::Release,
    );
    let machine = store.machine_mode()?;
    let store_schema = u32::try_from(store.schema_version()?).unwrap_or(u32::MAX);
    let disk = env
        .disks
        .free_bytes(env.home.root())
        .map_or(DiskLevel::Ok, |free| {
            disk_level(free, &Limits::from_config(&env.config.watch))
        });
    Ok(Local {
        installed_line,
        facts: UpdateFacts {
            enabled: env.config.update.enabled,
            requested: opts.to.is_some(),
            installed,
            release: None,
            last_failure: state.failure(),
            machine,
            disk,
            now_ms: env.clock.now_ms(),
        },
        store_schema,
    })
}

fn text_of(line: &BuildLine) -> String {
    format!("{} ({}, schema {})", line.version, line.commit, line.schema)
}

fn known_release(release: &ReleaseInfo) -> Option<Release> {
    Some(Release {
        version: Version::from_tag(&release.tag)?,
        published_ms: update::parse_utc(&release.published_at).unwrap_or(0),
    })
}

enum Asked {
    Refused(String),
    Failed(StepFailure),
}

/// Asks GitHub for the newest release, or the one `to` names; `None` when none is published.
fn ask(
    env: &UpdateEnv<'_>,
    repo: &str,
    state: &mut UpdateState,
    to: Option<Version>,
    dry_run: bool,
) -> Result<Option<ReleaseInfo>, Asked> {
    if let Some(to) = to {
        return match env.releases.tagged(repo, &to.tag()) {
            Ok(release) => Ok(Some(release)),
            Err(ReleaseError::NotFound { .. }) => Err(Asked::Refused(format!(
                "{repo} has no release {}",
                to.tag()
            ))),
            Err(error) => Err(Asked::Failed(StepFailure::of_release("ask", &error))),
        };
    }
    let etag = state.release.as_ref().and_then(|_| state.etag.clone());
    let fetched = env.releases.latest(repo, etag.as_deref());
    if !dry_run {
        state.last_poll = Some(update::format_utc(env.clock.now_ms()));
    }
    match fetched {
        Ok(Fetched::Fresh { release, etag }) => {
            if !dry_run {
                state.etag = etag;
                state.release = Some(release.clone());
            }
            Ok(Some(release))
        }
        Ok(Fetched::NotModified) => Ok(state.release.clone()),
        Err(ReleaseError::NotFound { .. }) => Ok(None),
        Err(error) => Err(Asked::Failed(StepFailure::of_release("ask", &error))),
    }
}

fn finish_without_install(
    env: &UpdateEnv<'_>,
    store: &mut Store,
    state: &mut UpdateState,
    opts: UpdateOptions,
    step: UpdateStep,
    installed: &str,
    newest: Option<&str>,
) -> Result<UpdateReport> {
    let report = match step {
        UpdateStep::UpToDate => UpdateReport::UpToDate {
            installed: installed.to_owned(),
            newest: newest.map(str::to_owned),
        },
        UpdateStep::Skip(reason) => {
            if reason.worth_logging() && !opts.dry_run {
                store.append_event(
                    env.actor,
                    None,
                    None,
                    "update-skipped",
                    &serde_json::json!({
                        "reason": reason.as_str(),
                        "detail": reason.to_string(),
                        "tag": newest,
                        "installed": installed,
                    }),
                )?;
            }
            UpdateReport::Skipped {
                reason: reason.as_str().to_owned(),
                detail: reason.to_string(),
            }
        }
        UpdateStep::Wait { until } => UpdateReport::Waiting {
            until: update::format_utc(until),
        },
        UpdateStep::Install { tag } => UpdateReport::WouldInstall {
            tag: tag.tag(),
            installed: installed.to_owned(),
        },
    };
    if !opts.dry_run {
        if matches!(report, UpdateReport::UpToDate { .. }) && state.last_failure.is_some() {
            state.last_failure = None;
            store.resolve_findings(FindingId::UpdateFailed, &[], env.actor)?;
        }
        if !matches!(report, UpdateReport::Waiting { .. }) {
            state.result(env.clock.now_ms(), report.to_string());
        }
        state.save(env.home)?;
    }
    Ok(report)
}

fn record_failure(
    env: &UpdateEnv<'_>,
    store: Option<&mut Store>,
    state: &mut UpdateState,
    tag: Option<Version>,
    failure: StepFailure,
) -> Result<UpdateReport> {
    let StepFailure { step, kind, error } = failure;
    let now = env.clock.now_ms();
    let next = update::next_failure(state.failure().as_ref(), tag, kind, now);
    state.last_failure = Some(StoredFailure {
        tag,
        kind,
        step: step.to_owned(),
        attempts: next.attempts,
        at: update::format_utc(now),
        error: error.clone(),
    });
    let data = serde_json::json!({
        "tag": tag.map(Version::tag),
        "step": step,
        "kind": kind.as_str(),
        "attempts": next.attempts,
        "error": error,
    });
    let report = UpdateReport::Failed {
        tag: tag.map(Version::tag),
        step: step.to_owned(),
        kind,
        attempts: next.attempts,
        error,
    };
    state.result(now, report.to_string());
    state.save(env.home)?;
    if kind == FailureKind::Deterministic || next.attempts >= REPORT_TRANSIENT_FROM {
        let told = Told {
            tag,
            kind,
            step,
            data: &data,
        };
        let new = if let Some(store) = store {
            tell_failure(env, store, &told)?
        } else {
            tell_failure(env, &mut env.home.open_store()?, &told)?
        };
        if new {
            state.notice(now, report.to_string());
            state.save(env.home)?;
        }
    }
    Ok(report)
}

/// A failure worth telling, as [`tell_failure`] reports it.
struct Told<'a> {
    tag: Option<Version>,
    kind: FailureKind,
    step: &'a str,
    data: &'a serde_json::Value,
}

/// Logs `update-failed` and raises the standing finding; the first time it stands, also sends a host notification and
/// returns `true`, so the caller leaves a notice for the orchestrator.
fn tell_failure(env: &UpdateEnv<'_>, store: &mut Store, told: &Told<'_>) -> Result<bool> {
    store.append_event(env.actor, None, None, "update-failed", told.data)?;
    let key = failure_key(told.tag, told.kind);
    store.resolve_findings(
        FindingId::UpdateFailed,
        std::slice::from_ref(&key),
        env.actor,
    )?;
    let new = store.raise_finding(&key, FindingId::UpdateFailed, None, told.data, env.actor)?;
    if new {
        let release = told
            .tag
            .map_or_else(|| "the newest release".to_owned(), Version::tag);
        if let Some(host) = env.host {
            drop(host.notify(&format!(
                "headwork: the update to {release} failed at {}; the installed build runs on",
                told.step
            )));
        }
    }
    Ok(new)
}

/// The key of the standing `update-failed` finding: one per failed release, one for transient failures.
fn failure_key(tag: Option<Version>, kind: FailureKind) -> String {
    match (kind, tag) {
        (FailureKind::Deterministic, Some(tag)) => format!("update-failed:{}", tag.tag()),
        (FailureKind::Deterministic, None) => "update-failed:deterministic".to_owned(),
        (FailureKind::Transient, _) => "update-failed:transient".to_owned(),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the update's parts, carried from one step to the next"
)]
fn install_release(
    env: &UpdateEnv<'_>,
    store: Store,
    lock: Option<UpdateLock>,
    state: &mut UpdateState,
    opts: UpdateOptions,
    local: &Local,
    tag: Version,
    release: &ReleaseInfo,
) -> Result<UpdateReport> {
    let from = text_of(&local.installed_line);
    store.append_event(
        env.actor,
        None,
        None,
        "update-started",
        &serde_json::json!({ "tag": tag.tag(), "from": from }),
    )?;
    let downloaded = match fetch_and_check(env, &store, local, tag, release) {
        Ok(downloaded) => downloaded,
        Err(failure) if failure.step == "schema" && opts.to.is_some() => {
            store.append_event(
                env.actor,
                None,
                None,
                "update-failed",
                &serde_json::json!({ "tag": tag.tag(), "step": "schema", "kind": "refused", "error": failure.error }),
            )?;
            return Ok(UpdateReport::Refused {
                tag: tag.tag(),
                reason: failure.error,
            });
        }
        Err(failure) => {
            let mut store = store;
            return record_failure(env, Some(&mut store), state, Some(tag), failure);
        }
    };
    drop(store);
    let builds = env.builds;
    let verify = |target: &Path| -> Result<()> {
        let printed = builds.version(target)?;
        if printed.starts_with("headwork ") {
            Ok(())
        } else {
            bail!("{} --version printed {printed:?}", target.display())
        }
    };
    if let Err(error) = install::install(&downloaded.program, env.install_dir, verify) {
        let failure = StepFailure::deterministic("install", format!("{error:#}"));
        return record_failure(env, None, state, Some(tag), failure);
    }
    let now = env.clock.now_ms();
    state.last_failure = None;
    state.result(
        now,
        format!("installed {}, replacing headwork {from}", tag.tag()),
    );
    state.save(env.home)?;
    prune(&env.updates_dir(), tag);
    drop(lock);
    let args = env.handover_args(vec![
        "update".to_owned(),
        "--after".to_owned(),
        from.clone(),
    ]);
    let (handed_over, handover_error) = match env.builds.start(&env.installed(), &args) {
        Ok(started) => (Some(started), None),
        Err(error) => (None, Some(format!("{error:#}"))),
    };
    Ok(UpdateReport::Installed {
        tag: tag.tag(),
        from,
        to: text_of(&downloaded.line),
        handed_over,
        handover_error,
    })
}

/// A downloaded program that passed its checks.
#[derive(Debug)]
struct Downloaded {
    program: PathBuf,
    line: BuildLine,
}

fn fetch_and_check(
    env: &UpdateEnv<'_>,
    store: &Store,
    local: &Local,
    tag: Version,
    release: &ReleaseInfo,
) -> Result<Downloaded, StepFailure> {
    let dir = env.updates_dir().join(tag.tag());
    std::fs::create_dir_all(&dir).map_err(|error| {
        StepFailure::transient("download", format!("creating {}: {error}", dir.display()))
    })?;
    let asset = release.asset(env.asset).ok_or_else(|| {
        StepFailure::deterministic("download", format!("{} has no {}", release.tag, env.asset))
    })?;
    let sums = release.asset(SUMS_NAME).ok_or_else(|| {
        StepFailure::deterministic("download", format!("{} has no {SUMS_NAME}", release.tag))
    })?;
    let program = dir.join(env.asset);
    let sums_path = dir.join(SUMS_NAME);
    for (url, to) in [(&asset.url, &program), (&sums.url, &sums_path)] {
        env.releases
            .download(url, to)
            .map_err(|error| StepFailure::of_release("download", &error))?;
    }
    check_hash(&program, &sums_path, env.asset)?;
    make_runnable(&program).map_err(|error| StepFailure::transient("download", error))?;
    let printed = env
        .builds
        .version(&program)
        .map_err(|error| StepFailure::deterministic("smoke", format!("{error:#}")))?;
    let line = BuildLine::parse(&printed).ok_or_else(|| {
        StepFailure::deterministic(
            "smoke",
            format!("the downloaded build's --version printed {printed:?}"),
        )
    })?;
    if !update::knows_store(line.schema, local.store_schema) {
        return Err(StepFailure::deterministic(
            "schema",
            format!(
                "it knows store schema {}, older than this store's {}; the store cannot go back",
                line.schema, local.store_schema
            ),
        ));
    }
    smoke(env, store, &dir, &program)?;
    Ok(Downloaded { program, line })
}

fn check_hash(program: &Path, sums: &Path, name: &str) -> Result<(), StepFailure> {
    let text = std::fs::read_to_string(sums).map_err(|error| {
        StepFailure::transient("download", format!("reading {}: {error}", sums.display()))
    })?;
    let expected = expected_hash(&text, name).ok_or_else(|| {
        StepFailure::deterministic("hash", format!("{SUMS_NAME} lists no {name}"))
    })?;
    let actual = sha256_file(program).map_err(|error| {
        StepFailure::transient(
            "download",
            format!("reading {}: {error}", program.display()),
        )
    })?;
    if actual == expected {
        Ok(())
    } else {
        Err(StepFailure::deterministic(
            "hash",
            format!("{name} has SHA-256 {actual}, {SUMS_NAME} says {expected}"),
        ))
    }
}

/// The hash `SHA256SUMS` (as `sha256sum` writes it: `<hex>  <name>`, or `<hex> *<name>`) lists for `name`, in lower
/// case.
#[must_use]
pub fn expected_hash(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        let file = file.trim_start();
        let file = file.strip_prefix('*').unwrap_or(file);
        (file == name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

/// The SHA-256 of a file, in lower-case hex.
///
/// # Errors
///
/// Fails when the file cannot be read.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        let Some(chunk) = buffer.get(..read).filter(|chunk| !chunk.is_empty()) else {
            break;
        };
        hasher.update(chunk);
    }
    let mut hex = String::with_capacity(64);
    for byte in &hasher.finalize() {
        let _infallible: fmt::Result = fmt::Write::write_fmt(&mut hex, format_args!("{byte:02x}"));
    }
    Ok(hex)
}

#[cfg(unix)]
fn make_runnable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|error| format!("making {} runnable: {error}", path.display()))
}

#[cfg(not(unix))]
#[expect(clippy::unnecessary_wraps, reason = "unix needs the result")]
const fn make_runnable(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Runs both builds' `doctor --json` on a scratch home holding the live config and a copy of the store; a check the
/// downloaded build fails that the installed one does not fail too fails the smoke test.
fn smoke(
    env: &UpdateEnv<'_>,
    store: &Store,
    dir: &Path,
    program: &Path,
) -> Result<(), StepFailure> {
    let scratch = dir.join("smoke");
    let io = |error: std::io::Error| {
        StepFailure::transient("smoke", format!("preparing {}: {error}", scratch.display()))
    };
    if scratch.exists() {
        std::fs::remove_dir_all(&scratch).map_err(io)?;
    }
    std::fs::create_dir_all(&scratch).map_err(io)?;
    let config = env.home.config_path();
    if config.exists() {
        std::fs::copy(&config, scratch.join("config.toml")).map_err(io)?;
    }
    if env.home.root().join("state.db").exists() {
        store.copy_to(&scratch.join("state.db")).map_err(|error| {
            StepFailure::transient("smoke", format!("copying the store: {error}"))
        })?;
    }
    let failing = |program: &Path| -> Result<Vec<String>, StepFailure> {
        let checks = env
            .builds
            .doctor(program, &scratch)
            .map_err(|error| StepFailure::deterministic("smoke", format!("{error:#}")))?;
        Ok(checks
            .into_iter()
            .filter(|(_, status)| status == "fail")
            .map(|(id, _)| id)
            .collect())
    };
    let before = failing(&env.installed())?;
    let after = failing(program)?;
    let new: Vec<String> = after
        .into_iter()
        .filter(|id| !before.contains(id))
        .collect();
    if !new.is_empty() {
        return Err(StepFailure::deterministic(
            "smoke",
            format!(
                "its doctor fails {} on a copy of this machine's config and store, which the installed build passes; the copy is in {}",
                new.join(", "),
                super::slashed(&scratch)
            ),
        ));
    }
    drop(std::fs::remove_dir_all(&scratch));
    Ok(())
}

/// Deletes all but the newest [`KEPT_RELEASES`] release folders, never the one just installed; other files stay.
fn prune(dir: &Path, installed: Version) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut folders: Vec<(Version, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter_map(|path| {
            let version = Version::from_tag(path.file_name()?.to_str()?)?;
            Some((version, path))
        })
        .collect();
    folders.sort_by_key(|(version, _)| std::cmp::Reverse(*version));
    for (version, path) in folders.into_iter().skip(KEPT_RELEASES) {
        if version != installed {
            drop(std::fs::remove_dir_all(path));
        }
    }
}

fn touch(path: &Path) -> Result<()> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map(drop)
        .with_context(|| format!("touching {}", path.display()))
}

/// What happened to the main checkout after an install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "checkout", rename_all = "kebab-case")]
pub enum Forward {
    /// Fast-forwarded to the release's commit.
    Forwarded {
        /// The commit it was at.
        from: String,
        /// The release's commit.
        to: String,
        /// Whether files under `plugin/` changed, which sessions started before pick up only after park and resume.
        plugin_changed: bool,
    },
    /// It already holds the release's commit.
    AlreadyThere,
    /// Left as it is; the next update tries again.
    NotForwarded {
        /// Why.
        reason: String,
    },
}

/// What `update --after` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AfterReport {
    /// The build replaced.
    pub from: String,
    /// This build.
    pub to: String,
    /// The main checkout.
    pub checkout: Forward,
    /// The fresh `headwork update` started because another was asked for meanwhile or a newer release appeared.
    pub again: Option<Started>,
}

impl fmt::Display for AfterReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "headwork {} installed, replacing {}", self.to, self.from)?;
        match &self.checkout {
            Forward::Forwarded {
                to, plugin_changed, ..
            } => {
                write!(f, "; the main checkout is at {to}")?;
                if *plugin_changed {
                    f.write_str(
                        "; plugin files changed: sessions started before keep the old hooks until park and resume",
                    )?;
                }
            }
            Forward::AlreadyThere => {
                f.write_str("; the main checkout already holds the release")?;
            }
            Forward::NotForwarded { reason } => {
                write!(f, "; the main checkout is left as it is: {reason}")?;
            }
        }
        if let Some(again) = &self.again {
            write!(f, "; another update runs (pid {})", again.pid)?;
        }
        Ok(())
    }
}

/// What `update --after` runs as: the newly installed build, its version line and commit.
#[derive(Debug, Clone, Copy)]
pub struct ThisBuild<'a> {
    /// `--version` after the name.
    pub text: &'a str,
    /// The release version, `None` for a local build.
    pub release: Option<Version>,
    /// The commit, `None` for a local build.
    pub commit: Option<&'a str>,
}

/// The second half of an update, run by the newly installed build.
///
/// It forwards the project's main checkout to the release's commit (best effort), records `update-installed`, and
/// starts a fresh update when one was asked for meanwhile or a newer release appeared.
///
/// # Errors
///
/// Fails when the lock stays taken, the store cannot be opened or the state cannot be written.
pub fn after(env: &UpdateEnv<'_>, from: &str, this: ThisBuild<'_>) -> Result<AfterReport> {
    let Some(lock) = UpdateLock::take_waiting(env.home, LOCK_WAIT)? else {
        bail!(
            "{} stayed taken for {} s; run `headwork update` again",
            env.home.root().join(LOCK_FILE).display(),
            LOCK_WAIT.as_secs()
        );
    };
    let mut store = env.home.open_store()?;
    let checkout = forward_checkout(env.home, env.config, this.commit);
    if let Forward::NotForwarded { reason } = &checkout {
        store.append_event(
            env.actor,
            None,
            None,
            "checkout-not-forwarded",
            &serde_json::json!({ "commit": this.commit, "reason": reason }),
        )?;
    }
    let tag = this.release.map(Version::tag);
    store.append_event(
        env.actor,
        None,
        None,
        "update-installed",
        &serde_json::json!({ "tag": tag, "from": from, "to": this.text, "checkout": checkout }),
    )?;
    store.resolve_findings(FindingId::UpdateFailed, &[], env.actor)?;
    drop(store);
    let mut state = UpdateState::load(env.home);
    if let Some(text) = install_notice(from, this, &checkout, env.home) {
        state.notice(env.clock.now_ms(), text);
    }
    let pending = env.home.root().join(PENDING_FILE);
    let asked = pending.exists();
    drop(std::fs::remove_file(&pending));
    let newer = !asked && newer_release(env, &mut state, this.release);
    state.save(env.home)?;
    drop(lock);
    let again = if asked || newer {
        env.builds
            .start(
                &env.installed(),
                &env.handover_args(vec!["update".to_owned()]),
            )
            .ok()
    } else {
        None
    };
    Ok(AfterReport {
        from: from.to_owned(),
        to: this.text.to_owned(),
        checkout,
        again,
    })
}

/// What the orchestrator is told after an install: only when plugin files changed (sessions started before keep the old
/// hooks) or the new build migrated the store.
fn install_notice(
    from: &str,
    this: ThisBuild<'_>,
    checkout: &Forward,
    home: &Home,
) -> Option<String> {
    let name = this
        .release
        .map_or_else(|| format!("headwork {}", this.text), Version::tag);
    let mut parts = Vec::new();
    if matches!(
        checkout,
        Forward::Forwarded {
            plugin_changed: true,
            ..
        }
    ) {
        parts.push(format!(
            "it changed plugin files: sessions started before {name} keep the old hooks until park and resume"
        ));
    }
    let schema = |text: &str| BuildLine::parse(&format!("headwork {text}")).map(|line| line.schema);
    if let (Some(old), Some(new)) = (schema(from), schema(this.text))
        && new > old
    {
        parts.push(format!(
            "it migrated the store from schema {old} to {new} (a copy of the old one is in {})",
            super::slashed(&home.root().join("backups"))
        ));
    }
    (!parts.is_empty()).then(|| format!("{name} is installed; {}.", parts.join("; ")))
}

/// Takes the notices waiting for the orchestrator's next prompt, oldest first, so each is told once.
///
/// Empty when none wait, or while an update holds the lock: they wait for the prompt after.
///
/// # Errors
///
/// Fails when the lock or `update.json` cannot be handled.
pub fn take_notices(home: &Home) -> Result<Vec<String>> {
    if UpdateState::load(home).notices.is_empty() {
        return Ok(Vec::new());
    }
    let Some(_lock) = UpdateLock::try_take(home)? else {
        return Ok(Vec::new());
    };
    let mut state = UpdateState::load(home);
    let notices = std::mem::take(&mut state.notices);
    if !notices.is_empty() {
        state.save(home)?;
    }
    Ok(notices.into_iter().map(|notice| notice.text).collect())
}

/// What started an update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TriggeredBy {
    /// A task of `[update] project` closed.
    Close,
    /// The tick found a poll due.
    Tick,
    /// The orchestrator's MCP tool `update`.
    Orchestrator,
}

impl TriggeredBy {
    /// The word in the `update-triggered` event.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Close => "close",
            Self::Tick => "tick",
            Self::Orchestrator => "orchestrator",
        }
    }
}

/// Starts a detached `headwork update`; tests replace it.
pub trait UpdateStarter: fmt::Debug {
    /// Starts `headwork <args>` detached, so it outlives the caller.
    ///
    /// # Errors
    ///
    /// Fails when the program does not start.
    fn start(&self, args: &[String]) -> Result<Started>;
}

/// The real start: `exe` with `HEADWORK_HOME` at the home, in the home folder, detached.
#[derive(Debug, Clone, Copy)]
pub struct Detached<'a> {
    /// The home folder the update works on.
    pub home: &'a Home,
    /// The `headwork` program, the one finishers run.
    pub exe: &'a Path,
}

impl UpdateStarter for Detached<'_> {
    fn start(&self, args: &[String]) -> Result<Started> {
        let mut command = Command::new(self.exe);
        command
            .args(args)
            .env(HOME_VAR, self.home.root())
            .current_dir(self.home.root());
        let (pid, note) = proc::spawn_detached(&mut command)
            .with_context(|| format!("starting {} {}", self.exe.display(), args.join(" ")))?;
        Ok(Started { pid, note })
    }
}

/// A detached update a trigger started, or tried to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Triggered {
    /// What started it.
    pub by: TriggeredBy,
    /// The arguments after `headwork`.
    pub args: Vec<String>,
    /// The process, when it started.
    pub started: Option<Started>,
    /// Why it did not start, when it did not.
    pub error: Option<String>,
}

/// Starts a detached `headwork <args>` for `by` and logs `update-triggered`; a start that fails is recorded in the
/// result and the event, not returned as an error.
///
/// # Errors
///
/// Fails only when the event cannot be logged.
pub fn trigger(
    store: &Store,
    via: &dyn UpdateStarter,
    actor: Actor,
    task: Option<TaskId>,
    by: TriggeredBy,
    args: Vec<String>,
) -> Result<Triggered> {
    let (started, error) = match via.start(&args) {
        Ok(process) => (Some(process), None),
        Err(error) => (None, Some(format!("{error:#}"))),
    };
    let triggered = Triggered {
        by,
        args,
        started,
        error,
    };
    store.append_event(
        actor,
        task,
        None,
        "update-triggered",
        &serde_json::json!({
            "by": by.as_str(),
            "args": triggered.args,
            "pid": triggered.started.as_ref().map(|started| started.pid),
            "note": triggered.started.as_ref().and_then(|started| started.note.clone()),
            "error": triggered.error,
        }),
    )?;
    Ok(triggered)
}

/// The arguments of the close trigger's update: wait up to [`CLOSE_WAIT_MIN`] minutes for the merge's release.
#[must_use]
pub fn close_args() -> Vec<String> {
    vec![
        "update".to_owned(),
        "--wait".to_owned(),
        CLOSE_WAIT_MIN.to_string(),
    ]
}

/// The tick's poll facts, from `update.json`, the lock, the machine mode and the disk.
///
/// # Errors
///
/// Fails when the store or the lock cannot be read.
pub fn poll_facts(
    home: &Home,
    config: &Config,
    store: &Store,
    disk: DiskLevel,
    now_ms: u64,
) -> Result<update::PollFacts> {
    let state = UpdateState::load(home);
    let running = UpdateLock::try_take(home)?.is_none();
    Ok(update::PollFacts {
        enabled: config.update.enabled,
        poll_min: config.update.poll_min,
        last_poll_ms: state.last_poll.as_deref().and_then(update::parse_utc),
        last_failure: state.failure(),
        machine: store.machine_mode()?,
        disk,
        running,
        now_ms,
    })
}

/// Now, in milliseconds since the Unix epoch, by the system clock.
#[must_use]
pub fn now_ms() -> u64 {
    millis_of(SystemTime::now())
}

fn newer_release(env: &UpdateEnv<'_>, state: &mut UpdateState, this: Option<Version>) -> bool {
    let Some(repo) = env.config.update_repo() else {
        return false;
    };
    match env.releases.latest(repo.as_str(), state.etag.as_deref()) {
        Ok(Fetched::Fresh { release, etag }) => {
            let newer = Version::from_tag(&release.tag)
                .zip(this)
                .is_some_and(|(seen, this)| seen > this);
            state.last_poll = Some(update::format_utc(env.clock.now_ms()));
            state.etag = etag;
            state.release = Some(release);
            newer
        }
        Ok(Fetched::NotModified) | Err(_) => false,
    }
}

const GIT_TIMEOUT: Duration = Duration::from_secs(90);

/// Fast-forwards `[update] project`'s main checkout to `commit`.
///
/// It runs `git fetch`, then `git merge --ff-only`, without prompts and with a timeout. A checkout off `master`, dirty,
/// ahead of the release or with `index.lock` taken is left alone.
#[must_use]
pub fn forward_checkout(home: &Home, config: &Config, commit: Option<&str>) -> Forward {
    let not = |reason: String| Forward::NotForwarded { reason };
    let Some(commit) = commit else {
        return not("a local build names no release commit".to_owned());
    };
    if commit.is_empty() || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return not(format!("`{commit}` is not a commit id"));
    }
    let Some(project) = config.project(&config.update.project) else {
        return not(format!(
            "project `{}` is not configured",
            config.update.project
        ));
    };
    let main = home.expand(&project.main);
    let git = |args: &[&str]| -> Result<timed::Ran, String> {
        timed::run(
            Command::new("git")
                .args(args)
                .current_dir(&main)
                .env("GIT_TERMINAL_PROMPT", "0")
                .env(
                    "GIT_SSH_COMMAND",
                    "ssh -o BatchMode=yes -o ConnectTimeout=10",
                ),
            GIT_TIMEOUT,
        )
        .map_err(|error| error.to_string())
    };
    let out = |args: &[&str]| -> Result<String, String> {
        let ran = git(args)?;
        if ran.success() {
            Ok(ran.stdout.trim().to_owned())
        } else {
            Err(format!("git {} failed: {}", args.join(" "), ran.printed()))
        }
    };
    match forward_steps(&main, commit, &git, &out) {
        Ok(forward) | Err(forward) => forward,
    }
}

type GitRun<'a> = dyn Fn(&[&str]) -> Result<timed::Ran, String> + 'a;
type GitOut<'a> = dyn Fn(&[&str]) -> Result<String, String> + 'a;

fn forward_steps(
    main: &Path,
    commit: &str,
    git: &GitRun<'_>,
    out: &GitOut<'_>,
) -> Result<Forward, Forward> {
    let not = |reason: String| Forward::NotForwarded { reason };
    if !main.join(".git").exists() {
        return Err(not(format!("{} is not a git checkout", main.display())));
    }
    let branch = out(&["symbolic-ref", "--short", "-q", "HEAD"])
        .map_err(|_| not("HEAD is detached".to_owned()))?;
    if branch != "master" {
        return Err(not(format!("it is on `{branch}`, not master")));
    }
    let git_dir = out(&["rev-parse", "--git-dir"]).map_err(not)?;
    if main.join(git_dir).join("index.lock").exists() {
        return Err(not("index.lock is taken".to_owned()));
    }
    if !out(&["status", "--porcelain", "--untracked-files=no"])
        .map_err(not)?
        .is_empty()
    {
        return Err(not("it has uncommitted changes".to_owned()));
    }
    let head = out(&["rev-parse", "HEAD"]).map_err(not)?;
    if git(&["merge-base", "--is-ancestor", commit, "HEAD"]).is_ok_and(|ran| ran.success()) {
        return Ok(Forward::AlreadyThere);
    }
    out(&["fetch", "--quiet", "origin"]).map_err(not)?;
    let ahead = out(&["rev-list", "--count", &format!("{commit}..HEAD")])
        .map_err(|_| not(format!("the release commit {commit} is not in origin")))?;
    if ahead != "0" {
        return Err(not(format!(
            "it has {ahead} local commits the release does not"
        )));
    }
    out(&["merge", "--ff-only", "--quiet", commit]).map_err(not)?;
    let plugin_changed =
        !git(&["diff", "--quiet", &head, commit, "--", "plugin"]).is_ok_and(|ran| ran.success());
    Ok(Forward::Forwarded {
        from: head,
        to: commit.to_owned(),
        plugin_changed,
    })
}

/// What `doctor` says about updates: on or off, this build, the newest release seen and the last result; a warning
/// for a latched failure or a transient one from its third attempt on.
#[must_use]
pub fn doctor_summary(home: &Home, config: &Config) -> (bool, String) {
    let state = UpdateState::load(home);
    let mut parts = vec![if config.update.enabled {
        format!(
            "on for project `{}`, polled every {} min by the tick",
            config.update.project, config.update.poll_min
        )
    } else {
        "off ([update] enabled = false)".to_owned()
    }];
    parts.push(format!("this build {}", crate::version::text()));
    if let Some(release) = &state.release {
        parts.push(format!("newest release seen {}", release.tag));
    }
    if let Some(result) = &state.last_result {
        parts.push(format!("last result at {}: {}", result.at, result.outcome));
    }
    if let Some(failure) = state.failure_to_report() {
        parts.push(format!(
            "failing: {} at {} ({}, attempt {}): {}",
            failure
                .tag
                .map_or_else(|| "the newest release".to_owned(), Version::tag),
            failure.step,
            failure.kind.as_str(),
            failure.attempts,
            failure.error
        ));
    }
    (state.failure_to_report().is_some(), parts.join("; "))
}
