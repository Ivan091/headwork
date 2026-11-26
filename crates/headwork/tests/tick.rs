//! The tick against the fake host, the fake git host, fake disks and a temporary home: one test per finding.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use headwork::home::Home;
use headwork::ops::Env;
use headwork::ops::lost::{self, Lost, LostCause};
use headwork::ops::rules::RuleFiles;
use headwork::ops::spawn::{SpawnRequest, spawn};
use headwork::ops::tick::{
    DELETE_WORKTREE, KEEP_WORKTREE, Status, TRUST_FOLDER, TickEnv, TickReport, tick,
};
use headwork::ops::update::{Started, UpdateLock, UpdateStarter, UpdateState};
use headwork_adapters::disk::FakeDisks;
use headwork_adapters::git::{FakeGit, PrInfo, PrState};
use headwork_adapters::host::fake::{FAKE_PID_BASE, FakeHost};
use headwork_adapters::host::{SurfaceId, TerminalHost};
use headwork_adapters::proc::is_alive;
use headwork_core::config::ParkPolicy;
use headwork_core::lifecycle::SessionFacts;
use headwork_core::watch::Limits;
use headwork_core::{
    Actor, Config, FindingId, MachineMode, Model, PermissionMode, ProjectName, SessionState, Slug,
    TaskRequest, TaskState, Trigger,
};
use headwork_store::{EventFilter, NewJob, PrSeen, Store, Task};

const GIB: u64 = 1024 * 1024 * 1024;
const SLEEPER_ROLE: &str = "HEADWORK_TICK_ROLE";

struct World {
    dir: tempfile::TempDir,
    home: Home,
    config: Config,
    config_text: String,
    store: Store,
    host: FakeHost,
    git: FakeGit,
    disks: FakeDisks,
    limits: Limits,
    brief: PathBuf,
    children: Vec<Child>,
    updates: FakeStarter,
    slots: headwork::ops::waiting::FakeSlots,
}

/// Records the updates the tick starts instead of starting them.
#[derive(Debug, Default)]
struct FakeStarter {
    started: std::cell::RefCell<Vec<Vec<String>>>,
}

impl UpdateStarter for FakeStarter {
    fn start(&self, args: &[String]) -> anyhow::Result<Started> {
        self.started.borrow_mut().push(args.to_vec());
        Ok(Started {
            pid: 99,
            note: None,
        })
    }
}

fn git_in(dir: &Path, args: &[&str]) {
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

const fn roomy_limits() -> Limits {
    Limits {
        disk_warn: 15 * GIB,
        disk_stop: 5 * GIB,
        growth: GIB,
        file: 20 * GIB,
        stuck_after: 3600,
        idle_free_merged: Some(3600),
        idle_free_review: Some(3600),
        spawn_timeout: 120,
        ask_remind: 7200,
        merged_grace: 600,
        closing_grace: 60,
    }
}

fn world_with(enforce: bool, max_workers: u32) -> World {
    world_extra(enforce, max_workers, "")
}

fn world_extra(enforce: bool, max_workers: u32, extra: &str) -> World {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("demo");
    std::fs::create_dir(&main).unwrap();
    git_in(&main, &["init", "-q", "-b", "master"]);
    git_in(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let main_text = main.display().to_string().replace('\\', "/");
    let text = format!(
        "[machine]\nhost = \"wmux\"\nmax_workers = {max_workers}\n\n[hooks]\nenforce = {enforce}\n\n\
         [project.demo]\nmain = \"{main_text}\"\nworktree_new = \"builtin\"\nworktree_remove = \"builtin\"\n\
         worktree_path = \"{{main}}-{{slug}}\"\ndeliver = \"pr\"\nmerge = \"deliver\"\n\
         allowed_repos = [\"acme/demo\"]\n{extra}"
    );
    let config = Config::parse(&text, "test").unwrap();
    let config_text = text;
    let home = Home::at(Some(dir.path().join("home")))
        .unwrap()
        .with_claude_json(dir.path().join(".claude.json"))
        .with_claude_dir(dir.path().join(".claude"));
    let mut store = home.open_store().unwrap();
    store.sync_projects(&config).unwrap();
    let hash = RuleFiles::load(&home, &config, &[], "orchestrator")
        .unwrap()
        .hash()
        .to_owned();
    store.observe_rules(&hash, Actor::User).unwrap();
    let brief = dir.path().join("brief.md");
    std::fs::write(&brief, "# Task\n").unwrap();
    let disks = FakeDisks::new();
    disks.set(dir.path(), 100 * GIB);
    disks.set(&home_dir(), 100 * GIB);
    World {
        dir,
        home,
        config,
        config_text,
        store,
        host: FakeHost::new(),
        git: FakeGit::new(),
        disks,
        limits: roomy_limits(),
        brief,
        children: Vec::new(),
        updates: FakeStarter::default(),
        slots: headwork::ops::waiting::FakeSlots::default(),
    }
}

fn home_dir() -> PathBuf {
    std::env::home_dir().unwrap()
}

fn world() -> World {
    world_with(false, 10)
}

fn spawn_task(w: &mut World, slug: &str) -> Task {
    let request = SpawnRequest {
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
        brief: &w.brief,
        depends_on: None,
        permission_mode: PermissionMode::Auto,
        allow_ask: &[],
    };
    let env = Env {
        home: &w.home,
        config: &w.config,
        host: &w.host,
        exe: Path::new("S:/bin/headwork.exe"),
        actor: Actor::User,
    };
    spawn(&mut w.store, &env, &request).unwrap().0
}

fn run_tick(w: &mut World) -> TickReport {
    let env = Env {
        home: &w.home,
        config: &w.config,
        host: &w.host,
        exe: Path::new("S:/bin/headwork.exe"),
        actor: Actor::Tick,
    };
    tick(
        &mut w.store,
        &TickEnv {
            env: &env,
            git: &w.git,
            disks: &w.disks,
            temp: w.dir.path(),
            limits: w.limits,
            updates: &w.updates,
            slots: &w.slots,
        },
    )
    .unwrap()
}

fn start(w: &mut World, task: &Task, launcher: u32, claude: u32) -> String {
    let session = w.store.session_of(task.id).unwrap().unwrap();
    w.store
        .session_launched(&session.id, launcher, claude, "job")
        .unwrap();
    let version = w.store.rules_version().unwrap().version;
    w.store
        .set_rules_seen(&session.id, Some(task.id), version, 0)
        .unwrap();
    w.store
        .transition(
            task.id,
            &Trigger::SessionStarted(SessionFacts {
                session_matches: true,
                launcher_alive: true,
            }),
            Actor::Hook,
        )
        .unwrap();
    session.id
}

fn working(w: &mut World, slug: &str) -> (Task, String) {
    let task = spawn_task(w, slug);
    let child = sleeper();
    let pid = child.id();
    w.children.push(child);
    let session = start(w, &task, pid, pid);
    (w.store.task(task.id).unwrap(), session)
}

fn state(w: &World, task: &Task) -> TaskState {
    w.store.task(task.id).unwrap().state
}

fn count(w: &World, kind: &str) -> usize {
    w.store
        .events(EventFilter {
            task: None,
            limit: 10_000,
        })
        .unwrap()
        .iter()
        .filter(|e| e.kind == kind)
        .count()
}

fn rules(report: &TickReport) -> Vec<(FindingId, Status)> {
    report.lines.iter().map(|l| (l.rule, l.status)).collect()
}

fn pr(state: PrState) -> PrInfo {
    PrInfo {
        repo: "acme/demo".to_owned(),
        number: 7,
        url: "https://github.com/acme/demo/pull/7".to_owned(),
        state,
        created_at: None,
    }
}

fn sleeper() -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "helper_sleeper",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(SLEEPER_ROLE, "sleeper")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn wait_dead(pid: u32) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if !is_alive(pid).unwrap() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
#[ignore = "a helper started by the tick tests"]
fn helper_sleeper() {
    if std::env::var_os(SLEEPER_ROLE).is_some() {
        std::thread::sleep(Duration::from_secs(120));
    }
}

#[test]
fn a_quiet_world_reports_nothing_and_a_second_tick_adds_no_events() {
    let mut w = world();
    working(&mut w, "sand");
    let first = run_tick(&mut w);
    assert!(first.lines.is_empty(), "{:?}", first.lines);
    let events = count(&w, "finding");
    run_tick(&mut w);
    assert_eq!(count(&w, "finding"), events, "idempotent");
}

#[test]
fn a_session_that_never_starts_fails_after_the_spawn_timeout() {
    let mut w = world();
    let task = spawn_task(&mut w, "sand");
    run_tick(&mut w);
    assert_eq!(state(&w, &task), TaskState::Spawning, "inside the timeout");
    w.limits.spawn_timeout = 0;
    let report = run_tick(&mut w);
    assert_eq!(state(&w, &task), TaskState::Failed, "after the timeout");
    assert_eq!(
        rules(&report),
        [(FindingId::SpawnTimeout, Status::Applied)],
        "finding"
    );
    assert!(
        !w.host
            .calls()
            .iter()
            .any(|c| c.starts_with("close-workspace")),
        "shadow mode leaves the pane alone: {:?}",
        w.host.calls()
    );
}

#[test]
fn enforcing_the_spawn_timeout_closes_the_pane() {
    let mut w = world_with(true, 10);
    let task = spawn_task(&mut w, "sand");
    w.limits.spawn_timeout = 0;
    run_tick(&mut w);
    assert_eq!(state(&w, &task), TaskState::Failed, "failed");
    assert!(
        w.host
            .calls()
            .iter()
            .any(|c| c.starts_with("close-workspace")),
        "{:?}",
        w.host.calls()
    );
}

#[test]
fn a_vanished_pane_interrupts_the_task() {
    let mut w = world();
    let (task, _) = working(&mut w, "sand");
    let session = w.store.session_of(task.id).unwrap().unwrap();
    w.host
        .close_surface(&SurfaceId(session.host_surface.unwrap()))
        .unwrap();
    let report = run_tick(&mut w);
    assert_eq!(state(&w, &task), TaskState::Interrupted, "interrupted");
    assert_eq!(
        rules(&report),
        [(FindingId::PaneLost, Status::Applied)],
        "finding"
    );
}

#[test]
fn a_dead_launcher_interrupts_the_task() {
    let mut w = world();
    let task = spawn_task(&mut w, "sand");
    start(&mut w, &task, FAKE_PID_BASE, FAKE_PID_BASE);
    run_tick(&mut w);
    assert_eq!(state(&w, &task), TaskState::Interrupted, "interrupted");
}

fn reconcile(w: &mut World) -> Vec<Lost> {
    let env = Env {
        home: &w.home,
        config: &w.config,
        host: &w.host,
        exe: Path::new("S:/bin/headwork.exe"),
        actor: Actor::User,
    };
    lost::reconcile(&mut w.store, &env).unwrap()
}

#[test]
fn reconcile_interrupts_a_task_whose_launcher_is_gone_and_leaves_live_ones() {
    let mut w = world();
    let gone = spawn_task(&mut w, "gone");
    start(&mut w, &gone, FAKE_PID_BASE, FAKE_PID_BASE);
    let (live, _) = working(&mut w, "live");
    let moved = reconcile(&mut w);
    assert_eq!(
        moved,
        [Lost {
            task: gone.id,
            from: TaskState::Working,
            cause: LostCause::Launcher
        }],
        "only the dead one"
    );
    assert_eq!(state(&w, &gone), TaskState::Interrupted, "interrupted");
    assert_eq!(state(&w, &live), TaskState::Working, "the live one runs on");
    let session = w.store.session_of(gone.id).unwrap().unwrap();
    assert_eq!(session.state, SessionState::Gone, "the session is gone");
    assert!(reconcile(&mut w).is_empty(), "a second run changes nothing");
}

#[test]
fn reconcile_interrupts_a_task_whose_session_ended_while_its_launcher_lingers() {
    let mut w = world();
    let (task, session) = working(&mut w, "sand");
    w.store
        .set_session_state(&session, SessionState::Ended, Actor::Hook)
        .unwrap();
    let moved = reconcile(&mut w);
    assert_eq!(moved.len(), 1, "moved");
    assert_eq!(moved[0].cause, LostCause::SessionEnded, "cause");
    assert_eq!(state(&w, &task), TaskState::Interrupted, "interrupted");
}

#[test]
fn reconcile_waits_while_the_machine_parks() {
    let mut w = world();
    let task = spawn_task(&mut w, "sand");
    start(&mut w, &task, FAKE_PID_BASE, FAKE_PID_BASE);
    w.store
        .set_machine_mode(MachineMode::Parking, Actor::User)
        .unwrap();
    assert!(reconcile(&mut w).is_empty(), "parking owns the task");
    assert_eq!(state(&w, &task), TaskState::Working, "unchanged");
}

#[test]
fn the_board_command_shows_a_task_whose_process_is_gone_as_interrupted() {
    let mut w = world();
    std::fs::write(w.home.root().join("config.toml"), &w.config_text).unwrap();
    let task = spawn_task(&mut w, "sand");
    start(&mut w, &task, FAKE_PID_BASE, FAKE_PID_BASE);
    let output = Command::new(env!("CARGO_BIN_EXE_headwork"))
        .args(["board", "--json"])
        .env("HEADWORK_HOME", w.home.root())
        .env("HEADWORK_CLAUDE_JSON", w.home.root().join(".claude.json"))
        .env("HEADWORK_CLAUDE_DIR", w.home.root().join(".claude"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let board: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(board["tasks"][0]["state"], "interrupted", "{text}");
    assert_eq!(
        state(&w, &task),
        TaskState::Interrupted,
        "recorded in the store"
    );
}

#[test]
fn a_park_in_progress_hides_lost_panes() {
    let mut w = world();
    let (task, _) = working(&mut w, "sand");
    w.store
        .set_machine_mode(MachineMode::Parking, Actor::User)
        .unwrap();
    let session = w.store.session_of(task.id).unwrap().unwrap();
    w.host
        .close_surface(&SurfaceId(session.host_surface.unwrap()))
        .unwrap();
    run_tick(&mut w);
    assert_eq!(
        state(&w, &task),
        TaskState::Working,
        "parking owns the task"
    );
}

#[test]
fn a_pane_no_task_owns_is_reported_once_and_cleared_when_it_goes() {
    let mut w = world();
    let pane = w.host.add_foreign_pane("demo", "demo/stray", FAKE_PID_BASE);
    let report = run_tick(&mut w);
    assert_eq!(
        rules(&report),
        [(FindingId::OrphanPane, Status::Standing)],
        "reported"
    );
    assert!(
        w.host
            .calls()
            .iter()
            .any(|c| c.starts_with("notify") && c.contains("demo/stray")),
        "{:?}",
        w.host.calls()
    );
    run_tick(&mut w);
    assert_eq!(count(&w, "finding"), 1, "logged once");
    w.host.exit(&pane.agent);
    run_tick(&mut w);
    assert_eq!(count(&w, "finding-cleared"), 1, "cleared");
}

#[test]
fn a_pane_of_an_unknown_project_is_not_ours() {
    let mut w = world();
    w.host
        .add_foreign_pane("other", "other/stray", FAKE_PID_BASE);
    assert!(run_tick(&mut w).lines.is_empty(), "ignored");
}

#[test]
fn pull_requests_move_a_task_through_review_to_merged() {
    let mut w = world();
    let (task, _) = working(&mut w, "sand");
    w.git.set("sand", pr(PrState::Open));
    let report = run_tick(&mut w);
    assert_eq!(state(&w, &task), TaskState::InReview, "open PR");
    assert_eq!(
        rules(&report),
        [(FindingId::PrOpened, Status::Applied)],
        "opened"
    );
    w.git.set("sand", pr(PrState::Merged));
    let report = run_tick(&mut w);
    assert_eq!(state(&w, &task), TaskState::Merged, "merged PR");
    assert_eq!(
        rules(&report),
        [(FindingId::PrMerged, Status::Applied)],
        "merged"
    );
    assert_eq!(
        w.store.board(false).unwrap().tasks[0]
            .pr
            .as_ref()
            .unwrap()
            .state,
        "merged",
        "recorded"
    );
}

#[test]
fn a_pull_request_merged_while_working_goes_straight_to_merged() {
    let mut w = world();
    let (task, _) = working(&mut w, "sand");
    w.git.set("sand", pr(PrState::Merged));
    run_tick(&mut w);
    assert_eq!(state(&w, &task), TaskState::Merged, "merged");
}

#[test]
fn a_closed_pull_request_sends_the_task_back_to_working() {
    let mut w = world();
    let (task, _) = working(&mut w, "sand");
    w.git.set("sand", pr(PrState::Open));
    run_tick(&mut w);
    w.git.set("sand", pr(PrState::Closed));
    let report = run_tick(&mut w);
    assert_eq!(state(&w, &task), TaskState::Working, "back to work");
    assert_eq!(
        rules(&report),
        [(FindingId::PrClosed, Status::Applied)],
        "finding"
    );
}

#[test]
fn a_pull_request_of_a_foreign_repository_does_not_open_a_review() {
    let mut w = world();
    let (task, _) = working(&mut w, "sand");
    let mut foreign = pr(PrState::Open);
    foreign.repo = "someone/else".to_owned();
    w.git.set("sand", foreign);
    run_tick(&mut w);
    assert_eq!(state(&w, &task), TaskState::Working, "refused");
}

fn second_pr(state: PrState) -> PrInfo {
    PrInfo {
        number: 8,
        url: "https://github.com/acme/demo/pull/8".to_owned(),
        ..pr(state)
    }
}

#[test]
fn a_later_pull_request_takes_a_merged_task_back_to_review() {
    let mut w = world();
    let (task, _) = working(&mut w, "sand");
    w.git.set("sand", pr(PrState::Merged));
    run_tick(&mut w);
    assert_eq!(state(&w, &task), TaskState::Merged, "first part merged");
    w.git.set("sand-part-two", second_pr(PrState::Open));
    w.store
        .record_pr(
            task.id,
            &PrSeen {
                repo: "acme/demo",
                number: 8,
                url: "https://github.com/acme/demo/pull/8",
                state: "open",
            },
            Actor::Worker,
        )
        .unwrap();
    let report = run_tick(&mut w);
    assert_eq!(
        state(&w, &task),
        TaskState::InReview,
        "the second part is open"
    );
    assert_eq!(
        rules(&report),
        [(FindingId::PrOpened, Status::Applied)],
        "opened"
    );
    w.git.set("sand-part-two", second_pr(PrState::Merged));
    run_tick(&mut w);
    assert_eq!(state(&w, &task), TaskState::Merged, "both parts merged");
}

#[test]
fn a_renamed_branch_is_found_by_its_checked_out_name() {
    let mut w = world();
    let (task, _) = working(&mut w, "sand");
    let dir = w.store.worktree_of(task.id).unwrap().unwrap().path;
    git_in(Path::new(&dir), &["checkout", "-q", "-b", "portal-pistons"]);
    w.git.set("portal-pistons", pr(PrState::Open));
    run_tick(&mut w);
    assert_eq!(
        state(&w, &task),
        TaskState::InReview,
        "found on the renamed branch"
    );
}

fn merged(w: &mut World, slug: &str) -> Task {
    let (task, _) = working(w, slug);
    w.git.set(slug, pr(PrState::Merged));
    run_tick(w);
    w.store.task(task.id).unwrap()
}

fn reported(w: &World, task: &Task, session: &str) {
    w.store
        .append_event(
            Actor::Hook,
            Some(task.id),
            Some(session),
            "tool-used",
            &serde_json::json!({ "tool": "SendMessage" }),
        )
        .unwrap();
}

#[test]
fn shadow_mode_only_reports_a_merged_task_past_its_grace() {
    let mut w = world();
    let task = merged(&mut w, "sand");
    let session = w.store.session_of(task.id).unwrap().unwrap();
    reported(&w, &task, &session.id);
    w.limits.merged_grace = 0;
    let report = run_tick(&mut w);
    assert_eq!(state(&w, &task), TaskState::Merged, "untouched");
    assert_eq!(
        rules(&report),
        [(FindingId::MergedGrace, Status::Shadow)],
        "would close"
    );
}

#[test]
fn enforcing_starts_closing_a_merged_task_that_reported() {
    let mut w = world_with(true, 10);
    let task = merged(&mut w, "sand");
    let session = w.store.session_of(task.id).unwrap().unwrap();
    w.limits.merged_grace = 0;
    run_tick(&mut w);
    assert_eq!(state(&w, &task), TaskState::Merged, "no report sent yet");
    reported(&w, &task, &session.id);
    let report = run_tick(&mut w);
    assert_eq!(
        rules(&report)[0],
        (FindingId::MergedGrace, Status::Applied),
        "closing"
    );
    assert_ne!(state(&w, &task), TaskState::Merged, "left merged");
}

#[test]
fn a_merged_grace_waits_for_a_running_job() {
    let mut w = world_with(true, 10);
    let task = merged(&mut w, "sand");
    let session = w.store.session_of(task.id).unwrap().unwrap();
    reported(&w, &task, &session.id);
    w.limits.merged_grace = 0;
    let mut child = sleeper();
    let argv = vec!["train".to_owned()];
    w.store
        .register_job(
            &NewJob {
                task_id: Some(task.id),
                session_id: Some(&session.id),
                project: "demo",
                label: "training",
                park_policy: ParkPolicy::Finish,
                argv: &argv,
                cwd: ".",
                pid: Some(child.id()),
                identity: headwork_adapters::proc::identify(child.id()).as_ref(),
                checkpoint_path: None,
                heavy: false,
                restart: false,
                log_path: "train.log",
            },
            Actor::Worker,
        )
        .unwrap();
    run_tick(&mut w);
    assert_eq!(state(&w, &task), TaskState::Merged, "the job still runs");
    child.kill().unwrap();
    child.wait().unwrap();
    run_tick(&mut w);
    assert_ne!(state(&w, &task), TaskState::Merged, "closing once it ended");
}

#[test]
fn a_merged_grace_waits_for_a_leads_close_go() {
    let mut w = world_extra(true, 10, "\n[leads]\nenabled = true\n");
    let task = merged(&mut w, "sand");
    assert!(task.shape.is_lead(), "a lead");
    let session = w.store.session_of(task.id).unwrap().unwrap();
    reported(&w, &task, &session.id);
    w.limits.merged_grace = 0;
    run_tick(&mut w);
    assert_eq!(
        state(&w, &task),
        TaskState::Merged,
        "no go from the user yet"
    );
    w.store
        .append_event(
            Actor::Hook,
            Some(task.id),
            Some(&session.id),
            "close-go",
            &serde_json::json!({}),
        )
        .unwrap();
    let report = run_tick(&mut w);
    assert_eq!(
        rules(&report)[0],
        (FindingId::MergedGrace, Status::Applied),
        "closing after the go"
    );
    assert_ne!(state(&w, &task), TaskState::Merged, "left merged");
}

#[test]
fn a_merged_grace_waits_for_its_time() {
    let mut w = world_with(true, 10);
    let task = merged(&mut w, "sand");
    let session = w.store.session_of(task.id).unwrap().unwrap();
    reported(&w, &task, &session.id);
    run_tick(&mut w);
    assert_eq!(state(&w, &task), TaskState::Merged, "ten minutes not over");
}

fn closing_with(w: &mut World, slug: &str, launcher: u32) -> Task {
    let task = spawn_task(w, slug);
    start(w, &task, launcher, launcher);
    w.store
        .transition(task.id, &Trigger::ForceClose, Actor::User)
        .unwrap();
    w.store.task(task.id).unwrap()
}

#[test]
fn a_closing_task_whose_processes_are_gone_is_finished() {
    let mut w = world();
    let task = closing_with(&mut w, "sand", FAKE_PID_BASE);
    let dir = w.store.worktree_of(task.id).unwrap().unwrap().path;
    run_tick(&mut w);
    assert_eq!(state(&w, &task), TaskState::Closed, "closed");
    assert!(!Path::new(&dir).exists(), "worktree removed");
}

#[test]
fn a_second_finisher_leaves_a_claimed_close_alone() {
    let mut w = world();
    let task = closing_with(&mut w, "sand", FAKE_PID_BASE);
    let folder = w.home.root().join("closing");
    std::fs::create_dir_all(&folder).unwrap();
    let claim = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(folder.join(format!("{}.lock", task.id)))
        .unwrap();
    claim.try_lock().unwrap();
    run_tick(&mut w);
    assert_eq!(
        state(&w, &task),
        TaskState::Closing,
        "another finisher holds the close"
    );
    assert_eq!(count(&w, "close-claimed"), 1, "logged");
    drop(claim);
    run_tick(&mut w);
    assert_eq!(
        state(&w, &task),
        TaskState::Closed,
        "closed once the claim is free"
    );
}

#[test]
fn shadow_mode_leaves_a_lingering_process_of_a_closing_task() {
    let mut w = world();
    w.limits.closing_grace = 0;
    let mut child = sleeper();
    let task = closing_with(&mut w, "sand", child.id());
    let report = run_tick(&mut w);
    assert_eq!(
        rules(&report),
        [(FindingId::LeftoverProcess, Status::Shadow)],
        "would end"
    );
    assert!(is_alive(child.id()).unwrap(), "still running");
    assert_eq!(state(&w, &task), TaskState::Closing, "still closing");
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn enforcing_ends_a_lingering_process_of_a_closing_task_then_closes_it() {
    let mut w = world_with(true, 10);
    w.limits.closing_grace = 0;
    let mut child = sleeper();
    let pid = child.id();
    let task = closing_with(&mut w, "sand", pid);
    let report = run_tick(&mut w);
    assert_eq!(
        rules(&report),
        [(FindingId::LeftoverProcess, Status::Applied)],
        "ended"
    );
    assert!(wait_dead(pid), "the sleeper is gone");
    child.wait().unwrap();
    run_tick(&mut w);
    assert_eq!(
        state(&w, &task),
        TaskState::Closed,
        "closed on the next tick"
    );
}

#[test]
fn a_young_closing_task_keeps_its_launcher_a_moment() {
    let mut w = world_with(true, 10);
    let mut child = sleeper();
    closing_with(&mut w, "sand", child.id());
    run_tick(&mut w);
    assert!(is_alive(child.id()).unwrap(), "inside the hand-off grace");
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn only_enforcement_starts_closing_dropped_tasks() {
    for enforce in [false, true] {
        let mut w = world_with(enforce, 10);
        let task = spawn_task(&mut w, "sand");
        w.store
            .transition(task.id, &Trigger::Drop, Actor::User)
            .unwrap();
        run_tick(&mut w);
        let after = state(&w, &task);
        let expected = if enforce {
            TaskState::Closed
        } else {
            TaskState::Dropped
        };
        assert_eq!(after, expected, "enforce {enforce}");
    }
}

fn dirty_closing(w: &mut World, slug: &str) -> (Task, PathBuf) {
    let task = closing_with(w, slug, FAKE_PID_BASE);
    let dir = PathBuf::from(w.store.worktree_of(task.id).unwrap().unwrap().path);
    std::fs::write(dir.join("scratch.txt"), "unsaved").unwrap();
    (task, dir)
}

#[test]
fn a_worktree_holding_work_asks_at_once_and_is_not_retried() {
    let mut w = world();
    let task = closing_with(&mut w, "sand", FAKE_PID_BASE);
    let dir = PathBuf::from(w.store.worktree_of(task.id).unwrap().unwrap().path);
    std::fs::write(dir.join("scratch.txt"), "unsaved").unwrap();
    run_tick(&mut w);
    assert_eq!(count(&w, "worktree-refused"), 1, "one refusal");
    let refused = count(&w, "refused");
    run_tick(&mut w);
    assert!(
        w.store.topic_ask(task.id, "worktree").unwrap().is_some(),
        "a lasting refusal asks without backing off"
    );
    run_tick(&mut w);
    assert_eq!(
        count(&w, "worktree-refused"),
        1,
        "no second attempt while the ask waits"
    );
    assert_eq!(count(&w, "refused"), refused, "no transition refusal again");
    assert!(dir.exists(), "nothing deleted");
}

#[test]
fn a_transient_removal_failure_backs_off_without_asking() {
    let mut w = world();
    let task = closing_with(&mut w, "sand", FAKE_PID_BASE);
    let dir = PathBuf::from(w.store.worktree_of(task.id).unwrap().unwrap().path);
    w.store
        .record_removal_failure(
            task.id,
            "error: failed to delete 'sand': Permission denied",
            Actor::Tick,
        )
        .unwrap();
    run_tick(&mut w);
    run_tick(&mut w);
    assert_eq!(
        count(&w, "worktree-refused"),
        1,
        "no attempt inside the first minute, only the seeded refusal"
    );
    assert!(
        w.store.topic_ask(task.id, "worktree").unwrap().is_none(),
        "a transient failure does not ask yet"
    );
    assert!(dir.exists(), "nothing deleted");
}

#[test]
fn a_worktree_that_cannot_be_removed_becomes_one_ask() {
    let mut w = world();
    let (task, dir) = dirty_closing(&mut w, "sand");
    let report = run_tick(&mut w);
    assert_eq!(
        rules(&report),
        [(FindingId::WorktreeRefused, Status::Applied)],
        "asked"
    );
    let ask = w.store.topic_ask(task.id, "worktree").unwrap().unwrap();
    assert_eq!(
        ask.state,
        headwork_core::AskState::Pending,
        "waits for the orchestrator to put it"
    );
    run_tick(&mut w);
    assert_eq!(count(&w, "ask-opened"), 1, "asked once");
    assert!(dir.exists(), "nothing deleted");
    assert_eq!(state(&w, &task), TaskState::Closing, "still closing");
}

#[test]
fn answering_delete_removes_the_dirty_worktree_and_closes() {
    let mut w = world();
    let (task, dir) = dirty_closing(&mut w, "sand");
    run_tick(&mut w);
    let ask = w.store.topic_ask(task.id, "worktree").unwrap().unwrap();
    w.store
        .answer_ask(ask.id, DELETE_WORKTREE, Actor::User)
        .unwrap();
    run_tick(&mut w);
    assert!(!dir.exists(), "deleted");
    assert_eq!(state(&w, &task), TaskState::Closed, "closed");
    assert_eq!(
        w.store.removal_attempts(task.id).unwrap().failures,
        0,
        "the answer cleared the failures"
    );
}

#[test]
fn a_failed_delete_asks_once_more_and_says_delete_was_tried() {
    let mut w = world();
    let (task, _) = dirty_closing(&mut w, "sand");
    run_tick(&mut w);
    let ask = w.store.topic_ask(task.id, "worktree").unwrap().unwrap();
    w.store
        .answer_ask(ask.id, DELETE_WORKTREE, Actor::User)
        .unwrap();
    w.store.set_ask_topic(ask.id, "worktree-handled").unwrap();
    w.store
        .record_removal_failure(task.id, "boom", Actor::Tick)
        .unwrap();
    let opened = count(&w, "ask-opened");
    run_tick(&mut w);
    run_tick(&mut w);
    run_tick(&mut w);
    assert_eq!(
        count(&w, "ask-opened"),
        opened + 1,
        "one more ask, not one per tick"
    );
    assert!(
        notified(&w, "was tried and failed: boom"),
        "{:?}",
        w.host.calls()
    );
}

#[test]
fn answering_keep_keeps_the_worktree_and_closes() {
    let mut w = world();
    let (task, dir) = dirty_closing(&mut w, "sand");
    run_tick(&mut w);
    let ask = w.store.topic_ask(task.id, "worktree").unwrap().unwrap();
    w.store
        .answer_ask(ask.id, KEEP_WORKTREE, Actor::User)
        .unwrap();
    run_tick(&mut w);
    assert!(dir.exists(), "kept");
    assert_eq!(state(&w, &task), TaskState::Closed, "closed");
}

fn notified(w: &World, needle: &str) -> bool {
    w.host
        .calls()
        .iter()
        .any(|c| c.starts_with("notify") && c.contains(needle))
}

#[test]
fn low_disk_space_warns_once_and_clears_when_space_returns() {
    let mut w = world();
    w.disks.set(w.dir.path(), 10 * GIB);
    w.disks.set(&home_dir(), 10 * GIB);
    let report = run_tick(&mut w);
    assert!(
        rules(&report).contains(&(FindingId::DiskWarn, Status::Standing)),
        "{:?}",
        report.lines
    );
    assert!(!report.queue_paused, "a warning keeps the queue running");
    assert!(notified(&w, "GB left"), "{:?}", w.host.calls());
    run_tick(&mut w);
    assert_eq!(count(&w, "finding"), 1, "logged once");
    w.disks.set(w.dir.path(), 100 * GIB);
    w.disks.set(&home_dir(), 100 * GIB);
    run_tick(&mut w);
    assert_eq!(count(&w, "finding-cleared"), 1, "cleared");
}

#[test]
fn a_nearly_full_disk_pauses_the_queue() {
    let mut w = world_with(false, 1);
    let first = spawn_task(&mut w, "first");
    let second = spawn_task(&mut w, "second");
    assert_eq!(second.state, TaskState::Queued, "no free slot");
    w.store
        .transition(first.id, &Trigger::LaunchFailed, Actor::User)
        .unwrap();
    w.disks.set(w.dir.path(), GIB);
    w.disks.set(&home_dir(), GIB);
    let report = run_tick(&mut w);
    assert!(report.queue_paused, "paused");
    assert!(report.started.is_empty(), "nothing started");
    assert_eq!(state(&w, &second), TaskState::Queued, "still queued");
    assert!(
        rules(&report).contains(&(FindingId::DiskStop, Status::Standing)),
        "{:?}",
        report.lines
    );
    w.disks.set(w.dir.path(), 100 * GIB);
    w.disks.set(&home_dir(), 100 * GIB);
    let report = run_tick(&mut w);
    assert_eq!(report.started.len(), 1, "started once the disk has room");
}

#[test]
fn an_unreadable_volume_is_a_finding_not_a_failure() {
    let mut w = world();
    w.disks = FakeDisks::new();
    let report = run_tick(&mut w);
    assert!(
        rules(&report)
            .iter()
            .all(|(rule, _)| *rule == FindingId::TickError),
        "{:?}",
        report.lines
    );
    assert!(!report.lines.is_empty(), "reported");
}

const fn runaway_limits() -> Limits {
    Limits {
        growth: 500,
        file: 1000,
        ..roomy_limits()
    }
}

fn write_file(path: &Path, len: usize) {
    std::fs::write(path, vec![b'x'; len]).unwrap();
}

#[test]
fn a_huge_file_in_a_live_worktree_is_reported_and_its_session_left_alone() {
    let mut w = world_with(true, 10);
    w.limits = runaway_limits();
    let (task, _) = working(&mut w, "sand");
    let dir = PathBuf::from(w.store.worktree_of(task.id).unwrap().unwrap().path);
    write_file(&dir.join("big.bin"), 1500);
    let report = run_tick(&mut w);
    let line = report
        .lines
        .iter()
        .find(|l| l.rule == FindingId::Runaway)
        .unwrap();
    assert_eq!(
        line.status,
        Status::Standing,
        "a live task is alerted, not ended"
    );
    assert_eq!(line.task.as_deref(), Some("demo-sand"), "attributed");
    assert!(notified(&w, "Runaway"), "{:?}", w.host.calls());
    assert!(is_alive(w.children[0].id()).unwrap(), "the session lives");
}

#[test]
fn growth_between_ticks_counts_as_runaway() {
    let mut w = world();
    w.limits = runaway_limits();
    let (task, _) = working(&mut w, "sand");
    let dir = PathBuf::from(w.store.worktree_of(task.id).unwrap().unwrap().path);
    write_file(&dir.join("log.txt"), 400);
    assert!(run_tick(&mut w).lines.is_empty(), "small first");
    write_file(&dir.join("log.txt"), 950);
    let report = run_tick(&mut w);
    assert_eq!(
        rules(&report),
        [(FindingId::Runaway, Status::Standing)],
        "grew by 550"
    );
    write_file(&dir.join("log.txt"), 960);
    run_tick(&mut w);
    assert_eq!(count(&w, "finding-cleared"), 1, "growth stopped");
}

#[test]
fn a_file_seen_for_the_first_time_gets_no_growth_verdict() {
    let mut w = world();
    w.limits = runaway_limits();
    let (task, _) = working(&mut w, "sand");
    let dir = PathBuf::from(w.store.worktree_of(task.id).unwrap().unwrap().path);
    write_file(&dir.join("old.log"), 950);
    assert!(run_tick(&mut w).lines.is_empty(), "no earlier sample");
    assert!(run_tick(&mut w).lines.is_empty(), "no growth since");
}

#[test]
fn a_runaway_file_of_a_closing_task_ends_its_processes_only_when_enforcing() {
    for enforce in [false, true] {
        let mut w = world_with(enforce, 10);
        w.limits = runaway_limits();
        w.limits.closing_grace = 3600;
        let mut child = sleeper();
        let task = closing_with(&mut w, "sand", child.id());
        let dir = PathBuf::from(w.store.worktree_of(task.id).unwrap().unwrap().path);
        write_file(&dir.join("big.bin"), 1500);
        let report = run_tick(&mut w);
        let line = report
            .lines
            .iter()
            .find(|l| l.rule == FindingId::Runaway)
            .unwrap();
        let (status, alive) = if enforce {
            (Status::Applied, false)
        } else {
            (Status::Shadow, true)
        };
        assert_eq!(line.status, status, "enforce {enforce}");
        if enforce {
            assert!(wait_dead(child.id()), "ended");
        }
        assert_eq!(is_alive(child.id()).unwrap(), alive, "enforce {enforce}");
        child.kill().ok();
        child.wait().ok();
    }
}

#[test]
fn a_session_with_an_unchanging_screen_is_stuck_on_the_second_look() {
    let mut w = world();
    w.limits.stuck_after = 0;
    let (task, session) = working(&mut w, "sand");
    let first = run_tick(&mut w);
    assert!(
        first.lines.is_empty(),
        "first look only samples: {:?}",
        first.lines
    );
    let second = run_tick(&mut w);
    assert_eq!(
        rules(&second),
        [(FindingId::Stuck, Status::Standing)],
        "unchanged screen"
    );
    assert!(notified(&w, "stuck"), "{:?}", w.host.calls());
    run_tick(&mut w);
    assert_eq!(count(&w, "finding"), 1, "reported once");
    w.store
        .set_session_state(&session, SessionState::Gone, Actor::Tick)
        .unwrap();
    run_tick(&mut w);
    assert_eq!(
        count(&w, "finding-cleared"),
        1,
        "cleared when the session is gone"
    );
    assert_eq!(
        state(&w, &task),
        TaskState::Interrupted,
        "and the task is interrupted"
    );
}

fn idle_marked(w: &World, session: &str) -> bool {
    w.store.session_idle_marked(session).unwrap()
}

fn live_and_waiting(w: &World) -> (u32, u32) {
    let board = w.store.board(false).unwrap();
    (board.live_workers, board.waiting_workers)
}

#[test]
fn a_merged_session_idle_at_its_prompt_frees_its_slot_for_a_queued_task() {
    let mut w = world_with(false, 1);
    let task = merged(&mut w, "sand");
    let session = w.store.session_of(task.id).unwrap().unwrap().id;
    w.limits.idle_free_merged = Some(0);
    let queued = spawn_task(&mut w, "snow");
    assert_eq!(state(&w, &queued), TaskState::Queued, "the project is full");
    assert_eq!(
        live_and_waiting(&w),
        (1, 0),
        "working as far as the slots go"
    );
    let first = run_tick(&mut w);
    assert!(
        !rules(&first).contains(&(FindingId::IdleSlot, Status::Standing)),
        "first look only samples"
    );
    assert!(!idle_marked(&w, &session), "not marked yet");
    let second = run_tick(&mut w);
    assert!(
        rules(&second).contains(&(FindingId::IdleSlot, Status::Standing)),
        "{:?}",
        second.lines
    );
    assert!(idle_marked(&w, &session), "marked idle");
    assert!(notified(&w, "worker slot is freed"), "{:?}", w.host.calls());
    assert_ne!(
        state(&w, &queued),
        TaskState::Queued,
        "the queued task took the slot"
    );
    assert_eq!(live_and_waiting(&w), (1, 1), "one waiting, one working");
    assert_eq!(state(&w, &task), TaskState::Merged, "nothing was closed");
    let findings = count(&w, "finding");
    run_tick(&mut w);
    assert_eq!(count(&w, "session-idle"), 1, "marked once");
    assert_eq!(count(&w, "finding"), findings, "reported once");
}

#[test]
fn a_session_that_runs_a_job_or_a_shell_keeps_its_slot() {
    let mut w = world_with(false, 1);
    let task = merged(&mut w, "sand");
    let session = w.store.session_of(task.id).unwrap().unwrap().id;
    w.limits.idle_free_merged = Some(0);
    let mut child = sleeper();
    let argv = vec!["train".to_owned()];
    w.store
        .register_job(
            &NewJob {
                task_id: Some(task.id),
                session_id: Some(&session),
                project: "demo",
                label: "training",
                park_policy: ParkPolicy::Finish,
                argv: &argv,
                cwd: ".",
                pid: Some(child.id()),
                identity: headwork_adapters::proc::identify(child.id()).as_ref(),
                checkpoint_path: None,
                heavy: false,
                restart: false,
                log_path: "train.log",
            },
            Actor::Worker,
        )
        .unwrap();
    let findings = count(&w, "finding");
    for _ in 0..3 {
        run_tick(&mut w);
    }
    assert!(!idle_marked(&w, &session), "a job runs");
    assert_eq!(count(&w, "finding"), findings, "no finding");
    child.kill().unwrap();
    child.wait().unwrap();

    let launcher = sleeper();
    let launcher_pid = launcher.id();
    w.children.push(launcher);
    w.store
        .session_launched(
            &session,
            launcher_pid,
            headwork::ops::waiting::FAKE_CLAUDE,
            "job",
        )
        .unwrap();
    w.slots.set_shell(true);
    for _ in 0..3 {
        run_tick(&mut w);
    }
    assert!(!idle_marked(&w, &session), "a shell runs");
    assert_eq!(count(&w, "finding"), findings, "no finding either");
}

#[test]
fn a_review_session_waits_for_its_own_idle_limit() {
    let mut w = world_with(false, 1);
    let (task, session) = working(&mut w, "sand");
    w.git.set("sand", pr(PrState::Open));
    run_tick(&mut w);
    assert_eq!(state(&w, &task), TaskState::InReview, "in review");
    w.limits.idle_free_merged = Some(0);
    w.limits.idle_free_review = Some(3600);
    for _ in 0..3 {
        run_tick(&mut w);
    }
    assert!(!idle_marked(&w, &session), "the review limit is longer");
    w.limits.idle_free_review = Some(0);
    for _ in 0..2 {
        run_tick(&mut w);
    }
    assert!(
        idle_marked(&w, &session),
        "marked once its own limit passed"
    );
}

#[test]
fn a_closing_task_with_a_live_idle_session_is_reported_stuck() {
    let mut w = world();
    w.limits.stuck_after = 0;
    let (task, session) = working(&mut w, "sand");
    w.store
        .transition(task.id, &Trigger::ForceClose, Actor::Orchestrator)
        .unwrap();
    assert_eq!(state(&w, &task), TaskState::Closing, "closing");
    run_tick(&mut w);
    let second = run_tick(&mut w);
    assert!(
        rules(&second).contains(&(FindingId::Stuck, Status::Standing)),
        "{:?}",
        second.lines
    );
    assert!(notified(&w, "stuck in closing"), "{:?}", w.host.calls());
    assert!(!idle_marked(&w, &session), "a closing session is not freed");
}

#[test]
fn a_pane_that_cannot_be_read_does_not_stop_the_rest_of_the_tick() {
    let mut w = world();
    w.limits.stuck_after = 0;
    let (bad, bad_session) = working(&mut w, "sand");
    let (good, good_session) = working(&mut w, "snow");
    w.host.fail_screen(&trust_surface(&w, &bad));
    let report = run_tick(&mut w);
    assert_eq!(
        rules(&report),
        [(FindingId::TickError, Status::Standing)],
        "{:?}",
        report.lines
    );
    assert_eq!(
        report.lines[0].task.as_deref(),
        Some(bad.name.to_string().as_str())
    );
    assert!(
        w.store.screen_sample(&good_session).unwrap().is_some(),
        "the next pane was still looked at"
    );
    assert!(
        w.store.screen_sample(&bad_session).unwrap().is_none(),
        "the unreadable one was not"
    );
    assert!(
        w.store.last_tick_age_s().unwrap().is_some(),
        "the tick was recorded"
    );
    assert_eq!(state(&w, &good), TaskState::Working, "still working");
}

#[test]
fn a_stage_that_fails_is_recorded_and_the_later_stages_still_run() {
    let mut w = world();
    let task = spawn_task(&mut w, "sand");
    w.host.fail_screen(&trust_surface(&w, &task));
    let report = run_tick(&mut w);
    assert!(
        rules(&report)
            .iter()
            .any(|(rule, _)| *rule == FindingId::TickError),
        "{:?}",
        report.lines
    );
    assert!(
        w.store.last_tick_age_s().unwrap().is_some(),
        "the tick was recorded"
    );
}

#[test]
fn a_session_that_moved_recently_is_not_stuck() {
    let mut w = world();
    working(&mut w, "sand");
    run_tick(&mut w);
    assert!(run_tick(&mut w).lines.is_empty(), "within the limit");
}

fn open_ask(w: &mut World, task: &Task, session: &str) {
    w.store
        .open_ask(
            &headwork_store::NewAsk {
                task_id: Some(task.id),
                session_id: Some(session.to_owned()),
                kind: headwork_core::AskKind::Question,
                question: "Which one?".to_owned(),
                options: vec!["A".to_owned(), "B".to_owned()],
            },
            Actor::Hook,
        )
        .unwrap();
}

#[test]
fn an_ask_that_waits_too_long_is_announced_again() {
    let mut w = world();
    let (task, session) = working(&mut w, "sand");
    open_ask(&mut w, &task, &session);
    run_tick(&mut w);
    assert_eq!(count(&w, "finding"), 0, "young asks are left alone");
    w.limits.ask_remind = 0;
    let report = run_tick(&mut w);
    assert_eq!(
        rules(&report),
        [(FindingId::AskStale, Status::Applied)],
        "reminded"
    );
    assert!(notified(&w, "Which one?"), "{:?}", w.host.calls());
}

#[test]
fn closing_a_task_retires_the_asks_it_left_waiting() {
    let mut w = world();
    let task = closing_with(&mut w, "sand", FAKE_PID_BASE);
    let session = w.store.session_of(task.id).unwrap().unwrap().id;
    open_ask(&mut w, &task, &session);
    run_tick(&mut w);
    assert_eq!(state(&w, &task), TaskState::Closed, "closed");
    assert!(
        !w.store.has_waiting_ask(task.id).unwrap(),
        "no ask of a closed task waits on the user"
    );
}

#[test]
fn an_ask_left_on_a_closed_task_is_retired_and_never_announced() {
    let mut w = world();
    let task = closing_with(&mut w, "sand", FAKE_PID_BASE);
    run_tick(&mut w);
    assert_eq!(state(&w, &task), TaskState::Closed, "closed");
    let session = w.store.session_of(task.id).unwrap().unwrap().id;
    open_ask(&mut w, &task, &session);
    w.limits.ask_remind = 0;
    run_tick(&mut w);
    assert!(
        !w.store.has_waiting_ask(task.id).unwrap(),
        "the leftover ask is retired"
    );
    assert!(!notified(&w, "Which one?"), "{:?}", w.host.calls());
}

#[test]
fn a_reminder_is_not_repeated_inside_its_interval() {
    let mut w = world();
    let (task, session) = working(&mut w, "sand");
    open_ask(&mut w, &task, &session);
    w.limits.ask_remind = 0;
    run_tick(&mut w);
    w.limits.ask_remind = 3600;
    run_tick(&mut w);
    assert_eq!(count(&w, "finding"), 1, "once");
}

#[test]
fn a_session_on_old_rules_is_reported_until_it_catches_up() {
    let mut w = world();
    let (task, session) = working(&mut w, "sand");
    w.store.observe_rules("edited", Actor::User).unwrap();
    let report = run_tick(&mut w);
    assert_eq!(
        rules(&report),
        [(FindingId::RulesStale, Status::Standing)],
        "behind the new rules"
    );
    let version = w.store.rules_version().unwrap().version;
    w.store
        .set_rules_seen(&session, Some(task.id), version, 0)
        .unwrap();
    run_tick(&mut w);
    assert_eq!(count(&w, "finding-cleared"), 1, "caught up");
}

#[test]
fn the_tick_starts_queued_tasks_that_fit() {
    let mut w = world_with(false, 1);
    let first = spawn_task(&mut w, "first");
    let second = spawn_task(&mut w, "second");
    assert_eq!(second.state, TaskState::Queued, "waiting");
    w.store
        .transition(first.id, &Trigger::LaunchFailed, Actor::User)
        .unwrap();
    let report = run_tick(&mut w);
    assert_eq!(report.started.len(), 1, "started");
    assert_eq!(state(&w, &second), TaskState::Spawning, "spawning");
}

#[test]
fn the_tick_is_recorded() {
    let w = world();
    assert!(!w.store.record_tick().unwrap(), "first tick");
    assert!(w.store.record_tick().unwrap(), "later ticks");
}

impl Drop for World {
    fn drop(&mut self) {
        for child in &mut self.children {
            child.kill().ok();
            child.wait().ok();
        }
    }
}

const TRUST_SCREEN: &str = " Do you trust this folder?\n S:/Code/demo-sand\n > 1. Yes, I trust this folder\n   2. No, exit\n";

fn trust_surface(w: &World, task: &Task) -> SurfaceId {
    let session = w.store.session_of(task.id).unwrap().unwrap();
    SurfaceId(session.host_surface.unwrap())
}

#[test]
fn a_trust_prompt_on_a_starting_pane_becomes_an_ask_and_holds_the_spawn_timeout() {
    let mut w = world();
    let task = spawn_task(&mut w, "sand");
    let surface = trust_surface(&w, &task);
    w.host.set_screen(&surface, TRUST_SCREEN);
    w.limits.spawn_timeout = 0;
    let report = run_tick(&mut w);
    assert_eq!(
        state(&w, &task),
        TaskState::Spawning,
        "held while the user is asked"
    );
    assert_eq!(
        rules(&report),
        [(FindingId::TrustPrompt, Status::Standing)],
        "finding"
    );
    let ask = w.store.topic_ask(task.id, "trust").unwrap().unwrap();
    assert!(ask.state.is_waiting(), "asked");
    assert!(
        notified(&w, "Do you trust this folder"),
        "{:?}",
        w.host.calls()
    );
    assert!(
        w.host
            .calls()
            .iter()
            .any(|c| c.starts_with(&format!("blocked {surface}")) && c.contains("(2 choices)")),
        "blocked state with the dialog's two answers: {:?}",
        w.host.calls()
    );
    run_tick(&mut w);
    assert_eq!(count(&w, "ask-opened"), 1, "asked once");
    assert_eq!(count(&w, "finding"), 1, "reported once");
    w.host.set_screen(&surface, " Welcome to Claude Code\n > ");
    run_tick(&mut w);
    assert!(
        w.store.topic_ask(task.id, "trust").unwrap().is_none(),
        "the ask is retired"
    );
    assert_eq!(count(&w, "ask-superseded"), 1, "superseded");
    assert_eq!(count(&w, "finding-cleared"), 1, "finding cleared");
    assert!(
        w.host
            .calls()
            .iter()
            .any(|c| c == &format!("unblocked {surface}")),
        "blocked state cleared: {:?}",
        w.host.calls()
    );
    assert_eq!(
        state(&w, &task),
        TaskState::Failed,
        "the spawn timeout applies again"
    );
}

#[test]
fn a_trust_prompt_after_the_launcher_started_claude_is_seen_too() {
    let mut w = world();
    let task = spawn_task(&mut w, "sand");
    let session = w.store.session_of(task.id).unwrap().unwrap();
    w.store
        .session_launched(
            &session.id,
            std::process::id(),
            std::process::id(),
            "process-group",
        )
        .unwrap();
    let surface = trust_surface(&w, &task);
    w.host.set_screen(&surface, TRUST_SCREEN);
    let report = run_tick(&mut w);
    assert_eq!(
        rules(&report),
        [(FindingId::TrustPrompt, Status::Standing)],
        "a launched session whose task still spawns can wait at the dialog"
    );
    w.store
        .transition(
            task.id,
            &Trigger::SessionStarted(SessionFacts {
                session_matches: true,
                launcher_alive: true,
            }),
            Actor::Hook,
        )
        .unwrap();
    run_tick(&mut w);
    assert!(
        w.store.topic_ask(task.id, "trust").unwrap().is_none(),
        "answered in the pane: the session started, so the ask is retired"
    );
    assert!(
        w.host
            .calls()
            .iter()
            .any(|c| c == &format!("unblocked {surface}")),
        "blocked state cleared: {:?}",
        w.host.calls()
    );
}

#[test]
fn an_answered_trust_ask_presses_the_dialogs_yes() {
    let mut w = world();
    let task = spawn_task(&mut w, "sand");
    let surface = trust_surface(&w, &task);
    w.host.set_screen(&surface, TRUST_SCREEN);
    run_tick(&mut w);
    let ask = w.store.topic_ask(task.id, "trust").unwrap().unwrap();
    w.store
        .answer_ask(ask.id, TRUST_FOLDER, Actor::User)
        .unwrap();
    run_tick(&mut w);
    assert!(
        w.host
            .calls()
            .iter()
            .any(|c| c == &format!("send-key {surface} Char('1')")),
        "{:?}",
        w.host.calls()
    );
    assert!(
        w.store.topic_ask(task.id, "trust").unwrap().is_none(),
        "handled once"
    );
}

#[test]
fn a_live_session_is_never_read_for_a_trust_prompt() {
    let mut w = world();
    let (task, _) = working(&mut w, "sand");
    w.host.set_screen(&trust_surface(&w, &task), TRUST_SCREEN);
    let report = run_tick(&mut w);
    assert!(report.lines.is_empty(), "{:?}", report.lines);
    assert!(
        w.store.topic_ask(task.id, "trust").unwrap().is_none(),
        "no ask"
    );
}

macro_rules! env_as {
    ($w:expr, $actor:expr) => {
        Env {
            home: &$w.home,
            config: &$w.config,
            host: &$w.host,
            exe: Path::new("S:/bin/headwork.exe"),
            actor: $actor,
        }
    };
}

fn dropped_dirty(w: &mut World, slug: &str) -> (Task, PathBuf) {
    let task = spawn_task(w, slug);
    start(w, &task, FAKE_PID_BASE, FAKE_PID_BASE);
    w.store
        .transition(task.id, &Trigger::Drop, Actor::Orchestrator)
        .unwrap();
    w.store
        .transition(task.id, &Trigger::BeginClosing, Actor::Orchestrator)
        .unwrap();
    let dir = PathBuf::from(w.store.worktree_of(task.id).unwrap().unwrap().path);
    std::fs::write(dir.join("scratch.txt"), "unsaved").unwrap();
    (task, dir)
}

fn worktree_ask(w: &World, task: &Task) -> headwork_store::AskId {
    w.store.topic_ask(task.id, "worktree").unwrap().unwrap().id
}

fn answered_by(w: &World) -> Vec<(Actor, String)> {
    w.store
        .events(EventFilter {
            task: None,
            limit: 10_000,
        })
        .unwrap()
        .iter()
        .filter(|e| e.kind == "ask-answered")
        .map(|e| (e.actor, e.data["answer"].as_str().unwrap().to_owned()))
        .collect()
}

#[test]
fn a_closing_task_whose_session_ended_frees_its_slot_for_the_queue() {
    let mut w = world_with(false, 1);
    let (stuck, _) = dropped_dirty(&mut w, "stuck");
    run_tick(&mut w);
    assert_eq!(
        state(&w, &stuck),
        TaskState::Closing,
        "stuck on its worktree"
    );
    let next = spawn_task(&mut w, "next");
    assert_eq!(
        state(&w, &next),
        TaskState::Spawning,
        "the stuck task's slot is free"
    );
}

#[test]
fn the_user_answers_keep_at_the_cli_and_the_dropped_task_closes() {
    use headwork::ops::answer::answer;
    let mut w = world();
    let (task, dir) = dropped_dirty(&mut w, "sand");
    run_tick(&mut w);
    let ask = worktree_ask(&w, &task);
    let env = env_as!(w, Actor::User);
    let report = answer(&mut w.store, &env, ask, "1").unwrap();
    assert_eq!(report.answer, KEEP_WORKTREE, "the whole option");
    let close = report.close.unwrap();
    assert_eq!(close.state, TaskState::Closed, "{close:?}");
    assert!(dir.exists(), "kept on disk");
    assert_eq!(
        answered_by(&w),
        [(Actor::User, KEEP_WORKTREE.to_owned())],
        "recorded with who answered"
    );
    let again = answer(&mut w.store, &env, ask, "1").unwrap_err();
    assert!(again.to_string().contains("no longer waits"), "{again}");
}

#[test]
fn the_orchestrator_passes_on_delete_and_the_worktree_goes() {
    use headwork::ops::answer::answer;
    let mut w = world();
    let (task, dir) = dropped_dirty(&mut w, "sand");
    run_tick(&mut w);
    let ask = worktree_ask(&w, &task);
    let env = env_as!(w, Actor::Orchestrator);
    let report = answer(&mut w.store, &env, ask, DELETE_WORKTREE).unwrap();
    assert_eq!(report.by, Actor::Orchestrator, "who answered");
    assert_eq!(report.close.unwrap().state, TaskState::Closed, "closed");
    assert!(!dir.exists(), "deleted on the Delete answer");
    assert_eq!(
        answered_by(&w),
        [(Actor::Orchestrator, DELETE_WORKTREE.to_owned())],
        "recorded"
    );
}

#[test]
fn an_answer_without_an_id_goes_to_the_only_waiting_ask_and_refuses_when_there_is_not_one() {
    use headwork::ops::answer::{answer, only_waiting};
    let mut w = world();
    let none = only_waiting(&w.store).unwrap_err();
    assert!(none.to_string().contains("no ask"), "{none}");
    let (task, _) = dropped_dirty(&mut w, "sand");
    run_tick(&mut w);
    let ask = worktree_ask(&w, &task);
    assert_eq!(only_waiting(&w.store).unwrap(), ask, "the one that waits");
    let (other, _) = dropped_dirty(&mut w, "gravel");
    run_tick(&mut w);
    let many = only_waiting(&w.store).unwrap_err();
    assert!(
        many.to_string().contains("2 asks wait") && many.to_string().contains(&ask.0.to_string()),
        "{many}"
    );
    let env = env_as!(w, Actor::User);
    answer(&mut w.store, &env, ask, "1").unwrap();
    assert_eq!(
        only_waiting(&w.store).unwrap(),
        worktree_ask(&w, &other),
        "the answered one no longer counts"
    );
}

#[test]
fn an_answer_that_names_no_option_or_comes_from_a_worker_changes_nothing() {
    use headwork::ops::answer::answer;
    let mut w = world();
    let (task, dir) = dropped_dirty(&mut w, "sand");
    run_tick(&mut w);
    let ask = worktree_ask(&w, &task);
    let env = env_as!(w, Actor::User);
    for choice in ["Delete", "3", "yes"] {
        let refused = answer(&mut w.store, &env, ask, choice).unwrap_err();
        assert!(
            refused.to_string().contains("none of the options"),
            "{choice}: {refused}"
        );
    }
    let worker = env_as!(w, Actor::Worker);
    assert!(
        answer(&mut w.store, &worker, ask, "2").is_err(),
        "a worker never answers"
    );
    let unknown = answer(&mut w.store, &env, headwork_store::AskId(9999), "1").unwrap_err();
    assert!(unknown.to_string().contains("no ask"), "{unknown}");
    assert!(dir.exists(), "nothing deleted");
    assert_eq!(state(&w, &task), TaskState::Closing, "still closing");
    assert!(answered_by(&w).is_empty(), "no answer recorded");
}

#[test]
fn an_ask_of_a_sessions_own_dialog_is_not_answered_by_id() {
    use headwork::ops::answer::answer;
    let mut w = world();
    let task = spawn_task(&mut w, "sand");
    let session = w.store.session_of(task.id).unwrap().unwrap();
    let ask = w
        .store
        .open_ask(
            &headwork_store::NewAsk {
                task_id: Some(task.id),
                session_id: Some(session.id),
                kind: headwork_core::AskKind::Question,
                question: "Ship it?".to_owned(),
                options: vec!["Yes".to_owned(), "No".to_owned()],
            },
            Actor::Hook,
        )
        .unwrap();
    let env = env_as!(w, Actor::Orchestrator);
    let refused = answer(&mut w.store, &env, ask, "1").unwrap_err();
    assert!(refused.to_string().contains("own dialog"), "{refused}");
}

#[test]
fn force_keeps_the_worktree_of_a_dropped_task_with_no_pull_request() {
    use headwork::ops::close::close_forced;
    let mut w = world();
    let (task, dir) = dropped_dirty(&mut w, "sand");
    run_tick(&mut w);
    let env = env_as!(w, Actor::Orchestrator);
    let report = close_forced(&mut w.store, &env, &w.git, task.id).unwrap();
    assert_eq!(report.state, TaskState::Closed, "{report:?}");
    assert!(dir.exists(), "kept, never deleted");
    let ask = w.store.topic_ask(task.id, "worktree").unwrap().unwrap();
    assert_eq!(
        ask.state,
        headwork_core::AskState::Superseded,
        "the worktree ask no longer waits"
    );
}

#[test]
fn force_still_refuses_a_task_that_was_not_dropped_and_has_no_pull_request() {
    use headwork::ops::close::close_forced;
    let mut w = world();
    let (task, dir) = dirty_closing(&mut w, "sand");
    run_tick(&mut w);
    let env = env_as!(w, Actor::Orchestrator);
    let refused = close_forced(&mut w.store, &env, &w.git, task.id).unwrap_err();
    assert!(refused.to_string().contains("was not dropped"), "{refused}");
    assert!(dir.exists(), "nothing deleted");
    assert_eq!(state(&w, &task), TaskState::Closing, "still closing");
}

#[test]
fn a_dependency_closed_from_review_counts_merged_once_its_pull_request_merged() {
    let mut w = world();
    let first = spawn_task(&mut w, "first");
    start(&mut w, &first, FAKE_PID_BASE, FAKE_PID_BASE);
    let pr = PrInfo {
        repo: "acme/demo".to_owned(),
        number: 48,
        url: "https://github.com/acme/demo/pull/48".to_owned(),
        state: PrState::Open,
        created_at: None,
    };
    w.store
        .record_pr(
            first.id,
            &PrSeen {
                repo: &pr.repo,
                number: pr.number,
                url: &pr.url,
                state: "open",
            },
            Actor::Worker,
        )
        .unwrap();
    w.store
        .transition(first.id, &Trigger::ForceClose, Actor::Orchestrator)
        .unwrap();
    let env = env_as!(w, Actor::User);
    let request = SpawnRequest {
        request: TaskRequest {
            project: ProjectName::new("demo").unwrap(),
            slug: Slug::new("second").unwrap(),
            kind: "task".to_owned(),
            model: Some(Model::Haiku),
            effort: None,
            in_main: false,
            keep: false,
            close: headwork_core::CloseMode::Confirm,
            review: None,
            decision: None,
        },
        brief: &w.brief,
        depends_on: Some(first.id),
        permission_mode: PermissionMode::Auto,
        allow_ask: &[],
    };
    let second = spawn(&mut w.store, &env, &request).unwrap().0;
    run_tick(&mut w);
    assert_eq!(
        state(&w, &first),
        TaskState::Closed,
        "closed without merged"
    );
    assert_eq!(
        state(&w, &second),
        TaskState::Queued,
        "its pull request is still open"
    );
    w.git.set(
        "first",
        PrInfo {
            state: PrState::Merged,
            ..pr
        },
    );
    run_tick(&mut w);
    assert_eq!(
        state(&w, &second),
        TaskState::Spawning,
        "the dependency's pull request merged"
    );
}

const LEADS: &str = "\n[leads]\nenabled = true\n";

fn working_part(w: &mut World, lead: &Task, part: &str) -> (Task, String) {
    let new = headwork_store::NewTask {
        request: TaskRequest {
            project: ProjectName::new("demo").unwrap(),
            slug: Slug::new(format!("{}-{part}", lead.slug())).unwrap(),
            kind: "task".to_owned(),
            model: Some(Model::Haiku),
            effort: None,
            in_main: false,
            keep: false,
            close: headwork_core::CloseMode::Confirm,
            review: None,
            decision: None,
        },
        brief_path: w.brief.display().to_string(),
        depends_on: None,
        permission_mode: PermissionMode::Auto,
        shape: headwork_core::TaskShape::Part { parent: lead.id },
    };
    let task = w.store.create_task(&w.config, &new, Actor::Worker).unwrap();
    let env = Env {
        home: &w.home,
        config: &w.config,
        host: &w.host,
        exe: Path::new("S:/bin/headwork.exe"),
        actor: Actor::User,
    };
    headwork::ops::spawn::pump(&mut w.store, &env).unwrap();
    let child = sleeper();
    let pid = child.id();
    w.children.push(child);
    let session = start(w, &task, pid, pid);
    (w.store.task(task.id).unwrap(), session)
}

fn surface(w: &World, task: &Task) -> SurfaceId {
    SurfaceId(
        w.store
            .session_of(task.id)
            .unwrap()
            .unwrap()
            .host_surface
            .unwrap(),
    )
}

#[test]
fn a_part_whose_lead_failed_is_reported_orphaned_once() {
    let mut w = world_extra(false, 10, LEADS);
    let lead = spawn_task(&mut w, "plan");
    assert!(lead.shape.is_lead(), "a lead");
    let (part, _) = working_part(&mut w, &lead, "review");
    assert_eq!(state(&w, &part), TaskState::Working, "the part runs");
    let quiet = run_tick(&mut w);
    assert!(
        !rules(&quiet)
            .iter()
            .any(|(rule, _)| *rule == FindingId::OrphanPart),
        "a spawning lead is no orphan maker: {:?}",
        quiet.lines
    );
    w.limits.spawn_timeout = 0;
    let report = run_tick(&mut w);
    assert_eq!(state(&w, &lead), TaskState::Failed, "the lead failed");
    let line = report
        .lines
        .iter()
        .find(|line| line.rule == FindingId::OrphanPart)
        .unwrap();
    assert_eq!(line.task.as_deref(), Some("demo-plan-review"), "the part");
    assert_eq!(line.status, Status::Standing, "reported, not acted on");
    assert_eq!(state(&w, &part), TaskState::Working, "left running");
    let raised = count(&w, "finding");
    run_tick(&mut w);
    assert_eq!(count(&w, "finding"), raised, "raised once");
}

#[test]
fn a_part_tab_moved_out_of_its_leads_workspace_is_a_stray_tab_not_a_lost_pane() {
    let mut w = world_extra(false, 10, LEADS);
    let lead = spawn_task(&mut w, "plan");
    let child = sleeper();
    let pid = child.id();
    w.children.push(child);
    start(&mut w, &lead, pid, pid);
    let (part, _) = working_part(&mut w, &lead, "review");
    let quiet = run_tick(&mut w);
    assert!(quiet.lines.is_empty(), "{:?}", quiet.lines);
    w.host.move_surface(&surface(&w, &part), "elsewhere");
    let report = run_tick(&mut w);
    assert_eq!(
        rules(&report),
        [(FindingId::StrayTab, Status::Standing)],
        "{:?}",
        report.lines
    );
    assert_eq!(state(&w, &part), TaskState::Working, "still running");
    assert!(
        !w.host.calls().iter().any(|c| c.starts_with("close-")),
        "nothing closed or moved back: {:?}",
        w.host.calls()
    );
}

const UPDATES_ON: &str = "\n[update]\nenabled = true\nproject = \"demo\"\npoll_min = 15\n";

#[test]
fn the_tick_starts_an_update_when_a_poll_is_due_even_in_shadow_mode() {
    let mut w = world_extra(false, 10, UPDATES_ON);
    let report = run_tick(&mut w);
    let update = report.update.as_ref().unwrap();
    assert_eq!(update.args, ["update"], "a plain update");
    assert_eq!(update.started.as_ref().map(|s| s.pid), Some(99), "pid");
    assert_eq!(w.updates.started.borrow().len(), 1, "started once");
    assert_eq!(count(&w, "update-triggered"), 1, "logged");
    UpdateState {
        last_poll: Some(headwork_core::update::format_utc(
            headwork::ops::update::now_ms(),
        )),
        ..UpdateState::default()
    }
    .save(&w.home)
    .unwrap();
    let report = run_tick(&mut w);
    assert!(report.update.is_none(), "polled just now: not due");
    assert_eq!(w.updates.started.borrow().len(), 1, "nothing more started");
}

#[test]
fn the_tick_starts_no_update_while_one_runs_or_the_trigger_is_off() {
    let mut w = world_extra(false, 10, UPDATES_ON);
    let held = UpdateLock::try_take(&w.home).unwrap().unwrap();
    assert!(run_tick(&mut w).update.is_none(), "an update runs");
    drop(held);
    for extra in [
        "",
        "\n[update]\nenabled = false\nproject = \"demo\"\n",
        "\n[update]\nenabled = true\nproject = \"demo\"\npoll_min = 0\n",
    ] {
        let mut w = world_extra(false, 10, extra);
        assert!(run_tick(&mut w).update.is_none(), "{extra}");
        assert!(w.updates.started.borrow().is_empty(), "{extra}");
        assert_eq!(count(&w, "update-triggered"), 0, "{extra}");
    }
    assert!(
        w.updates.started.borrow().is_empty(),
        "nothing started while locked"
    );
}

fn record_reused(store: &mut Store, session: &str, launcher: u32, claude: u32) {
    let earlier = |pid: u32| {
        headwork_adapters::proc::identify(pid).map(|now| headwork_core::process::ProcessIdentity {
            started: now.started - 3600,
            ..now
        })
    };
    let (launcher_identity, claude_identity) = (earlier(launcher), earlier(claude));
    store
        .session_launched_identified(
            session,
            headwork_store::Launched {
                pid: launcher,
                identity: launcher_identity.as_ref(),
            },
            headwork_store::Launched {
                pid: claude,
                identity: claude_identity.as_ref(),
            },
            "job",
        )
        .unwrap();
}

#[test]
fn enforcing_leaves_a_closing_tasks_reused_pids_alone_and_closes_it() {
    let mut w = world_with(true, 10);
    w.limits.closing_grace = 0;
    let mut child = sleeper();
    let task = closing_with(&mut w, "sand", child.id());
    let session = w.store.session_of(task.id).unwrap().unwrap();
    record_reused(&mut w.store, &session.id, child.id(), child.id());
    let report = run_tick(&mut w);
    assert!(
        !rules(&report).contains(&(FindingId::LeftoverProcess, Status::Applied)),
        "nothing of the session runs: {:?}",
        report.lines
    );
    assert_eq!(state(&w, &task), TaskState::Closed, "closed at once");
    assert!(
        is_alive(child.id()).unwrap(),
        "the program holding the pid runs on"
    );
    assert!(count(&w, "pid-reused") >= 1, "logged");
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn a_merged_grace_close_leaves_a_reused_pid_alone() {
    let mut w = world_with(true, 10);
    let task = merged(&mut w, "sand");
    let session = w.store.session_of(task.id).unwrap().unwrap();
    let pid = session.claude_pid.unwrap();
    let launcher = sleeper();
    let own = headwork_adapters::proc::identify(launcher.id());
    let earlier =
        headwork_adapters::proc::identify(pid).map(|now| headwork_core::process::ProcessIdentity {
            started: now.started - 3600,
            ..now
        });
    w.store
        .session_launched_identified(
            &session.id,
            headwork_store::Launched {
                pid: launcher.id(),
                identity: own.as_ref(),
            },
            headwork_store::Launched {
                pid,
                identity: earlier.as_ref(),
            },
            "job",
        )
        .unwrap();
    w.children.push(launcher);
    reported(&w, &task, &session.id);
    w.limits.merged_grace = 0;
    let report = run_tick(&mut w);
    assert_eq!(
        rules(&report)[0],
        (FindingId::MergedGrace, Status::Applied),
        "closing"
    );
    assert!(
        is_alive(pid).unwrap(),
        "the program holding the pid runs on"
    );
    assert!(count(&w, "pid-reused") >= 1, "logged");
}

fn origin_with_note(w: &World) {
    let origin = w.dir.path().join("origin");
    std::fs::create_dir_all(origin.join("migrations")).unwrap();
    git_in(&origin, &["init", "-q", "-b", "master"]);
    std::fs::write(
        origin.join("migrations/n.toml"),
        "id = \"n\"\ntitle = \"Note\"\ncheck = \"false\"\npull = false\n\n[[step]]\nkind = \"agent\"\nrun = \"true\"\n",
    )
    .unwrap();
    git_in(&origin, &["add", "."]);
    git_in(&origin, &["commit", "-q", "-m", "note"]);
    let main = w.dir.path().join("demo");
    git_in(
        &main,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git_in(&main, &["fetch", "-q", "origin", "master"]);
}

#[test]
fn a_pending_migration_note_is_a_finding_until_it_is_applied() {
    let mut w = world();
    origin_with_note(&w);
    let report = run_tick(&mut w);
    assert!(
        rules(&report).contains(&(FindingId::MigrationPending, Status::Standing)),
        "{:?}",
        rules(&report)
    );
    assert_eq!(
        w.store
            .open_finding_keys(FindingId::MigrationPending)
            .unwrap(),
        ["migration-pending:demo/n"],
        "raised once"
    );

    let applied = headwork::ops::migrate::apply(
        &mut w.store,
        &w.home,
        &w.config,
        "demo/n",
        &headwork::ops::migrate::ApplyOptions::default(),
        Actor::User,
    )
    .unwrap();
    assert_eq!(applied.exit_code(), 0, "{applied}");

    let report = run_tick(&mut w);
    assert!(
        !rules(&report).contains(&(FindingId::MigrationPending, Status::Standing)),
        "{:?}",
        rules(&report)
    );
    assert!(
        w.store
            .open_finding_keys(FindingId::MigrationPending)
            .unwrap()
            .is_empty(),
        "cleared"
    );
}

#[test]
fn a_repository_that_cannot_be_read_keeps_its_findings_and_raises_a_tick_error() {
    let mut w = world();
    origin_with_note(&w);
    run_tick(&mut w);
    let main = w.dir.path().join("demo");
    git_in(&main, &["remote", "set-url", "origin", "/no/such/origin"]);
    git_in(&main, &["update-ref", "-d", "refs/remotes/origin/master"]);
    let report = run_tick(&mut w);
    assert!(
        rules(&report).contains(&(FindingId::TickError, Status::Standing)),
        "{:?}",
        rules(&report)
    );
    assert_eq!(
        w.store
            .open_finding_keys(FindingId::MigrationPending)
            .unwrap(),
        ["migration-pending:demo/n"],
        "the finding of the unreadable repository stays"
    );
    assert!(
        w.store
            .open_finding_keys(FindingId::TickError)
            .unwrap()
            .contains(&"tick-error:migrations:demo".to_owned()),
        "the error names the repository"
    );
}

#[test]
fn a_queued_task_on_a_gone_dependency_is_told_once_and_logged_once() {
    let mut w = world();
    let gone = spawn_task(&mut w, "engine");
    let request = SpawnRequest {
        request: TaskRequest {
            project: ProjectName::new("demo").unwrap(),
            slug: Slug::new("scene").unwrap(),
            kind: "task".to_owned(),
            model: Some(Model::Haiku),
            effort: None,
            in_main: false,
            keep: false,
            close: headwork_core::CloseMode::Confirm,
            review: None,
            decision: None,
        },
        brief: &w.brief,
        depends_on: Some(gone.id),
        permission_mode: PermissionMode::Auto,
        allow_ask: &[],
    };
    let env = Env {
        home: &w.home,
        config: &w.config,
        host: &w.host,
        exe: Path::new("S:/bin/headwork.exe"),
        actor: Actor::User,
    };
    let dependent = spawn(&mut w.store, &env, &request).unwrap().0;
    w.store.drop_task(gone.id, Actor::Orchestrator).unwrap();

    let first = run_tick(&mut w);
    assert!(
        rules(&first).contains(&(FindingId::DeadDependency, Status::Standing)),
        "{:?}",
        first.lines
    );
    assert!(notified(&w, "never merge"), "{:?}", w.host.calls());
    let board = w.store.board(false).unwrap();
    let entry = board
        .tasks
        .iter()
        .find(|entry| entry.task.id == dependent.id)
        .unwrap();
    assert_eq!(
        entry.dead_dependency.as_deref(),
        Some("demo-engine (dropped)"),
        "board"
    );

    let notices = |w: &World| {
        w.host
            .calls()
            .iter()
            .filter(|c| c.starts_with("notify") && c.contains("never merge"))
            .count()
    };
    run_tick(&mut w);
    run_tick(&mut w);
    assert_eq!(notices(&w), 1, "told once");
    let gone_refusals = w
        .store
        .events(EventFilter {
            task: None,
            limit: 10_000,
        })
        .unwrap()
        .iter()
        .filter(|e| e.kind == "refused" && e.data.to_string().contains("is gone"))
        .count();
    assert_eq!(gone_refusals, 1, "one refusal logged, not one per tick");
    assert_eq!(state(&w, &dependent), TaskState::Queued, "still queued");
}
