//! Spawn, the queue and the launcher's command against the fake host, a temporary home and a temporary git repo.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use headwork::home::Home;
use headwork::ops::Env;
use headwork::ops::launch::{StartMode, claude_command};
use headwork::ops::spawn::{Outcome, SpawnRequest, pump, spawn};
use headwork_adapters::host::TerminalHost;
use headwork_adapters::host::fake::FakeHost;
use headwork_core::{
    Actor, Config, Model, PermissionMode, ProjectName, SessionState, Slug, TaskRequest, TaskState,
};
use headwork_store::{EventFilter, Store};

const EXE: &str = "S:/bin/headwork.exe";

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

fn world(max_children: u32, worktree_new: &str) -> World {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("demo");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "master"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let main_text = main.display().to_string().replace('\\', "/");
    let text = format!(
        r#"[machine]
host = "wmux"
max_children = {max_children}

[project.demo]
main = "{main_text}"
worktree_new = "{worktree_new}"
worktree_remove = "builtin"
worktree_path = "{{main}}-{{slug}}"
deliver = "pr"
merge = "deliver"
main_only = ["probe"]
"#
    );
    let config = Config::parse(&text, "test").unwrap();
    let home = Home::at(Some(dir.path().join("home"))).unwrap();
    let mut store = home.open_store().unwrap();
    store.sync_projects(&config).unwrap();
    let brief = dir.path().join("brief.md");
    std::fs::write(&brief, "# Task\n\nSay ready.\n").unwrap();
    World {
        dir,
        home,
        config,
        store,
        host: FakeHost::new(),
        brief,
    }
}

fn request<'a>(brief: &'a Path, slug: &str, in_main: bool) -> SpawnRequest<'a> {
    SpawnRequest {
        request: TaskRequest {
            project: ProjectName::new("demo").unwrap(),
            slug: Slug::new(slug).unwrap(),
            kind: if in_main { "probe" } else { "task" }.to_owned(),
            model: Some(Model::Haiku),
            in_main,
            keep: false,
        },
        brief,
        depends_on: None,
        permission_mode: PermissionMode::Auto,
    }
}

impl World {
    fn spawn(
        &mut self,
        slug: &str,
        in_main: bool,
    ) -> (headwork_store::Task, Vec<headwork::ops::spawn::Pumped>) {
        let request = request(&self.brief, slug, in_main);
        let env = Env {
            home: &self.home,
            config: &self.config,
            host: &self.host,
            exe: Path::new(EXE),
            actor: Actor::User,
        };
        spawn(&mut self.store, &env, &request).unwrap()
    }
}

#[test]
fn spawn_makes_the_worktree_the_workspace_and_the_launcher_pane() {
    let mut w = world(10, "builtin");
    let (task, pumped) = w.spawn("sand", false);
    assert_eq!(task.state, TaskState::Spawning, "started at once");
    assert!(
        matches!(
            pumped.first().map(|p| &p.outcome),
            Some(Outcome::Started { .. })
        ),
        "{pumped:?}"
    );
    let worktree = w.dir.path().join("demo-sand");
    assert!(worktree.is_dir(), "the worktree exists");
    assert_eq!(
        w.store.worktree_of(task.id).unwrap().unwrap().branch,
        "sand",
        "worktree recorded"
    );
    let brief = w.home.brief_path(&task.name);
    assert!(
        std::fs::read_to_string(&brief)
            .unwrap()
            .contains("Say ready."),
        "brief copied"
    );
    let calls = w.host.calls();
    assert_eq!(calls.first().unwrap(), "new-workspace demo", "{calls:?}");
    assert!(
        calls
            .get(1)
            .unwrap()
            .starts_with("spawn ws-1 demo/sand in ")
            && calls.get(1).unwrap().ends_with("fresh=true"),
        "{calls:?}"
    );
    let pane = w.host.panes().unwrap().into_iter().next().unwrap();
    assert!(
        pane.command.starts_with(&format!(
            "S:/bin/headwork.exe launch --task {} --home ",
            task.id
        )),
        "{}",
        pane.command
    );
    let session = w.store.session_of(task.id).unwrap().unwrap();
    assert_eq!(
        session.state,
        SessionState::Starting,
        "starting until the launcher runs"
    );
    assert_eq!(session.rc_name, "demo-sand", "name");
    assert_eq!(
        session.host_surface.as_deref(),
        Some(pane.surface.as_str()),
        "surface"
    );
    assert_eq!(session.id.len(), 36, "a uuid session id");
}

#[test]
fn main_checkout_tasks_need_no_worktree_and_share_the_workspace() {
    let mut w = world(10, "builtin");
    w.spawn("sand", false);
    let (probe, _) = w.spawn("probe", true);
    assert_eq!(probe.state, TaskState::Spawning, "started");
    assert!(
        w.store.worktree_of(probe.id).unwrap().is_none(),
        "no worktree"
    );
    let calls = w.host.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.starts_with("new-workspace"))
            .count(),
        1,
        "one workspace per project: {calls:?}"
    );
    assert!(calls.last().unwrap().ends_with("fresh=false"), "{calls:?}");
}

#[test]
fn tasks_beyond_the_cap_wait_in_the_queue_and_start_when_a_slot_frees() {
    let mut w = world(1, "builtin");
    let (first, _) = w.spawn("one", false);
    let (second, pumped) = w.spawn("two", false);
    assert_eq!(second.state, TaskState::Queued, "waits");
    assert_eq!(
        pumped.first().map(|p| &p.outcome),
        Some(&Outcome::Waiting {
            reason: "all 1 child slots are taken".to_owned()
        }),
        "{pumped:?}"
    );
    w.store
        .transition(first.id, &headwork_core::Trigger::LaunchFailed, Actor::User)
        .unwrap();
    let env = Env {
        home: &w.home,
        config: &w.config,
        host: &w.host,
        exe: Path::new(EXE),
        actor: Actor::User,
    };
    let pumped = pump(&mut w.store, &env).unwrap();
    assert!(
        matches!(
            pumped.first().map(|p| &p.outcome),
            Some(Outcome::Started { .. })
        ),
        "{pumped:?}"
    );
    assert_eq!(
        w.store.task(second.id).unwrap().state,
        TaskState::Spawning,
        "started"
    );
}

#[test]
fn a_failing_worktree_command_fails_the_task_and_logs_why() {
    let mut w = world(10, "echo cannot make {slug} >&2; exit 4");
    let (task, pumped) = w.spawn("sand", false);
    assert_eq!(task.state, TaskState::Failed, "failed");
    assert!(
        matches!(pumped.first().map(|p| &p.outcome), Some(Outcome::Failed { error }) if error.contains("cannot make sand")),
        "{pumped:?}"
    );
    let events = w
        .store
        .events(EventFilter {
            task: Some(task.id),
            limit: 20,
        })
        .unwrap();
    assert!(events.iter().any(|e| e.kind == "launch-error"), "logged");
    assert!(
        w.host.calls().iter().all(|c| !c.starts_with("spawn")),
        "no pane"
    );
}

#[test]
fn the_launcher_starts_claude_with_identity_name_model_and_mode() {
    let mut w = world(10, "builtin");
    let (task, _) = w.spawn("sand", false);
    let session = w.store.session_of(task.id).unwrap().unwrap();
    let dir = w.dir.path().join("demo-sand");
    let command = claude_command(
        &w.home,
        &w.config,
        &task,
        &session,
        &dir,
        Path::new(EXE),
        StartMode::New,
    )
    .unwrap();
    assert_eq!(command.get_program(), "claude", "program");
    let args: Vec<String> = command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let expected_start = [
        "--session-id",
        session.id.as_str(),
        "--remote-control",
        "demo-sand",
        "-n",
        "demo-sand",
        "--model",
        "haiku",
        "--permission-mode",
        "auto",
        "--",
    ];
    assert_eq!(args.get(..11).unwrap(), expected_start, "{args:?}");
    assert!(
        args.last()
            .unwrap()
            .starts_with("You are the child session demo-sand. Read your brief at "),
        "{args:?}"
    );
    let envs: Vec<(String, String)> = command
        .get_envs()
        .filter_map(|(k, v)| {
            Some((
                k.to_string_lossy().into_owned(),
                v?.to_string_lossy().into_owned(),
            ))
        })
        .collect();
    for (key, value) in [
        ("HEADWORK_SESSION", session.id.clone()),
        ("HEADWORK_ROLE", "child".to_owned()),
        ("HEADWORK_TASK", task.id.to_string()),
        ("HEADWORK_EXE", EXE.to_owned()),
    ] {
        assert!(envs.contains(&(key.to_owned(), value)), "{key} in {envs:?}");
    }
    assert_eq!(command.get_current_dir(), Some(dir.as_path()), "cwd");
}
