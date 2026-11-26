//! `headwork`: the command line of the headwork orchestrator.
//!
//! `board` and `log` read the store; `spawn` creates a task and runs the queue; `launch` is what a task's pane runs.
//! All read `$HEADWORK_HOME` (default `~/.headwork`).

mod render;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use headwork::home::Home;
use headwork::ops::answer::answer;
use headwork::ops::clean::{CleanOptions, clean};
use headwork::ops::close::{CloseReport, close, close_forced, close_own, finish_after, reap};
use headwork::ops::doctor::{Doctor, SystemProbe, doctor};
use headwork::ops::effort;
use headwork::ops::graph::{GraphQuery, Which};
use headwork::ops::hook::{Git, HookEnv, HookEvent, parse_input, run_hook, scope_for};
use headwork::ops::import::{Sources, config_section, import, missing_projects};
use headwork::ops::install::{Stamp, install};
use headwork::ops::launch::{
    OrchestratorOrigin, OrchestratorStart, StartMode, exited, launch_orchestrator,
};
use headwork::ops::mcp::{Identity, Server, serve_lines};
use headwork::ops::park::{ParkOptions, park};
use headwork::ops::prs::refresh_dependencies;
use headwork::ops::reap::{ReapOptions, reap as reap_processes};
use headwork::ops::relay::Relay;
use headwork::ops::reload;
use headwork::ops::resume::{self, ResumeOptions, TaskOutcome, resume};
use headwork::ops::shadow::shadow_report;
use headwork::ops::spawn::{Outcome, Pumped, SpawnRequest, pump};
use headwork::ops::tick::{TickEnv, tick};
use headwork::ops::tick_lock::{self, Acquired};
use headwork::ops::update::{
    LOCK_WAIT, SystemBuilds, SystemClock, ThisBuild, UpdateEnv, UpdateLock, UpdateOptions, after,
    update,
};
use headwork::ops::usage::{self, UsageOptions};
use headwork::ops::waiting::LiveSlots;
use headwork::ops::{Env, host_for};
use headwork_adapters::disk::SystemDisks;
use headwork_adapters::git::Gh;
use headwork_adapters::release::Curl;
use headwork_core::guard::{ToolCall, check_tool};
use headwork_core::watch::Limits;
use headwork_core::{
    Actor, CloseMode, Config, Decision, Effort, FindingId, Model, PermissionMode, Platform,
    ProjectName, Role, Slug, TaskRequest,
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
    /// One line per top-level task, rolled up from its parts: working, needs you, in review, finished, failed.
    Status {
        /// Include closed tasks.
        #[arg(long)]
        all: bool,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// The agents working together as a graph: the orchestrator, tasks, leads and parts, subagents, helpers, jobs,
    /// pull requests, reviews and asks, with who spawned, called, messaged, reported to and asked whom.
    Graph {
        /// Draw the graph as it stood then: an RFC 3339 date and time, a date `YYYY-MM-DD`, or a span back from now
        /// such as `90m`, `24h`, `7d`.
        #[arg(long)]
        at: Option<String>,
        /// Include closed and dropped tasks.
        #[arg(long)]
        all: bool,
        /// Only this task with its lead and parts, as `<project>-<slug>`.
        #[arg(long, conflicts_with = "all")]
        task: Option<String>,
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
    /// Show what waits on the user: asks, to-do items due, migration notes, pull requests, standing findings.
    Digest {
        /// Print the digest as JSON.
        #[arg(long)]
        json: bool,
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
        #[arg(long, default_value = headwork::ops::update::DEFAULT_DIR)]
        dir: String,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Install headwork's newest release from GitHub, smoke-tested against a copy of the store first; one update runs
    /// at a time.
    Update {
        /// Print the decision and change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Ask GitHub once a minute, for up to this many minutes, until a release newer than the installed build appears.
        #[arg(long, value_name = "MINUTES", conflicts_with = "to")]
        wait: Option<u64>,
        /// Install this release (such as v0.1.7), newer or older; refused when it knows an older store schema.
        #[arg(long, value_name = "TAG", value_parser = parse_tag)]
        to: Option<headwork_core::update::Version>,
        /// The folder holding the installed `headwork`.
        #[arg(long, default_value = headwork::ops::update::DEFAULT_DIR)]
        dir: String,
        /// Finish an update as the newly installed build: the version line of the build it replaced.
        #[arg(long, hide = true, value_name = "OLD VERSION", conflicts_with_all = ["dry_run", "wait", "to"])]
        after: Option<String>,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Act on review flows: the graphs of reviewers a review runs (`docs/review-flows.md`).
    Flow {
        #[command(subcommand)]
        action: FlowAction,
    },
    /// Migration notes: what a change asks of this machine, read from each repository's default branch on GitHub
    /// (`docs/migrations.md`).
    Migrate {
        #[command(subcommand)]
        action: MigrateAction,
    },
    /// The to-do list kept in the store and exported to `todo.md` (`docs/todo.md`): list, add, done, reopen, edit, import,
    /// export.
    Todo(TodoArgs),
    /// The effective config generated from the shared, machine and local layers (`docs/config-sync.md`).
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Serve the agent graph as a web page on loopback, read only, until stopped (docs/dashboard.md).
    Dash {
        /// The port to listen on.
        #[arg(long, default_value_t = headwork::ops::dash::DEFAULT_PORT)]
        port: u16,
        /// The address to bind; loopback only.
        #[arg(long, default_value = "127.0.0.1")]
        bind: std::net::IpAddr,
        /// Open the page in the default browser.
        #[arg(long)]
        open: bool,
        /// Keep trying to take the port from the server this one replaces after an update.
        #[arg(long, hide = true)]
        takeover: bool,
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
    /// Tokens spent by every Claude session, read from Claude Code's transcripts: one line per group, a column per token
    /// type. Reads the transcripts changed since the last read first.
    Usage {
        /// Group by: project, task, kind, model, effort, role, agent, folder, day, session or delegation (main session against
        /// subagents per role).
        #[arg(long, default_value = "task")]
        by: String,
        /// Only responses at or after this time: an RFC 3339 date and time, a date `YYYY-MM-DD`, or a span back from now
        /// such as `24h`, `7d`.
        #[arg(long)]
        since: Option<String>,
        /// Only this task and its parts, as `<project>-<slug>` or `<project>/<slug>`.
        #[arg(long)]
        task: Option<String>,
        /// Break sessions headwork did not start down like the others, instead of one `unmanaged` line.
        #[arg(long)]
        unmanaged: bool,
        /// Read every transcript on disk from its start (safe to repeat: each response is stored once).
        #[arg(long)]
        backfill: bool,
        /// Report what is stored without reading the transcripts first.
        #[arg(long, conflicts_with = "backfill")]
        no_read: bool,
        /// List what transcripts cannot show, so the totals are not read as complete.
        #[arg(long)]
        gaps: bool,
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
        /// The agent definition; `orchestrator`, or `root` when only `~/.claude/agents/root.md` exists.
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

/// What `flow` does.
#[derive(Debug, Subcommand)]
enum FlowAction {
    /// Validate every flow, or one: ids unique, the graph acyclic, prompts present, models allowed, levels 1 to 3
    /// defined. Exits 1 on a problem.
    Check {
        /// One flow's name; every flow when absent.
        name: Option<String>,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
}

/// How `migrate mark` records a note.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum SettledArg {
    /// Done by hand.
    Applied,
    /// Left out on purpose.
    Skipped,
}

impl From<SettledArg> for headwork_core::migrate::Settled {
    fn from(arg: SettledArg) -> Self {
        match arg {
            SettledArg::Applied => Self::Applied,
            SettledArg::Skipped => Self::Skipped,
        }
    }
}

/// The arguments of `todo`: a subcommand, or the filters of `todo list` when there is none.
#[derive(Debug, clap::Args)]
struct TodoArgs {
    #[command(subcommand)]
    action: Option<TodoAction>,
    #[command(flatten)]
    list: TodoListArgs,
}

/// What `todo list` shows.
#[derive(Debug, Clone, clap::Args)]
struct TodoListArgs {
    /// Only this project.
    #[arg(long)]
    project: Option<String>,
    /// Only items for this owner: `user` or `agent`.
    #[arg(long)]
    owner: Option<headwork_core::todo::TodoOwner>,
    /// `open` (the default), `done`, `dropped` or `all`.
    #[arg(long, default_value = "open")]
    state: String,
    /// Print JSON instead of text.
    #[arg(long)]
    json: bool,
}

/// What `todo` does.
#[derive(Debug, Subcommand)]
enum TodoAction {
    /// List items, open ones by default.
    List(TodoListArgs),
    /// Add an item, last.
    Add {
        /// The area heading, such as `me-sim` or `Setup`.
        project: String,
        /// The item text; starting with `For you` makes it the user's.
        text: String,
        /// A subheading under the project.
        #[arg(long)]
        section: Option<String>,
        /// Who acts: `user` or `agent`; the user when the text starts with `For you`.
        #[arg(long)]
        owner: Option<headwork_core::todo::TodoOwner>,
        /// The first day it may start, `YYYY-MM-DD`.
        #[arg(long, value_name = "DATE")]
        not_before: Option<headwork_core::todo::TodoDate>,
        /// The day it is due, `YYYY-MM-DD`.
        #[arg(long, value_name = "DATE")]
        due: Option<headwork_core::todo::TodoDate>,
        /// A sub-bullet under the item; repeatable.
        #[arg(long = "note", value_name = "NOTE")]
        notes: Vec<String>,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Mark an open item done, or dropped with `--drop`, today.
    Done {
        /// The item's id.
        id: i64,
        /// What was done, or why it was dropped.
        #[arg(long)]
        note: Option<String>,
        /// Drop the item instead of finishing it; a dropped item is kept in the store and not exported.
        #[arg(long)]
        drop: bool,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Take a done or dropped item back to open.
    Reopen {
        /// The item's id.
        id: i64,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Change an item.
    Edit {
        /// The item's id.
        id: i64,
        /// The new text.
        #[arg(long)]
        text: Option<String>,
        /// The new project.
        #[arg(long)]
        project: Option<String>,
        /// The new subheading; `none` takes the item out of its subheading.
        #[arg(long)]
        section: Option<String>,
        /// The new owner: `user` or `agent`.
        #[arg(long)]
        owner: Option<headwork_core::todo::TodoOwner>,
        /// The new first day, `YYYY-MM-DD`, or `none` to clear it.
        #[arg(long, value_name = "DATE|none")]
        not_before: Option<String>,
        /// The new due day, `YYYY-MM-DD`, or `none` to clear it.
        #[arg(long, value_name = "DATE|none")]
        due: Option<String>,
        /// Add a sub-bullet.
        #[arg(long, value_name = "NOTE")]
        append: Option<String>,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Read a `todo.md` and add the items the store lacks (matched by text); never deletes, changes or rewrites anything.
    Import {
        /// The file; `[todo] file` when absent.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Report what would be added and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Write the store's items to `todo.md` now, when the content changed since the last export.
    Export {
        /// The file; `[todo] file` when absent.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
}

/// What `migrate` does.
#[derive(Debug, Subcommand)]
enum MigrateAction {
    /// The notes of every configured repository: pending, applied, skipped, not applicable.
    List {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
        /// Read the refs fetched before; touch no network.
        #[arg(long)]
        no_fetch: bool,
    },
    /// One note in full.
    Show {
        /// `<repo>/<id>`, or the id when only one repository has it.
        note: String,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Run a note: its check, then its steps in order. Stops at the first confirm step (exit 3) or user step (exit 4)
    /// and prints what to ask; exit 1 when a step fails.
    Apply {
        /// `<repo>/<id>`, or the id when only one repository has it.
        note: String,
        /// Go on from this step (0 is the pull before the steps), skipping the check.
        #[arg(long)]
        from: Option<usize>,
        /// The user agreed to the step `--from` names, or did it.
        #[arg(long, requires = "from")]
        confirmed: bool,
        /// The machine's name for steps that use it, when this machine has no `sources.toml`.
        #[arg(long)]
        machine: Option<String>,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Record a note as applied or skipped without running it.
    Mark {
        /// `<repo>/<id>`, or the id when only one repository has it.
        note: String,
        /// How to record it.
        #[arg(value_enum)]
        state: SettledArg,
        /// Why.
        #[arg(long)]
        reason: String,
    },
    /// Validate the notes in `<path>/migrations/` (the working tree), for gates. Exits 1 on a problem.
    Check {
        /// The repository folder; the current folder when absent.
        path: Option<PathBuf>,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
}

/// What `config` does.
#[derive(Debug, Subcommand)]
enum ConfigAction {
    /// Merge and validate the sources without writing anything. Exits 1 on a problem.
    Check {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Pull the repository, merge the layers, validate the result and replace `config.toml` atomically.
    Sync {
        /// Do not run `git pull --ff-only` first.
        #[arg(long)]
        no_pull: bool,
        /// Validate and print the diff, write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Print the effective config, or each key with the layer it came from.
    Show {
        /// One line per key: key, layer, value.
        #[arg(long)]
        origin: bool,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Put the previous `config.toml` back, keeping the replaced one as `config.toml.rolled-back`.
    Rollback {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Split today's `config.toml` into shared, machine and local files and set up `sources.toml`.
    Adopt {
        /// This machine's name, the folder under `machines/` in the repository.
        #[arg(long)]
        machine: String,
        /// The repository folder; the Claude folder when absent.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Write the files instead of printing them.
        #[arg(long)]
        write: bool,
        /// Overwrite files that exist.
        #[arg(long)]
        force: bool,
        /// Write the machine part into the repository (`machines/<machine>/headwork.toml`) instead of keeping it in
        /// `config.machine.toml` on this machine.
        #[arg(long)]
        machine_in_repo: bool,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
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
    /// The task kind; the slug for a main-checkout task, `task` otherwise. Kind `silent` still means `--close silent`.
    #[arg(long)]
    kind: Option<String>,
    /// Whether the lead asks the user before it closes: `confirm` (the default) or `silent`. Follow the decision table in
    /// your rules.
    #[arg(long)]
    close: Option<CloseMode>,
    /// The review planned for the task, started at its first pull request or `done`: 0 none, 1 light, 2 standard, 3 strict.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u8).range(0..=3))]
    review: Option<u8>,
    /// Who chose `--close` and `--review`: `user` when the user's own words did, `judged` when the rules' table did.
    #[arg(long, value_name = "WHO")]
    decided_by: Option<String>,
    /// A one-line reason; required when `--close` or `--review` was judged.
    #[arg(long)]
    reason: Option<String>,
    /// Start only after this task is merged.
    #[arg(long)]
    after: Option<String>,
    /// A user-only command the user asked this worker to run, exactly as the user asked for it; repeatable. The
    /// worker's first action is the `Allow` ask for it, and it runs the command only after the user's tap.
    #[arg(long = "allow-ask", value_name = "COMMAND")]
    allow_ask: Vec<String>,
    /// The to-do item (its id from `headwork todo`) this task comes from: it is linked to the task and finished when the
    /// task closes.
    #[arg(long, value_name = "ID")]
    todo: Option<i64>,
    /// Print JSON instead of text.
    #[arg(long)]
    json: bool,
}

/// The arguments with `orchestrator` added when only the program name was given, so a bare `headwork` runs the orchestrator.
fn with_default_command(mut args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    if args.len() == 1 {
        args.push("orchestrator".into());
    }
    args
}

fn main() -> ExitCode {
    if let Some(code) = forward_if_launcher() {
        return code;
    }
    let matches = Cli::command()
        .version(headwork::version::text())
        .get_matches_from(with_default_command(std::env::args_os().collect()));
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
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

/// Runs as the windowless launcher when this program is `headworkw` ([`headwork::ops::windowless`]).
fn forward_if_launcher() -> Option<ExitCode> {
    use headwork::ops::windowless;
    let exe = std::env::current_exe().ok()?;
    if !windowless::is_launcher(&exe) {
        return None;
    }
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let forwarded = Home::locate().and_then(|home| windowless::forward(&exe, &args, home.root()));
    Some(forwarded.unwrap_or(ExitCode::FAILURE))
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
        Command::Update {
            dry_run,
            wait,
            to,
            dir,
            after,
            json,
        } => {
            let opts = UpdateOptions {
                dry_run,
                wait_min: wait,
                to,
            };
            return update_command(&dir, opts, after.as_deref(), json, out);
        }
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
        Command::Dash {
            port,
            bind,
            open,
            takeover,
        } => return dash_command(port, bind, open, takeover, out),
        Command::Flow {
            action: FlowAction::Check { name, json },
        } => return flow_check_command(name.as_deref(), json, out),
        Command::Migrate { action } => return migrate_command(action, out),
        Command::Config { action } => return config_command(action, out),
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
        | Command::Status { .. }
        | Command::Graph { .. }
        | Command::Log { .. }
        | Command::Spawn(_)
        | Command::Todo(_)
        | Command::Queue { .. }
        | Command::Close { .. }
        | Command::Answer { .. }
        | Command::Tick { .. }
        | Command::Jobs { .. }
        | Command::Job { .. }
        | Command::Retitle { .. }
        | Command::Recap { .. }
        | Command::Digest { .. }
        | Command::ShadowReport { .. }
        | Command::Usage { .. }
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
        Command::Status { all, json } => status_command(&mut store, &ctx, all, json, out),
        Command::Graph {
            at,
            all,
            task,
            json,
        } => {
            let which = match (task, all) {
                (Some(name), _) => Which::Task(name),
                (None, true) => Which::All,
                (None, false) => Which::Live,
            };
            graph_command(&mut store, &ctx, &GraphQuery { at, which }, json, out)
        }
        Command::Log { name, limit, json } => log(&store, &ctx, name, limit, json, out),
        Command::Spawn(args) => spawn_command(&mut store, &ctx, args, out),
        Command::Todo(args) => todo_command(&mut store, &ctx, args, out),
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
        Command::Recap { name } => recap_command(&store, ctx.config, &name, out),
        Command::Digest { json } => digest_command(&store, &ctx, json, out),
        Command::Retitle { name } => retitle_command(&store, &ctx, name.as_deref(), out),
        Command::ShadowReport { since, json } => {
            shadow_command(&store, &ctx, since.as_deref(), json, out)
        }
        Command::Usage {
            by,
            since,
            task,
            unmanaged,
            backfill,
            no_read,
            gaps,
            json,
        } => {
            let options = UsageOptions {
                by: by.parse()?,
                since,
                task,
                unmanaged,
            };
            let read = (!no_read).then_some(backfill);
            usage_command(&mut store, &ctx, &options, read, gaps, json, out)
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
        | Command::Dash { .. }
        | Command::Flow { .. }
        | Command::Config { .. }
        | Command::Migrate { .. }
        | Command::Orchestrator { .. }
        | Command::Hook { .. }
        | Command::Mcp { .. }
        | Command::Exited { .. }
        | Command::Install { .. }
        | Command::Update { .. }
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
    let claude_dir = home.claude_dir().to_path_buf();
    let opts = ResumeOptions {
        logon,
        wait_for_host: std::time::Duration::from_secs(wait),
        claude_dir,
    };
    let own_surface = resume::SURFACE_VARS
        .iter()
        .find_map(|var| std::env::var(var).ok().filter(|value| !value.is_empty()));
    if let Some(surface) = own_surface {
        let detached = standalone(home, Actor::User, |store, env| {
            if resume::would_close(store, env, &surface)? {
                resume::start_detached(store, env, logon, opts.wait_for_host).map(Some)
            } else {
                Ok(None)
            }
        })?;
        if let Some(pid) = detached {
            if json {
                json_out(out, &serde_json::json!({ "detached": pid }))?;
            } else {
                writeln!(
                    out,
                    "resume reopens the session in this pane, so it goes on detached (pid {pid}); this pane closes and \
                     `headwork board` shows the result"
                )?;
            }
            return Ok(ExitCode::SUCCESS);
        }
    }
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

fn digest_command(
    store: &Store,
    ctx: &Ctx<'_>,
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let digest = headwork::ops::digest::digest(store, ctx.config)?;
    if json {
        json_out(out, &digest)?;
    } else if digest.is_empty() {
        writeln!(out, "nothing waits on you")?;
    } else {
        out.write_all(digest.render().as_bytes())?;
    }
    Ok(ExitCode::SUCCESS)
}

fn recap_command(
    store: &Store,
    config: &Config,
    name: &str,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let task = store
        .find_task(name)?
        .with_context(|| format!("no task named `{name}`"))?;
    match store.latest_recap(task.id)? {
        Some(recap) => write!(
            out,
            "{}{} ({}, {})\n\n{}\n",
            headwork::ops::digest::section(store, config),
            task.name,
            recap.kind,
            recap.created_at,
            recap.body
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
        out.write_all(headwork::ops::digest::section(store, ctx.config).as_bytes())?;
        out.write_all(
            render::board(
                &board,
                ctx.config.machine.max_workers.get(),
                &ctx.config.usage,
            )
            .as_bytes(),
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

fn status_command(
    store: &mut Store,
    ctx: &Ctx<'_>,
    all: bool,
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    ctx.with_env(|env| headwork::ops::lost::reconcile(store, env).map(drop))?;
    let lines = headwork::ops::status::status(store, all, &ctx.config.usage)?;
    if json {
        json_out(out, &lines)?;
    } else {
        out.write_all(headwork::ops::digest::section(store, ctx.config).as_bytes())?;
        out.write_all(render::statuses(&lines).as_bytes())?;
    }
    Ok(ExitCode::SUCCESS)
}

fn dash_command(
    port: u16,
    bind: std::net::IpAddr,
    open: bool,
    takeover: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let home = Home::locate()?;
    home.open_store()?;
    let options = headwork::ops::dash::DashOptions {
        bind,
        port,
        db: home.store_path(),
        takeover,
    };
    if open {
        headwork::ops::dash::open_in_browser(&headwork::ops::dash::url(&options));
    }
    headwork::ops::dash::serve(&options, out)?;
    Ok(ExitCode::SUCCESS)
}

fn graph_command(
    store: &mut Store,
    ctx: &Ctx<'_>,
    query: &GraphQuery,
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    if query.at.is_none() {
        ctx.with_env(|env| headwork::ops::lost::reconcile(store, env).map(drop))?;
    }
    let graph = headwork::ops::graph::graph(store, query)?;
    if json {
        json_out(out, &graph)?;
    } else {
        out.write_all(render::graph(&graph).as_bytes())?;
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
    let flags = headwork::ops::flags::resolve(
        &kind,
        args.close,
        args.review,
        args.decided_by.as_deref(),
        args.reason.as_deref(),
    )?;
    let request = SpawnRequest {
        request: TaskRequest {
            project: ProjectName::new(args.project)?,
            slug: Slug::new(args.slug)?,
            kind,
            model: args.model,
            effort: args.effort,
            in_main: args.in_main,
            keep: args.keep,
            close: flags.close,
            review: flags.review,
            decision: flags.decision,
        },
        brief: &args.brief,
        depends_on,
        permission_mode: args.mode,
        allow_ask: &args.allow_ask,
    };
    let spawned = ctx.with_env(|env| {
        reap(store, env)?;
        headwork::ops::todo::spawn_linked(store, env, &request, args.todo)
    })?;
    if args.json {
        json_out(
            out,
            &serde_json::json!({
                "task": spawned.task,
                "queue": spawned.pumped,
                "todo_unlinked": spawned.unlinked,
            }),
        )?;
    } else {
        writeln!(out, "{} {}", spawned.task.name, spawned.task.state)?;
        if let Some(why) = &spawned.unlinked {
            writeln!(out, "the task runs unlinked: {why}")?;
        }
        out.write_all(render::pumped(&spawned.pumped).as_bytes())?;
    }
    Ok(exit_for(&spawned.pumped))
}

fn todo_command(
    store: &mut Store,
    ctx: &Ctx<'_>,
    args: TodoArgs,
    out: &mut impl Write,
) -> Result<ExitCode> {
    use headwork::ops::todo as list;
    match args.action.unwrap_or(TodoAction::List(args.list)) {
        TodoAction::List(filter) => todo_list_command(store, &filter, out)?,
        TodoAction::Import {
            file,
            dry_run,
            json,
        } => {
            let report = list::import(
                store,
                ctx.home,
                ctx.config,
                file.as_deref(),
                dry_run,
                Actor::User,
            )?;
            if json {
                json_out(out, &report)?;
            } else {
                out.write_all(report.text().as_bytes())?;
            }
        }
        TodoAction::Export { file, json } => {
            let done = list::export(store, ctx.home, ctx.config, file.as_deref(), Actor::User)?;
            if json {
                json_out(out, &done)?;
            } else {
                writeln!(out, "{}", done.summary())?;
            }
            if matches!(done, list::Export::NotImported { .. }) {
                return Ok(ExitCode::FAILURE);
            }
        }
        changing @ (TodoAction::Add { .. }
        | TodoAction::Done { .. }
        | TodoAction::Reopen { .. }
        | TodoAction::Edit { .. }) => todo_change_command(store, ctx, changing, out)?,
    }
    Ok(ExitCode::SUCCESS)
}

fn todo_list_command(store: &Store, filter: &TodoListArgs, out: &mut impl Write) -> Result<()> {
    let items = headwork::ops::todo::list(
        store,
        filter.project.as_deref(),
        filter.owner,
        Some(&filter.state),
    )?;
    if filter.json {
        json_out(out, &items)?;
    } else if items.is_empty() {
        writeln!(out, "no matching to-do items")?;
    } else {
        for item in &items {
            writeln!(out, "{}", item.line())?;
        }
    }
    Ok(())
}

fn todo_change_command(
    store: &mut Store,
    ctx: &Ctx<'_>,
    action: TodoAction,
    out: &mut impl Write,
) -> Result<()> {
    use headwork::ops::todo::{self as list, AddRequest};
    let (done, json) = match action {
        TodoAction::Add {
            project,
            text,
            section,
            owner,
            not_before,
            due,
            notes,
            json,
        } => {
            let request = AddRequest {
                project,
                text,
                section,
                owner,
                not_before,
                due,
                notes,
            };
            (ctx.with_env(|env| list::add(store, env, &request))?, json)
        }
        TodoAction::Done {
            id,
            note,
            drop,
            json,
        } => (
            ctx.with_env(|env| list::finish(store, env, id, note.as_deref(), drop))?,
            json,
        ),
        TodoAction::Reopen { id, json } => {
            (ctx.with_env(|env| list::reopen(store, env, id))?, json)
        }
        TodoAction::Edit {
            id,
            text,
            project,
            section,
            owner,
            not_before,
            due,
            append,
            json,
        } => {
            let edit = headwork_store::TodoEdit {
                text,
                project,
                section: section.map(|section| (section != "none").then_some(section)),
                owner,
                not_before: not_before.as_deref().map(date_or_none).transpose()?,
                due: due.as_deref().map(date_or_none).transpose()?,
                append,
                added: None,
            };
            (ctx.with_env(|env| list::edit(store, env, id, &edit))?, json)
        }
        TodoAction::List(_) | TodoAction::Import { .. } | TodoAction::Export { .. } => {
            return Ok(());
        }
    };
    if json {
        json_out(out, &done)
    } else {
        Ok(out.write_all(done.text().as_bytes())?)
    }
}

fn date_or_none(text: &str) -> Result<Option<headwork_core::todo::TodoDate>> {
    if text == "none" {
        Ok(None)
    } else {
        Ok(Some(text.parse()?))
    }
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
        refresh_dependencies(store, env, &Gh::default())?;
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
        if !report.left_to_launcher() {
            pump(store, env)?;
        }
        Ok(report)
    })?;
    if json {
        json_out(out, &report)?;
    } else {
        match &report.pending {
            None => match report
                .usage
                .as_ref()
                .filter(|usage| usage.tokens.total() > 0)
            {
                Some(usage) => writeln!(
                    out,
                    "{} {} · {} tokens",
                    report.name,
                    report.state,
                    headwork_core::usage::short_count(usage.tokens.total())
                )?,
                None => writeln!(out, "{} {}", report.name, report.state)?,
            },
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

/// Serves the session's MCP requests. `config.toml` is read again before each request, so a change (such as a new
/// `max_workers`) reaches a long-running server at its next call; a config that no longer parses leaves the last good
/// one in use.
fn mcp_command() -> Result<ExitCode> {
    let home = Home::locate()?;
    let mut config = home.load_config()?;
    let mut store = home.open_store()?;
    store
        .sync_projects(&config)
        .context("recording the configured projects")?;
    let exe = std::env::current_exe().context("finding the headwork program")?;
    let git = Gh::default();
    let identity = Identity::from_env();
    serve_lines(io::stdin().lock(), io::stdout().lock(), |message| {
        if let Ok(fresh) = home.load_config()
            && fresh != config
        {
            store
                .sync_projects(&fresh)
                .context("recording the configured projects")?;
            config = fresh;
        }
        let host = host_for(&config)?;
        let env = Env {
            home: &home,
            config: &config,
            host: host.as_ref(),
            exe: &exe,
            actor: Actor::User,
        };
        let mut server = Server::new(&mut store, env, &git, identity.clone());
        let response = server.handle(message);
        server.finish_pending()?;
        Ok(response)
    })?;
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

fn parse_tag(text: &str) -> Result<headwork_core::update::Version, String> {
    headwork_core::update::Version::from_tag(text)
        .ok_or_else(|| format!("`{text}` is not a release tag such as v0.1.7"))
}

fn update_command(
    dir: &str,
    opts: UpdateOptions,
    after_from: Option<&str>,
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let home = Home::locate()?;
    let config = home.load_config()?;
    let install_dir = home.expand(dir);
    let releases = Curl::discovered();
    let asset = headwork_adapters::release::asset_name()?;
    let host = host_for(&config).ok();
    let env = UpdateEnv {
        home: &home,
        config: &config,
        releases: &releases,
        builds: &SystemBuilds,
        disks: &SystemDisks,
        clock: &SystemClock,
        install_dir: &install_dir,
        asset,
        actor: Actor::User,
        host: host.as_deref(),
    };
    if let Some(from) = after_from {
        let text = headwork::version::text();
        let this = ThisBuild {
            text: &text,
            release: headwork::version::release().and_then(headwork_core::update::Version::parse),
            commit: headwork::version::commit(),
        };
        let report = after(&env, from, this)?;
        if json {
            json_out(out, &report)?;
        } else {
            writeln!(out, "{report}")?;
        }
        return Ok(ExitCode::SUCCESS);
    }
    let report = update(&env, opts)?;
    if json {
        json_out(out, &report)?;
    } else {
        writeln!(out, "{report}")?;
    }
    Ok(if report.failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn install_command(dir: &str, json: bool, out: &mut impl Write) -> Result<ExitCode> {
    let home = Home::locate()?;
    let source = std::env::current_exe().context("finding the headwork program")?;
    let Some(_lock) = UpdateLock::take_waiting(&home, LOCK_WAIT)? else {
        bail!(
            "an update is running ({} stayed taken for {} s); try again when it is done",
            home.root().join(headwork::ops::update::LOCK_FILE).display(),
            LOCK_WAIT.as_secs()
        );
    };
    let report = install(&source, &home.expand(dir), verify_installed)?;
    let config = headwork::ops::config_sync::after_install(
        &home,
        &home
            .expand(dir)
            .join(headwork::ops::install::program_name()),
        Identity::from_env().actor(),
    );
    let (sessions, backup) = if home.root().join("state.db").exists() {
        let store = home.open_store()?;
        let sessions = reload::classify(
            &store.live_sessions_with_event(reload::EVENT_BUILD)?,
            |exe| Stamp::of(exe).ok(),
        );
        (sessions, store.backup().map(ToString::to_string))
    } else {
        (Vec::new(), None)
    };
    if json {
        json_out(
            out,
            &serde_json::json!({ "install": report, "sessions": sessions, "backup": backup, "config": config }),
        )?;
    } else {
        out.write_all(render::installed(&report, &sessions).as_bytes())?;
        if let Some(backup) = backup {
            writeln!(out, "{backup}")?;
        }
        if let Some(config) = config {
            writeln!(out, "{config}")?;
        }
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
    let slots = LiveSlots {
        home: &home,
        exe: &exe,
    };
    let env = HookEnv {
        home: &home,
        config: &config,
        host: host.as_ref(),
        git: &Git,
        exe: &exe,
        temp: &temp,
        slots: &slots,
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
    let slots = LiveSlots {
        home: &home,
        exe: &exe,
    };
    let env = HookEnv {
        home: &home,
        config: &config,
        host: host.as_ref(),
        git: &Git,
        exe: &exe,
        temp: &temp,
        slots: &slots,
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
    let now = tick_lock::now_s();
    let _lock = match tick_lock::acquire(ctx.home.root(), now)? {
        Acquired::Held(lock) => lock,
        Acquired::Busy(stamp) => {
            if let Some(stamp) = stamp.filter(|stamp| stamp.hung(now)) {
                let held_min = now.saturating_sub(stamp.started_s) / 60;
                let detail = serde_json::json!({ "pid": stamp.pid, "held_min": held_min });
                store.raise_finding(
                    "tick-hung",
                    FindingId::TickHung,
                    None,
                    &detail,
                    Actor::Tick,
                )?;
                writeln!(
                    out,
                    "tick-hung: the tick of process {} has held tick.lock for {held_min} min",
                    stamp.pid
                )?;
                return Ok(ExitCode::FAILURE);
            }
            writeln!(out, "another tick is running")?;
            return Ok(ExitCode::SUCCESS);
        }
    };
    let git = Gh::default();
    let disks = SystemDisks;
    let temp = std::env::temp_dir();
    let updates = headwork::ops::update::Detached {
        home: ctx.home,
        exe: ctx.exe,
    };
    let slots = LiveSlots {
        home: ctx.home,
        exe: ctx.exe,
    };
    let report = ctx.with_actor(Actor::Tick, |env| {
        tick(
            store,
            &TickEnv {
                env,
                git: &git,
                disks: &disks,
                temp: &temp,
                limits: Limits::from_config(&ctx.config.watch),
                updates: &updates,
                slots: &slots,
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
    use headwork::ops::doctor::Probe;
    use headwork::ops::launchd::{self, Agent, SystemLaunchctl};
    use headwork::ops::schedule::{self, Schtasks};
    let platform = Platform::current();
    if let Some(reason) = platform.no_scheduler() {
        bail!("{reason}");
    }
    let home = Home::locate()?;
    let user_home = home.expand("~");
    let windows = platform == Platform::Windows;
    let report = if install {
        let installed = home.expand(&format!(
            "~/.local/bin/headwork{}",
            std::env::consts::EXE_SUFFIX
        ));
        let current = std::env::current_exe().context("finding the headwork program")?;
        let exe = schedule::program(&installed, &current);
        if windows {
            let launcher = headwork::ops::windowless::write_launcher(&exe)?;
            schedule::install(&Schtasks, &launcher)?
        } else {
            let mut programs = vec![exe.clone()];
            programs.extend(
                launchd::TICK_PROGRAMS
                    .iter()
                    .filter_map(|program| SystemProbe.which(program)),
            );
            programs.push(PathBuf::from(headwork_adapters::host::cmux::BUNDLED_CLI));
            let agent = Agent::new(
                &exe,
                &user_home,
                home.root(),
                launchd::agent_path(&programs),
            );
            launchd::install(&SystemLaunchctl, &agent)?
        }
    } else if windows {
        schedule::uninstall(&Schtasks)?
    } else {
        launchd::uninstall(&SystemLaunchctl, &launchd::plist_path(&user_home))?
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

fn flow_check_command(name: Option<&str>, json: bool, out: &mut impl Write) -> Result<ExitCode> {
    let home = Home::locate()?;
    let config = home.load_config()?;
    let mut reports = headwork::ops::flows::check_all(home.root(), &config.models.allowed);
    if let Some(name) = name {
        reports.retain(|report| report.name == name);
        if reports.is_empty() {
            anyhow::bail!("no flow `{name}`");
        }
    }
    if json {
        json_out(out, &reports)?;
    } else {
        out.write_all(render::flows_checked(&reports).as_bytes())?;
    }
    Ok(
        if reports.iter().all(headwork::ops::flows::FlowReport::ok) {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        },
    )
}

fn config_command(action: ConfigAction, out: &mut impl Write) -> Result<ExitCode> {
    use headwork::ops::config_sync::{self as sync, AdoptOptions, Logging, SyncOptions};
    let home = Home::locate()?;
    let platform = Platform::current();
    let actor = Identity::from_env().actor();
    let exit = |ok: bool| {
        if ok {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    };
    match action {
        ConfigAction::Check { json } => {
            let report = sync::check(&home, platform);
            if json {
                json_out(out, &report)?;
            } else {
                out.write_all(report.to_string().as_bytes())?;
            }
            Ok(exit(report.ok))
        }
        ConfigAction::Sync {
            no_pull,
            dry_run,
            json,
        } => {
            let exe = std::env::current_exe().context("finding the headwork program")?;
            let store = home.open_store()?;
            let options = SyncOptions {
                pull: !no_pull,
                dry_run,
            };
            let log = Logging {
                store: Some(&store),
                actor,
            };
            let report = sync::sync(&home, &exe, options, platform, log)?;
            if report.ok && !dry_run {
                drop(headwork::ops::migrate::announce(
                    &home,
                    &home.load_config()?,
                ));
            }
            if json {
                json_out(out, &report)?;
            } else {
                out.write_all(report.to_string().as_bytes())?;
            }
            Ok(exit(report.ok))
        }
        ConfigAction::Show { origin, json } => {
            require_synced(&home)?;
            config_show(&home, origin, json, out)?;
            Ok(ExitCode::SUCCESS)
        }
        ConfigAction::Rollback { json } => {
            require_synced(&home)?;
            let store = home.open_store()?;
            let log = Logging {
                store: Some(&store),
                actor,
            };
            let report = sync::rollback(&home, log)?;
            if json {
                json_out(out, &report)?;
            } else {
                writeln!(
                    out,
                    "restored config.toml.prev as {}; the replaced file is {}",
                    report.restored.display(),
                    report.kept_as.display()
                )?;
            }
            Ok(ExitCode::SUCCESS)
        }
        ConfigAction::Adopt {
            machine,
            repo,
            write,
            force,
            machine_in_repo,
            json,
        } => {
            let options = AdoptOptions {
                machine,
                repo,
                write,
                force,
                machine_in_repo,
            };
            let report = sync::adopt(&home, &options, platform)?;
            if json {
                json_out(out, &report)?;
            } else {
                out.write_all(report.to_string().as_bytes())?;
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn config_show(home: &Home, origin: bool, json: bool, out: &mut impl Write) -> Result<()> {
    if origin {
        let keys = headwork::ops::config_sync::show_origins(home)?;
        if json {
            json_out(out, &keys)?;
        } else {
            for key in keys {
                writeln!(out, "{}\t{}\t{}", key.key, key.layer, key.value)?;
            }
        }
    } else {
        let path = home.config_path();
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        if json {
            json_out(out, &serde_json::json!({ "config": text }))?;
        } else {
            out.write_all(text.as_bytes())?;
        }
    }
    Ok(())
}

fn require_synced(home: &Home) -> Result<()> {
    if home.sources_path().exists() {
        Ok(())
    } else {
        bail!(
            "not synced: {} does not exist; `headwork config adopt --machine <name>` sets it up",
            home.sources_path().display()
        )
    }
}

fn doctor_command(
    claude_dir: Option<PathBuf>,
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let home = Home::locate()?;
    let claude_dir = claude_dir.unwrap_or_else(|| home.claude_dir().to_path_buf());
    let exe = std::env::current_exe().context("finding the headwork program")?;
    let report = doctor(&Doctor {
        home: &home,
        exe: &exe,
        claude_dir: &claude_dir,
        probe: &SystemProbe,
        disks: &SystemDisks,
        host: host_for,
        platform: Platform::current(),
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

fn usage_command(
    store: &mut Store,
    ctx: &Ctx<'_>,
    options: &UsageOptions,
    read: Option<bool>,
    gaps: bool,
    json: bool,
    out: &mut impl Write,
) -> Result<ExitCode> {
    let ingested = read
        .map(|from_start| {
            let files = usage::transcripts(ctx.home.claude_dir());
            usage::ingest(store, &files, from_start)
        })
        .transpose()?;
    let mut report = usage::report(store, options, &ctx.config.usage)?;
    report.ingest = ingested;
    if json {
        let mut value = serde_json::to_value(&report)?;
        if gaps {
            value["gaps"] = serde_json::json!(usage::GAPS);
        }
        json_out(out, &value)?;
    } else {
        out.write_all(render::usage(&report).as_bytes())?;
        if gaps {
            out.write_all(render::usage_gaps(usage::GAPS).as_bytes())?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn orchestrator_command(home: &Home, start: &OrchestratorStart) -> Result<ExitCode> {
    let code = launch_orchestrator(home, start)?;
    let code = code.and_then(|code| u8::try_from(code).ok()).unwrap_or(1);
    Ok(ExitCode::from(code))
}

struct Notes {
    home: Home,
    config: Config,
    store: Store,
    actor: Actor,
}

impl Notes {
    fn open() -> Result<Self> {
        let home = Home::locate()?;
        let config = home.load_config()?;
        let store = home.open_store()?;
        Ok(Self {
            home,
            config,
            store,
            actor: Identity::from_env().actor(),
        })
    }
}

fn migrate_command(action: MigrateAction, out: &mut impl Write) -> Result<ExitCode> {
    use headwork::ops::migrate::{self as notes, ApplyOptions};
    match action {
        MigrateAction::Check { path, json } => {
            let checked = notes::check(&path.unwrap_or_else(|| PathBuf::from(".")))?;
            if json {
                json_out(out, &checked)?;
            } else {
                write!(out, "{checked}")?;
            }
            Ok(if checked.problems.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        MigrateAction::List { json, no_fetch } => {
            let n = Notes::open()?;
            let report = notes::report(&n.store, &n.home, &n.config, !no_fetch)?;
            if json {
                json_out(out, &report)?;
            } else {
                write!(out, "{report}")?;
            }
            Ok(ExitCode::SUCCESS)
        }
        MigrateAction::Show { note, json } => {
            let n = Notes::open()?;
            let report = notes::report(&n.store, &n.home, &n.config, false)?;
            let entry = report.find(&note)?;
            if json {
                json_out(out, entry)?;
            } else {
                write!(
                    out,
                    "{}",
                    notes::describe(entry, &n.store, &n.home, &n.config)
                )?;
            }
            Ok(ExitCode::SUCCESS)
        }
        MigrateAction::Apply {
            note,
            from,
            confirmed,
            machine,
            json,
        } => {
            let mut n = Notes::open()?;
            let options = ApplyOptions {
                machine,
                from,
                confirmed,
            };
            let outcome = notes::apply(&mut n.store, &n.home, &n.config, &note, &options, n.actor)?;
            if json {
                json_out(out, &outcome)?;
            } else {
                write!(out, "{outcome}")?;
            }
            Ok(ExitCode::from(outcome.exit_code()))
        }
        MigrateAction::Mark {
            note,
            state,
            reason,
        } => {
            let mut n = Notes::open()?;
            let said = notes::mark(
                &mut n.store,
                &n.home,
                &n.config,
                &note,
                state.into(),
                &reason,
                n.actor,
            )?;
            writeln!(out, "{said}")?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

#[cfg(test)]
mod default_command_tests {
    use super::with_default_command;

    #[test]
    fn bare_invocation_runs_the_orchestrator() {
        let args = with_default_command(vec!["headwork".into()]);
        assert_eq!(
            args,
            ["headwork", "orchestrator"],
            "bare headwork gets the orchestrator command"
        );
    }

    #[test]
    fn explicit_arguments_are_untouched() {
        let args = with_default_command(vec!["headwork".into(), "board".into()]);
        assert_eq!(args, ["headwork", "board"], "an explicit command stays");
    }
}
