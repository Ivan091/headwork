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
use headwork::ops::Env;
use headwork::ops::spawn::{Outcome, Pumped, SpawnRequest, pump, spawn};
use headwork_adapters::host::wmux::{Cli as WmuxCli, Wmux};
use headwork_core::config::HostKind;
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
    /// Start every queued task that fits.
    Queue {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
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

fn host(config: &Config) -> Result<Wmux<WmuxCli>> {
    match config.machine.host {
        HostKind::Wmux => Ok(Wmux::new(WmuxCli::locate(
            config.machine.host_bin.as_deref().map(Path::new),
        ))),
        HostKind::Cmux => bail!("the cmux host arrives in milestone 8"),
    }
}

fn run(cli: Cli, out: &mut impl Write) -> Result<ExitCode> {
    if let Command::Launch { task, home } = cli.command {
        let home = Home::at(home)?;
        let code = headwork::ops::launch::launch(&home, TaskId(task))?;
        let code = code.and_then(|code| u8::try_from(code).ok()).unwrap_or(1);
        return Ok(ExitCode::from(code));
    }
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
    match cli.command {
        Command::Board { all, json } => board(&store, &config, all, json, out),
        Command::Log { name, limit, json } => log(&store, name, limit, json, out),
        Command::Spawn(args) => spawn_command(&mut store, &ctx, args, out),
        Command::Queue { json } => queue_command(&mut store, &ctx, json, out),
        Command::Launch { .. } => Ok(ExitCode::SUCCESS),
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
        let host = host(self.config)?;
        act(&Env {
            home: self.home,
            config: self.config,
            host: &host,
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
    let (task, pumped) = ctx.with_env(|env| spawn(store, env, &request))?;
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
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let pumped = ctx.with_env(|env| pump(store, env))?;
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
