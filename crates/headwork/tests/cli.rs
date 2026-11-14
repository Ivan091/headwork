//! The `headwork` binary against a temporary `HEADWORK_HOME`.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;
use std::process::{Command, Output};

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

fn headwork(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_headwork"))
        .args(args)
        .env("HEADWORK_HOME", home)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn home_with_fixture() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), fixture("config.toml")).unwrap();
    home
}

#[test]
fn board_shows_the_configured_projects() {
    let home = home_with_fixture();
    let output = headwork(home.path(), &["board"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.starts_with("machine normal · 0 of 10 slots · 0 asks waiting"),
        "{text}"
    );
    for project in ["me-sim", "me-studio", "work-service"] {
        assert!(
            text.contains(&format!("\n{project} (")),
            "{project} missing: {text}"
        );
    }
    assert!(
        home.path().join("state.db").exists(),
        "the store is created"
    );
}

#[test]
fn board_json_is_machine_readable() {
    let home = home_with_fixture();
    let output = headwork(home.path(), &["board", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let json: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(json["machine"], "normal", "mode");
    assert_eq!(json["projects"].as_array().unwrap().len(), 3, "projects");
}

#[test]
fn log_shows_the_project_sync() {
    let home = home_with_fixture();
    let output = headwork(home.path(), &["log"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("projects-synced"),
        "{}",
        stdout(&output)
    );
    let again = headwork(home.path(), &["log", "--json"]);
    let events: serde_json::Value = serde_json::from_str(&stdout(&again)).unwrap();
    assert_eq!(
        events.as_array().unwrap().len(),
        1,
        "an unchanged config logs no second sync"
    );
}

#[test]
fn log_of_an_unknown_task_fails() {
    let home = home_with_fixture();
    let output = headwork(home.path(), &["log", "me-sim-nothing"]);
    assert!(!output.status.success(), "unknown task must fail");
    assert!(
        stderr(&output).contains("no task named `me-sim-nothing`"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_missing_config_is_explained() {
    let home = tempfile::tempdir().unwrap();
    let output = headwork(home.path(), &["board"]);
    assert!(!output.status.success(), "no config must fail");
    assert!(
        stderr(&output).contains("no config at"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_widening_overlay_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join(".headwork.toml"),
        fixture("widening.headwork.toml"),
    )
    .unwrap();
    let main = project.path().display().to_string().replace('\\', "/");
    let config = format!(
        "[machine]\nhost = \"wmux\"\n\n[project.repo]\nmain = \"{main}\"\nworktree_new = \"builtin\"\n\
         worktree_remove = \"builtin\"\nworktree_path = \"{{main}}-{{slug}}\"\ndeliver = \"pr\"\nmerge = \"human\"\n"
    );
    std::fs::write(home.path().join("config.toml"), config).unwrap();
    let output = headwork(home.path(), &["board"]);
    assert!(!output.status.success(), "a widening overlay must fail");
    assert!(
        stderr(&output).contains("`allowed_repos` may not be set"),
        "{}",
        stderr(&output)
    );
}

fn hook(home: &Path, session: Option<&str>, event: &str, input: &str) -> Output {
    use std::io::Write as _;
    let mut command = Command::new(env!("CARGO_BIN_EXE_headwork"));
    command
        .args(["hook", event])
        .env("HEADWORK_HOME", home)
        .env_remove("HEADWORK_SESSION")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(session) = session {
        command.env("HEADWORK_SESSION", session);
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn hooks_of_unmanaged_sessions_allow_without_touching_the_store() {
    let home = home_with_fixture();
    let recorded = fixture("hooks/pre-tool-use-edit.json");
    let output = hook(home.path(), None, "pre-tool-use", &recorded);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(output.stdout.is_empty(), "no output");
    assert!(!home.path().join("state.db").exists(), "no store opened");
    let output = hook(
        home.path(),
        Some("not-in-the-store"),
        "pre-tool-use",
        &recorded,
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(output.stdout.is_empty(), "unknown sessions are unmanaged");
}

#[test]
fn a_broken_hook_input_never_blocks() {
    let home = home_with_fixture();
    let output = hook(home.path(), Some("s"), "stop", "{not json");
    assert_eq!(output.status.code(), Some(1), "a non-blocking error");
    assert!(
        stderr(&output).contains("headwork hook Stop"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn rule_test_decides_a_command_as_the_root() {
    let home = home_with_fixture();
    let output = headwork(home.path(), &["rule", "test", "git push origin master"]);
    assert_eq!(output.status.code(), Some(1), "denied");
    assert!(
        stdout(&output).starts_with("deny root-git (root, shadow): "),
        "{}",
        stdout(&output)
    );
    let output = headwork(home.path(), &["rule", "test", "git status && gh pr list"]);
    assert!(output.status.success(), "{}", stdout(&output));
    assert_eq!(stdout(&output), "allow (root, shadow)\n", "allowed");
    let output = headwork(
        home.path(),
        &["rule", "test", "scripts/publish-now.sh", "--json"],
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["result"]["rule"], "user-only", "{json}");
}

#[test]
fn tick_runs_in_shadow_mode_on_an_empty_store() {
    let home = tempfile::tempdir().unwrap();
    let config = fixture("config.toml")
        .replace(
            "[machine]",
            "[machine]\nhost_bin = \"S:/no-such-host/wmux.js\"",
        )
        .replace("disk_warn_gb = 15", "disk_warn_gb = 2")
        .replace("disk_stop_gb = 5", "disk_stop_gb = 1")
        .replace(
            "roots = [\"~/.claude/projects\", \"{temp}\", \"S:/Code\"]",
            "roots = []",
        );
    std::fs::write(home.path().join("config.toml"), config).unwrap();
    let output = headwork(home.path(), &["tick", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let json: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(json["enforce"], false, "shadow by default");
    assert_eq!(json["host_up"], false, "the host is not there");
    assert!(home.path().join("tick.lock").exists(), "the lock file");
}
