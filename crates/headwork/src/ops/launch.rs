//! `launch`: the process a pane runs (design section 3.1).
//!
//! It gives the session its identity through the environment, starts `claude` with the task's fixed session id,
//! name, model and permission mode inside a tree it owns, records both process ids, waits, and records the exit.
//! With the plugin configured (`hooks.plugin_dir`), its `SessionStart` hook reports the session as started; without
//! it, the launcher does so itself once `claude` runs.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use headwork_adapters::proc::{self, OwnedChild, Ownership};
use headwork_adapters::runtime::{
    ClaudeLaunch, ENV_EXE, ENV_HOME, ENV_ROLE, ENV_SESSION, ENV_TASK,
};
use headwork_core::lifecycle::{SessionFacts, Trigger};
use headwork_core::{Actor, Config, Role, TaskState};
use headwork_store::{SessionRow, Store, Task, TaskId};

use super::reload::{EVENT_BUILD, build_event};
use super::{Env, close};
use crate::home::Home;

/// How a session starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartMode {
    /// A new conversation with the brief.
    New,
    /// The earlier conversation continues after a park or a crash.
    Resumed {
        /// The session was cut off, not parked.
        crash: bool,
    },
    /// The earlier conversation is gone; a new one starts from the brief and the recap.
    Fresh {
        /// The session was cut off, not parked.
        crash: bool,
    },
}

/// The first prompt of a session.
#[must_use]
pub fn first_prompt(home: &Home, task: &Task, mode: StartMode) -> String {
    let handoff = super::slashed(&home.handoff_path(&task.name));
    let cause = |crash: bool| if crash { "crash" } else { "restart" };
    match mode {
        StartMode::New => format!(
            "You are the worker session {}. Read your brief at {} and carry it out.",
            task.name, task.brief_path
        ),
        StartMode::Resumed { crash } => format!(
            "Resumed after a {}. Read your recap at {handoff} and continue where it says; if it names a question \
             you had open, ask it again with AskUserQuestion.",
            cause(crash)
        ),
        StartMode::Fresh { crash } => format!(
            "You are the worker session {}. Your earlier conversation could not be restored after a {}. \
             Read your brief at {} and your recap at {handoff}, then continue where the recap says.",
            task.name,
            cause(crash),
            task.brief_path
        ),
    }
}

/// The `claude` command for a task's session, with its identity in the environment. `exe` is the `headwork` program
/// the plugin's hooks call; with `hooks.plugin_dir` set, the session loads that plugin.
///
/// # Errors
///
/// Fails when the project left the config or the task's folder is unknown.
pub fn claude_command(
    home: &Home,
    config: &Config,
    task: &Task,
    session: &SessionRow,
    dir: &Path,
    exe: &Path,
    mode: StartMode,
) -> Result<Command> {
    let project = config
        .project(task.project())
        .with_context(|| format!("project `{}` is not configured", task.project()))?;
    let name = task.name.to_string();
    let prompt = first_prompt(home, task, mode);
    let plugin_dir = plugin_dir(home, config);
    let argv = ClaudeLaunch {
        plugin_dir: plugin_dir.as_deref(),
        session_id: &session.id,
        resume: matches!(mode, StartMode::Resumed { .. }),
        name: &name,
        remote_control: project
            .remote_control
            .unwrap_or(config.machine.remote_control),
        model: Some(task.model),
        effort: task.effort,
        permission_mode: task.permission_mode,
        agent: None,
        prompt: &prompt,
    }
    .argv();
    let (program, rest) = argv.split_first().context("an empty argv")?;
    let mut command = Command::new(program);
    command
        .args(rest)
        .current_dir(dir)
        .env(ENV_SESSION, &session.id)
        .env(ENV_ROLE, Role::Worker.as_str())
        .env(ENV_TASK, task.id.to_string())
        .env(ENV_HOME, home.root())
        .env(ENV_EXE, exe);
    Ok(command)
}

/// The plugin folder sessions load, expanded, when the config names one.
#[must_use]
pub fn plugin_dir(home: &Home, config: &Config) -> Option<String> {
    config
        .hooks
        .plugin_dir
        .as_deref()
        .map(|dir| super::slashed(&home.expand(dir)))
}

/// Runs a task's session to its end and returns the exit code of `claude`.
///
/// # Errors
///
/// Fails when the store or config cannot be read; a `claude` that does not start moves the task to `failed` first.
pub fn launch(home: &Home, id: TaskId, mode: StartMode) -> Result<Option<i32>> {
    let config = home.load_config()?;
    let mut store = home.open_store()?;
    let task = store.task(id)?;
    let session = store.session_of(id)?.with_context(|| {
        format!(
            "task {} has no session; start it with `headwork spawn`",
            task.name
        )
    })?;
    std::env::set_current_dir(home.root())
        .with_context(|| format!("entering {}", home.root().display()))?;
    proc::ignore_interrupts()?;
    let started = start(home, &config, &mut store, &task, &session, mode);
    let child = match started {
        Ok(child) => child,
        Err(error) => {
            let reason = format!("{error:#}");
            store.append_event(
                Actor::Worker,
                Some(id),
                Some(&session.id),
                "launch-error",
                &serde_json::json!({ "error": reason }),
            )?;
            if task.state == TaskState::Spawning {
                store.transition(id, &Trigger::LaunchFailed, Actor::Worker)?;
            }
            return Err(error);
        }
    };
    drop(store);
    let status = child.wait();
    let code = status
        .as_ref()
        .ok()
        .and_then(std::process::ExitStatus::code);
    hand_exit(home, &session.id, Some(id), code)?;
    status?;
    Ok(code)
}

/// Has the installed build record the end of a session (`headwork exited`), so a launcher that has run for days
/// does not apply its own, older rules or meet a store schema it does not know. Only when that program cannot run
/// the command at all does the launcher do the work itself.
fn hand_exit(home: &Home, session: &str, task: Option<TaskId>, code: Option<i32>) -> Result<()> {
    let exe = std::env::current_exe().context("finding the headwork program")?;
    let mut command = Command::new(&exe);
    command
        .args(["exited", "--session", session, "--launcher-pid"])
        .arg(std::process::id().to_string())
        .arg("--home")
        .arg(home.root());
    if let Some(task) = task {
        command.args(["--task", &task.to_string()]);
    }
    if let Some(code) = code {
        command.args(["--code", &code.to_string()]);
    }
    match command.status() {
        Ok(status) if status.code() == Some(USAGE_ERROR) => {}
        Ok(status) if status.success() => return Ok(()),
        Ok(status) => anyhow::bail!("`headwork exited` failed with {status}"),
        Err(_) => {}
    }
    exited(home, session, task, code, std::process::id())
}

/// The exit code `clap` gives a command line it does not know.
const USAGE_ERROR: i32 = 2;

/// Records that a session's `claude` exited with `code`.
///
/// The session ends, a task whose session never got going fails, a running one is interrupted, and a closing one is
/// handed to a detached finisher that waits for the launcher `launcher_pid` before it closes the pane.
///
/// # Errors
///
/// Fails when the store or config cannot be read or the finisher cannot start.
pub fn exited(
    home: &Home,
    session: &str,
    task: Option<TaskId>,
    code: Option<i32>,
    launcher_pid: u32,
) -> Result<()> {
    let mut store = home.open_store()?;
    store.session_ended(session, code)?;
    let Some(id) = task else {
        return Ok(());
    };
    after_exit(&mut store, id)?;
    if store.task(id)?.state != TaskState::Closing {
        return Ok(());
    }
    let config = home.load_config()?;
    let host = super::host_for(&config)?;
    let exe = std::env::current_exe().context("finding the headwork program")?;
    let env = Env {
        home,
        config: &config,
        host: host.as_ref(),
        exe: &exe,
        actor: Actor::Worker,
    };
    let placed = match store.session_of(id)? {
        Some(row) => row,
        None => store
            .session(session)?
            .map(|(row, _)| row)
            .with_context(|| format!("session {session} is not in the store"))?,
    };
    close::hand_off(&mut store, &env, id, &placed, launcher_pid)
}

fn start(
    home: &Home,
    config: &Config,
    store: &mut Store,
    task: &Task,
    session: &SessionRow,
    mode: StartMode,
) -> Result<OwnedChild> {
    let dir = if task.in_main {
        let project = config
            .project(task.project())
            .with_context(|| format!("project `{}` is not configured", task.project()))?;
        home.expand(&project.main)
    } else {
        let worktree = store
            .worktree_of(task.id)?
            .with_context(|| format!("task {} has no worktree", task.name))?;
        PathBuf::from(worktree.path)
    };
    let exe = std::env::current_exe().context("finding the headwork program")?;
    let mut command = claude_command(home, config, task, session, &dir, &exe, mode)?;
    let child = OwnedChild::spawn(&mut command)?;
    let tree = match child.ownership() {
        Ownership::Job => "job".to_owned(),
        Ownership::ProcessGroup => "process-group".to_owned(),
        Ownership::Unowned(reason) => format!("unowned: {reason}"),
    };
    store.session_launched(&session.id, std::process::id(), child.pid(), &tree)?;
    record_build(store, Some(task.id), &session.id, &exe)?;
    if plugin_dir(home, config).is_some() {
        return Ok(child);
    }
    let facts = SessionFacts {
        session_matches: true,
        launcher_alive: true,
    };
    if let Err(error) = store.transition(task.id, &Trigger::SessionStarted(facts), Actor::Worker) {
        store.append_event(
            Actor::Worker,
            Some(task.id),
            Some(&session.id),
            "note",
            &serde_json::json!({ "start": error.to_string() }),
        )?;
    }
    Ok(child)
}

/// Records which build this launcher runs and which program its hooks and MCP server call ([`super::reload`]).
fn record_build(store: &Store, task: Option<TaskId>, session: &str, exe: &Path) -> Result<()> {
    store.append_event(
        Actor::Worker,
        task,
        Some(session),
        EVENT_BUILD,
        &build_event(exe),
    )?;
    Ok(())
}

/// Moves a task whose `claude` exited on its own: a session that never got going fails, a running one is
/// interrupted. Closing and closed tasks are left to `close`.
fn after_exit(store: &mut Store, id: TaskId) -> Result<()> {
    let task = store.task(id)?;
    let trigger = match task.state {
        TaskState::Spawning => Trigger::LaunchFailed,
        TaskState::Working | TaskState::NeedsUser | TaskState::InReview | TaskState::Merged => {
            Trigger::ProcessLost {
                machine: store.machine_mode()?,
            }
        }
        TaskState::Queued
        | TaskState::Closing
        | TaskState::Closed
        | TaskState::Dropped
        | TaskState::Failed
        | TaskState::Interrupted
        | TaskState::Parking
        | TaskState::Parked => return Ok(()),
    };
    match store.transition(id, &trigger, Actor::Worker) {
        Ok(_) | Err(headwork_store::StoreError::Refused { .. }) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Where the orchestrator session comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrchestratorOrigin {
    /// A new session, recorded now.
    New,
    /// A session a resume recorded or brought back.
    Back {
        /// Its id.
        session: String,
        /// Continue its conversation; otherwise it is a new conversation that starts from the recap.
        resume: bool,
        /// The resume's summary, which holds the orchestrator's recap.
        summary: Option<String>,
    },
}

/// How the orchestrator session starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestratorStart {
    /// The session and Remote Control name of a new orchestrator; one brought back keeps its recorded name.
    pub name: String,
    /// The agent definition; none picks it with [`orchestrator_agent`].
    pub agent: Option<String>,
    /// The first prompt of a new orchestrator.
    pub prompt: String,
    /// The model; claude's own default (the user's settings) when absent.
    pub model: Option<headwork_core::Model>,
    /// The effort; claude's default when absent.
    pub effort: Option<headwork_core::Effort>,
    /// The permission mode.
    pub mode: headwork_core::PermissionMode,
    /// A new session, or one a resume brings back.
    pub origin: OrchestratorOrigin,
}

/// The first prompt of the orchestrator session.
#[must_use]
pub fn orchestrator_prompt(start: &OrchestratorStart) -> String {
    let OrchestratorOrigin::Back {
        resume, summary, ..
    } = &start.origin
    else {
        return start.prompt.clone();
    };
    let summary = summary.as_deref().map_or_else(String::new, |path| {
        format!(" Read the resume summary at {path}: what came back, and your recap.")
    });
    if *resume {
        format!(
            "Resumed after a restart.{summary} Continue where your recap says; if it names a question you had open, \
             ask it again with AskUserQuestion."
        )
    } else {
        format!(
            "You are the orchestrator session. Your earlier conversation could not be restored after a restart. Read \
             HANDOFF.md beside CLAUDE.md.{summary} Then continue where the recap says."
        )
    }
}

/// Runs the orchestrator session in the current folder as a store session.
///
/// It gets its identity in the environment and returns the exit code of `claude`. The Remote Control name is
/// `start.name` (`orchestrator` by default) for a new session and the recorded one for a session brought back; the
/// agent is `start.agent`, else [`orchestrator_agent`]. The folder is recorded so a resume can start it there again.
///
/// # Errors
///
/// Fails when the store or config cannot be read, a session to bring back is unknown, or `claude` does not start.
pub fn launch_orchestrator(home: &Home, start: &OrchestratorStart) -> Result<Option<i32>> {
    let config = home.load_config()?;
    let mut store = home.open_store()?;
    let (id, resume, name) = match &start.origin {
        OrchestratorOrigin::New => {
            let id = uuid::Uuid::new_v4().to_string();
            store.record_session(
                &headwork_store::NewSession {
                    id: id.clone(),
                    task_id: None,
                    role: Role::Orchestrator,
                    rc_name: start.name.clone(),
                    host: config.machine.host.as_str().to_owned(),
                    host_workspace: None,
                    host_surface: None,
                },
                Actor::User,
            )?;
            (id, false, start.name.clone())
        }
        OrchestratorOrigin::Back {
            session, resume, ..
        } => {
            let row = store
                .session(session)?
                .with_context(|| format!("orchestrator session {session} is not in the store"))?;
            (session.clone(), *resume, row.0.rc_name)
        }
    };
    let dir = std::env::current_dir().context("finding the current folder")?;
    store.set_session_dir(&id, &super::slashed(&dir))?;
    let exe = std::env::current_exe().context("finding the headwork program")?;
    let plugin_dir = plugin_dir(home, &config);
    let prompt = orchestrator_prompt(start);
    let agent = start.agent.clone().unwrap_or_else(|| {
        let claude_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map_or_else(|| home.expand("~/.claude"), PathBuf::from);
        orchestrator_agent(&claude_dir).to_owned()
    });
    let argv = ClaudeLaunch {
        plugin_dir: plugin_dir.as_deref(),
        session_id: &id,
        resume,
        name: &name,
        remote_control: config.machine.remote_control,
        model: start.model,
        effort: start.effort,
        permission_mode: start.mode,
        agent: Some(&agent),
        prompt: &prompt,
    }
    .argv();
    let (program, rest) = argv.split_first().context("an empty argv")?;
    let mut command = Command::new(program);
    command
        .args(rest)
        .env(ENV_SESSION, &id)
        .env(ENV_ROLE, Role::Orchestrator.as_str())
        .env(ENV_HOME, home.root())
        .env(ENV_EXE, &exe);
    proc::ignore_interrupts()?;
    let child = OwnedChild::spawn(&mut command)?;
    let tree = match child.ownership() {
        Ownership::Job => "job".to_owned(),
        Ownership::ProcessGroup => "process-group".to_owned(),
        Ownership::Unowned(reason) => format!("unowned: {reason}"),
    };
    store.session_launched(&id, std::process::id(), child.pid(), &tree)?;
    record_build(&store, None, &id, &exe)?;
    drop(store);
    let status = child.wait();
    let code = status
        .as_ref()
        .ok()
        .and_then(std::process::ExitStatus::code);
    hand_exit(home, &id, None, code)?;
    status?;
    Ok(code)
}

/// The agent definition the orchestrator runs: `orchestrator` once `<claude_dir>/agents/orchestrator.md` exists, else
/// `root`, its name before 2026-11-14.
#[must_use]
pub fn orchestrator_agent(claude_dir: &Path) -> &'static str {
    if claude_dir.join("agents/orchestrator.md").is_file() {
        "orchestrator"
    } else {
        "root"
    }
}
