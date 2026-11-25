//! `headwork config` against a temporary `HEADWORK_HOME` and a temporary config repository.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixture() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/config.toml");
    let host = if cfg!(target_os = "macos") {
        "cmux"
    } else {
        "wmux"
    };
    std::fs::read_to_string(path)
        .unwrap()
        .replace("host = \"wmux\"", &format!("host = \"{host}\""))
        .replace(
            "notify_ntfy = \"\"",
            "notify_ntfy = \"https://ntfy.example/secret-topic\"",
        )
}

fn headwork(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_headwork"))
        .args(args)
        .env("HEADWORK_HOME", home)
        .env("HEADWORK_CLAUDE_JSON", home.join(".claude.json"))
        .env("HEADWORK_CLAUDE_DIR", home.join(".claude"))
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

struct Synced {
    home: tempfile::TempDir,
    repo: tempfile::TempDir,
}

impl Synced {
    fn config(&self) -> PathBuf {
        self.home.path().join("config.toml")
    }

    fn machine_file(&self) -> PathBuf {
        self.repo.path().join("machines/test/headwork.toml")
    }

    fn run(&self, args: &[&str]) -> Output {
        headwork(self.home.path(), args)
    }

    fn commit_repo(&self, message: &str) {
        git(self.repo.path(), &["add", "-A"]);
        git(self.repo.path(), &["commit", "-q", "-m", message]);
    }
}

fn adopted() -> Synced {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), fixture()).unwrap();
    git(repo.path(), &["init", "-q"]);
    let repo_arg = repo.path().to_str().unwrap().to_owned();
    let output = headwork(
        home.path(),
        &[
            "config",
            "adopt",
            "--machine",
            "test",
            "--repo",
            &repo_arg,
            "--write",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let synced = Synced { home, repo };
    synced.commit_repo("adopt");
    synced
}

#[test]
fn every_command_but_adopt_says_the_machine_is_not_synced() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), fixture()).unwrap();
    for args in [
        &["config", "check"][..],
        &["config", "sync", "--no-pull"],
        &["config", "show"],
        &["config", "rollback"],
    ] {
        let output = headwork(home.path(), args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let text = format!("{}{}", stdout(&output), stderr(&output));
        assert!(text.contains("not synced"), "{args:?}: {text}");
    }
}

#[test]
fn adopt_splits_the_config_and_the_split_round_trips() {
    let synced = adopted();
    let shared = std::fs::read_to_string(synced.repo.path().join("headwork/config.toml")).unwrap();
    let machine = std::fs::read_to_string(synced.machine_file()).unwrap();
    let local = std::fs::read_to_string(synced.home.path().join("config.local.toml")).unwrap();
    assert!(shared.contains("[models]"), "{shared}");
    assert!(!shared.contains("main = "), "no main in shared: {shared}");
    assert!(
        !shared.contains("secret-topic"),
        "no secret in shared: {shared}"
    );
    assert!(
        machine.contains("main = \"S:/Code/me-sim/me-sim\""),
        "{machine}"
    );
    assert!(machine.contains("host = "), "{machine}");
    assert!(
        !machine.contains("secret-topic"),
        "no secret in machine: {machine}"
    );
    assert!(local.contains("secret-topic"), "{local}");
    let sources = std::fs::read_to_string(synced.home.path().join("sources.toml")).unwrap();
    assert!(sources.contains("machine = \"test\""), "{sources}");
    let check = synced.run(&["config", "check", "--json"]);
    assert!(
        check.status.success(),
        "{}{}",
        stdout(&check),
        stderr(&check)
    );
    let report: serde_json::Value = serde_json::from_str(&stdout(&check)).unwrap();
    assert_eq!(report["ok"], true, "{report}");
}

#[test]
fn adopt_without_write_prints_and_changes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), fixture()).unwrap();
    let repo_arg = repo.path().to_str().unwrap();
    let output = headwork(
        home.path(),
        &["config", "adopt", "--machine", "test", "--repo", repo_arg],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("nothing written"),
        "{}",
        stdout(&output)
    );
    assert!(
        !home.path().join("sources.toml").exists(),
        "no sources.toml"
    );
    assert!(
        !repo.path().join("headwork").exists(),
        "no repository files"
    );
}

#[test]
fn adopt_refuses_to_overwrite_without_force() {
    let synced = adopted();
    let repo_arg = synced.repo.path().to_str().unwrap().to_owned();
    let args = [
        "config",
        "adopt",
        "--machine",
        "test",
        "--repo",
        &repo_arg,
        "--write",
    ];
    let refused = synced.run(&args);
    assert_eq!(refused.status.code(), Some(1), "{}", stdout(&refused));
    assert!(stderr(&refused).contains("--force"), "{}", stderr(&refused));
    let forced = synced.run(&[&args[..], &["--force"]].concat());
    assert!(forced.status.success(), "{}", stderr(&forced));
}

#[test]
fn sync_writes_the_config_its_previous_copy_and_the_origins() {
    let synced = adopted();
    let before = std::fs::read_to_string(synced.config()).unwrap();
    let machine = std::fs::read_to_string(synced.machine_file()).unwrap();
    std::fs::write(
        synced.machine_file(),
        machine.replace("max_workers = 10", "max_workers = 7"),
    )
    .unwrap();
    synced.commit_repo("fewer workers");
    let dry = synced.run(&["config", "sync", "--no-pull", "--dry-run"]);
    assert!(dry.status.success(), "{}{}", stdout(&dry), stderr(&dry));
    assert!(
        stdout(&dry).contains("+ max_workers = 7"),
        "{}",
        stdout(&dry)
    );
    assert_eq!(
        std::fs::read_to_string(synced.config()).unwrap(),
        before,
        "dry run"
    );
    let output = synced.run(&["config", "sync", "--no-pull"]);
    assert!(
        output.status.success(),
        "{}{}",
        stdout(&output),
        stderr(&output)
    );
    let text = stdout(&output);
    assert!(
        text.contains("- max_workers = 10") && text.contains("+ max_workers = 7"),
        "{text}"
    );
    let after = std::fs::read_to_string(synced.config()).unwrap();
    assert!(after.contains("max_workers = 7"), "{after}");
    let prev = std::fs::read_to_string(synced.home.path().join("config.toml.prev")).unwrap();
    assert_eq!(prev, before, "the previous file is kept");
    let origin: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(synced.home.path().join("config.origin.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(origin["machine"], "test", "{origin}");
    assert_eq!(
        origin["origins"]["machine.max_workers"], "machine",
        "{origin}"
    );
    assert_eq!(origin["origins"]["models.default"], "shared", "{origin}");
    assert_eq!(
        origin["origins"]["machine.notify_ntfy"], "local",
        "{origin}"
    );
    assert_eq!(origin["commit"].as_str().unwrap().len(), 40, "{origin}");
    assert_eq!(
        origin["written_sha256"].as_str().unwrap().len(),
        64,
        "{origin}"
    );
    let log = stdout(&synced.run(&["log"]));
    assert!(log.contains("config-synced"), "{log}");
}

#[test]
fn a_broken_machine_file_fails_the_sync_and_leaves_the_config_alone() {
    let synced = adopted();
    let before = std::fs::read(synced.config()).unwrap();
    std::fs::write(synced.machine_file(), "[machine\nhost =").unwrap();
    let output = synced.run(&["config", "sync", "--no-pull", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    let report: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(report["ok"], false, "{report}");
    let message = report["problems"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("headwork.toml"),
        "names the file: {message}"
    );
    assert_eq!(
        std::fs::read(synced.config()).unwrap(),
        before,
        "config.toml is untouched"
    );
    assert!(
        !synced.home.path().join("config.toml.prev").exists(),
        "no previous copy"
    );
    let log = stdout(&synced.run(&["log"]));
    assert!(log.contains("config-sync-failed"), "{log}");
}

#[test]
fn a_secret_in_a_tracked_file_is_refused() {
    let synced = adopted();
    let machine = std::fs::read_to_string(synced.machine_file()).unwrap();
    std::fs::write(
        synced.machine_file(),
        machine.replace(
            "[machine]",
            "[machine]\nnotify_ntfy = \"https://ntfy.example/leak\"",
        ),
    )
    .unwrap();
    let output = synced.run(&["config", "check"]);
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    assert!(
        stdout(&output).contains("machine.notify_ntfy"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn rollback_puts_the_previous_config_back() {
    let synced = adopted();
    let before = std::fs::read_to_string(synced.config()).unwrap();
    let machine = std::fs::read_to_string(synced.machine_file()).unwrap();
    std::fs::write(
        synced.machine_file(),
        machine.replace("max_workers = 10", "max_workers = 7"),
    )
    .unwrap();
    synced.commit_repo("fewer workers");
    assert!(
        synced
            .run(&["config", "sync", "--no-pull"])
            .status
            .success()
    );
    let rolled = synced.run(&["config", "rollback"]);
    assert!(rolled.status.success(), "{}", stderr(&rolled));
    assert_eq!(
        std::fs::read_to_string(synced.config()).unwrap(),
        before,
        "restored"
    );
    let kept = std::fs::read_to_string(synced.home.path().join("config.toml.rolled-back")).unwrap();
    assert!(kept.contains("max_workers = 7"), "{kept}");
    let log = stdout(&synced.run(&["log"]));
    assert!(log.contains("config-rolled-back"), "{log}");
}

#[test]
fn show_origin_lists_each_key_with_its_layer() {
    let synced = adopted();
    assert!(
        synced
            .run(&["config", "sync", "--no-pull"])
            .status
            .success()
    );
    let output = synced.run(&["config", "show", "--origin"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    let line = |key: &str| {
        text.lines()
            .find(|line| line.starts_with(&format!("{key}\t")))
            .unwrap_or_else(|| panic!("no line for {key}: {text}"))
            .split('\t')
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(line("models.default")[1], "shared", "{text}");
    assert_eq!(line("machine.max_workers")[1], "machine", "{text}");
    assert_eq!(line("machine.notify_ntfy")[1], "local", "{text}");
    assert_eq!(line("models.default")[2], "\"sonnet\"", "{text}");
    let plain = stdout(&synced.run(&["config", "show"]));
    assert!(plain.contains("[models]"), "{plain}");
}

#[test]
fn doctor_reports_stale_sources_and_a_hand_edited_config() {
    let synced = adopted();
    assert!(
        synced
            .run(&["config", "sync", "--no-pull"])
            .status
            .success()
    );
    let status = |output: &Output, id: &str| -> String {
        let report: serde_json::Value = serde_json::from_str(&stdout(output)).unwrap();
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["id"] == id)
            .unwrap_or_else(|| panic!("no check {id}: {report}"))["status"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let clean = synced.run(&["doctor", "--json"]);
    assert_eq!(status(&clean, "config-sources"), "ok");
    assert_eq!(status(&clean, "config-stale"), "ok");
    assert_eq!(status(&clean, "config-drift"), "ok");
    assert_eq!(status(&clean, "config-secret"), "ok");
    assert_eq!(status(&clean, "config-host"), "ok");
    let text = std::fs::read_to_string(synced.config()).unwrap();
    std::fs::write(
        synced.config(),
        text.replace("max_workers = 10", "max_workers = 9"),
    )
    .unwrap();
    let drifted = synced.run(&["doctor", "--json"]);
    assert_eq!(status(&drifted, "config-drift"), "warn");
    let machine = std::fs::read_to_string(synced.machine_file()).unwrap();
    std::fs::write(
        synced.machine_file(),
        machine.replace("max_workers = 10", "max_workers = 8"),
    )
    .unwrap();
    synced.commit_repo("more");
    let stale = synced.run(&["doctor", "--json"]);
    assert_eq!(status(&stale, "config-stale"), "warn");
}

#[test]
fn doctor_says_a_hand_written_config_is_not_synced() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), fixture()).unwrap();
    let output = headwork(home.path(), &["doctor", "--json"]);
    let report: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    let checks = report["checks"].as_array().unwrap();
    let sources = checks
        .iter()
        .find(|check| check["id"] == "config-sources")
        .unwrap();
    assert_eq!(sources["status"], "ok", "{sources}");
    assert!(
        sources["detail"].as_str().unwrap().contains("not synced"),
        "{sources}"
    );
    assert!(
        checks.iter().all(|check| check["id"] != "config-drift"),
        "{report}"
    );
}

#[test]
fn the_tick_raises_config_drift_and_clears_it_after_a_sync() {
    let synced = adopted();
    assert!(
        synced
            .run(&["config", "sync", "--no-pull"])
            .status
            .success()
    );
    let rules = |output: &Output| -> Vec<String> {
        let report: serde_json::Value = serde_json::from_str(&stdout(output)).unwrap();
        report["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line["rule"].as_str().unwrap().to_owned())
            .collect()
    };
    let calm = synced.run(&["tick", "--json"]);
    assert!(calm.status.success(), "{}{}", stdout(&calm), stderr(&calm));
    assert!(
        !rules(&calm).iter().any(|rule| rule.starts_with("config-")),
        "{}",
        stdout(&calm)
    );
    let text = std::fs::read_to_string(synced.config()).unwrap();
    std::fs::write(
        synced.config(),
        text.replace("max_workers = 10", "max_workers = 9"),
    )
    .unwrap();
    let drifted = synced.run(&["tick", "--json"]);
    assert!(
        rules(&drifted).contains(&"config-drift".to_owned()),
        "{}",
        stdout(&drifted)
    );
    assert!(
        synced
            .run(&["config", "sync", "--no-pull"])
            .status
            .success()
    );
    let healed = synced.run(&["tick", "--json"]);
    assert!(
        !rules(&healed)
            .iter()
            .any(|rule| rule.starts_with("config-")),
        "{}",
        stdout(&healed)
    );
}
