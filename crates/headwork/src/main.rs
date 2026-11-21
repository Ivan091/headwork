//! `headwork`: the command line of the headwork orchestrator.
//!
//! `board` and `log` read the store; `spawn` creates a task and runs the queue; `launch` is what a task's pane runs.
//! All read `$HEADWORK_HOME` (default `~/.headwork`).

mod render;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use headwork::home::Home;
use headwork::ops::answer::answer;
use headwork::ops::clean::{CleanOptions, clean};
use headwork::ops::close::{CloseReport, close, close_forced, close_own, finish_after, reap};
use headwork::ops::doctor::{Doctor, SystemProbe, doctor};
use headwork::ops::effort;
use headwork::ops::hook::{Git, HookEnv, HookEvent, parse_input, run_hook, scope_for};
use headwork::ops::import::{Sources, config_section, import, missing_projects};
use headwork::ops::install::{Stamp, install};
use headwork::ops::launch::{
    OrchestratorOrigin, OrchestratorStart, StartMode, exited, launch_orchestrator,
};
use headwork::ops::mcp::{Identity, Server};
use headwork::ops::park::{ParkOptions, park};
use headwork::ops::reap::{ReapOptions, reap as reap_processes};
use headwork::ops::relay::Relay;
use headwork::ops::reload;
use headwork::ops::resume::{ResumeOptions, TaskOutcome, resume};
use headwork::ops::shadow::shadow_report;
use headwork::ops::spawn::{Outcome, Pumped, SpawnRequest, pump, spawn};
use headwork::ops::tick::{TickEnv, tick};
use headwork::ops::{Env, host_for};
use headwork_adapters::disk::SystemDisks;
use headwork_adapters::git::Gh;
use headwork_core::guard::{ToolCall, check_tool};
use headwork_core::watch::Limits;
use headwork_core::{
    Actor, Config, Decision, Effort, Model, PermissionMode, Platform, ProjectName, Role, Slug,
    TaskRequest,
};
use headwork_store::{AskId, EventFilter, Store, TaskId};

/// Manages Claude Code sessions with hard rules in code.
#[derive(Debug, Parser)]
#[command(name = "headwork", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show projects, tasks, sessions, states and asks.
    Board {
        /// Include closed tasks.
        #[arg(long)]
        all: bool,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Show the event log, all of it or one task's.
    Log {
        /// A task, as `<project>-<slug>` or `<project>/<slug>`.
        name: Option<String>,
        /// Show at most this many of the newest events.
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Create a task and start it in its own pane when a slot is free; otherwise it waits in the queue.
    Spawn(SpawnArgs),
    /// Start every queued task that fits; with `--order`, put these queued tasks first, in this order.
    Queue {
        /// Task names, comma-separated.
        #[arg(long, value_delimiter = ',')]
        order: Vec<String>,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Close a task: its session, pane, process tree and worktree, verified before it is marked closed. From a worker's
    /// own session it takes the checked path (merged or no pull request, report sent, nothing waiting on the user).
    Close {
        /// A task, as `<project>-<slug>` or `<project>/<slug>`.
        name: String,
        /// Finish a task stuck in `closing` whose pull requests are all merged and that has no live job; a worktree the
        /// remove command refuses is kept on disk instead of blocking.
        #[arg(long)]
        force: bool,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Answer an ask the tick raised (one no session's dialog takes, such as keeping or deleting a closing task's
    /// worktree) with one of its options, and carry the answer out.
    Answer {
        /// The ask's id, as `board` and the inbox show it.
        ask: i64,
        /// The option: its number from 1, or its whole text.
        choice: String,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Adopt the workers, projects and worktrees of the orchestrate scripts.
    Import {
        /// The orchestrate `projects.conf`; projects missing from `config.toml` are appended to it.
        #[arg(long)]
        projects_conf: Option<PathBuf>,
        /// The Claude config folder.
        #[arg(long)]
        claude_dir: Option<PathBuf>,
        /// Show what would be adopted and appended, change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Reconcile the store with reality: panes, processes, pull requests, worktrees, disks, runaway files, stuck
    /// sessions and asks. Meant to run every few minutes from a scheduler.
    Tick {
        #[arg(long, conflicts_with = "uninstall", help = Platform::current().tick_install_help())]
        install: bool,
        #[arg(long, help = Platform::current().tick_uninstall_help())]
        uninstall: bool,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Prepare for a restart: every session writes a recap, registered jobs stop at a checkpoint, the panes close.
    Park {
        /// Seconds to wait for recaps and checkpoints before forcing what is left.
        #[arg(long, default_value_t = 300)]
        deadline: u64,
        /// The headwork home.
        #[arg(long)]
        home: Option<PathBuf>,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// After a restart: reopen parked sessions with their recaps, restart jobs, write a summary for the orchestrator.
    Resume {
        /// Started by a logon task: sessions interrupted by a crash come back only as `resume_after_crash` allows.
        #[arg(long)]
        logon: bool,
        /// Seconds to wait for the terminal host to answer.
        #[arg(long, default_value_t = 0)]
        wait: u64,
        /// The headwork home.
        #[arg(long)]
        home: Option<PathBuf>,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// List the registered jobs that are not over.
    Jobs {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Act on one registered job.
    Job {
        #[command(subcommand)]
        action: JobAction,
    },
    /// Set the title of every live worker pane (or one task's) from its model and effort.
    Retitle {
        /// A task, as `<project>-<slug>` or `<project>/<slug>`; every live task when absent.
        name: Option<String>,
    },
    /// Show a task's newest recap.
    Recap {
        /// A task, as `<project>-<slug>` or `<project>/<slug>`.
        name: String,
    },
    /// Finish closing a task once a process is gone: what a closing launcher leaves behind.
    #[command(hide = true)]
    Finish {
        /// The task's id.
        #[arg(long)]
        task: i64,
        /// Wait for this process to end first.
        #[arg(long)]
        wait_pid: u32,
        /// The headwork home.
        #[arg(long)]
        home: Option<PathBuf>,
    },
    /// Answer a Claude Code hook: the plugin's hooks run this with the event's JSON on stdin.
    #[command(hide = true)]
    Hook {
        /// The event: session-start, user-prompt-submit, pre-tool-use, post-tool-use, stop or session-end.
        event: HookEvent,
    },
    /// Serve the MCP tools of the calling session over stdio: the plugin's `.mcp.json` runs this. It relays to an
    /// inner server and restarts that one when a new build is installed.
    #[command(hide = true)]
    Mcp {
        /// Be the server itself, not the relay; every later build must keep this flag.
        #[arg(long)]
        serve: bool,
    },
    /// Record that a session's `claude` exited: what a launcher has the installed build do.
    #[command(hide = true)]
    Exited {
        /// The Claude session id.
        #[arg(long)]
        session: String,
        /// The session's task, absent for the orchestrator.
        #[arg(long)]
        task: Option<i64>,
        /// The exit code of `claude`.
        #[arg(long, allow_negative_numbers = true)]
        code: Option<i32>,
        /// The launcher, which a closing task's finisher waits for.
        #[arg(long)]
        launcher_pid: u32,
        /// The headwork home.
        #[arg(long)]
        home: Option<PathBuf>,
    },
    /// Install this build as `headwork` in a folder on PATH, safely while the old one runs, and say which running
    /// sessions pick it up by themselves.
    Install {
        /// The folder.
        #[arg(long, default_value = "~/.local/bin")]
        dir: String,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Check the install: binary, config, store, host, gh, claude, plugin, hooks, disk. Exits 1 on a problem.
    Doctor {
        /// The Claude config folder.
        #[arg(long)]
        claude_dir: Option<PathBuf>,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Remove the worktrees of merged and closed tasks through the project's own remove command, and Claude Code
    /// agent worktrees in main checkouts whose agent is gone and whose work landed; list the ones left and why.
    Clean {
        /// Print the plan and change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Also run plain `git gc` in each main checkout where no main-only job or task runs.
        #[arg(long)]
        gc: bool,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// End what ended sessions left running (the processes their launcher and `claude` started) and orphaned `tail`
    /// and `bash` idle for over 10 minutes. Never `claude`, `me-studio`, `headwork` or a registered job, nor what they
    /// started.
    Reap {
        /// Print what would be ended and end nothing.
        #[arg(long)]
        dry_run: bool,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// The would-deny events the hooks logged in shadow mode, by rule id.
    ShadowReport {
        /// Only events at or after this time: an RFC 3339 date and time, a date `YYYY-MM-DD`, or a span back from now
        /// such as `90m`, `24h`, `7d`.
        #[arg(long)]
        since: Option<String>,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Run the orchestrator session here, recorded in the store. `headwork root` is its old name and still works.
    #[command(alias = "root")]
    Orchestrator {
        /// The session and Remote Control name; workers message it. A session brought back keeps its own name.
        #[arg(long, default_value = headwork_core::ORCHESTRATOR_ADDRESS)]
        name: String,
        /// The agent definition; `orchestrator` when `~/.claude/agents/orchestrator.md` exists, else `root`.
        #[arg(long)]
        agent: Option<String>,
        /// The model; claude's own default (the user's settings) when absent.
        #[arg(long)]
        model: Option<Model>,
        /// The effort; claude's default when absent.
        #[arg(long)]
        effort: Option<Effort>,
        /// The permission mode.
        #[arg(long, default_value = "default")]
        mode: PermissionMode,
        /// The first prompt.
        #[arg(
            long,
            default_value = "You are the orchestrator session. Read HANDOFF.md beside CLAUDE.md and wait for the user."
        )]
        prompt: String,
        /// Run this recorded orchestrator session, as a resume does, instead of recording a new one.
        #[arg(long)]
        session: Option<String>,
        /// Continue that session's conversation; without it the session starts fresh from its recap.
        #[arg(long, requires = "session")]
        resume: bool,
        /// The resume's summary, named in the first prompt of an orchestrator that comes back.
        #[arg(long, requires = "session")]
        summary: Option<String>,
        /// The headwork home; panes do not inherit the caller's environment.
        #[arg(long)]
        home: Option<PathBuf>,
    },
    /// Check the hard rules by hand.
    Rule {
        #[command(subcommand)]
        action: RuleAction,
    },
    /// Run a task's session: the program each pane runs.
    Launch {
        /// The task's id.
        #[arg(long)]
        task: i64,
        /// Continue the task's earlier conversation.
        #[arg(long, conflicts_with = "fresh")]
        resume: bool,
        /// Start a new conversation from the brief and the recap, because the earlier one is gone.
        #[arg(long)]
        fresh: bool,
        /// The session was cut off by a crash, not parked.
        #[arg(long)]
        crash: bool,
        /// The headwork home; panes do not inherit the caller's environment.
        #[arg(long)]
        home: Option<PathBuf>,
    },
}

/// What `job` does.
#[derive(Debug, Subcommand)]
enum JobAction {
    /// Mark a job over without touching any process, for a job whose process ended unseen or whose pid another
    /// program now holds.
    Forget {
        /// The job's id, as `headwork jobs` lists it.
        id: i64,
        /// Why it is over; recorded in the job's note and the event log.
        #[arg(long)]
        reason: String,
    },
}

/// What `rule` does.
#[derive(Debug, Subcommand)]
enum RuleAction {
    /// Decide a Bash command as the orchestrator, or as a task's worker with `--task`; exits 1 on a deny.
    Test {
        /// The command line, or the file with `--edit`.
        command: String,
        /// Decide an `Edit` of the file the argument names instead of a Bash command.
        #[arg(long)]
        edit: bool,
        /// Decide as this task's worker session.
        #[arg(long)]
        task: Option<String>,
        /// The working folder; the task's folder or the current one when absent.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// The checked-out branch, instead of asking git.
        #[arg(long)]
        branch: Option<String>,
        /// As if run with `run_in_background`.
        #[arg(long)]
        background: bool,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
}

/// The arguments of `spawn`.
#[derive(Debug, clap::Args)]
struct SpawnArgs {
    /// The project.
    project: String,
    /// The task's slug, kebab-case.
    slug: String,
    /// The brief file; it is copied into the home folder.
    brief: PathBuf,
    /// Run in the project's main checkout; the kind must be main-only.
    #[arg(long = "main")]
    in_main: bool,
    /// A standing role that does not close when idle.
    #[arg(long)]
    keep: bool,
    /// The model; the machine default when absent.
    #[arg(long)]
    model: Option<Model>,
    /// The effort; the machine default when absent.
    #[arg(long)]
    effort: Option<Effort>,
    /// The permission mode.
    #[arg(long, default_value = "auto")]
    mode: PermissionMode,
    /// The task kind; the slug for a main-checkout task, `task` otherwise.
    #[arg(long)]
    kind: Option<String>,
    /// Start only after this task is merged.
    #[arg(long)]
    after: Option<String>,
    /// A user-only command the user asked this worker to run, exactly as the user asked for it; repeatable. The
    /// worker's first action is the `Allow` ask for it, and it runs the command only after the user's tap.
    #[arg(long = "allow-ask", value_name = "COMMAND")]
    allow_ask: Vec<String>,
    /// Print JSON instead of text.
    #[arg(long)]
    json: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut out = io::stdout().lock();
    match run(cli, &mut out) {
        Ok(code) => code,
        Err(error) => {
            let mut err = io::stderr().lock();
            if writeln!(err, "headwork: {error:#}").is_err() {
                return ExitCode::from(2);
            }
            ExitCode::FAILURE
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per command that needs no store"
)]
fn run(cli: Cli, out: &mut impl Write) -> Result<ExitCode> {
    let command = match cli.command {
        Command::Hook { event } => return Ok(hook_command(event, out)),
        Command::Mcp { serve: true } => return mcp_command(),
        Command::Mcp { serve: false } => return relay_command(),
        Command::Exited {
            session,
            task,
            code,
            launcher_pid,
            home,
        } => {
            exited(
                &Home::at(home)?,
                &session,
                task.map(TaskId),
                code,
                launcher_pid,
            )?;
            return Ok(ExitCode::SUCCESS);
        }
        Command::Install { dir, json } => return install_command(&dir, json, out),
        Command::Tick {
            install,
            uninstall,
            json,
        } if install || uninstall => return schedule_command(install, json, out),
        Command::Rule {
            action:
                RuleAction::Test {
                    command,
                    edit,
                    task,
                    cwd,
                    branch,
                    background,
                    json,
                },
        } => {
            let probe = RuleProbe {
                command: &command,
                edit,
                task: task.as_deref(),
                cwd,
                branch,
                background,
            };
            return rule_test(&probe, json, out);
        }
        Command::Doctor { claude_dir, json } => return doctor_command(claude_dir, json, out),
        Command::Orchestrator {
            name,
            agent,
            model,
            effort,
            mode,
            prompt,
            session,
            resume,
            summary,
            home,
        } => {
            let origin = session.map_or(OrchestratorOrigin::New, |session| {
                OrchestratorOrigin::Back {
                    session,
                    resume,
                    summary,
                }
            });
            return orchestrator_command(
                &Home::at(home)?,
                &OrchestratorStart {
                    name,
                    agent,
                    prompt,
                    model,
                    effort,
                    mode,
                    origin,
                },
            );
        }
        Command::Launch {
            task,
            resume,
            fresh,
            crash,
            home,
        } => return launch_command(&Home::at(home)?, task, (resume, fresh, crash)),
        Command::Park {
            deadline,
            home,
            json,
        } => return park_command(&Home::at(home)?, deadline, json, out),
        Command::Resume {
            logon,
            wait,
            home,
            json,
        } => return resume_command(&Home::at(home)?, logon, wait, json, out),
        Command::Finish {
            task,
            wait_pid,
            home,
        } => return finish_command(&Home::at(home)?, TaskId(task), wait_pid),
        Command::Import {
            projects_conf,
            claude_dir,
            dry_run,
            json,
        } => return import_command(projects_conf, claude_dir, dry_run, json, out),
        other @ (Command::Board { .. }
        | Command::Log { .. }
        | Command::Spawn(_)
        | Command::Queue { .. }
        | Command::Close { .. }
        | Command::Answer { .. }
        | Command::Tick { .. }
        | Command::Jobs { .. }
        | Command::Job { .. }
        | Command::Retitle { .. }
        | Command::Recap { .. }
        | Command::ShadowReport { .. }
        | Command::Clean { .. }
        | Command::Reap { .. }) => other,
    };
    let home = Home::locate()?;
    let config = home.load_config()?;
    let mut store = home.open_store()?;
    store
        .sync_projects(&config)
        .context("recording the configured projects")?;
    let exe = std::env::current_exe().context("finding the headwork program")?;
    let ctx = Ctx {
        home: &home,
        config: &config,
        exe: &exe,
    };
    match command {
        Command::Board { all, json } => board(&mut store, &ctx, all, json, out),
        Command::Log { name, limit, json } => log(&store, &ctx, name, limit, json, out),
        Command::Spawn(args) => spawn_command(&mut store, &ctx, args, out),
        Command::Queue { order, json } => queue_command(&mut store, &ctx, &order, json, out),
        Command::Close { name, force, json } => {
            close_command(&mut store, &ctx, &name, force, json, out)
        }
        Command::Answer { ask, choice, json } => {
            answer_command(&mut store, &ctx, AskId(ask), &choice, json, out)
        }
        Command::Tick { json, .. } => tick_command(&mut store, &ctx, json, out),
        Command::Jobs { json } => jobs_command(&mut store, json, out),
        Command::Job {
            action: JobAction::Forget { id, reason },
        } => {
            let job = headwork::ops::jobs::forget(&mut store, id, &reason, Actor::User)?;
            writeln!(out, "job {} (`{}`) is over: {reason}", job.id, job.label)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Recap { name } => recap_command(&store, &name, out),
        Command::Retitle { name } => retitle_command(&store, &ctx, name.as_deref(), out),
        Command::ShadowReport { since, json } => {
            shadow_command(&store, &ctx, since.as_deref(), json, out)
        }
        Command::Clean { dry_run, gc, json } => {
            clean_command(&mut store, &ctx, CleanOptions { dry_run, gc }, json, out)
        }
        Command::Reap { dry_run, json } => {
            reap_command(&mut store, &ctx, ReapOptions { dry_run }, json, out)
        }
        Command::Launch { .. }
        | Command::Park { .. }
        | Command::Resume { .. }
        | Command::Finish { .. }
        | Command::Import { .. }
        | Command::Doctor { .. }
        | Command::Orchestrator { .. }
        | Command::Hook { .. }
        | Command::Mcp { .. }
        | Command::Exited { .. }
        | Command::Install { .. }
        | Command::Rule { .. } => Ok(ExitCode::SUCCESS),
    }
}

fn standalone<T>(
    home: &Home,
    actor: Actor,
    act: impl FnOnce(&mut Store, &Env<'_>) -> Result<T>,
) -> Result<T> {
    let config = home.load_config()?;
    let mut store = home.open_store()?;
    store
        .sync_projects(&config)
        .context("recording the configured projects")?;
    let exe = std::env::current_exe().context("finding the headwork program")?;
    let host = host_for(&config)?;
    let env = Env {
        home,
        config: &config,
        host: host.as_ref(),
        exe: &exe,
        actor,
    };
    act(&mut store, &env)
}

fn park_command(home: &Home, deadline: u64, json: bool, out: &mut impl Write) -> Result<ExitCode> {
    let opts = ParkOptions {
        deadline: std::time::Duration::from_secs(deadline),
        ..ParkOptions::default()
    };
    let report = standalone(home, Actor::User, |store, env| park(store, env, opts))?;
    if json {
        json_out(out, &report)?;
    } else {
        out.write_all(render::parked(&report).as_bytes())?;
    }
    Ok(if report.safe_to_restart {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn resume_command(
    home: &Home,
    logon: bool,
    wait: u64,
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let claude_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map_or_else(|| home.expand("~/.claude"), PathBuf::from);
    let opts = ResumeOptions {
        logon,
        wait_for_host: std::time::Duration::from_secs(wait),
        claude_dir,
    };
    let report = standalone(home, Actor::User, |store, env| resume(store, env, &opts))?;
    if json {
        json_out(out, &report)?;
    } else {
        out.write_all(render::resumed(&report).as_bytes())?;
    }
    let failed = report
        .tasks
        .iter()
        .any(|task| matches!(task.outcome, TaskOutcome::Failed { .. }));
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn jobs_command(store: &mut Store, json: bool, out: &mut impl Write) -> Result<ExitCode> {
    headwork::ops::jobs::refresh(store, Actor::User)?;
    let jobs = store.open_jobs(None)?;
    if json {
        json_out(out, &jobs)?;
    } else {
        out.write_all(render::jobs(&jobs).as_bytes())?;
    }
    Ok(ExitCode::SUCCESS)
}

fn retitle_command(
    store: &Store,
    ctx: &Ctx<'_>,
    name: Option<&str>,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let only = name
        .map(|name| {
            store
                .find_task(name)?
                .with_context(|| format!("no task named `{name}`"))
        })
        .transpose()?;
    let (done, failed) =
        ctx.with_env(|env| headwork::ops::title::retitle(store, env, only.as_ref()))?;
    for item in &done {
        writeln!(out, "{} -> {}", item.surface, item.title)?;
    }
    for line in &failed {
        writeln!(out, "failed: {line}")?;
    }
    if done.is_empty() && failed.is_empty() {
        writeln!(out, "no live session to retitle")?;
    }
    Ok(if failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn recap_command(store: &Store, name: &str, out: &mut impl Write) -> Result<ExitCode> {
    let task = store
        .find_task(name)?
        .with_context(|| format!("no task named `{name}`"))?;
    match store.latest_recap(task.id)? {
        Some(recap) => writeln!(
            out,
            "{} ({}, {})\n\n{}",
            task.name, recap.kind, recap.created_at, recap.body
        )?,
        None => writeln!(out, "{} has no recap", task.name)?,
    }
    Ok(ExitCode::SUCCESS)
}

/// What every command that opens panes needs, besides the store.
struct Ctx<'a> {
    home: &'a Home,
    config: &'a Config,
    exe: &'a Path,
}

impl Ctx<'_> {
    fn with_env<T>(&self, act: impl FnOnce(&Env<'_>) -> Result<T>) -> Result<T> {
        self.with_actor(Actor::User, act)
    }

    fn with_actor<T>(&self, actor: Actor, act: impl FnOnce(&Env<'_>) -> Result<T>) -> Result<T> {
        let host = host_for(self.config)?;
        act(&Env {
            home: self.home,
            config: self.config,
            host: host.as_ref(),
            exe: self.exe,
            actor,
        })
    }
}

fn json_out(out: &mut impl Write, value: &impl serde::Serialize) -> Result<()> {
    serde_json::to_writer_pretty(&mut *out, value)?;
    writeln!(out)?;
    Ok(())
}

fn board(
    store: &mut Store,
    ctx: &Ctx<'_>,
    all: bool,
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    ctx.with_env(|env| headwork::ops::lost::reconcile(store, env).map(drop))?;
    let mut board = store.board(all)?;
    for entry in &mut board.tasks {
        entry.task.effort = Some(effort::of_task(ctx.home, ctx.config, &entry.task));
    }
    if json {
        json_out(out, &board)?;
    } else {
        out.write_all(render::board(&board, ctx.config.machine.max_workers.get()).as_bytes())?;
    }
    Ok(ExitCode::SUCCESS)
}

fn log(
    store: &Store,
    ctx: &Ctx<'_>,
    name: Option<String>,
    limit: u32,
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let task = match name {
        Some(name) => match store.find_task(&name)? {
            Some(task) => Some(task.id),
            None => bail!("no task named `{name}`"),
        },
        None => None,
    };
    let mut events = store.events(EventFilter { task, limit })?;
    for event in &mut events {
        if event.kind != "task-created"
            || !event
                .data
                .get("effort")
                .is_some_and(serde_json::Value::is_null)
        {
            continue;
        }
        if let Some(id) = event.task_id {
            let found = store.task(id)?;
            event.data["effort"] = serde_json::json!(effort::of_task(ctx.home, ctx.config, &found));
        }
    }
    if json {
        json_out(out, &events)?;
    } else {
        out.write_all(render::events(&events).as_bytes())?;
    }
    Ok(ExitCode::SUCCESS)
}

fn spawn_command(
    store: &mut Store,
    ctx: &Ctx<'_>,
    args: SpawnArgs,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let depends_on = match &args.after {
        Some(name) => Some(
            store
                .find_task(name)?
                .with_context(|| format!("no task named `{name}`"))?
                .id,
        ),
        None => None,
    };
    let kind = args.kind.unwrap_or_else(|| {
        if args.in_main {
            args.slug.clone()
        } else {
            "task".to_owned()
        }
    });
    let request = SpawnRequest {
        request: TaskRequest {
            project: ProjectName::new(args.project)?,
            slug: Slug::new(args.slug)?,
            kind,
            model: args.model,
            effort: args.effort,
            in_main: args.in_main,
            keep: args.keep,
        },
        brief: &args.brief,
        depends_on,
        permission_mode: args.mode,
        allow_ask: &args.allow_ask,
    };
    let (task, pumped) = ctx.with_env(|env| {
        reap(store, env)?;
        spawn(store, env, &request)
    })?;
    if args.json {
        json_out(out, &serde_json::json!({ "task": task, "queue": pumped }))?;
    } else {
        writeln!(out, "{} {}", task.name, task.state)?;
        out.write_all(render::pumped(&pumped).as_bytes())?;
    }
    Ok(exit_for(&pumped))
}

fn queue_command(
    store: &mut Store,
    ctx: &Ctx<'_>,
    order: &[String],
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    if !order.is_empty() {
        let ids = order
            .iter()
            .map(|name| {
                store
                    .find_task(name)?
                    .map(|task| task.id)
                    .with_context(|| format!("no task named `{name}`"))
            })
            .collect::<Result<Vec<_>>>()?;
        store.set_queue_order(&ids, Actor::User)?;
    }
    let pumped = ctx.with_env(|env| {
        reap(store, env)?;
        pump(store, env)
    })?;
    if json {
        json_out(out, &pumped)?;
    } else if pumped.is_empty() {
        writeln!(out, "the queue is empty")?;
    } else {
        out.write_all(render::pumped(&pumped).as_bytes())?;
    }
    Ok(exit_for(&pumped))
}

fn exit_for(pumped: &[Pumped]) -> ExitCode {
    if pumped
        .iter()
        .any(|p| matches!(p.outcome, Outcome::Failed { .. }))
    {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn close_command(
    store: &mut Store,
    ctx: &Ctx<'_>,
    name: &str,
    force: bool,
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let task = store
        .find_task(name)?
        .with_context(|| format!("no task named `{name}`"))?;
    let identity = Identity::from_env();
    let actor = identity.actor();
    let own_task = match identity.session.as_deref() {
        Some(own) => store.session_of(task.id)?.is_some_and(|s| s.id == own),
        None => false,
    };
    let report: CloseReport = ctx.with_actor(actor, |env| {
        let report = if force {
            close_forced(store, env, &Gh::default(), task.id)?
        } else if actor == Actor::Worker && own_task {
            close_own(
                store,
                env,
                &Gh::default(),
                task.id,
                identity.session.as_deref(),
            )?
        } else {
            close(store, env, task.id, identity.session.as_deref())?
        };
        pump(store, env)?;
        Ok(report)
    })?;
    if json {
        json_out(out, &report)?;
    } else {
        match &report.pending {
            None => writeln!(out, "{} {}", report.name, report.state)?,
            Some(pending) => writeln!(out, "{} {}: {pending}", report.name, report.state)?,
        }
    }
    Ok(if report.pending.is_none() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn answer_command(
    store: &mut Store,
    ctx: &Ctx<'_>,
    ask: AskId,
    choice: &str,
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let actor = Identity::from_env().actor();
    let report = ctx.with_actor(actor, |env| {
        let report = answer(store, env, ask, choice)?;
        pump(store, env)?;
        Ok(report)
    })?;
    if json {
        json_out(out, &report)?;
        return Ok(ExitCode::SUCCESS);
    }
    let task = report.task.as_deref().unwrap_or("no task");
    writeln!(out, "ask {} ({task}): {}", report.ask, report.answer)?;
    match &report.close {
        None => {}
        Some(close) => match &close.pending {
            None => writeln!(out, "{} {}", close.name, close.state)?,
            Some(pending) => writeln!(out, "{} {}: {pending}", close.name, close.state)?,
        },
    }
    Ok(ExitCode::SUCCESS)
}

fn finish_command(home: &Home, task: TaskId, wait_pid: u32) -> Result<ExitCode> {
    let config = home.load_config()?;
    let mut store = home.open_store()?;
    let exe = std::env::current_exe().context("finding the headwork program")?;
    let ctx = Ctx {
        home,
        config: &config,
        exe: &exe,
    };
    let report = ctx.with_env(|env| finish_after(&mut store, env, task, wait_pid))?;
    Ok(if report.pending.is_none() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn import_command(
    projects_conf: Option<PathBuf>,
    claude_dir: Option<PathBuf>,
    dry_run: bool,
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let home = Home::locate()?;
    let claude_dir = claude_dir.unwrap_or_else(|| home.expand("~/.claude"));
    let conf_path =
        projects_conf.unwrap_or_else(|| claude_dir.join("skills/orchestrate/projects.conf"));
    let config_path = home.root().join("config.toml");
    let existing = std::fs::read_to_string(&config_path).ok();
    let base = existing
        .clone()
        .unwrap_or_else(|| "[machine]\nhost = \"wmux\"\n".to_owned());
    let base_config = Config::parse(&base, &config_path.display().to_string())?;
    let conf = std::fs::read_to_string(&conf_path).unwrap_or_default();
    let mut sections = String::new();
    let mut added = Vec::new();
    for (name, main) in missing_projects(&conf, &base_config) {
        let origin = std::process::Command::new("git")
            .args(["remote", "get-url", "origin"])
            .current_dir(&main)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
        sections.push_str(&config_section(&name, &main, origin.as_deref()));
        added.push(name);
    }
    let merged = format!(
        "{}{sections}",
        base.trim_end_matches('\n').to_owned() + "\n"
    );
    let config = Config::parse(&merged, "the imported config")?;
    if !dry_run && (existing.is_none() || !sections.is_empty()) {
        std::fs::create_dir_all(home.root())?;
        std::fs::write(&config_path, &merged)
            .with_context(|| format!("writing {}", config_path.display()))?;
    }
    let mut store = home.open_store()?;
    if !dry_run {
        store.sync_projects(&config)?;
    }
    let exe = std::env::current_exe().context("finding the headwork program")?;
    let ctx = Ctx {
        home: &home,
        config: &config,
        exe: &exe,
    };
    let briefs_dir = claude_dir.join("cache/orchestrate/briefs");
    let sources = Sources {
        claude_dir: &claude_dir,
        briefs_dir: &briefs_dir,
    };
    let report = ctx.with_env(|env| import(&mut store, env, sources, dry_run))?;
    if json {
        json_out(
            out,
            &serde_json::json!({ "projects_added": added, "report": report, "dry_run": dry_run }),
        )?;
    } else {
        out.write_all(render::imported(&added, &report, dry_run).as_bytes())?;
    }
    Ok(ExitCode::SUCCESS)
}

fn mcp_command() -> Result<ExitCode> {
    let home = Home::locate()?;
    let config = home.load_config()?;
    let mut store = home.open_store()?;
    store
        .sync_projects(&config)
        .context("recording the configured projects")?;
    let host = host_for(&config)?;
    let exe = std::env::current_exe().context("finding the headwork program")?;
    let env = Env {
        home: &home,
        config: &config,
        host: host.as_ref(),
        exe: &exe,
        actor: Actor::User,
    };
    let git = Gh::default();
    let mut server = Server::new(&mut store, env, &git, Identity::from_env());
    server.serve(io::stdin().lock(), io::stdout().lock())?;
    Ok(ExitCode::SUCCESS)
}

/// The relay Claude starts: it serves through an inner `mcp --serve` started from the program the session's hooks
/// call (`HEADWORK_EXE`), else from this program.
fn relay_command() -> Result<ExitCode> {
    let exe = match std::env::var_os(headwork_adapters::runtime::ENV_EXE) {
        Some(exe) if !exe.is_empty() => PathBuf::from(exe),
        _ => std::env::current_exe().context("finding the headwork program")?,
    };
    Relay::new(exe).serve(io::stdin().lock(), io::stdout().lock())?;
    Ok(ExitCode::SUCCESS)
}

fn install_command(dir: &str, json: bool, out: &mut impl Write) -> Result<ExitCode> {
    let home = Home::locate()?;
    let source = std::env::current_exe().context("finding the headwork program")?;
    let report = install(&source, &home.expand(dir), verify_installed)?;
    let sessions = if home.root().join("state.db").exists() {
        let store = home.open_store()?;
        reload::classify(
            &store.live_sessions_with_event(reload::EVENT_BUILD)?,
            |exe| Stamp::of(exe).ok(),
        )
    } else {
        Vec::new()
    };
    if json {
        json_out(
            out,
            &serde_json::json!({ "install": report, "sessions": sessions }),
        )?;
    } else {
        out.write_all(render::installed(&report, &sessions).as_bytes())?;
    }
    Ok(ExitCode::SUCCESS)
}

fn verify_installed(target: &Path) -> Result<()> {
    let output = std::process::Command::new(target)
        .arg("--version")
        .output()
        .with_context(|| format!("running {} --version", target.display()))?;
    let text = String::from_utf8_lossy(&output.stdout);
    if output.status.success() && text.starts_with("headwork ") {
        Ok(())
    } else {
        bail!("{} --version printed {text:?}", target.display())
    }
}

/// Runs one hook. A session without `HEADWORK_SESSION` is unmanaged and allowed at once. A failure inside the hook
/// never blocks: it goes to stderr with exit code 1, which Claude shows without stopping the call.
fn hook_command(event: HookEvent, out: &mut impl Write) -> ExitCode {
    let Ok(session) = std::env::var(headwork_adapters::runtime::ENV_SESSION) else {
        return ExitCode::SUCCESS;
    };
    match answer_hook(event, &session) {
        Ok(reply) => {
            let stdout_ok = reply
                .stdout
                .is_none_or(|text| writeln!(out, "{text}").is_ok());
            let stderr_ok = reply
                .stderr
                .is_none_or(|text| writeln!(io::stderr().lock(), "{text}").is_ok());
            ExitCode::from(if stdout_ok && stderr_ok {
                reply.exit
            } else {
                1
            })
        }
        Err(error) => {
            let written = writeln!(
                io::stderr().lock(),
                "headwork hook {}: {error:#}",
                event.claude_name()
            );
            ExitCode::from(if written.is_ok() { 1 } else { 3 })
        }
    }
}

fn answer_hook(event: HookEvent, session: &str) -> Result<headwork::ops::hook::HookReply> {
    let mut text = String::new();
    io::Read::read_to_string(&mut io::stdin().lock(), &mut text)
        .context("reading the hook's input")?;
    let input = parse_input(&text)?;
    let home = Home::locate()?;
    let config = home.load_config()?;
    let mut store = home.open_store()?;
    let host = host_for(&config)?;
    let exe = std::env::current_exe().context("finding the headwork program")?;
    let temp = std::env::temp_dir();
    let env = HookEnv {
        home: &home,
        config: &config,
        host: host.as_ref(),
        git: &Git,
        exe: &exe,
        temp: &temp,
    };
    run_hook(&mut store, &env, event, session, &input)
}

/// What `rule test` decides.
struct RuleProbe<'a> {
    command: &'a str,
    edit: bool,
    task: Option<&'a str>,
    cwd: Option<PathBuf>,
    branch: Option<String>,
    background: bool,
}

fn rule_test(probe: &RuleProbe<'_>, json: bool, out: &mut impl Write) -> Result<ExitCode> {
    let home = Home::locate()?;
    let config = home.load_config()?;
    let mut store = home.open_store()?;
    store.sync_projects(&config)?;
    let task = probe
        .task
        .map(|name| {
            store
                .find_task(name)?
                .with_context(|| format!("no task named `{name}`"))
        })
        .transpose()?;
    let host = host_for(&config)?;
    let exe = std::env::current_exe().context("finding the headwork program")?;
    let temp = std::env::temp_dir();
    let env = HookEnv {
        home: &home,
        config: &config,
        host: host.as_ref(),
        git: &Git,
        exe: &exe,
        temp: &temp,
    };
    let cwd = match (&probe.cwd, &task) {
        (Some(cwd), _) => cwd.clone(),
        (None, Some(task)) => Env {
            home: &home,
            config: &config,
            host: host.as_ref(),
            exe: &exe,
            actor: Actor::User,
        }
        .task_dir(task)?,
        (None, None) => std::env::current_dir()?,
    };
    let role = if task.is_some() {
        Role::Worker
    } else {
        Role::Orchestrator
    };
    let cwd = headwork::ops::slashed(&cwd);
    let mut scope = scope_for(
        &store,
        &env,
        role,
        task.as_ref(),
        &cwd,
        (!probe.edit).then_some(probe.command),
    )?;
    if let Some(branch) = &probe.branch {
        scope.branch = Some(branch.clone());
    }
    let call = if probe.edit {
        ToolCall::Write {
            tool: "Edit",
            path: probe.command,
        }
    } else {
        ToolCall::Bash {
            command: probe.command,
            background: probe.background,
        }
    };
    let decision = check_tool(&scope, &call);
    let enforced = match &decision {
        Decision::Deny(verdict) => config.hooks.enforces(verdict.rule),
        Decision::Allow => config.hooks.enforce,
    };
    let mode = if enforced { "enforced" } else { "shadow" };
    if json {
        json_out(
            out,
            &serde_json::json!({ "role": role, "mode": mode, "result": decision }),
        )?;
    } else {
        match &decision {
            Decision::Allow => writeln!(out, "allow ({role}, {mode})")?,
            Decision::Deny(verdict) => writeln!(
                out,
                "deny {} ({role}, {mode}): {}",
                verdict.rule, verdict.reason
            )?,
        }
    }
    Ok(match decision {
        Decision::Allow => ExitCode::SUCCESS,
        Decision::Deny(_) => ExitCode::FAILURE,
    })
}

/// Runs one tick under a lock file, so two overlapping scheduler runs never act at once.
fn tick_command(
    store: &mut Store,
    ctx: &Ctx<'_>,
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let lock_path = ctx.home.root().join("tick.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("opening {}", lock_path.display()))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            writeln!(out, "another tick is running")?;
            return Ok(ExitCode::SUCCESS);
        }
        Err(std::fs::TryLockError::Error(error)) => {
            return Err(error).with_context(|| format!("locking {}", lock_path.display()));
        }
    }
    let git = Gh::default();
    let disks = SystemDisks;
    let temp = std::env::temp_dir();
    let report = ctx.with_actor(Actor::Tick, |env| {
        tick(
            store,
            &TickEnv {
                env,
                git: &git,
                disks: &disks,
                temp: &temp,
                limits: Limits::from_config(&ctx.config.watch),
            },
        )
    })?;
    if json {
        json_out(out, &report)?;
    } else {
        out.write_all(render::ticked(&report).as_bytes())?;
    }
    Ok(ExitCode::SUCCESS)
}

fn schedule_command(install: bool, json: bool, out: &mut impl Write) -> Result<ExitCode> {
    use headwork::ops::schedule::{self, Schtasks};
    let report = if install {
        let home = Home::locate()?;
        let installed = home.expand(&format!(
            "~/.local/bin/headwork{}",
            std::env::consts::EXE_SUFFIX
        ));
        let current = std::env::current_exe().context("finding the headwork program")?;
        schedule::install(&Schtasks, &schedule::program(&installed, &current))?
    } else {
        schedule::uninstall(&Schtasks)?
    };
    if json {
        json_out(out, &report)?;
    } else {
        out.write_all(render::scheduled(&report).as_bytes())?;
    }
    Ok(ExitCode::SUCCESS)
}

fn launch_command(home: &Home, task: i64, flags: (bool, bool, bool)) -> Result<ExitCode> {
    let (resume, fresh, crash) = flags;
    let mode = match (resume, fresh) {
        (true, _) => StartMode::Resumed { crash },
        (false, true) => StartMode::Fresh { crash },
        (false, false) => StartMode::New,
    };
    let code = headwork::ops::launch::launch(home, TaskId(task), mode)?;
    let code = code.and_then(|code| u8::try_from(code).ok()).unwrap_or(1);
    Ok(ExitCode::from(code))
}

fn doctor_command(
    claude_dir: Option<PathBuf>,
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let home = Home::locate()?;
    let claude_dir = claude_dir
        .or_else(|| std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from))
        .unwrap_or_else(|| home.expand("~/.claude"));
    let exe = std::env::current_exe().context("finding the headwork program")?;
    let report = doctor(&Doctor {
        home: &home,
        exe: &exe,
        claude_dir: &claude_dir,
        probe: &SystemProbe,
        disks: &SystemDisks,
        host: host_for,
    });
    if json {
        json_out(out, &report)?;
    } else {
        out.write_all(render::doctored(&report).as_bytes())?;
    }
    Ok(if report.healthy() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn clean_command(
    store: &mut Store,
    ctx: &Ctx<'_>,
    options: CleanOptions,
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let actor = Identity::from_env().actor();
    let report = ctx.with_actor(actor, |env| clean(store, env, options))?;
    if json {
        json_out(out, &report)?;
    } else {
        out.write_all(render::cleaned(&report).as_bytes())?;
    }
    Ok(if report.failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn reap_command(
    store: &mut Store,
    ctx: &Ctx<'_>,
    options: ReapOptions,
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let actor = Identity::from_env().actor();
    let report = ctx.with_actor(actor, |env| reap_processes(store, env, options))?;
    if json {
        json_out(out, &report)?;
    } else {
        out.write_all(render::reaped(&report).as_bytes())?;
    }
    Ok(if report.failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn shadow_command(
    store: &Store,
    ctx: &Ctx<'_>,
    since: Option<&str>,
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let report = shadow_report(store, &ctx.config.hooks, since)?;
    if json {
        json_out(out, &report)?;
    } else {
        out.write_all(render::shadowed(&report).as_bytes())?;
    }
    Ok(ExitCode::SUCCESS)
}

fn orchestrator_command(home: &Home, start: &OrchestratorStart) -> Result<ExitCode> {
    let code = launch_orchestrator(home, start)?;
    let code = code.and_then(|code| u8::try_from(code).ok()).unwrap_or(1);
    Ok(ExitCode::from(code))
}
