//! `doctor`: checks that the install works and says what to do about what does not (design section 8).
//!
//! Every check is a [`Check`] with a status; only a [`Status::Fail`] makes the command exit non-zero. What the
//! machine offers (programs, the process list, the terminal host, disks) comes in through [`Probe`] and the other
//! arguments of [`Doctor`], so the checks run against fakes in tests.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;
use headwork_adapters::disk::Disks;
use headwork_adapters::host::TerminalHost;
use headwork_adapters::trust::Trust;
use headwork_adapters::{proc, shell};
use headwork_core::{Actor, Config};
use headwork_store::{EVENT_WOULD_DENY, EventFilter, MIGRATIONS, Store};
use serde::Serialize;
use serde_json::Value;

use super::install::Stamp;
use super::reload::{self, Freshness};
use super::slashed;
use crate::home::Home;

/// The plugin's name in a marketplace entry `headwork@<marketplace>`.
pub const PLUGIN_NAME: &str = "headwork";

/// The events the plugin must hook, as `headwork hook <event>` spells them.
const HOOK_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "user-prompt-submit"),
    ("PreToolUse", "pre-tool-use"),
    ("PostToolUse", "post-tool-use"),
    ("Stop", "stop"),
    ("SessionEnd", "session-end"),
];

/// How a check came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Fine.
    Ok,
    /// Worth knowing; nothing is broken.
    Warn,
    /// Broken: the install does not work as it should.
    Fail,
}

/// One thing doctor looked at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    /// A short stable name.
    pub id: &'static str,
    /// How it came out.
    pub status: Status,
    /// What was found.
    pub detail: String,
    /// What to do about it, when it is not fine.
    pub fix: Option<String>,
}

impl Check {
    fn ok(id: &'static str, detail: impl Into<String>) -> Self {
        Self {
            id,
            status: Status::Ok,
            detail: detail.into(),
            fix: None,
        }
    }

    fn warn(id: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            id,
            status: Status::Warn,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    fn fail(id: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            id,
            status: Status::Fail,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
}

/// Everything doctor found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    /// The checks, in the order they ran.
    pub checks: Vec<Check>,
}

impl DoctorReport {
    /// How many checks have this status.
    #[must_use]
    pub fn count(&self, status: Status) -> usize {
        self.checks
            .iter()
            .filter(|check| check.status == status)
            .count()
    }

    /// Whether no check failed.
    #[must_use]
    pub fn healthy(&self) -> bool {
        self.count(Status::Fail) == 0
    }
}

/// What doctor asks the machine: programs and processes.
pub trait Probe: fmt::Debug {
    /// Runs a program and returns what it printed when it exited successfully.
    fn run(&self, program: &str, args: &[&str]) -> Option<String>;
    /// Where a program is on `PATH`.
    fn which(&self, program: &str) -> Option<PathBuf>;
    /// The ids of the processes running this image.
    fn pids_named(&self, image: &str) -> Option<Vec<u32>>;
}

/// The real machine.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProbe;

impl Probe for SystemProbe {
    fn run(&self, program: &str, args: &[&str]) -> Option<String> {
        let output = Command::new(program).args(args).output().ok()?;
        output.status.success().then(|| {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            text
        })
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        let names = [program.to_owned(), format!("{program}.exe")];
        std::env::split_paths(&path)
            .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
            .find(|candidate| candidate.is_file())
    }

    fn pids_named(&self, image: &str) -> Option<Vec<u32>> {
        proc::pids_named(image).ok()
    }
}

/// How to build the terminal host once the config is known.
pub type HostFactory = fn(&Config) -> Result<Box<dyn TerminalHost>>;

/// What the checks look at.
#[derive(Debug, Clone, Copy)]
pub struct Doctor<'a> {
    /// The headwork home.
    pub home: &'a Home,
    /// This program.
    pub exe: &'a Path,
    /// The Claude config folder, holding `settings.json`.
    pub claude_dir: &'a Path,
    /// Programs and processes.
    pub probe: &'a dyn Probe,
    /// Free disk space.
    pub disks: &'a dyn Disks,
    /// The terminal host.
    pub host: HostFactory,
}

/// Runs every check.
#[must_use]
pub fn doctor(d: &Doctor<'_>) -> DoctorReport {
    let mut checks = vec![binary(d), on_path(d)];
    let config = d.home.load_config();
    match &config {
        Ok(config) => checks.push(Check::ok(
            "config",
            format!(
                "{} projects, host {}, {}",
                config.projects.len(),
                config.machine.host.as_str(),
                if config.hooks.enforce {
                    "enforcing"
                } else {
                    "shadow mode"
                }
            ),
        )),
        Err(error) => checks.push(Check::fail(
            "config",
            format!("{error:#}"),
            "run `headwork import` to write config.toml from the orchestrate projects.conf, or write one as in docs/design.md section 5",
        )),
    }
    if config.is_ok() {
        checks.push(deprecated_keys_check(d));
        if let Ok(config) = &config {
            checks.push(unenforced_keys_check(config));
        }
    }
    let store = store_check(d, &mut checks);
    if let Ok(config) = &config {
        checks.push(host_check(d, config));
        checks.push(shell_check(config));
    }
    checks.push(gh_check(d));
    checks.push(claude_check(d));
    checks.extend(plugin_checks(d, config.as_ref().ok()));
    if let Ok(config) = &config {
        checks.push(enforce_check(config));
        checks.push(disk_check(d, config));
    }
    if let Ok(config) = &config {
        checks.push(trust_check(d, config, store.as_ref()));
    }
    if let Some(store) = &store {
        checks.extend(store_checks(d, store));
        let enforce = config.as_ref().is_ok_and(|config| config.hooks.enforce);
        checks.push(tick_check(store, enforce));
    }
    DoctorReport { checks }
}

/// Trusts every project's main checkout in Claude Code's state file, so no worker stops at the trust prompt, and
/// logs each change when the store opened.
fn trust_check(d: &Doctor<'_>, config: &Config, store: Option<&Store>) -> Check {
    let found = super::trust::ensure_all(d.home, config);
    let mut granted = Vec::new();
    let mut failed = Vec::new();
    for one in &found {
        if let Some(store) = store {
            drop(super::trust::log(store, d.home, Actor::User, None, one));
        }
        match &one.outcome {
            Ok(Trust::Already) => {}
            Ok(Trust::Granted) => granted.push(one.key.clone()),
            Err(error) => failed.push(format!("{}: {error}", one.project)),
        }
    }
    if !failed.is_empty() {
        return Check::warn(
            "trust",
            failed.join("; "),
            format!(
                "fix {} or trust the folder once by starting `claude` in it; a worker stopped at the prompt is raised as an ask",
                d.home.claude_json().display()
            ),
        );
    }
    let detail = if granted.is_empty() {
        format!("all {} project folders are trusted", found.len())
    } else {
        format!(
            "{} project folders trusted; trusted now: {}",
            found.len(),
            granted.join(", ")
        )
    };
    Check::ok("trust", detail)
}

fn unenforced_keys_check(config: &Config) -> Check {
    let mut keys = Vec::new();
    if !config.machine.notify_ntfy.is_empty() {
        keys.push("machine.notify_ntfy".to_owned());
    }
    keys.extend(
        config
            .projects
            .iter()
            .filter(|(_, project)| project.heavy_slots.is_some())
            .map(|(name, _)| format!("project.{}.heavy_slots", name.as_str())),
    );
    if keys.is_empty() {
        return Check::ok("unenforced-keys", "no configured key is ignored");
    }
    Check::warn(
        "unenforced-keys",
        format!(
            "set but not enforced yet (recorded and informational only): {}",
            keys.join(", ")
        ),
        "nothing to do; remove them or leave them as documentation (docs/decisions.md, 2026-11-16)",
    )
}

fn deprecated_keys_check(d: &Doctor<'_>) -> Check {
    let text = std::fs::read_to_string(d.home.config_path()).unwrap_or_default();
    let renamed = Config::deprecated_keys(&text);
    if renamed.is_empty() {
        return Check::ok("config-keys", "no deprecated keys");
    }
    let list: Vec<String> = renamed
        .iter()
        .map(|(old, new)| format!("{old} (now {new})"))
        .collect();
    Check::warn(
        "config-keys",
        format!("deprecated keys, still read: {}", list.join(", ")),
        "rename them in config.toml",
    )
}

fn binary(d: &Doctor<'_>) -> Check {
    let version = env!("CARGO_PKG_VERSION");
    if d.exe.is_file() {
        Check::ok(
            "binary",
            format!("headwork {version} at {}", slashed(d.exe)),
        )
    } else {
        Check::fail(
            "binary",
            format!("the running program {} is gone", slashed(d.exe)),
            "rebuild it with `cargo build --release -p headwork` and run that one",
        )
    }
}

fn on_path(d: &Doctor<'_>) -> Check {
    d.probe.which("headwork").map_or_else(
        || {
            Check::warn(
                "path",
                "`headwork` is not on PATH; sessions started by headwork use HEADWORK_EXE and are not affected",
                format!(
                    "copy {} to a folder on PATH, such as ~/.local/bin/headwork{}",
                    slashed(d.exe),
                    std::env::consts::EXE_SUFFIX
                ),
            )
        },
        |found| {
            Check::ok(
                "path",
                format!("`headwork` resolves to {}", slashed(&found)),
            )
        },
    )
}

fn store_check(d: &Doctor<'_>, checks: &mut Vec<Check>) -> Option<Store> {
    let path = d.home.root().join("state.db");
    if !path.exists() {
        checks.push(Check::warn(
            "store",
            format!("{} does not exist yet", slashed(&path)),
            "any command that reads the store, such as `headwork board`, creates it",
        ));
        return None;
    }
    match d.home.open_store() {
        Ok(store) => {
            let latest = i64::try_from(MIGRATIONS.len()).unwrap_or(i64::MAX);
            match store.schema_version() {
                Ok(found) if found == latest => checks.push(Check::ok(
                    "store",
                    format!("{} at schema {found}", slashed(&path)),
                )),
                Ok(found) => checks.push(Check::fail(
                    "store",
                    format!("schema {found}, this build knows {latest}"),
                    "open it with a headwork build at least as new as the one that wrote it",
                )),
                Err(error) => checks.push(Check::fail(
                    "store",
                    format!("{error}"),
                    "restore state.db from a copy, or move it away to start a new store",
                )),
            }
            Some(store)
        }
        Err(error) => {
            checks.push(Check::fail(
                "store",
                format!("{error:#}"),
                "if the schema is newer than this build, use a newer headwork; otherwise move state.db away to start a new store",
            ));
            None
        }
    }
}

const SHELL_FIX: &str =
    "set `shell = \"C:/Program Files/Git/bin/bash.exe\"` under [machine] in config.toml";

fn shell_check(config: &Config) -> Check {
    let configured = config.machine.shell.as_deref();
    let shell = shell::resolve(configured);
    let shown = slashed(&shell);
    let source = if configured.is_some() {
        "configured"
    } else {
        "default"
    };
    if shell::is_wsl_bash(&shell) {
        return Check::fail(
            "shell",
            format!(
                "the {source} shell {shown} is WSL's bash: its git has no SSH keys, so worktree scripts hang"
            ),
            SHELL_FIX,
        );
    }
    if cfg!(windows) && shell::is_bare(&shell) {
        return Check::warn(
            "shell",
            format!(
                "the {source} shell is a bare `{shown}`, which Windows may resolve to WSL's System32/bash.exe"
            ),
            SHELL_FIX,
        );
    }
    if !shell::is_bare(&shell) && !shell.is_file() {
        return Check::fail(
            "shell",
            format!("the {source} shell {shown} does not exist"),
            SHELL_FIX,
        );
    }
    Check::ok("shell", format!("{source} shell {shown}"))
}

fn host_check(d: &Doctor<'_>, config: &Config) -> Check {
    let reply = (d.host)(config).and_then(|host| host.ping().map_err(anyhow::Error::from));
    match reply {
        Ok(()) => Check::ok("host", format!("{} answers", config.machine.host.as_str())),
        Err(error) => Check::fail(
            "host",
            format!(
                "{} does not answer: {error:#}",
                config.machine.host.as_str()
            ),
            "start wmux, or set `machine.host_bin` in config.toml to its CLI",
        ),
    }
}

fn gh_check(d: &Doctor<'_>) -> Check {
    d.probe.run("gh", &["auth", "status"]).map_or_else(
        || {
            Check::fail(
                "gh",
                "`gh auth status` failed: gh is missing or logged out",
                "run `gh auth login`; pull request state in the tick needs it",
            )
        },
        |text| {
            Check::ok(
                "gh",
                text.lines()
                    .find(|line| line.contains("Logged in"))
                    .map_or("logged in", str::trim)
                    .to_owned(),
            )
        },
    )
}

fn claude_check(d: &Doctor<'_>) -> Check {
    d.probe.run("claude", &["--version"]).map_or_else(
        || {
            Check::fail(
                "claude",
                "`claude --version` failed",
                "install Claude Code and put it on PATH",
            )
        },
        |text| Check::ok("claude", text.trim().to_owned()),
    )
}

/// What `settings.json` says about the plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Installed {
    marketplace: String,
    enabled: bool,
    root: Option<PathBuf>,
}

fn installed_plugin(settings: &Value) -> Option<Installed> {
    let enabled = settings.get("enabledPlugins")?.as_object()?;
    let prefix = format!("{PLUGIN_NAME}@");
    let (key, value) = enabled.iter().find(|(key, _)| key.starts_with(&prefix))?;
    let marketplace = key.strip_prefix(&prefix)?.to_owned();
    let root = settings
        .get("extraKnownMarketplaces")
        .and_then(|known| known.get(&marketplace))
        .and_then(|entry| entry.get("source"))
        .and_then(|source| source.get("path"))
        .and_then(Value::as_str)
        .map(PathBuf::from);
    Some(Installed {
        marketplace,
        enabled: value.as_bool().unwrap_or(false),
        root,
    })
}

const INSTALL_FIX: &str = concat!(
    "add `\"extraKnownMarketplaces\": {\"headwork\": {\"source\": {\"source\": \"directory\", \"path\": \"<repo>\"}}}` ",
    "and `\"enabledPlugins\": {\"headwork@headwork\": true}` to ~/.claude/settings.json, ",
    "or set `hooks.plugin_dir` in config.toml"
);

fn plugin_checks(d: &Doctor<'_>, config: Option<&Config>) -> Vec<Check> {
    let settings = std::fs::read_to_string(d.claude_dir.join("settings.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let installed = settings.as_ref().and_then(installed_plugin);
    let launcher_dir = config
        .and_then(|config| config.hooks.plugin_dir.as_deref())
        .map(|dir| d.home.expand(dir));
    let mut checks = Vec::new();
    let plugin_dir = match (&installed, &launcher_dir) {
        (Some(installed), launcher) if installed.enabled => {
            let Some(root) = &installed.root else {
                checks.push(Check::fail(
                    "plugin",
                    format!(
                        "enabled as {PLUGIN_NAME}@{} but the marketplace has no directory source in settings.json",
                        installed.marketplace
                    ),
                    INSTALL_FIX,
                ));
                return checks;
            };
            let plugin = root.join("plugin");
            let manifest = plugin.join(".claude-plugin/plugin.json").is_file();
            let market = root.join(".claude-plugin/marketplace.json").is_file();
            if manifest && market {
                checks.push(Check::ok(
                    "plugin",
                    format!(
                        "enabled as {PLUGIN_NAME}@{} from {}",
                        installed.marketplace,
                        slashed(root)
                    ),
                ));
            } else {
                checks.push(Check::fail(
                    "plugin",
                    format!(
                        "{} lacks plugin/.claude-plugin/plugin.json or .claude-plugin/marketplace.json",
                        slashed(root)
                    ),
                    "check out a headwork revision that has them (milestone 7 or later)",
                ));
            }
            if launcher.is_some() {
                checks.push(Check::warn(
                    "plugin-twice",
                    "the plugin is installed in settings.json and also named by hooks.plugin_dir: hooks would run twice",
                    "remove `plugin_dir` from [hooks] in config.toml",
                ));
            }
            Some(plugin)
        }
        (_, Some(dir)) => {
            checks.push(Check::ok(
                "plugin",
                format!(
                    "loaded by the launcher from {} (managed sessions only)",
                    slashed(dir)
                ),
            ));
            Some(dir.clone())
        }
        (Some(_), None) => {
            checks.push(Check::fail(
                "plugin",
                "settings.json lists the plugin but disabled",
                "set `\"headwork@headwork\": true` in enabledPlugins",
            ));
            None
        }
        (None, None) => {
            checks.push(Check::fail(
                "plugin",
                "no session loads the plugin: it is not in settings.json and hooks.plugin_dir is unset",
                INSTALL_FIX,
            ));
            None
        }
    };
    if let Some(plugin) = plugin_dir {
        checks.push(hooks_check(&plugin));
    }
    if installed.is_some_and(|installed| installed.enabled) {
        checks.push(loaded_check(d));
    }
    checks
}

fn hooks_check(plugin: &Path) -> Check {
    let path = plugin.join("hooks/hooks.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Check::fail(
            "hooks",
            format!("{} is missing", slashed(&path)),
            "check out a headwork revision that has plugin/hooks/hooks.json",
        );
    };
    let Ok(json) = serde_json::from_str::<Value>(&text) else {
        return Check::fail(
            "hooks",
            format!("{} is not valid JSON", slashed(&path)),
            "restore it from git",
        );
    };
    let missing: Vec<&str> = HOOK_EVENTS
        .iter()
        .filter(|(claude, word)| {
            !json["hooks"][claude]
                .to_string()
                .contains(&format!("hook {word}"))
        })
        .map(|(claude, _)| *claude)
        .collect();
    if missing.is_empty() {
        Check::ok(
            "hooks",
            format!("{} events call `headwork hook`", HOOK_EVENTS.len()),
        )
    } else {
        Check::fail(
            "hooks",
            format!("no `headwork hook` command for {}", missing.join(", ")),
            "restore plugin/hooks/hooks.json from git",
        )
    }
}

fn loaded_check(d: &Doctor<'_>) -> Check {
    match d.probe.run("claude", &["plugin", "list"]) {
        Some(text) if text.contains(PLUGIN_NAME) => {
            Check::ok("plugin-loaded", "`claude plugin list` shows headwork")
        }
        Some(_) => Check::warn(
            "plugin-loaded",
            "`claude plugin list` does not show headwork",
            "start a new session; if it still does not show, check the marketplace path in settings.json",
        ),
        None => Check::warn(
            "plugin-loaded",
            "`claude plugin list` did not run",
            "update Claude Code",
        ),
    }
}

fn enforce_check(config: &Config) -> Check {
    if !config.hooks.enforce && !config.hooks.enforce_rules.is_empty() {
        let rules: Vec<&str> = config
            .hooks
            .enforce_rules
            .iter()
            .map(|rule| rule.as_str())
            .collect();
        Check::ok(
            "enforce",
            format!(
                "hooks.enforce = false with enforce_rules: denies by {} block the call, every other rule only logs would-deny events",
                rules.join(", ")
            ),
        )
    } else if config.hooks.enforce {
        Check::ok(
            "enforce",
            "hooks.enforce = true: denies block the call; the tick closes and removes",
        )
    } else {
        Check::ok(
            "enforce",
            "hooks.enforce = false: shadow mode, hooks only log would-deny events (`headwork shadow-report`)",
        )
    }
}

fn disk_check(d: &Doctor<'_>, config: &Config) -> Check {
    let mut probe = d.home.root();
    while !probe.exists() {
        match probe.parent() {
            Some(parent) => probe = parent,
            None => break,
        }
    }
    match d.disks.free_bytes(probe) {
        Ok(bytes) => {
            let gb = bytes / (1024 * 1024 * 1024);
            let stop = u64::from(config.watch.disk_stop_gb);
            let warn = u64::from(config.watch.disk_warn_gb);
            let text = format!("{gb} GB free on the volume of {}", slashed(probe));
            if gb < stop {
                Check::fail(
                    "disk",
                    text,
                    format!(
                        "free space below disk_stop_gb = {stop}: the queue stays paused; delete renders or caches"
                    ),
                )
            } else if gb < warn {
                Check::warn(
                    "disk",
                    text,
                    format!(
                        "free space below disk_warn_gb = {warn}; delete renders or caches soon"
                    ),
                )
            } else {
                Check::ok("disk", text)
            }
        }
        Err(error) => Check::warn(
            "disk",
            format!("cannot read free space of {}: {error}", slashed(probe)),
            "check the volume",
        ),
    }
}

fn store_checks(d: &Doctor<'_>, store: &Store) -> Vec<Check> {
    let mut checks = Vec::new();
    checks.push(match store.live_orchestrator_session() {
        Ok(Some(root)) => Check::ok(
            "orchestrator",
            format!("orchestrator session {} is in the store", root.rc_name),
        ),
        Ok(None) => Check::warn(
            "orchestrator",
            "no live orchestrator session in the store: the orchestrator runs unmanaged",
            "start the orchestrator with `headwork orchestrator`",
        ),
        Err(error) => Check::warn("orchestrator", format!("{error}"), "check the store"),
    });
    checks.push(orphans(d, store));
    checks.push(builds(store));
    checks.push(shadow_count(store));
    checks
}

/// A tick older than this counts as not running; the tick is meant to run every minute.
const TICK_STALE_S: u64 = 300;

fn tick_check(store: &Store, enforce: bool) -> Check {
    let age = match store.last_tick_age_s() {
        Ok(age) => age,
        Err(error) => return Check::warn("tick", format!("{error}"), "check the store"),
    };
    let when = age.map_or_else(
        || "no tick has run".to_owned(),
        |age| format!("the last tick ran {} min ago", age / 60),
    );
    match age {
        Some(age) if age <= TICK_STALE_S => Check::ok("tick", when),
        _ if enforce => Check::warn(
            "tick",
            format!(
                "{when}: with enforcement on, nothing moves merged tasks, interrupted sessions or the queue"
            ),
            "schedule `headwork tick` every minute (CLAUDE.md, Scheduling the tick)",
        ),
        _ => Check::ok(
            "tick",
            format!(
                "{when}; nothing schedules it in shadow mode, which is expected until enforcement is turned on"
            ),
        ),
    }
}

fn builds(store: &Store) -> Check {
    match store.live_sessions_with_event(reload::EVENT_BUILD) {
        Ok(live) => {
            let sessions = reload::classify(&live, |exe| Stamp::of(exe).ok());
            let detail = reload::summary(&sessions);
            if reload::named(&sessions, Freshness::Pinned).is_empty() {
                Check::ok("builds", detail)
            } else {
                Check::warn("builds", detail, reload::PINNED_FIX)
            }
        }
        Err(error) => Check::warn("builds", format!("{error}"), "check the store"),
    }
}

const fn claude_image() -> &'static str {
    if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    }
}

fn orphans(d: &Doctor<'_>, store: &Store) -> Check {
    let Some(running) = d.probe.pids_named(claude_image()) else {
        return Check::warn(
            "processes",
            "the process list could not be read",
            "check that `tasklist` (Windows) or `pgrep` runs",
        );
    };
    let tracked: Vec<u32> = store
        .live_session_pids()
        .unwrap_or_default()
        .into_iter()
        .flat_map(|(_, claude, launcher)| [claude, launcher])
        .flatten()
        .collect();
    let untracked: Vec<String> = running
        .iter()
        .filter(|pid| !tracked.contains(pid))
        .map(ToString::to_string)
        .collect();
    Check::ok(
        "processes",
        format!(
            "{} {} processes, {} tracked by the store; untracked (unmanaged sessions or leftovers; Windows gives no parent ids \
             without PowerShell, so none can be called an orphan): {}",
            running.len(),
            claude_image(),
            running.len() - untracked.len(),
            if untracked.is_empty() {
                "none".to_owned()
            } else {
                untracked.join(", ")
            }
        ),
    )
}

fn shadow_count(store: &Store) -> Check {
    match store.events(EventFilter {
        task: None,
        limit: u32::MAX,
    }) {
        Ok(events) => {
            let count = events
                .iter()
                .filter(|event| event.kind == EVENT_WOULD_DENY)
                .count();
            Check::ok(
                "shadow",
                format!(
                    "{count} would-deny events in the log of {} events; `headwork shadow-report` lists them by rule",
                    events.len()
                ),
            )
        }
        Err(error) => Check::warn("shadow", format!("{error}"), "check the store"),
    }
}
