//! `headwork flow check` and the built-in flows against a temporary `HEADWORK_HOME`.

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
        stdout(&output).contains("[ok  ] code         built-in")
            && stdout(&output).contains("[ok  ] plan         built-in"),
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
