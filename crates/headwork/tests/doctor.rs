//! `doctor` and `shadow-report` against a temporary home, fake probes and fake disks.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::unnecessary_wraps,
    reason = "tests fail loudly on an unexpected error, and the host factory has a fixed signature"
)]

use std::path::{Path, PathBuf};

use headwork::home::Home;
use headwork::ops::doctor::{Doctor, DoctorReport, Probe, Status, doctor};
use headwork::ops::shadow::shadow_report;
use headwork_adapters::disk::FakeDisks;
use headwork_adapters::host::TerminalHost;
use headwork_adapters::host::fake::FakeHost;
use headwork_core::config::HooksConfig;
use headwork_core::{Actor, Config, Role};
use headwork_store::{EVENT_WOULD_DENY, NewSession};
use serde_json::json;

const GIB: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Default)]
struct FakeProbe {
    missing: Vec<&'static str>,
    claude: Vec<u32>,
}

impl Probe for FakeProbe {
    fn run(&self, program: &str, args: &[&str]) -> Option<String> {
        if self.missing.contains(&program) {
            return None;
        }
        Some(match (program, args.first().copied()) {
            ("gh", _) => "Logged in to github.com account someone".to_owned(),
            ("claude", Some("plugin")) => "headwork@headwork enabled".to_owned(),
            _ => "2.1.325 (Claude Code)".to_owned(),
        })
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        (!self.missing.contains(&program)).then(|| PathBuf::from("/bin").join(program))
    }

    fn pids_named(&self, _image: &str) -> Option<Vec<u32>> {
        Some(self.claude.clone())
    }
}

fn fake_host(_config: &Config) -> anyhow::Result<Box<dyn TerminalHost>> {
    Ok(Box::new(FakeHost::new()))
}

struct Install {
    dir: tempfile::TempDir,
    home: Home,
    claude: PathBuf,
    repo: PathBuf,
    disks: FakeDisks,
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn hooks_json(events: &[(&str, &str)]) -> String {
    let mut hooks = serde_json::Map::new();
    for (claude, word) in events {
        hooks.insert(
            (*claude).to_owned(),
            json!([{ "hooks": [{ "type": "command", "command": format!("\"${{HEADWORK_EXE:-true}}\" hook {word}") }] }]),
        );
    }
    json!({ "hooks": hooks }).to_string()
}

const ALL_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "user-prompt-submit"),
    ("PreToolUse", "pre-tool-use"),
    ("PostToolUse", "post-tool-use"),
    ("Stop", "stop"),
    ("SessionEnd", "session-end"),
];

fn install() -> Install {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("headwork-home");
    let home = Home::at(Some(root.clone()))
        .unwrap()
        .with_claude_json(dir.path().join(".claude.json"));
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/config.toml");
    write(
        &root.join("config.toml"),
        &std::fs::read_to_string(fixture).unwrap(),
    );
    drop(home.open_store().unwrap());
    let repo = dir.path().join("repo");
    write(&repo.join(".claude-plugin/marketplace.json"), "{}");
    write(&repo.join("plugin/.claude-plugin/plugin.json"), "{}");
    write(
        &repo.join("plugin/hooks/hooks.json"),
        &hooks_json(ALL_EVENTS),
    );
    let claude = dir.path().join("claude");
    let settings = json!({
        "enabledPlugins": { "headwork@headwork": true },
        "extraKnownMarketplaces": {
            "headwork": { "source": { "source": "directory", "path": repo.display().to_string() } }
        }
    });
    write(&claude.join("settings.json"), &settings.to_string());
    let disks = FakeDisks::new();
    disks.set(dir.path(), 100 * GIB);
    Install {
        dir,
        home,
        claude,
        repo,
        disks,
    }
}

fn run(install: &Install, probe: &FakeProbe) -> DoctorReport {
    let exe = std::env::current_exe().unwrap();
    doctor(&Doctor {
        home: &install.home,
        exe: &exe,
        claude_dir: &install.claude,
        probe,
        disks: &install.disks,
        host: fake_host,
    })
}

fn status(report: &DoctorReport, id: &str) -> Status {
    report
        .checks
        .iter()
        .find(|check| check.id == id)
        .unwrap_or_else(|| panic!("no check `{id}` in {:?}", report.checks))
        .status
}

#[test]
fn a_complete_install_is_healthy() {
    let install = install();
    let report = run(&install, &FakeProbe::default());
    assert!(report.healthy(), "{:?}", report.checks);
    assert_eq!(status(&report, "plugin"), Status::Ok, "plugin");
    assert_eq!(status(&report, "hooks"), Status::Ok, "hooks");
    assert_eq!(status(&report, "enforce"), Status::Ok, "enforce");
    assert_eq!(
        status(&report, "orchestrator"),
        Status::Warn,
        "no orchestrator session yet"
    );
}

#[test]
fn a_missing_plugin_is_a_problem_with_a_fix() {
    let install = install();
    std::fs::remove_file(install.claude.join("settings.json")).unwrap();
    let report = run(&install, &FakeProbe::default());
    assert!(!report.healthy(), "no plugin must fail");
    let check = report.checks.iter().find(|c| c.id == "plugin").unwrap();
    assert_eq!(check.status, Status::Fail, "plugin");
    assert!(
        check.fix.as_deref().unwrap().contains("enabledPlugins"),
        "{check:?}"
    );
}

#[test]
fn a_hooks_file_that_lacks_an_event_fails() {
    let install = install();
    write(
        &install.repo.join("plugin/hooks/hooks.json"),
        &hooks_json(&ALL_EVENTS[..5]),
    );
    let report = run(&install, &FakeProbe::default());
    let check = report.checks.iter().find(|c| c.id == "hooks").unwrap();
    assert_eq!(check.status, Status::Fail, "hooks");
    assert!(check.detail.contains("SessionEnd"), "{}", check.detail);
}

#[test]
fn gh_and_claude_missing_fail_and_a_missing_path_entry_warns() {
    let install = install();
    let probe = FakeProbe {
        missing: vec!["gh", "claude", "headwork"],
        claude: Vec::new(),
    };
    let report = run(&install, &probe);
    assert_eq!(status(&report, "gh"), Status::Fail, "gh");
    assert_eq!(status(&report, "claude"), Status::Fail, "claude");
    assert_eq!(status(&report, "path"), Status::Warn, "path");
    assert!(!report.healthy(), "problems make the report unhealthy");
}

#[test]
fn little_free_disk_warns_and_almost_none_fails() {
    let install = install();
    install.disks.set(install.dir.path(), 10 * GIB);
    assert_eq!(
        status(&run(&install, &FakeProbe::default()), "disk"),
        Status::Warn,
        "below the warn line"
    );
    install.disks.set(install.dir.path(), GIB);
    assert_eq!(
        status(&run(&install, &FakeProbe::default()), "disk"),
        Status::Fail,
        "below the stop line"
    );
}

#[test]
fn a_wsl_shell_fails_and_the_default_does_not() {
    let install = install();
    let report = run(&install, &FakeProbe::default());
    assert_ne!(status(&report, "shell"), Status::Fail, "the default shell");
    let config = install.home.root().join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    let wsl = text.replacen(
        "[machine]\n",
        "[machine]\nshell = \"C:/Windows/System32/bash.exe\"\n",
        1,
    );
    assert_ne!(wsl, text, "the fixture has a [machine] table");
    write(&config, &wsl);
    let report = run(&install, &FakeProbe::default());
    assert_eq!(status(&report, "shell"), Status::Fail, "WSL's bash");
}

#[test]
fn a_missing_tick_warns_only_when_enforcing() {
    let install = install();
    let report = run(&install, &FakeProbe::default());
    assert_eq!(status(&report, "tick"), Status::Ok, "shadow mode");
    let config = install.home.root().join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    write(&config, &format!("{text}\n[hooks]\nenforce = true\n"));
    let report = run(&install, &FakeProbe::default());
    assert_eq!(status(&report, "tick"), Status::Warn, "enforcing, no tick");
    install.home.open_store().unwrap().record_tick().unwrap();
    let report = run(&install, &FakeProbe::default());
    assert_eq!(status(&report, "tick"), Status::Ok, "a tick just ran");
}

#[test]
fn an_orchestrator_session_in_the_store_satisfies_the_orchestrator_check() {
    let install = install();
    let mut store = install.home.open_store().unwrap();
    store
        .record_session(
            &NewSession {
                id: "root-1".to_owned(),
                task_id: None,
                role: Role::Orchestrator,
                rc_name: "root".to_owned(),
                host: "wmux".to_owned(),
                host_workspace: None,
                host_surface: None,
            },
            Actor::User,
        )
        .unwrap();
    store.session_launched("root-1", 10, 11, "job").unwrap();
    drop(store);
    let probe = FakeProbe {
        missing: Vec::new(),
        claude: vec![11, 99],
    };
    let report = run(&install, &probe);
    assert_eq!(status(&report, "orchestrator"), Status::Ok, "root");
    let processes = report.checks.iter().find(|c| c.id == "processes").unwrap();
    assert!(
        processes.detail.contains("2 ") && processes.detail.contains(": 99"),
        "{}",
        processes.detail
    );
}

#[test]
fn the_shadow_report_groups_would_denies_by_rule() {
    let install = install();
    let store = install.home.open_store().unwrap();
    for (rule, input) in [
        ("no_main_commit", "git commit -m a"),
        ("no_main_commit", "git commit -m b"),
        ("long_jobs", "cargo run --release -p render"),
    ] {
        store
            .append_event(
                Actor::Hook,
                None,
                Some("s1"),
                EVENT_WOULD_DENY,
                &json!({ "hook": "PreToolUse", "tool": "Bash", "rule": rule, "reason": "r", "input": input }),
            )
            .unwrap();
    }
    let report = shadow_report(&store, &HooksConfig::default(), None).unwrap();
    assert_eq!(report.would_deny, 3, "total");
    assert_eq!(
        report.rules[0].rule, "no_main_commit",
        "most frequent first"
    );
    assert_eq!(report.rules[0].count, 2, "count");
    assert_eq!(
        report.rules[0].samples[0].input, "git commit -m b",
        "newest sample first"
    );
    assert_eq!(report.rules[1].rule, "long_jobs", "second rule");
    assert_eq!(report.sessions, 1, "sessions");
}

#[test]
fn an_ignored_config_key_is_flagged() {
    let install = install();
    let path = install.home.config_path();
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        text.replacen(
            "notify_ntfy = \"\"",
            "notify_ntfy = \"https://ntfy.sh/x\"",
            1,
        ),
    )
    .unwrap();
    let report = run(&install, &FakeProbe::default());
    assert_eq!(status(&report, "unenforced-keys"), Status::Warn, "ntfy set");
}

#[test]
fn doctor_trusts_every_project_main_checkout_and_reports_it() {
    let install = install();
    let file = install.home.claude_json().to_path_buf();
    write(
        &file,
        r#"{"projects":{"S:/Code/me-sim/me-sim":{"hasTrustDialogAccepted":true}}}"#,
    );
    let report = run(&install, &FakeProbe::default());
    let check = report.checks.iter().find(|c| c.id == "trust").unwrap();
    assert_eq!(check.status, Status::Ok, "{check:?}");
    assert!(
        check.detail.contains("S:/Code/me-studio/me-studio"),
        "{check:?}"
    );
    assert!(
        !check.detail.contains("trusted now: S:/Code/me-sim/me-sim"),
        "{check:?}"
    );
    let state: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let projects = state["projects"].as_object().unwrap();
    assert_eq!(
        projects.len(),
        3,
        "one key per project: {:?}",
        projects.keys()
    );
    assert!(
        projects
            .values()
            .all(|p| p["hasTrustDialogAccepted"] == true),
        "all trusted"
    );
    let again = run(&install, &FakeProbe::default());
    let check = again.checks.iter().find(|c| c.id == "trust").unwrap();
    assert!(
        check
            .detail
            .starts_with("all 3 project folders are trusted"),
        "{check:?}"
    );
}

#[test]
fn a_broken_claude_state_file_warns_without_its_contents() {
    let install = install();
    write(
        install.home.claude_json(),
        r#"{"oauthAccount":"secret" broken"#,
    );
    let report = run(&install, &FakeProbe::default());
    let check = report.checks.iter().find(|c| c.id == "trust").unwrap();
    assert_eq!(check.status, Status::Warn, "{check:?}");
    assert!(!format!("{check:?}").contains("secret"), "{check:?}");
}

fn log_would_deny(store: &headwork_store::Store, rule: &str) {
    store
        .append_event(
            Actor::Hook,
            None,
            Some("s1"),
            EVENT_WOULD_DENY,
            &json!({ "hook": "Stop", "tool": "", "rule": rule, "reason": "r", "input": "x" }),
        )
        .unwrap();
}

#[test]
fn the_shadow_report_reads_only_the_window_since() {
    let install = install();
    let store = install.home.open_store().unwrap();
    log_would_deny(&store, "stop-unfinished");
    let hooks = HooksConfig::default();
    let all = shadow_report(&store, &hooks, None).unwrap();
    assert_eq!((all.would_deny, all.since), (1, None), "no window");
    let past = shadow_report(&store, &hooks, Some("2020-01-01")).unwrap();
    assert_eq!(past.would_deny, 1, "a plain date");
    assert_eq!(
        past.since.as_deref(),
        Some("2020-01-01T00:00:00.000Z"),
        "normalized"
    );
    let offset = shadow_report(&store, &hooks, Some("2020-01-01T02:00:00+02:00")).unwrap();
    assert_eq!(
        offset.since.as_deref(),
        Some("2020-01-01T00:00:00.000Z"),
        "RFC 3339 offset"
    );
    let recent = shadow_report(&store, &hooks, Some("24h")).unwrap();
    assert_eq!(recent.would_deny, 1, "a relative span");
    let future = shadow_report(&store, &hooks, Some("2999-01-01")).unwrap();
    assert_eq!(
        (future.events, future.would_deny),
        (0, 0),
        "nothing before the window"
    );
    assert!(
        shadow_report(&store, &hooks, Some("lately")).is_err(),
        "not a time"
    );
}

#[test]
fn the_shadow_report_marks_the_rules_that_enforce() {
    let install = install();
    let store = install.home.open_store().unwrap();
    log_would_deny(&store, "stop-unfinished");
    log_would_deny(&store, "child-edit-outside");
    let hooks = Config::parse(
        "[machine]\nhost = \"wmux\"\n[hooks]\nenforce_rules = [\"worker-edit-outside\"]\n",
        "t",
    )
    .unwrap()
    .hooks;
    let report = shadow_report(&store, &hooks, None).unwrap();
    let enforced: Vec<(&str, bool)> = report
        .rules
        .iter()
        .map(|rule| (rule.rule.as_str(), rule.enforced))
        .collect();
    assert_eq!(
        enforced,
        [("stop-unfinished", false), ("worker-edit-outside", true)],
        "old spelling reads as the new rule"
    );
    assert_eq!(report.enforce_rules, ["worker-edit-outside"], "listed");
}

#[test]
fn doctor_names_the_rules_that_enforce() {
    let install = install();
    let config = install.home.root().join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    write(
        &config,
        &format!("{text}\n[hooks]\nenforce_rules = [\"stop-unfinished\"]\n"),
    );
    let report = run(&install, &FakeProbe::default());
    let check = report.checks.iter().find(|c| c.id == "enforce").unwrap();
    assert_eq!(check.status, Status::Ok, "{check:?}");
    assert!(check.detail.contains("stop-unfinished"), "{check:?}");
}
