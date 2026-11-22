//! The MCP relay picks up a newly installed build between two tool calls, with the real binary installed into a
//! temporary folder.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, UNIX_EPOCH};

use headwork::ops::install::{clean, install};
use serde_json::{Value, json};

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

fn build_dated(dir: &Path, seconds: u64) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(format!("headwork{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(env!("CARGO_BIN_EXE_headwork"), &path).unwrap();
    let file = std::fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_secs(seconds))
        .unwrap();
    path
}

struct Session {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Session {
    fn send(&mut self, message: &Value) {
        writeln!(self.input, "{message}").unwrap();
        self.input.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        assert!(
            self.output.read_line(&mut line).unwrap() > 0,
            "the relay ended early"
        );
        serde_json::from_str(&line).unwrap()
    }

    fn finish(mut self) -> String {
        drop(self.input);
        let mut rest = String::new();
        for line in self.output.lines() {
            rest.push_str(&line.unwrap());
        }
        assert!(
            self.child.wait().unwrap().success(),
            "the relay exits cleanly"
        );
        rest
    }
}

fn tool_names(reply: &Value) -> Vec<String> {
    reply["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn the_relay_switches_to_a_new_build_between_calls() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("config.toml"), fixture("config.toml")).unwrap();
    let bin = root.path().join("bin");
    let first = build_dated(&root.path().join("build-1"), 1_000_000);
    let installed = install(&first, &bin, |_| Ok(())).unwrap().target;

    let mut child = Command::new(&installed)
        .arg("mcp")
        .env("HEADWORK_HOME", &home)
        .env("HEADWORK_CLAUDE_JSON", home.join(".claude.json"))
        .env("HEADWORK_CLAUDE_DIR", home.join(".claude"))
        .env("HEADWORK_EXE", &installed)
        .env("HEADWORK_SESSION", "0b6c1d2e-0000-4000-8000-00000000000a")
        .env("HEADWORK_ROLE", "root")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut session = Session {
        input: child.stdin.take().unwrap(),
        output: BufReader::new(child.stdout.take().unwrap()),
        child,
    };
    session.send(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } },
    }));
    let init = session.read();
    assert_eq!(init["id"], 1, "{init}");
    assert_eq!(
        init["result"]["capabilities"]["tools"]["listChanged"], true,
        "{init}"
    );
    session.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    session.send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let before = tool_names(&session.read());
    assert!(
        before.contains(&"spawn".to_owned()),
        "the orchestrator's tools: {before:?}"
    );

    let second = build_dated(&root.path().join("build-2"), 2_000_000);
    let report = install(&second, &bin, |_| Ok(())).unwrap();
    if cfg!(windows) {
        assert_eq!(
            report.kept.len(),
            1,
            "the running server's copy is kept: {report:?}"
        );
    }

    session.send(&json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }));
    let reply = session.read();
    assert_eq!(reply["id"], 3, "{reply}");
    assert_eq!(
        tool_names(&reply),
        before,
        "the same tools from the new server"
    );
    let notice = session.read();
    assert_eq!(
        notice["method"], "notifications/tools/list_changed",
        "{notice}"
    );

    session.send(&json!({ "jsonrpc": "2.0", "id": 4, "method": "ping" }));
    assert_eq!(session.read()["id"], 4, "the new server answers");
    assert_eq!(session.finish(), "", "no second notice without a new build");

    let (_, kept) = clean(&bin).unwrap();
    assert!(kept.is_empty(), "every retired server has exited: {kept:?}");
}

fn board_projects(session: &mut Session, id: u64) -> Vec<String> {
    session.send(&json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": { "name": "board", "arguments": {} },
    }));
    let reply = session.read();
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    let board: Value = serde_json::from_str(text).unwrap();
    board["projects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|project| project["enabled"] == true)
        .map(|project| project["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn the_server_reads_the_config_again_before_each_call() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let config = fixture("config.toml");
    std::fs::write(home.join("config.toml"), &config).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_headwork"))
        .args(["mcp", "--serve"])
        .env("HEADWORK_HOME", &home)
        .env("HEADWORK_CLAUDE_JSON", home.join(".claude.json"))
        .env("HEADWORK_CLAUDE_DIR", home.join(".claude"))
        .env("HEADWORK_SESSION", "0b6c1d2e-0000-4000-8000-00000000000b")
        .env("HEADWORK_ROLE", "orchestrator")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut session = Session {
        input: child.stdin.take().unwrap(),
        output: BufReader::new(child.stdout.take().unwrap()),
        child,
    };
    let before = board_projects(&mut session, 1);
    assert!(
        !before.contains(&"extra".to_owned()),
        "not configured yet: {before:?}"
    );
    let added = format!(
        "{config}\n[project.extra]\nmain = \"~/work/extra\"\nworktree_new = \"builtin\"\nworktree_remove = \"builtin\"\n\
         worktree_path = \"~/work/extra-wt/{{slug}}\"\ndeliver = \"pr\"\nmerge = \"human\"\nallowed_repos = [\"acme/extra\"]\n"
    );
    std::fs::write(home.join("config.toml"), added).unwrap();
    let after = board_projects(&mut session, 2);
    assert!(
        after.contains(&"extra".to_owned()),
        "the edited config reached the running server: {after:?}"
    );
    std::fs::write(home.join("config.toml"), "not toml [").unwrap();
    let broken = board_projects(&mut session, 3);
    assert_eq!(broken, after, "a broken config leaves the last good one");
    session.finish();
}
