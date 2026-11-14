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
    let home = Home::at(Some(root.clone())).unwrap();
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
    let report = shadow_report(&store, false).unwrap();
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
