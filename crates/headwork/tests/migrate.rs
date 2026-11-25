//! `headwork migrate` against a temporary `HEADWORK_HOME` and a project whose checkout has an origin with notes.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;
use std::process::{Command, Output};

const CHECKED: &str = "id = \"checked\"\ntitle = \"Already done\"\ncheck = \"true\"\n\n[[step]]\nkind = \"agent\"\nrun = \"false\"\n";

const STEPS: &str = "id = \"steps\"\ntitle = \"Three kinds\"\ncheck = \"false\"\n\n\
[[step]]\nkind = \"agent\"\nrun = \"echo agent-ran\"\n\n\
[[step]]\nkind = \"confirm\"\nrun = \"echo confirm-ran\"\nbackup = \"echo backed-up\"\n\n\
[[step]]\nkind = \"user\"\nsay = \"Do the thing yourself.\"\n";

const BROKEN: &str =
    "id = \"broken\"\ntitle = \"Bad\"\n\n[[step]]\nkind = \"agent\"\nrun = \"sudo ls\"\n";

fn git(dir: &Path, args: &[&str]) {
    let done = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(done.status.success(), "git {args:?}: {done:?}");
}

fn forward(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

struct Machine {
    home: tempfile::TempDir,
    origin: std::path::PathBuf,
    clone: std::path::PathBuf,
    _root: tempfile::TempDir,
}

impl Machine {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let origin = root.path().join("origin");
        std::fs::create_dir_all(origin.join("migrations")).unwrap();
        git(&origin, &["init", "-q", "-b", "main"]);
        std::fs::write(origin.join("migrations/checked.toml"), CHECKED).unwrap();
        std::fs::write(origin.join("migrations/steps.toml"), STEPS).unwrap();
        std::fs::write(origin.join("migrations/broken.toml"), BROKEN).unwrap();
        git(&origin, &["add", "."]);
        git(&origin, &["commit", "-q", "-m", "notes"]);
        let clone = root.path().join("clone");
        git(
            root.path(),
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/config.toml");
        let host = if cfg!(target_os = "macos") {
            "cmux"
        } else {
            "wmux"
        };
        let config = std::fs::read_to_string(fixture)
            .unwrap()
            .replace("host = \"wmux\"", &format!("host = \"{host}\""))
            .replace("S:/Code/me-sim/me-sim", &forward(&clone));
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), config).unwrap();
        Self {
            home,
            origin,
            clone,
            _root: root,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_headwork"))
            .args(args)
            .env("HEADWORK_HOME", self.home.path())
            .env(
                "HEADWORK_CLAUDE_JSON",
                self.home.path().join(".claude.json"),
            )
            .env("HEADWORK_CLAUDE_DIR", self.home.path().join(".claude"))
            .output()
            .unwrap()
    }

    fn list(&self) -> String {
        let output = self.run(&["migrate", "list", "--no-fetch"]);
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    }
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn list_shows_pending_notes_and_invalid_files() {
    let machine = Machine::new();
    let listed = machine.list();
    assert!(listed.contains("pending         me-sim/steps"), "{listed}");
    assert!(
        listed.contains("pending         me-sim/checked"),
        "{listed}"
    );
    assert!(listed.contains("invalid         me-sim/broken"), "{listed}");
    assert!(listed.contains("must not contain `sudo`"), "{listed}");
}

#[test]
fn a_passing_check_records_the_note_without_running_it() {
    let machine = Machine::new();
    let output = machine.run(&["migrate", "apply", "checked"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        machine.list().contains("applied         me-sim/checked"),
        "applied"
    );
}

#[test]
fn apply_stops_at_the_confirm_step_and_the_user_step_then_finishes() {
    let machine = Machine::new();
    let first = machine.run(&["migrate", "apply", "me-sim/steps"]);
    assert_eq!(first.status.code(), Some(3), "{first:?}");
    let said = text(&first);
    assert!(said.contains("step 2 is a confirm step"), "{said}");
    assert!(said.contains("backup first: echo backed-up"), "{said}");
    assert!(
        machine.list().contains("pending         me-sim/steps"),
        "still pending"
    );

    let second = machine.run(&["migrate", "apply", "steps", "--from", "2", "--confirmed"]);
    assert_eq!(second.status.code(), Some(4), "{second:?}");
    assert!(
        text(&second).contains("Do the thing yourself."),
        "{}",
        text(&second)
    );

    let third = machine.run(&["migrate", "apply", "steps", "--from", "3", "--confirmed"]);
    assert_eq!(third.status.code(), Some(0), "{third:?}");
    assert!(
        machine.list().contains("applied         me-sim/steps"),
        "applied"
    );
    let again = machine.run(&["migrate", "apply", "steps"]);
    assert!(text(&again).contains("already applied"), "{}", text(&again));
}

#[test]
fn a_note_not_pulled_yet_is_seen_and_pulled_by_apply() {
    let machine = Machine::new();
    std::fs::write(
        machine.origin.join("migrations/later.toml"),
        "id = \"later\"\ntitle = \"Later\"\ncheck = \"false\"\n\n[[step]]\nkind = \"agent\"\nrun = \"test -f migrations/later.toml\"\n",
    )
    .unwrap();
    git(&machine.origin, &["add", "."]);
    git(&machine.origin, &["commit", "-q", "-m", "later"]);
    let listed = text(&machine.run(&["migrate", "list"]));
    assert!(listed.contains("pending         me-sim/later"), "{listed}");
    assert!(
        !machine.clone.join("migrations/later.toml").exists(),
        "not pulled by list"
    );
    let applied = machine.run(&["migrate", "apply", "later"]);
    assert_eq!(
        applied.status.code(),
        Some(0),
        "{applied:?} {}",
        text(&applied)
    );
    assert!(
        machine.clone.join("migrations/later.toml").exists(),
        "pulled by apply"
    );
}

#[test]
fn mark_skips_a_note_and_needs_a_reason() {
    let machine = Machine::new();
    let refused = machine.run(&["migrate", "mark", "steps", "skipped", "--reason", " "]);
    assert!(!refused.status.success(), "empty reason");
    let done = machine.run(&[
        "migrate",
        "mark",
        "steps",
        "skipped",
        "--reason",
        "not on this machine",
    ]);
    assert!(done.status.success(), "{done:?}");
    assert!(
        machine.list().contains("skipped         me-sim/steps"),
        "skipped"
    );
}

#[test]
fn check_fails_on_a_broken_note_in_the_working_tree() {
    let machine = Machine::new();
    let output = machine.run(&["migrate", "check", machine.clone.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(text(&output).contains("broken.toml"), "{}", text(&output));
}

#[test]
fn a_step_that_names_the_machine_needs_a_name_when_there_is_no_sources_file() {
    let machine = Machine::new();
    std::fs::write(
        machine.origin.join("migrations/named.toml"),
        "id = \"named\"\ntitle = \"Named\"\ncheck = \"false\"\n\n[[step]]\nkind = \"agent\"\nrun = \"test {machine} = pc\"\n",
    )
    .unwrap();
    git(&machine.origin, &["add", "."]);
    git(&machine.origin, &["commit", "-q", "-m", "named"]);
    let refused = machine.run(&["migrate", "apply", "named"]);
    assert!(!refused.status.success(), "no name");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("--machine"),
        "{refused:?}"
    );
    let applied = machine.run(&["migrate", "apply", "named", "--machine", "pc"]);
    assert_eq!(applied.status.code(), Some(0), "{applied:?}");
}
