//! The MCP server (design section 3.3): JSON-RPC over stdio, newline-delimited, offering the orchestrator's or a worker's tools
//! by the identity the launcher put in the environment.
//!
//! The role is read once and checked again on every call. A session without an identity is unmanaged: it sees no
//! tools and every call is refused. A worker's tools also need the session row of a task, so a forged role alone gets
//! nothing.

use std::io::{BufRead, Write};

use anyhow::{Context, Result, anyhow, bail};
use headwork_core::lifecycle::Trigger;
use headwork_core::{
    Actor, Effort, MachineMode, Model, PermissionMode, ProjectName, RecapKind, Role, Slug,
    TaskRequest, TaskState,
};
use headwork_store::{NewPr, NewRecap, Store, StoreError, Task};
use serde_json::{Value, json};

use super::close::close;
use super::jobs::{self, Registration};
use super::rules;
use super::spawn::{SpawnRequest, pump, spawn};
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
                    "kind": { "type": "string" },
                    "keep": { "type": "boolean", "description": "A standing role that does not close when idle." },
                    "in_main": { "type": "boolean", "description": "Run in the project's main checkout." },
                    "depends_on": { "type": "string", "description": "Name of a task that must be merged first." },
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
        description: "Close a task without dropping it: a keep task, or one stuck after done.",
        roles: ORCHESTRATOR,
        schema: || object(json!({ "name": { "type": "string" } }), &["name"]),
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
                    "pr": { "type": "string", "description": "A GitHub pull request URL, for status review." },
                }),
                &["status", "summary"],
            )
        },
    },
    ToolDef {
        name: "done",
        description: "Finish the task: records the report, then closes this session, its pane and its worktree. Refused \
                      while a question to the user is open.",
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
        description: "Write down what you were doing, your next step and what is running. A park asks for it before a \
                      restart; also write one when you stop for the day. It is handed back to you when you resume.",
        roles: BOTH,
        schema: || {
            object(
                json!({ "text": { "type": "string", "description": "What you were doing, the next step, what runs." } }),
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
        name: "park",
        description: "Park the machine before a restart: every session writes a recap, jobs stop at a checkpoint, the \
                      panes close. It runs in the background; read `board` for the machine mode, and write your own \
                      recap first.",
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
    identity: Identity,
    closing: Option<Task>,
}

impl<'a> Server<'a> {
    /// A server for one session. `env.actor` is replaced by the role's actor.
    #[must_use]
    pub const fn new(store: &'a mut Store, mut env: Env<'a>, identity: Identity) -> Self {
        env.actor = match identity.role {
            Some(Role::Orchestrator) => Actor::Orchestrator,
            Some(Role::Worker) => Actor::Worker,
            None => Actor::User,
        };
        Self {
            store,
            env,
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
    pub fn serve(&mut self, input: impl BufRead, mut output: impl Write) -> Result<()> {
        for line in input.lines() {
            let line = line.context("reading a request")?;
            if line.trim().is_empty() {
                continue;
            }
            let response = match serde_json::from_str::<Value>(&line) {
                Ok(message) => self.handle(&message),
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
            self.finish_pending()?;
        }
        Ok(())
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
        close(
            self.store,
            &self.env,
            task.id,
            self.identity.session.as_deref(),
        )?;
        pump(self.store, &self.env)?;
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
        TOOLS
            .iter()
            .filter(|tool| tool.roles.contains(&role))
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
        match name {
            "board" => self.board(args),
            "inbox" => self.inbox(),
            "spawn" => self.spawn(args),
            "drop" => self.drop_task(args),
            "close" => self.close_task(args),
            "queue" => self.queue(args),
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
            "job_status" => self.job_status(&session, role),
            "job_stop" => {
                let task = self.own_task(&session)?;
                self.job_stop(&task, args)
            }
            "park" => self.start_detached("park", args),
            "resume" => self.start_detached("resume", args),
            other => bail!("unknown tool `{other}`"),
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

    fn board(&self, args: &Value) -> Result<String> {
        let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
        to_text(&self.store.board(all)?)
    }

    fn inbox(&self) -> Result<String> {
        let board = self.store.board(false)?;
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
            },
            brief: &file,
            depends_on,
            permission_mode: PermissionMode::Auto,
        };
        reap(self.store, &self.env)?;
        let result = spawn(self.store, &self.env, &request);
        std::fs::remove_file(&file).ok();
        let (task, pumped) = result?;
        to_text(&json!({ "task": task, "queue": pumped }))
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
        self.store
            .transition(task.id, &Trigger::Drop, self.env.actor)?;
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
        let report = close(self.store, &self.env, task.id, None)?;
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
                        .any(|repo| repo.as_str() == pr.repo)
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
        to_text(&json!({ "recorded": true, "status": status, "state": state }))
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
        let summary = args
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("done")
            .to_owned();
        self.store
            .record_report(task.id, session, "done", &summary, &[], None)?;
        self.closing = Some(task);
        Ok("done recorded; this session, its pane and its worktree close now".to_owned())
    }
}

fn text(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("`{key}` is required and must be a string"))
}

fn flag(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn to_text(value: &impl serde::Serialize) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)?)
}

/// `owner/name` and the number of a GitHub pull request URL.
fn parse_pr(url: &str) -> Result<NewPr> {
    let rest = url
        .strip_prefix("https://github.com/")
        .ok_or_else(|| anyhow!("`pr` must be a https://github.com/<owner>/<repo>/pull/<n> URL"))?;
    let mut parts = rest.trim_end_matches('/').split('/');
    let (Some(owner), Some(name), Some("pull"), Some(number), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        bail!("`pr` must be a https://github.com/<owner>/<repo>/pull/<n> URL");
    };
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
