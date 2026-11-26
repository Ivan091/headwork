//! The MCP server (design section 3.3): JSON-RPC over stdio, newline-delimited, offering the orchestrator's or a worker's tools
//! by the identity the launcher put in the environment.
//!
//! The role is read once and checked again on every call. A session without an identity is unmanaged: it sees no
//! tools and every call is refused. A worker's tools also need the session row of a task, so a forged role alone gets
//! nothing.

use std::io::{BufRead, Write};

use anyhow::{Context, Result, anyhow, bail};
use headwork_adapters::git::GitHost;
use headwork_core::lifecycle::Trigger;
use headwork_core::{
    Actor, Effort, MachineMode, Model, PermissionMode, ProjectName, RecapKind, Role, Slug,
    TaskRequest, TaskShape, TaskState, user_go,
};
use headwork_store::{Board, NewPr, NewRecap, Store, StoreError, Task, TaskId};
use serde_json::{Value, json};

use super::close::{begin_own_close, close, close_dropped_part, close_forced, end_closing};
use super::jobs::{self, Registration};
use super::review_loop;
use super::rules;
use super::spawn::{SpawnRequest, pump, spawn_shaped};
use super::{Env, close::reap};

/// The protocol revision offered when the client names none.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// The statuses a `report` may carry.
pub const STATUSES: [&str; 4] = ["progress", "review", "blocked", "done"];

/// Who is calling: the environment the launcher set.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Identity {
    /// The Claude session id.
    pub session: Option<String>,
    /// The role it claims.
    pub role: Option<Role>,
}

impl Identity {
    /// Reads `HEADWORK_SESSION` and `HEADWORK_ROLE`; a missing or unknown role leaves the session unmanaged.
    #[must_use]
    pub fn from_env() -> Self {
        let session = std::env::var(headwork_adapters::runtime::ENV_SESSION).ok();
        let role = std::env::var(headwork_adapters::runtime::ENV_ROLE)
            .ok()
            .and_then(|role| role.parse().ok());
        Self { session, role }
    }

    /// Who acts for this identity in the event log: the orchestrator, a worker, or else the user.
    #[must_use]
    pub const fn actor(&self) -> Actor {
        match self.role {
            Some(Role::Orchestrator) => Actor::Orchestrator,
            Some(Role::Worker) => Actor::Worker,
            None => Actor::User,
        }
    }

    fn managed(&self) -> Option<(&str, Role)> {
        self.session.as_deref().zip(self.role)
    }
}

struct ToolDef {
    name: &'static str,
    description: &'static str,
    roles: &'static [Role],
    schema: fn() -> Value,
}

const BOTH: &[Role] = &[Role::Orchestrator, Role::Worker];
const ORCHESTRATOR: &[Role] = &[Role::Orchestrator];
const WORKER: &[Role] = &[Role::Worker];

/// Tool names accepted for one more release and no longer listed, each with the tool that replaced it.
const DEPRECATED_TOOL_NAMES: &[(&str, &str)] = &[
    ("todo_list", "backlog_list"),
    ("todo_add", "backlog_add"),
    ("todo_done", "backlog_done"),
    ("todo_edit", "backlog_edit"),
];

/// The worker tools only a lead's session sees and may call.
const LEAD_TOOLS: &[&str] = &["part_spawn", "part_drop", "review_start", "review_resolve"];

/// The worker tools only a reviewer lead (a part of kind `review`) sees and may call.
const REVIEWER_TOOLS: &[&str] = &[
    "review_next",
    "review_node_done",
    "review_deliver",
    "review_verdict",
    "review_finish",
];

const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "board",
        description: "Tasks by state, queue order, pull requests, slots and machine mode.",
        roles: BOTH,
        schema: || {
            object(
                json!({ "all": { "type": "boolean", "description": "Include closed tasks." } }),
                &[],
            )
        },
    },
    ToolDef {
        name: "status",
        description: concat!(
            "One line per top-level task, rolled up from its parts: working, needs you, in review <PR URLs>, finished, ",
            "failed. What you tell the user unless asked for more.",
        ),
        roles: ORCHESTRATOR,
        schema: || {
            object(
                json!({ "all": { "type": "boolean", "description": "Include closed tasks." } }),
                &[],
            )
        },
    },
    ToolDef {
        name: "usage",
        description: concat!(
            "Tokens spent by every Claude session, by type (input, cache writes, cache reads, output), grouped by task, ",
            "project, kind, model, effort, role, agent, folder, day or session. Reads the transcripts changed since the ",
            "last read first.",
        ),
        roles: ORCHESTRATOR,
        schema: || {
            object(
                json!({
                    "by": {
                        "type": "string",
                        "enum": ["task", "project", "kind", "model", "effort", "role", "agent", "folder", "day", "session"],
                        "description": "The grouping; task by default."
                    },
                    "since": { "type": "string", "description": "A span back from now (24h, 7d), a date or an RFC 3339 time." },
                    "task": { "type": "string", "description": "Only this task and its parts, as <project>-<slug>." },
                    "unmanaged": {
                        "type": "boolean",
                        "description": "Break sessions headwork did not start down instead of one unmanaged line."
                    }
                }),
                &[],
            )
        },
    },
    ToolDef {
        name: "inbox",
        description: "Everything waiting on the user, oldest first, each with the worker's name.",
        roles: BOTH,
        schema: || object(json!({}), &[]),
    },
    ToolDef {
        name: "spawn",
        description: "Create a task from a brief and start it in its own pane when a slot is free; otherwise it queues.",
        roles: ORCHESTRATOR,
        schema: || {
            object(
                json!({
                    "project": { "type": "string" },
                    "slug": { "type": "string", "description": "kebab-case, unique among live tasks" },
                    "brief": { "type": "string", "description": "The brief's text; it must stand alone." },
                    "model": { "type": "string", "enum": ["haiku", "sonnet", "opus", "fable"] },
                    "effort": { "type": "string", "enum": ["low", "medium", "high", "xhigh", "max"] },
                    "kind": {
                        "type": "string",
                        "description": "The task kind: it picks flow, grants, main-only. Kind `silent` still means `close: silent`."
                    },
                    "close": {
                        "type": "string",
                        "enum": ["confirm", "silent"],
                        "description": "Whether the lead asks the user before closing. Follow the decision table in your rules."
                    },
                    "review": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 3,
                        "description": "Planned review: 0 none, 1 light, 2 standard, 3 strict. Follow the decision table in your rules."
                    },
                    "decided_by": {
                        "type": "string",
                        "enum": ["user", "judged"],
                        "description": "`user`: the user's words set it; `judged`: the table did. Needed with either."
                    },
                    "reason": {
                        "type": "string",
                        "description": "One line: why these values. Required when `decided_by` is `judged`."
                    },
                    "keep": { "type": "boolean", "description": "A standing role that does not close when idle." },
                    "in_main": { "type": "boolean", "description": "Run in the project's main checkout." },
                    "depends_on": { "type": "string", "description": "Name of a task that must be merged first." },
                    "backlog": {
                        "type": "integer",
                        "description": concat!(
                            "The id of the open backlog item this task comes from (`backlog_list`): it is linked to the task and ",
                            "finished when the task closes. Refused before the task starts when the item is not open or a live task holds ",
                            "it; a link refused after the task started leaves it running unlinked and the result says why ",
                            "(`backlog_unlinked`).",
                        ),
                    },
                    "allow_ask": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": concat!(
                            "User-only commands the user explicitly asked this worker to run, each exactly as the user ",
                            "asked. The worker's first action is the `Allow` ask for them; it runs a command only after ",
                            "the user's tap; more than four become several asks. The kind must be main-only. A brief may ",
                            "forbid any user-only command without this (\"never run `x`\").",
                        ),
                    },
                }),
                &["project", "slug", "brief"],
            )
        },
    },
    ToolDef {
        name: "drop",
        description: "Drop a task and close it; the user's words are recorded in the event log.",
        roles: ORCHESTRATOR,
        schema: || {
            object(
                json!({ "name": { "type": "string" }, "user_words": { "type": "string" } }),
                &["name", "user_words"],
            )
        },
    },
    ToolDef {
        name: "close",
        description: concat!(
            "Close a task without dropping it: a keep task, or one stuck after done. `force` finishes a task stuck in ",
            "closing whose pull requests are all merged, or a dropped one with none, and that has no live job: a worktree ",
            "the remove command refuses is then kept on disk instead of blocking the close. It is refused for any other task.",
        ),
        roles: ORCHESTRATOR,
        schema: || {
            object(
                json!({
                    "name": { "type": "string" },
                    "force": {
                        "type": "boolean",
                        "description": concat!(
                            "Finish a task stuck in closing whose pull requests are all merged, or a dropped one with none, ",
                            "and that has no live job.",
                        ),
                    },
                }),
                &["name"],
            )
        },
    },
    ToolDef {
        name: "answer",
        description: concat!(
            "Pass on the user's answer to an ask the tick raised (the inbox lists it with its id and options, and no ",
            "session's dialog takes it), such as keeping or deleting a closing task's worktree. Ask the user first with ",
            "AskUserQuestion, offering the ask's own options, and pass on only the option the user picked: never choose ",
            "for the user, least of all `Delete`. `choice` is the option's number from 1 or its whole text. The answer ",
            "is recorded as yours and carried out at once.",
        ),
        roles: ORCHESTRATOR,
        schema: || {
            object(
                json!({
                    "ask": { "type": "integer", "description": "The ask's id. Omit it when exactly one such ask waits." },
                    "choice": { "type": "string", "description": "The option the user picked: its number from 1 or its text." },
                }),
                &["choice"],
            )
        },
    },
    ToolDef {
        name: "queue",
        description: "Put these queued tasks first, in this order, and start every queued task that fits.",
        roles: ORCHESTRATOR,
        schema: || {
            object(
                json!({ "order": { "type": "array", "items": { "type": "string" } } }),
                &[],
            )
        },
    },
    ToolDef {
        name: "clean",
        description: concat!(
            "Remove the worktrees of merged and closed tasks (through each project's own remove command, never a dirty ",
            "one) and Claude Code agent worktrees whose agent is gone and whose work landed; list the ones left and why. ",
            "`gc` also runs plain `git gc` in each main checkout where no main-only job or task runs. Use it for cleanup ",
            "instead of handing deletions to the user.",
        ),
        roles: ORCHESTRATOR,
        schema: || {
            object(
                json!({
                    "dry_run": { "type": "boolean", "description": "Report the plan and change nothing." },
                    "gc": { "type": "boolean", "description": "Also run `git gc` where the main checkout is quiet." },
                }),
                &[],
            )
        },
    },
    ToolDef {
        name: "reap",
        description: concat!(
            "End what ended sessions left running (the processes their launcher and claude started) and orphaned tail ",
            "and bash idle for over 10 minutes; never claude, me-studio, headwork or a registered job, nor what they ",
            "started. Lists what it ended and what it left. Use it for orphan processes instead of handing kills to the user.",
        ),
        roles: ORCHESTRATOR,
        schema: || {
            object(
                json!({ "dry_run": { "type": "boolean", "description": "Report what would be ended and end nothing." } }),
                &[],
            )
        },
    },
    ToolDef {
        name: "update",
        description: concat!(
            "Install headwork's newest release (or the release `to` names, newer or older) through `headwork update`: ",
            "a detached run that checks the download, smoke-tests it on a copy of the store, installs it and hands over ",
            "to the new build; this returns at once with its pid, and the event log and `headwork doctor` show how it ",
            "ended. `dry_run` answers here with the decision and changes nothing. Updates normally start by themselves ",
            "after a headwork task closes; use this when the user asks, or for a rollback with `to`.",
        ),
        roles: ORCHESTRATOR,
        schema: || {
            object(
                json!({
                    "to": { "type": "string", "description": "A release tag such as v0.1.7: install it, newer or older." },
                    "dry_run": { "type": "boolean", "description": "Say what an update would do and change nothing." },
                }),
                &[],
            )
        },
    },
    ToolDef {
        name: "config_sync",
        description: concat!(
            "Regenerate this machine's config.toml from the shared, machine and local layers (`headwork config sync`): ",
            "pull the config repository (`pull`, default true), merge, validate on a scratch home and replace the file ",
            "atomically. A failure leaves config.toml as it was and says which file and key. `dry_run` validates and ",
            "shows the diff and writes nothing. Use it after a config change merged in the repository.",
        ),
        roles: ORCHESTRATOR,
        schema: || {
            object(
                json!({
                    "pull": { "type": "boolean", "description": "Run `git pull --ff-only` in the repository first; default true." },
                    "dry_run": { "type": "boolean", "description": "Validate and report the diff, write nothing." },
                }),
                &[],
            )
        },
    },
    ToolDef {
        name: "migrate_list",
        description: concat!(
            "The migration notes of this machine's repositories (`headwork migrate list`): pending, applied, skipped and not ",
            "applicable, read from each repository's default branch. `fetch` (default false) fetches first. A pending note is ",
            "run with `headwork migrate apply <repo>/<id>` in a main-folder task; the orchestrator asks the user for its ",
            "confirm and user steps.",
        ),
        roles: BOTH,
        schema: || {
            object(
                json!({ "fetch": { "type": "boolean", "description": "Fetch each repository first; default false." } }),
                &[],
            )
        },
    },
    ToolDef {
        name: "migrate_mark",
        description: "Record a migration note as applied or skipped without running it, with the reason (`headwork migrate mark`).",
        roles: ORCHESTRATOR,
        schema: || {
            object(
                json!({
                    "note": { "type": "string", "description": "<repo>/<id>, or the id when only one repository has it." },
                    "state": { "type": "string", "enum": ["applied", "skipped"] },
                    "reason": { "type": "string" },
                }),
                &["note", "state", "reason"],
            )
        },
    },
    ToolDef {
        name: "backlog_list",
        description: concat!(
            "The backlog list (kept in the store, exported to `backlog.md`): one line per item with its id, project, text, owner, ",
            "dates and the task spawned for it. Open items by default.",
        ),
        roles: ORCHESTRATOR,
        schema: || {
            object(
                json!({
                    "project": { "type": "string" },
                    "owner": { "type": "string", "enum": ["user", "agent"] },
                    "state": { "type": "string", "enum": ["open", "done", "dropped", "all"], "description": "open by default." },
                }),
                &[],
            )
        },
    },
    ToolDef {
        name: "backlog_add",
        description: concat!(
            "Add an item to the backlog list, last, and export `backlog.md`. Never edit `backlog.md` by hand. Text that starts with ",
            "`For you` is the user's. Commit and push the regenerated file afterwards.",
        ),
        roles: ORCHESTRATOR,
        schema: || {
            object(
                json!({
                    "project": { "type": "string", "description": "The area heading: me-sim, headwork, me-studio, Setup, ..." },
                    "text": { "type": "string" },
                    "section": { "type": "string", "description": "A subheading under the project." },
                    "owner": { "type": "string", "enum": ["user", "agent"], "description": "The user for `For you` text." },
                    "not_before": { "type": "string", "description": "YYYY-MM-DD: do not start before." },
                    "due": { "type": "string", "description": "YYYY-MM-DD." },
                    "notes": { "type": "array", "items": { "type": "string" }, "description": "Sub-bullets." },
                }),
                &["project", "text"],
            )
        },
    },
    ToolDef {
        name: "backlog_done",
        description: "Mark an open backlog item done, or dropped with `drop`, today, with a note, and export `backlog.md`.",
        roles: ORCHESTRATOR,
        schema: || {
            object(
                json!({
                    "id": { "type": "integer" },
                    "note": { "type": "string", "description": "What was done, or why it was dropped." },
                    "drop": { "type": "boolean", "description": "Drop the item instead of finishing it." },
                }),
                &["id"],
            )
        },
    },
    ToolDef {
        name: "backlog_edit",
        description: "Change a backlog item (text, project, section, owner, dates, a note to append) and export `backlog.md`.",
        roles: ORCHESTRATOR,
        schema: || {
            object(
                json!({
                    "id": { "type": "integer" },
                    "text": { "type": "string" },
                    "project": { "type": "string" },
                    "section": { "type": "string", "description": "`none` takes the item out of its section." },
                    "owner": { "type": "string", "enum": ["user", "agent"] },
                    "not_before": { "type": "string", "description": "YYYY-MM-DD, or `none` to clear it." },
                    "due": { "type": "string", "description": "YYYY-MM-DD, or `none` to clear it." },
                    "append": { "type": "string", "description": "A note line to add." },
                }),
                &["id"],
            )
        },
    },
    ToolDef {
        name: "rules",
        description: "The rules added at run time that are still active.",
        roles: ORCHESTRATOR,
        schema: || object(json!({}), &[]),
    },
    ToolDef {
        name: "rule_add",
        description: "Add a soft rule for `global`, `orchestrator`, `worker` or a project; every session gets it on its next prompt.",
        roles: ORCHESTRATOR,
        schema: || {
            object(
                json!({ "scope": { "type": "string" }, "text": { "type": "string" } }),
                &["scope", "text"],
            )
        },
    },
    ToolDef {
        name: "rule_retire",
        description: "Retire a rule added with rule_add; every session gets the change on its next prompt.",
        roles: ORCHESTRATOR,
        schema: || object(json!({ "id": { "type": "integer" } }), &["id"]),
    },
    ToolDef {
        name: "report",
        description: "Report progress. `blocked` and `done` let you end a turn with the task unfinished; `review` with a \
                      pull request moves the task to in_review.",
        roles: WORKER,
        schema: || {
            object(
                json!({
                    "status": { "type": "string", "enum": STATUSES },
                    "summary": { "type": "string" },
                    "links": { "type": "array", "items": { "type": "string" } },
                    "pr": {
                        "type": "string",
                        "description": "The pull request URL, https://<host>/<owner>/<repo>/pull/<n> on github.com or a GitHub \
                                        Enterprise host; for status review.",
                    },
                }),
                &["status", "summary"],
            )
        },
    },
    ToolDef {
        name: "done",
        description: "Finish the task through the checked close: a merged pull request moves it through in_review and \
                      merged, then this session, its pane and its worktree close. Refused, with the task left as it \
                      was, while a pull request of the task is open, a question waits on the user, a job runs, or \
                      your report has not reached the orchestrator by SendMessage.",
        roles: WORKER,
        schema: || {
            object(
                json!({ "summary": { "type": "string", "description": "What was built, test results, pull request links." } }),
                &[],
            )
        },
    },
    ToolDef {
        name: "recap",
        description: "Write down everything a new session needs to carry on without this conversation: what was done \
                      and what is left, the decisions you took and why, open questions and what waits on the user, the \
                      exact next step, files, branches and pull requests, what runs and how to start it again, and the \
                      user's standing constraints. A park asks for it before a restart; also write one when you stop \
                      for the day. After a restart a resume opens a new session with an empty context that starts from \
                      this recap.",
        roles: BOTH,
        schema: || {
            object(
                json!({ "text": {
                    "type": "string",
                    "description": "Done, decisions and why, open questions, next step, files, what runs, standing wishes."
                } }),
                &["text"],
            )
        },
    },
    ToolDef {
        name: "job_register",
        description: "Register a long-running process you started (a training run, a render) so a park can stop it \
                      cleanly and a resume can start it again. Policy `checkpoint` waits for a fresh checkpoint file \
                      before stopping, `rerun` stops it at once and runs it again, `finish` is left to run.",
        roles: WORKER,
        schema: || {
            object(
                json!({
                    "label": { "type": "string", "description": "A few words naming the job." },
                    "argv": { "type": "array", "items": { "type": "string" }, "description": "The command line that starts it again." },
                    "pid": { "type": "integer", "description": "The process id it runs as now." },
                    "cwd": { "type": "string", "description": "The folder it runs in; your folder when absent." },
                    "park_policy": { "type": "string", "enum": ["checkpoint", "rerun", "finish"] },
                    "checkpoint_path": { "type": "string", "description": "The file a checkpoint job rewrites." },
                    "restart": { "type": "boolean", "description": "Start it again after a restart; true when absent." },
                    "heavy": { "type": "boolean" },
                }),
                &["label", "argv"],
            )
        },
    },
    ToolDef {
        name: "job_start",
        description: "Start a long-running process (a training run, a render) detached from this session and register \
                      it, so it survives your turn and a park can stop it cleanly and a resume can start it again. \
                      Use this instead of a background shell. Policies as for job_register.",
        roles: WORKER,
        schema: || {
            object(
                json!({
                    "label": { "type": "string", "description": "A few words naming the job." },
                    "argv": { "type": "array", "items": { "type": "string" }, "description": "The command line, program first." },
                    "cwd": { "type": "string", "description": "The folder to run in; your folder when absent." },
                    "park_policy": { "type": "string", "enum": ["checkpoint", "rerun", "finish"] },
                    "checkpoint_path": { "type": "string", "description": "The file a checkpoint job rewrites." },
                    "restart": { "type": "boolean", "description": "Start it again after a restart; true when absent." },
                    "heavy": { "type": "boolean" },
                }),
                &["label", "argv"],
            )
        },
    },
    ToolDef {
        name: "helper_tab",
        description: "Open a short-lived helper agent session (a separate program shown in wmux, such as `claude -p ...`) as a 
                      new tab in your own workspace, never a new workspace or a split. It closes with your workspace. 
                      In-process subagents (the Agent tool) need no tab.",
        roles: WORKER,
        schema: || {
            object(
                json!({
                    "argv": { "type": "array", "items": { "type": "string" }, "description": "The command line, program first." },
                    "cwd": { "type": "string", "description": "The folder to run in; your folder when absent." },
                }),
                &["argv"],
            )
        },
    },
    ToolDef {
        name: "worktree_add",
        description: "Make a helper worktree for one part of your task, beside your own, with the project's worktree \
                      command: its slug is also its branch. It is writable for you, and headwork removes it when your \
                      task closes (one that still holds work stays for the user). Use it for parts you or your \
                      subagents build separately; never make worktrees by hand. Calling it again with the same slug \
                      returns the same one.",
        roles: WORKER,
        schema: || {
            object(
                json!({
                    "slug": { "type": "string", "description": "A kebab-case name for the part, such as `press-shapes`." },
                }),
                &["slug"],
            )
        },
    },
    ToolDef {
        name: "part_spawn",
        description: "A lead's tool: start a part, a session of its own for one piece of your task, as a tab in your \
                      workspace. Its task is `<your slug>-<slug>` in your project. Parts are limited only by the machine and \
                      project slots (and `[leads] max_parts` when the user sets it); one that does not fit waits in the queue right \
                      behind you. Fable needs the user's approval.",
        roles: WORKER,
        schema: || {
            object(
                json!({
                    "slug": { "type": "string", "description": "The part's own kebab-case word, such as `review`." },
                    "brief": { "type": "string", "description": "The part's brief; it must stand alone." },
                    "model": {
                        "type": "string",
                        "enum": ["haiku", "sonnet", "opus", "fable"],
                        "description": "Omit for Sonnet; your own model is never passed down. Opus needs `reason`.",
                    },
                    "reason": {
                        "type": "string",
                        "description": concat!(
                            "Why this part needs Opus (new architecture, publishing or ledger code, security, ",
                            "concurrency, audit, plan); recorded."
                        ),
                    },
                    "after": {
                        "type": "string",
                        "description": "Another part of yours (its word or full name) whose pull request must merge first.",
                    },
                    "project": {
                        "type": "string",
                        "description": "Only your own project; another project's work goes to the orchestrator as a new task.",
                    },
                    "close": {
                        "type": "string",
                        "enum": ["confirm", "silent"],
                        "description": "Whether the lead asks the user before closing. Follow the decision table in your rules."
                    },
                    "review": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 3,
                        "description": "Planned review: 0 none, 1 light, 2 standard, 3 strict. Follow the decision table in your rules."
                    },
                    "decided_by": {
                        "type": "string",
                        "enum": ["user", "judged"],
                        "description": "`user`: the user's words set it; `judged`: the table did. Needed with either."
                    },
                    "flag_reason": {
                        "type": "string",
                        "description": "One line: why these `close` and `review` values (`reason` is for Opus). Needed when judged."
                    },
                    "allow_ask": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": concat!(
                            "User-only commands the user explicitly asked this part to run, each exactly as asked; only ",
                            "commands the user allowed you, its lead. The part's first action is the `Allow` ask for them: ",
                            "the user taps again in its tab. Its brief may name only those user-only commands too, and ",
                            "a part that names one runs in the main checkout under your kind, so only while you run there.",
                        ),
                    },
                }),
                &["slug", "brief"],
            )
        },
    },
    ToolDef {
        name: "review_start",
        description: "A lead's tool: start the review the user asked for with `review <level>` in your tab. It pins \
                      your HEAD (commit everything first), makes a read-only worktree for it and starts the reviewer \
                      lead as your part `review`. Refused when the user asked for none.",
        roles: WORKER,
        schema: || object(json!({}), &[]),
    },
    ToolDef {
        name: "review_resolve",
        description: "A lead's tool: answer every finding the reviewer lead sent you, after committing the fixes. Each \
                      item is the finding's number, `fixed` (note: the commit) or `disputed` (note: the reason; at \
                      level 2 and 3 with evidence). It pins your new HEAD for the re-check.",
        roles: WORKER,
        schema: || {
            object(
                json!({
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "integer" },
                                "action": { "type": "string", "enum": ["fixed", "disputed"] },
                                "note": { "type": "string" },
                            },
                            "required": ["id", "action", "note"],
                        },
                    },
                }),
                &["items"],
            )
        },
    },
    ToolDef {
        name: "review_next",
        description: "A reviewer lead's tool: the nodes of your review that may start now, each with its model and the \
                      exact prompt for its fresh subagent; they are marked running. Empty while running nodes hold \
                      the rest back; `next` says what to do.",
        roles: WORKER,
        schema: || object(json!({}), &[]),
    },
    ToolDef {
        name: "review_node_done",
        description: "A reviewer lead's tool: hand back one node's result. A plain node gives `findings` (a list of \
                      {file, line, severity, summary, scenario}; empty when it found nothing); a node that runs once \
                      per finding gives `item` (the finding's number) and `verdict`; a subagent that failed gives \
                      `failed` with why.",
        roles: WORKER,
        schema: || {
            object(
                json!({
                    "node": { "type": "string" },
                    "item": { "type": "integer" },
                    "findings": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "file": { "type": ["string", "null"] },
                                "line": { "type": ["integer", "null"] },
                                "severity": { "type": "string", "enum": ["important", "nit", "doubt"] },
                                "summary": { "type": "string" },
                                "scenario": { "type": "string" },
                            },
                            "required": ["severity", "summary", "scenario"],
                        },
                    },
                    "verdict": { "type": "string", "enum": ["confirmed", "plausible", "rejected"] },
                    "failed": { "type": "string" },
                }),
                &["node"],
            )
        },
    },
    ToolDef {
        name: "review_deliver",
        description: "A reviewer lead's tool: once every node of the round is over, mark the findings that go on as \
                      delivered and get them numbered, with the implementing lead's address to send them to.",
        roles: WORKER,
        schema: || object(json!({}), &[]),
    },
    ToolDef {
        name: "review_verdict",
        description: "A reviewer lead's tool: record your re-check of every fixed or disputed finding, each from a \
                      fresh subagent: `fix` accepted or rejected, or `dispute` accepted or kept, with a note. headwork \
                      applies the level's dispute rule and says what comes next.",
        roles: WORKER,
        schema: || {
            object(
                json!({
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "integer" },
                                "fix": { "type": "string", "enum": ["accepted", "rejected"] },
                                "dispute": { "type": "string", "enum": ["accepted", "kept"] },
                                "note": { "type": "string" },
                            },
                            "required": ["id"],
                        },
                    },
                }),
                &["items"],
            )
        },
    },
    ToolDef {
        name: "review_finish",
        description: "A reviewer lead's tool: finish the review once nothing is left to fix, re-check, ask or run; \
                      then tell the implementing lead the outcome and close yourself with `done`.",
        roles: WORKER,
        schema: || object(json!({}), &[]),
    },
    ToolDef {
        name: "part_drop",
        description: "A lead's tool: drop one of your parts and close it.",
        roles: WORKER,
        schema: || {
            object(
                json!({
                    "name": { "type": "string", "description": "The part's word or full task name." },
                    "reason": { "type": "string", "description": "Why, for the event log." },
                }),
                &["name"],
            )
        },
    },
    ToolDef {
        name: "job_status",
        description: "The registered jobs that are not over: your own for a worker, all for the orchestrator.",
        roles: BOTH,
        schema: || object(json!({}), &[]),
    },
    ToolDef {
        name: "job_stop",
        description: "Stop one of your registered jobs now and record it as stopped.",
        roles: WORKER,
        schema: || object(json!({ "id": { "type": "integer" } }), &["id"]),
    },
    ToolDef {
        name: "job_forget",
        description: "Mark a registered job over without touching any process: for a job whose process ended unseen or \
                      whose pid another program now holds. A worker forgets its own jobs, the orchestrator any.",
        roles: BOTH,
        schema: || {
            object(
                json!({
                    "id": { "type": "integer" },
                    "reason": { "type": "string", "description": "Why the job is over; it goes into the log." },
                }),
                &["id", "reason"],
            )
        },
    },
    ToolDef {
        name: "park",
        description: "Park the machine before a restart: every session writes a recap, jobs stop at a checkpoint, the \
                      panes close. It runs in the background; read `board` for the machine mode. Write your own \
                      recap first, in the same turn: the park takes it as yours; without one it asks you for it.",
        roles: ORCHESTRATOR,
        schema: || {
            object(
                json!({
                    "deadline_s": {
                        "type": "integer",
                        "description": "Seconds to wait for recaps and checkpoints; 300 when absent.",
                    },
                }),
                &[],
            )
        },
    },
    ToolDef {
        name: "resume",
        description: "Bring parked sessions back after a restart and restart jobs. It runs in the background and \
                      writes a summary file.",
        roles: ORCHESTRATOR,
        schema: || object(json!({}), &[]),
    },
];

#[expect(
    clippy::needless_pass_by_value,
    reason = "call sites build the properties inline with json!"
)]
fn object(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required })
}

/// The server: a store, the environment commands act in, and who is calling.
#[derive(Debug)]
pub struct Server<'a> {
    store: &'a mut Store,
    env: Env<'a>,
    git: &'a dyn GitHost,
    identity: Identity,
    closing: Option<Task>,
}

/// Reads newline-delimited JSON-RPC requests and writes each response `respond` gives, flushed. A line that is not
/// JSON gets a parse error.
///
/// # Errors
///
/// Fails when reading or writing fails, or when `respond` fails.
pub fn serve_lines(
    input: impl BufRead,
    mut output: impl Write,
    mut respond: impl FnMut(&Value) -> Result<Option<Value>>,
) -> Result<()> {
    for line in input.lines() {
        let line = line.context("reading a request")?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(message) => respond(&message)?,
            Err(error) => Some(failure(
                &Value::Null,
                -32700,
                &format!("parse error: {error}"),
            )),
        };
        if let Some(response) = response {
            writeln!(output, "{response}").context("writing a response")?;
            output.flush().context("flushing a response")?;
        }
    }
    Ok(())
}

impl<'a> Server<'a> {
    /// A server for one session. `env.actor` is replaced by the role's actor.
    #[must_use]
    pub const fn new(
        store: &'a mut Store,
        mut env: Env<'a>,
        git: &'a dyn GitHost,
        identity: Identity,
    ) -> Self {
        env.actor = identity.actor();
        Self {
            store,
            env,
            git,
            identity,
            closing: None,
        }
    }

    /// Reads requests line by line from `input` and writes each response as one line to `output`, until `input` ends.
    /// A `done` call closes the session only after its response is written.
    ///
    /// # Errors
    ///
    /// Fails when reading or writing fails.
    pub fn serve(&mut self, input: impl BufRead, output: impl Write) -> Result<()> {
        serve_lines(input, output, |message| {
            let response = self.handle(message);
            self.finish_pending()?;
            Ok(response)
        })
    }

    /// Answers one JSON-RPC message; notifications get no answer.
    pub fn handle(&mut self, message: &Value) -> Option<Value> {
        let id = message.get("id").cloned()?;
        let method = message.get("method").and_then(Value::as_str)?;
        let params = message.get("params").unwrap_or(&Value::Null);
        Some(match method {
            "initialize" => success(&id, &self.initialize(params)),
            "ping" => success(&id, &json!({})),
            "tools/list" => success(&id, &json!({ "tools": self.listed() })),
            "tools/call" => success(&id, &self.call(params)),
            other => failure(&id, -32601, &format!("method not found: {other}")),
        })
    }

    /// Closes the task a `done` call finished, now that its response is out. A no-op otherwise.
    ///
    /// # Errors
    ///
    /// Fails when the close fails.
    pub fn finish_pending(&mut self) -> Result<()> {
        let Some(task) = self.closing.take() else {
            return Ok(());
        };
        let report = end_closing(
            self.store,
            &self.env,
            task.id,
            self.identity.session.as_deref(),
        )?;
        if !report.left_to_launcher() {
            pump(self.store, &self.env)?;
        }
        Ok(())
    }

    #[expect(clippy::unused_self, reason = "kept beside the other handlers")]
    fn initialize(&self, params: &Value) -> Value {
        let version = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(PROTOCOL_VERSION);
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": true } },
            "serverInfo": { "name": "headwork", "version": env!("CARGO_PKG_VERSION") },
        })
    }

    fn listed(&self) -> Vec<Value> {
        let Some((_, role)) = self.identity.managed() else {
            return Vec::new();
        };
        let lead = self.is_lead();
        let reviewer = self.is_reviewer();
        TOOLS
            .iter()
            .filter(|tool| tool.roles.contains(&role))
            .filter(|tool| lead || !LEAD_TOOLS.contains(&tool.name))
            .filter(|tool| reviewer || !REVIEWER_TOOLS.contains(&tool.name))
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": (tool.schema)(),
                })
            })
            .collect()
    }

    fn call(&mut self, params: &Value) -> Value {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let name = DEPRECATED_TOOL_NAMES
            .iter()
            .find(|(old, _)| *old == name)
            .map_or(name, |(_, current)| *current);
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        match self.dispatch(name, &args) {
            Ok(text) => json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
            Err(error) => {
                json!({ "content": [{ "type": "text", "text": format!("{error:#}") }], "isError": true })
            }
        }
    }

    fn dispatch(&mut self, name: &str, args: &Value) -> Result<String> {
        let Some((session, role)) = self.identity.managed() else {
            bail!("this session is not managed by headwork: its tools are unavailable");
        };
        let session = session.to_owned();
        let tool = TOOLS
            .iter()
            .find(|tool| tool.name == name)
            .ok_or_else(|| anyhow!("unknown tool `{name}`"))?;
        if !tool.roles.contains(&role) {
            bail!(
                "`{name}` is a tool of the {} role; you are the {role}",
                tool.roles[0]
            );
        }
        if LEAD_TOOLS.contains(&name) && !self.is_lead() {
            bail!("`{name}` is a lead's tool; your task is not a lead");
        }
        if REVIEWER_TOOLS.contains(&name) && !self.is_reviewer() {
            bail!("`{name}` is a reviewer lead's tool; your task is not a review");
        }
        match name {
            "board" => self.board(&session, role, args),
            "inbox" => self.inbox(&session, role),
            "status" => self.status(args),
            "usage" => self.usage(args),
            "part_spawn" | "part_drop" | "review_start" | "review_resolve" => {
                self.lead_tool(name, &session, args)
            }
            "review_next" | "review_node_done" | "review_deliver" | "review_verdict"
            | "review_finish" => self.reviewer_tool(name, &session, args),
            "spawn" => self.spawn(args),
            "drop" => self.drop_task(args),
            "close" => self.close_task(args),
            "answer" => self.answer(args),
            "queue" => self.queue(args),
            "clean" => self.clean(args),
            "reap" => {
                let options = super::reap::ReapOptions {
                    dry_run: flag(args, "dry_run"),
                };
                to_text(&super::reap::reap(self.store, &self.env, options)?)
            }
            "rules" => to_text(&self.store.active_rules()?),
            "rule_add" => self.rule_add(args),
            "rule_retire" => self.rule_retire(args),
            "report" => {
                let task = self.own_task(&session)?;
                self.report(&session, &task, args)
            }
            "done" => {
                let task = self.own_task(&session)?;
                self.done(&session, task, args)
            }
            "recap" => {
                let task = match role {
                    Role::Orchestrator => None,
                    Role::Worker => Some(self.own_task(&session)?),
                };
                self.recap(&session, task.as_ref(), args)
            }
            "job_register" => {
                let task = self.own_task(&session)?;
                self.job_register(&session, &task, args)
            }
            "job_start" => {
                let task = self.own_task(&session)?;
                let registration = registration(args)?;
                to_text(&jobs::start(
                    self.store,
                    &self.env,
                    &task,
                    &session,
                    &registration,
                )?)
            }
            "helper_tab" => self.helper_tab(&session, args),
            "worktree_add" => {
                let task = self.own_task(&session)?;
                let slug = text(args, "slug")?;
                to_text(&super::helpers::add(self.store, &self.env, &task, &slug)?)
            }
            "job_status" => self.job_status(&session, role),
            "job_stop" => {
                let task = self.own_task(&session)?;
                self.job_stop(&task, args)
            }
            "job_forget" => self.job_forget(&session, role, args),
            "config_sync" => self.config_sync(args),
            "update" => self.update(args),
            "migrate_list" => self.migrate_list(args),
            "migrate_mark" => self.migrate_mark(args),
            "backlog_list" => self.backlog_list(args),
            "backlog_add" => self.backlog_add(args),
            "backlog_done" => self.backlog_done(args),
            "backlog_edit" => self.backlog_edit(args),
            "park" => self.start_detached("park", args),
            "resume" => self.start_detached("resume", args),
            other => bail!("unknown tool `{other}`"),
        }
    }

    fn clean(&mut self, args: &Value) -> Result<String> {
        let options = super::clean::CleanOptions {
            dry_run: flag(args, "dry_run"),
            gc: flag(args, "gc"),
        };
        to_text(&super::clean::clean(self.store, &self.env, options)?)
    }

    fn lead_tool(&mut self, name: &str, session: &str, args: &Value) -> Result<String> {
        let lead = self.own_task(session)?;
        match name {
            "part_spawn" => self.part_spawn(&lead, args),
            "part_drop" => self.part_drop(&lead, args),
            "review_start" => to_text(&super::review::start(self.store, &self.env, &lead)?),
            "review_resolve" => {
                let items: Vec<review_loop::Resolve> = items(args)?;
                to_text(&review_loop::resolve(self.store, &self.env, &lead, &items)?)
            }
            other => bail!("unknown tool `{other}`"),
        }
    }

    fn reviewer_tool(&mut self, name: &str, session: &str, args: &Value) -> Result<String> {
        let part = self.own_task(session)?;
        match name {
            "review_next" => to_text(&review_loop::next(self.store, &self.env, &part)?),
            "review_node_done" => {
                let done: review_loop::NodeDone =
                    serde_json::from_value(args.clone()).map_err(|error| {
                        anyhow!("the node's result does not have the expected shape: {error}")
                    })?;
                Ok(review_loop::node_done(self.store, &self.env, &part, &done)?)
            }
            "review_deliver" => to_text(&review_loop::deliver(self.store, &self.env, &part)?),
            "review_verdict" => {
                let items: Vec<review_loop::Verdict> = items(args)?;
                to_text(&review_loop::verdict(self.store, &self.env, &part, &items)?)
            }
            "review_finish" => to_text(&review_loop::finish(self.store, &self.env, &part)?),
            other => bail!("unknown tool `{other}`"),
        }
    }

    fn is_reviewer(&self) -> bool {
        match self.identity.managed() {
            Some((session, Role::Worker)) => self
                .own_task(session)
                .is_ok_and(|task| task.kind == headwork_core::config::REVIEW_KIND),
            Some(_) | None => false,
        }
    }

    fn recap(&mut self, session: &str, task: Option<&Task>, args: &Value) -> Result<String> {
        let body = text(args, "text")?;
        if body.trim().is_empty() {
            bail!("`text` is empty: write what you were doing, the next step and what runs");
        }
        let id = self.store.add_recap(
            &NewRecap {
                task_id: task.map(|task| task.id),
                session_id: Some(session),
                kind: RecapKind::Session,
                body: &body,
            },
            self.env.actor,
        )?;
        let parking = self.store.machine_mode()? == MachineMode::Parking;
        let next = match (parking, task) {
            (false, _) => "recorded",
            (true, Some(_)) => "stop now: end your turn, headwork closes this pane",
            (true, None) => {
                "recorded: tell the user when the park reports safe to restart; headwork reopens you after the restart"
            }
        };
        to_text(&json!({
            "recap": id,
            "parking": parking,
            "next": next,
        }))
    }

    fn helper_tab(&self, session: &str, args: &Value) -> Result<String> {
        let task = self.own_task(session)?;
        let registration = registration(&json!({
            "label": "helper",
            "argv": args.get("argv").cloned().unwrap_or(Value::Null),
            "cwd": args.get("cwd").cloned().unwrap_or(Value::Null),
        }))?;
        if registration.argv.is_empty() {
            bail!("`argv` is empty: name the program to run");
        }
        let mut program = registration.argv;
        if headwork_core::usage::fresh_claude(&program) {
            let helper = uuid::Uuid::new_v4().to_string();
            program.splice(1..1, ["--session-id".to_owned(), helper.clone()]);
            self.store
                .record_helper_session(task.id, &helper, &program, self.env.actor)?;
        }
        let (row, _) = self
            .store
            .session(session)?
            .ok_or_else(|| anyhow!("this session is not in headwork's store"))?;
        let workspace = super::place::helper_workspace(
            self.store,
            &self.env,
            &task,
            row.host_workspace.map(headwork_adapters::host::WorkspaceId),
        )?;
        let dir = match registration.cwd {
            Some(cwd) => std::path::PathBuf::from(cwd),
            None => self.env.task_dir(&task)?,
        };
        let surface = super::place::open_helper(&self.env, &workspace, &dir, &program)?;
        to_text(&json!({ "surface": surface.as_str(), "workspace": workspace.as_str() }))
    }

    fn job_register(&mut self, session: &str, task: &Task, args: &Value) -> Result<String> {
        let registration = registration(args)?;
        to_text(&jobs::register(
            self.store,
            &self.env,
            task,
            session,
            &registration,
        )?)
    }

    fn job_status(&mut self, session: &str, role: Role) -> Result<String> {
        jobs::refresh(self.store, self.env.actor)?;
        let task = if role == Role::Worker {
            Some(self.own_task(session)?.id)
        } else {
            None
        };
        to_text(&self.store.open_jobs(task)?)
    }

    fn job_stop(&mut self, task: &Task, args: &Value) -> Result<String> {
        let id = args
            .get("id")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("`id` is required and must be an integer"))?;
        let job = self
            .store
            .job(id)?
            .filter(|job| job.task_id == Some(task.id))
            .ok_or_else(|| anyhow!("no job {id} of yours"))?;
        jobs::stop(self.store, &job, "stopped by its session", self.env.actor)?;
        to_text(&self.store.job(id)?)
    }

    fn job_forget(&mut self, session: &str, role: Role, args: &Value) -> Result<String> {
        let id = args
            .get("id")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("`id` is required and must be an integer"))?;
        let reason = args
            .get("reason")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("`reason` is required"))?;
        if role == Role::Worker {
            let task = self.own_task(session)?;
            self.store
                .job(id)?
                .filter(|job| job.task_id == Some(task.id))
                .ok_or_else(|| anyhow!("no job {id} of yours"))?;
        }
        to_text(&jobs::forget(self.store, id, reason, self.env.actor)?)
    }

    fn migrate_list(&self, args: &Value) -> Result<String> {
        let report = super::migrate::report(
            self.store,
            self.env.home,
            self.env.config,
            flag(args, "fetch"),
        )?;
        to_text(&report)
    }

    fn migrate_mark(&mut self, args: &Value) -> Result<String> {
        let state = headwork_core::migrate::Settled::parse(&text(args, "state")?)
            .ok_or_else(|| anyhow!("`state` is `applied` or `skipped`"))?;
        super::migrate::mark(
            self.store,
            self.env.home,
            self.env.config,
            &text(args, "note")?,
            state,
            &text(args, "reason")?,
            self.env.actor,
        )
    }

    fn backlog_list(&self, args: &Value) -> Result<String> {
        let word = |key| args.get(key).and_then(Value::as_str);
        let items = super::backlog::list(
            self.store,
            word("project"),
            word("owner").map(str::parse).transpose()?,
            word("state"),
        )?;
        to_text(
            &items
                .iter()
                .map(headwork_core::backlog::BacklogItem::line)
                .collect::<Vec<_>>(),
        )
    }

    fn backlog_add(&mut self, args: &Value) -> Result<String> {
        let optional = |key| args.get(key).and_then(Value::as_str);
        let notes: Vec<String> = match args.get("notes") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_owned)
                        .context("`notes` holds lines as strings")
                })
                .collect::<Result<_>>()?,
            Some(_) => bail!("`notes` is a list of lines"),
        };
        let request = super::backlog::AddRequest {
            project: text(args, "project")?,
            text: text(args, "text")?,
            section: optional("section").map(str::to_owned),
            owner: optional("owner").map(str::parse).transpose()?,
            not_before: optional("not_before").map(str::parse).transpose()?,
            due: optional("due").map(str::parse).transpose()?,
            notes,
        };
        to_text(&super::backlog::add(self.store, &self.env, &request)?)
    }

    fn backlog_done(&mut self, args: &Value) -> Result<String> {
        let note = args.get("note").and_then(Value::as_str);
        to_text(&super::backlog::finish(
            self.store,
            &self.env,
            backlog_id(args)?,
            note,
            flag(args, "drop"),
        )?)
    }

    fn backlog_edit(&mut self, args: &Value) -> Result<String> {
        let optional = |key| args.get(key).and_then(Value::as_str);
        let date = |key| -> Result<Option<Option<headwork_core::backlog::BacklogDate>>> {
            optional(key)
                .map(|word| {
                    if word == "none" {
                        Ok(None)
                    } else {
                        Ok(Some(word.parse()?))
                    }
                })
                .transpose()
        };
        let edit = headwork_store::BacklogEdit {
            text: optional("text").map(str::to_owned),
            project: optional("project").map(str::to_owned),
            section: optional("section").map(|word| (word != "none").then(|| word.to_owned())),
            owner: optional("owner").map(str::parse).transpose()?,
            not_before: date("not_before")?,
            due: date("due")?,
            append: optional("append").map(str::to_owned),
            added: None,
        };
        to_text(&super::backlog::edit(
            self.store,
            &self.env,
            backlog_id(args)?,
            &edit,
        )?)
    }

    fn config_sync(&self, args: &Value) -> Result<String> {
        let dry_run = flag(args, "dry_run");
        let options = super::config_sync::SyncOptions {
            pull: args.get("pull").and_then(Value::as_bool).unwrap_or(true),
            dry_run,
        };
        let log = super::config_sync::Logging {
            store: Some(&*self.store),
            actor: self.env.actor,
        };
        let report = super::config_sync::sync(
            self.env.home,
            self.env.exe,
            options,
            headwork_core::Platform::current(),
            log,
        )?;
        if report.ok && !dry_run {
            drop(super::migrate::announce(self.env.home, self.env.config));
        }
        to_text(&report)
    }

    fn update(&self, args: &Value) -> Result<String> {
        let to = match args.get("to") {
            None | Some(Value::Null) => None,
            Some(Value::String(tag)) => Some(
                headwork_core::update::Version::from_tag(tag)
                    .ok_or_else(|| anyhow!("`to` is `{tag}`, not a release tag such as v0.1.7"))?,
            ),
            Some(other) => bail!("`to` must be a release tag such as v0.1.7, not {other}"),
        };
        if flag(args, "dry_run") {
            let install_dir = self.env.home.expand(super::update::DEFAULT_DIR);
            let releases = headwork_adapters::release::Curl::discovered();
            let env = super::update::UpdateEnv {
                home: self.env.home,
                config: self.env.config,
                releases: &releases,
                builds: &super::update::SystemBuilds,
                disks: &headwork_adapters::disk::SystemDisks,
                clock: &super::update::SystemClock,
                install_dir: &install_dir,
                asset: headwork_adapters::release::asset_name()?,
                actor: self.env.actor,
                host: None,
            };
            let options = super::update::UpdateOptions {
                dry_run: true,
                wait_min: None,
                to,
            };
            return Ok(super::update::update(&env, options)?.to_string());
        }
        let mut command = vec!["update".to_owned()];
        if let Some(to) = to {
            command.extend(["--to".to_owned(), to.tag()]);
        }
        let detached = super::update::Detached {
            home: self.env.home,
            exe: self.env.exe,
        };
        let triggered = super::update::trigger(
            self.store,
            &detached,
            self.env.actor,
            None,
            super::update::TriggeredBy::Orchestrator,
            command,
        )?;
        match (&triggered.started, &triggered.error) {
            (Some(started), _) => to_text(&json!({
                "started": format!("headwork {}", triggered.args.join(" ")),
                "pid": started.pid,
                "note": started.note,
                "read": "the event log (update-installed, update-failed) and `headwork doctor` (check update) say how it ended",
            })),
            (None, error) => bail!(
                "`headwork {}` did not start: {}",
                triggered.args.join(" "),
                error.as_deref().unwrap_or("no reason given")
            ),
        }
    }

    fn start_detached(&self, command: &'static str, args: &Value) -> Result<String> {
        let mut process = std::process::Command::new(self.env.exe);
        process.arg(command);
        if command == "park"
            && let Some(seconds) = args.get("deadline_s").and_then(Value::as_u64)
        {
            process.args(["--deadline", &seconds.to_string()]);
        }
        process
            .arg("--home")
            .arg(self.env.home.root())
            .current_dir(self.env.home.root());
        let (pid, _) = headwork_adapters::proc::spawn_detached(&mut process)?;
        self.store.append_event(
            self.env.actor,
            None,
            None,
            "detached",
            &json!({ "command": command, "pid": pid }),
        )?;
        to_text(
            &json!({ "started": command, "pid": pid, "read": "`board` shows the machine mode" }),
        )
    }

    fn own_task(&self, session: &str) -> Result<Task> {
        let row = self
            .store
            .hook_session(session)?
            .filter(|row| row.role == Role::Worker)
            .and_then(|row| row.task_id)
            .ok_or_else(|| anyhow!("this session has no task in headwork"))?;
        Ok(self.store.task(row)?)
    }

    /// Whether the calling session is a worker whose task is a lead.
    fn is_lead(&self) -> bool {
        match self.identity.managed() {
            Some((session, Role::Worker)) => self
                .own_task(session)
                .is_ok_and(|task| task.shape.is_lead()),
            Some(_) | None => false,
        }
    }

    /// The tasks a session's `board` and `inbox` show: for a lead or a part, the lead and its parts; `None` for the
    /// orchestrator and a plain worker, who see the whole machine.
    fn group(&self, session: &str, role: Role) -> Result<Option<Vec<TaskId>>> {
        if role != Role::Worker {
            return Ok(None);
        }
        let task = self.own_task(session)?;
        let lead = match task.shape {
            TaskShape::Lead => task.id,
            TaskShape::Part { parent } => parent,
            TaskShape::Plain => return Ok(None),
        };
        let mut ids = vec![lead];
        ids.extend(self.store.parts(lead)?);
        Ok(Some(ids))
    }

    fn scoped_board(&self, session: &str, role: Role, all: bool) -> Result<Board> {
        let mut board = self.store.board(all)?;
        if let Some(ids) = self.group(session, role)? {
            let own = self.own_task(session)?;
            board.tasks.retain(|entry| ids.contains(&entry.task.id));
            board.projects.retain(|row| &row.name == own.project());
            board.other_asks.clear();
        }
        Ok(board)
    }

    fn board(&mut self, session: &str, role: Role, args: &Value) -> Result<String> {
        let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
        super::lost::reconcile(self.store, &self.env)?;
        let board = to_text(&self.scoped_board(session, role, all)?)?;
        Ok(if role == Role::Orchestrator {
            format!(
                "{}{board}",
                super::digest::section(self.store, self.env.config)
            )
        } else {
            board
        })
    }

    fn status(&mut self, args: &Value) -> Result<String> {
        super::lost::reconcile(self.store, &self.env)?;
        let lines = to_text(&super::status::status(
            self.store,
            flag(args, "all"),
            &self.env.config.usage,
        )?)?;
        Ok(format!(
            "{}{lines}",
            super::digest::section(self.store, self.env.config)
        ))
    }

    fn usage(&mut self, args: &Value) -> Result<String> {
        let word = |key| args.get(key).and_then(Value::as_str).map(str::to_owned);
        let options = super::usage::UsageOptions {
            by: word("by").as_deref().unwrap_or("task").parse()?,
            since: word("since"),
            task: word("task"),
            unmanaged: flag(args, "unmanaged"),
        };
        let files = super::usage::transcripts(self.env.home.claude_dir());
        let ingested = super::usage::ingest(self.store, &files, false)?;
        let mut report = super::usage::report(self.store, &options, &self.env.config.usage)?;
        report.ingest = Some(ingested);
        to_text(&report)
    }

    fn inbox(&self, session: &str, role: Role) -> Result<String> {
        let board = self.scoped_board(session, role, false)?;
        let mut items: Vec<Value> = Vec::new();
        for entry in &board.tasks {
            for ask in &entry.asks {
                items.push(json!({ "task": entry.task.name.to_string(), "ask": ask }));
            }
        }
        for ask in &board.other_asks {
            items.push(json!({ "task": null, "ask": ask }));
        }
        items.sort_by_key(|item| item["ask"]["created_at"].as_str().map(str::to_owned));
        to_text(&items)
    }

    fn spawn(&mut self, args: &Value) -> Result<String> {
        let project = ProjectName::new(text(args, "project")?)?;
        let slug = Slug::new(text(args, "slug")?)?;
        let brief = text(args, "brief")?;
        let in_main = flag(args, "in_main");
        let kind = args.get("kind").and_then(Value::as_str).map_or_else(
            || {
                if in_main {
                    slug.as_str().to_owned()
                } else {
                    "task".to_owned()
                }
            },
            str::to_owned,
        );
        let flags = spawn_flags(args, &kind, "reason")?;
        let model: Option<Model> = args
            .get("model")
            .and_then(Value::as_str)
            .map(str::parse)
            .transpose()?;
        let effort: Option<Effort> = args
            .get("effort")
            .and_then(Value::as_str)
            .map(str::parse)
            .transpose()?;
        let depends_on = match args.get("depends_on").and_then(Value::as_str) {
            Some(name) => Some(
                self.store
                    .find_task(name)?
                    .with_context(|| format!("no task named `{name}`"))?
                    .id,
            ),
            None => None,
        };
        let allow_ask = allow_asks(args)?;
        let incoming = self.env.home.root().join("incoming");
        std::fs::create_dir_all(&incoming).context("creating the incoming briefs folder")?;
        let file = incoming.join(format!("{project}-{slug}.md"));
        std::fs::write(&file, brief).with_context(|| format!("writing {}", file.display()))?;
        let request = SpawnRequest {
            request: TaskRequest {
                project,
                slug,
                kind,
                model,
                effort,
                in_main,
                keep: flag(args, "keep"),
                close: flags.close,
                review: flags.review,
                decision: flags.decision,
            },
            brief: &file,
            depends_on,
            permission_mode: PermissionMode::Auto,
            allow_ask: &allow_ask,
        };
        let item = match args.get("backlog").or_else(|| args.get("todo")) {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_i64()
                    .ok_or_else(|| anyhow!("`backlog` is a backlog item's id, an integer"))?,
            ),
        };
        reap(self.store, &self.env)?;
        let result = super::backlog::spawn_linked(self.store, &self.env, &request, item);
        std::fs::remove_file(&file).ok();
        let spawned = result?;
        to_text(&json!({
            "task": spawned.task,
            "queue": spawned.pumped,
            "backlog_unlinked": spawned.unlinked,
        }))
    }

    fn part_spawn(&mut self, lead: &Task, args: &Value) -> Result<String> {
        let project = lead.project().clone();
        if let Some(asked) = args.get("project").and_then(Value::as_str)
            && asked != project.as_str()
        {
            bail!(
                "a part runs in its lead's project `{project}`; work in `{asked}` goes to the orchestrator as a new task"
            );
        }
        let word = text(args, "slug")?;
        let prefix = format!("{}-", lead.slug());
        let slug = Slug::new(if word.starts_with(&prefix) {
            word
        } else {
            format!("{prefix}{word}")
        })?;
        let brief = text(args, "brief")?;
        let model: Option<Model> = args
            .get("model")
            .and_then(Value::as_str)
            .map(str::parse)
            .transpose()?;
        let flags = spawn_flags(args, "task", "flag_reason")?;
        let opus_reason = match model {
            Some(Model::Opus) => Some(
                text(args, "reason")
                    .ok()
                    .filter(|reason| !reason.trim().is_empty())
                    .context(concat!(
                        "a part on Opus needs a `reason` (new architecture, publishing or ledger code, security, ",
                        "concurrency, an audit or a plan); otherwise leave `model` out for Sonnet"
                    ))?,
            ),
            _ => None,
        };
        let depends_on = match args.get("after").and_then(Value::as_str) {
            Some(after) => Some(self.part_of(lead, after)?.id),
            None => None,
        };
        let allow_ask = allow_asks(args)?;
        let carries_go = !allow_ask.is_empty()
            || !user_go::named_user_only(&brief, &self.env.config.all_user_only()).is_empty();
        if carries_go && !lead.in_main {
            bail!(
                "a part names a user-only command, which runs in the main checkout, but you, its lead, run in a worktree: \
                 run it yourself after the user's `Allow`, or leave it out of the part's brief"
            );
        }
        let (kind, in_main) = if carries_go {
            (lead.kind.clone(), true)
        } else {
            ("task".to_owned(), false)
        };
        let incoming = self.env.home.root().join("incoming");
        std::fs::create_dir_all(&incoming).context("creating the incoming briefs folder")?;
        let file = incoming.join(format!("{project}-{slug}.md"));
        std::fs::write(&file, brief).with_context(|| format!("writing {}", file.display()))?;
        let request = SpawnRequest {
            request: TaskRequest {
                project,
                slug,
                kind,
                model,
                effort: None,
                in_main,
                keep: false,
                close: flags.close,
                review: flags.review,
                decision: flags.decision,
            },
            brief: &file,
            depends_on,
            permission_mode: PermissionMode::Auto,
            allow_ask: &allow_ask,
        };
        reap(self.store, &self.env)?;
        let result = spawn_shaped(
            self.store,
            &self.env,
            &request,
            TaskShape::Part { parent: lead.id },
        );
        std::fs::remove_file(&file).ok();
        let (task, pumped) = result?;
        if let Some(reason) = opus_reason {
            self.store.append_event(
                self.env.actor,
                Some(task.id),
                None,
                "part-opus-reason",
                &json!({ "lead": lead.name.to_string(), "reason": reason }),
            )?;
        }
        let fable = task.fable_ask.map(
            |_| "Fable waits for the user's approval: the part starts once the user allows it",
        );
        to_text(&json!({ "task": task, "queue": pumped, "fable": fable }))
    }

    /// A part of `lead` by its word (`review`), its slug (`<lead slug>-review`) or its task name.
    fn part_of(&self, lead: &Task, name: &str) -> Result<Task> {
        let slug = lead.slug().as_str();
        let candidates = [
            name.to_owned(),
            format!("{}-{name}", lead.project()),
            format!("{}-{slug}-{name}", lead.project()),
        ];
        for candidate in &candidates {
            if let Some(task) = self.store.find_task(candidate)?
                && task.shape.parent() == Some(lead.id)
            {
                return Ok(task);
            }
        }
        bail!("`{name}` is not a part of {}", lead.name)
    }

    fn part_drop(&mut self, lead: &Task, args: &Value) -> Result<String> {
        let part = self.part_of(lead, &text(args, "name")?)?;
        let reason = args
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("dropped by its lead");
        self.store.drop_task(part.id, self.env.actor)?;
        self.store.append_event(
            self.env.actor,
            Some(part.id),
            None,
            "drop-reason",
            &json!({ "lead": lead.name.to_string(), "reason": reason }),
        )?;
        let report = close_dropped_part(self.store, &self.env, lead.id, part.id)?;
        pump(self.store, &self.env)?;
        to_text(&report)
    }

    fn named(&self, args: &Value) -> Result<Task> {
        let name = text(args, "name")?;
        self.store
            .find_task(&name)?
            .with_context(|| format!("no task named `{name}`"))
    }

    fn drop_task(&mut self, args: &Value) -> Result<String> {
        let task = self.named(args)?;
        let words = text(args, "user_words")?;
        self.store.drop_task(task.id, self.env.actor)?;
        self.store.append_event(
            self.env.actor,
            Some(task.id),
            None,
            "drop-reason",
            &json!({ "user_words": words }),
        )?;
        let report = close(self.store, &self.env, task.id, None)?;
        pump(self.store, &self.env)?;
        to_text(&report)
    }

    fn close_task(&mut self, args: &Value) -> Result<String> {
        let task = self.named(args)?;
        let report = if args.get("force").and_then(Value::as_bool).unwrap_or(false) {
            close_forced(self.store, &self.env, self.git, task.id)?
        } else {
            close(self.store, &self.env, task.id, None)?
        };
        super::prs::refresh_dependencies(self.store, &self.env, self.git)?;
        pump(self.store, &self.env)?;
        to_text(&report)
    }

    fn answer(&mut self, args: &Value) -> Result<String> {
        let ask = match args.get("ask") {
            Some(ask) => headwork_store::AskId(ask.as_i64().context("`ask` must be an ask's id")?),
            None => super::answer::only_waiting(self.store)?,
        };
        let choice = text(args, "choice")?;
        let report = super::answer::answer(self.store, &self.env, ask, &choice)?;
        pump(self.store, &self.env)?;
        to_text(&report)
    }

    fn queue(&mut self, args: &Value) -> Result<String> {
        let names: Vec<String> = args
            .get("order")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if !names.is_empty() {
            let ids = names
                .iter()
                .map(|name| {
                    self.store
                        .find_task(name)?
                        .map(|task| task.id)
                        .with_context(|| format!("no task named `{name}`"))
                })
                .collect::<Result<Vec<_>>>()?;
            self.store.set_queue_order(&ids, self.env.actor)?;
        }
        reap(self.store, &self.env)?;
        super::prs::refresh_dependencies(self.store, &self.env, self.git)?;
        to_text(&pump(self.store, &self.env)?)
    }

    fn rule_add(&mut self, args: &Value) -> Result<String> {
        let scope = text(args, "scope")?;
        let scope = scope
            .parse::<Role>()
            .map_or(scope, |role| role.as_str().to_owned());
        let body = text(args, "text")?;
        let known = matches!(scope.as_str(), "global" | "orchestrator" | "worker")
            || self
                .env
                .config
                .projects
                .keys()
                .any(|project| project.as_str() == scope);
        if !known {
            bail!(
                "unknown scope `{scope}`: use global, orchestrator, worker or a configured project"
            );
        }
        if body.trim().is_empty() {
            bail!("a rule needs text");
        }
        let id = self.store.add_rule(&scope, &body, self.env.actor)?;
        let version = self.bump_rules()?;
        to_text(&json!({ "id": id, "scope": scope, "rules_version": version }))
    }

    fn rule_retire(&mut self, args: &Value) -> Result<String> {
        let id = args
            .get("id")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("`id` must be an integer"))?;
        self.store.retire_rule(id, self.env.actor)?;
        let version = self.bump_rules()?;
        to_text(&json!({ "retired": id, "rules_version": version }))
    }

    fn bump_rules(&mut self) -> Result<i64> {
        let files = rules::current(self.store, self.env.home, self.env.config, self.env.host)?;
        Ok(self.store.observe_rules(files.hash(), self.env.actor)?)
    }

    fn report(&mut self, session: &str, task: &Task, args: &Value) -> Result<String> {
        let status = text(args, "status")?;
        if !STATUSES.contains(&status.as_str()) {
            bail!("`status` must be one of {}", STATUSES.join(", "));
        }
        let summary = text(args, "summary")?;
        let links: Vec<String> = args
            .get("links")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let pr = args
            .get("pr")
            .and_then(Value::as_str)
            .map(parse_pr)
            .transpose()?;
        if status == "review" && pr.is_none() {
            bail!("a `review` report needs `pr`, the pull request's URL");
        }
        self.store
            .record_report(task.id, session, &status, &summary, &links, pr.as_ref())?;
        let mut moved = None;
        if status == "review"
            && task.state != TaskState::InReview
            && let Some(pr) = &pr
        {
            let repo_allowed = self
                .env
                .config
                .project(task.project())
                .is_some_and(|project| {
                    project
                        .allowed_repos
                        .iter()
                        .any(|repo| repo.as_str().eq_ignore_ascii_case(&pr.repo))
                });
            match self.store.transition(
                task.id,
                &Trigger::ReviewOpened { repo_allowed },
                self.env.actor,
            ) {
                Ok(step) => moved = Some(step.to),
                Err(StoreError::Refused { refusal, .. }) => {
                    return to_text(
                        &json!({ "recorded": true, "state": task.state, "note": refusal.to_string() }),
                    );
                }
                Err(error) => return Err(error.into()),
            }
        }
        let state = moved.unwrap_or(self.store.task(task.id)?.state);
        let note = if status == "review" {
            let fresh = self.store.task(task.id)?;
            super::flags::start_planned(
                self.store,
                self.env.config,
                self.env.home.root(),
                &fresh,
                headwork_core::PlanMoment::FirstPullRequest,
                self.env.actor,
            )?
            .note()
        } else {
            None
        };
        let mut reply = json!({ "recorded": true, "status": status, "state": state });
        if let Some(note) = note {
            reply["note"] = Value::String(note);
        }
        to_text(&reply)
    }

    fn done(&mut self, session: &str, task: Task, args: &Value) -> Result<String> {
        if task.keep {
            bail!("a keep task does not finish by itself: the orchestrator or the user closes it");
        }
        if self.store.open_ask_of(session)?.is_some() {
            bail!("a question to the user is still open: wait for the answer before `done`");
        }
        if let Some(job) = self
            .store
            .open_jobs(Some(task.id))?
            .into_iter()
            .find(|job| job.state == headwork_core::JobState::Running)
        {
            bail!(
                "job `{}` of this task still runs: stop it with `job_stop` or wait for it before `done`",
                job.label
            );
        }
        if matches!(
            task.state,
            TaskState::Closing | TaskState::Closed | TaskState::Dropped
        ) {
            bail!("the task is already {}", task.state);
        }
        match super::flags::start_planned(
            self.store,
            self.env.config,
            self.env.home.root(),
            &task,
            headwork_core::PlanMoment::Done,
            self.env.actor,
        )? {
            super::flags::PlannedOutcome::Requested { review, level, .. } => bail!(
                "review-planned: a level-{level} review (review {review}) was planned for this task; commit your work, then \
                 call `review_start`"
            ),
            super::flags::PlannedOutcome::FlowBroken(problem) => bail!(
                "a review was planned for this task, but {problem}; the plan is cleared and no review started: tell the \
                 user, then call `done` again"
            ),
            super::flags::PlannedOutcome::None | super::flags::PlannedOutcome::AlreadyOpen => {}
        }
        let summary = args
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("done")
            .to_owned();
        begin_own_close(self.store, &self.env, self.git, task.id)?;
        self.store
            .record_report(task.id, session, "done", &summary, &[], None)?;
        self.closing = Some(task);
        Ok("done recorded; this session, its pane and its worktree close now".to_owned())
    }
}

fn items<T: serde::de::DeserializeOwned>(args: &Value) -> Result<Vec<T>> {
    let items = args.get("items").cloned().unwrap_or_else(|| json!([]));
    serde_json::from_value(items)
        .map_err(|error| anyhow!("`items` does not have the expected shape: {error}"))
}

fn backlog_id(args: &Value) -> Result<i64> {
    args.get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("`id` is required and must be an integer"))
}

fn text(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("`{key}` is required and must be a string"))
}

fn spawn_flags(args: &Value, kind: &str, reason_key: &str) -> Result<super::flags::SpawnFlags> {
    let close: Option<headwork_core::CloseMode> = args
        .get("close")
        .and_then(Value::as_str)
        .map(str::parse)
        .transpose()?;
    let review = match args.get("review") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_u64()
                .and_then(|number| u8::try_from(number).ok())
                .ok_or_else(|| anyhow!("`review` must be 0, 1, 2 or 3"))?,
        ),
    };
    super::flags::resolve(
        kind,
        close,
        review,
        args.get("decided_by").and_then(Value::as_str),
        args.get(reason_key).and_then(Value::as_str),
    )
}

fn flag(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// The `allow_ask` commands of a spawn or a part spawn: a list of strings, one string, or none.
fn allow_asks(args: &Value) -> Result<Vec<String>> {
    match args.get("allow_ask") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .context("`allow_ask` holds commands as strings")
            })
            .collect(),
        Some(Value::String(command)) => Ok(vec![command.clone()]),
        Some(_) => bail!("`allow_ask` is a list of commands"),
    }
}

fn to_text(value: &impl serde::Serialize) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)?)
}

/// `owner/name` and the number of a GitHub pull request URL.
const PR_URL_SHAPE: &str = "`pr` must be a https://<host>/<owner>/<repo>/pull/<n> URL, on github.com or a GitHub Enterprise host";

/// The pull request at `url`, `https://<host>/<owner>/<repo>/pull/<n>` on any host: github.com or a GitHub
/// Enterprise one such as `ghe.example.com`. The URL is kept whole, host included; `repo` is `<owner>/<repo>`.
fn parse_pr(url: &str) -> Result<NewPr> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| anyhow!(PR_URL_SHAPE))?;
    let mut parts = rest.trim_end_matches('/').split('/');
    let (Some(host), Some(owner), Some(name), Some("pull"), Some(number), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        bail!(PR_URL_SHAPE);
    };
    if [host, owner, name].iter().any(|part| part.is_empty()) {
        bail!(PR_URL_SHAPE);
    }
    Ok(NewPr {
        repo: format!("{owner}/{name}"),
        number: number
            .parse()
            .map_err(|_| anyhow!("`pr` has no pull request number"))?,
        url: url.to_owned(),
    })
}

fn success(id: &Value, result: &Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn failure(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn registration(args: &Value) -> Result<Registration> {
    let command_line = args
        .get("argv")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .ok_or_else(|| anyhow!("`argv` is required and must be an array of strings"))?;
    Ok(Registration {
        label: text(args, "label")?,
        argv: command_line,
        cwd: args.get("cwd").and_then(Value::as_str).map(str::to_owned),
        pid: args
            .get("pid")
            .and_then(Value::as_u64)
            .map(u32::try_from)
            .transpose()
            .context("`pid` does not fit a process id")?,
        park_policy: args
            .get("park_policy")
            .and_then(Value::as_str)
            .map(str::parse)
            .transpose()?,
        checkpoint_path: args
            .get("checkpoint_path")
            .and_then(Value::as_str)
            .map(str::to_owned),
        restart: args.get("restart").and_then(Value::as_bool),
        heavy: flag(args, "heavy"),
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "tests")]

    use super::parse_pr;

    #[test]
    fn a_pull_request_url_parses_on_any_host() {
        let cases = [
            (
                "https://github.com/Ivan091/demo/pull/12",
                "Ivan091/demo",
                12,
            ),
            (
                "https://ghe.example.com/team/config/pull/3",
                "team/config",
                3,
            ),
            (
                "https://ghe.example.com/team/config/pull/3/",
                "team/config",
                3,
            ),
        ];
        for (url, repo, number) in cases {
            let pr = parse_pr(url).unwrap();
            assert_eq!((pr.repo.as_str(), pr.number), (repo, number), "{url}");
            assert_eq!(pr.url, url, "the URL is kept whole, host included");
        }
    }

    #[test]
    fn anything_else_is_refused() {
        for url in [
            "http://github.com/o/r/pull/1",
            "github.com/o/r/pull/1",
            "https://github.com/o/r/issues/1",
            "https://github.com/o/r/pull/1/files",
            "https://github.com/o/pull/1",
            "https:///o/r/pull/1",
            "https://ghe.example.com//r/pull/1",
            "https://ghe.example.com/o/r/pull/x",
        ] {
            assert!(parse_pr(url).is_err(), "{url}");
        }
    }
}
