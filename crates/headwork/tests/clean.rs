//! `headwork clean` against the fake host, a temporary home and a temporary git repository.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use headwork::home::Home;
use headwork::ops::Env;
use headwork::ops::clean::{
    CleanOptions, CleanReport, GcOutcome, Outcome, WorktreeClean, clean, is_agent_worktree,
};
use headwork::ops::close::close;
use headwork::ops::spawn::{SpawnRequest, spawn};
use headwork_adapters::host::fake::FakeHost;
use headwork_adapters::worktree::same_path;
use headwork_core::{
    Actor, Config, Model, PermissionMode, ProjectName, Slug, TaskRequest, TaskState,
};
use headwork_store::{Store, Task};

const EXE: &str = "S:/bin/headwork.exe";
const DEAD_PID: u32 = 999_999;

struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    main: PathBuf,
    home: Home,
    config: Config,
    store: Store,
    host: FakeHost,
    brief: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
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
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn branch_exists(main: &Path, branch: &str) -> bool {
    Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .current_dir(main)
        .output()
        .unwrap()
        .status
        .success()
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let long = std::fs::canonicalize(dir.path()).unwrap();
    let root = PathBuf::from(long.display().to_string().trim_start_matches(r"\\?\"));
    let main = root.join("demo");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "master"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let main_text = main.display().to_string().replace('\\', "/");
    let text = format!(
        "[machine]\nhost = \"wmux\"\nmax_workers = 10\n\n[project.demo]\nmain = \"{main_text}\"\n\
         worktree_new = \"builtin\"\nworktree_remove = \"builtin\"\nworktree_path = \"{{main}}-{{slug}}\"\n\
         deliver = \"pr\"\nmerge = \"deliver\"\nmain_only = [\"probe\"]\n"
    );
    let config = Config::parse(&text, "test").unwrap();
    let home = Home::at(Some(root.join("home")))
        .unwrap()
        .with_claude_json(root.join(".claude.json"))
        .with_claude_dir(root.join(".claude"));
    let mut store = home.open_store().unwrap();
    store.sync_projects(&config).unwrap();
    let brief = root.join("brief.md");
    std::fs::write(&brief, "# Task\n").unwrap();
    World {
        _dir: dir,
        root,
        main,
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
            exe: Path::new(EXE),
            actor: Actor::User,
        }
    };
}

fn spawn_task(w: &mut World, slug: &str, in_main: bool) -> Task {
    let request = SpawnRequest {
        request: TaskRequest {
            project: ProjectName::new("demo").unwrap(),
            slug: Slug::new(slug).unwrap(),
            kind: if in_main { "probe" } else { "task" }.to_owned(),
            model: Some(Model::Haiku),
            effort: None,
            in_main,
            keep: false,
            close: headwork_core::CloseMode::Confirm,
            review: None,
            decision: None,
        },
        brief: &w.brief,
        depends_on: None,
        permission_mode: PermissionMode::Auto,
        allow_ask: &[],
        epic: None,
    };
    let env = env!(w);
    spawn(&mut w.store, &env, &request).unwrap().0
}

/// A closed task whose worktree stayed behind, as one kept at close does.
fn closed_with_worktree(w: &mut World, slug: &str) -> PathBuf {
    let task = spawn_task(w, slug, false);
    w.store
        .set_worktree_state(task.id, "kept", Actor::User)
        .unwrap();
    let env = env!(w);
    let report = close(&mut w.store, &env, task.id, None).unwrap();
    assert_eq!(report.state, TaskState::Closed, "{report:?}");
    let worktree = w.root.join(format!("demo-{slug}"));
    assert!(worktree.is_dir(), "kept");
    worktree
}

fn run(w: &mut World, dry_run: bool, gc: bool) -> CleanReport {
    let env = env!(w);
    clean(&mut w.store, &env, CleanOptions { dry_run, gc }).unwrap()
}

fn entry<'a>(report: &'a CleanReport, path: &Path) -> &'a WorktreeClean {
    let wanted = path.display().to_string().replace('\\', "/");
    report.projects[0]
        .worktrees
        .iter()
        .find(|worktree| same_path(Path::new(&worktree.path), path))
        .unwrap_or_else(|| panic!("{wanted} not in {report:?}"))
}

fn left_reason(worktree: &WorktreeClean) -> &str {
    match &worktree.outcome {
        Outcome::Left { reason } => reason,
        other @ (Outcome::WouldRemove
        | Outcome::Removed
        | Outcome::WouldPrune
        | Outcome::Pruned
        | Outcome::Failed { .. }) => panic!("not left: {other:?}"),
    }
}

fn agent_worktree(w: &World, name: &str) -> PathBuf {
    let path = w.main.join(".claude/worktrees").join(name);
    let text = path.display().to_string().replace('\\', "/");
    git(&w.main, &["worktree", "add", "-q", "-b", name, &text]);
    path
}

fn lock(w: &World, path: &Path, reason: &str) {
    let text = path.display().to_string().replace('\\', "/");
    git(&w.main, &["worktree", "lock", "--reason", reason, &text]);
}

#[test]
fn a_closed_tasks_leftover_worktree_is_planned_then_removed() {
    let mut w = world();
    let worktree = closed_with_worktree(&mut w, "sand");
    let planned = run(&mut w, true, false);
    assert!(planned.dry_run, "a dry run");
    let item = entry(&planned, &worktree);
    assert_eq!(item.outcome, Outcome::WouldRemove, "{item:?}");
    assert_eq!(item.owner, "task demo-sand", "{item:?}");
    assert!(worktree.is_dir(), "a dry run changes nothing");

    let done = run(&mut w, false, false);
    assert_eq!(
        entry(&done, &worktree).outcome,
        Outcome::Removed,
        "{done:?}"
    );
    assert!(!worktree.exists(), "gone");
    assert!(!branch_exists(&w.main, "sand"), "its empty branch went too");
    let task = w.store.find_task("demo-sand").unwrap();
    let row = task.map(|task| w.store.worktree_of(task.id).unwrap().unwrap());
    assert_eq!(row.unwrap().state, "removed", "recorded");
    assert!(!done.failed(), "{done:?}");
}

#[test]
fn clean_tries_a_kept_worktree_whose_old_failures_are_still_on_record() {
    let mut w = world();
    let worktree = closed_with_worktree(&mut w, "sand");
    let task = w.store.find_task("demo-sand").unwrap().unwrap();
    w.store
        .record_removal_failure(task.id, "stale: Permission denied", Actor::User)
        .unwrap();
    let done = run(&mut w, false, false);
    assert_eq!(
        entry(&done, &worktree).outcome,
        Outcome::Removed,
        "{done:?}"
    );
    assert!(!worktree.exists(), "gone");
}

#[test]
fn a_failed_clean_reports_the_stored_error_and_leaves_a_kept_worktree_kept() {
    let mut w = world();
    let dirty = closed_with_worktree(&mut w, "sand");
    std::fs::write(dirty.join("work.txt"), "unsaved").unwrap();
    let task = w.store.find_task("demo-sand").unwrap().unwrap();
    w.store
        .record_removal_failure(task.id, "stale", Actor::User)
        .unwrap();
    let report = run(&mut w, false, false);
    assert!(
        left_reason(entry(&report, &dirty)).contains("uncommitted"),
        "{report:?}"
    );
    assert_eq!(w.store.worktree_of(task.id).unwrap().unwrap().state, "kept");
}

#[test]
fn a_dirty_worktree_and_a_live_tasks_worktree_are_left_with_their_reasons() {
    let mut w = world();
    let dirty = closed_with_worktree(&mut w, "sand");
    std::fs::write(dirty.join("work.txt"), "unsaved").unwrap();
    spawn_task(&mut w, "rock", false);
    let live = w.root.join("demo-rock");
    let report = run(&mut w, false, false);
    assert!(
        left_reason(entry(&report, &dirty)).contains("uncommitted"),
        "{report:?}"
    );
    assert!(dirty.join("work.txt").exists(), "never removed");
    assert!(
        left_reason(entry(&report, &live)).contains("spawning"),
        "{report:?}"
    );
    assert!(live.is_dir(), "a live task's worktree stays");
}

#[test]
fn a_worktree_no_task_owns_is_listed_and_left() {
    let mut w = world();
    let other = w.root.join("demo-by-hand");
    let text = other.display().to_string().replace('\\', "/");
    git(&w.main, &["worktree", "add", "-q", "--detach", &text]);
    let report = run(&mut w, false, false);
    let item = entry(&report, &other);
    assert_eq!(item.owner, "nobody", "{item:?}");
    assert!(left_reason(item).contains("no headwork task"), "{item:?}");
    assert!(other.is_dir(), "left");
}

#[test]
fn a_merged_agent_worktree_whose_agent_is_gone_goes_with_its_branch() {
    let mut w = world();
    let path = agent_worktree(&w, "agent-done");
    lock(
        &w,
        &path,
        &format!("claude agent agent-done (pid {DEAD_PID})"),
    );
    assert!(is_agent_worktree(&w.main, &path), "recognised");
    let planned = run(&mut w, true, false);
    let item = entry(&planned, &path);
    assert_eq!(item.outcome, Outcome::WouldRemove, "{item:?}");
    assert_eq!(item.owner, "agent", "{item:?}");

    let done = run(&mut w, false, false);
    assert_eq!(entry(&done, &path).outcome, Outcome::Removed, "{done:?}");
    assert!(!path.exists(), "gone");
    assert!(
        !branch_exists(&w.main, "agent-done"),
        "merged branch deleted"
    );
}

#[test]
fn an_agent_worktree_whose_agent_runs_or_whose_work_is_unmerged_stays() {
    let mut w = world();
    let running = agent_worktree(&w, "agent-running");
    lock(
        &w,
        &running,
        &format!("claude agent agent-running (pid {})", std::process::id()),
    );
    let unmerged = agent_worktree(&w, "agent-unmerged");
    git(
        &unmerged,
        &["commit", "-q", "--allow-empty", "-m", "only here"],
    );
    let dirty = agent_worktree(&w, "agent-dirty");
    std::fs::write(dirty.join("scratch.txt"), "x").unwrap();
    let foreign = agent_worktree(&w, "agent-foreign");
    lock(&w, &foreign, "on a removable disk");

    let report = run(&mut w, false, false);
    assert!(
        left_reason(entry(&report, &running)).contains("still runs"),
        "{report:?}"
    );
    assert!(
        left_reason(entry(&report, &unmerged)).contains("lacks"),
        "{report:?}"
    );
    assert!(
        left_reason(entry(&report, &dirty)).contains("uncommitted"),
        "{report:?}"
    );
    assert!(
        left_reason(entry(&report, &foreign)).contains("removable disk"),
        "{report:?}"
    );
    for path in [&running, &unmerged, &dirty, &foreign] {
        assert!(path.is_dir(), "{} stays", path.display());
    }
    assert!(branch_exists(&w.main, "agent-unmerged"), "its work stays");
}

#[test]
fn an_agent_worktree_whose_work_landed_as_another_commit_goes_and_keeps_its_branch() {
    let mut w = world();
    let path = agent_worktree(&w, "agent-squashed");
    std::fs::write(path.join("a.txt"), "one").unwrap();
    git(&path, &["add", "a.txt"]);
    git(&path, &["commit", "-q", "-m", "feat: the change"]);
    std::fs::write(w.main.join("a.txt"), "one, reviewed").unwrap();
    git(&w.main, &["add", "a.txt"]);
    git(&w.main, &["commit", "-q", "-m", "feat: the change"]);

    let report = run(&mut w, false, false);
    let item = entry(&report, &path);
    assert_eq!(item.outcome, Outcome::Removed, "{item:?}");
    assert!(item.note.as_deref().unwrap().contains("kept"), "{item:?}");
    assert!(!path.exists(), "gone");
    assert!(branch_exists(&w.main, "agent-squashed"), "its branch stays");
}

#[test]
fn an_agent_worktree_whose_folder_is_gone_is_forgotten() {
    let mut w = world();
    let path = agent_worktree(&w, "agent-stale");
    lock(
        &w,
        &path,
        &format!("claude agent agent-stale (pid {DEAD_PID})"),
    );
    std::fs::remove_dir_all(&path).unwrap();
    let planned = run(&mut w, true, false);
    assert_eq!(
        entry(&planned, &path).outcome,
        Outcome::WouldPrune,
        "{planned:?}"
    );
    let done = run(&mut w, false, false);
    assert_eq!(entry(&done, &path).outcome, Outcome::Pruned, "{done:?}");
    let again = run(&mut w, false, false);
    assert!(again.projects[0].worktrees.is_empty(), "{again:?}");
}

#[test]
fn gc_runs_only_where_no_main_only_task_runs() {
    let mut w = world();
    let planned = run(&mut w, true, true);
    assert_eq!(
        planned.projects[0].gc,
        Some(GcOutcome::WouldRun),
        "{planned:?}"
    );
    let ran = run(&mut w, false, true);
    assert_eq!(ran.projects[0].gc, Some(GcOutcome::Ran), "{ran:?}");
    let quiet = run(&mut w, false, false);
    assert_eq!(quiet.projects[0].gc, None, "only when asked");

    spawn_task(&mut w, "probe", true);
    let skipped = run(&mut w, false, true);
    match &skipped.projects[0].gc {
        Some(GcOutcome::Skipped { reason }) => {
            assert!(reason.contains("demo-probe"), "{reason}");
        }
        other => panic!("not skipped: {other:?}"),
    }
}
