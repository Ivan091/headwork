//! Spawn, the queue and the launcher's command against the fake host, a temporary home and a temporary git repo.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use headwork::home::Home;
use headwork::ops::Env;
use headwork::ops::close::close_surface_or_workspace;
use headwork::ops::launch::{StartMode, claude_command, exited, orchestrator_agent};
use headwork::ops::place::IDLE_LABEL;
use headwork::ops::spawn::{Outcome, SpawnRequest, pump, spawn};
use headwork_adapters::host::fake::FakeHost;
use headwork_adapters::host::{PaneId, SurfaceId, Target, TerminalHost, WorkspaceId};
use headwork_core::{
    Actor, Config, Effort, Model, PermissionMode, PlaceKind, ProjectName, SessionState, Slug,
    TaskRequest, TaskState,
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

fn world(max_workers: u32, worktree_new: &str) -> World {
    world_with(max_workers, worktree_new, "")
}

fn world_with(max_workers: u32, worktree_new: &str, extra: &str) -> World {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("demo");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "master"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let main_text = main.display().to_string().replace('\\', "/");
    let text = format!(
        r#"[machine]
host = "wmux"
max_workers = {max_workers}

[project.demo]
main = "{main_text}"
worktree_new = "{worktree_new}"
worktree_remove = "builtin"
worktree_path = "{{main}}-{{slug}}"
deliver = "pr"
merge = "deliver"
main_only = ["probe"]
{extra}
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
            effort: Some(Effort::High),
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
            && calls.get(1).unwrap().ends_with(" pane pane-2"),
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
    assert!(
        calls.iter().any(|c| c.starts_with("split ")),
        "the second child gets a pane of its own: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .rfind(|c| !c.starts_with("rename-surface"))
            .unwrap()
            .ends_with(" replace"),
        "{calls:?}"
    );
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
            reason: "all 1 worker slots are taken".to_owned()
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
        "--effort",
        "high",
        "--",
    ];
    assert_eq!(args.get(..13).unwrap(), expected_start, "{args:?}");
    assert!(
        args.last()
            .unwrap()
            .starts_with("You are the worker session demo-sand. Read your brief at "),
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
        ("HEADWORK_ROLE", "worker".to_owned()),
        ("HEADWORK_TASK", task.id.to_string()),
        ("HEADWORK_EXE", EXE.to_owned()),
    ] {
        assert!(envs.contains(&(key.to_owned(), value)), "{key} in {envs:?}");
    }
    assert_eq!(command.get_current_dir(), Some(dir.as_path()), "cwd");
}

#[test]
fn exited_ends_the_session_and_fails_a_session_that_never_got_going() {
    let mut w = world(10, "builtin");
    let (task, _) = w.spawn("sand", false);
    let session = w.store.session_of(task.id).unwrap().unwrap();
    w.store.session_launched(&session.id, 1, 2, "job").unwrap();
    exited(&w.home, &session.id, Some(task.id), Some(3), 1).unwrap();
    let row = w.store.session_of(task.id).unwrap().unwrap();
    assert_eq!(row.state, SessionState::Ended, "the session ended");
    assert_eq!(row.exit_code, Some(3), "with claude's exit code");
    assert_eq!(
        w.store.task(task.id).unwrap().state,
        TaskState::Failed,
        "a spawning task fails"
    );
}

#[test]
fn the_exited_command_does_the_launchers_exit_work() {
    let mut w = world(10, "builtin");
    let (task, _) = w.spawn("sand", false);
    let session = w.store.session_of(task.id).unwrap().unwrap();
    w.store.session_launched(&session.id, 1, 2, "job").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_headwork"))
        .args([
            "exited",
            "--session",
            &session.id,
            "--launcher-pid",
            "1",
            "--code",
            "-1",
            "--task",
        ])
        .arg(task.id.to_string())
        .arg("--home")
        .arg(w.home.root())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let row = w.store.session_of(task.id).unwrap().unwrap();
    assert_eq!(
        (row.state, row.exit_code),
        (SessionState::Ended, Some(-1)),
        "ended with a negative code"
    );
    assert_eq!(
        w.store.task(task.id).unwrap().state,
        TaskState::Failed,
        "a spawning task fails"
    );
}

fn surface_of(w: &World, task: &headwork_store::Task) -> String {
    w.store
        .session_of(task.id)
        .unwrap()
        .unwrap()
        .host_surface
        .unwrap()
}

fn layout_of(w: &World) -> Vec<(usize, f64, f64)> {
    let workspace = w.host.workspaces().unwrap().into_iter().next().unwrap().id;
    w.host
        .layout(&workspace)
        .unwrap()
        .cells()
        .into_iter()
        .map(|cell| (cell.surfaces.len(), cell.width, cell.height))
        .collect()
}

fn env(w: &World) -> Env<'_> {
    Env {
        home: &w.home,
        config: &w.config,
        host: &w.host,
        exe: Path::new(EXE),
        actor: Actor::User,
    }
}

fn release(w: &World, task: &headwork_store::Task) {
    let session = w.store.session_of(task.id).unwrap().unwrap();
    let workspace = session.host_workspace.map(WorkspaceId);
    close_surface_or_workspace(&env(w), workspace.as_ref(), &SurfaceId(surface_of(w, task)))
        .unwrap();
}

#[test]
fn four_workers_split_into_four_panes_and_the_fifth_stacks_as_a_tab() {
    let mut w = world(10, "builtin");
    let tasks: Vec<_> = ["a", "b", "c", "d", "e"]
        .iter()
        .map(|slug| w.spawn(&format!("task-{slug}"), false).0)
        .collect();
    let places: Vec<_> = tasks
        .iter()
        .map(|task| {
            let session = w.store.session_of(task.id).unwrap().unwrap();
            (session.host_place, session.host_pane)
        })
        .collect();
    let kinds: Vec<_> = places.iter().map(|(kind, _)| *kind).collect();
    assert_eq!(
        kinds,
        [
            Some(PlaceKind::Pane),
            Some(PlaceKind::Pane),
            Some(PlaceKind::Pane),
            Some(PlaceKind::Pane),
            Some(PlaceKind::Tab),
        ],
        "four panes, then a tab"
    );
    let mut panes: Vec<_> = places
        .iter()
        .take(4)
        .map(|(_, pane)| pane.clone())
        .collect();
    panes.dedup();
    assert_eq!(panes.len(), 4, "four different panes: {places:?}");
    assert_eq!(
        layout_of(&w),
        [(1, 0.25, 0.5), (1, 0.25, 0.5), (1, 0.5, 0.5), (2, 0.5, 1.0)],
        "the first pane split three times, the fifth child a tab of the last pane"
    );
    let calls = w.host.calls();
    let splits = calls.iter().filter(|c| c.starts_with("split ")).count();
    assert_eq!(splits, 3, "one split per worker after the first: {calls:?}");
}

fn spawned_placeholders(w: &World) -> usize {
    w.host
        .calls()
        .iter()
        .filter(|c| c.starts_with("spawn ") && c.contains(" headwork:free in "))
        .count()
}

#[test]
fn three_workers_close_one_by_one_and_leave_no_pane() {
    let mut w = world(10, "builtin");
    let tasks: Vec<_> = ["a", "b", "c"]
        .iter()
        .map(|slug| w.spawn(&format!("task-{slug}"), false).0)
        .collect();
    assert_eq!(layout_of(&w).len(), 3, "three panes");
    let mut left = Vec::new();
    for task in &tasks {
        release(&w, task);
        left.push(
            w.host
                .workspaces()
                .unwrap()
                .first()
                .map_or(0, |_| layout_of(&w).len()),
        );
    }
    assert_eq!(left, [2, 1, 0], "each close takes its pane away");
    assert!(
        w.host.workspaces().unwrap().is_empty(),
        "the workspace closed"
    );
    assert_eq!(spawned_placeholders(&w), 0, "{:?}", w.host.calls());
}

#[test]
fn a_close_beside_a_busy_pane_leaves_no_placeholder_and_the_next_spawn_splits_again() {
    let mut w = world(10, "builtin");
    let tasks: Vec<_> = ["a", "b", "c", "d"]
        .iter()
        .map(|slug| w.spawn(&format!("task-{slug}"), false).0)
        .collect();
    let gone = tasks.get(1).unwrap();
    let pane = w.store.session_of(gone.id).unwrap().unwrap().host_pane;
    release(&w, gone);
    let ended = w.store.session_of(gone.id).unwrap().unwrap().id;
    w.store.session_ended(&ended, Some(0)).unwrap();
    assert_eq!(layout_of(&w).len(), 3, "its pane is gone, three stay");
    assert_eq!(spawned_placeholders(&w), 0, "{:?}", w.host.calls());
    let splits = |w: &World| {
        w.host
            .calls()
            .iter()
            .filter(|c| c.starts_with("split "))
            .count()
    };
    let before = splits(&w);
    let (next, _) = w.spawn("task-e", false);
    let session = w.store.session_of(next.id).unwrap().unwrap();
    assert_eq!(
        session.host_place,
        Some(PlaceKind::Pane),
        "a pane of its own"
    );
    assert_ne!(session.host_pane, pane, "a new pane, not the closed one");
    assert_eq!(
        splits(&w),
        before + 1,
        "it split again: {:?}",
        w.host.calls()
    );
    assert_eq!(layout_of(&w).len(), 4, "four panes again");
}

#[test]
fn a_placeholder_an_older_build_left_is_closed() {
    let mut w = world(10, "builtin");
    let (a, _) = w.spawn("task-a", false);
    let (b, _) = w.spawn("task-b", false);
    let workspace = w.host.workspaces().unwrap().into_iter().next().unwrap().id;
    let pane = w
        .store
        .session_of(b.id)
        .unwrap()
        .unwrap()
        .host_pane
        .unwrap();
    let placeholder = w
        .host
        .spawn(
            &workspace,
            Path::new("."),
            IDLE_LABEL,
            &[EXE.to_owned(), "idle".to_owned(), "demo".to_owned()],
            &Target::Pane {
                pane: PaneId(pane),
                replace_tab: false,
            },
        )
        .unwrap();
    release(&w, &b);
    assert!(
        w.host
            .calls()
            .contains(&format!("close-surface {}", placeholder.surface)),
        "{:?}",
        w.host.calls()
    );
    assert_eq!(
        layout_of(&w).len(),
        1,
        "only the first worker's pane is left"
    );
    release(&w, &a);
    assert!(
        w.host.workspaces().unwrap().is_empty(),
        "the workspace closed"
    );
}

#[test]
fn the_workspace_closes_with_its_last_worker() {
    let mut w = world(10, "builtin");
    let (a, _) = w.spawn("task-a", false);
    let (b, _) = w.spawn("task-b", false);
    release(&w, &b);
    assert_eq!(w.host.workspaces().unwrap().len(), 1, "a worker is left");
    release(&w, &a);
    assert!(
        w.host.workspaces().unwrap().is_empty(),
        "shells hold no session: {:?}",
        w.host.calls()
    );
}

#[test]
fn a_free_pane_the_user_left_is_used_before_splitting() {
    let mut w = world(10, "builtin");
    let (first, _) = w.spawn("task-a", false);
    let workspace = w.host.workspaces().unwrap().into_iter().next().unwrap().id;
    let (pane, _shell) = w
        .host
        .split(&SurfaceId(surface_of(&w, &first)), false)
        .unwrap();
    let (second, _) = w.spawn("task-b", false);
    let session = w.store.session_of(second.id).unwrap().unwrap();
    assert_eq!(
        session.host_pane.as_deref(),
        Some(pane.as_str()),
        "the user's pane"
    );
    let calls = w.host.calls();
    assert_eq!(
        calls.iter().filter(|c| c.starts_with("split ")).count(),
        1,
        "only the user's split: {calls:?}"
    );
    let replaced = format!("pane {pane} replace");
    assert!(
        calls
            .iter()
            .rfind(|c| !c.starts_with("rename-surface"))
            .unwrap()
            .ends_with(&replaced),
        "its lone shell is replaced: {calls:?}"
    );
    assert_eq!(
        w.host.layout(&workspace).unwrap().cells().len(),
        2,
        "two panes"
    );
}

#[test]
fn a_split_that_lands_elsewhere_is_closed_and_the_child_stacks() {
    let mut w = world(10, "builtin");
    w.spawn("task-a", false);
    w.spawn("task-b", false);
    w.host.stray_splits();
    let (third, _) = w.spawn("task-c", false);
    let calls = w.host.calls();
    let stray = calls
        .iter()
        .position(|c| c.ends_with(" elsewhere"))
        .unwrap();
    assert!(
        calls.get(stray + 1).unwrap().starts_with("close-pane "),
        "the stray pane is closed: {calls:?}"
    );
    let session = w.store.session_of(third.id).unwrap().unwrap();
    assert_eq!(session.host_place, Some(PlaceKind::Tab), "stacked");
    assert_eq!(layout_of(&w).len(), 2, "still two panes");
}

#[test]
fn tabs_mode_keeps_one_pane() {
    let mut w = world_with(10, "builtin", "\n[layout]\nmode = \"tabs\"\n");
    for slug in ["a", "b", "c"] {
        w.spawn(&format!("task-{slug}"), false);
    }
    let calls = w.host.calls();
    assert!(calls.iter().all(|c| !c.starts_with("split ")), "{calls:?}");
    let spawns: Vec<&String> = calls.iter().filter(|c| c.starts_with("spawn ")).collect();
    assert!(
        spawns.first().unwrap().ends_with(" pane pane-2 replace"),
        "{spawns:?}"
    );
    assert!(
        spawns.iter().skip(1).all(|c| c.ends_with(" tab")),
        "{spawns:?}"
    );
    assert_eq!(layout_of(&w), [(3, 1.0, 1.0)], "three tabs in one pane");
}

#[test]
fn a_limit_of_two_panes_stacks_the_third() {
    let mut w = world_with(10, "builtin", "\n[layout]\npanes = 2\n");
    for slug in ["a", "b", "c"] {
        w.spawn(&format!("task-{slug}"), false);
    }
    assert_eq!(
        layout_of(&w),
        [(1, 0.5, 1.0), (2, 0.5, 1.0)],
        "two panes, the third a tab"
    );
}

#[test]
fn a_pane_is_titled_with_the_task_name_model_and_effort_when_it_opens() {
    let mut w = world(10, "builtin");
    let (task, _) = w.spawn("sand", false);
    let surface = surface_of(&w, &task);
    assert!(
        w.host
            .calls()
            .contains(&format!("rename-surface {surface} demo-sand")),
        "{:?}",
        w.host.calls()
    );
}

#[test]
fn retitle_sets_every_live_pane_or_the_one_named_and_follows_the_layout_template() {
    let mut w = world_with(10, "builtin", "");
    let (sand, _) = w.spawn("sand", false);
    let (dune, _) = w.spawn("dune", false);
    w.config.layout.title = "{effort}|{model}|{name}".to_owned();
    let (all, failed) = headwork::ops::title::retitle(&w.store, &env(&w), None).unwrap();
    assert!(failed.is_empty(), "{failed:?}");
    let titles: Vec<&str> = all.iter().map(|item| item.title.as_str()).collect();
    assert_eq!(
        titles,
        ["high|haiku|demo-sand", "high|haiku|demo-dune"],
        "all live tasks"
    );
    let (one, _) = headwork::ops::title::retitle(&w.store, &env(&w), Some(&dune)).unwrap();
    assert_eq!(one.len(), 1, "one task");
    assert_eq!(
        one.first().unwrap().surface,
        surface_of(&w, &dune),
        "its surface"
    );
    assert_ne!(
        surface_of(&w, &sand),
        surface_of(&w, &dune),
        "distinct panes"
    );
}

#[test]
fn a_spawn_without_an_effort_stores_the_level_the_settings_give_its_model() {
    let mut w = world(10, "builtin");
    let settings = w.dir.path().join("demo").join(".claude");
    std::fs::create_dir(&settings).unwrap();
    std::fs::write(
        settings.join("settings.json"),
        r#"{"effortLevel": "xhigh"}"#,
    )
    .unwrap();
    let mut request = request(&w.brief, "sand", false);
    request.request.effort = None;
    let env = Env {
        home: &w.home,
        config: &w.config,
        host: &w.host,
        exe: Path::new(EXE),
        actor: Actor::User,
    };
    let (task, _) = spawn(&mut w.store, &env, &request).unwrap();
    assert_eq!(
        task.effort,
        Some(Effort::Xhigh),
        "the project's setting is stored"
    );
    let surface = surface_of(&w, &task);
    assert!(
        w.host
            .calls()
            .contains(&format!("rename-surface {surface} demo-sand")),
        "{:?}",
        w.host.calls()
    );
}

#[test]
fn the_orchestrator_runs_its_agent_file_once_it_exists() {
    let claude = tempfile::tempdir().unwrap();
    assert_eq!(
        orchestrator_agent(claude.path()),
        "root",
        "the old agent until then"
    );
    std::fs::create_dir(claude.path().join("agents")).unwrap();
    std::fs::write(claude.path().join("agents/orchestrator.md"), "x").unwrap();
    assert_eq!(
        orchestrator_agent(claude.path()),
        "orchestrator",
        "the new agent"
    );
}
