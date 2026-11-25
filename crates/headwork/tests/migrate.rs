//! `headwork migrate` against a temporary `HEADWORK_HOME` and a project whose checkout has an origin with notes.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;

use headwork_core::migrate::MACHINE_PLACEHOLDER as PLACEHOLDER;
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
        format!(
            "id = \"named\"\ntitle = \"Named\"\ncheck = \"false\"\n\n[[step]]\nkind = \"agent\"\nrun = \"test {} = pc\"\n",
            headwork_core::migrate::MACHINE_PLACEHOLDER
        ),
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

fn add_note(machine: &Machine, id: &str, text: &str) {
    std::fs::write(machine.origin.join(format!("migrations/{id}.toml")), text).unwrap();
    git(&machine.origin, &["add", "."]);
    git(&machine.origin, &["commit", "-q", "-m", id]);
}

fn head(machine: &Machine) -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&machine.clone)
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn a_note_that_says_pull_false_is_not_pulled_by_apply() {
    let machine = Machine::new();
    add_note(
        &machine,
        "nopull",
        "id = \"nopull\"\ntitle = \"No pull\"\ncheck = \"false\"\npull = false\n\n[[step]]\nkind = \"agent\"\nrun = \"true\"\n",
    );
    let before = head(&machine);
    let applied = machine.run(&["migrate", "apply", "nopull"]);
    assert_eq!(applied.status.code(), Some(0), "{applied:?}");
    assert_eq!(head(&machine), before, "the checkout stays where it was");
    assert!(
        !machine.clone.join("migrations/nopull.toml").exists(),
        "not pulled"
    );
    assert!(
        machine.list().contains("applied         me-sim/nopull"),
        "applied all the same"
    );
}

#[test]
fn a_failing_step_leaves_the_note_pending_with_a_failure_exit() {
    let machine = Machine::new();
    add_note(
        &machine,
        "failing",
        "id = \"failing\"\ntitle = \"Fails\"\ncheck = \"false\"\npull = false\n\n\
         [[step]]\nkind = \"agent\"\nrun = \"echo first-step\"\n\n\
         [[step]]\nkind = \"agent\"\nrun = \"echo boom; exit 7\"\n",
    );
    let output = machine.run(&["migrate", "apply", "failing"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let said = text(&output);
    assert!(said.contains("step 2 failed"), "{said}");
    assert!(
        said.contains("first-step") && said.contains("boom"),
        "earlier and failing output are both kept: {said}"
    );
    assert!(
        machine.list().contains("pending         me-sim/failing"),
        "still pending"
    );
}

#[test]
fn from_without_confirmed_asks_again_and_runs_nothing() {
    let machine = Machine::new();
    let output = machine.run(&["migrate", "apply", "steps", "--from", "2"]);
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(
        !text(&output).contains("$ echo"),
        "nothing ran: {}",
        text(&output)
    );
    assert!(
        machine.list().contains("pending         me-sim/steps"),
        "still pending"
    );
}

#[test]
fn from_past_the_last_step_is_refused() {
    let machine = Machine::new();
    let output = machine.run(&["migrate", "apply", "steps", "--from", "9", "--confirmed"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("past the last one"),
        "{output:?}"
    );
    assert!(
        machine.list().contains("pending         me-sim/steps"),
        "still pending"
    );
}

#[test]
fn applying_an_invalid_note_fails_and_prints_the_problems() {
    let machine = Machine::new();
    let output = machine.run(&["migrate", "apply", "broken"]);
    assert!(!output.status.success(), "{output:?}");
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.contains("not a valid note"), "{said}");
    assert!(said.contains("must not contain `sudo`"), "{said}");
}

#[test]
fn notes_for_other_machines_are_not_applicable_and_a_skipped_one_stays_skipped() {
    let machine = Machine::new();
    for (id, filter) in [
        ("other-os", "os = [\"no-such-os\"]"),
        ("other-name", "machines = [\"nobody\"]"),
        ("other-project", "requires = [\"no-such-project\"]"),
    ] {
        add_note(
            &machine,
            id,
            &format!(
                "id = \"{id}\"\ntitle = \"Filtered\"\n{filter}\ncheck = \"false\"\npull = false\n\n[[step]]\nkind = \"agent\"\nrun = \"false\"\n"
            ),
        );
    }
    let listed = text(&machine.run(&["migrate", "list"]));
    for id in ["other-os", "other-name", "other-project"] {
        assert!(
            listed.contains(&format!("not-applicable  me-sim/{id}")),
            "{id}: {listed}"
        );
    }
    let applied = machine.run(&["migrate", "apply", "other-os"]);
    assert_eq!(applied.status.code(), Some(0), "{applied:?}");
    assert!(
        text(&applied).contains("not applicable"),
        "{}",
        text(&applied)
    );

    machine.run(&["migrate", "mark", "steps", "skipped", "--reason", "no"]);
    let again = machine.run(&["migrate", "apply", "steps"]);
    assert!(text(&again).contains("already skipped"), "{}", text(&again));
}

#[test]
fn a_failed_fetch_is_reported_by_list_and_refuses_apply() {
    let machine = Machine::new();
    git(
        &machine.clone,
        &["remote", "set-url", "origin", "/no/such/origin"],
    );
    let listed = text(&machine.run(&["migrate", "list"]));
    assert!(listed.contains("problem: me-sim: fetch failed"), "{listed}");
    assert!(
        listed.contains("me-sim/steps"),
        "cached notes are listed: {listed}"
    );
    let applied = machine.run(&["migrate", "apply", "steps"]);
    assert!(!applied.status.success(), "{applied:?}");
    assert!(
        String::from_utf8_lossy(&applied.stderr).contains("fetch failed"),
        "{applied:?}"
    );
}

#[test]
fn show_leaves_the_machine_placeholder_visible_when_the_name_is_unknown() {
    let machine = Machine::new();
    add_note(
        &machine,
        "named",
        &format!(
            "id = \"named\"\ntitle = \"Named\"\ncheck = \"false\"\n\n[[step]]\nkind = \"agent\"\nrun = \"echo {PLACEHOLDER}\"\n"
        ),
    );
    machine.run(&["migrate", "list"]);
    let shown = text(&machine.run(&["migrate", "show", "named"]));
    assert!(shown.contains(&format!("echo {PLACEHOLDER}")), "{shown}");
    assert!(shown.contains("machine: unknown"), "{shown}");
}

#[test]
fn show_tells_how_a_settled_note_was_settled() {
    let machine = Machine::new();
    machine.run(&[
        "migrate", "mark", "steps", "skipped", "--reason", "not here",
    ]);
    let shown = text(&machine.run(&["migrate", "show", "steps"]));
    assert!(shown.contains("settled: skipped by"), "{shown}");
    assert!(shown.contains("not here"), "{shown}");
}

#[test]
fn repository_keys_never_name_two_checkouts() {
    use headwork::home::Home;
    use headwork::ops::migrate::sources;
    let dir = tempfile::tempdir().unwrap();
    let home = Home::at(Some(dir.path().join("home"))).unwrap();
    let config = headwork_core::Config::parse(
        &format!(
            "[machine]\nhost = \"wmux\"\n\n[hooks]\norchestrator_repos = [\"{0}/a/Notes\", \"{0}/b/notes\", \"{0}/c/demo\"]\n\n\
             [project.notes]\nmain = \"{0}/p/notes\"\nworktree_new = \"builtin\"\nworktree_remove = \"builtin\"\n\
             worktree_path = \"{{main}}-{{slug}}\"\ndeliver = \"pr\"\nmerge = \"deliver\"\nallowed_repos = [\"acme/notes\"]\n\n\
             [project.demo]\nmain = \"{0}/p/demo\"\nworktree_new = \"builtin\"\nworktree_remove = \"builtin\"\n\
             worktree_path = \"{{main}}-{{slug}}\"\ndeliver = \"pr\"\nmerge = \"deliver\"\nallowed_repos = [\"acme/demo\"]\n",
            forward(dir.path())
        ),
        "test",
    )
    .unwrap();
    let keys: Vec<String> = sources(&home, &config)
        .into_iter()
        .map(|source| source.repo)
        .collect();
    assert_eq!(
        keys,
        [
            "demo",
            "notes",
            "notes-notes",
            "notes-notes-2",
            "demo-notes"
        ],
        "{keys:?}"
    );
}
