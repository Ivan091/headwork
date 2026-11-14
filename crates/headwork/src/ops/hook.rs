//! `headwork hook <event>`: what each Claude Code hook decides (design section 3.2).
//!
//! A hook reads the event's JSON from stdin and its session's identity from `HEADWORK_SESSION`; a session without it,
//! or unknown to the store, is unmanaged and always allowed. Every deny is logged with its rule id. In shadow mode
//! (`hooks.enforce = false`) the call still goes ahead; enforced, the hook exits 2 with the reason on stderr, the
//! form that blocks in every permission mode.

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;

use anyhow::{Context, Result};
use headwork_adapters::host::{Choice, SurfaceId, TerminalHost};
use headwork_core::config::{Delivery, PushMode, WorktreeCommand};
use headwork_core::guard::ask::{AskFacts, Precondition, Question, check_ask};
use headwork_core::guard::stop::{StopFacts, check_stop};
use headwork_core::guard::{
    DEFAULT_BRANCHES, Decision, GuardPath, Scope, ToolCall, check_tool, repo_from_url,
};
use headwork_core::lifecycle::{SessionFacts, Trigger};
use headwork_core::{
    Actor, AskKind, Config, Role, SessionState, TaskState, is_orchestrator_address,
};
use headwork_store::{
    EVENT_DENY, EVENT_PROMPT, EVENT_READDRESSED, EVENT_TOOL, EVENT_WOULD_DENY, HookSession, NewAsk,
    Store, StoreError, Task,
};
use serde_json::{Value, json};

use super::rules::{self, Card};
use super::slashed;
use crate::home::Home;

/// The hook events headwork handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    /// The session started, resumed, cleared or compacted.
    SessionStart,
    /// A prompt arrived: from the user, a schedule or another session.
    UserPromptSubmit,
    /// A tool is about to run.
    PreToolUse,
    /// A tool ran.
    PostToolUse,
    /// The turn is ending.
    Stop,
    /// The session ended.
    SessionEnd,
}

impl HookEvent {
    /// Claude's name of the event.
    #[must_use]
    pub const fn claude_name(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::Stop => "Stop",
            Self::SessionEnd => "SessionEnd",
        }
    }
}

impl FromStr for HookEvent {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        Ok(match text {
            "session-start" | "SessionStart" => Self::SessionStart,
            "user-prompt-submit" | "UserPromptSubmit" => Self::UserPromptSubmit,
            "pre-tool-use" | "PreToolUse" => Self::PreToolUse,
            "post-tool-use" | "PostToolUse" => Self::PostToolUse,
            "stop" | "Stop" => Self::Stop,
            "session-end" | "SessionEnd" => Self::SessionEnd,
            other => anyhow::bail!("unknown hook event `{other}`"),
        })
    }
}

/// Tools whose calls `PostToolUse` records for the ask and stop checks.
pub const TRACKED_TOOLS: &[&str] = &[
    "AskUserQuestion",
    "PushNotification",
    "SendUserFile",
    "SendMessage",
];

/// What a hook answers Claude.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HookReply {
    /// JSON for stdout, such as `additionalContext`.
    pub stdout: Option<String>,
    /// The reason of a blocking deny, for stderr.
    pub stderr: Option<String>,
    /// 0, or 2 for a blocking deny.
    pub exit: u8,
}

impl HookReply {
    fn context(event: HookEvent, text: &str) -> Self {
        let body = json!({
            "hookSpecificOutput": { "hookEventName": event.claude_name(), "additionalContext": text }
        });
        Self {
            stdout: Some(body.to_string()),
            ..Self::default()
        }
    }
}

/// Facts a hook reads from git.
pub trait GitProbe: std::fmt::Debug {
    /// The branch checked out in `dir`.
    fn branch(&self, dir: &str) -> Option<String>;
    /// The remotes of `dir`: name and `owner/name`.
    fn remotes(&self, dir: &str) -> Vec<(String, String)>;
}

/// The real git.
#[derive(Debug, Clone, Copy, Default)]
pub struct Git;

impl Git {
    fn run(dir: &str, args: &[&str]) -> Option<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}

impl GitProbe for Git {
    fn branch(&self, dir: &str) -> Option<String> {
        Self::run(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).filter(|branch| !branch.is_empty())
    }

    fn remotes(&self, dir: &str) -> Vec<(String, String)> {
        Self::run(dir, &["remote", "-v"])
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let mut words = line.split_whitespace();
                let name = words.next()?;
                let repo = repo_from_url(words.next()?)?;
                Some((name.to_owned(), repo))
            })
            .collect()
    }
}

/// Everything a hook acts with besides the store.
#[derive(Debug, Clone, Copy)]
pub struct HookEnv<'a> {
    /// The home folder.
    pub home: &'a Home,
    /// The machine config.
    pub config: &'a Config,
    /// The terminal host, for blocked states.
    pub host: &'a dyn TerminalHost,
    /// Git facts.
    pub git: &'a dyn GitProbe,
    /// The `headwork` program, named in the rules.
    pub exe: &'a Path,
    /// The temp folder, where Claude's scratchpad lives.
    pub temp: &'a Path,
}

/// Runs one hook for a managed session.
///
/// # Errors
///
/// Fails when the store fails; the caller reports it without blocking.
pub fn run_hook(
    store: &mut Store,
    env: &HookEnv<'_>,
    event: HookEvent,
    session_id: &str,
    input: &Value,
) -> Result<HookReply> {
    let Some(session) = store.hook_session(session_id)? else {
        return Ok(HookReply::default());
    };
    store.touch_session(&session.id)?;
    let task = session.task_id.map(|id| store.task(id)).transpose()?;
    let call = Call {
        env,
        session: &session,
        task: task.as_ref(),
        input,
    };
    match event {
        HookEvent::SessionStart => session_start(store, &call),
        HookEvent::UserPromptSubmit => prompt(store, &call),
        HookEvent::PreToolUse => pre_tool(store, &call),
        HookEvent::PostToolUse => post_tool(store, &call),
        HookEvent::Stop => stop(store, &call),
        HookEvent::SessionEnd => session_end(store, &call),
    }
}

/// One hook call.
#[derive(Debug, Clone, Copy)]
struct Call<'a> {
    env: &'a HookEnv<'a>,
    session: &'a HookSession,
    task: Option<&'a Task>,
    input: &'a Value,
}

impl Call<'_> {
    fn text(&self, key: &str) -> Option<&str> {
        self.input.get(key).and_then(Value::as_str)
    }

    fn cwd(&self) -> String {
        self.text("cwd").map_or_else(
            || {
                std::env::current_dir()
                    .map(|dir| slashed(&dir))
                    .unwrap_or_default()
            },
            str::to_owned,
        )
    }
}

fn task_dir(store: &Store, env: &HookEnv<'_>, task: &Task) -> Result<Option<PathBuf>> {
    if task.in_main {
        return Ok(env
            .config
            .project(task.project())
            .map(|project| env.home.expand(&project.main)));
    }
    Ok(store
        .worktree_of(task.id)?
        .map(|worktree| PathBuf::from(worktree.path)))
}

fn rules_for(store: &mut Store, call: &Call<'_>) -> Result<(String, i64)> {
    let env = call.env;
    let files = rules::current(store, env.home, env.config, env.host)?;
    let version = store.observe_rules(files.hash(), Actor::Hook)?;
    let dir = call
        .task
        .map(|task| task_dir(store, env, task))
        .transpose()?
        .flatten();
    let card = call
        .task
        .zip(dir.as_deref())
        .map(|(task, dir)| Card { task, dir });
    let text = files.render(call.session.role, env.config, env.exe, card.as_ref());
    Ok((text, version))
}

fn session_start(store: &mut Store, call: &Call<'_>) -> Result<HookReply> {
    if let Some(task) = call.task
        && task.state == TaskState::Spawning
    {
        let facts = SessionFacts {
            session_matches: call.text("session_id") == Some(call.session.id.as_str()),
            launcher_alive: true,
        };
        if let Err(error) = store.transition(task.id, &Trigger::SessionStarted(facts), Actor::Hook)
        {
            note(store, call, &json!({ "session-start": error.to_string() }))?;
        }
    }
    if call.session.state == SessionState::Starting {
        store.set_session_state(&call.session.id, SessionState::Live, Actor::Hook)?;
    }
    let (text, version) = rules_for(store, call)?;
    store.set_rules_seen(&call.session.id, call.session.task_id, version, text.len())?;
    Ok(HookReply::context(HookEvent::SessionStart, &text))
}

fn prompt(store: &mut Store, call: &Call<'_>) -> Result<HookReply> {
    let text = call.text("prompt").unwrap_or_default();
    let from = text
        .strip_prefix("<cross-session-message")
        .and_then(|rest| rest.split_once("from-name=\""))
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(name, _)| name.to_owned());
    store.append_event(
        Actor::Hook,
        call.session.task_id,
        Some(&call.session.id),
        EVENT_PROMPT,
        &json!({ "cross_session": text.starts_with("<cross-session-message"), "from": from }),
    )?;
    let files = rules::current(store, call.env.home, call.env.config, call.env.host)?;
    let version = store.observe_rules(files.hash(), Actor::Hook)?;
    if call.session.rules_seen >= version {
        return Ok(HookReply::default());
    }
    let (rules, version) = rules_for(store, call)?;
    store.set_rules_seen(&call.session.id, call.session.task_id, version, rules.len())?;
    let text = format!(
        "headwork: your rules changed (version {version}); they replace the earlier ones.\n\n{rules}"
    );
    Ok(HookReply::context(HookEvent::UserPromptSubmit, &text))
}

/// The guard's view of the calling session.
///
/// # Errors
///
/// Fails when the store fails.
pub fn scope_for(
    store: &Store,
    env: &HookEnv<'_>,
    role: Role,
    task: Option<&Task>,
    cwd: &str,
    command: Option<&str>,
) -> Result<Scope> {
    let config = env.config;
    let mut writable: Vec<GuardPath> = config
        .hooks
        .writable
        .iter()
        .map(|dir| GuardPath::new(&slashed(&env.home.expand(dir))))
        .collect();
    writable.push(GuardPath::new(&slashed(env.temp)));
    let mut scope = Scope {
        role: Some(role),
        cwd: cwd.to_owned(),
        default_branches: DEFAULT_BRANCHES
            .iter()
            .map(|branch| (*branch).to_owned())
            .collect(),
        ..Scope::default()
    };
    match role {
        Role::Orchestrator => {
            writable.push(GuardPath::new(&slashed(env.home.root())));
            writable.extend(
                config
                    .hooks
                    .orchestrator_writable
                    .iter()
                    .map(|dir| GuardPath::new(&slashed(&env.home.expand(dir)))),
            );
            for project in config.projects.values() {
                scope.user_only.extend(project.user_only.iter().cloned());
                if let Delivery::Command(command) = &project.deliver {
                    scope.scripts.extend(first_word(command));
                }
                for worktree in [&project.worktree_new, &project.worktree_remove] {
                    if let WorktreeCommand::Command(command) = worktree {
                        scope.scripts.extend(first_word(command));
                    }
                }
            }
            scope.scripts.sort();
            scope.scripts.dedup();
        }
        Role::Worker => {
            if let Some(task) = task {
                if let Some(dir) = task_dir(store, env, task)? {
                    writable.push(GuardPath::new(&slashed(&dir)));
                }
                scope.fable_allowed = store.fable_allowed(task.id)?;
                if let Some(project) = config.project(task.project()) {
                    scope.read_only = project
                        .read_only_paths
                        .iter()
                        .map(|dir| GuardPath::new(&slashed(&env.home.expand(dir))))
                        .collect();
                    scope.user_only.clone_from(&project.user_only);
                    scope.long_jobs.clone_from(&project.long_jobs);
                    scope.own_delivery = matches!(project.deliver, Delivery::Command(_));
                    scope.allowed_repos = project
                        .allowed_repos
                        .iter()
                        .map(|repo| repo.as_str().to_owned())
                        .collect();
                }
            }
            if let Some(command) = command
                && command.contains("git")
            {
                scope.branch = env.git.branch(cwd);
                if command.contains("push") {
                    scope.remotes = env.git.remotes(cwd);
                }
            }
        }
    }
    scope.writable = writable;
    Ok(scope)
}

fn first_word(command: &str) -> Option<String> {
    command.split_whitespace().next().map(str::to_owned)
}

fn tool_call<'a>(name: &'a str, input: &'a Value) -> ToolCall<'a> {
    let text = |key: &str| input.get(key).and_then(Value::as_str);
    match name {
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => text("file_path")
            .or_else(|| text("notebook_path"))
            .map_or(ToolCall::Other, |path| ToolCall::Write { tool: name, path }),
        "Bash" => text("command").map_or(ToolCall::Other, |command| ToolCall::Bash {
            command,
            background: input
                .get("run_in_background")
                .and_then(Value::as_bool)
                .unwrap_or_default(),
        }),
        "Skill" => text("skill").map_or(ToolCall::Other, |name| ToolCall::Skill { name }),
        "Agent" | "Task" => ToolCall::Agent {
            tool: name,
            model: text("model"),
        },
        "Workflow" => ToolCall::Agent {
            tool: name,
            model: text("script")
                .filter(|script| {
                    [
                        "model: 'fable'",
                        "model: \"fable\"",
                        "model:'fable'",
                        "model:\"fable\"",
                    ]
                    .iter()
                    .any(|form| script.contains(form))
                })
                .map(|_| "fable"),
        },
        _ => ToolCall::Other,
    }
}

fn questions(input: &Value) -> Vec<(String, String, Vec<String>)> {
    input
        .get("questions")
        .and_then(Value::as_array)
        .map(|questions| {
            questions
                .iter()
                .map(|question| {
                    let text = |key: &str| {
                        question
                            .get(key)
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned()
                    };
                    let options = question
                        .get("options")
                        .and_then(Value::as_array)
                        .map(|options| {
                            options
                                .iter()
                                .filter_map(|option| {
                                    option
                                        .get("label")
                                        .and_then(Value::as_str)
                                        .or_else(|| option.as_str())
                                        .map(str::to_owned)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    (text("question"), text("header"), options)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn pre_tool(store: &mut Store, call: &Call<'_>) -> Result<HookReply> {
    let tool = call.text("tool_name").unwrap_or_default();
    let empty = Value::Null;
    let tool_input = call.input.get("tool_input").unwrap_or(&empty);
    let decision = if tool == "AskUserQuestion" {
        ask_decision(store, call, tool_input)?
    } else {
        let parsed = tool_call(tool, tool_input);
        let command = match parsed {
            ToolCall::Bash { command, .. } => Some(command),
            ToolCall::Write { .. }
            | ToolCall::Skill { .. }
            | ToolCall::Agent { .. }
            | ToolCall::Other => None,
        };
        let mut scope = scope_for(
            store,
            call.env,
            call.session.role,
            call.task,
            &call.cwd(),
            command,
        )?;
        if let Some(pad) = call.text("scratchpad_dir") {
            scope.writable.push(GuardPath::new(pad));
        }
        check_tool(&scope, &parsed)
    };
    let reply = decide(
        store,
        call,
        HookEvent::PreToolUse,
        tool,
        summary(tool, tool_input).as_deref(),
        &decision,
    )?;
    if tool == "AskUserQuestion" && reply.exit == 0 {
        open_ask(store, call, tool_input)?;
    }
    if tool == "SendMessage"
        && reply.exit == 0
        && let Some(input) = readdress(store, call, tool_input)?
    {
        return Ok(HookReply {
            stdout: Some(
                json!({
                    "hookSpecificOutput": {
                        "hookEventName": HookEvent::PreToolUse.claude_name(),
                        "permissionDecision": "allow",
                        "permissionDecisionReason": "headwork: sent to the orchestrator's current address",
                        "updatedInput": input,
                    }
                })
                .to_string(),
            ),
            ..reply
        });
    }
    Ok(reply)
}

/// A session message to `orchestrator` or `root`, the orchestrator's old name, goes to the orchestrator's live address
/// when that is the other one: workers told one name keep reaching an orchestrator started under the other. Returns
/// the tool input with `to` replaced, or none when nothing changes.
fn readdress(store: &Store, call: &Call<'_>, tool_input: &Value) -> Result<Option<Value>> {
    let Some(to) = tool_input.get("to").and_then(Value::as_str) else {
        return Ok(None);
    };
    if !is_orchestrator_address(to) {
        return Ok(None);
    }
    let address = rules::orchestrator_address(store, call.env.host)?;
    if address == to {
        return Ok(None);
    }
    store.append_event(
        Actor::Hook,
        call.session.task_id,
        Some(&call.session.id),
        EVENT_READDRESSED,
        &json!({ "from": to, "to": address }),
    )?;
    let mut input = tool_input.clone();
    if let Some(fields) = input.as_object_mut() {
        fields.insert("to".to_owned(), Value::String(address));
    }
    Ok(Some(input))
}

fn ask_decision(store: &Store, call: &Call<'_>, tool_input: &Value) -> Result<Decision> {
    let config = call.env.config;
    let project = call.task.and_then(|task| config.project(task.project()));
    let push = project
        .and_then(|project| project.push)
        .unwrap_or(config.machine.push);
    let push = match push {
        PushMode::None => Precondition::NotRequired,
        PushMode::Claude
            if store.used_tool_within(
                &call.session.id,
                "PushNotification",
                config.hooks.push_window_secs,
            )? =>
        {
            Precondition::Met
        }
        PushMode::Claude => Precondition::Missing,
    };
    let review_file = match project {
        Some(project) if project.review.send_file => {
            if store.used_tool_within(
                &call.session.id,
                "SendUserFile",
                config.hooks.file_window_secs,
            )? {
                Precondition::Met
            } else {
                Precondition::Missing
            }
        }
        Some(_) | None => Precondition::NotRequired,
    };
    let parsed = questions(tool_input);
    let questions: Vec<Question<'_>> = parsed
        .iter()
        .map(|(_, header, options)| Question {
            header,
            options: options.iter().map(String::as_str).collect(),
        })
        .collect();
    Ok(check_ask(&questions, AskFacts { push, review_file }))
}

fn open_ask(store: &mut Store, call: &Call<'_>, tool_input: &Value) -> Result<()> {
    let parsed = questions(tool_input);
    let Some((question, header, options)) = parsed.first() else {
        return Ok(());
    };
    let kind = if header.trim().eq_ignore_ascii_case("review") {
        AskKind::Review
    } else {
        AskKind::Question
    };
    store.open_ask(
        &NewAsk {
            task_id: call.session.task_id,
            session_id: Some(call.session.id.clone()),
            kind,
            question: question.clone(),
            options: options.clone(),
        },
        Actor::Hook,
    )?;
    if let Some(task) = call.task {
        lenient(store.transition(task.id, &Trigger::AskOpened, Actor::Hook))?;
    }
    if let Some(surface) = &call.session.host_surface {
        let choices: Vec<Choice> = options
            .iter()
            .zip(1..)
            .map(|(label, key)| Choice {
                id: format!("option-{key}"),
                label: label.clone(),
                key: key.to_string(),
            })
            .collect();
        let shown = call
            .env
            .host
            .show_blocked(&SurfaceId(surface.clone()), question, &choices);
        if let Err(error) = shown {
            note(store, call, &json!({ "blocked-state": error.to_string() }))?;
        }
    }
    Ok(())
}

fn post_tool(store: &mut Store, call: &Call<'_>) -> Result<HookReply> {
    let tool = call.text("tool_name").unwrap_or_default();
    if !TRACKED_TOOLS.contains(&tool) {
        return Ok(HookReply::default());
    }
    store.append_event(
        Actor::Hook,
        call.session.task_id,
        Some(&call.session.id),
        EVENT_TOOL,
        &json!({ "tool": tool }),
    )?;
    if tool == "AskUserQuestion" {
        let response = call
            .input
            .get("tool_response")
            .cloned()
            .unwrap_or(Value::Null);
        let answer = response.get("answers").cloned().unwrap_or(response);
        let answer = answer
            .as_str()
            .map_or_else(|| answer.to_string(), str::to_owned);
        if let Some(ask) = store.open_ask_of(&call.session.id)? {
            store.answer_ask(ask, &answer, Actor::Hook)?;
        }
        if let Some(task) = call.task {
            lenient(store.transition(task.id, &Trigger::AskAnswered, Actor::Hook))?;
        }
        if let Some(surface) = &call.session.host_surface
            && let Err(error) = call.env.host.clear_blocked(&SurfaceId(surface.clone()))
        {
            note(store, call, &json!({ "blocked-state": error.to_string() }))?;
        }
    }
    Ok(HookReply::default())
}

fn stop(store: &Store, call: &Call<'_>) -> Result<HookReply> {
    let turn = store.turn_so_far(&call.session.id)?;
    let transcript_message = if call.text("last_assistant_message").is_none() {
        call.text("transcript_path")
            .and_then(|path| last_assistant_text(Path::new(path)))
    } else {
        None
    };
    let last_message = call
        .text("last_assistant_message")
        .or(transcript_message.as_deref());
    let facts = StopFacts {
        role: call.session.role,
        task: call.task.map(|task| task.state),
        blocked_in_row: turn.stops_denied,
        asked: turn.asked,
        messaged: turn.messaged,
        reported: turn.reported,
        keep: call.task.is_some_and(|task| task.keep),
        last_message,
    };
    let decision = check_stop(&facts);
    let tail = last_message.map(|message| tail_chars(message, 200));
    decide(store, call, HookEvent::Stop, "", tail.as_deref(), &decision)
}

fn session_end(store: &mut Store, call: &Call<'_>) -> Result<HookReply> {
    store.append_event(
        Actor::Hook,
        call.session.task_id,
        Some(&call.session.id),
        "session-end",
        &json!({ "reason": call.text("reason") }),
    )?;
    store.set_session_state(&call.session.id, SessionState::Ended, Actor::Hook)?;
    Ok(HookReply::default())
}

/// Logs a deny and turns it into the reply: blocking when enforced, silent in shadow mode.
fn decide(
    store: &Store,
    call: &Call<'_>,
    event: HookEvent,
    tool: &str,
    input: Option<&str>,
    decision: &Decision,
) -> Result<HookReply> {
    let Some(verdict) = decision.verdict() else {
        return Ok(HookReply::default());
    };
    let enforce = call.env.config.hooks.enforce;
    store.append_event(
        Actor::Hook,
        call.session.task_id,
        Some(&call.session.id),
        if enforce {
            EVENT_DENY
        } else {
            EVENT_WOULD_DENY
        },
        &json!({
            "hook": event.claude_name(),
            "tool": tool,
            "rule": verdict.rule,
            "reason": verdict.reason,
            "input": input,
        }),
    )?;
    if !enforce {
        return Ok(HookReply::default());
    }
    Ok(HookReply {
        stdout: None,
        stderr: Some(format!(
            "headwork rule {}: {}",
            verdict.rule, verdict.reason
        )),
        exit: 2,
    })
}

fn summary(tool: &str, input: &Value) -> Option<String> {
    let text = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .map(|value| tail_chars(value, 300))
    };
    match tool {
        "Bash" => text("command"),
        "Edit" | "Write" | "MultiEdit" => text("file_path"),
        "NotebookEdit" => text("notebook_path"),
        "Skill" => text("skill"),
        "Agent" | "Task" => text("model"),
        "AskUserQuestion" => input
            .get("questions")
            .map(|questions| tail_chars(&questions.to_string(), 300)),
        _ => None,
    }
}

fn tail_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    text.chars().skip(count - max).collect()
}

fn note(store: &Store, call: &Call<'_>, data: &Value) -> Result<()> {
    store.append_event(
        Actor::Hook,
        call.session.task_id,
        Some(&call.session.id),
        "note",
        data,
    )?;
    Ok(())
}

fn lenient<T>(result: Result<T, StoreError>) -> Result<()> {
    match result {
        Ok(_) | Err(StoreError::Refused { .. }) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// The text of the last assistant message in a transcript, read from its last 256 KiB.
#[must_use]
pub fn last_assistant_text(path: &Path) -> Option<String> {
    const TAIL: u64 = 256 * 1024;
    let mut file = std::fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(TAIL)))
        .ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    text.lines().rev().find_map(|line| {
        let entry: Value = serde_json::from_str(line).ok()?;
        if entry.get("type").and_then(Value::as_str) != Some("assistant") {
            return None;
        }
        let content = entry.get("message")?.get("content")?.as_array()?;
        let texts: Vec<&str> = content
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect();
        (!texts.is_empty()).then(|| texts.join("\n"))
    })
}

/// Reads a hook's input from the event JSON, tolerating an empty stdin.
///
/// # Errors
///
/// Fails on malformed JSON.
pub fn parse_input(text: &str) -> Result<Value> {
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(text).context("reading the hook's JSON input")
}
