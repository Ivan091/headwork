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
use headwork::ops::resume::{ResumeOptions, TaskOutcome, resume};
use headwork::ops::spawn::{SpawnRequest, spawn};
use headwork_adapters::host::fake::FakeHost;
use headwork_adapters::host::{SurfaceState, TerminalHost};
use headwork_adapters::proc::is_alive;
use headwork_core::config::ParkPolicy;
use headwork_core::lifecycle::SessionFacts;
use headwork_core::{
    Actor, AskKind, Config, JobState, MachineMode, Model, PermissionMode, ProjectName, RecapKind,
    Slug, TaskRequest, TaskState, Trigger,
};
use headwork_store::{NewAsk, NewRecap, Store, Task};

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
         deliver = \"pr\"\nmerge = \"deliver\"\n"
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
            },
            brief: &self.brief,
            depends_on: None,
            permission_mode: PermissionMode::Auto,
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
                Actor::Child,
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
                },
                brief: &w.brief,
                depends_on: None,
                permission_mode: PermissionMode::Auto,
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
    let env = env!(w);
    let resumed = resume(&mut w.store, &env, &opts).unwrap();
    assert_eq!(
        w.store.machine_mode().unwrap(),
        MachineMode::Normal,
        "machine"
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
        matches!(outcome("demo-sand").outcome, TaskOutcome::Resumed { .. }),
        "{resumed:?}"
    );
    assert!(
        matches!(&outcome("demo-mist").outcome, TaskOutcome::Fresh { reason, .. } if reason.contains("transcript")),
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
            .any(|c| c.contains("--resume") && c.contains(&format!("--task {}", sand.id))),
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
    assert_eq!(session.id, sand_session, "same conversation");
    let task = w.store.task(sand.id).unwrap();
    let command = claude_command(
        &w.home,
        &w.config,
        &task,
        &session,
        &w.dir.path().join("demo-sand"),
        Path::new(EXE),
        StartMode::Resumed { crash: false },
    )
    .unwrap();
    let args: Vec<String> = command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args.first().map(String::as_str),
        Some("--resume"),
        "{args:?}"
    );
    assert_eq!(args.get(1), Some(&sand_session), "{args:?}");
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
        "left for the root to ask"
    );
    let by_hand = ResumeOptions {
        logon: false,
        wait_for_host: Duration::ZERO,
        claude_dir,
    };
    let env = env!(w);
    let report = resume(&mut w.store, &env, &by_hand).unwrap();
    assert!(
        matches!(report.tasks[0].outcome, TaskOutcome::Resumed { .. }),
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
            .any(|c| c.contains("--resume") && c.contains("--crash")),
        "{:?}",
        panes_of(&w)
    );
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
