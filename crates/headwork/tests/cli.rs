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
