//! The backlog list: the CLI against a temporary `HEADWORK_HOME` and a temporary copy of the fixture, the export rules, and
//! a task spawned for an item against the fake host.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use headwork::home::Home;
use headwork::ops::Env;
use headwork::ops::backlog::{self, AddRequest, Export};
use headwork::ops::close::close;
use headwork::ops::spawn::SpawnRequest;
use headwork_adapters::host::fake::FakeHost;
use headwork_core::backlog::BacklogStatus;
use headwork_core::{
    Actor, Config, Model, PermissionMode, ProjectName, Slug, TaskRequest, TaskState,
};
use headwork_store::{BacklogFilter, EventFilter, Store};

fn fixture() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/backlog.md");
    std::fs::read_to_string(path).unwrap()
}

fn forward(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

struct Machine {
    dir: tempfile::TempDir,
}

impl Machine {
    fn new(with_file: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let file = forward(&dir.path().join("backlog.md"));
        let item = if with_file {
            format!("\n[backlog]\nfile = \"{file}\"\n")
        } else {
            String::new()
        };
        std::fs::write(
            home.join("config.toml"),
            format!("[machine]\nhost = \"wmux\"\nmax_workers = 4\n{item}"),
        )
        .unwrap();
        Self { dir }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn file(&self) -> PathBuf {
        self.dir.path().join("backlog.md")
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_in(self.dir.path(), args)
    }

    fn run_in(&self, cwd: &Path, args: &[&str]) -> Output {
        self.run_as(cwd, None, args)
    }

    fn run_in_session(&self, role: Option<&str>, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_headwork"));
        command
            .current_dir(self.dir.path())
            .args(args)
            .env("HEADWORK_HOME", self.home())
            .env("HEADWORK_CLAUDE_JSON", self.dir.path().join(".claude.json"))
            .env("HEADWORK_CLAUDE_DIR", self.dir.path().join(".claude"))
            .env("HEADWORK_SESSION", "some-session")
            .env_remove("HEADWORK_ROLE");
        if let Some(role) = role {
            command.env("HEADWORK_ROLE", role);
        }
        command.output().unwrap()
    }

    fn run_as(&self, cwd: &Path, role: Option<&str>, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_headwork"));
        command
            .current_dir(cwd)
            .args(args)
            .env("HEADWORK_HOME", self.home())
            .env("HEADWORK_CLAUDE_JSON", self.dir.path().join(".claude.json"))
            .env("HEADWORK_CLAUDE_DIR", self.dir.path().join(".claude"))
            .env_remove("HEADWORK_ROLE")
            .env_remove("HEADWORK_SESSION");
        if let Some(role) = role {
            command.env("HEADWORK_ROLE", role);
        }
        command.output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn backups(&self) -> usize {
        std::fs::read_dir(self.home().join("backlog-backups")).map_or(0, Iterator::count)
    }
}

#[test]
fn the_fixture_imports_idempotently_and_the_dry_run_writes_nothing() {
    let m = Machine::new(true);
    std::fs::write(m.file(), fixture()).unwrap();
    let dry = m.ok(&["backlog", "import", "--dry-run"]);
    assert!(
        dry.contains(
            "18 added, 0 already in the store, 1 duplicate in the file, 3 lines not parsed"
        ),
        "{dry}"
    );
    assert!(
        dry.contains("line 18: stray words that belong to nothing"),
        "{dry}"
    );
    assert!(
        m.ok(&["backlog", "list", "--state", "all"])
            .contains("no matching")
    );
    let real = m.ok(&["backlog", "import"]);
    assert!(real.starts_with("imported "), "{real}");
    let again = m.ok(&["backlog", "import"]);
    assert!(
        again.contains("0 added, 18 already in the store, 1 duplicate in the file"),
        "{again}"
    );
    assert_eq!(
        m.ok(&["backlog", "list", "--state", "all", "--json"])
            .matches("\"project\"")
            .count(),
        18
    );
    assert_eq!(
        std::fs::read_to_string(m.file()).unwrap(),
        fixture(),
        "an import never rewrites the file"
    );
}

#[test]
fn export_refuses_a_file_that_was_never_imported_then_keeps_a_copy_of_the_original() {
    let m = Machine::new(true);
    std::fs::write(m.file(), fixture()).unwrap();
    let refused = m.run(&["backlog", "export"]);
    assert!(!refused.status.success(), "refused");
    assert!(
        String::from_utf8_lossy(&refused.stdout).contains("run `headwork backlog import` first"),
        "{}",
        String::from_utf8_lossy(&refused.stdout)
    );
    assert_eq!(std::fs::read_to_string(m.file()).unwrap(), fixture());
    m.ok(&["backlog", "import"]);
    let first = m.ok(&["backlog", "export"]);
    assert!(first.contains("a copy is in"), "{first}");
    assert_eq!(m.backups(), 1);
    let written = std::fs::read_to_string(m.file()).unwrap();
    assert!(
        written.starts_with("# Backlog\n\nGenerated by headwork"),
        "{written}"
    );
    assert!(written.contains("## Done (last 20)"), "{written}");
    assert!(
        !written.contains("Food connector"),
        "dropped items are not exported"
    );
    let same = m.ok(&["backlog", "export"]);
    assert!(same.contains("up to date"), "{same}");
    assert_eq!(m.backups(), 1, "an unchanged export copies nothing");
}

#[test]
fn a_change_exports_and_a_hand_edit_is_backed_up_before_it_is_overwritten() {
    let m = Machine::new(true);
    std::fs::write(m.file(), fixture()).unwrap();
    m.ok(&["backlog", "import"]);
    m.ok(&["backlog", "export"]);
    let added = m.ok(&[
        "backlog",
        "add",
        "headwork",
        "For you: look at the digest",
        "--due",
        "2026-12-24",
        "--note",
        "first note",
    ]);
    assert!(
        added.contains("[headwork] look at the digest (user"),
        "{added}"
    );
    assert!(added.contains("exported to"), "{added}");
    assert_eq!(
        m.backups(),
        1,
        "the export that followed the import's original"
    );
    let text = std::fs::read_to_string(m.file()).unwrap();
    assert!(
        text.contains("## headwork\n\n- [ ] For you: look at the digest (due 2026-12-24) (added "),
        "{text}"
    );
    assert!(text.contains("\n  - first note\n"), "{text}");
    std::fs::write(m.file(), format!("{text}\n- [ ] edited by hand\n")).unwrap();
    let id = added
        .split_whitespace()
        .next()
        .unwrap()
        .trim_start_matches('#')
        .to_owned();
    let done = m.ok(&["backlog", "done", &id, "--note", "looked"]);
    assert!(done.contains("a copy is in"), "{done}");
    assert_eq!(m.backups(), 2);
    let after = std::fs::read_to_string(m.file()).unwrap();
    assert!(!after.contains("edited by hand"), "overwritten");
    assert!(after.contains("(done 20"), "{after}");
    let log = m.ok(&["log", "--limit", "200"]);
    assert!(log.contains("backlog-export-overwrote"), "{log}");
    let edited = m.ok(&["backlog", "edit", &id, "--due", "none", "--text", "renamed"]);
    assert!(edited.contains("renamed"), "{edited}");
    let reopened = m.ok(&["backlog", "reopen", &id]);
    assert!(reopened.contains("renamed (user"), "{reopened}");
}

#[test]
fn without_a_file_every_change_works_and_says_nothing_is_exported() {
    let m = Machine::new(false);
    let added = m.ok(&["backlog", "add", "me-sim", "Plain item"]);
    assert!(added.contains("no backlog file is set"), "{added}");
    let list = m.ok(&["backlog"]);
    assert!(
        list.contains("[me-sim] Plain item (agent, added "),
        "{list}"
    );
    let failed = m.run(&["backlog", "import"]);
    assert!(!failed.status.success(), "import needs a file");
}

fn id_of(line: &str) -> String {
    line.split_whitespace()
        .next()
        .unwrap()
        .trim_start_matches('#')
        .to_owned()
}

#[test]
fn a_deleted_file_is_written_again_and_a_hand_edit_stays_while_the_store_is_unchanged() {
    let m = Machine::new(true);
    std::fs::write(m.file(), fixture()).unwrap();
    m.ok(&["backlog", "import"]);
    m.ok(&["backlog", "export"]);
    std::fs::remove_file(m.file()).unwrap();
    let again = m.ok(&["backlog", "export"]);
    assert!(again.contains("exported to"), "{again}");
    assert!(!again.contains("a copy is in"), "{again}");
    assert!(m.file().exists());
    let edited = format!(
        "{}\n- [ ] by hand\n",
        std::fs::read_to_string(m.file()).unwrap()
    );
    std::fs::write(m.file(), &edited).unwrap();
    let same = m.ok(&["backlog", "export"]);
    assert!(same.contains("up to date"), "{same}");
    assert_eq!(std::fs::read_to_string(m.file()).unwrap(), edited);
}

#[test]
fn a_file_headwork_created_itself_counts_as_imported_so_every_add_exports() {
    let m = Machine::new(true);
    assert!(!m.file().exists());
    for text in ["One", "Two"] {
        let added = m.ok(&["backlog", "add", "me-sim", text]);
        assert!(added.contains("exported to"), "{added}");
        assert!(!added.contains("never imported"), "{added}");
    }
    let written = std::fs::read_to_string(m.file()).unwrap();
    assert!(
        written.contains("- [ ] One") && written.contains("- [ ] Two"),
        "{written}"
    );
}

#[test]
fn a_relative_and_an_absolute_name_of_one_file_are_one_record() {
    let m = Machine::new(false);
    std::fs::write(m.file(), fixture()).unwrap();
    let imported = m.run_in(m.dir.path(), &["backlog", "import", "--file", "backlog.md"]);
    assert!(imported.status.success(), "import");
    let absolute = forward(&m.file());
    let exported = m.ok(&["backlog", "export", "--file", &absolute]);
    assert!(exported.contains("exported to"), "{exported}");
    assert!(!exported.contains("never imported"), "{exported}");
    let relative = m.run_in(
        m.dir.path(),
        &["backlog", "export", "--file", "./backlog.md"],
    );
    assert!(
        String::from_utf8_lossy(&relative.stdout).contains("up to date"),
        "{}",
        String::from_utf8_lossy(&relative.stdout)
    );
}

#[test]
fn an_item_that_cannot_be_written_is_refused_with_the_reason() {
    let m = Machine::new(false);
    for (args, reason) in [
        (
            ["backlog", "add", "Done", "x"],
            "Done, Cancelled or Dropped",
        ),
        (["backlog", "add", "# x", "x"], "must not start with `#`"),
        (["backlog", "add", "me-sim", "  "], "the text is empty"),
        (["backlog", "add", "me-sim", "a\nb"], "must be one line"),
    ] {
        let refused = m.run(&args);
        assert!(!refused.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains(reason),
            "{args:?}: {}",
            String::from_utf8_lossy(&refused.stderr)
        );
    }
    let added = m.ok(&["backlog", "add", "me-sim", "Fine"]);
    let id = id_of(&added);
    let refused = m.run(&["backlog", "edit", &id, "--project", "dropped things"]);
    assert!(!refused.status.success());
    let refused = m.run(&["backlog", "edit", &id, "--text", " "]);
    assert!(!refused.status.success());
    let note = m.ok(&["backlog", "edit", &id, "--append", "  - a note "]);
    assert!(note.contains("Fine"), "{note}");
    assert!(m.ok(&["backlog", "list", "--json"]).contains("\"a note\""));
}

#[test]
fn an_added_text_reads_like_an_imported_line_so_a_later_import_finds_it() {
    let m = Machine::new(true);
    let added = m.ok(&["backlog", "add", "me-sim", "Ship it (due 2026-12-01)"]);
    assert!(added.contains("] Ship it (agent"), "{added}");
    assert!(added.contains("due 2026-12-01"), "{added}");
    let other = m.dir.path().join("other.md");
    std::fs::write(&other, "## me-sim\n\n- [ ] Ship it (due 2026-12-01)\n").unwrap();
    let imported = m.ok(&["backlog", "import", "--file", &forward(&other)]);
    assert!(imported.contains("0 added, 1 already"), "{imported}");
}

#[test]
fn the_owner_column_alone_decides_the_for_you_prefix() {
    let m = Machine::new(true);
    let added = m.ok(&["backlog", "add", "me-sim", "For you (audit): pay the bill"]);
    assert!(added.contains("pay the bill (user"), "{added}");
    assert!(
        std::fs::read_to_string(m.file())
            .unwrap()
            .contains("- [ ] For you: pay the bill")
    );
    let id = id_of(&added);
    let edited = m.ok(&["backlog", "edit", &id, "--owner", "agent"]);
    assert!(edited.contains("pay the bill (agent"), "{edited}");
    let written = std::fs::read_to_string(m.file()).unwrap();
    assert!(written.contains("- [ ] pay the bill"), "{written}");
    assert!(!written.contains("For you"), "{written}");
    let again = m.ok(&["backlog", "edit", &id, "--text", "For you: pay it now"]);
    assert!(again.contains("pay it now (user"), "{again}");
}

#[test]
fn a_file_that_is_not_utf8_is_not_imported_and_the_error_names_it() {
    let m = Machine::new(true);
    std::fs::write(m.file(), [0x23, 0x20, 0xff, 0xfe, 0x0a]).unwrap();
    let refused = m.run(&["backlog", "import"]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("backlog.md") && stderr.contains("not valid UTF-8"),
        "{stderr}"
    );
}

struct World {
    dir: tempfile::TempDir,
    home: Home,
    config: Config,
    store: Store,
    host: FakeHost,
    brief: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "git {args:?}");
}

fn world() -> World {
    world_at("backlog.md")
}

fn world_at(backlog_file: &str) -> World {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("demo");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "master"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let file = forward(&dir.path().join(backlog_file));
    let text = format!(
        "[machine]\nhost = \"wmux\"\nmax_workers = 4\n\n[backlog]\nfile = \"{file}\"\n\n[project.demo]\nmain = \"{}\"\n\
         worktree_new = \"builtin\"\nworktree_remove = \"builtin\"\nworktree_path = \"{{main}}-{{slug}}\"\n\
         deliver = \"pr\"\nmerge = \"deliver\"\nallowed_repos = [\"acme/demo\"]\n\n[watch]\nexit_grace_s = 2\n",
        forward(&main)
    );
    let config = Config::parse(&text, "test").unwrap();
    let home = Home::at(Some(dir.path().join("home")))
        .unwrap()
        .with_claude_json(dir.path().join(".claude.json"))
        .with_claude_dir(dir.path().join(".claude"));
    let mut store = home.open_store().unwrap();
    store.sync_projects(&config).unwrap();
    let brief = dir.path().join("brief.md");
    std::fs::write(&brief, "# Task\n").unwrap();
    std::fs::write(dir.path().join("backlog.md"), "# Backlog\n").unwrap();
    World {
        dir,
        home,
        config,
        store,
        host: FakeHost::new(),
        brief,
    }
}

macro_rules! env {
    ($w:expr) => {
        Env {
            home: &$w.home,
            config: &$w.config,
            host: &$w.host,
            exe: Path::new("S:/bin/headwork.exe"),
            actor: Actor::User,
        }
    };
}

fn request<'a>(brief: &'a Path, slug: &str) -> SpawnRequest<'a> {
    SpawnRequest {
        request: TaskRequest {
            project: ProjectName::new("demo").unwrap(),
            slug: Slug::new(slug).unwrap(),
            kind: "task".to_owned(),
            model: Some(Model::Haiku),
            effort: None,
            in_main: false,
            keep: false,
            close: headwork_core::CloseMode::Confirm,
            review: None,
            decision: None,
        },
        brief,
        depends_on: None,
        permission_mode: PermissionMode::Auto,
        allow_ask: &[],
    }
}

#[test]
fn a_task_spawned_for_an_item_finishes_it_when_the_task_closes_and_the_file_follows() {
    let mut w = world();
    let env = env!(w);
    backlog::import(&mut w.store, &w.home, &w.config, None, false, Actor::User).unwrap();
    let change = backlog::add(
        &mut w.store,
        &env,
        &AddRequest {
            project: "demo".to_owned(),
            text: "Build the thing".to_owned(),
            section: None,
            owner: None,
            not_before: None,
            due: None,
            notes: Vec::new(),
        },
    )
    .unwrap();
    assert!(
        matches!(change.export, Export::Written { .. }),
        "{:?}",
        change.export
    );
    let id = w.store.backlog_items(&BacklogFilter::default()).unwrap()[0].id;
    let refused = {
        let task = backlog::spawn_linked(&mut w.store, &env, &request(&w.brief, "sand"), Some(id))
            .unwrap()
            .task;
        assert_eq!(
            w.store.backlog_item(id).unwrap().task.map(|task| task.name),
            Some("demo-sand".to_owned())
        );
        let in_file = std::fs::read_to_string(w.dir.path().join("backlog.md")).unwrap();
        assert!(
            in_file.contains("[in progress: task demo-sand "),
            "{in_file}"
        );
        let second =
            backlog::spawn_linked(&mut w.store, &env, &request(&w.brief, "other"), Some(id));
        assert!(second.is_err(), "a live task holds the item");
        assert!(
            w.store.find_task("demo-other").unwrap().is_none(),
            "no task is made for a refused link"
        );
        task
    };
    let report = close(&mut w.store, &env, refused.id, None).unwrap();
    assert_eq!(report.state, TaskState::Closed, "{report:?}");
    let item = w.store.backlog_item(id).unwrap();
    assert_eq!(item.draft.status, BacklogStatus::Done);
    assert_eq!(
        item.draft.done_note.as_deref(),
        Some("task demo-sand, no pull request")
    );
    let in_file = std::fs::read_to_string(w.dir.path().join("backlog.md")).unwrap();
    assert!(
        in_file.contains("## Done (last 20)") && in_file.contains("- [x] Build the thing"),
        "{in_file}"
    );
    assert!(
        in_file.contains(": task demo-sand, no pull request)"),
        "{in_file}"
    );
    let kinds: Vec<String> = w
        .store
        .events(EventFilter {
            task: None,
            limit: 300,
        })
        .unwrap()
        .into_iter()
        .map(|event| event.kind)
        .collect();
    assert!(
        kinds.iter().any(|kind| kind == "backlog-moved"),
        "{kinds:?}"
    );
    assert!(
        kinds.iter().any(|kind| kind == "backlog-exported"),
        "{kinds:?}"
    );
}

fn kinds(w: &World) -> Vec<String> {
    w.store
        .events(EventFilter {
            task: None,
            limit: 500,
        })
        .unwrap()
        .into_iter()
        .map(|event| event.kind)
        .collect()
}

fn add_item(store: &mut Store, env: &Env<'_>, text: &str) -> i64 {
    let item = AddRequest {
        project: "demo".to_owned(),
        text: text.to_owned(),
        section: None,
        owner: None,
        not_before: None,
        due: None,
        notes: Vec::new(),
    };
    backlog::add(store, env, &item).unwrap();
    store
        .backlog_items(&BacklogFilter::default())
        .unwrap()
        .last()
        .unwrap()
        .id
}

#[test]
fn a_task_for_an_item_that_is_done_cancelled_or_unknown_is_refused_before_it_exists() {
    let mut w = world();
    let env = env!(w);
    let done = add_item(&mut w.store, &env, "Already done");
    backlog::finish(&mut w.store, &env, done, None).unwrap();
    let dropped = add_item(&mut w.store, &env, "Given up");
    backlog::cancel(&mut w.store, &env, dropped, None).unwrap();
    for (id, slug, words) in [
        (done, "late", "done"),
        (dropped, "gone", "cancelled"),
        (4242, "nobody", "no backlog item"),
    ] {
        let refused = backlog::spawn_linked(&mut w.store, &env, &request(&w.brief, slug), Some(id))
            .unwrap_err();
        assert!(refused.to_string().contains(words), "{refused}");
        assert!(
            w.store
                .find_task(&format!("demo-{slug}"))
                .unwrap()
                .is_none(),
            "no task is made for a refused link"
        );
    }
}

#[test]
fn a_failing_export_after_a_close_is_logged_and_the_close_still_succeeds() {
    let mut w = world_at("blocker/backlog.md");
    std::fs::write(w.dir.path().join("blocker"), "a file, not a folder").unwrap();
    let env = env!(w);
    let spawned =
        backlog::spawn_linked(&mut w.store, &env, &request(&w.brief, "sand"), None).unwrap();
    let report = close(&mut w.store, &env, spawned.task.id, None).unwrap();
    assert_eq!(report.state, TaskState::Closed, "{report:?}");
    assert!(
        kinds(&w).iter().any(|kind| kind == "backlog-export-failed"),
        "{:?}",
        kinds(&w)
    );
}

#[cfg(windows)]
#[test]
fn an_export_that_keeps_failing_to_commit_makes_one_copy_and_one_event() {
    use std::os::windows::fs::OpenOptionsExt;

    let mut w = world();
    let env = env!(w);
    backlog::import(&mut w.store, &w.home, &w.config, None, false, Actor::User).unwrap();
    add_item(&mut w.store, &env, "first");
    let file = w.dir.path().join("backlog.md");
    std::fs::write(&file, "# Backlog\n\nedited by hand\n").unwrap();
    let _held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&file)
        .unwrap();
    let second = AddRequest {
        project: "demo".to_owned(),
        text: "second".to_owned(),
        section: None,
        owner: None,
        not_before: None,
        due: None,
        notes: Vec::new(),
    };
    let change = backlog::add(&mut w.store, &env, &second).unwrap();
    assert!(matches!(change.export, Export::Failed { .. }), "{change:?}");
    for _ in 0..2 {
        let failed = backlog::export(&mut w.store, &w.home, &w.config, None, Actor::User);
        assert!(failed.is_err(), "{failed:?}");
    }
    let copies = std::fs::read_dir(w.home.root().join("backlog-backups"))
        .unwrap()
        .filter(|entry| {
            std::fs::read_to_string(entry.as_ref().unwrap().path())
                .unwrap()
                .contains("edited by hand")
        })
        .count();
    assert_eq!(copies, 1);
    let events = kinds(&w)
        .iter()
        .filter(|kind| *kind == "backlog-export-overwrote")
        .count();
    assert_eq!(
        events, 2,
        "one for the original file, one for the hand edit"
    );
}

fn moved_machine(leave_pointer: bool) -> (Machine, PathBuf) {
    let m = Machine::new(false);
    let old = m.dir.path().join("todo.md");
    std::fs::write(&old, fixture()).unwrap();
    let config = m.home().join("config.toml");
    let base = "[machine]\nhost = \"wmux\"\nmax_workers = 4\n";
    std::fs::write(
        &config,
        format!("{base}\n[todo]\nfile = \"{}\"\n", forward(&old)),
    )
    .unwrap();
    m.ok(&["backlog", "import"]);
    m.ok(&["backlog", "export"]);
    std::fs::write(
        &config,
        format!(
            "{base}\n[backlog]\nfile = \"{}\"\nleave_pointer = {leave_pointer}\n",
            forward(&m.file())
        ),
    )
    .unwrap();
    (m, old)
}

#[test]
fn an_export_to_a_new_file_moves_the_list_and_leaves_the_old_file_as_it_was() {
    let (m, old) = moved_machine(false);
    let before = std::fs::read_to_string(&old).unwrap();
    let moved = m.ok(&["backlog", "export"]);
    assert!(moved.contains("left as it was"), "{moved}");
    assert_eq!(
        std::fs::read_to_string(&old).unwrap(),
        before,
        "the old file is untouched"
    );
    assert_eq!(
        std::fs::read_to_string(m.file()).unwrap(),
        before,
        "the new file holds the list"
    );
    assert!(
        m.ok(&["log"]).contains("backlog-file-moved"),
        "the move is logged"
    );
    let id = id_of(&m.ok(&["backlog", "add", "headwork", "a later item"]));
    assert!(!id.is_empty(), "added");
    assert!(
        std::fs::read_to_string(m.file())
            .unwrap()
            .contains("a later item"),
        "the new file follows"
    );
    assert_eq!(
        std::fs::read_to_string(&old).unwrap(),
        before,
        "later exports leave the old file alone"
    );
}

#[test]
fn with_leave_pointer_the_old_file_keeps_one_pointer_line_and_its_text_is_backed_up() {
    let (m, old) = moved_machine(true);
    let before = std::fs::read_to_string(&old).unwrap();
    let kept = m.backups();
    let moved = m.ok(&["backlog", "export"]);
    assert!(moved.contains("which now points here"), "{moved}");
    let pointer = std::fs::read_to_string(&old).unwrap();
    assert_eq!(pointer.lines().count(), 1, "{pointer}");
    assert!(
        pointer.contains("/backlog.md, generated by headwork"),
        "{pointer}"
    );
    assert_eq!(m.backups(), kept + 1, "the old text is copied first");
    let mut copies: Vec<PathBuf> = std::fs::read_dir(m.home().join("backlog-backups"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    copies.sort();
    assert_eq!(
        std::fs::read_to_string(copies.last().unwrap()).unwrap(),
        before
    );
    m.ok(&["backlog", "add", "headwork", "a later item"]);
    assert_eq!(
        std::fs::read_to_string(&old).unwrap(),
        pointer,
        "the pointer is written once"
    );
    assert_eq!(m.backups(), kept + 1);
}

#[test]
fn an_export_to_another_file_by_name_is_never_a_move() {
    let m = Machine::new(true);
    m.ok(&["backlog", "add", "headwork", "an item"]);
    let preview = m.dir.path().join("preview.md");
    let written = m.ok(&["backlog", "export", "--file", preview.to_str().unwrap()]);
    assert!(!written.contains("moved"), "{written}");
    let configured = std::fs::read_to_string(m.file()).unwrap();
    m.ok(&["backlog", "add", "headwork", "a second item"]);
    let after = std::fs::read_to_string(m.file()).unwrap();
    assert!(after.contains("a second item"), "{after}");
    assert_ne!(after, configured, "the configured file follows the store");
    assert!(
        !m.ok(&["log"]).contains("backlog-file-moved"),
        "no move is logged"
    );
}

#[test]
fn a_pointer_that_cannot_be_written_stops_the_move_and_the_next_export_retries_it() {
    let (m, old) = moved_machine(true);
    let before = std::fs::read_to_string(&old).unwrap();
    let blocker = m.home().join("backlog-backups");
    std::fs::remove_dir_all(&blocker).unwrap();
    std::fs::write(&blocker, "not a folder").unwrap();
    let failed = m.run(&["backlog", "export"]);
    assert!(!failed.status.success(), "the copy fails");
    assert_eq!(
        std::fs::read_to_string(&old).unwrap(),
        before,
        "the old file is as it was"
    );
    assert!(!m.file().exists(), "the new file waits for the move");
    std::fs::remove_file(&blocker).unwrap();
    let moved = m.ok(&["backlog", "export"]);
    assert!(moved.contains("which now points here"), "{moved}");
    assert_eq!(std::fs::read_to_string(&old).unwrap().lines().count(), 1);
    assert!(
        std::fs::read_to_string(m.file()).unwrap().contains("##"),
        "the new file holds the list"
    );
}

#[test]
fn a_copied_new_file_is_imported_first_and_then_moved_with_the_pointer() {
    let (m, old) = moved_machine(true);
    std::fs::copy(&old, m.file()).unwrap();
    let refused = m.run(&["backlog", "export"]);
    assert!(
        !refused.status.success(),
        "a copy never imported is refused"
    );
    assert!(
        String::from_utf8_lossy(&refused.stdout).contains("never imported"),
        "{}",
        String::from_utf8_lossy(&refused.stdout)
    );
    m.ok(&["backlog", "import"]);
    let moved = m.ok(&["backlog", "export"]);
    assert!(moved.contains("which now points here"), "{moved}");
    assert_eq!(std::fs::read_to_string(&old).unwrap().lines().count(), 1);
}

#[test]
fn pointing_back_at_the_old_file_overwrites_the_pointer_without_a_copy() {
    let (m, old) = moved_machine(true);
    m.ok(&["backlog", "export"]);
    let kept = m.backups();
    let config = m.home().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[machine]\nhost = \"wmux\"\nmax_workers = 4\n\n[backlog]\nfile = \"{}\"\n",
            forward(&old)
        ),
    )
    .unwrap();
    let back = m.ok(&["backlog", "export"]);
    assert!(!back.contains("a copy is in"), "{back}");
    assert_eq!(
        m.backups(),
        kept,
        "the pointer is not taken for a hand edit"
    );
    assert!(
        std::fs::read_to_string(&old).unwrap().contains("##"),
        "the list is back"
    );
}

#[test]
fn the_status_commands_move_an_item_through_the_guard() {
    let m = Machine::new(false);
    let id = id_of(&m.ok(&["backlog", "add", "me-sim", "Move me"]));
    let refused = m.run(&["backlog", "start", &id]);
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("its project is not one headwork runs"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    let blocked = m.ok(&[
        "backlog",
        "block",
        &id,
        "--reason",
        "event",
        "--note",
        "the release",
    ]);
    assert!(
        blocked.contains("blocked (event: the release)"),
        "{blocked}"
    );
    let wrong = m.run(&[
        "backlog",
        "block",
        &id,
        "--reason",
        "task_lost",
        "--note",
        "x",
    ]);
    assert!(
        !wrong.status.success(),
        "a blocked item is not blocked again"
    );
    assert!(
        m.ok(&["backlog", "list", "--status", "blocked"])
            .contains("Move me"),
        "listed by status"
    );
    let unblocked = m.ok(&["backlog", "unblock", &id]);
    assert!(!unblocked.contains("blocked"), "{unblocked}");
    let as_orchestrator = m.run_as(
        m.dir.path(),
        Some("orchestrator"),
        &["backlog", "cancel", &id],
    );
    assert!(
        String::from_utf8_lossy(&as_orchestrator.stderr).contains("only with a reason"),
        "{}",
        String::from_utf8_lossy(&as_orchestrator.stderr)
    );
    let as_worker = m.run_as(
        m.dir.path(),
        Some("worker"),
        &["backlog", "cancel", &id, "--reason", "x"],
    );
    assert!(
        String::from_utf8_lossy(&as_worker.stderr)
            .contains("only the item its own task is linked to"),
        "{}",
        String::from_utf8_lossy(&as_worker.stderr)
    );
    m.ok(&["backlog", "cancel", &id, "--reason", "not wanted"]);
    assert!(
        m.ok(&["backlog", "list", "--status", "cancelled"])
            .contains("Move me"),
        "cancelled"
    );
    assert!(
        m.ok(&["backlog", "list", "--state", "dropped"])
            .contains("Move me"),
        "the old word dropped lists cancelled items"
    );
    assert!(
        !m.ok(&["backlog", "list"]).contains("Move me"),
        "the default lists open items"
    );
    let done_cancel = m.run(&["backlog", "cancel", &id]);
    assert!(!done_cancel.status.success(), "already cancelled");
    m.ok(&["backlog", "reopen", &id]);
    m.ok(&["backlog", "done", &id, "--drop", "--note", "old spelling"]);
    assert!(
        m.ok(&["backlog", "list", "--status", "cancelled", "--json"])
            .contains("old spelling"),
        "done --drop cancels"
    );
    m.ok(&["backlog", "reopen", &id]);
    m.ok(&["backlog", "done", &id, "--note", "by hand"]);
    let cancel_done = m.run(&["backlog", "cancel", &id]);
    assert!(
        String::from_utf8_lossy(&cancel_done.stderr).contains("never cancelled"),
        "{}",
        String::from_utf8_lossy(&cancel_done.stderr)
    );
    let moves = m.ok(&["log"]);
    assert!(moves.contains("backlog-moved"), "{moves}");
}

#[test]
fn a_managed_session_without_a_known_role_may_only_list() {
    let m = Machine::new(false);
    let id = id_of(&m.ok(&["backlog", "add", "me-sim", "Guarded"]));
    for role in [None, Some("bogus")] {
        for args in [
            vec!["backlog", "done", id.as_str()],
            vec!["backlog", "add", "me-sim", "Sneaky"],
            vec!["backlog", "import", "--dry-run"],
        ] {
            let refused = m.run_in_session(role, &args);
            assert!(!refused.status.success(), "{role:?} {args:?}");
            assert!(
                String::from_utf8_lossy(&refused.stderr)
                    .contains("HEADWORK_ROLE is missing or unknown"),
                "{role:?} {args:?}: {}",
                String::from_utf8_lossy(&refused.stderr)
            );
        }
        let listed = m.run_in_session(role, &["backlog", "list"]);
        assert!(listed.status.success(), "{role:?}: a listing still works");
    }
}
