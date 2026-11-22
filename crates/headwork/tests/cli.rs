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
        .env("HEADWORK_CLAUDE_JSON", home.join(".claude.json"))
        .env("HEADWORK_CLAUDE_DIR", home.join(".claude"))
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
fn rule_test_decides_a_command_as_the_orchestrator() {
    let home = home_with_fixture();
    let output = headwork(home.path(), &["rule", "test", "git push origin master"]);
    assert_eq!(output.status.code(), Some(1), "denied");
    assert!(
        stdout(&output).starts_with("deny orchestrator-git (orchestrator, shadow): "),
        "{}",
        stdout(&output)
    );
    let output = headwork(home.path(), &["rule", "test", "git status && gh pr list"]);
    assert!(output.status.success(), "{}", stdout(&output));
    assert_eq!(stdout(&output), "allow (orchestrator, shadow)\n", "allowed");
    let output = headwork(
        home.path(),
        &["rule", "test", "scripts/publish-now.sh", "--json"],
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["result"]["rule"], "user-only", "{json}");
    let output = headwork(
        home.path(),
        &["rule", "test", "--edit", "S:/Code/me-sim/me-sim/src/a.rs"],
    );
    assert_eq!(output.status.code(), Some(1), "an edit outside the home");
    assert!(
        stdout(&output).starts_with("deny orchestrator-edit (orchestrator, shadow): "),
        "{}",
        stdout(&output)
    );
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

#[test]
fn spawn_rejects_an_unknown_effort() {
    let home = home_with_fixture();
    let output = headwork(
        home.path(),
        &["spawn", "me-sim", "x", "brief.md", "--effort", "extreme"],
    );
    assert!(!output.status.success(), "refused");
    assert!(stderr(&output).contains("extreme"), "{}", stderr(&output));
}

#[test]
fn root_is_a_hidden_alias_of_orchestrator() {
    let home = tempfile::tempdir().unwrap();
    let help = stdout(&headwork(home.path(), &["--help"]));
    assert!(help.contains("orchestrator"), "{help}");
    assert!(
        !help
            .lines()
            .any(|line| line.trim_start().starts_with("root ")),
        "{help}"
    );
    let new = headwork(home.path(), &["orchestrator", "--help"]);
    let old = headwork(home.path(), &["root", "--help"]);
    assert!(old.status.success(), "{}", stderr(&old));
    assert!(stdout(&old).contains("--name <NAME>"), "{}", stdout(&old));
    assert_eq!(stdout(&old), stdout(&new), "the same command");
}

#[test]
fn reap_dry_run_ends_nothing_and_reports_its_plan() {
    let home = home_with_fixture();
    let output = headwork(home.path(), &["reap", "--dry-run", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let json: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(json["dry_run"], true, "dry run");
    let ended = json["ended"].as_array().unwrap();
    assert!(
        ended.iter().all(|p| p["outcome"]["outcome"] == "would_end"),
        "nothing ended: {json}"
    );
    let text = stdout(&headwork(home.path(), &["reap", "--dry-run"]));
    assert!(
        text.starts_with("nothing to reap") || text.starts_with("would end"),
        "{text}"
    );
}

#[test]
fn update_with_updates_off_skips_and_changes_nothing() {
    let home = home_with_fixture();
    let bin = tempfile::tempdir().unwrap();
    let installed = bin
        .path()
        .join(format!("headwork{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(env!("CARGO_BIN_EXE_headwork"), &installed).unwrap();
    let dir = bin.path().to_str().unwrap();
    let output = headwork(home.path(), &["update", "--dry-run", "--dir", dir]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("updates are off"),
        "{}",
        stdout(&output)
    );
    let output = headwork(home.path(), &["update", "--json", "--dir", dir]);
    assert!(output.status.success(), "{}", stderr(&output));
    let json: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(json["outcome"], "skipped", "{json}");
    assert_eq!(json["reason"], "off", "{json}");
    assert!(
        !home.path().join("updates").exists(),
        "nothing is downloaded"
    );
}

#[test]
fn update_refuses_a_tag_that_is_not_a_release() {
    let home = home_with_fixture();
    let output = headwork(home.path(), &["update", "--to", "latest"]);
    assert_eq!(output.status.code(), Some(2), "a usage error");
    assert!(
        stderr(&output).contains("not a release tag"),
        "{}",
        stderr(&output)
    );
}

fn assistant_line(id: &str, block: u32, input: u64, output: u64) -> String {
    serde_json::json!({
        "type": "assistant",
        "uuid": format!("u{id}{block}"),
        "timestamp": "2026-11-22T10:00:00.000Z",
        "effort": "high",
        "cwd": "S:/w",
        "message": {
            "id": id,
            "model": "claude-opus-5-5",
            "usage": {
                "input_tokens": input,
                "cache_read_input_tokens": 100,
                "output_tokens": output,
                "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 50}
            }
        }
    })
    .to_string()
}

#[test]
fn usage_reads_transcripts_once_and_groups_them() {
    let home = home_with_fixture();
    let folder = home.path().join(".claude/projects/S--w");
    let subagents = folder.join("0a/subagents");
    std::fs::create_dir_all(&subagents).unwrap();
    let main = [
        assistant_line("m1", 1, 10, 5),
        assistant_line("m1", 2, 10, 5),
        r#"{"type":"user","message":{"role":"user","content":"hi"}}"#.to_owned(),
        assistant_line("m2", 1, 1, 1),
    ];
    std::fs::write(
        folder.join("0a.jsonl"),
        format!("{}\n{{\"type\":\"assis", main.join("\n")),
    )
    .unwrap();
    std::fs::write(
        subagents.join("agent-x1.jsonl"),
        assistant_line("m3", 1, 2, 2) + "\n",
    )
    .unwrap();
    std::fs::write(
        subagents.join("agent-x1.meta.json"),
        r#"{"agentType":"Explore"}"#,
    )
    .unwrap();

    let output = headwork(
        home.path(),
        &["usage", "--by", "agent", "--unmanaged", "--json"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let json: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(
        json["ingest"]["inserted"], 3,
        "three responses, the half line left: {json}"
    );
    assert_eq!(json["total"]["responses"], 3, "{json}");
    assert_eq!(json["total"]["input"], 13, "{json}");
    assert_eq!(json["total"]["cache_write_1h"], 150, "{json}");
    let keys: Vec<&str> = json["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["main", "Explore"], "the session and its subagent");

    let again = headwork(home.path(), &["usage", "--by", "role"]);
    assert!(again.status.success(), "{}", stderr(&again));
    let text = stdout(&again);
    assert!(
        text.starts_with("read 0 of 2 transcripts"),
        "nothing new: {text}"
    );
    assert!(text.contains("\nunmanaged "), "one unmanaged line: {text}");

    let backfill = headwork(home.path(), &["usage", "--backfill", "--gaps"]);
    let text = stdout(&backfill);
    assert!(
        text.contains("4 response lines, 0 new responses"),
        "a backfill counts nothing twice: {text}"
    );
    assert!(text.contains("not counted:"), "gaps listed: {text}");

    let wrong = headwork(home.path(), &["usage", "--by", "colour"]);
    assert!(!wrong.status.success(), "an unknown grouping is refused");
}

#[test]
fn flow_check_passes_the_built_in_flows() {
    let home = home_with_fixture();
    let output = headwork(home.path(), &["flow", "check"]);
    assert!(
        output.status.success(),
        "{}{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        stdout(&output).contains("[ok  ] code         built-in"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn flow_check_reports_a_broken_override_and_exits_1() {
    let home = home_with_fixture();
    let flows = home.path().join("flows");
    std::fs::create_dir_all(&flows).unwrap();
    let broken = fixture("../../flows/code.toml")
        .replace("prompt = \"code/tests.md\"", "prompt = \"mine/tests.md\"");
    std::fs::write(flows.join("code.toml"), broken).unwrap();
    std::fs::write(flows.join("odd.toml"), "name = \"odd\"\n").unwrap();
    let output = headwork(home.path(), &["flow", "check", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    let json: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    let reports = json.as_array().unwrap();
    let code = reports
        .iter()
        .find(|report| report["name"] == "code")
        .unwrap();
    assert_eq!(
        code["origin"]["kind"], "home",
        "the home file overrides the built-in"
    );
    let problems = code["problems"].to_string();
    assert!(problems.contains("mine/tests.md"), "{problems}");
    assert!(
        problems.contains("code/correctness.md"),
        "prompts are read from beside the override: {problems}"
    );
    let odd = reports
        .iter()
        .find(|report| report["name"] == "odd")
        .unwrap();
    assert!(
        !odd["problems"].as_array().unwrap().is_empty(),
        "a flow missing its fields is refused"
    );
}

#[test]
fn every_file_in_flows_is_built_in() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../flows");
    let mut found = Vec::new();
    let mut dirs = vec![root.clone()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                found.push(
                    path.strip_prefix(&root)
                        .unwrap()
                        .to_string_lossy()
                        .replace(std::path::MAIN_SEPARATOR, "/"),
                );
            }
        }
    }
    found.sort();
    let mut built: Vec<String> = headwork::ops::flows::BUILTIN_FILES
        .iter()
        .map(|(path, _)| (*path).to_owned())
        .collect();
    built.sort();
    assert_eq!(found, built, "flows/ and BUILTIN_FILES list the same files");
}
