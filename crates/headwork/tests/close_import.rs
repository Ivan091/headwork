//! Close, reap, queue order and import against the fake host, a temporary home and a temporary git repo.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use headwork::home::Home;
use headwork::ops::Env;
use headwork::ops::close::{close, reap};
use headwork::ops::helpers::add as add_helper;
use headwork::ops::import::{
    Sources, config_section, import, missing_projects, read_transcript, repo_of_remote, worker_name,
};
use headwork::ops::spawn::{Outcome, Pumped, SpawnRequest, pump, spawn};
use headwork_adapters::host::fake::{FAKE_PID_BASE, FakeHost};
use headwork_adapters::host::{SurfaceId, SurfaceState};
use headwork_core::config::ParkPolicy;
use headwork_core::{
    Actor, Config, JobState, Model, PermissionMode, ProjectName, SessionState, Slug, TaskRequest,
    TaskState, Trigger,
};
use headwork_store::{EventFilter, NewJob, Store, Task};

const EXE: &str = "S:/bin/headwork.exe";

struct World {
    dir: tempfile::TempDir,
    main: PathBuf,
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

fn world(max_workers: u32) -> World {
    world_removing(max_workers, "builtin")
}

fn world_removing(max_workers: u32, remove: &str) -> World {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("demo");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "master"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let main_text = main.display().to_string().replace('\\', "/");
    let text = format!(
        "[machine]\nhost = \"wmux\"\nmax_workers = {max_workers}\n\n[project.demo]\nmain = \"{main_text}\"\n\
         worktree_new = \"builtin\"\nworktree_remove = \"{remove}\"\nworktree_path = \"{{main}}-{{slug}}\"\n\
         deliver = \"pr\"\nmerge = \"deliver\"\nmain_only = [\"probe\"]\n"
    );
    let config = Config::parse(&text, "test").unwrap();
    let home = Home::at(Some(dir.path().join("home")))
        .unwrap()
        .with_claude_json(dir.path().join(".claude.json"));
    let mut store = home.open_store().unwrap();
    store.sync_projects(&config).unwrap();
    let brief = dir.path().join("brief.md");
    std::fs::write(&brief, "# Task\n").unwrap();
    World {
        dir,
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

fn spawn_task(w: &mut World, slug: &str, in_main: bool, keep: bool) -> (Task, Vec<Pumped>) {
    let request = SpawnRequest {
        request: TaskRequest {
            project: ProjectName::new("demo").unwrap(),
            slug: Slug::new(slug).unwrap(),
            kind: if in_main { "probe" } else { "task" }.to_owned(),
            model: Some(Model::Haiku),
            effort: None,
            in_main,
            keep,
        },
        brief: &w.brief,
        depends_on: None,
        permission_mode: PermissionMode::Auto,
        allow_ask: &[],
    };
    let env = env!(w);
    spawn(&mut w.store, &env, &request).unwrap()
}

fn event_kinds(store: &Store, task: &Task) -> Vec<String> {
    store
        .events(EventFilter {
            task: Some(task.id),
            limit: 100,
        })
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

#[test]
fn briefs_end_with_how_to_close_except_for_keep_tasks() {
    let mut w = world(10);
    let (task, _) = spawn_task(&mut w, "sand", false, false);
    let brief = std::fs::read_to_string(w.home.brief_path(&task.name)).unwrap();
    assert!(
        brief.contains(&format!("`\"{EXE}\" close demo-sand`")),
        "{brief}"
    );
    let (keeper, _) = spawn_task(&mut w, "probe", true, true);
    let brief = std::fs::read_to_string(w.home.brief_path(&keeper.name)).unwrap();
    assert!(!brief.contains("## Closing yourself"), "{brief}");
}

#[test]
fn close_removes_brief_pane_and_worktree_then_marks_closed() {
    let mut w = world(10);
    let (task, _) = spawn_task(&mut w, "sand", false, false);
    let worktree = w.dir.path().join("demo-sand");
    assert!(worktree.is_dir(), "made");
    let env = env!(w);
    let report = close(&mut w.store, &env, task.id, None).unwrap();
    assert_eq!(report.state, TaskState::Closed, "{report:?}");
    assert_eq!(report.pending, None, "nothing pending");
    assert!(!worktree.exists(), "worktree removed");
    assert!(!w.home.brief_path(&task.name).exists(), "brief removed");
    assert_eq!(
        w.host.calls().last().unwrap(),
        "close-workspace ws-1",
        "the empty workspace closes with its last pane: {:?}",
        w.host.calls()
    );
    let kinds = event_kinds(&w.store, &task);
    for kind in ["worktree-state", "transition"] {
        assert!(kinds.iter().any(|k| k == kind), "{kind} in {kinds:?}");
    }
}

#[test]
fn a_remove_script_that_keeps_a_fresh_worktree_is_overruled_only_when_it_holds_no_work() {
    let mut w = world_removing(10, "echo not merged into master >&2; exit 1");
    let (fresh, _) = spawn_task(&mut w, "fresh", false, false);
    let (worked, _) = spawn_task(&mut w, "worked", false, false);
    let worked_dir = w.dir.path().join("demo-worked");
    git(
        &worked_dir,
        &["commit", "-q", "--allow-empty", "-m", "work"],
    );
    let env = env!(w);
    let report = close(&mut w.store, &env, fresh.id, None).unwrap();
    assert_eq!(report.state, TaskState::Closed, "{report:?}");
    assert!(
        !w.dir.path().join("demo-fresh").exists(),
        "fresh worktree removed"
    );
    assert!(!branch_exists(&w.main, "fresh"), "its empty branch deleted");
    assert!(
        event_kinds(&w.store, &fresh)
            .iter()
            .any(|k| k == "worktree-removed-pristine"),
        "logged"
    );
    let report = close(&mut w.store, &env, worked.id, None).unwrap();
    assert_eq!(report.state, TaskState::Closing, "{report:?}");
    assert!(worked_dir.exists(), "a worktree with work stays");
}

#[test]
fn close_ends_the_sessions_and_jobs_of_the_task() {
    let mut w = world(10);
    let (task, _) = spawn_task(&mut w, "sand", false, false);
    let session = w.store.session_of(task.id).unwrap().unwrap();
    w.store
        .set_session_state(&session.id, SessionState::Live, Actor::Hook)
        .unwrap();
    let argv = vec!["train".to_owned()];
    let job = w
        .store
        .register_job(
            &NewJob {
                task_id: Some(task.id),
                session_id: Some(&session.id),
                project: "demo",
                label: "training",
                park_policy: ParkPolicy::Rerun,
                argv: &argv,
                cwd: ".",
                pid: None,
                identity: None,
                checkpoint_path: None,
                heavy: false,
                restart: true,
                log_path: "train.log",
            },
            Actor::Worker,
        )
        .unwrap();
    let env = env!(w);
    let report = close(&mut w.store, &env, task.id, None).unwrap();
    assert_eq!(report.state, TaskState::Closed, "{report:?}");
    let session = w.store.session_of(task.id).unwrap().unwrap();
    assert_eq!(session.state, SessionState::Ended, "the session ended");
    let job = w.store.job(job).unwrap().unwrap();
    assert_eq!(job.state, JobState::Done, "the job is over");
    assert!(
        w.store.open_jobs(Some(task.id)).unwrap().is_empty(),
        "no open jobs"
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

#[test]
fn close_deletes_an_empty_branch_so_the_slug_can_be_spawned_again() {
    let mut w = world(10);
    let (task, _) = spawn_task(&mut w, "sand", false, false);
    assert!(branch_exists(&w.main, "sand"), "spawn made the branch");
    let env = env!(w);
    let report = close(&mut w.store, &env, task.id, None).unwrap();
    assert_eq!(report.state, TaskState::Closed, "{report:?}");
    assert!(!branch_exists(&w.main, "sand"), "the empty branch is gone");
    assert!(
        event_kinds(&w.store, &task)
            .iter()
            .any(|k| k == "branch-deleted"),
        "logged"
    );
    let (again, _) = spawn_task(&mut w, "sand", false, false);
    assert_eq!(
        w.store.task(again.id).unwrap().state,
        TaskState::Spawning,
        "the slug spawns again"
    );
    assert!(w.dir.path().join("demo-sand").is_dir(), "a new worktree");
}

#[test]
fn close_keeps_a_branch_with_commits_of_its_own() {
    let mut w = world(10);
    let (task, _) = spawn_task(&mut w, "sand", false, false);
    let worktree = w.dir.path().join("demo-sand");
    git(&worktree, &["commit", "-q", "--allow-empty", "-m", "work"]);
    let env = env!(w);
    let report = close(&mut w.store, &env, task.id, None).unwrap();
    assert_eq!(report.state, TaskState::Closed, "{report:?}");
    assert!(!worktree.exists(), "worktree removed");
    assert!(branch_exists(&w.main, "sand"), "the branch with work stays");
    assert!(
        event_kinds(&w.store, &task)
            .iter()
            .any(|k| k == "branch-kept"),
        "logged"
    );
}

#[test]
fn close_also_deletes_the_empty_branch_a_worker_renamed_to() {
    let mut w = world(10);
    let (task, _) = spawn_task(&mut w, "sand", false, false);
    let worktree = w.dir.path().join("demo-sand");
    git(&worktree, &["checkout", "-q", "-b", "portal-pistons"]);
    let env = env!(w);
    let report = close(&mut w.store, &env, task.id, None).unwrap();
    assert_eq!(report.state, TaskState::Closed, "{report:?}");
    assert!(!branch_exists(&w.main, "sand"), "the stored branch is gone");
    assert!(
        !branch_exists(&w.main, "portal-pistons"),
        "the renamed branch is gone"
    );
}

#[test]
fn helper_worktrees_are_made_beside_the_task_and_removed_with_it() {
    let mut w = world(10);
    let (task, _) = spawn_task(&mut w, "sand", false, false);
    let env = env!(w);
    let made = add_helper(&mut w.store, &env, &task, "sand-part").unwrap();
    let dir = w.dir.path().join("demo-sand-part");
    assert!(dir.is_dir(), "made beside the task's own: {made:?}");
    assert!(!made.existing, "new");
    assert!(
        add_helper(&mut w.store, &env, &task, "sand-part")
            .unwrap()
            .existing,
        "the same again"
    );
    assert!(
        add_helper(&mut w.store, &env, &task, "sand").is_err(),
        "the task's own slug"
    );
    assert!(
        add_helper(&mut w.store, &env, &task, "Not Kebab").is_err(),
        "a bad slug"
    );
    assert_eq!(
        w.store.helper_worktrees(task.id).unwrap().len(),
        1,
        "recorded"
    );
    assert_eq!(
        w.store.worktree_of(task.id).unwrap().unwrap().branch,
        "sand",
        "the task's own worktree is still its own"
    );
    let report = close(&mut w.store, &env, task.id, None).unwrap();
    assert_eq!(report.state, TaskState::Closed, "{report:?}");
    assert!(!dir.exists(), "the helper went with the task");
    assert!(
        w.store.helper_worktrees(task.id).unwrap().is_empty(),
        "recorded removed"
    );
    assert!(
        !branch_exists(&w.main, "sand-part"),
        "its empty branch is gone"
    );
}

#[test]
fn a_helper_worktree_with_work_stays_and_never_holds_up_the_close() {
    let mut w = world(10);
    let (task, _) = spawn_task(&mut w, "sand", false, false);
    let env = env!(w);
    add_helper(&mut w.store, &env, &task, "sand-part").unwrap();
    let dir = w.dir.path().join("demo-sand-part");
    std::fs::write(dir.join("work.txt"), "unsaved").unwrap();
    let report = close(&mut w.store, &env, task.id, None).unwrap();
    assert_eq!(report.state, TaskState::Closed, "{report:?}");
    assert!(dir.join("work.txt").exists(), "the work stays");
    assert!(
        event_kinds(&w.store, &task)
            .iter()
            .any(|k| k == "helper-worktree-refused"),
        "logged"
    );
}

#[test]
fn spawn_reuses_a_leftover_empty_branch_but_not_one_with_work() {
    let mut w = world(10);
    git(&w.main, &["branch", "sand"]);
    git(&w.main, &["commit", "-q", "--allow-empty", "-m", "later"]);
    let (task, _) = spawn_task(&mut w, "sand", false, false);
    assert_eq!(
        w.store.task(task.id).unwrap().state,
        TaskState::Spawning,
        "the empty branch is reused"
    );
    let worktree = w.dir.path().join("demo-sand");
    let head = |dir: &Path| {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    };
    assert_eq!(
        head(&worktree),
        head(&w.main),
        "reset to the main checkout's HEAD"
    );

    git(&w.main, &["branch", "rock"]);
    git(&w.main, &["checkout", "-q", "rock"]);
    git(
        &w.main,
        &["commit", "-q", "--allow-empty", "-m", "rock work"],
    );
    git(&w.main, &["checkout", "-q", "master"]);
    let (rock, _) = spawn_task(&mut w, "rock", false, false);
    assert_eq!(
        w.store.task(rock.id).unwrap().state,
        TaskState::Failed,
        "a branch with work is never reset"
    );
    assert!(!w.dir.path().join("demo-rock").exists(), "no worktree");
    assert!(branch_exists(&w.main, "rock"), "and stays");
}

#[test]
fn closing_one_of_two_panes_keeps_the_workspace() {
    let mut w = world(10);
    let (first, _) = spawn_task(&mut w, "one", false, false);
    spawn_task(&mut w, "two", false, false);
    let env = env!(w);
    close(&mut w.store, &env, first.id, None).unwrap();
    let calls = w.host.calls();
    assert!(
        calls.last().unwrap().starts_with("close-surface "),
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("close-workspace")),
        "{calls:?}"
    );
}

#[test]
fn a_dirty_worktree_keeps_the_task_closing_until_it_can_go() {
    let mut w = world(10);
    let (task, _) = spawn_task(&mut w, "sand", false, false);
    let worktree = w.dir.path().join("demo-sand");
    std::fs::write(worktree.join("work.txt"), "unsaved").unwrap();
    let env = env!(w);
    let report = close(&mut w.store, &env, task.id, None).unwrap();
    assert_eq!(report.state, TaskState::Closing, "{report:?}");
    assert!(
        report.pending.as_deref().unwrap().contains("worktree"),
        "{report:?}"
    );
    assert!(worktree.exists(), "never deleted by force");
    assert!(
        event_kinds(&w.store, &task)
            .iter()
            .any(|k| k == "worktree-refused"),
        "logged"
    );
    std::fs::remove_file(worktree.join("work.txt")).unwrap();
    let reaped = reap(&mut w.store, &env).unwrap();
    assert_eq!(
        reaped.first().unwrap().state,
        TaskState::Closed,
        "{reaped:?}"
    );
}

#[test]
fn an_empty_folder_git_no_longer_counts_as_a_worktree_closes() {
    let mut w = world(10);
    let (task, _) = spawn_task(&mut w, "stale", false, false);
    let worktree = w.dir.path().join("demo-stale");
    git(
        &w.main,
        &["worktree", "remove", "--force", worktree.to_str().unwrap()],
    );
    std::fs::create_dir(&worktree).unwrap();
    let env = env!(w);
    let report = close(&mut w.store, &env, task.id, None).unwrap();
    assert_eq!(report.state, TaskState::Closed, "{report:?}");
    assert!(!worktree.exists(), "the empty folder is deleted");
}

#[test]
fn a_stray_folder_with_files_is_refused_and_kept() {
    let mut w = world(10);
    let (task, _) = spawn_task(&mut w, "stray", false, false);
    let worktree = w.dir.path().join("demo-stray");
    git(
        &w.main,
        &["worktree", "remove", "--force", worktree.to_str().unwrap()],
    );
    std::fs::create_dir(&worktree).unwrap();
    std::fs::write(worktree.join("notes.txt"), "mine").unwrap();
    let env = env!(w);
    let report = close(&mut w.store, &env, task.id, None).unwrap();
    assert_eq!(report.state, TaskState::Closing, "{report:?}");
    assert!(worktree.join("notes.txt").exists(), "never deleted");
    assert!(
        event_kinds(&w.store, &task)
            .iter()
            .any(|k| k == "worktree-refused"),
        "logged"
    );
}

#[test]
fn closing_a_queued_task_closes_it_at_once_and_the_queue_moves() {
    let mut w = world(1);
    let (running, _) = spawn_task(&mut w, "one", false, false);
    let (queued, _) = spawn_task(&mut w, "two", false, false);
    assert_eq!(queued.state, TaskState::Queued, "waits");
    let env = env!(w);
    let report = close(&mut w.store, &env, queued.id, None).unwrap();
    assert_eq!(report.state, TaskState::Closed, "queued closes at once");
    let (third, _) = spawn_task(&mut w, "three", false, false);
    assert_eq!(third.state, TaskState::Queued, "still one slot");
    let env = env!(w);
    close(&mut w.store, &env, running.id, None).unwrap();
    let pumped = pump(&mut w.store, &env).unwrap();
    assert!(
        matches!(
            pumped.first().map(|p| &p.outcome),
            Some(Outcome::Started { .. })
        ),
        "{pumped:?}"
    );
    assert_eq!(
        w.store.task(third.id).unwrap().state,
        TaskState::Spawning,
        "started"
    );
}

#[test]
fn queue_order_puts_named_tasks_first() {
    let mut w = world(1);
    spawn_task(&mut w, "one", false, false);
    let (a, _) = spawn_task(&mut w, "a", false, false);
    let (b, _) = spawn_task(&mut w, "b", false, false);
    let (c, _) = spawn_task(&mut w, "c", false, false);
    assert_eq!(
        w.store.queued().unwrap(),
        [a.id, b.id, c.id],
        "oldest first"
    );
    w.store.set_queue_order(&[c.id, b.id], Actor::User).unwrap();
    assert_eq!(
        w.store.queued().unwrap(),
        [c.id, b.id, a.id],
        "ranked first"
    );
    w.store
        .transition(a.id, &Trigger::ForceClose, Actor::User)
        .unwrap();
    assert!(
        w.store.set_queue_order(&[a.id], Actor::User).is_err(),
        "only queued tasks can be ordered"
    );
}

struct Imported {
    claude_dir: PathBuf,
    briefs: PathBuf,
    restored: SurfaceId,
}

/// A labelled wmux agent in a worktree, a restored pane in the main checkout known only by its transcript, a pane of
/// no project, and a stray worktree.
fn import_world(w: &World) -> Imported {
    let worktree = w.dir.path().join("demo-old");
    git(
        &w.main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "old",
            &worktree.display().to_string(),
        ],
    );
    let stray = w.dir.path().join("demo-stray");
    git(
        &w.main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "stray",
            &stray.display().to_string(),
        ],
    );
    let agent = w
        .host
        .add_foreign_pane("demo", "demo/old", FAKE_PID_BASE + 100);
    w.host.add_state(SurfaceState {
        surface: agent.surface,
        state: "working".to_owned(),
        session_id: Some("11111111-1111-4111-8111-111111111111".to_owned()),
        blocked_reason: None,
    });
    let restored = w.host.add_restored_surface("demo");
    let session = "22222222-2222-4222-8222-222222222222";
    w.host.add_state(SurfaceState {
        surface: restored.clone(),
        state: "idle".to_owned(),
        session_id: Some(session.to_owned()),
        blocked_reason: None,
    });
    w.host.add_state(SurfaceState {
        surface: w.host.add_restored_surface("Workspace 1"),
        state: "idle".to_owned(),
        session_id: Some("33333333-3333-4333-8333-333333333333".to_owned()),
        blocked_reason: None,
    });
    let claude_dir = w.dir.path().join("claude");
    let transcripts = claude_dir.join("projects").join("S--demo");
    std::fs::create_dir_all(&transcripts).unwrap();
    let cwd = w.main.display().to_string();
    let lines = [
        serde_json::json!({ "type": "summary", "cwd": cwd }),
        serde_json::json!({
            "type": "user", "cwd": cwd,
            "message": { "role": "user", "content": "You are the worker session demo-probe. Read your brief at x.md." },
        }),
        serde_json::json!({ "type": "assistant", "message": { "model": "claude-opus-5-5", "content": [] } }),
    ];
    let text: Vec<String> = lines.iter().map(ToString::to_string).collect();
    std::fs::write(
        transcripts.join(format!("{session}.jsonl")),
        text.join("\n"),
    )
    .unwrap();
    let briefs = claude_dir.join("briefs");
    std::fs::create_dir_all(&briefs).unwrap();
    std::fs::write(briefs.join("demo-probe.md"), "# Keep running\n").unwrap();
    std::fs::write(briefs.join("demo-old.md"), "# Old\n\n## Closing yourself\n").unwrap();
    Imported {
        claude_dir,
        briefs,
        restored,
    }
}

#[test]
fn import_adopts_labelled_agents_and_restored_panes() {
    let mut w = world(10);
    let Imported {
        claude_dir,
        briefs,
        restored,
    } = import_world(&w);
    let sources = Sources {
        claude_dir: &claude_dir,
        briefs_dir: &briefs,
    };
    let env = env!(w);

    let dry = import(&mut w.store, &env, sources, true).unwrap();
    assert_eq!(dry.adopted, ["demo-old", "demo-probe"], "{dry:?}");
    assert!(
        w.store.find_task("demo-old").unwrap().is_none(),
        "a dry run changes nothing"
    );

    let report = import(&mut w.store, &env, sources, false).unwrap();
    assert_eq!(report.adopted, ["demo-old", "demo-probe"], "{report:?}");
    assert!(
        report
            .stray_worktrees
            .iter()
            .any(|p| p.ends_with("demo-stray")),
        "{report:?}"
    );
    let old = w.store.find_task("demo-old").unwrap().unwrap();
    assert_eq!(old.state, TaskState::Working, "adopted as working");
    assert!(!old.in_main && !old.keep, "{old:?}");
    assert_eq!(
        w.store.worktree_of(old.id).unwrap().unwrap().branch,
        "old",
        "branch from git"
    );
    let old_session = w.store.session_of(old.id).unwrap().unwrap();
    assert_eq!(
        old_session.id, "11111111-1111-4111-8111-111111111111",
        "session id from wmux"
    );
    assert_eq!(old_session.claude_pid, Some(FAKE_PID_BASE + 100), "pid");
    let probe = w.store.find_task("demo-probe").unwrap().unwrap();
    assert!(probe.in_main && probe.keep, "{probe:?}");
    assert_eq!(probe.model, Model::Opus, "model from the transcript");
    assert_eq!(
        w.store
            .session_of(probe.id)
            .unwrap()
            .unwrap()
            .host_surface
            .as_deref(),
        Some(restored.as_str()),
        "surface"
    );
    let again = import(&mut w.store, &env, sources, false).unwrap();
    assert!(again.adopted.is_empty(), "idempotent: {again:?}");
    assert_eq!(again.skipped.len(), 2, "{again:?}");
}

#[test]
fn import_helpers() {
    assert_eq!(
        worker_name("You are the worker session me-sim-sand. Read it."),
        Some("me-sim-sand".to_owned()),
        "name"
    );
    assert_eq!(
        worker_name("You are the child session me-sim-sand. Read it."),
        Some("me-sim-sand".to_owned()),
        "the orchestrate scripts' wording"
    );
    assert_eq!(worker_name("Hello"), None, "no name");
    assert_eq!(
        repo_of_remote("git@github.com:Ivan091/me-sim.git"),
        Some("Ivan091/me-sim".to_owned()),
        "ssh"
    );
    assert_eq!(
        repo_of_remote("https://github.com/acme/service"),
        Some("acme/service".to_owned()),
        "https"
    );
    let config = Config::parse(
        "[machine]\nhost = \"wmux\"\n[project.demo]\nmain = \"x\"\nworktree_new = \"builtin\"\n\
         worktree_remove = \"builtin\"\nworktree_path = \"{main}-{slug}\"\ndeliver = \"pr\"\nmerge = \"deliver\"\n",
        "t",
    )
    .unwrap();
    let missing = missing_projects(
        "demo=x\nme-sim=S:/Code/me-sim/me-sim\n# note\n\nBad Name=y\n",
        &config,
    );
    assert_eq!(
        missing,
        [("me-sim".to_owned(), "S:/Code/me-sim/me-sim".to_owned())],
        "only new, valid names"
    );
    let section = config_section("tool", "Z:/none", Some("git@github.com:o/tool.git"));
    let merged = format!("[machine]\nhost = \"wmux\"\n{section}");
    let parsed = Config::parse(&merged, "merged").unwrap();
    let tool = parsed.project(&ProjectName::new("tool").unwrap()).unwrap();
    assert_eq!(
        tool.allowed_repos.first().unwrap().as_str(),
        "o/tool",
        "repo"
    );
    let facts = read_transcript(Path::new("Z:/missing.jsonl"));
    assert_eq!(facts.name, None, "a missing transcript says nothing");
}

fn sleeper() -> std::process::Child {
    if cfg!(windows) {
        Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .spawn()
            .unwrap()
    } else {
        Command::new("sleep").arg("30").spawn().unwrap()
    }
}

#[test]
fn a_self_close_of_a_session_from_before_hot_reload_starts_a_backup_finisher() {
    let mut w = world(10);
    let mut launchers = Vec::new();
    for (slug, recorded) in [("old", false), ("new", true)] {
        let (task, _) = spawn_task(&mut w, slug, false, false);
        let session = w.store.session_of(task.id).unwrap().unwrap();
        let (launcher, claude) = (sleeper(), sleeper());
        w.store
            .session_launched(&session.id, launcher.id(), claude.id(), "job")
            .unwrap();
        if recorded {
            w.store
                .append_event(
                    Actor::Worker,
                    Some(task.id),
                    Some(&session.id),
                    "build",
                    &serde_json::json!({ "stamp": "1:1" }),
                )
                .unwrap();
        }
        let env = env!(w);
        let report = close(&mut w.store, &env, task.id, Some(&session.id)).unwrap();
        assert_eq!(
            report.state,
            TaskState::Closing,
            "the launcher finishes: {report:?}"
        );
        let backup = event_kinds(&w.store, &task)
            .iter()
            .any(|kind| kind == "backup-finisher");
        assert_eq!(
            backup, !recorded,
            "{slug}: a backup finisher only without a build record"
        );
        launchers.push(launcher);
    }
    for mut launcher in launchers {
        launcher.kill().unwrap_or(());
        launcher.wait().unwrap();
    }
}

fn stale_job(w: &mut World, task: &Task, pid: u32, policy: ParkPolicy) -> i64 {
    let argv = vec!["bash".to_owned(), "scripts/rl-run.sh".to_owned()];
    let identity =
        headwork_adapters::proc::identify(pid).map(|now| headwork_core::process::ProcessIdentity {
            started: now.started - 3600,
            ..now
        });
    w.store
        .register_job(
            &NewJob {
                task_id: Some(task.id),
                session_id: None,
                project: "demo",
                label: "sac32",
                park_policy: policy,
                argv: &argv,
                cwd: ".",
                pid: Some(pid),
                identity: identity.as_ref(),
                checkpoint_path: Some("out/checkpoint.json"),
                heavy: true,
                restart: true,
                log_path: "train.log",
            },
            Actor::Worker,
        )
        .unwrap()
}

fn assert_left_alone(w: &World, job: i64, other: &mut std::process::Child) {
    let row = w.store.job(job).unwrap().unwrap();
    assert_eq!(row.state, JobState::Done, "over, not parked: {row:?}");
    assert!(
        row.note
            .as_deref()
            .is_some_and(|note| note.contains("another program")),
        "{row:?}"
    );
    assert!(
        headwork_adapters::proc::is_alive(other.id()).unwrap(),
        "the program that took the pid still runs"
    );
    other.kill().unwrap();
    other.wait().unwrap();
}

#[test]
fn a_job_whose_pid_another_program_took_is_never_stopped() {
    let mut w = world(10);
    let (task, _) = spawn_task(&mut w, "sand", false, false);
    let mut other = sleeper();
    let job = stale_job(&mut w, &task, other.id(), ParkPolicy::Checkpoint);
    let row = w.store.job(job).unwrap().unwrap();
    assert!(
        !headwork::ops::jobs::has_running_job(&w.store, task.id).unwrap(),
        "does not count as running"
    );
    headwork::ops::jobs::stop(&mut w.store, &row, "stopped by its session", Actor::Worker).unwrap();
    assert_left_alone(&w, job, &mut other);
}

#[test]
fn refreshing_the_jobs_marks_a_taken_over_pid_over() {
    let mut w = world(10);
    let (task, _) = spawn_task(&mut w, "sand", false, false);
    let mut other = sleeper();
    let job = stale_job(&mut w, &task, other.id(), ParkPolicy::Checkpoint);
    let open = headwork::ops::jobs::refresh(&mut w.store, Actor::Tick).unwrap();
    assert!(open.iter().all(|row| row.id != job), "not listed");
    assert_left_alone(&w, job, &mut other);
}

#[test]
fn closing_a_task_leaves_a_program_that_took_its_job_pid() {
    let mut w = world(10);
    let (task, _) = spawn_task(&mut w, "sand", false, false);
    let mut other = sleeper();
    let job = stale_job(&mut w, &task, other.id(), ParkPolicy::Rerun);
    let env = env!(w);
    close(&mut w.store, &env, task.id, None).unwrap();
    assert_left_alone(&w, job, &mut other);
}

#[test]
fn a_job_from_before_identities_under_an_unrelated_program_is_left_alone() {
    let mut w = world(10);
    let (task, _) = spawn_task(&mut w, "sand", false, false);
    let mut other = sleeper();
    let argv = vec!["bash".to_owned(), "scripts/rl-run.sh".to_owned()];
    let job = w
        .store
        .register_job(
            &NewJob {
                task_id: Some(task.id),
                session_id: None,
                project: "demo",
                label: "sac32",
                park_policy: ParkPolicy::Checkpoint,
                argv: &argv,
                cwd: ".",
                pid: Some(other.id()),
                identity: None,
                checkpoint_path: Some("out/checkpoint.json"),
                heavy: true,
                restart: true,
                log_path: "train.log",
            },
            Actor::Worker,
        )
        .unwrap();
    let row = w.store.job(job).unwrap().unwrap();
    headwork::ops::jobs::stop(&mut w.store, &row, "stopped for the park", Actor::User).unwrap();
    assert_left_alone(&w, job, &mut other);
}

#[test]
fn forgetting_a_job_touches_no_process() {
    let mut w = world(10);
    let (task, _) = spawn_task(&mut w, "sand", false, false);
    let mut other = sleeper();
    let argv = vec!["ping".to_owned()];
    let identity = headwork_adapters::proc::identify(other.id());
    let job = w
        .store
        .register_job(
            &NewJob {
                task_id: Some(task.id),
                session_id: None,
                project: "demo",
                label: "pinger",
                park_policy: ParkPolicy::Rerun,
                argv: &argv,
                cwd: ".",
                pid: Some(other.id()),
                identity: identity.as_ref(),
                checkpoint_path: None,
                heavy: false,
                restart: true,
                log_path: "ping.log",
            },
            Actor::Worker,
        )
        .unwrap();
    assert!(
        headwork::ops::jobs::forget(&mut w.store, job, "  ", Actor::User).is_err(),
        "a reason is required"
    );
    let row =
        headwork::ops::jobs::forget(&mut w.store, job, "stopped and forked", Actor::User).unwrap();
    assert_eq!(
        (row.state, row.note.as_deref()),
        (JobState::Done, Some("forgotten: stopped and forked")),
        "over"
    );
    assert!(
        headwork::ops::jobs::forget(&mut w.store, job, "again", Actor::User).is_err(),
        "already over"
    );
    assert!(
        headwork::ops::jobs::forget(&mut w.store, 9999, "none", Actor::User).is_err(),
        "unknown"
    );
    assert!(
        headwork_adapters::proc::is_alive(other.id()).unwrap(),
        "the process still runs"
    );
    other.kill().unwrap();
    other.wait().unwrap();
}

fn merged_pr(state: headwork_adapters::git::PrState) -> headwork_adapters::git::PrInfo {
    headwork_adapters::git::PrInfo {
        repo: "acme/demo".to_owned(),
        number: 7,
        url: "https://github.com/acme/demo/pull/7".to_owned(),
        state,
    }
}

#[test]
fn force_finishes_a_stuck_close_by_keeping_the_refused_worktree() {
    use headwork::ops::close::close_forced;
    use headwork_adapters::git::{FakeGit, PrState};
    let mut w = world_removing(10, "echo not merged into master >&2; exit 1");
    let (task, _) = spawn_task(&mut w, "worked", false, false);
    let dir = w.dir.path().join("demo-worked");
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "work"]);
    let env = env!(w);
    let git_host = FakeGit::new();
    let refused = close_forced(&mut w.store, &env, &git_host, task.id).unwrap_err();
    assert!(
        refused.to_string().contains("stuck in closing"),
        "{refused}"
    );
    let report = close(&mut w.store, &env, task.id, None).unwrap();
    assert_eq!(report.state, TaskState::Closing, "stuck: {report:?}");
    let none = close_forced(&mut w.store, &env, &git_host, task.id).unwrap_err();
    assert!(none.to_string().contains("has none"), "{none}");
    git_host.set("worked", merged_pr(PrState::Open));
    let open = close_forced(&mut w.store, &env, &git_host, task.id).unwrap_err();
    assert!(open.to_string().contains("is open"), "{open}");
    assert_eq!(
        w.store.task(task.id).unwrap().state,
        TaskState::Closing,
        "unchanged"
    );
    git_host.set("worked", merged_pr(PrState::Merged));
    let report = close_forced(&mut w.store, &env, &git_host, task.id).unwrap();
    assert_eq!(report.state, TaskState::Closed, "{report:?}");
    assert!(dir.exists(), "the worktree stays on disk");
    let row = w.store.worktree_of(task.id).unwrap().unwrap();
    assert_eq!(row.state, "kept", "recorded kept");
    let kinds = event_kinds(&w.store, &task);
    for kind in ["force-finish", "worktree-kept"] {
        assert!(kinds.iter().any(|k| k == kind), "{kind} in {kinds:?}");
    }
}

#[test]
fn force_is_refused_while_a_job_runs_or_for_a_worker() {
    use headwork::ops::close::close_forced;
    use headwork_adapters::git::{FakeGit, PrState};
    let mut w = world_removing(10, "echo not merged into master >&2; exit 1");
    let (task, _) = spawn_task(&mut w, "worked", false, false);
    let dir = w.dir.path().join("demo-worked");
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "work"]);
    let env = env!(w);
    close(&mut w.store, &env, task.id, None).unwrap();
    let argv = vec!["train".to_owned()];
    w.store
        .register_job(
            &NewJob {
                task_id: Some(task.id),
                session_id: None,
                project: "demo",
                label: "training",
                park_policy: ParkPolicy::Rerun,
                argv: &argv,
                cwd: ".",
                pid: None,
                identity: None,
                checkpoint_path: None,
                heavy: false,
                restart: true,
                log_path: "train.log",
            },
            Actor::Worker,
        )
        .unwrap();
    let git_host = FakeGit::new();
    git_host.set("worked", merged_pr(PrState::Merged));
    let job = close_forced(&mut w.store, &env, &git_host, task.id).unwrap_err();
    assert!(job.to_string().contains("no live job"), "{job}");
    let worker = Env {
        actor: Actor::Worker,
        ..env!(w)
    };
    let refused = close_forced(&mut w.store, &worker, &git_host, task.id).unwrap_err();
    assert!(refused.to_string().contains("worker"), "{refused}");
    assert_eq!(
        w.store.task(task.id).unwrap().state,
        TaskState::Closing,
        "unchanged"
    );
}
