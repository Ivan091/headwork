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
use headwork::ops::import::{
    Sources, config_section, import, missing_projects, read_transcript, repo_of_remote, worker_name,
};
use headwork::ops::spawn::{Outcome, Pumped, SpawnRequest, pump, spawn};
use headwork_adapters::host::fake::{FAKE_PID_BASE, FakeHost};
use headwork_adapters::host::{SurfaceId, SurfaceState};
use headwork_core::{
    Actor, Config, Model, PermissionMode, ProjectName, Slug, TaskRequest, TaskState, Trigger,
};
use headwork_store::{EventFilter, Store, Task};

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
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("demo");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "master"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let main_text = main.display().to_string().replace('\\', "/");
    let text = format!(
        "[machine]\nhost = \"wmux\"\nmax_workers = {max_workers}\n\n[project.demo]\nmain = \"{main_text}\"\n\
         worktree_new = \"builtin\"\nworktree_remove = \"builtin\"\nworktree_path = \"{{main}}-{{slug}}\"\n\
         deliver = \"pr\"\nmerge = \"deliver\"\nmain_only = [\"probe\"]\n"
    );
    let config = Config::parse(&text, "test").unwrap();
    let home = Home::at(Some(dir.path().join("home"))).unwrap();
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
