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
use headwork::ops::close::{CloseReport, close, finish_after, reap};
use headwork::ops::import::{Sources, config_section, import, missing_projects};
use headwork::ops::spawn::{Outcome, Pumped, SpawnRequest, pump, spawn};
use headwork::ops::{Env, host_for};
use headwork_core::{Actor, Config, Model, PermissionMode, ProjectName, Slug, TaskRequest};
use headwork_store::{EventFilter, Store, TaskId};

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
    /// Close a task: its session, pane, process tree and worktree, verified before it is marked closed.
    Close {
        /// A task, as `<project>-<slug>` or `<project>/<slug>`.
        name: String,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Adopt the children, projects and worktrees of the orchestrate scripts.
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
    /// Run a task's session: the program each pane runs.
    Launch {
        /// The task's id.
        #[arg(long)]
        task: i64,
        /// The headwork home; panes do not inherit the caller's environment.
        #[arg(long)]
        home: Option<PathBuf>,
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
    /// The permission mode.
    #[arg(long, default_value = "auto")]
    mode: PermissionMode,
    /// The task kind; the slug for a main-checkout task, `task` otherwise.
    #[arg(long)]
    kind: Option<String>,
    /// Start only after this task is merged.
    #[arg(long)]
    after: Option<String>,
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

fn run(cli: Cli, out: &mut impl Write) -> Result<ExitCode> {
    let command = match cli.command {
        Command::Launch { task, home } => {
            let home = Home::at(home)?;
            let code = headwork::ops::launch::launch(&home, TaskId(task))?;
            let code = code.and_then(|code| u8::try_from(code).ok()).unwrap_or(1);
            return Ok(ExitCode::from(code));
        }
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
        | Command::Close { .. }) => other,
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
        Command::Board { all, json } => board(&store, &config, all, json, out),
        Command::Log { name, limit, json } => log(&store, name, limit, json, out),
        Command::Spawn(args) => spawn_command(&mut store, &ctx, args, out),
        Command::Queue { order, json } => queue_command(&mut store, &ctx, &order, json, out),
        Command::Close { name, json } => close_command(&mut store, &ctx, &name, json, out),
        Command::Launch { .. } | Command::Finish { .. } | Command::Import { .. } => {
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// What every command that opens panes needs, besides the store.
struct Ctx<'a> {
    home: &'a Home,
    config: &'a Config,
    exe: &'a Path,
}

impl Ctx<'_> {
    fn with_env<T>(&self, act: impl FnOnce(&Env<'_>) -> Result<T>) -> Result<T> {
        let host = host_for(self.config)?;
        act(&Env {
            home: self.home,
            config: self.config,
            host: host.as_ref(),
            exe: self.exe,
            actor: Actor::User,
        })
    }
}

fn json_out(out: &mut impl Write, value: &impl serde::Serialize) -> Result<()> {
    serde_json::to_writer_pretty(&mut *out, value)?;
    writeln!(out)?;
    Ok(())
}

fn board(
    store: &Store,
    config: &Config,
    all: bool,
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let board = store.board(all)?;
    if json {
        json_out(out, &board)?;
    } else {
        out.write_all(render::board(&board, config.machine.max_children.get()).as_bytes())?;
    }
    Ok(ExitCode::SUCCESS)
}

fn log(
    store: &Store,
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
    let events = store.events(EventFilter { task, limit })?;
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
            in_main: args.in_main,
            keep: args.keep,
        },
        brief: &args.brief,
        depends_on,
        permission_mode: args.mode,
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
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let task = store
        .find_task(name)?
        .with_context(|| format!("no task named `{name}`"))?;
    let own_session = std::env::var(headwork_adapters::runtime::ENV_SESSION).ok();
    let report: CloseReport = ctx.with_env(|env| {
        let report = close(store, env, task.id, own_session.as_deref())?;
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
