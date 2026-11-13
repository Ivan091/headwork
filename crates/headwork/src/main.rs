//! `headwork`: the command line of the headwork orchestrator.
//!
//! Milestone 1 offers the read side: `board` shows projects, tasks, sessions, states and asks; `log` shows the event
//! log. Both read `$HEADWORK_HOME` (default `~/.headwork`).

mod home;
mod render;

use std::io::{self, Write};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use headwork_store::EventFilter;

use crate::home::Home;

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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut out = io::stdout().lock();
    match run(cli, &mut out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let mut err = io::stderr().lock();
            if writeln!(err, "headwork: {error:#}").is_err() {
                return ExitCode::from(2);
            }
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli, out: &mut impl Write) -> Result<()> {
    let home = Home::locate()?;
    let config = home.load_config()?;
    let mut store = home.open_store()?;
    store
        .sync_projects(&config)
        .context("recording the configured projects")?;
    match cli.command {
        Command::Board { all, json } => {
            let board = store.board(all)?;
            if json {
                serde_json::to_writer_pretty(&mut *out, &board)?;
                writeln!(out)?;
            } else {
                out.write_all(render::board(&board, config.machine.max_children.get()).as_bytes())?;
            }
        }
        Command::Log { name, limit, json } => {
            let task = match name {
                Some(name) => match store.find_task(&name)? {
                    Some(task) => Some(task.id),
                    None => bail!("no task named `{name}`"),
                },
                None => None,
            };
            let events = store.events(EventFilter { task, limit })?;
            if json {
                serde_json::to_writer_pretty(&mut *out, &events)?;
                writeln!(out)?;
            } else {
                out.write_all(render::events(&events).as_bytes())?;
            }
        }
    }
    Ok(())
}
