//! Spawn → park → resume against the fake host, a temporary home, a temporary git repo, and sleeper processes standing
//! in for `claude` and for long jobs.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use headwork::home::Home;
use headwork::ops::Env;
use headwork::ops::jobs::{Registration, register};
use headwork::ops::launch::{StartMode, claude_command};
use headwork::ops::park::{ParkOptions, RecapSource, park};
use headwork::ops::resume::{ResumeOptions, TaskOutcome, last_mode, resume, would_close};
use headwork::ops::spawn::{SpawnRequest, pump, spawn};
use headwork_adapters::host::fake::FakeHost;
use headwork_adapters::host::{SurfaceState, TerminalHost};
use headwork_adapters::proc::is_alive;
use headwork_core::config::ParkPolicy;
use headwork_core::lifecycle::SessionFacts;
use headwork_core::{
    Actor, AskKind, Config, JobState, MachineMode, Model, PermissionMode, ProjectName, RecapKind,
    Role, SessionState, Slug, TaskRequest, TaskShape, TaskState, Trigger,
};
use headwork_store::{EventFilter, NewAsk, NewRecap, NewSession, NewTask, Store, Task};

const EXE: &str = "S:/bin/headwork.exe";
const SLEEPER_ROLE: &str = "HEADWORK_PARK_ROLE";

struct World {
    dir: tempfile::TempDir,
    home: Home,
    config: Config,
    store: Store,
    host: FakeHost,
    brief: PathBuf,
    children: Vec<Child>,
}

impl Drop for World {
    fn drop(&mut self) {
        for child in &mut self.children {
            drop(child.kill());
            drop(child.wait());
        }
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

fn world(resume_after_crash: &str) -> World {
    world_with(resume_after_crash, "")
}

fn world_with(resume_after_crash: &str, extra: &str) -> World {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("demo");
    std::fs::create_dir(&main).unwrap();
    git_in(&main, &["init", "-q", "-b", "master"]);
    git_in(&main, &["config", "user.name", "t"]);
    git_in(&main, &["config", "user.email", "t@t"]);
    std::fs::write(main.join("tracked.txt"), "one\n").unwrap();
    git_in(&main, &["add", "."]);
    git_in(&main, &["commit", "-q", "-m", "init"]);
    let main_text = main.display().to_string().replace('\\', "/");
    let text = format!(
        "[machine]\nhost = \"wmux\"\nresume_after_crash = \"{resume_after_crash}\"\n\n[project.demo]\nmain = \"{main_text}\"\n\
         worktree_new = \"builtin\"\nworktree_remove = \"builtin\"\nworktree_path = \"{{main}}-{{slug}}\"\n\
         deliver = \"pr\"\nmerge = \"deliver\"\n{extra}"
    );
    let config = Config::parse(&text, "test").unwrap();
    let home = Home::at(Some(dir.path().join("home")))
        .unwrap()
        .with_claude_json(dir.path().join(".claude.json"))
        .with_claude_dir(dir.path().join(".claude"));
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
        children: Vec::new(),
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

impl World {
    fn sleeper(&mut self) -> u32 {
        let child = Command::new(std::env::current_exe().unwrap())
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
            .unwrap();
        let pid = child.id();
        self.children.push(child);
        pid
    }

    fn working(&mut self, slug: &str) -> (Task, String, u32) {
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
            brief: &self.brief,
            depends_on: None,
            permission_mode: PermissionMode::Auto,
            allow_ask: &[],
        };
        let env = env!(self);
        let task = spawn(&mut self.store, &env, &request).unwrap().0;
        let pid = self.sleeper();
        let session = self.store.session_of(task.id).unwrap().unwrap();
        self.store
            .session_launched(&session.id, pid, pid, "job")
            .unwrap();
        self.store
            .transition(
                task.id,
                &Trigger::SessionStarted(SessionFacts {
                    session_matches: true,
                    launcher_alive: true,
                }),
                Actor::Hook,
            )
            .unwrap();
        (self.store.task(task.id).unwrap(), session.id, pid)
    }

    fn part(&mut self, lead: &Task, part: &str) -> (Task, String, u32) {
        let new = NewTask {
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
            brief_path: self.brief.display().to_string(),
            depends_on: None,
            permission_mode: PermissionMode::Auto,
            shape: TaskShape::Part { parent: lead.id },
        };
        let task = self
            .store
            .create_task(&self.config, &new, Actor::Worker)
            .unwrap();
        let env = env!(self);
        pump(&mut self.store, &env).unwrap();
        self.launched(task.id)
    }

    fn launched(&mut self, id: headwork_store::TaskId) -> (Task, String, u32) {
        let pid = self.sleeper();
        let session = self.store.session_of(id).unwrap().unwrap();
        self.store
            .session_launched(&session.id, pid, pid, "job")
            .unwrap();
        self.store
            .transition(
                id,
                &Trigger::SessionStarted(SessionFacts {
                    session_matches: true,
                    launcher_alive: true,
                }),
                Actor::Hook,
            )
            .unwrap();
        (self.store.task(id).unwrap(), session.id, pid)
    }

    fn crash(&mut self, task: &Task, pid: u32) {
        self.store
            .transition(
                task.id,
                &Trigger::ProcessLost {
                    machine: MachineMode::Normal,
                },
                Actor::Tick,
            )
            .unwrap();
        assert!(
            headwork_adapters::proc::kill_tree(pid).unwrap(),
            "the sleeper is ended"
        );
    }

    fn state(&self, task: &Task) -> TaskState {
        self.store.task(task.id).unwrap().state
    }
}

fn dead(pid: u32) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if !is_alive(pid).unwrap() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

const fn quick(deadline_ms: u64) -> ParkOptions {
    ParkOptions {
        deadline: Duration::from_millis(deadline_ms),
        poll: Duration::from_millis(50),
    }
}

fn job(label: &str, policy: ParkPolicy, pid: u32, checkpoint: Option<&Path>) -> Registration {
    Registration {
        label: label.to_owned(),
        argv: vec![
            std::env::current_exe().unwrap().display().to_string(),
            "--exact".to_owned(),
            "helper_sleeper".to_owned(),
            "--ignored".to_owned(),
        ],
        cwd: None,
        pid: Some(pid),
        park_policy: Some(policy),
        checkpoint_path: checkpoint.map(|p| p.display().to_string()),
        restart: None,
        heavy: false,
    }
}

fn claude_dir_with(w: &World, sessions: &[&str]) -> PathBuf {
    let dir = w.dir.path().join("claude");
    let projects = dir.join("projects").join("S--demo");
    std::fs::create_dir_all(&projects).unwrap();
    for id in sessions {
        std::fs::write(projects.join(format!("{id}.jsonl")), "{}\n").unwrap();
    }
    dir
}

#[test]
#[ignore = "a helper started by the park tests"]
fn helper_sleeper() {
    if std::env::var_os(SLEEPER_ROLE).is_some() {
        std::thread::sleep(Duration::from_secs(120));
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario from spawn to resume reads best in one piece"
)]
fn spawn_park_resume_round_trip() {
    let mut w = world("ask");
    let (sand, sand_session, sand_pid) = w.working("sand");
    let (mist, mist_session, mist_pid) = w.working("mist");
    std::fs::write(
        w.dir.path().join("demo-sand").join("tracked.txt"),
        "changed\n",
    )
    .unwrap();
    w.store
        .open_ask(
            &NewAsk {
                task_id: Some(mist.id),
                session_id: Some(mist_session.clone()),
                kind: AskKind::Question,
                question: "Ship it?".to_owned(),
                options: vec!["Ship (Recommended)".to_owned(), "Hold".to_owned()],
            },
            Actor::Hook,
        )
        .unwrap();
    w.store
        .transition(mist.id, &Trigger::AskOpened, Actor::Hook)
        .unwrap();

    let checkpoint = w.dir.path().join("checkpoint.json");
    std::fs::write(&checkpoint, "{}").unwrap();
    let train_pid = w.sleeper();
    let render_pid = w.sleeper();
    let env = env!(w);
    let train = register(
        &mut w.store,
        &env,
        &sand,
        &sand_session,
        &job(
            "train",
            ParkPolicy::Checkpoint,
            train_pid,
            Some(&checkpoint),
        ),
    )
    .unwrap();
    let render = register(
        &mut w.store,
        &env,
        &sand,
        &sand_session,
        &job("render", ParkPolicy::Rerun, render_pid, None),
    )
    .unwrap();

    let home = w.home.clone();
    let writer_task = sand.id;
    let sand_session_id = sand_session.clone();
    let checkpoint_file = checkpoint;
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        std::fs::write(&checkpoint_file, "{\"stage\":2}").unwrap();
        let mut store = home.open_store().unwrap();
        store
            .add_recap(
                &NewRecap {
                    task_id: Some(writer_task),
                    session_id: Some(&sand_session_id),
                    kind: RecapKind::Session,
                    body: "Rendering stage 2; next: compare the clips.",
                },
                Actor::Worker,
            )
            .unwrap();
    });
    let env = env!(w);
    let report = park(&mut w.store, &env, quick(20_000)).unwrap();
    writer.join().unwrap();

    assert!(report.safe_to_restart, "{report:?}");
    assert_eq!(
        w.store.machine_mode().unwrap(),
        MachineMode::Parked,
        "machine"
    );
    assert_eq!(w.state(&sand), TaskState::Parked, "sand");
    assert_eq!(w.state(&mist), TaskState::Parked, "mist");
    let by_name = |name: &str| report.sessions.iter().find(|s| s.name == name).unwrap();
    assert_eq!(
        by_name("demo-sand").recap,
        RecapSource::Session,
        "own recap"
    );
    assert!(by_name("demo-sand").snapshot.is_some(), "dirty tree saved");
    assert_eq!(
        by_name("demo-mist").recap,
        RecapSource::Mechanical,
        "blocked session"
    );
    assert_eq!(
        by_name("demo-mist").open_question.as_deref(),
        Some("Ship it?")
    );
    let handoff = std::fs::read_to_string(w.home.handoff_path(&sand.name)).unwrap();
    assert!(
        handoff.contains("compare the clips") && handoff.contains("tracked.txt"),
        "{handoff}"
    );
    let mist_handoff = std::fs::read_to_string(w.home.handoff_path(&mist.name)).unwrap();
    assert!(
        mist_handoff.contains("ask this again after the resume: Ship it?"),
        "{mist_handoff}"
    );
    assert!(dead(sand_pid) && dead(mist_pid), "sessions ended");
    assert!(dead(train_pid) && dead(render_pid), "jobs stopped");
    assert_eq!(
        w.store.job(train.id).unwrap().unwrap().state,
        JobState::Parked,
        "train"
    );
    assert!(
        w.store
            .job(train.id)
            .unwrap()
            .unwrap()
            .note
            .unwrap()
            .contains("fresh checkpoint"),
        "stopped after the checkpoint, not at the deadline"
    );
    assert_eq!(
        w.store.job(render.id).unwrap().unwrap().state,
        JobState::Parked,
        "render"
    );
    let calls = w.host.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("send ") && c.contains("recap")),
        "{calls:?}"
    );
    assert_eq!(
        calls.iter().filter(|c| c.starts_with("send ")).count(),
        1,
        "blocked session not nudged"
    );
    assert!(
        w.store.waiting_questions(mist.id).unwrap().is_empty(),
        "ask superseded"
    );
    let refused = {
        let env = env!(w);
        spawn(
            &mut w.store,
            &env,
            &SpawnRequest {
                request: TaskRequest {
                    project: ProjectName::new("demo").unwrap(),
                    slug: Slug::new("late").unwrap(),
                    kind: "task".to_owned(),
                    model: None,
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
            },
        )
    };
    assert!(refused.is_err(), "no spawn while parked");

    let restored = w.host.add_restored_surface("demo");
    w.host.add_state(SurfaceState {
        surface: restored.clone(),
        state: "idle".to_owned(),
        session_id: Some(sand_session.clone()),
        blocked_reason: None,
    });
    let claude_dir = claude_dir_with(&w, &[&sand_session]);
    let opts = ResumeOptions {
        logon: false,
        wait_for_host: Duration::ZERO,
        claude_dir,
    };
    std::fs::write(w.home.claude_json(), "{}").unwrap();
    let env = env!(w);
    let resumed = resume(&mut w.store, &env, &opts).unwrap();
    assert_eq!(
        w.store.machine_mode().unwrap(),
        MachineMode::Normal,
        "machine"
    );
    let main_key =
        headwork_adapters::trust::project_key(&w.dir.path().join("demo").display().to_string());
    assert!(
        headwork_adapters::trust::is_trusted(w.home.claude_json(), &main_key).unwrap(),
        "resume trusts the main checkout again before the panes open"
    );
    assert!(
        w.host
            .calls()
            .contains(&format!("close-surface {restored}")),
        "the pane the host restored by itself is closed first: {:?}",
        w.host.calls()
    );
    assert_eq!(w.state(&sand), TaskState::Spawning, "sand relaunching");
    let outcome = |name: &str| resumed.tasks.iter().find(|t| t.name == name).unwrap();
    assert!(
        matches!(outcome("demo-sand").outcome, TaskOutcome::Fresh { .. }),
        "{resumed:?}"
    );
    assert!(
        matches!(&outcome("demo-mist").outcome, TaskOutcome::Fresh { reason, .. } if reason.contains("new session")),
        "{resumed:?}"
    );
    assert!(
        outcome("demo-mist")
            .open_question
            .as_deref()
            .is_some_and(|q| q.starts_with("Ship it?")),
        "{resumed:?}"
    );
    let panes: Vec<String> = w
        .host
        .panes()
        .unwrap()
        .into_iter()
        .filter(|p| p.running)
        .map(|p| p.command)
        .collect();
    assert!(
        panes
            .iter()
            .any(|c| c.contains("--fresh") && c.contains(&format!("--task {}", sand.id))),
        "{panes:?}"
    );
    assert!(
        panes
            .iter()
            .any(|c| c.contains("--fresh") && c.contains(&format!("--task {}", mist.id))),
        "{panes:?}"
    );
    assert_eq!(resumed.jobs.len(), 2, "{resumed:?}");
    for id in [train.id, render.id] {
        let row = w.store.job(id).unwrap().unwrap();
        assert_eq!(row.state, JobState::Running, "restarted");
        assert!(
            row.pid
                .is_some_and(|pid| pid != train_pid && pid != render_pid),
            "a new process"
        );
    }
    let summary = std::fs::read_to_string(&resumed.summary).unwrap();
    assert!(
        summary.contains("demo-sand") && summary.contains("Ship it?") && summary.contains("train"),
        "{summary}"
    );
    let parks = w.store.current_park().unwrap().unwrap();
    assert!(
        parks.resumed_at.is_some() && parks.recap_path.is_some(),
        "park closed out"
    );

    let session = w.store.session_of(sand.id).unwrap().unwrap();
    assert_ne!(session.id, sand_session, "a new conversation");
    let task = w.store.task(sand.id).unwrap();
    let command = claude_command(
        &w.home,
        &w.config,
        &task,
        &session,
        &w.dir.path().join("demo-sand"),
        Path::new(EXE),
        StartMode::Fresh { crash: false },
    )
    .unwrap();
    let args: Vec<String> = command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args.first().map(String::as_str),
        Some("--session-id"),
        "{args:?}"
    );
    assert_eq!(args.get(1), Some(&session.id), "{args:?}");
    assert!(args.last().unwrap().contains("new session"), "{args:?}");
    assert!(args.last().unwrap().contains("handoffs"), "{args:?}");
    assert_ne!(
        mist_session,
        w.store.session_of(mist.id).unwrap().unwrap().id,
        "a new conversation"
    );
}

#[test]
fn the_deadline_forces_a_mechanical_recap_and_a_job_left_running_is_reported() {
    let mut w = world("ask");
    let (sand, session, _) = w.working("sand");
    let checkpoint = w.dir.path().join("checkpoint.json");
    std::fs::write(&checkpoint, "{}").unwrap();
    let train_pid = w.sleeper();
    let publish_pid = w.sleeper();
    let env = env!(w);
    let train = register(
        &mut w.store,
        &env,
        &sand,
        &session,
        &job(
            "train",
            ParkPolicy::Checkpoint,
            train_pid,
            Some(&checkpoint),
        ),
    )
    .unwrap();
    let publish = register(
        &mut w.store,
        &env,
        &sand,
        &session,
        &job("publish", ParkPolicy::Finish, publish_pid, None),
    )
    .unwrap();
    let report = park(&mut w.store, &env, quick(400)).unwrap();
    assert!(!report.safe_to_restart, "a job is left running: {report:?}");
    assert_eq!(
        report.sessions[0].recap,
        RecapSource::Mechanical,
        "no recap in time"
    );
    assert_eq!(w.state(&sand), TaskState::Parked, "forced");
    let train = w.store.job(train.id).unwrap().unwrap();
    assert!(train.note.unwrap().contains("deadline"), "lost work noted");
    assert!(dead(train_pid), "stopped");
    let publish = w.store.job(publish.id).unwrap().unwrap();
    assert_eq!(publish.state, JobState::Running, "left running");
    assert!(is_alive(publish_pid).unwrap(), "still alive");
    assert!(
        report
            .jobs
            .iter()
            .any(|j| j.label == "publish" && j.left_running),
        "{report:?}"
    );
    let again = park(&mut w.store, &env, quick(100));
    assert!(again.is_err(), "already parked");
}

#[test]
fn a_crash_resumes_by_hand_but_waits_for_the_root_at_logon() {
    let mut w = world("ask");
    let (sand, session, pid) = w.working("sand");
    w.store
        .transition(
            sand.id,
            &Trigger::ProcessLost {
                machine: MachineMode::Normal,
            },
            Actor::Tick,
        )
        .unwrap();
    assert!(headwork_adapters::proc::kill_tree(pid).unwrap());
    let claude_dir = claude_dir_with(&w, &[&session]);
    let env = env!(w);
    let logon = ResumeOptions {
        logon: true,
        wait_for_host: Duration::ZERO,
        claude_dir: claude_dir.clone(),
    };
    let report = resume(&mut w.store, &env, &logon).unwrap();
    assert!(
        matches!(report.tasks[0].outcome, TaskOutcome::Skipped { .. }),
        "{report:?}"
    );
    assert_eq!(
        w.state(&sand),
        TaskState::Interrupted,
        "left for the orchestrator to ask"
    );
    let by_hand = ResumeOptions {
        logon: false,
        wait_for_host: Duration::ZERO,
        claude_dir,
    };
    let env = env!(w);
    let report = resume(&mut w.store, &env, &by_hand).unwrap();
    assert!(
        matches!(report.tasks[0].outcome, TaskOutcome::Fresh { .. }),
        "{report:?}"
    );
    assert!(
        std::fs::read_to_string(&report.summary)
            .unwrap()
            .contains("crash"),
        "the summary says it was a crash"
    );
    assert!(
        panes_of(&w)
            .iter()
            .any(|c| c.contains("--fresh") && c.contains("--crash")),
        "{:?}",
        panes_of(&w)
    );
    let handoff =
        std::fs::read_to_string(w.home.handoff_path(&w.store.task(sand.id).unwrap().name)).unwrap();
    assert!(
        handoff.contains("resumed after a crash") && handoff.contains("mechanical recap"),
        "a crash leaves no handoff, so resume writes one: {handoff}"
    );
}

fn fresh_orchestrator(line: &str, old: &str) -> String {
    assert!(
        !line.contains("--resume"),
        "never continues the old conversation: {line}"
    );
    let id = line
        .split("--session ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap()
        .to_owned();
    assert_ne!(id, old, "a new session: {line}");
    id
}

fn panes_of(w: &World) -> Vec<String> {
    w.host
        .panes()
        .unwrap()
        .into_iter()
        .map(|p| p.command)
        .collect()
}

#[test]
fn resume_refuses_while_a_park_runs() {
    let mut w = world("ask");
    w.working("sand");
    w.store
        .set_machine_mode(MachineMode::Parking, Actor::User)
        .unwrap();
    let env = env!(w);
    let opts = ResumeOptions {
        logon: false,
        wait_for_host: Duration::ZERO,
        claude_dir: w.dir.path().join("claude"),
    };
    assert!(
        resume(&mut w.store, &env, &opts).is_err(),
        "a park is running"
    );
}

impl World {
    /// A running orchestrator session in the user's own pane, as `headwork orchestrator` records it.
    fn orchestrator(&mut self) -> (String, u32, PathBuf) {
        let id = format!("orchestrator-{}", self.children.len());
        self.store
            .record_session(
                &NewSession {
                    id: id.clone(),
                    task_id: None,
                    role: Role::Orchestrator,
                    rc_name: "orchestrator".to_owned(),
                    host: "wmux".to_owned(),
                    host_workspace: None,
                    host_surface: None,
                },
                Actor::User,
            )
            .unwrap();
        let pid = self.sleeper();
        self.store.session_launched(&id, pid, pid, "job").unwrap();
        let dir = self.dir.path().join("code");
        std::fs::create_dir_all(&dir).unwrap();
        self.store
            .set_session_dir(&id, &dir.display().to_string().replace('\\', "/"))
            .unwrap();
        (id, pid, dir)
    }

    fn seen_in(&self, workspace: &str, session: &str) -> String {
        let surface = self.host.add_restored_surface(workspace);
        self.host.add_state(SurfaceState {
            surface: surface.clone(),
            state: "idle".to_owned(),
            session_id: Some(session.to_owned()),
            blocked_reason: None,
        });
        surface.to_string()
    }
}

#[test]
fn park_asks_the_root_for_a_recap_and_leaves_it_running_and_resume_brings_it_back() {
    let mut w = world("ask");
    let (sand, sand_session, _) = w.working("sand");
    let (orchestrator, root_pid, root_dir) = w.orchestrator();
    let surface = w.seen_in("Workspace 1", &orchestrator);
    let record = w.store.begin_park(60, Actor::User).unwrap();
    w.store
        .add_recap(
            &NewRecap {
                task_id: None,
                session_id: Some(&orchestrator),
                kind: RecapKind::Session,
                body: "coordinating sand; the user waits on nothing",
            },
            Actor::Orchestrator,
        )
        .unwrap();
    let env = env!(w);
    let report = park(&mut w.store, &env, quick(400)).unwrap();
    assert_eq!(report.park, record.id, "the park carries on");
    let parked_root = report.orchestrator.clone().unwrap();
    assert_eq!(
        parked_root.session, orchestrator,
        "the orchestrator is recorded"
    );
    assert_eq!(parked_root.recap, RecapSource::Session, "{report:?}");
    assert!(
        w.host
            .calls()
            .iter()
            .any(|c| c.starts_with(&format!("send {surface} ")) && c.contains("recap")),
        "the orchestrator is asked for a recap: {:?}",
        w.host.calls()
    );
    assert_eq!(w.events_of("orchestrator-asked"), 1, "the ask is logged");
    assert!(
        is_alive(root_pid).unwrap(),
        "the orchestrator keeps running"
    );
    assert_eq!(w.state(&sand), TaskState::Parked, "the worker is parked");
    assert!(report.safe_to_restart, "{report:?}");

    assert!(headwork_adapters::proc::kill_tree(root_pid).unwrap());
    assert!(dead(root_pid), "the restart ends the orchestrator");
    let restored = w.seen_in("Workspace 1", &orchestrator);
    let claude_dir = claude_dir_with(&w, &[&sand_session, &orchestrator]);
    let env = env!(w);
    let opts = ResumeOptions {
        logon: true,
        wait_for_host: Duration::ZERO,
        claude_dir,
    };
    let report = resume(&mut w.store, &env, &opts).unwrap();
    assert!(
        matches!(report.orchestrator, Some(TaskOutcome::Fresh { .. })),
        "{report:?}"
    );
    let line = panes_of(&w)
        .into_iter()
        .find(|c| c.contains(" orchestrator "))
        .unwrap();
    let fresh = fresh_orchestrator(&line, &orchestrator);
    assert!(
        w.host
            .calls()
            .contains(&format!("close-surface {restored}")),
        "the pane the host restored for the orchestrator is closed: {:?}",
        w.host.calls()
    );
    let placed = w.store.session(&fresh).unwrap().unwrap().0;
    assert!(placed.host_surface.is_some(), "the new pane is recorded");
    assert_eq!(
        w.store.session_dir(&fresh).unwrap().unwrap(),
        root_dir.display().to_string().replace('\\', "/"),
        "same folder"
    );
    let summary = std::fs::read_to_string(&report.summary).unwrap();
    assert!(summary.contains("## Orchestrator"), "{summary}");
    assert!(summary.contains("coordinating sand"), "{summary}");
}

#[test]
fn an_orchestrator_whose_transcript_is_gone_starts_fresh_and_a_running_one_is_left_alone() {
    let mut w = world("never");
    let (orchestrator, root_pid, _) = w.orchestrator();
    let env = env!(w);
    let report = park(&mut w.store, &env, quick(200)).unwrap();
    let parked_root = report.orchestrator.clone().unwrap();
    assert_eq!(parked_root.recap, RecapSource::Mechanical, "{report:?}");
    assert!(parked_root.problem.is_some(), "its pane is unknown");
    assert_eq!(
        w.events_of("orchestrator-unasked"),
        1,
        "why it was not asked is logged"
    );
    let mechanical = w
        .store
        .latest_session_recap(&orchestrator)
        .unwrap()
        .unwrap();
    assert!(
        mechanical.body.contains("its pane is unknown"),
        "the mechanical recap says why: {}",
        mechanical.body
    );

    let opts = ResumeOptions {
        logon: false,
        wait_for_host: Duration::ZERO,
        claude_dir: claude_dir_with(&w, &[]),
    };
    let report = resume(&mut w.store, &env, &opts).unwrap();
    assert!(
        matches!(&report.orchestrator, Some(TaskOutcome::Skipped { reason }) if reason.contains("still runs")),
        "the orchestrator never stopped: {report:?}"
    );

    assert!(headwork_adapters::proc::kill_tree(root_pid).unwrap());
    assert!(dead(root_pid), "a crash ends the orchestrator");
    let logon = ResumeOptions {
        logon: true,
        ..opts
    };
    let report = resume(&mut w.store, &env, &logon).unwrap();
    assert!(
        matches!(report.orchestrator, Some(TaskOutcome::Fresh { .. })),
        "a crashed orchestrator comes back at logon even with resume_after_crash = never: {report:?}"
    );
    let line = panes_of(&w)
        .into_iter()
        .find(|c| c.contains(" orchestrator "))
        .unwrap();
    assert!(!line.contains("--resume"), "{line}");
    assert!(!line.contains(&orchestrator), "a new session: {line}");
}

impl World {
    /// The orchestrator's prompt hook firing: the start of one of its turns.
    fn prompted(&self, session: &str) {
        self.store
            .append_event(
                Actor::Hook,
                None,
                Some(session),
                "prompt",
                &serde_json::json!({ "cross_session": false, "from": null }),
            )
            .unwrap();
    }

    fn orchestrator_recap(&mut self, session: &str, body: &str) {
        self.store
            .add_recap(
                &NewRecap {
                    task_id: None,
                    session_id: Some(session),
                    kind: RecapKind::Session,
                    body,
                },
                Actor::Orchestrator,
            )
            .unwrap();
    }
}

#[test]
fn park_takes_the_recap_the_orchestrator_wrote_in_the_turn_that_started_it() {
    let mut w = world("ask");
    let (orchestrator, _, _) = w.orchestrator();
    let surface = w.seen_in("Workspace 1", &orchestrator);
    w.prompted(&orchestrator);
    w.orchestrator_recap(&orchestrator, "coordinating sand before the restart");
    let env = env!(w);
    let started = Instant::now();
    let report = park(&mut w.store, &env, quick(5_000)).unwrap();
    let parked = report.orchestrator.clone().unwrap();
    assert_eq!(parked.recap, RecapSource::Session, "{report:?}");
    assert_eq!(parked.problem, None, "{report:?}");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "the park does not wait for a recap it already has"
    );
    assert!(
        !w.host
            .calls()
            .iter()
            .any(|c| c.starts_with(&format!("send {surface} "))),
        "the orchestrator is not asked again: {:?}",
        w.host.calls()
    );
    assert_eq!(w.events_of("recap-adopted"), 1, "logged");
    let recap = w
        .store
        .orchestrator_park_recap(report.park, &orchestrator)
        .unwrap()
        .unwrap();
    assert_eq!(recap.kind, RecapKind::Session, "its own recap");
    assert_eq!(
        recap.body, "coordinating sand before the restart",
        "its own words"
    );
}

#[test]
fn a_recap_from_before_the_orchestrators_latest_turn_does_not_count_and_it_is_asked() {
    let mut w = world("ask");
    let (orchestrator, _, _) = w.orchestrator();
    let surface = w.seen_in("Workspace 1", &orchestrator);
    w.orchestrator_recap(&orchestrator, "an old recap");
    std::thread::sleep(Duration::from_millis(20));
    w.prompted(&orchestrator);
    let env = env!(w);
    let report = park(&mut w.store, &env, quick(300)).unwrap();
    let parked = report.orchestrator.clone().unwrap();
    assert_eq!(
        parked.recap,
        RecapSource::Mechanical,
        "it never answered: {report:?}"
    );
    assert!(
        w.host
            .calls()
            .iter()
            .any(|c| c.starts_with(&format!("send {surface} ")) && c.contains("recap")),
        "the orchestrator is asked for a recap: {:?}",
        w.host.calls()
    );
    assert_eq!(w.events_of("recap-adopted"), 0, "nothing adopted");
    assert_eq!(w.events_of("orchestrator-asked"), 1, "the ask is logged");
}

impl World {
    /// An orchestrator session the store recorded and saw end, whose conversation the host runs again outside headwork
    /// in the surface returned, as wmux does after a reboot with a plain `claude --resume <id>`.
    fn unmanaged_orchestrator(&mut self) -> (String, String) {
        let (id, pid, _) = self.orchestrator();
        assert!(
            headwork_adapters::proc::kill_tree(pid).unwrap(),
            "the sleeper is ended"
        );
        assert!(dead(pid), "the managed process is gone");
        self.store
            .set_session_state(&id, SessionState::Ended, Actor::Hook)
            .unwrap();
        let surface = self.seen_in("Workspace 1", &id);
        (id, surface)
    }

    fn events_of(&self, kind: &str) -> usize {
        self.store
            .events(EventFilter {
                task: None,
                limit: 500,
            })
            .unwrap()
            .iter()
            .filter(|event| event.kind == kind)
            .count()
    }
}

#[test]
fn park_records_an_orchestrator_running_outside_headwork_and_resume_brings_it_back_under_headwork()
{
    let mut w = world("ask");
    let (_, sand_session, _) = w.working("sand");
    let (orchestrator, surface) = w.unmanaged_orchestrator();
    let env = env!(w);
    let report = park(&mut w.store, &env, quick(200)).unwrap();
    let parked = report.orchestrator.clone().unwrap();
    assert_eq!(
        parked.session, orchestrator,
        "the park records it: {report:?}"
    );
    assert_eq!(
        parked.recap,
        RecapSource::Mechanical,
        "it has no recap tool"
    );
    assert!(
        parked
            .problem
            .as_deref()
            .is_some_and(|p| p.contains("outside headwork")),
        "{report:?}"
    );
    assert!(
        !w.host
            .calls()
            .iter()
            .any(|c| c.starts_with(&format!("send {surface} "))),
        "it is not asked for a recap it cannot write: {:?}",
        w.host.calls()
    );
    assert_eq!(w.events_of("orchestrator-unmanaged"), 1, "logged");

    let claude_dir = claude_dir_with(&w, &[&sand_session, &orchestrator]);
    let env = env!(w);
    let opts = ResumeOptions {
        logon: false,
        wait_for_host: Duration::ZERO,
        claude_dir,
    };
    assert!(
        would_close(&w.store, &env, &surface).unwrap(),
        "a resume started in its surface must detach"
    );
    let report = resume(&mut w.store, &env, &opts).unwrap();
    assert!(
        matches!(report.orchestrator, Some(TaskOutcome::Fresh { .. })),
        "{report:?}"
    );
    let line = panes_of(&w)
        .into_iter()
        .find(|c| c.contains(" orchestrator "))
        .unwrap();
    let fresh = fresh_orchestrator(&line, &orchestrator);
    assert!(
        w.host.calls().contains(&format!("close-surface {surface}")),
        "the copy outside headwork is closed: {:?}",
        w.host.calls()
    );
    assert!(
        w.store.session(&fresh).unwrap().is_some(),
        "the new session is recorded"
    );
    let summary = std::fs::read_to_string(&report.summary).unwrap();
    assert!(summary.contains("ran outside headwork"), "{summary}");
    assert!(
        summary.contains("## Tasks")
            && summary.contains("demo-sand")
            && summary.contains("## Recent log"),
        "a mechanical orchestrator recap built from the store: {summary}"
    );
}

#[test]
fn a_resume_without_a_park_brings_an_orchestrator_the_host_restored_back_under_headwork() {
    let mut w = world("never");
    let (orchestrator, surface) = w.unmanaged_orchestrator();
    let other = w.seen_in("Workspace 2", "someone-else");
    let claude_dir = claude_dir_with(&w, &[&orchestrator]);
    let env = env!(w);
    assert!(
        would_close(&w.store, &env, &surface).unwrap(),
        "its own surface"
    );
    assert!(
        !would_close(&w.store, &env, &other).unwrap(),
        "another surface"
    );
    let opts = ResumeOptions {
        logon: true,
        wait_for_host: Duration::ZERO,
        claude_dir,
    };
    let report = resume(&mut w.store, &env, &opts).unwrap();
    assert!(
        matches!(report.orchestrator, Some(TaskOutcome::Fresh { .. })),
        "{report:?}"
    );
    assert!(
        w.host.calls().contains(&format!("close-surface {surface}")),
        "{:?}",
        w.host.calls()
    );
    assert_eq!(w.events_of("orchestrator-unmanaged"), 1, "logged");
    let line = panes_of(&w)
        .into_iter()
        .find(|c| c.contains(" orchestrator "))
        .unwrap();
    let fresh = fresh_orchestrator(&line, &orchestrator);
    let placed = w.store.session(&fresh).unwrap().unwrap().0;
    assert_eq!(
        placed.state,
        SessionState::Starting,
        "back under headwork: {placed:?}"
    );
}

const LEADS: &str = "\n[leads]\nenabled = true\n";

fn workspace_of(w: &World, task: &Task) -> String {
    w.store
        .session_of(task.id)
        .unwrap()
        .unwrap()
        .host_workspace
        .unwrap()
}

#[test]
fn resume_brings_each_lead_back_before_its_parts_and_the_parts_become_tabs_again() {
    let mut w = world_with("ask", LEADS);
    let (lead, lead_session, lead_pid) = w.working("plan");
    let (review, review_session, review_pid) = w.part(&lead, "review");
    let (render, render_session, render_pid) = w.part(&lead, "render");
    w.crash(&render, render_pid);
    w.crash(&review, review_pid);
    w.crash(&lead, lead_pid);
    let old = workspace_of(&w, &lead);
    w.host
        .close_workspace(&headwork_adapters::host::WorkspaceId(old.clone()))
        .unwrap();
    let claude_dir = claude_dir_with(&w, &[&lead_session, &review_session, &render_session]);
    let env = env!(w);
    let report = resume(
        &mut w.store,
        &env,
        &ResumeOptions {
            logon: false,
            wait_for_host: Duration::ZERO,
            claude_dir,
        },
    )
    .unwrap();
    let order: Vec<&str> = report.tasks.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        order,
        ["demo-plan", "demo-plan-review", "demo-plan-render"],
        "the lead first, then its parts in the order they were made: {report:?}"
    );
    assert!(
        report
            .tasks
            .iter()
            .all(|t| matches!(t.outcome, TaskOutcome::Fresh { .. })),
        "{report:?}"
    );
    let workspace = workspace_of(&w, &lead);
    assert_ne!(workspace, old, "the lead opened its workspace again");
    assert_eq!(workspace_of(&w, &review), workspace, "a tab of it");
    assert_eq!(workspace_of(&w, &render), workspace, "a tab of it");
    let created = w
        .host
        .calls()
        .iter()
        .filter(|c| c.starts_with("new-workspace "))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        created,
        ["new-workspace demo-plan", "new-workspace demo-plan"],
        "only the lead makes a workspace, at spawn and at resume"
    );
}

#[test]
fn a_part_whose_lead_does_not_come_back_waits_for_it() {
    let mut w = world_with("ask", LEADS);
    let (lead, lead_session, lead_pid) = w.working("plan");
    let (review, review_session, review_pid) = w.part(&lead, "review");
    w.crash(&review, review_pid);
    w.crash(&lead, lead_pid);
    std::fs::remove_dir_all(w.dir.path().join("demo-plan")).unwrap();
    let claude_dir = claude_dir_with(&w, &[&lead_session, &review_session]);
    let env = env!(w);
    let report = resume(
        &mut w.store,
        &env,
        &ResumeOptions {
            logon: false,
            wait_for_host: Duration::ZERO,
            claude_dir,
        },
    )
    .unwrap();
    assert!(
        matches!(report.tasks[0].outcome, TaskOutcome::Failed { .. }),
        "the lead's folder is gone: {report:?}"
    );
    assert!(
        matches!(&report.tasks[1].outcome, TaskOutcome::Skipped { reason } if reason.contains("demo-plan")),
        "{report:?}"
    );
    assert_eq!(w.state(&review), TaskState::Interrupted, "it waits");
}

/// A transcript that switched from `auto` to `plan` by shift+tab, then quoted a mode inside a tool result.
const PLAN_TRANSCRIPT: &str = concat!(
    r#"{"type":"permission-mode","permissionMode":"auto","sessionId":"s"}"#,
    "
",
    r#"{"type":"user","message":{"role":"user","content":"go"},"permissionMode":"auto"}"#,
    "
",
    r#"{"type":"permission-mode","permissionMode":"plan","sessionId":"s"}"#,
    "
",
    r#"{"type":"assistant","message":{"content":"\"permissionMode\":\"default\""}}"#,
    "
",
);

fn transcript(claude_dir: &Path, session: &str, text: &str) {
    std::fs::write(
        claude_dir
            .join("projects")
            .join("S--demo")
            .join(format!("{session}.jsonl")),
        text,
    )
    .unwrap();
}

fn launch_args(w: &World, task: &Task, mode: StartMode) -> Vec<String> {
    let task = w.store.task(task.id).unwrap();
    let session = w.store.session_of(task.id).unwrap().unwrap();
    claude_command(
        &w.home,
        &w.config,
        &task,
        &session,
        &w.dir.path().join("demo-sand"),
        Path::new(EXE),
        mode,
    )
    .unwrap()
    .get_args()
    .map(|a| a.to_string_lossy().into_owned())
    .collect()
}

fn mode_flag(args: &[String]) -> Option<&str> {
    args.windows(2)
        .find(|pair| pair[0] == "--permission-mode")
        .map(|pair| pair[1].as_str())
}

#[test]
fn the_last_top_level_mode_of_a_transcript_counts() {
    let cases: &[(&str, Option<PermissionMode>)] = &[
        (PLAN_TRANSCRIPT, Some(PermissionMode::Plan)),
        ("", None),
        (
            "{}
", None,
        ),
        (
            "not json \"permissionMode\"
",
            None,
        ),
        (
            concat!(
                r#"{"type":"permission-mode","permissionMode":"auto"}"#,
                "
",
                r#"{"type":"permission-mode","permissionMode":"bypassPermissions"}"#,
                "
"
            ),
            None,
        ),
        (
            concat!(
                r#"{"type":"permission-mode","permissionMode":"plan"}"#,
                "
",
                r#"{"type":"user","permissionMode":"acceptEdits"}"#,
                "
"
            ),
            Some(PermissionMode::AcceptEdits),
        ),
    ];
    for (text, expected) in cases {
        assert_eq!(last_mode(text), *expected, "{text}");
    }
}

#[test]
fn a_resumed_worker_starts_in_the_mode_its_transcript_last_records() {
    let mut w = world("ask");
    let (sand, session, pid) = w.working("sand");
    w.crash(&sand, pid);
    assert_eq!(
        mode_flag(&launch_args(&w, &sand, StartMode::New)),
        Some("auto"),
        "spawned in the task's mode"
    );
    let claude_dir = claude_dir_with(&w, &[&session]);
    transcript(&claude_dir, &session, PLAN_TRANSCRIPT);
    let env = env!(w);
    let opts = ResumeOptions {
        logon: false,
        wait_for_host: Duration::ZERO,
        claude_dir,
    };
    let report = resume(&mut w.store, &env, &opts).unwrap();
    assert!(
        matches!(report.tasks[0].outcome, TaskOutcome::Fresh { .. }),
        "{report:?}"
    );
    let row = w.store.session_of(sand.id).unwrap().unwrap();
    assert_ne!(row.id, session, "a new conversation");
    assert_eq!(row.permission_mode, Some(PermissionMode::Plan), "recorded");
    assert_eq!(
        mode_flag(&launch_args(&w, &sand, StartMode::Fresh { crash: true })),
        Some("plan"),
        "back in the mode it was left in"
    );
    assert_eq!(w.events_of("session-mode"), 1, "the change is logged");
}

#[test]
fn a_worker_whose_transcript_is_gone_comes_back_in_the_mode_its_hooks_reported() {
    let mut w = world("ask");
    let (sand, session, pid) = w.working("sand");
    w.store
        .set_session_mode(&session, PermissionMode::AcceptEdits, Actor::Hook)
        .unwrap();
    w.crash(&sand, pid);
    let env = env!(w);
    let opts = ResumeOptions {
        logon: false,
        wait_for_host: Duration::ZERO,
        claude_dir: claude_dir_with(&w, &[]),
    };
    let report = resume(&mut w.store, &env, &opts).unwrap();
    assert!(
        matches!(report.tasks[0].outcome, TaskOutcome::Fresh { .. }),
        "{report:?}"
    );
    let row = w.store.session_of(sand.id).unwrap().unwrap();
    assert_ne!(row.id, session, "a new conversation");
    assert_eq!(
        mode_flag(&launch_args(&w, &sand, StartMode::Fresh { crash: true })),
        Some("acceptEdits"),
        "the new conversation keeps the mode"
    );
}

#[test]
fn an_orchestrator_comes_back_in_the_mode_its_transcript_last_records() {
    let mut w = world("never");
    let (orchestrator, _) = w.unmanaged_orchestrator();
    let claude_dir = claude_dir_with(&w, &[&orchestrator]);
    transcript(
        &claude_dir,
        &orchestrator,
        concat!(
            r#"{"type":"permission-mode","permissionMode":"default"}"#,
            "
",
            r#"{"type":"permission-mode","permissionMode":"auto"}"#,
            "
"
        ),
    );
    let env = env!(w);
    let opts = ResumeOptions {
        logon: true,
        wait_for_host: Duration::ZERO,
        claude_dir,
    };
    resume(&mut w.store, &env, &opts).unwrap();
    let line = panes_of(&w)
        .into_iter()
        .find(|c| c.contains(" orchestrator "))
        .unwrap();
    fresh_orchestrator(&line, &orchestrator);
    assert!(line.contains("--mode auto --summary"), "{line}");
}

#[test]
fn an_orchestrator_whose_transcript_is_gone_comes_back_in_the_mode_its_hooks_reported() {
    let mut w = world("never");
    let (orchestrator, _) = w.unmanaged_orchestrator();
    w.store
        .set_session_mode(&orchestrator, PermissionMode::Auto, Actor::Hook)
        .unwrap();
    let env = env!(w);
    let opts = ResumeOptions {
        logon: true,
        wait_for_host: Duration::ZERO,
        claude_dir: claude_dir_with(&w, &[]),
    };
    let report = resume(&mut w.store, &env, &opts).unwrap();
    assert!(
        matches!(report.orchestrator, Some(TaskOutcome::Fresh { .. })),
        "{report:?}"
    );
    let line = panes_of(&w)
        .into_iter()
        .find(|c| c.contains(" orchestrator "))
        .unwrap();
    assert!(line.contains("--mode auto --summary"), "{line}");
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
fn park_leaves_a_program_that_took_a_sessions_pid_over_alone() {
    let mut w = world("ask");
    let (sand, session, pid) = w.working("sand");
    record_reused(&mut w.store, &session, pid, pid);
    let env = env!(w);
    park(&mut w.store, &env, quick(200)).unwrap();
    assert_eq!(w.state(&sand), TaskState::Parked, "parked");
    assert!(
        is_alive(pid).unwrap(),
        "the program holding the pid runs on"
    );
    let reused = w
        .store
        .events(EventFilter {
            task: Some(sand.id),
            limit: 200,
        })
        .unwrap()
        .iter()
        .filter(|e| e.kind == "pid-reused")
        .count();
    assert!(reused >= 1, "logged");
}
