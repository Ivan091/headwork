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
use headwork::ops::launch::{
    OrchestratorOrigin, OrchestratorStart, StartMode, claude_command, first_prompt,
    orchestrator_prompt,
};
use headwork::ops::park::{OrchestratorEnd, ParkOptions, RecapSource, park};
use headwork::ops::recap::{mechanical, orchestrator_mechanical};
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
use headwork_store::{
    EVENT_TOOL, EventFilter, NewAsk, NewRecap, NewSession, NewTask, PrSeen, Store, Task,
};

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
         deliver = \"pr\"\nmerge = \"deliver\"\n\n[watch]\nexit_grace_s = 1\n{extra}"
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
        keep_orchestrator: false,
    }
}

const fn keeping(deadline_ms: u64) -> ParkOptions {
    ParkOptions {
        keep_orchestrator: true,
        ..quick(deadline_ms)
    }
}

fn orchestrator_panes(w: &World) -> usize {
    panes_of(w)
        .iter()
        .filter(|c| c.contains(" orchestrator "))
        .count()
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
        gate: None,
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
        calls
            .iter()
            .filter(|c| c.starts_with("send ") && !c.ends_with(" /exit"))
            .count(),
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
        matches!(&outcome("demo-mist").outcome, TaskOutcome::Fresh { surface, .. } if !surface.is_empty()),
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
        let pid = self.sleeper();
        let (id, dir) = self.orchestrator_as(pid);
        (id, pid, dir)
    }

    /// An orchestrator session whose launcher and `claude` are recorded as `pid`.
    fn orchestrator_as(&mut self, pid: u32) -> (String, PathBuf) {
        let id = format!("orchestrator-{}-{pid}", self.children.len());
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
        self.store.session_launched(&id, pid, pid, "job").unwrap();
        let dir = self.dir.path().join("code");
        std::fs::create_dir_all(&dir).unwrap();
        self.store
            .set_session_dir(&id, &dir.display().to_string().replace('\\', "/"))
            .unwrap();
        (id, dir)
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

impl World {
    /// The orchestrator in `surface` was asked to exit twice after every worker's pane closed, then its processes
    /// ended, its pane closed and its session recorded over.
    fn assert_closed_last(&self, surface: &str, orchestrator: &str, pid: u32) {
        let calls = self.host.calls();
        let exit = calls
            .iter()
            .position(|c| c == &format!("send {surface} /exit"));
        let worker_closed = calls
            .iter()
            .rposition(|c| c.starts_with("close-") && c.split(' ').nth(1) != Some(surface));
        assert!(
            exit.is_some() && worker_closed.is_some() && exit > worker_closed,
            "/exit is typed into its pane after the workers' panes close: {calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|c| c == &format!("send-key {surface} Escape")),
            "the second ask interrupts: {calls:?}"
        );
        assert!(
            calls.contains(&format!("close-surface {surface}")),
            "its pane is closed: {calls:?}"
        );
        assert_eq!(
            self.session_events_of(orchestrator, "exit-asked"),
            3,
            "the own ladder: /exit, then after Escape, then Ctrl+C twice"
        );
        assert_eq!(self.events_of("orchestrator-closed"), 1, "logged");
        assert!(dead(pid), "the park ends the orchestrator");
        assert_eq!(
            self.store.session(orchestrator).unwrap().unwrap().0.state,
            SessionState::Ended,
            "its session is over"
        );
    }
}

#[test]
fn park_asks_the_root_for_a_recap_closes_it_last_and_resume_brings_exactly_one_back() {
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
    assert_eq!(
        parked_root.end,
        OrchestratorEnd::Ended,
        "the fake pane ignores /exit, so its processes are ended: {report:?}"
    );
    w.assert_closed_last(&surface, &orchestrator, root_pid);
    assert_eq!(w.state(&sand), TaskState::Parked, "the worker is parked");
    assert!(report.safe_to_restart, "{report:?}");
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
    assert_eq!(orchestrator_panes(&w), 1, "exactly one orchestrator opens");
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
    let report = park(&mut w.store, &env, keeping(200)).unwrap();
    let parked_root = report.orchestrator.clone().unwrap();
    assert_eq!(parked_root.end, OrchestratorEnd::Kept, "{report:?}");
    assert!(
        is_alive(root_pid).unwrap(),
        "--keep-orchestrator leaves it running"
    );
    assert_eq!(w.events_of("orchestrator-kept"), 1, "logged");
    assert_eq!(w.events_of("exit-asked"), 0, "not asked to exit");
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
    assert_eq!(orchestrator_panes(&w), 1, "exactly one orchestrator opens");
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
    let report = park(&mut w.store, &env, keeping(5_000)).unwrap();
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
            .any(|c| c.starts_with(&format!("send {surface} ")) && c.contains("recap")),
        "it is not asked for a recap it cannot write: {:?}",
        w.host.calls()
    );
    assert_eq!(w.events_of("orchestrator-unmanaged"), 1, "logged");
    assert_eq!(
        parked.end,
        OrchestratorEnd::Ended,
        "its pane is closed: {report:?}"
    );
    assert!(
        w.host.calls().contains(&format!("send {surface} /exit"))
            && w.host.calls().contains(&format!("close-surface {surface}")),
        "asked to exit, then its pane closed: {:?}",
        w.host.calls()
    );
    let surface = w.seen_in("Workspace 1", &orchestrator);

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
    assert_eq!(orchestrator_panes(&w), 1, "exactly one orchestrator opens");
    assert!(
        w.host.calls().contains(&format!("close-surface {surface}")),
        "the copy the host restored after the restart is closed: {:?}",
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

fn plain_task(w: &mut World, slug: &str) -> Task {
    let new = NewTask {
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
        brief_path: w.brief.display().to_string(),
        depends_on: None,
        permission_mode: PermissionMode::Auto,
        shape: TaskShape::Plain,
    };
    w.store.create_task(&w.config, &new, Actor::User).unwrap()
}

fn add_recap(w: &mut World, task: &Task, kind: RecapKind, body: &str) {
    w.store
        .add_recap(
            &NewRecap {
                task_id: Some(task.id),
                session_id: None,
                kind,
                body,
            },
            Actor::Worker,
        )
        .unwrap();
}

fn resume_by_hand(w: &mut World, sessions: &[&str]) {
    let opts = ResumeOptions {
        logon: false,
        wait_for_host: Duration::ZERO,
        claude_dir: claude_dir_with(w, sessions),
    };
    let env = env!(w);
    resume(&mut w.store, &env, &opts).unwrap();
}

#[test]
fn a_resume_keeps_a_handoff_a_park_wrote() {
    let mut w = world("ask");
    let (sand, session, _) = w.working("sand");
    let env = env!(w);
    park(&mut w.store, &env, quick(200)).unwrap();
    let path = w.home.handoff_path(&sand.name);
    std::fs::write(&path, "KEPT AS WRITTEN\n").unwrap();
    resume_by_hand(&mut w, &[&session]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "KEPT AS WRITTEN\n");
}

#[test]
fn a_resume_builds_a_missing_handoff_from_the_sessions_own_recap_and_the_facts() {
    let mut w = world("ask");
    let (sand, session, _) = w.working("sand");
    let env = env!(w);
    park(&mut w.store, &env, quick(200)).unwrap();
    let path = w.home.handoff_path(&sand.name);
    std::fs::remove_file(&path).unwrap();
    add_recap(
        &mut w,
        &sand,
        RecapKind::Session,
        "Rendering stage 3; next: compare.",
    );
    resume_by_hand(&mut w, &[&session]);
    let handoff = std::fs::read_to_string(&path).unwrap();
    let own = handoff.find("# Your own recap").unwrap();
    let body = handoff.find("Rendering stage 3; next: compare.").unwrap();
    let facts = handoff.find("resumed with no recap on file").unwrap();
    assert!(own < body && body < facts, "{handoff}");
    assert!(
        handoff.contains("- branch: ") && handoff.contains("- last report: "),
        "{handoff}"
    );
}

#[test]
fn after_a_crash_the_handoff_holds_the_session_recap_even_when_an_interrupted_one_is_newer() {
    let mut w = world("ask");
    let (sand, session, pid) = w.working("sand");
    add_recap(
        &mut w,
        &sand,
        RecapKind::Session,
        "SESSION BODY: next is the render.",
    );
    add_recap(
        &mut w,
        &sand,
        RecapKind::Interrupted,
        "INTERRUPTED BODY mechanical",
    );
    w.crash(&sand, pid);
    resume_by_hand(&mut w, &[&session]);
    let handoff = std::fs::read_to_string(w.home.handoff_path(&sand.name)).unwrap();
    assert!(handoff.contains("# Your own recap"), "{handoff}");
    assert!(
        handoff.contains("SESSION BODY: next is the render."),
        "{handoff}"
    );
    assert!(handoff.contains("resumed after a crash"), "{handoff}");
}

#[test]
fn the_mechanical_recap_lists_prs_parts_and_jobs_and_leaves_tool_events_out_of_the_log() {
    let mut w = world_with("ask", LEADS);
    let (lead, session, _) = w.working("lead");
    let (part, _, _) = w.part(&lead, "p");
    w.store
        .record_pr(
            lead.id,
            &PrSeen {
                repo: "o/r",
                number: 7,
                url: "https://github.com/o/r/pull/7",
                state: "open",
            },
            Actor::Tick,
        )
        .unwrap();
    let checkpoint = w.dir.path().join("cp.json");
    std::fs::write(&checkpoint, "{}").unwrap();
    let pid = w.sleeper();
    let mut registration = job("train", ParkPolicy::Checkpoint, pid, Some(&checkpoint));
    registration.restart = Some(false);
    registration.cwd = Some("S:/work/dir".to_owned());
    let env = env!(w);
    register(&mut w.store, &env, &lead, &session, &registration).unwrap();
    w.store
        .append_event(
            Actor::Hook,
            Some(lead.id),
            None,
            "marker-event",
            &serde_json::json!({}),
        )
        .unwrap();
    w.store
        .append_event(
            Actor::Hook,
            Some(lead.id),
            None,
            "long-event",
            &serde_json::json!({ "note": "y".repeat(400) }),
        )
        .unwrap();
    for _ in 0..60 {
        w.store
            .append_event(
                Actor::Hook,
                Some(lead.id),
                None,
                EVENT_TOOL,
                &serde_json::json!({ "tool": "Bash" }),
            )
            .unwrap();
    }
    let lead = w.store.task(lead.id).unwrap();
    let text = mechanical(&w.store, &env, &lead, "testing").unwrap();
    assert!(text.starts_with("# Recap of demo-lead (testing)"), "{text}");
    assert!(
        text.contains("- pull request https://github.com/o/r/pull/7 (open)"),
        "{text}"
    );
    assert!(text.contains(&format!("- part {} (", part.name)), "{text}");
    assert!(text.contains("- job `train` (checkpoint, "), "{text}");
    assert!(text.contains("restart no)"), "{text}");
    assert!(text.contains("--exact helper_sleeper --ignored`"), "{text}");
    assert!(text.contains("long-event"), "{text}");
    assert!(text.contains(&format!("{}...", "y".repeat(100))), "{text}");
    assert!(!text.contains(&"y".repeat(200)), "{text}");
    assert!(
        text.contains(&format!(", checkpoint {}", checkpoint.display())),
        "{text}"
    );
    assert!(text.contains(" in S:/work/dir"), "{text}");
    assert!(
        text.contains("marker-event"),
        "the limit counts events returned, not tool events: {text}"
    );
    assert!(!text.contains(EVENT_TOOL), "{text}");
}

#[test]
fn the_orchestrators_mechanical_recap_says_none_for_an_empty_board_and_lists_each_section() {
    let mut w = world("ask");
    let empty = orchestrator_mechanical(&w.store, "r").unwrap();
    assert!(empty.contains("## Tasks\n\nnone\n"), "{empty}");
    assert!(
        !empty.contains("## Queue") && !empty.contains("## Asks without a task"),
        "{empty}"
    );
    assert!(
        !empty.contains("## Rules") && !empty.contains("## Registered jobs"),
        "{empty}"
    );

    let (sand, session, _) = w.working("sand");
    let queued = plain_task(&mut w, "later");
    assert_eq!(queued.state, TaskState::Queued);
    w.store
        .open_ask(
            &NewAsk {
                task_id: None,
                session_id: None,
                kind: AskKind::Question,
                question: "Which video next?".to_owned(),
                options: vec!["A".to_owned(), "B".to_owned()],
            },
            Actor::Orchestrator,
        )
        .unwrap();
    w.store
        .add_rule("worker", "never touch the render queue", Actor::User)
        .unwrap();
    let pid = w.sleeper();
    let env = env!(w);
    register(
        &mut w.store,
        &env,
        &sand,
        &session,
        &job("train", ParkPolicy::Rerun, pid, None),
    )
    .unwrap();
    let text = orchestrator_mechanical(&w.store, "r").unwrap();
    assert!(
        text.starts_with("# Recap of the orchestrator (r)"),
        "{text}"
    );
    assert!(text.contains("- demo-sand (task, "), "{text}");
    assert!(
        text.contains("## Queue, in start order\n\n- demo-later\n"),
        "{text}"
    );
    assert!(
        text.contains("## Asks without a task\n\n- Which video next? (options: A | B)"),
        "{text}"
    );
    assert!(
        text.contains("(still active)\n\n- [worker] never touch the render queue"),
        "{text}"
    );
    assert!(
        text.contains("## Registered jobs not over\n\n- `train` (rerun, "),
        "{text}"
    );
    assert!(text.contains("## Recent log, oldest first"), "{text}");
}

#[test]
fn fresh_prompts_tell_the_session_to_ask_its_open_question_again() {
    let mut w = world("ask");
    let task = plain_task(&mut w, "sand");
    for crash in [false, true] {
        let prompt = first_prompt(&w.home, &task, StartMode::Fresh { crash });
        assert!(
            prompt.contains("ask it again with AskUserQuestion"),
            "{prompt}"
        );
        assert!(
            prompt.contains("as a new session so your context is fresh"),
            "{prompt}"
        );
        let cause = if crash { "crash" } else { "restart" };
        assert!(prompt.contains(&format!("after a {cause}")), "{prompt}");
        assert!(prompt.contains("handoffs"), "{prompt}");
    }
    let start = |resume: bool| OrchestratorStart {
        name: "orchestrator".to_owned(),
        agent: None,
        prompt: "NEW PROMPT".to_owned(),
        model: None,
        effort: None,
        mode: PermissionMode::Auto,
        origin: OrchestratorOrigin::Back {
            session: "s".to_owned(),
            resume,
            summary: Some("S:/summary.md".to_owned()),
        },
    };
    let fresh = orchestrator_prompt(&start(false));
    assert!(
        fresh.contains("started again after a restart as a new session so your context is fresh"),
        "{fresh}"
    );
    assert!(
        fresh.contains("Read HANDOFF.md beside CLAUDE.md."),
        "{fresh}"
    );
    assert!(fresh.contains("S:/summary.md"), "{fresh}");
    assert!(
        fresh.contains("ask it again with AskUserQuestion"),
        "{fresh}"
    );
    let resumed = orchestrator_prompt(&start(true));
    assert!(resumed.starts_with("Resumed after a restart."), "{resumed}");
    assert!(
        resumed.contains("ask it again with AskUserQuestion"),
        "{resumed}"
    );
}

#[test]
fn an_orchestrator_resume_ends_the_old_session_and_the_new_one_starts_in_its_mode() {
    let mut w = world("never");
    let (old, _) = w.unmanaged_orchestrator();
    w.store
        .set_session_mode(&old, PermissionMode::Plan, Actor::Hook)
        .unwrap();
    let env = env!(w);
    let opts = ResumeOptions {
        logon: true,
        wait_for_host: Duration::ZERO,
        claude_dir: claude_dir_with(&w, &[&old]),
    };
    resume(&mut w.store, &env, &opts).unwrap();
    assert_eq!(
        w.store.session(&old).unwrap().unwrap().0.state,
        SessionState::Ended
    );
    let line = panes_of(&w)
        .into_iter()
        .find(|c| c.contains(" orchestrator "))
        .unwrap();
    let new = fresh_orchestrator(&line, &old);
    let (row, task) = w.store.session(&new).unwrap().unwrap();
    assert_eq!(task, None);
    assert_eq!(row.rc_name, "orchestrator");
    assert!(line.contains("--mode plan --summary"), "{line}");
}

#[test]
fn a_gated_job_takes_a_free_slot_for_its_pid_and_reads_as_waiting_while_its_waiter_file_lives() {
    use headwork::ops::jobs::{claim_gate, statuses};
    use headwork_core::gate::{GateClass, Owner};

    let mut w = world("ask");
    let slots = w.dir.path().join("slots");
    w.config.gates.dir = Some(slots.display().to_string().replace('\\', "/"));
    let (sand, session, _) = w.working("sand");
    let pid = w.sleeper();
    let mut registration = job("train", ParkPolicy::Rerun, pid, None);
    registration.gate = Some(GateClass::Gpu);
    let env = env!(w);
    let row = register(&mut w.store, &env, &sand, &session, &registration).unwrap();
    assert_eq!(row.gate, Some(GateClass::Gpu), "stored as the gate");
    assert!(row.heavy, "the older flag still reads true");

    assert_eq!(claim_gate(&env, &sand, &row), None, "a free slot is taken");
    let owner = std::fs::read_to_string(slots.join("gpu").join("slot-1").join("owner")).unwrap();
    assert!(
        owner.contains(&format!("pid={pid}")) && owner.contains("native=1"),
        "{owner}"
    );
    assert_eq!(
        claim_gate(&env, &sand, &row),
        None,
        "the pid holds one already"
    );
    assert!(!slots.join("gpu").join("slot-2").exists(), "no second slot");

    let waiting = Owner {
        pid,
        native: true,
        class: Some(GateClass::Gpu),
        task: "demo-sand".to_owned(),
        command: "train".to_owned(),
        epoch: Some(headwork_adapters::gate::epoch_now()),
        ..Owner::default()
    };
    let listed = statuses(&w.home, &w.config, vec![row.clone()]);
    assert_eq!(listed[0].waiting_for, None, "no waiter file yet");
    std::fs::write(
        slots.join("gpu").join(format!("wait-{pid}")),
        waiting.render(),
    )
    .unwrap();
    let listed = statuses(&w.home, &w.config, vec![row]);
    assert_eq!(listed[0].waiting_for, Some(GateClass::Gpu), "waiter file");
    let json = serde_json::to_value(&listed[0]).unwrap();
    assert_eq!(json["gate"], "gpu", "{json}");
    assert_eq!(json["waiting_for"], "gpu", "{json}");
}

#[test]
fn a_job_over_a_full_gate_gets_a_warning() {
    use headwork::ops::jobs::claim_gate;
    use headwork_core::gate::GateClass;

    let mut w = world("ask");
    let slots = w.dir.path().join("slots");
    w.config.gates.dir = Some(slots.display().to_string().replace('\\', "/"));
    let (sand, session, _) = w.working("sand");
    let pids: Vec<u32> = (0..4).map(|_| w.sleeper()).collect();
    let env = env!(w);
    for (label, pid) in ["a", "b", "c"].into_iter().zip(&pids) {
        let mut registration = job(label, ParkPolicy::Rerun, *pid, None);
        registration.gate = Some(GateClass::Cpu);
        let row = register(&mut w.store, &env, &sand, &session, &registration).unwrap();
        assert_eq!(claim_gate(&env, &sand, &row), None, "{label}");
    }
    let mut registration = job("d", ParkPolicy::Rerun, pids[3], None);
    registration.gate = Some(GateClass::Cpu);
    let row = register(&mut w.store, &env, &sand, &session, &registration).unwrap();
    let warning = claim_gate(&env, &sand, &row).unwrap();
    assert!(warning.contains("runs over the cpu gate"), "{warning}");
}

#[test]
fn the_board_line_and_status_show_gate_holders_and_waiting_tasks() {
    use headwork::ops::gate::{board_line, waiting_tasks};
    use headwork::ops::status::status_waiting;
    use headwork_core::gate::{GateClass, Owner};

    let mut w = world_with("ask", LEADS);
    let slots = w.dir.path().join("slots");
    w.config.gates.dir = Some(slots.display().to_string().replace('\\', "/"));
    assert_eq!(
        board_line(&w.home, &w.config.gates),
        None,
        "no folder, no line"
    );
    assert!(!slots.exists(), "reading never creates it");

    let (lead, _, _) = w.working("plan");
    let (part, _, _) = w.part(&lead, "render");
    let (other, _, _) = w.working("other");
    let owner = |task: &Task, command: &str| Owner {
        pid: std::process::id(),
        native: true,
        class: Some(GateClass::Cpu),
        task: task.name.to_string(),
        task_id: Some(task.id.0),
        worktree: "S:/Code/demo".to_owned(),
        command: command.to_owned(),
        started: "2026-11-26 10:00:00".to_owned(),
        epoch: None,
    };
    let write_aged = |path: &Path, owner: &Owner, age_s: u64| {
        std::fs::write(path, owner.render()).unwrap();
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(age_s))
            .unwrap();
    };
    let slot = slots.join("cpu").join("slot-1");
    std::fs::create_dir_all(&slot).unwrap();
    write_aged(&slot.join("owner"), &owner(&other, "cargo test"), 240);
    write_aged(
        &slots
            .join("cpu")
            .join(format!("wait-{}", std::process::id())),
        &owner(&part, "bash scripts/check.sh"),
        30,
    );

    let line = board_line(&w.home, &w.config.gates).unwrap();
    assert_eq!(
        line,
        "Gates: cpu 1/3 (demo-other cargo test 4m) \u{b7} 1 waiting \u{b7} gpu 0/2\n"
    );
    let waiting = waiting_tasks(&w.home, &w.config.gates);
    assert_eq!(waiting, vec![(part.id.0, GateClass::Cpu)]);
    let lines = status_waiting(
        &w.store,
        false,
        &headwork_core::usage::UsageConfig::default(),
        &waiting,
    )
    .unwrap();
    let find = |name: &str| lines.iter().find(|line| line.task == name).unwrap();
    assert!(
        find("demo-plan").line.ends_with("waiting for cpu"),
        "a lead rolls up its part: {}",
        find("demo-plan").line
    );
    assert_eq!(find("demo-plan").waiting_for, Some(GateClass::Cpu));
    assert!(
        !find("demo-other").line.contains("waiting for"),
        "{}",
        find("demo-other").line
    );
}

#[test]
fn a_gated_job_whose_pid_already_waits_takes_no_slot() {
    use headwork::ops::jobs::claim_gate;
    use headwork_core::gate::{GateClass, Owner};

    let mut w = world("ask");
    let slots = w.dir.path().join("slots");
    w.config.gates.dir = Some(slots.display().to_string().replace('\\', "/"));
    let (sand, session, _) = w.working("sand");
    let pid = w.sleeper();
    let mut registration = job("render", ParkPolicy::Rerun, pid, None);
    registration.gate = Some(GateClass::Cpu);
    let env = env!(w);
    let row = register(&mut w.store, &env, &sand, &session, &registration).unwrap();
    let waiting = Owner {
        pid,
        native: true,
        class: Some(GateClass::Cpu),
        command: "render".to_owned(),
        epoch: Some(headwork_adapters::gate::epoch_now()),
        ..Owner::default()
    };
    std::fs::create_dir_all(slots.join("cpu")).unwrap();
    std::fs::write(
        slots.join("cpu").join(format!("wait-{pid}")),
        waiting.render(),
    )
    .unwrap();
    assert_eq!(
        claim_gate(&env, &sand, &row),
        None,
        "its gate takes a slot in turn"
    );
    assert!(!slots.join("cpu").join("slot-1").exists(), "no slot taken");
}

fn detached_argv(w: &World) -> Vec<String> {
    let events = w
        .store
        .events(EventFilter {
            task: None,
            limit: 500,
        })
        .unwrap();
    let event = events.iter().find(|e| e.kind == "detached").unwrap();
    event.data["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_park_running_under_the_orchestrator_hands_its_close_to_a_detached_closer() {
    let mut w = world("ask");
    let own = std::process::id();
    let (orchestrator, _) = w.orchestrator_as(own);
    let surface = w.seen_in("Workspace 1", &orchestrator);
    let exe = std::env::current_exe().unwrap();
    let env = Env {
        home: &w.home,
        config: &w.config,
        host: &w.host,
        exe: &exe,
        actor: Actor::User,
    };
    let report = park(&mut w.store, &env, quick(200)).unwrap();
    let parked = report.orchestrator.clone().unwrap();
    assert!(
        matches!(parked.end, OrchestratorEnd::Detached { .. }),
        "a park under the orchestrator's pid cannot end it itself: {report:?}"
    );
    let home = w.home.root().display().to_string();
    assert_eq!(
        detached_argv(&w),
        [
            "park",
            "--close-orchestrator",
            orchestrator.as_str(),
            "--after",
            own.to_string().as_str(),
            "--home",
            home.as_str(),
        ],
        "the closer waits for this park, in this home"
    );
    assert!(
        !w.host.calls().contains(&format!("send {surface} /exit")),
        "the park itself types nothing: {:?}",
        w.host.calls()
    );
    assert_ne!(
        w.store.session(&orchestrator).unwrap().unwrap().0.state,
        SessionState::Ended,
        "the closer ends it later"
    );
}

#[test]
fn a_closer_that_does_not_start_is_reported_and_the_orchestrator_is_left() {
    let mut w = world("ask");
    let (orchestrator, _) = w.orchestrator_as(std::process::id());
    let env = env!(w);
    let report = park(&mut w.store, &env, quick(200)).unwrap();
    let parked = report.orchestrator.clone().unwrap();
    assert!(
        matches!(&parked.end, OrchestratorEnd::Failed { problem } if problem.contains("did not start")),
        "{report:?}"
    );
    assert_eq!(w.events_of("orchestrator-closed"), 1, "logged");
    assert!(
        report.safe_to_restart,
        "the restart ends it anyway: {report:?}"
    );
    assert_ne!(
        w.store.session(&orchestrator).unwrap().unwrap().0.state,
        SessionState::Ended,
        "left running"
    );
}

#[test]
fn the_closer_waits_for_the_park_to_exit_before_it_closes_the_orchestrator() {
    let mut w = world("ask");
    let (orchestrator, root_pid, _) = w.orchestrator();
    let surface = w.seen_in("Workspace 1", &orchestrator);
    let park_pid = w.sleeper();
    let started = Instant::now();
    let killer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1_500));
        headwork_adapters::proc::kill_tree(park_pid).unwrap()
    });
    let env = env!(w);
    let end =
        headwork::ops::park::close_orchestrator_after(&mut w.store, &env, &orchestrator, park_pid)
            .unwrap();
    assert!(killer.join().unwrap(), "the park stand-in ran until killed");
    assert!(
        started.elapsed() >= Duration::from_millis(1_400),
        "nothing is closed while the park runs: {:?}",
        started.elapsed()
    );
    assert_eq!(end, OrchestratorEnd::Ended, "the fake pane ignores /exit");
    assert!(dead(root_pid), "then the orchestrator is ended");
    assert!(
        w.host.calls().contains(&format!("close-surface {surface}")),
        "and its pane closed: {:?}",
        w.host.calls()
    );
}

impl World {
    fn session_events_of(&self, session: &str, kind: &str) -> usize {
        self.store
            .events(EventFilter {
                task: None,
                limit: 500,
            })
            .unwrap()
            .iter()
            .filter(|event| event.kind == kind && event.session_id.as_deref() == Some(session))
            .count()
    }

    fn surface_of(&self, task: &Task) -> String {
        self.store
            .session_of(task.id)
            .unwrap()
            .unwrap()
            .host_surface
            .unwrap()
    }
}

#[test]
fn park_climbs_the_exit_ladder_for_every_worker_in_one_pass_before_it_closes_their_panes() {
    let mut w = world("ask");
    w.config.watch.exit_grace_s = 3;
    let (sand, sand_session, sand_pid) = w.working("sand");
    let (mist, mist_session, mist_pid) = w.working("mist");
    let (sand_surface, mist_surface) = (w.surface_of(&sand), w.surface_of(&mist));
    let env = env!(w);
    let start = Instant::now();
    let report = park(&mut w.store, &env, quick(200)).unwrap();
    let took = start.elapsed();
    assert!(report.safe_to_restart, "{report:?}");
    let calls = w.host.calls();
    let ladder_starts = calls.iter().position(|c| c.ends_with(" Escape")).unwrap();
    let rest = calls.get(ladder_starts..).unwrap();
    let ladder = [
        "Escape",
        "Ctrl('u')",
        "/exit",
        "Enter",
        "Escape",
        "Ctrl('u')",
        "/exit",
        "Enter",
        "Ctrl('c')",
        "Ctrl('c')",
    ];
    for surface in [&sand_surface, &mist_surface] {
        let typed: Vec<&str> = rest
            .iter()
            .filter_map(|c| {
                c.strip_prefix(&format!("send-key {surface} "))
                    .or_else(|| c.strip_prefix(&format!("send {surface} ")))
            })
            .collect();
        assert_eq!(typed, ladder, "{surface}: the outside ladder: {calls:?}");
    }
    let at = |call: &str| rest.iter().position(|c| c == call).unwrap();
    assert!(
        at(&format!("send-key {mist_surface} Escape")) < at(&format!("send {sand_surface} /exit")),
        "each input goes into every pane before the next: {calls:?}"
    );
    let last_ctrl_c = rest
        .iter()
        .rposition(|c| c.ends_with(" Ctrl('c')"))
        .unwrap();
    let first_end = rest
        .iter()
        .position(|c| c.starts_with("close-") || c.starts_with("kill "))
        .unwrap();
    assert!(
        last_ctrl_c < first_end,
        "panes close and programs end only after the ladder: {calls:?}"
    );
    for session in [&sand_session, &mist_session] {
        assert_eq!(
            w.session_events_of(session, "exit-asked"),
            3,
            "{session}: every step"
        );
    }
    assert!(
        dead(sand_pid) && dead(mist_pid),
        "both ended after the ladder"
    );
    assert!(
        took < Duration::from_secs(18),
        "one ladder of three 3 s graces for both, not one each: {took:?}"
    );
}

fn restored_pane_closed(w: &World) -> Vec<serde_json::Value> {
    w.store
        .events(EventFilter {
            task: None,
            limit: 500,
        })
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "restored-pane-closed")
        .map(|event| event.data)
        .collect()
}

fn typed_into(w: &World, surface: &str) -> Vec<String> {
    w.host
        .calls()
        .into_iter()
        .filter_map(|call| {
            call.strip_prefix(&format!("send-key {surface} "))
                .or_else(|| call.strip_prefix(&format!("send {surface} ")))
                .map(str::to_owned)
        })
        .collect()
}

fn resume_restored(w: &mut World, orchestrator: &str) {
    let claude_dir = claude_dir_with(w, &[orchestrator]);
    let env = env!(w);
    let opts = ResumeOptions {
        logon: true,
        wait_for_host: Duration::ZERO,
        claude_dir,
    };
    resume(&mut w.store, &env, &opts).unwrap();
}

#[test]
fn a_restored_pane_whose_session_leaves_on_exit_is_closed_after_one_step() {
    let mut w = world("never");
    let (orchestrator, surface) = w.unmanaged_orchestrator();
    w.host
        .leave_on_exit(&headwork_adapters::host::SurfaceId(surface.clone()));
    resume_restored(&mut w, &orchestrator);
    assert_eq!(
        typed_into(&w, &surface),
        ["Escape", "Ctrl('u')", "/exit", "Enter"],
        "one step: the host stopped showing the session after /exit"
    );
    let closed = restored_pane_closed(&w);
    assert_eq!(closed.len(), 1, "{closed:?}");
    assert_eq!(closed[0]["exited"], true, "{closed:?}");
    assert!(
        w.host.calls().contains(&format!("close-surface {surface}")),
        "{:?}",
        w.host.calls()
    );
}

#[test]
fn a_restored_pane_whose_host_stops_answering_gets_the_whole_ladder_then_closes() {
    let mut w = world("never");
    let (orchestrator, surface) = w.unmanaged_orchestrator();
    w.host.fail_states_once_typed();
    resume_restored(&mut w, &orchestrator);
    assert_eq!(
        typed_into(&w, &surface),
        [
            "Escape",
            "Ctrl('u')",
            "/exit",
            "Enter",
            "Escape",
            "Ctrl('u')",
            "/exit",
            "Enter",
            "Ctrl('c')",
            "Ctrl('c')"
        ],
        "a host that does not answer counts as still showing the session"
    );
    let closed = restored_pane_closed(&w);
    assert_eq!(closed.len(), 1, "{closed:?}");
    assert_eq!(closed[0]["exited"], false, "{closed:?}");
    let calls = w.host.calls();
    let last_key = calls
        .iter()
        .rposition(|c| c == &format!("send-key {surface} Ctrl('c')"))
        .unwrap();
    let close = calls
        .iter()
        .position(|c| c == &format!("close-surface {surface}"))
        .unwrap();
    assert!(
        last_key < close,
        "the pane closes after the ladder: {calls:?}"
    );
}

#[test]
fn the_kanban_board_rolls_a_lead_with_an_asking_part_into_needs_you_and_shows_its_gate_wait() {
    use headwork::ops::kanban::kanban;
    use headwork_core::TaskStatus;
    use headwork_core::gate::GateClass;
    use headwork_core::kanban::Options;

    let mut w = world_with("ask", LEADS);
    let (lead, _, _) = w.working("plan");
    let (part, part_session, _) = w.part(&lead, "render");
    w.store
        .open_ask(
            &NewAsk {
                task_id: Some(part.id),
                session_id: Some(part_session),
                kind: AskKind::Question,
                question: "Ship it?".to_owned(),
                options: vec!["Ship".to_owned(), "Hold".to_owned()],
            },
            Actor::Hook,
        )
        .unwrap();
    w.store
        .transition(part.id, &Trigger::AskOpened, Actor::Hook)
        .unwrap();
    let board = kanban(
        &w.store,
        &headwork_core::usage::UsageConfig::default(),
        &[(part.id.0, GateClass::Cpu)],
        &Options::default(),
    )
    .unwrap();
    let card = board
        .unlinked
        .iter()
        .find(|task| task.name == "demo-plan")
        .unwrap();
    assert_eq!(
        card.status,
        TaskStatus::NeedsYou,
        "the asking part lifts the lead"
    );
    assert_eq!(card.parts, 1, "one part counted");
    assert_eq!(
        card.asking_parts,
        vec!["demo-plan-render".to_owned()],
        "who asks"
    );
    assert_eq!(
        card.waiting_for.as_deref(),
        Some("cpu"),
        "the gate wait of the part"
    );
}
