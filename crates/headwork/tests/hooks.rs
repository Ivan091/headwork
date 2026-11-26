//! The hooks in-process, against the fake host, a fixed git probe, a temporary home and a temporary git repo: rules
//! injection and push, the role guard in shadow and enforced mode, the ask round trip, the stop gate and session end.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use headwork::home::Home;
use headwork::ops::Env;
use headwork::ops::hook::{GitProbe, HookEnv, HookEvent, HookReply, run_hook};
use headwork::ops::mcp::{Identity, Server};
use headwork::ops::rules::{
    ASK_BEFORE_CLOSE_HEADING, CLOSING_HEADING, ORCHESTRATOR_RULES, PLAIN_LEAD_HEADING, RuleFiles,
    WORKER_RULES, platform_words,
};
use headwork::ops::spawn::{SpawnRequest, spawn};
use headwork::ops::update::{Notice, UpdateState};
use headwork::ops::waiting::FakeSlots;
use headwork_adapters::git::{FakeGit, PrInfo, PrState};
use headwork_adapters::host::fake::FakeHost;
use headwork_core::review::{Level, ReviewTrigger};
use headwork_core::usage::{Tokens, UsageLine};
use headwork_core::{
    Actor, CloseMode, Config, Model, PermissionMode, Platform, ProjectName, ReviewState, Role,
    SILENT_KIND, SessionState, Slug, TaskRequest, TaskState, Trigger,
};
use headwork_store::{
    EventFilter, EventRecord, FileMark, NewSession, Store, Task, UsageBatch, UsageTag,
};
use serde_json::{Value, json};

const EXE: &str = "S:/bin/headwork.exe";

#[derive(Debug)]
struct FixedGit(&'static str);

impl GitProbe for FixedGit {
    fn branch(&self, _dir: &str) -> Option<String> {
        Some(self.0.to_owned())
    }

    fn remotes(&self, _dir: &str) -> Vec<(String, String)> {
        vec![("origin".to_owned(), "Ivan091/demo".to_owned())]
    }
}

struct World {
    dir: tempfile::TempDir,
    home: Home,
    config: Config,
    store: Store,
    host: FakeHost,
    git: FixedGit,
    prs: FakeGit,
    task: Task,
    session: String,
    slots: FakeSlots,
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "git {args:?}");
}

fn world(hooks: &str, keep: bool) -> World {
    world_in(hooks, "", keep)
}

/// A world whose `[hooks]` and project tables get these extra lines, with `@DIR@` standing for its temporary folder.
fn world_in(hooks: &str, project: &str, keep: bool) -> World {
    world_of(hooks, project, keep, "task")
}

/// A world as [`world_in`] whose task is of this kind.
fn world_of(hooks: &str, project: &str, keep: bool, kind: &str) -> World {
    world_flagged(
        hooks,
        project,
        keep,
        kind,
        CloseMode::default_for(kind),
        None,
    )
}

/// A world as [`world_of`] whose task has this close mode and planned review.
fn world_flagged(
    hooks: &str,
    project: &str,
    keep: bool,
    kind: &str,
    close: CloseMode,
    review: Option<Level>,
) -> World {
    let dir = tempfile::tempdir().unwrap();
    let dir_text = dir.path().display().to_string().replace('\\', "/");
    let hooks = hooks.replace("@DIR@", &dir_text);
    let project = project.replace("@DIR@", &dir_text);
    let main = dir.path().join("demo");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "master"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let main_text = main.display().to_string().replace('\\', "/");
    let text = format!(
        "[machine]\nhost = \"wmux\"\n\n[hooks]\nplugin_dir = \"S:/plugin\"\n{hooks}\n\n[project.demo]\nmain = \"{main_text}\"\n\
         worktree_new = \"builtin\"\nworktree_remove = \"builtin\"\nworktree_path = \"{{main}}-{{slug}}\"\n\
         deliver = \"scripts/ship.sh\"\nmerge = \"deliver\"\nallowed_repos = [\"Ivan091/demo\"]\n\
         user_only = [\"scripts/publish.sh\"]\n{project}\n"
    );
    let config = Config::parse(&text, "test").unwrap();
    let home = Home::at(Some(dir.path().join("home")))
        .unwrap()
        .with_claude_json(dir.path().join(".claude.json"))
        .with_claude_dir(dir.path().join(".claude"));
    let mut store = home.open_store().unwrap();
    store.sync_projects(&config).unwrap();
    let brief = dir.path().join("brief.md");
    std::fs::write(&brief, "# Task\n\nDo the thing.\n").unwrap();
    let host = FakeHost::new();
    let request = SpawnRequest {
        request: TaskRequest {
            project: ProjectName::new("demo").unwrap(),
            slug: Slug::new("sand").unwrap(),
            kind: kind.to_owned(),
            model: Some(Model::Haiku),
            effort: None,
            in_main: config
                .project(&ProjectName::new("demo").unwrap())
                .is_some_and(|demo| demo.is_main_only(kind)),
            keep,
            close,
            review,
            decision: None,
        },
        brief: &brief,
        depends_on: None,
        permission_mode: PermissionMode::Auto,
        allow_ask: &[],
    };
    let env = Env {
        home: &home,
        config: &config,
        host: &host,
        exe: Path::new(EXE),
        actor: Actor::User,
    };
    let (task, _) = spawn(&mut store, &env, &request).unwrap();
    let session = store.session_of(task.id).unwrap().unwrap().id;
    World {
        dir,
        home,
        config,
        store,
        host,
        git: FixedGit("sand"),
        prs: FakeGit::new(),
        task,
        session,
        slots: FakeSlots::hook_of_claude(false),
    }
}

impl World {
    fn worktree(&self) -> PathBuf {
        self.dir.path().join("demo-sand")
    }

    fn hook(&mut self, event: HookEvent, input: &Value) -> HookReply {
        let temp = self.dir.path().join("temp");
        let env = HookEnv {
            home: &self.home,
            config: &self.config,
            host: &self.host,
            git: &self.git,
            exe: Path::new(EXE),
            temp: &temp,
            slots: &self.slots,
        };
        let session = self.session.clone();
        run_hook(&mut self.store, &env, event, &session, input).unwrap()
    }

    fn start(&mut self) -> HookReply {
        let input = json!({ "session_id": self.session, "source": "startup" });
        self.hook(HookEvent::SessionStart, &input)
    }

    fn pre(&mut self, tool: &str, tool_input: &Value) -> HookReply {
        let cwd = self.worktree().display().to_string();
        let input = json!({ "tool_name": tool, "tool_input": tool_input, "cwd": cwd });
        self.hook(HookEvent::PreToolUse, &input)
    }

    fn post(&mut self, tool: &str, response: &Value) -> HookReply {
        let input = json!({ "tool_name": tool, "tool_input": {}, "tool_response": response });
        self.hook(HookEvent::PostToolUse, &input)
    }

    fn events(&self) -> Vec<EventRecord> {
        self.store
            .events(EventFilter {
                task: Some(self.task.id),
                limit: 200,
            })
            .unwrap()
    }

    fn denies(&self) -> Vec<(String, String)> {
        self.events()
            .into_iter()
            .filter(|event| event.kind == "deny" || event.kind == "would-deny")
            .map(|event| {
                let rule = event.data["rule"].as_str().unwrap_or_default().to_owned();
                (event.kind, rule)
            })
            .collect()
    }

    fn state(&self) -> TaskState {
        self.store.task(self.task.id).unwrap().state
    }
}

fn context(reply: &HookReply) -> String {
    let body: Value = serde_json::from_str(reply.stdout.as_deref().unwrap()).unwrap();
    body["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn ask_input(options: &[&str], header: &str) -> Value {
    let options: Vec<Value> = options
        .iter()
        .map(|label| json!({ "label": label, "description": "d" }))
        .collect();
    json!({ "questions": [{ "question": "Ship?", "header": header, "options": options, "multiSelect": false }] })
}

#[test]
fn with_the_plugin_the_brief_holds_only_the_task() {
    let w = world("", false);
    let brief = std::fs::read_to_string(w.home.brief_path(&w.task.name)).unwrap();
    assert_eq!(brief, "# Task\n\nDo the thing.\n", "task text only");
}

#[test]
fn session_start_marks_the_task_working_and_injects_the_rules() {
    let mut w = world("", false);
    assert_eq!(w.state(), TaskState::Spawning, "before");
    let reply = w.start();
    assert_eq!(reply.exit, 0, "allowed");
    let text = context(&reply);
    assert!(text.contains("worker session `demo-sand`"), "{text}");
    assert!(
        text.contains(&format!("`\"{EXE}\" close demo-sand`")),
        "{text}"
    );
    assert!(
        text.contains("`scripts/ship.sh` from your folder"),
        "{text}"
    );
    assert!(text.contains("in shadow mode"), "{text}");
    assert!(
        text.contains(Platform::current().stray_shells()),
        "this OS's words: {text}"
    );
    assert!(!text.contains("{stray}"), "placeholders filled: {text}");
    assert_eq!(w.state(), TaskState::Working, "after");
    let session = w.store.hook_session(&w.session).unwrap().unwrap();
    assert_eq!(session.rules_seen, 1, "rules seen");
    assert_eq!(session.state, SessionState::Live, "live");
}

#[test]
fn hooks_record_the_permission_mode_the_session_is_in() {
    let mut w = world("", false);
    w.start();
    let mode = |w: &World| {
        w.store
            .session(&w.session)
            .unwrap()
            .unwrap()
            .0
            .permission_mode
    };
    assert_eq!(mode(&w), None, "nothing reported yet");
    let cwd = w.worktree().display().to_string();
    for (reported, expected) in [
        ("plan", Some(PermissionMode::Plan)),
        ("bypassPermissions", Some(PermissionMode::Plan)),
        ("auto", Some(PermissionMode::Auto)),
    ] {
        let input = json!({
            "tool_name": "Read",
            "tool_input": { "file_path": "x" },
            "cwd": cwd,
            "permission_mode": reported,
        });
        w.hook(HookEvent::PreToolUse, &input);
        assert_eq!(mode(&w), expected, "after {reported}");
    }
}

#[test]
fn the_built_in_rules_name_the_programs_of_their_platform() {
    for rules in [ORCHESTRATOR_RULES, WORKER_RULES] {
        let windows = platform_words(rules, Platform::Windows);
        assert!(windows.contains("`tail.exe` or `bash.exe`"), "{windows}");
        assert!(windows.contains("`taskkill`"), "{windows}");
        for platform in [Platform::MacOs, Platform::Unix] {
            let text = platform_words(rules, platform);
            assert!(text.contains("`tail` or `bash`"), "{text}");
            assert!(text.contains("process-group kill"), "{text}");
            for word in [
                "taskkill",
                "tail.exe",
                "bash.exe",
                "schtasks",
                "scheduled task",
                "{stray}",
                "{kill}",
                "{tick_install}",
            ] {
                assert!(!text.contains(word), "{platform:?} {word}: {text}");
            }
        }
    }
    let windows = platform_words(ORCHESTRATOR_RULES, Platform::Windows);
    assert!(
        windows.contains("creates the Windows scheduled task \"headwork tick\""),
        "{windows}"
    );
    let mac = platform_words(ORCHESTRATOR_RULES, Platform::MacOs);
    assert!(
        mac.contains("loads the launchd agent `com.headwork.tick`"),
        "{mac}"
    );
    let unix = platform_words(ORCHESTRATOR_RULES, Platform::Unix);
    assert!(unix.contains("has no scheduler for it yet"), "{unix}");
}

fn record_context(w: &mut World, id: &str, at: &str, cache_read: u64) {
    let line = UsageLine {
        message_id: id.to_owned(),
        at: at.to_owned(),
        model: "claude-opus-5-5".to_owned(),
        effort: None,
        speed: None,
        service_tier: None,
        cwd: None,
        tokens: Tokens {
            input: 10,
            cache_read,
            ..Tokens::default()
        },
    };
    let tag = w
        .store
        .usage_tag(&w.session)
        .unwrap()
        .unwrap_or(UsageTag::UNMANAGED);
    w.store
        .record_usage(&UsageBatch {
            path: &format!("{id}.jsonl"),
            session_id: &w.session,
            agent_id: None,
            agent_meta: None,
            tag,
            dir: "S:/Code/x",
            lines: &[line],
            mark: FileMark {
                read_to: 1,
                size: 1,
                mtime_ms: 1,
            },
        })
        .unwrap();
}

#[test]
fn a_long_session_is_nudged_once_per_step_of_the_context_threshold() {
    let mut w = world("", false);
    w.start();
    let prompt = json!({ "prompt": "go" });
    record_context(&mut w, "m1", "2026-11-23T10:00:00Z", 150_000);
    assert_eq!(
        w.hook(HookEvent::UserPromptSubmit, &prompt).stdout,
        None,
        "below"
    );
    record_context(&mut w, "m2", "2026-11-23T10:01:00Z", 250_000);
    let text = context(&w.hook(HookEvent::UserPromptSubmit, &prompt));
    assert!(text.contains("your context is 250k tokens"), "{text}");
    assert!(
        text.contains("`general-purpose` subagent on Sonnet"),
        "{text}"
    );
    assert_eq!(
        w.hook(HookEvent::UserPromptSubmit, &prompt).stdout,
        None,
        "same step"
    );
    record_context(&mut w, "m3", "2026-11-23T10:02:00Z", 420_000);
    let text = context(&w.hook(HookEvent::UserPromptSubmit, &prompt));
    assert!(text.contains("your context is 420k tokens"), "{text}");
    let nudges = w
        .events()
        .iter()
        .filter(|event| event.kind == "context-nudge")
        .count();
    assert_eq!(nudges, 2, "one event per step");
}

#[test]
fn the_context_nudge_is_off_at_zero() {
    let mut w = world("", false);
    w.start();
    w.config.usage.nudge_context_k = 0;
    record_context(&mut w, "m1", "2026-11-23T10:00:00Z", 900_000);
    let reply = w.hook(HookEvent::UserPromptSubmit, &json!({ "prompt": "go" }));
    assert_eq!(reply.stdout, None, "off");
}

#[test]
fn a_keep_task_gets_no_closing_section() {
    let mut w = world("", true);
    let text = context(&w.start());
    assert!(!text.contains(CLOSING_HEADING), "{text}");
    assert!(!text.contains("close demo-sand"), "{text}");
}

#[test]
fn changed_rules_reach_a_running_session_on_its_next_prompt_once() {
    let mut w = world("", false);
    w.start();
    let prompt = json!({ "prompt": "hello" });
    assert_eq!(
        w.hook(HookEvent::UserPromptSubmit, &prompt).stdout,
        None,
        "unchanged"
    );
    let rules = w.home.root().join("rules");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(rules.join("global.md"), "Global rule PAPAYA.\n").unwrap();
    let message = json!({
        "prompt": "<cross-session-message from=\"uds:x\" from-name=\"root\" from-mode=\"prompting\">hi</cross-session-message>"
    });
    let text = context(&w.hook(HookEvent::UserPromptSubmit, &message));
    assert!(text.contains("rules changed (version 2)"), "{text}");
    assert!(text.contains("PAPAYA"), "{text}");
    assert_eq!(
        w.hook(HookEvent::UserPromptSubmit, &prompt).stdout,
        None,
        "pushed once"
    );
    let from_root = w
        .events()
        .into_iter()
        .any(|event| event.kind == "prompt" && event.data["from"] == "root");
    assert!(from_root, "the message is recorded with its sender");
}

#[test]
fn shadow_mode_logs_what_it_would_deny_and_allows() {
    let mut w = world("", false);
    w.start();
    w.git = FixedGit("master");
    let reply = w.pre("Bash", &json!({ "command": "git commit -m x" }));
    assert_eq!(reply, HookReply::default(), "allowed in shadow mode");
    let outside = w.dir.path().join("demo").join("src.rs");
    let path = outside.display().to_string();
    w.pre("Write", &json!({ "file_path": path, "content": "x" }));
    w.pre("Bash", &json!({ "command": "bash scripts/publish.sh" }));
    w.pre("Skill", &json!({ "skill": "ship" }));
    let inside = w.worktree().join("src.rs").display().to_string();
    w.pre("Edit", &json!({ "file_path": inside }));
    w.pre("Bash", &json!({ "command": "git status" }));
    let rules: Vec<(String, String)> = [
        "worker-default-branch",
        "worker-edit-outside",
        "user-only",
        "worker-global-skill",
    ]
    .iter()
    .map(|rule| ("would-deny".to_owned(), (*rule).to_owned()))
    .collect();
    assert_eq!(w.denies(), rules, "every rule logged once, allows silent");
}

#[test]
fn a_worker_writes_its_helper_worktrees_and_subagent_worktrees_but_not_others() {
    let mut w = world("", false);
    w.start();
    let helper = w.dir.path().join("demo-sand-part");
    std::fs::create_dir(&helper).unwrap();
    let helper_text = helper.display().to_string().replace('\\', "/");
    w.store
        .record_helper_worktree(w.task.id, &helper_text, "sand-part", Actor::Worker)
        .unwrap();
    let subagent = w
        .dir
        .path()
        .join("demo")
        .join(".claude")
        .join("worktrees")
        .join("agent-1")
        .join("a.rs");
    let other = w.dir.path().join("demo-other").join("a.rs");
    for path in [helper.join("a.rs"), subagent] {
        w.pre(
            "Write",
            &json!({ "file_path": path.display().to_string(), "content": "x" }),
        );
    }
    assert!(w.denies().is_empty(), "both writable: {:?}", w.denies());
    w.pre(
        "Write",
        &json!({ "file_path": other.display().to_string(), "content": "x" }),
    );
    assert_eq!(
        w.denies(),
        [("would-deny".to_owned(), "worker-edit-outside".to_owned())],
        "another folder of the project is not"
    );
}

#[test]
fn a_worker_closes_only_itself_and_writes_git_only_in_its_folders() {
    let mut w = world("", false);
    w.start();
    let main = w.dir.path().join("demo").display().to_string();
    for command in [
        format!("\"{EXE}\" close demo-sand"),
        "headwork board".to_owned(),
        format!("git -C {main} status"),
        "git commit -m x".to_owned(),
        "headwork close demo-other".to_owned(),
        format!("{EXE} spawn demo x brief.md"),
        format!("git -C {main} commit -m x"),
        format!("cd {main} && git push"),
    ] {
        w.pre("Bash", &json!({ "command": command }));
    }
    let rules: Vec<(String, String)> = [
        "worker-admin",
        "worker-admin",
        "worker-git-outside",
        "worker-git-outside",
    ]
    .iter()
    .map(|rule| ("would-deny".to_owned(), (*rule).to_owned()))
    .collect();
    assert_eq!(w.denies(), rules, "own close and reads pass");
}

#[test]
fn enforced_denies_exit_2_with_the_reason() {
    let mut w = world("enforce = true", false);
    w.start();
    w.git = FixedGit("master");
    let reply = w.pre("Bash", &json!({ "command": "git push" }));
    assert_eq!(reply.exit, 2, "blocked");
    let reason = reply.stderr.unwrap();
    assert!(
        reason.starts_with("headwork rule worker-default-branch: "),
        "{reason}"
    );
    assert_eq!(
        w.denies(),
        [("deny".to_owned(), "worker-default-branch".to_owned())],
        "logged as a deny"
    );
    w.git = FixedGit("sand");
    assert_eq!(
        w.pre("Bash", &json!({ "command": "git push -u origin sand" })),
        HookReply::default(),
        "a task branch push passes"
    );
}

#[test]
fn an_ask_opens_blocks_the_pane_and_closes_with_the_answer() {
    let mut w = world("", false);
    w.start();
    w.post("PushNotification", &json!({}));
    let reply = w.pre(
        "AskUserQuestion",
        &ask_input(&["Ship it (Recommended)", "Drop it"], "Ship"),
    );
    assert_eq!(reply, HookReply::default(), "a good ask");
    assert!(w.denies().is_empty(), "{:?}", w.denies());
    assert_eq!(w.state(), TaskState::NeedsUser, "waiting on the user");
    let surface = w
        .store
        .session_of(w.task.id)
        .unwrap()
        .unwrap()
        .host_surface
        .unwrap();
    assert!(
        w.host
            .calls()
            .contains(&format!("blocked {surface} Ship? (2 choices)")),
        "{:?}",
        w.host.calls()
    );
    w.post(
        "AskUserQuestion",
        &json!({ "answers": { "Ship?": "Ship it (Recommended)" } }),
    );
    assert_eq!(w.state(), TaskState::Working, "back to work");
    assert!(
        w.host.calls().contains(&format!("unblocked {surface}")),
        "{:?}",
        w.host.calls()
    );
    assert!(
        w.store.open_ask_of(&w.session).unwrap().is_none(),
        "answered"
    );
    let answer = w
        .events()
        .into_iter()
        .find(|event| event.kind == "ask-answered")
        .map(|event| event.data["answer"].as_str().unwrap_or_default().to_owned());
    assert_eq!(
        answer.as_deref(),
        Some(r#"{"Ship?":"Ship it (Recommended)"}"#),
        "the answer is kept"
    );
}

#[test]
fn ask_checks_name_their_rules() {
    let mut w = world("enforce = true", false);
    w.start();
    let missing_push = w.pre(
        "AskUserQuestion",
        &ask_input(&["Ship it (Recommended)", "Drop it"], "Ship"),
    );
    assert_eq!(missing_push.exit, 2, "no push first");
    assert_eq!(w.state(), TaskState::Working, "a denied ask opens nothing");
    w.post("PushNotification", &json!({}));
    let changes = w.pre(
        "AskUserQuestion",
        &ask_input(&["Ship it", "Needs changes"], "Ship"),
    );
    assert_eq!(changes.exit, 2, "a needs-changes option");
    let review = w.pre(
        "AskUserQuestion",
        &ask_input(&["Good (Recommended)", "Drop it"], "Review"),
    );
    assert_eq!(review.exit, 2, "no file sent");
    let rules: Vec<String> = w.denies().into_iter().map(|(_, rule)| rule).collect();
    assert_eq!(
        rules,
        ["ask-push", "ask-changes-option", "ask-review-file"],
        "rules"
    );
}

fn open_asks(w: &World) -> usize {
    w.events()
        .iter()
        .filter(|event| event.kind == "ask-opened")
        .count()
        - w.events()
            .iter()
            .filter(|event| matches!(event.kind.as_str(), "ask-answered" | "ask-superseded"))
            .count()
}

#[test]
fn a_new_ask_or_a_typed_prompt_supersedes_an_abandoned_one() {
    let mut w = world("", false);
    w.start();
    w.post("PushNotification", &json!({}));
    w.pre(
        "AskUserQuestion",
        &ask_input(&["Ship it (Recommended)", "Drop it"], "Ship"),
    );
    w.pre(
        "AskUserQuestion",
        &ask_input(&["Merge (Recommended)", "Wait"], "Merge"),
    );
    assert_eq!(
        open_asks(&w),
        1,
        "the abandoned ask is superseded by the next"
    );
    w.hook(
        HookEvent::UserPromptSubmit,
        &json!({ "prompt": "<cross-session-message from-name=\"orchestrator\">status?</cross-session-message>" }),
    );
    assert_eq!(
        open_asks(&w),
        1,
        "a message from another session answers nothing"
    );
    assert_eq!(w.state(), TaskState::NeedsUser, "still waiting");
    w.hook(
        HookEvent::UserPromptSubmit,
        &json!({ "prompt": "merge it after CI" }),
    );
    assert_eq!(open_asks(&w), 0, "the user typed past the dialog");
    assert!(
        w.store.open_ask_of(&w.session).unwrap().is_none(),
        "no open ask"
    );
    assert_eq!(w.state(), TaskState::Working, "back to work");
}

#[test]
fn an_ask_in_review_keeps_the_task_in_review() {
    let mut w = world("", false);
    w.start();
    w.store
        .transition(
            w.task.id,
            &Trigger::ReviewOpened { repo_allowed: true },
            Actor::Hook,
        )
        .unwrap();
    w.post("PushNotification", &json!({}));
    w.post("SendUserFile", &json!({}));
    w.pre(
        "AskUserQuestion",
        &ask_input(&["Good (Recommended)", "Drop it"], "Review"),
    );
    assert_eq!(
        w.state(),
        TaskState::InReview,
        "in review while the user looks"
    );
    assert!(
        w.store.open_ask_of(&w.session).unwrap().is_some(),
        "the ask waits"
    );
    w.post(
        "AskUserQuestion",
        &json!({ "answers": "Good (Recommended)" }),
    );
    assert_eq!(w.state(), TaskState::InReview, "still in review");
    assert!(
        !w.events().iter().any(|event| event.kind == "refused"),
        "{:?}",
        w.events()
    );
}

#[test]
fn no_push_is_demanded_right_after_the_user_typed() {
    let mut w = world("enforce = true", false);
    w.start();
    w.hook(
        HookEvent::UserPromptSubmit,
        &json!({ "prompt": "<cross-session-message from-name=\"orchestrator\">go</cross-session-message>" }),
    );
    let after_message = w.pre(
        "AskUserQuestion",
        &ask_input(&["Ship it (Recommended)", "Drop it"], "Ship"),
    );
    assert_eq!(
        after_message.exit, 2,
        "a session message does not put the user at the screen"
    );
    w.hook(HookEvent::UserPromptSubmit, &json!({ "prompt": "go" }));
    let after_prompt = w.pre(
        "AskUserQuestion",
        &ask_input(&["Ship it (Recommended)", "Drop it"], "Ship"),
    );
    assert_eq!(
        after_prompt,
        HookReply::default(),
        "the user just typed here"
    );
}

fn allow_ask(command: &str) -> Value {
    json!({ "questions": [{
        "question": format!("May this task run `{command}`?"),
        "header": "Allow",
        "options": [
            { "label": "Allow it for this task (Recommended)", "description": "d" },
            { "label": "Keep it mine", "description": "d" },
        ],
        "multiSelect": false,
    }] })
}

fn answer_allow(w: &mut World, command: &str, answer: &str) {
    let asked = allow_ask(command);
    w.post("PushNotification", &json!({}));
    w.pre("AskUserQuestion", &asked);
    let question = format!("May this task run `{command}`?");
    let input = json!({
        "tool_name": "AskUserQuestion",
        "tool_input": asked,
        "tool_response": { "answers": { question: answer } },
    });
    w.hook(HookEvent::PostToolUse, &input);
}

#[test]
fn an_allow_ask_lets_one_task_run_a_user_only_command() {
    let mut w = world("enforce = true", false);
    w.start();
    let publish = json!({ "command": "bash scripts/publish.sh --all" });
    assert_eq!(w.pre("Bash", &publish).exit, 2, "the user's alone");
    answer_allow(&mut w, "scripts/publish.sh --all", "Keep it mine");
    assert_eq!(w.pre("Bash", &publish).exit, 2, "refused stays refused");
    answer_allow(
        &mut w,
        "scripts/publish.sh --all",
        "Allow it for this task (Recommended)",
    );
    assert_eq!(
        w.pre("Bash", &publish),
        HookReply::default(),
        "allowed for this task"
    );
    assert_eq!(
        w.store.user_only_allowed(w.task.id).unwrap(),
        ["scripts/publish.sh"],
        "recorded once, as the configured pattern"
    );
}

#[test]
fn the_first_action_ask_of_a_spawn_grants_its_command() {
    let mut w = world("enforce = true", false);
    w.start();
    let question = "Allow demo-sand to run `scripts/publish.sh --all`?";
    let asked = json!({ "questions": [{
        "question": question,
        "header": "Allow",
        "options": [
            { "label": "Allow it (Recommended)", "description": "d" },
            { "label": "Do not allow it", "description": "d" },
        ],
        "multiSelect": false,
    }] });
    let publish = json!({ "command": "scripts/publish.sh --all" });
    for (answer, allowed) in [("Do not allow it", false), ("Allow it (Recommended)", true)] {
        w.post("PushNotification", &json!({}));
        assert_eq!(
            w.pre("AskUserQuestion", &asked),
            HookReply::default(),
            "the rendered ask passes the ask checks"
        );
        w.hook(
            HookEvent::PostToolUse,
            &json!({
                "tool_name": "AskUserQuestion",
                "tool_input": asked,
                "tool_response": { "answers": { question: answer } },
            }),
        );
        assert_eq!(
            w.pre("Bash", &publish).exit == 0,
            allowed,
            "after {answer:?}"
        );
    }
}

#[test]
fn the_stop_gate_wants_an_ask_or_a_report() {
    let mut w = world("push_window_secs = 0", false);
    w.start();
    w.hook(HookEvent::UserPromptSubmit, &json!({ "prompt": "go" }));
    w.hook(
        HookEvent::Stop,
        &json!({ "last_assistant_message": "Shall I merge it?", "stop_hook_active": false }),
    );
    w.hook(
        HookEvent::Stop,
        &json!({ "last_assistant_message": "Step 3 done.", "stop_hook_active": true }),
    );
    w.hook(HookEvent::UserPromptSubmit, &json!({ "prompt": "finish" }));
    w.post("SendMessage", &json!({}));
    w.hook(
        HookEvent::Stop,
        &json!({ "last_assistant_message": "Reported to root." }),
    );
    let rules: Vec<String> = w.denies().into_iter().map(|(_, rule)| rule).collect();
    assert_eq!(rules, ["stop-plain-question", "stop-unfinished"], "rules");
}

#[test]
fn a_session_message_logs_its_address_and_summary_but_never_its_body() {
    let mut w = world("", false);
    w.start();
    let long = "x".repeat(300);
    let input = json!({
        "tool_name": "SendMessage",
        "tool_input": { "to": " headwork-lead ", "summary": long, "message": "the secret body" },
        "tool_response": {},
    });
    w.hook(HookEvent::PostToolUse, &input);
    let event = w
        .events()
        .into_iter()
        .rev()
        .find(|event| event.kind == "tool-used")
        .unwrap();
    assert_eq!(event.data["to"], "headwork-lead", "the trimmed address");
    assert_eq!(
        event.data["summary"].as_str().map(str::len),
        Some(120),
        "the summary, cut"
    );
    assert!(!event.data.to_string().contains("secret"), "no body");
}

#[test]
fn tool_used_keeps_only_the_tool_for_other_tools_and_drops_empty_or_odd_message_fields() {
    let mut w = world("", false);
    w.start();
    let cases = [
        (
            "PushNotification",
            json!({ "message": "hi" }),
            json!({ "tool": "PushNotification" }),
        ),
        (
            "SendMessage",
            json!({ "to": "  ", "summary": "" }),
            json!({ "tool": "SendMessage", "to": null, "summary": null }),
        ),
        (
            "SendMessage",
            json!({ "to": "lead\u{1b}[2J", "summary": "ž".repeat(130) }),
            json!({ "tool": "SendMessage", "to": "lead[2J", "summary": "ž".repeat(120) }),
        ),
    ];
    for (tool, tool_input, logged) in cases {
        let input = json!({ "tool_name": tool, "tool_input": tool_input, "tool_response": {} });
        w.hook(HookEvent::PostToolUse, &input);
        let event = w
            .events()
            .into_iter()
            .rev()
            .find(|event| event.kind == "tool-used")
            .unwrap();
        assert_eq!(event.data, logged, "{tool}");
    }
}

#[test]
fn a_plain_reply_to_the_user_who_just_typed_is_no_silent_stop() {
    let mut w = world("", false);
    w.start();
    w.hook(
        HookEvent::UserPromptSubmit,
        &json!({ "prompt": "what does the form want?" }),
    );
    w.hook(
        HookEvent::Stop,
        &json!({ "last_assistant_message": "It wants the privacy URL." }),
    );
    w.hook(
        HookEvent::UserPromptSubmit,
        &json!({ "prompt": "and then?" }),
    );
    w.hook(
        HookEvent::Stop,
        &json!({ "last_assistant_message": "Shall I check it?" }),
    );
    let rules: Vec<String> = w.denies().into_iter().map(|(_, rule)| rule).collect();
    assert_eq!(rules, ["stop-plain-question"], "only the plain question");
}

#[test]
fn the_stop_gate_reads_the_transcript_when_needed() {
    let mut w = world("enforce = true", false);
    w.start();
    let transcript = w.dir.path().join("t.jsonl");
    let lines = [
        json!({ "type": "user", "message": { "content": "hi" } }),
        json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": "Want me to ship it?" }] } }),
    ];
    let text: Vec<String> = lines.iter().map(Value::to_string).collect();
    std::fs::write(&transcript, text.join("\n")).unwrap();
    let path = transcript.display().to_string();
    let reply = w.hook(HookEvent::Stop, &json!({ "transcript_path": path }));
    assert_eq!(reply.exit, 2, "blocked");
    assert!(
        reply.stderr.unwrap().contains("stop-plain-question"),
        "the rule"
    );
}

#[test]
fn session_end_is_recorded() {
    let mut w = world("", false);
    w.start();
    w.hook(
        HookEvent::SessionEnd,
        &json!({ "reason": "prompt_input_exit" }),
    );
    let session = w.store.hook_session(&w.session).unwrap().unwrap();
    assert_eq!(session.state, SessionState::Ended, "ended");
    assert!(
        w.events()
            .iter()
            .any(|event| event.kind == "session-end" && event.data["reason"] == "prompt_input_exit"),
        "reason kept"
    );
}

#[test]
fn a_keep_worker_whose_session_ends_is_interrupted_not_left_working() {
    let mut w = world("", true);
    w.start();
    assert_eq!(w.state(), TaskState::Working, "started");
    w.hook(
        HookEvent::UserPromptSubmit,
        &json!({ "prompt": "<cross-session-message from-name=\"root\">go on</cross-session-message>" }),
    );
    w.hook(HookEvent::SessionEnd, &json!({ "reason": "other" }));
    assert_eq!(
        w.state(),
        TaskState::Interrupted,
        "the ended session is not working"
    );
    let task = w.store.task(w.task.id).unwrap();
    assert_eq!(
        task.state_before_park,
        Some(TaskState::Working),
        "resume returns to working"
    );
    let session = w.store.hook_session(&w.session).unwrap().unwrap();
    assert_eq!(
        session.state,
        SessionState::Ended,
        "the session stays ended"
    );
    assert!(
        w.events().iter().any(|event| event.kind == "transition"
            && event.data["trigger"] == "process-lost"
            && event.data["to"] == "interrupted"),
        "the transition is logged: {:?}",
        w.events()
    );
    assert!(
        w.store.latest_recap(w.task.id).unwrap().is_some(),
        "a recap for the resume"
    );
}

#[test]
fn a_normal_worker_that_exits_without_closing_is_interrupted() {
    let mut w = world("", false);
    w.start();
    w.hook(
        HookEvent::SessionEnd,
        &json!({ "reason": "prompt_input_exit" }),
    );
    assert_eq!(w.state(), TaskState::Interrupted, "interrupted");
}

#[test]
fn clear_and_resume_keep_the_session_and_the_task_running() {
    let mut w = world("", false);
    w.start();
    for reason in ["clear", "resume"] {
        w.hook(HookEvent::SessionEnd, &json!({ "reason": reason }));
        assert_eq!(w.state(), TaskState::Working, "{reason}: still working");
        let session = w.store.hook_session(&w.session).unwrap().unwrap();
        assert_eq!(session.state, SessionState::Live, "{reason}: still live");
    }
}

#[test]
fn a_session_end_during_close_leaves_the_task_closing() {
    let mut w = world("", true);
    w.start();
    w.store
        .transition(w.task.id, &Trigger::ForceClose, Actor::Orchestrator)
        .unwrap();
    w.hook(HookEvent::SessionEnd, &json!({ "reason": "other" }));
    assert_eq!(w.state(), TaskState::Closing, "close owns the task");
    assert!(
        !w.events().iter().any(|event| event.kind == "refused"),
        "nothing to refuse: {:?}",
        w.events()
    );
}

#[test]
fn the_mcp_board_never_shows_a_task_whose_session_ended_as_working() {
    let mut w = world("", true);
    w.start();
    let session = w.session.clone();
    w.store
        .set_session_state(&session, SessionState::Ended, Actor::Hook)
        .unwrap();
    let (is_error, text) = w.tool(orchestrator_identity(), "board", &json!({}));
    assert!(!is_error, "{text}");
    let board: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(board["tasks"][0]["state"], "interrupted", "{text}");
    assert_eq!(w.state(), TaskState::Interrupted, "recorded");
}

#[test]
fn unknown_sessions_are_left_alone() {
    let mut w = world("enforce = true", false);
    w.session = "not-managed".to_owned();
    let reply = w.pre("Bash", &json!({ "command": "git push origin master" }));
    assert_eq!(reply, HookReply::default(), "allowed");
    assert!(w.denies().is_empty(), "nothing logged");
}

#[test]
fn root_rules_name_the_binary_and_fit_the_hook_limit() {
    let w = world("", false);
    let files = RuleFiles::load(&w.home, &w.config, &[], "orchestrator").unwrap();
    let text = files.render(Role::Orchestrator, &w.config, Path::new(EXE), None);
    assert!(text.contains(&format!("\"{EXE}\" spawn")), "{text}");
    assert!(text.len() < 10_000, "{}", text.len());
}

#[test]
fn orchestrator_rules_keep_a_margin_under_the_hook_limit_with_long_paths() {
    let w = world("", false);
    let files = RuleFiles::load(&w.home, &w.config, &[], "orchestrator").unwrap();
    let long_exe = format!("S:/{}/headwork.exe", "d".repeat(80));
    let text = files.render(Role::Orchestrator, &w.config, Path::new(&long_exe), None);
    assert!(text.contains(&format!("\"{long_exe}\" spawn")), "{text}");
    assert!(text.len() < 9_400, "{}", text.len());
}

fn worker_identity(w: &World) -> Identity {
    Identity {
        session: Some(w.session.clone()),
        role: Some(Role::Worker),
    }
}

fn orchestrator_identity() -> Identity {
    Identity {
        session: Some("root-session".to_owned()),
        role: Some(Role::Orchestrator),
    }
}

impl World {
    fn server(&mut self, identity: Identity) -> Server<'_> {
        let env = Env {
            home: &self.home,
            config: &self.config,
            host: &self.host,
            exe: Path::new(EXE),
            actor: Actor::User,
        };
        Server::new(&mut self.store, env, &self.prs, identity)
    }

    fn rpc(&mut self, identity: Identity, message: &Value) -> Value {
        self.server(identity).handle(message).unwrap()
    }

    fn tool(&mut self, identity: Identity, name: &str, arguments: &Value) -> (bool, String) {
        let message = json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        let result = self.rpc(identity, &message)["result"].clone();
        let text = result["content"][0]["text"].as_str().unwrap().to_owned();
        (result["isError"].as_bool().unwrap(), text)
    }

    fn tool_names(&mut self, identity: Identity) -> Vec<String> {
        let message = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
        let reply = self.rpc(identity, &message);
        reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect()
    }
}

#[test]
fn mcp_initialize_is_golden() {
    let mut w = world("", false);
    let message = json!({
        "jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": { "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } },
    });
    let identity = worker_identity(&w);
    let reply = w.rpc(identity, &message);
    assert_eq!(
        reply,
        json!({
            "jsonrpc": "2.0", "id": 0,
            "result": {
                "protocolVersion": "2025-03-26",
                "capabilities": { "tools": { "listChanged": true } },
                "serverInfo": { "name": "headwork", "version": "0.1.0" },
            },
        }),
        "initialize"
    );
    let ping = json!({ "jsonrpc": "2.0", "id": 3, "method": "ping" });
    assert_eq!(
        w.rpc(orchestrator_identity(), &ping),
        json!({ "jsonrpc": "2.0", "id": 3, "result": {} }),
        "ping"
    );
    let unknown = json!({ "jsonrpc": "2.0", "id": 4, "method": "nope" });
    assert_eq!(
        w.rpc(orchestrator_identity(), &unknown),
        json!({ "jsonrpc": "2.0", "id": 4, "error": { "code": -32601, "message": "method not found: nope" } }),
        "unknown method"
    );
}

#[test]
fn mcp_notifications_get_no_answer_and_serve_reads_lines() {
    let mut w = world("", false);
    let identity = worker_identity(&w);
    let input = [
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string(),
        "not json".to_owned(),
        json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }).to_string(),
    ]
    .join("\n");
    let mut out = Vec::new();
    w.server(identity)
        .serve(std::io::Cursor::new(input), &mut out)
        .unwrap();
    let lines: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "one parse error and one pong: {lines:?}");
    assert_eq!(lines[0]["error"]["code"], -32700, "parse error");
    assert_eq!(lines[1]["result"], json!({}), "pong");
}

#[test]
fn mcp_tools_follow_the_role() {
    let mut w = world("", false);
    let identity = worker_identity(&w);
    let child = w.tool_names(identity);
    assert_eq!(
        child,
        [
            "board",
            "inbox",
            "migrate_list",
            "report",
            "done",
            "recap",
            "job_register",
            "job_start",
            "helper_tab",
            "worktree_add",
            "job_status",
            "job_stop",
            "job_forget"
        ],
        "child"
    );
    let root = w.tool_names(orchestrator_identity());
    assert_eq!(
        root,
        [
            "board",
            "status",
            "usage",
            "inbox",
            "spawn",
            "drop",
            "close",
            "answer",
            "queue",
            "clean",
            "reap",
            "update",
            "config_sync",
            "migrate_list",
            "migrate_mark",
            "backlog_list",
            "backlog_add",
            "backlog_done",
            "backlog_edit",
            "rules",
            "rule_add",
            "rule_retire",
            "recap",
            "job_status",
            "job_forget",
            "park",
            "resume"
        ],
        "root"
    );
    assert!(w.tool_names(Identity::default()).is_empty(), "unmanaged");
}

#[test]
fn mcp_report_schema_is_golden() {
    let mut w = world("", false);
    let message = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
    let identity = worker_identity(&w);
    let reply = w.rpc(identity, &message);
    let report = reply["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "report")
        .unwrap()
        .clone();
    assert_eq!(
        report["inputSchema"],
        json!({
            "type": "object",
            "properties": {
                "status": { "type": "string", "enum": ["progress", "review", "blocked", "done"] },
                "summary": { "type": "string" },
                "links": { "type": "array", "items": { "type": "string" } },
                "pr": {
                    "type": "string",
                    "description": "The pull request URL, https://<host>/<owner>/<repo>/pull/<n> on github.com or a GitHub \
                                    Enterprise host; for status review.",
                },
            },
            "required": ["status", "summary"],
        }),
        "report schema"
    );
}

#[test]
fn mcp_calls_check_the_role_again() {
    let mut w = world("", false);
    let child = worker_identity(&w);
    let (is_error, text) = w.tool(child.clone(), "spawn", &json!({}));
    assert!(
        is_error && text.contains("tool of the orchestrator role"),
        "{text}"
    );
    let (is_error, text) = w.tool(orchestrator_identity(), "report", &json!({}));
    assert!(
        is_error && text.contains("tool of the worker role"),
        "{text}"
    );
    let (is_error, text) = w.tool(Identity::default(), "board", &json!({}));
    assert!(is_error && text.contains("not managed"), "{text}");
    let forged = Identity {
        session: Some("no-such-session".to_owned()),
        role: Some(Role::Worker),
    };
    let (is_error, text) = w.tool(
        forged,
        "report",
        &json!({ "status": "progress", "summary": "x" }),
    );
    assert!(is_error && text.contains("no task"), "{text}");
    let (is_error, _) = w.tool(child, "nope", &json!({}));
    assert!(is_error, "unknown tool");
}

#[test]
fn mcp_board_and_inbox_answer_both_roles() {
    let mut w = world("", false);
    w.start();
    let (is_error, text) = w.tool(orchestrator_identity(), "board", &json!({}));
    assert!(!is_error && text.contains("demo-sand"), "{text}");
    let identity = worker_identity(&w);
    let (is_error, text) = w.tool(identity, "inbox", &json!({}));
    assert!(!is_error && text.trim() == "[]", "{text}");
}

#[test]
fn a_blocked_or_done_report_lets_the_stop_gate_through() {
    let mut w = world("push_window_secs = 0", false);
    w.start();
    w.hook(HookEvent::UserPromptSubmit, &json!({ "prompt": "go" }));
    let stop = json!({ "last_assistant_message": "Waiting on the API key." });
    w.hook(HookEvent::Stop, &stop);
    let identity = worker_identity(&w);
    let (is_error, text) = w.tool(
        identity.clone(),
        "report",
        &json!({ "status": "progress", "summary": "half way" }),
    );
    assert!(!is_error, "{text}");
    w.hook(HookEvent::Stop, &stop);
    let (is_error, text) = w.tool(
        identity,
        "report",
        &json!({ "status": "blocked", "summary": "needs the API key" }),
    );
    assert!(!is_error, "{text}");
    w.hook(HookEvent::Stop, &stop);
    let rules: Vec<String> = w.denies().into_iter().map(|(_, rule)| rule).collect();
    assert_eq!(
        rules,
        ["stop-unfinished", "stop-unfinished"],
        "progress does not count; blocked does"
    );
    let last = w.store.task(w.task.id).unwrap().last_report;
    assert_eq!(last.as_deref(), Some("needs the API key"), "last report");
    w.hook(HookEvent::UserPromptSubmit, &json!({ "prompt": "again" }));
    w.hook(HookEvent::Stop, &stop);
    assert_eq!(w.denies().len(), 3, "a new turn needs a new report");
}

#[test]
fn a_review_report_with_an_allowed_pull_request_moves_the_task() {
    let mut w = world("", false);
    w.start();
    let identity = worker_identity(&w);
    let (is_error, text) = w.tool(
        identity.clone(),
        "report",
        &json!({ "status": "review", "summary": "ready" }),
    );
    assert!(is_error && text.contains("needs `pr`"), "{text}");
    let (is_error, text) = w.tool(
        identity,
        "report",
        &json!({ "status": "review", "summary": "ready", "pr": "https://github.com/Ivan091/demo/pull/12" }),
    );
    assert!(!is_error && text.contains("in_review"), "{text}");
    assert_eq!(w.state(), TaskState::InReview, "state");
    let board = w.store.board(false).unwrap();
    let pr = board.tasks[0].pr.clone().unwrap();
    assert_eq!(pr.url, "https://github.com/Ivan091/demo/pull/12", "pr");
}

#[test]
fn a_review_report_naming_a_foreign_repository_is_refused_by_the_lifecycle() {
    let mut w = world("", false);
    w.start();
    let identity = worker_identity(&w);
    let (is_error, text) = w.tool(
        identity,
        "report",
        &json!({ "status": "review", "summary": "ready", "pr": "https://github.com/Other/repo/pull/1" }),
    );
    assert!(!is_error && text.contains("note"), "{text}");
    assert_eq!(w.state(), TaskState::Working, "state");
}

#[test]
fn rule_add_reaches_running_sessions_and_rule_retire_takes_it_back() {
    let mut w = world("", false);
    w.start();
    let prompt = json!({ "prompt": "hello" });
    assert_eq!(
        w.hook(HookEvent::UserPromptSubmit, &prompt).stdout,
        None,
        "unchanged"
    );
    let (is_error, text) = w.tool(
        orchestrator_identity(),
        "rule_add",
        &json!({ "scope": "child", "text": "Always say KUMQUAT." }),
    );
    assert!(!is_error && text.contains("\"rules_version\": 2"), "{text}");
    let pushed = context(&w.hook(HookEvent::UserPromptSubmit, &prompt));
    assert!(
        pushed.contains("KUMQUAT") && pushed.contains("version 2"),
        "{pushed}"
    );
    assert_eq!(
        w.hook(HookEvent::UserPromptSubmit, &prompt).stdout,
        None,
        "pushed once"
    );
    let (_, listed) = w.tool(orchestrator_identity(), "rules", &json!({}));
    assert!(listed.contains("KUMQUAT"), "{listed}");
    let (is_error, text) = w.tool(orchestrator_identity(), "rule_retire", &json!({ "id": 1 }));
    assert!(!is_error && text.contains("\"rules_version\": 3"), "{text}");
    let pushed = context(&w.hook(HookEvent::UserPromptSubmit, &prompt));
    assert!(
        !pushed.contains("KUMQUAT") && pushed.contains("version 3"),
        "{pushed}"
    );
    let (is_error, _) = w.tool(orchestrator_identity(), "rule_retire", &json!({ "id": 1 }));
    assert!(is_error, "already retired");
    let (is_error, text) = w.tool(
        orchestrator_identity(),
        "rule_add",
        &json!({ "scope": "nowhere", "text": "x" }),
    );
    assert!(is_error && text.contains("unknown scope"), "{text}");
}

#[test]
fn done_refuses_an_open_ask_then_closes_after_the_response() {
    let mut w = world("", false);
    w.start();
    w.pre(
        "AskUserQuestion",
        &ask_input(&["Ship (Recommended)", "Drop"], "Ship"),
    );
    let identity = worker_identity(&w);
    let (is_error, text) = w.tool(identity.clone(), "done", &json!({}));
    assert!(is_error && text.contains("still open"), "{text}");
    w.post("AskUserQuestion", &json!({ "answers": "Ship" }));
    w.post("SendMessage", &json!({}));
    let mut server = w.server(identity);
    let message = json!({
        "jsonrpc": "2.0", "id": 9, "method": "tools/call",
        "params": { "name": "done", "arguments": { "summary": "built it" } },
    });
    let reply = server.handle(&message).unwrap();
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    server.finish_pending().unwrap();
    let state = w.state();
    assert!(
        matches!(state, TaskState::Closing | TaskState::Closed),
        "closing after done: {state}"
    );
    let last = w.store.task(w.task.id).unwrap().last_report;
    assert_eq!(last.as_deref(), Some("built it"), "report");
    let moves = w.transitions();
    assert!(
        moves.contains(&("finish".to_owned(), Actor::Worker)),
        "a task without a pull request finishes through the checked path, by the worker: {moves:?}"
    );
    assert!(
        !moves.iter().any(|(trigger, _)| trigger == "force-close"),
        "no force-close: {moves:?}"
    );
}

const PR_URL: &str = "https://github.com/Ivan091/demo/pull/5";

fn pr(state: PrState) -> PrInfo {
    PrInfo {
        repo: "Ivan091/demo".to_owned(),
        number: 5,
        url: PR_URL.to_owned(),
        state,
        created_at: None,
    }
}

impl World {
    fn transitions(&self) -> Vec<(String, Actor)> {
        let mut out: Vec<(String, Actor)> = self
            .events()
            .into_iter()
            .filter(|event| event.kind == "transition")
            .map(|event| {
                (
                    event.data["trigger"].as_str().unwrap().to_owned(),
                    event.actor,
                )
            })
            .collect();
        out.retain(|(trigger, _)| trigger != "start");
        out
    }
}

#[test]
fn done_closes_a_merged_task_through_in_review_and_merged() {
    let mut w = world("", false);
    w.start();
    w.post("SendMessage", &json!({}));
    w.prs.set("sand", pr(PrState::Merged));
    let (is_error, text) = w.tool(worker_identity(&w), "done", &json!({ "summary": "merged" }));
    assert!(!is_error, "{text}");
    assert_eq!(w.state(), TaskState::Closing, "closing");
    let moves: Vec<String> = w.transitions().into_iter().map(|(t, _)| t).collect();
    assert_eq!(
        moves,
        ["session-started", "review-opened", "merged", "close"],
        "in_review and merged are reached"
    );
    assert!(
        w.transitions()
            .iter()
            .skip(1)
            .all(|(_, actor)| *actor == Actor::Worker),
        "every step after the session start is the worker's: {:?}",
        w.transitions()
    );
    let recorded = w.events().into_iter().any(|event| {
        event.kind == "pr-state" && event.data["to"] == "merged" && event.data["url"] == PR_URL
    });
    assert!(recorded, "the pull request is recorded");
}

#[test]
fn done_refuses_a_missing_report_or_an_open_pull_request_and_leaves_the_task() {
    let mut w = world("", false);
    w.start();
    let identity = worker_identity(&w);
    let (is_error, text) = w.tool(identity.clone(), "done", &json!({}));
    assert!(is_error && text.contains("report"), "{text}");
    assert_eq!(w.state(), TaskState::Working, "still working");
    w.post("SendMessage", &json!({}));
    w.prs.set("sand", pr(PrState::Open));
    let (is_error, text) = w.tool(identity, "done", &json!({}));
    assert!(is_error && text.contains("still open"), "{text}");
    assert_eq!(w.state(), TaskState::Working, "still working");
    let refusals = w
        .events()
        .into_iter()
        .filter(|event| event.kind == "close-refused" && event.actor == Actor::Worker)
        .count();
    assert_eq!(refusals, 2, "each refusal is logged");
    assert_eq!(
        w.store.task(w.task.id).unwrap().last_report,
        None,
        "a refused done records no report"
    );
}

#[test]
fn done_after_a_pull_request_closed_unmerged_returns_to_working_and_finishes() {
    let mut w = world("", false);
    w.start();
    let identity = worker_identity(&w);
    let (is_error, text) = w.tool(
        identity.clone(),
        "report",
        &json!({ "status": "review", "summary": "please review", "pr": PR_URL }),
    );
    assert!(!is_error, "{text}");
    assert_eq!(w.state(), TaskState::InReview, "in review");
    w.post("SendMessage", &json!({}));
    w.prs.set("other-branch", pr(PrState::Closed));
    let (is_error, text) = w.tool(identity, "done", &json!({}));
    assert!(!is_error, "{text}");
    let moves: Vec<String> = w.transitions().into_iter().map(|(t, _)| t).collect();
    assert_eq!(
        moves,
        [
            "session-started",
            "review-opened",
            "changes-requested",
            "finish"
        ],
        "the recorded pull request is checked by its URL"
    );
}

#[test]
fn an_enterprise_host_pull_request_is_recorded_and_seen_merged_by_its_url() {
    let url = "https://ghe.example.com/Ivan091/demo/pull/5";
    let mut w = world("", false);
    w.start();
    let identity = worker_identity(&w);
    let (is_error, text) = w.tool(
        identity.clone(),
        "report",
        &json!({ "status": "review", "summary": "please review", "pr": url }),
    );
    assert!(!is_error && text.contains("in_review"), "{text}");
    let board = w.store.board(false).unwrap();
    assert_eq!(
        board.tasks[0].pr.clone().unwrap().url,
        url,
        "the host is kept"
    );
    w.post("SendMessage", &json!({}));
    w.prs.set(
        "renamed-branch",
        PrInfo {
            url: url.to_owned(),
            ..pr(PrState::Merged)
        },
    );
    let (is_error, text) = w.tool(identity, "done", &json!({}));
    assert!(!is_error, "{text}");
    assert_eq!(w.state(), TaskState::Closing, "closing");
    let moves: Vec<String> = w.transitions().into_iter().map(|(t, _)| t).collect();
    assert_eq!(
        moves,
        ["session-started", "review-opened", "merged", "close"],
        "the merge is seen through the recorded enterprise URL"
    );
}

#[test]
fn a_review_report_matches_the_allowed_repository_whatever_its_case() {
    let mut w = world("", false);
    w.start();
    let (is_error, text) = w.tool(
        worker_identity(&w),
        "report",
        &json!({ "status": "review", "summary": "ready", "pr": "https://github.com/ivan091/DEMO/pull/12" }),
    );
    assert!(!is_error && text.contains("in_review"), "{text}");
    assert_eq!(w.state(), TaskState::InReview, "state");
}

#[test]
fn a_new_pull_request_reported_while_in_review_is_recorded_without_a_refusal() {
    let mut w = world("", false);
    w.start();
    let identity = worker_identity(&w);
    for number in [12, 13] {
        let (is_error, text) = w.tool(
            identity.clone(),
            "report",
            &json!({ "status": "review", "summary": "ready", "pr": format!("https://github.com/Ivan091/demo/pull/{number}") }),
        );
        assert!(!is_error && text.contains("in_review"), "{text}");
    }
    assert_eq!(w.state(), TaskState::InReview, "state");
    assert_eq!(
        w.store.recorded_prs(w.task.id).unwrap().len(),
        2,
        "both are recorded"
    );
    let refusals = w
        .events()
        .into_iter()
        .filter(|event| event.kind == "refused")
        .count();
    assert_eq!(refusals, 0, "no transition is tried");
}

#[test]
fn done_ignores_an_earlier_tasks_merged_pull_request_on_a_reused_branch() {
    let mut w = world("", false);
    w.start();
    w.post("SendMessage", &json!({}));
    w.prs.set(
        "sand",
        PrInfo {
            created_at: Some("2020-01-01T00:00:00Z".to_owned()),
            ..pr(PrState::Merged)
        },
    );
    let (is_error, text) = w.tool(worker_identity(&w), "done", &json!({}));
    assert!(!is_error, "{text}");
    let moves: Vec<String> = w.transitions().into_iter().map(|(t, _)| t).collect();
    assert_eq!(
        moves,
        ["session-started", "finish"],
        "the older pull request does not count as this task's merge"
    );
    assert!(
        w.store.recorded_prs(w.task.id).unwrap().is_empty(),
        "and is not recorded for it"
    );
}

#[test]
fn done_after_a_later_pull_request_closed_unmerged_does_not_read_as_merged() {
    let second = "https://github.com/Ivan091/demo/pull/6";
    let mut w = world("", false);
    w.start();
    let identity = worker_identity(&w);
    for url in [PR_URL, second] {
        let (is_error, text) = w.tool(
            identity.clone(),
            "report",
            &json!({ "status": "review", "summary": "please review", "pr": url }),
        );
        assert!(!is_error, "{text}");
    }
    w.post("SendMessage", &json!({}));
    w.prs.set("first", pr(PrState::Merged));
    w.prs.set(
        "second",
        PrInfo {
            number: 6,
            url: second.to_owned(),
            ..pr(PrState::Closed)
        },
    );
    let (is_error, text) = w.tool(identity, "done", &json!({}));
    assert!(!is_error, "{text}");
    let moves: Vec<String> = w.transitions().into_iter().map(|(t, _)| t).collect();
    assert_eq!(
        moves,
        [
            "session-started",
            "review-opened",
            "changes-requested",
            "finish"
        ],
        "the newest pull request, closed unmerged, decides"
    );
}

#[test]
fn only_the_user_and_the_orchestrator_force_close() {
    let mut w = world("", false);
    w.start();
    let env = Env {
        home: &w.home,
        config: &w.config,
        host: &w.host,
        exe: Path::new(EXE),
        actor: Actor::Worker,
    };
    let refused = headwork::ops::close::close(&mut w.store, &env, w.task.id, None);
    assert!(
        refused.is_err_and(|error| error.to_string().contains("does not force-close")),
        "a worker never force-closes"
    );
    assert_eq!(w.state(), TaskState::Working, "untouched");
    let (is_error, text) = w.tool(
        orchestrator_identity(),
        "close",
        &json!({ "name": "demo-sand" }),
    );
    assert!(!is_error, "{text}");
    assert!(
        w.transitions()
            .contains(&("force-close".to_owned(), Actor::Orchestrator)),
        "the orchestrator's force-close is recorded as its own: {:?}",
        w.transitions()
    );
}

#[test]
fn drop_records_the_users_words_and_closes() {
    let mut w = world("", false);
    w.start();
    let (is_error, text) = w.tool(
        orchestrator_identity(),
        "drop",
        &json!({ "name": "demo-sand", "user_words": "not needed any more" }),
    );
    assert!(!is_error, "{text}");
    let state = w.state();
    assert!(
        matches!(state, TaskState::Closing | TaskState::Closed),
        "{state}"
    );
    let words = w.events().into_iter().any(|event| {
        event.kind == "drop-reason" && event.data["user_words"] == "not needed any more"
    });
    assert!(words, "the user's words are logged");
}

#[test]
fn the_root_writes_a_recap_of_its_own_session_and_a_park_keeps_it() {
    let mut w = world("", false);
    let (is_error, text) = w.tool(
        orchestrator_identity(),
        "recap",
        &json!({ "text": "Watching sand." }),
    );
    assert!(!is_error, "{text}");
    let recap = w
        .store
        .latest_session_recap("root-session")
        .unwrap()
        .unwrap();
    assert_eq!(recap.task_id, None, "no task");
    assert_eq!(recap.park_id, None, "not a park");
    let park = w.store.begin_park(60, Actor::User).unwrap();
    let (is_error, text) = w.tool(
        orchestrator_identity(),
        "recap",
        &json!({ "text": "Parking." }),
    );
    assert!(!is_error && text.contains("reopens you"), "{text}");
    let parked = w
        .store
        .orchestrator_park_recap(park.id, "root-session")
        .unwrap()
        .unwrap();
    assert_eq!(parked.body, "Parking.", "the park's recap");
}

#[test]
fn a_child_writes_a_recap_and_registers_a_job_that_blocks_done() {
    let mut w = world("", false);
    w.start();
    let identity = worker_identity(&w);
    let (is_error, text) = w.tool(identity.clone(), "recap", &json!({ "text": "  " }));
    assert!(is_error && text.contains("empty"), "{text}");
    let (is_error, text) = w.tool(
        identity.clone(),
        "recap",
        &json!({ "text": "Halfway through the render." }),
    );
    assert!(!is_error, "{text}");
    let recap = w.store.latest_recap(w.task.id).unwrap().unwrap();
    assert_eq!(recap.body, "Halfway through the render.", "stored");
    assert_eq!(recap.park_id, None, "not a park");
    let (is_error, text) = w.tool(
        identity.clone(),
        "job_register",
        &json!({ "label": "train", "argv": ["node", "-e", "0"], "park_policy": "checkpoint" }),
    );
    assert!(is_error && text.contains("checkpoint file"), "{text}");
    let (is_error, text) = w.tool(
        identity.clone(),
        "job_register",
        &json!({
            "label": "train", "argv": ["node", "-e", "0"], "park_policy": "checkpoint",
            "checkpoint_path": "C:/out/checkpoint.json", "pid": std::process::id(),
        }),
    );
    assert!(!is_error, "{text}");
    let job: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(job["state"], "running", "{job}");
    let (_, status) = w.tool(identity.clone(), "job_status", &json!({}));
    assert!(status.contains("train"), "{status}");
    let (is_error, text) = w.tool(identity.clone(), "done", &json!({ "summary": "x" }));
    assert!(is_error && text.contains("still runs"), "{text}");
    let (_, root_status) = w.tool(orchestrator_identity(), "job_status", &json!({}));
    assert!(
        root_status.contains("train"),
        "the orchestrator sees every job: {root_status}"
    );
    let (is_error, text) = w.tool(identity, "park", &json!({}));
    assert!(is_error && text.contains("orchestrator role"), "{text}");
}

#[test]
fn job_start_runs_the_command_detached_and_logs_to_the_job_log() {
    let mut w = world("", false);
    w.start();
    let identity = worker_identity(&w);
    let cwd = w.dir.path().display().to_string();
    let (is_error, text) = w.tool(
        identity,
        "job_start",
        &json!({ "label": "version check", "argv": ["git", "--version"], "cwd": cwd }),
    );
    assert!(!is_error, "{text}");
    let job: Value = serde_json::from_str(&text).unwrap();
    assert!(job["pid"].as_u64().is_some(), "started: {job}");
    assert_eq!(job["park_policy"], "rerun", "default policy: {job}");
    let log = job["log_path"].as_str().unwrap().to_owned();
    let start = std::time::Instant::now();
    while std::fs::read_to_string(&log).unwrap_or_default().is_empty()
        && start.elapsed().as_secs() < 10
    {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(
        std::fs::read_to_string(&log)
            .unwrap()
            .contains("git version"),
        "output in the log"
    );
}

#[test]
fn a_message_to_either_orchestrator_address_reaches_the_live_one() {
    let mut w = world("", false);
    let text = context(&w.start());
    assert!(text.contains("the session named `orchestrator`"), "{text}");
    let message = |to: &str| json!({ "to": to, "message": "done" });
    assert_eq!(
        w.pre("SendMessage", &message("orchestrator")),
        HookReply::default(),
        "the default address goes through unchanged"
    );
    assert_eq!(
        w.pre("SendMessage", &message("orchestrator [ab12]")),
        HookReply::default(),
        "the live address with a ref goes through unchanged, ref and all"
    );
    assert!(
        !w.events().iter().any(|event| event.kind == "readdressed"),
        "nothing readdressed"
    );
    let readdressed = |reply: &HookReply| -> Value {
        let body: Value = serde_json::from_str(reply.stdout.as_deref().unwrap()).unwrap();
        body["hookSpecificOutput"]["updatedInput"].clone()
    };
    let reply = w.pre("SendMessage", &message("root"));
    assert_eq!(
        readdressed(&reply),
        message("orchestrator"),
        "root is an alias"
    );
    w.store
        .record_session(
            &NewSession {
                id: "old-root".to_owned(),
                task_id: None,
                role: Role::Orchestrator,
                rc_name: "root".to_owned(),
                host: "wmux".to_owned(),
                host_workspace: None,
                host_surface: None,
            },
            Actor::User,
        )
        .unwrap();
    let reply = w.pre("SendMessage", &message("orchestrator"));
    assert_eq!(
        readdressed(&reply),
        message("root"),
        "an orchestrator still named root gets the new name's messages"
    );
    assert_eq!(
        w.pre("SendMessage", &message("me-sim-x")),
        HookReply::default(),
        "messages to workers are left alone"
    );
    let moved = w
        .events()
        .iter()
        .filter(|event| event.kind == "readdressed")
        .count();
    assert_eq!(moved, 2, "each readdress logged");
}

#[test]
fn waiting_on_background_work_lets_the_stop_gate_through() {
    let mut w = world("push_window_secs = 0", false);
    w.start();
    let stop = json!({ "last_assistant_message": "Waiting for the build." });
    let post = |w: &mut World, tool: &str, input: &Value, response: &Value| {
        let input = json!({ "tool_name": tool, "tool_input": input, "tool_response": response });
        w.hook(HookEvent::PostToolUse, &input)
    };
    w.hook(HookEvent::UserPromptSubmit, &json!({ "prompt": "go" }));
    post(
        &mut w,
        "Bash",
        &json!({ "command": "cargo build" }),
        &json!({ "stdout": "ok", "backgroundTaskId": null }),
    );
    w.hook(HookEvent::Stop, &stop);
    assert_eq!(w.denies().len(), 1, "a foreground Bash is not waiting");
    for (tool, input, response) in [
        (
            "Bash",
            json!({ "command": "sleep 99", "run_in_background": true }),
            json!({}),
        ),
        ("Monitor", json!({}), json!({})),
        ("Workflow", json!({}), json!({})),
        ("Agent", json!({ "run_in_background": true }), json!({})),
        ("Agent", json!({}), json!({ "status": "async_launched" })),
        (
            "Bash",
            json!({ "command": "scripts/ship.sh -m x" }),
            json!({ "stdout": "", "backgroundTaskId": "bje2sgi00", "timedOutAfterMs": 600_000 }),
        ),
    ] {
        w.hook(HookEvent::UserPromptSubmit, &json!({ "prompt": "again" }));
        post(&mut w, tool, &input, &response);
        w.hook(HookEvent::Stop, &stop);
        assert_eq!(w.denies().len(), 1, "{tool} {input} lets the stop through");
    }
    w.hook(HookEvent::UserPromptSubmit, &json!({ "prompt": "next" }));
    w.hook(HookEvent::Stop, &stop);
    assert_eq!(
        w.denies().len(),
        2,
        "a turn without background work is denied again"
    );
}

#[test]
fn task_notifications_close_background_work_one_by_one() {
    let mut w = world("push_window_secs = 0", false);
    w.start();
    let stop = json!({ "last_assistant_message": "Waiting for the agents." });
    let notice = json!({ "prompt": "<task-notification>
<status>completed</status>
</task-notification>" });
    w.hook(HookEvent::UserPromptSubmit, &json!({ "prompt": "go" }));
    for _ in 0..2 {
        w.hook(
            HookEvent::PostToolUse,
            &json!({ "tool_name": "Agent", "tool_input": { "run_in_background": true }, "tool_response": {} }),
        );
    }
    w.hook(HookEvent::UserPromptSubmit, &notice);
    w.hook(HookEvent::Stop, &stop);
    assert_eq!(w.denies().len(), 0, "one agent still runs");
    w.hook(HookEvent::UserPromptSubmit, &notice);
    w.hook(HookEvent::Stop, &stop);
    assert_eq!(w.denies().len(), 1, "both reported, nothing else done");
    let marked = w
        .events()
        .into_iter()
        .filter(|event| event.kind == "prompt" && event.data["notification"] == true)
        .count();
    assert_eq!(marked, 2, "notifications are marked");
}

#[test]
fn enforce_rules_block_only_the_listed_rule_in_shadow_mode() {
    let mut w = world(
        "enforce_rules = [\"stop-unfinished\"]\npush_window_secs = 0",
        false,
    );
    w.start();
    w.hook(HookEvent::UserPromptSubmit, &json!({ "prompt": "go" }));
    let stop = w.hook(
        HookEvent::Stop,
        &json!({ "last_assistant_message": "Step done." }),
    );
    assert_eq!(stop.exit, 2, "the listed rule blocks");
    let shadow = w.pre("Skill", &json!({ "skill": "ship" }));
    assert_eq!(shadow, HookReply::default(), "an unlisted rule only logs");
    assert_eq!(
        w.denies(),
        [
            ("deny".to_owned(), "stop-unfinished".to_owned()),
            ("would-deny".to_owned(), "worker-global-skill".to_owned())
        ],
        "events"
    );
}

#[test]
fn extra_writable_paths_follow_the_project_the_kind_and_the_brief() {
    let mut w = world_in(
        "brief_writable = [\"@DIR@/notes\"]",
        "extra_writable = [{ path = \"@DIR@/notes/handoff.md\", kinds = [\"task\"] }, \
         { path = \"@DIR@/notes/night\", kinds = [\"car-training\"] }]",
        false,
    );
    w.start();
    let notes = w.dir.path().join("notes");
    let write = |w: &mut World, name: &str| {
        let path = notes.join(name).display().to_string();
        w.pre("Write", &json!({ "file_path": path, "content": "x" }));
        w.denies().len()
    };
    assert_eq!(write(&mut w, "handoff.md"), 0, "granted to this kind");
    assert_eq!(
        write(&mut w, "night/a.md"),
        1,
        "granted to another kind only"
    );
    assert_eq!(write(&mut w, "plan.md"), 2, "not named yet");
    let brief = format!("# Task\n\nWrites: `{}`\n", notes.join("plan.md").display());
    std::fs::write(&w.task.brief_path, brief).unwrap();
    assert_eq!(
        write(&mut w, "plan.md"),
        2,
        "named on the brief's Writes: line"
    );
    assert_eq!(write(&mut w, "other.md"), 3, "a sibling stays refused");
    let notes_text = notes.display().to_string().replace('\\', "/");
    w.pre("Bash", &json!({ "command": format!("cd {notes_text} && git add plan.md handoff.md && git commit -m x && git push") }));
    assert_eq!(w.denies().len(), 3, "commits its granted files");
    w.pre(
        "Bash",
        &json!({ "command": format!("git -C {notes_text} add -A") }),
    );
    assert_eq!(w.denies().len(), 4, "not the whole repository");
    let logged = w
        .events()
        .into_iter()
        .filter(|event| event.kind == "would-deny")
        .all(|event| {
            event.data["cwd"]
                .as_str()
                .is_some_and(|cwd| !cwd.is_empty())
        });
    assert!(
        logged,
        "each would-deny records the session's folder for a replay"
    );
}

const LEADS: &str = "\n[leads]\nenabled = true\nmax_parts = 1\n";

impl World {
    fn part(&mut self, part: &str) -> Task {
        let new = headwork_store::NewTask {
            request: TaskRequest {
                project: ProjectName::new("demo").unwrap(),
                slug: Slug::new(format!("sand-{part}")).unwrap(),
                kind: "task".to_owned(),
                model: Some(Model::Haiku),
                effort: None,
                in_main: false,
                keep: false,
                close: CloseMode::Confirm,
                review: None,
                decision: None,
            },
            brief_path: self.dir.path().join("brief.md").display().to_string(),
            depends_on: None,
            permission_mode: PermissionMode::Auto,
            shape: headwork_core::TaskShape::Part {
                parent: self.task.id,
            },
        };
        self.store
            .create_task(&self.config, &new, Actor::Worker)
            .unwrap()
    }
}

#[test]
fn with_leads_enabled_a_spawned_task_is_a_lead_and_without_it_plain() {
    let w = world(LEADS, false);
    assert_eq!(w.task.shape, headwork_core::TaskShape::Lead, "lead");
    let w = world("", false);
    assert_eq!(w.task.shape, headwork_core::TaskShape::Plain, "plain");
}

#[test]
fn a_lead_may_not_finish_while_a_part_is_not_closed() {
    let mut w = world(LEADS, false);
    w.start();
    w.post("SendMessage", &json!({}));
    let part = w.part("review");
    let identity = worker_identity(&w);
    let (is_error, text) = w.tool(identity.clone(), "done", &json!({ "summary": "built" }));
    assert!(is_error && text.contains("lead-parts-live"), "{text}");
    assert_eq!(w.state(), TaskState::Working, "the lead keeps working");
    let (is_error, text) = w.tool(
        orchestrator_identity(),
        "drop",
        &json!({ "name": part.name.to_string(), "user_words": "not needed" }),
    );
    assert!(!is_error, "{text}");
    assert_eq!(
        w.store.task(part.id).unwrap().state,
        TaskState::Closed,
        "the dropped part closed"
    );
    assert_eq!(
        w.state(),
        TaskState::Working,
        "dropping a part leaves its lead"
    );
    let (is_error, text) = w.tool(identity.clone(), "done", &json!({ "summary": "built" }));
    assert!(is_error && text.contains("lead-close-unasked"), "{text}");
    give_close_go(&mut w, "Close it (Recommended)");
    let mut server = w.server(identity);
    let message = json!({
        "jsonrpc": "2.0", "id": 9, "method": "tools/call",
        "params": { "name": "done", "arguments": { "summary": "built" } },
    });
    let reply = server.handle(&message).unwrap();
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    server.finish_pending().unwrap();
    assert!(
        matches!(w.state(), TaskState::Closing | TaskState::Closed),
        "the lead closes once its parts are closed"
    );
}

fn give_close_go(w: &mut World, answer: &str) {
    let asked = ask_input(&["Close it (Recommended)", "Keep it open"], "Close");
    w.pre("AskUserQuestion", &asked);
    let input = json!({
        "tool_name": "AskUserQuestion",
        "tool_input": asked,
        "tool_response": { "answers": { "Ship?": answer } },
    });
    w.hook(HookEvent::PostToolUse, &input);
}

#[test]
fn a_lead_closes_only_after_the_users_go_unless_ask_before_close_is_off() {
    let mut w = world(LEADS, false);
    w.start();
    w.post("SendMessage", &json!({}));
    let identity = worker_identity(&w);
    let (is_error, text) = w.tool(identity.clone(), "done", &json!({ "summary": "built" }));
    assert!(is_error && text.contains("lead-close-unasked"), "{text}");
    give_close_go(&mut w, "Keep it open");
    let (is_error, text) = w.tool(identity.clone(), "done", &json!({ "summary": "built" }));
    assert!(
        is_error && text.contains("lead-close-unasked"),
        "keep it open is no go: {text}"
    );
    assert_eq!(w.state(), TaskState::Working, "the lead keeps working");
    give_close_go(&mut w, "Close it (Recommended)");
    assert!(
        w.events().iter().any(|event| event.kind == "close-go"),
        "the go is recorded"
    );
    let (is_error, text) = w.tool(identity, "done", &json!({ "summary": "built" }));
    assert!(!is_error, "{text}");

    let mut off = world(&format!("{LEADS}ask_before_close = false\n"), false);
    off.start();
    off.post("SendMessage", &json!({}));
    let identity = worker_identity(&off);
    let (is_error, text) = off.tool(identity, "done", &json!({ "summary": "built" }));
    assert!(
        !is_error,
        "with the setting off a lead closes without asking: {text}"
    );

    let mut plain = world("", false);
    plain.start();
    plain.post("SendMessage", &json!({}));
    let identity = worker_identity(&plain);
    let (is_error, text) = plain.tool(identity, "done", &json!({ "summary": "built" }));
    assert!(!is_error, "a plain worker never asks: {text}");
}

#[test]
fn a_silent_lead_closes_without_the_users_go() {
    let mut w = world_of(LEADS, "", false, SILENT_KIND);
    let rules = context(&w.start());
    assert!(
        rules.contains("You are the lead of this task")
            && !rules.contains(ASK_BEFORE_CLOSE_HEADING),
        "a silent lead is not told to ask: {rules}"
    );
    w.post("SendMessage", &json!({}));
    let identity = worker_identity(&w);
    let (is_error, text) = w.tool(identity, "done", &json!({ "summary": "built" }));
    assert!(!is_error, "a silent lead closes without asking: {text}");
    assert!(
        !w.events().iter().any(|event| event.kind == "close-go"),
        "no go was needed"
    );
}

#[test]
fn a_leads_own_headwork_close_waits_for_the_users_go() {
    let mut w = world(LEADS, false);
    w.start();
    w.post("SendMessage", &json!({}));
    let env = Env {
        home: &w.home,
        config: &w.config,
        host: &w.host,
        exe: Path::new(EXE),
        actor: Actor::Worker,
    };
    let refused = headwork::ops::close::begin_own_close(&mut w.store, &env, &w.prs, w.task.id);
    assert!(
        refused.is_err_and(|error| error.to_string().contains("lead-close-unasked")),
        "the self-close is refused without a go"
    );
    assert_eq!(w.state(), TaskState::Working, "the lead keeps working");
    assert!(
        w.events().iter().any(|event| event.kind == "close-refused"),
        "the refusal is logged"
    );
    give_close_go(&mut w, "Close it (Recommended)");
    let env = Env {
        home: &w.home,
        config: &w.config,
        host: &w.host,
        exe: Path::new(EXE),
        actor: Actor::Worker,
    };
    headwork::ops::close::begin_own_close(&mut w.store, &env, &w.prs, w.task.id).unwrap();
    assert_eq!(w.state(), TaskState::Closing, "closing after the go");
}

#[test]
fn dropping_a_lead_drops_and_closes_its_parts_first() {
    let mut w = world(LEADS, false);
    w.start();
    let running = w.part("review");
    let waiting = w.part("render");
    let env = Env {
        home: &w.home,
        config: &w.config,
        host: &w.host,
        exe: Path::new(EXE),
        actor: Actor::Orchestrator,
    };
    let pumped = headwork::ops::spawn::pump(&mut w.store, &env).unwrap();
    assert_eq!(pumped.len(), 2, "{pumped:?}");
    assert!(
        matches!(
            pumped[0].outcome,
            headwork::ops::spawn::Outcome::Started { .. }
        ),
        "the first part starts: {pumped:?}"
    );
    assert!(
        matches!(&pumped[1].outcome, headwork::ops::spawn::Outcome::Waiting { reason } if reason.contains("max_parts")),
        "the second waits for max_parts = 1: {pumped:?}"
    );
    let (is_error, text) = w.tool(
        orchestrator_identity(),
        "drop",
        &json!({ "name": w.task.name.to_string(), "user_words": "stop it all" }),
    );
    assert!(!is_error, "{text}");
    for (id, what) in [
        (running.id, "running part"),
        (waiting.id, "waiting part"),
        (w.task.id, "lead"),
    ] {
        assert_eq!(w.store.task(id).unwrap().state, TaskState::Closed, "{what}");
    }
    let cascaded = w
        .store
        .events(EventFilter {
            task: Some(running.id),
            limit: 100,
        })
        .unwrap()
        .into_iter()
        .any(|event| event.kind == "drop-cascade");
    assert!(cascaded, "the part's drop is logged as the lead's cascade");
}

fn part_spawn(w: &mut World, arguments: &Value) -> (bool, String) {
    let identity = worker_identity(w);
    w.tool(identity, "part_spawn", arguments)
}

fn part_named(w: &World, slug: &str) -> Task {
    w.store.find_task(&format!("demo-{slug}")).unwrap().unwrap()
}

#[test]
fn only_a_lead_sees_and_calls_the_part_tools() {
    let mut w = world(LEADS, false);
    let names = w.tool_names(worker_identity(&w));
    assert!(
        names.contains(&"part_spawn".to_owned()) && names.contains(&"part_drop".to_owned()),
        "{names:?}"
    );
    let mut plain = world("", false);
    let names = plain.tool_names(worker_identity(&plain));
    assert!(
        !names.iter().any(|name| name.starts_with("part_")),
        "{names:?}"
    );
    let (is_error, text) = part_spawn(
        &mut plain,
        &json!({ "slug": "review", "brief": "Review it." }),
    );
    assert!(is_error && text.contains("not a lead"), "{text}");
    let names = w.tool_names(orchestrator_identity());
    assert!(
        !names.iter().any(|name| name.starts_with("part_")),
        "{names:?}"
    );
    let (is_error, text) = w.tool(
        orchestrator_identity(),
        "part_spawn",
        &json!({ "slug": "review", "brief": "Review it." }),
    );
    assert!(is_error && text.contains("worker"), "{text}");
}

#[test]
fn part_spawn_names_the_part_after_its_lead_in_the_leads_project() {
    let mut w = world(LEADS, false);
    w.start();
    let (is_error, text) = part_spawn(&mut w, &json!({ "slug": "review", "brief": "Review it." }));
    assert!(!is_error, "{text}");
    let part = part_named(&w, "sand-review");
    assert_eq!(
        part.shape,
        headwork_core::TaskShape::Part { parent: w.task.id },
        "a part of the lead"
    );
    assert_eq!(part.project().as_str(), "demo", "the lead's project");
    assert_eq!(part.state, TaskState::Spawning, "it starts: {text}");
    let (is_error, text) = part_spawn(
        &mut w,
        &json!({ "slug": "sand-render", "brief": "Render it." }),
    );
    assert!(!is_error, "{text}");
    assert_eq!(
        part_named(&w, "sand-render").state,
        TaskState::Queued,
        "an already prefixed slug is kept, and the second part waits for max_parts = 1"
    );
    assert!(text.contains("max_parts"), "{text}");
    let (is_error, text) = part_spawn(
        &mut w,
        &json!({ "slug": "port", "brief": "Port it.", "project": "other" }),
    );
    assert!(is_error && text.contains("new task"), "{text}");
}

#[test]
fn a_part_of_a_worktree_lead_carries_no_user_only_command() {
    let mut w = world(
        "enforce = true\n[leads]\nenabled = true\nmax_parts = 2\n",
        false,
    );
    w.start();
    answer_allow(
        &mut w,
        "scripts/publish.sh --all",
        "Allow it for this task (Recommended)",
    );
    let (is_error, text) = part_spawn(
        &mut w,
        &json!({ "slug": "ship", "brief": "Publish.", "allow_ask": ["scripts/publish.sh --all"] }),
    );
    assert!(
        is_error && text.contains("main checkout"),
        "publishing runs in the main checkout: {text}"
    );
    let (is_error, text) = part_spawn(
        &mut w,
        &json!({ "slug": "fix", "brief": "Fix it. Never run `scripts/publish.sh`." }),
    );
    assert!(!is_error, "a forbidden command names nothing: {text}");
    assert!(
        !part_named(&w, "sand-fix").in_main,
        "a part without a go stays in a worktree"
    );
}

#[test]
fn a_part_carries_only_the_user_only_commands_its_lead_was_allowed() {
    let mut w = world_of(LEADS, "main_only = [\"publish\"]", false, "publish");
    assert!(w.task.in_main, "the lead runs in the main checkout");
    w.start();
    let (is_error, text) = part_spawn(
        &mut w,
        &json!({ "slug": "ship", "brief": "Then run `scripts/publish.sh --all`." }),
    );
    assert!(
        is_error && text.contains("has not allowed its lead"),
        "nothing allowed yet: {text}"
    );
    let (is_error, text) = part_spawn(
        &mut w,
        &json!({ "slug": "fix", "brief": "Fix it. Never run `scripts/publish.sh`." }),
    );
    assert!(!is_error, "a forbidden command names nothing: {text}");
    answer_allow(
        &mut w,
        "scripts/publish.sh --all",
        "Allow it for this task (Recommended)",
    );
    let (is_error, text) = part_spawn(
        &mut w,
        &json!({
            "slug": "ship",
            "brief": "Publish the build.",
            "allow_ask": ["scripts/publish.sh --all"],
        }),
    );
    assert!(!is_error, "the lead was allowed it: {text}");
    let part = part_named(&w, "sand-ship");
    assert!(
        part.in_main && part.kind == "publish",
        "it runs in the main checkout under its lead's kind"
    );
    let brief = std::fs::read_to_string(w.home.brief_path(&part.name)).unwrap();
    assert!(
        brief.contains("## First action: the user's go")
            && brief.contains("run `scripts/publish.sh --all`?"),
        "the part asks the user itself: {brief}"
    );
    assert!(
        w.store.user_only_allowed(part.id).unwrap().is_empty(),
        "the lead's go is not the part's"
    );
}

#[test]
fn part_spawn_keeps_the_model_within_the_allowed_and_fable_with_the_user() {
    let leads = format!("{LEADS}\n[models]\nallowed = [\"haiku\", \"sonnet\", \"fable\"]\n");
    let mut w = world(&leads, false);
    w.start();
    let (is_error, text) = part_spawn(
        &mut w,
        &json!({ "slug": "review", "brief": "Review it.", "model": "opus", "reason": "an audit" }),
    );
    assert!(is_error && text.contains("opus"), "{text}");
    assert!(
        w.store.find_task("demo-sand-review").unwrap().is_none(),
        "no part for a model not allowed"
    );
    let (is_error, text) = part_spawn(
        &mut w,
        &json!({ "slug": "render", "brief": "Render it.", "model": "fable" }),
    );
    assert!(!is_error, "{text}");
    let part = part_named(&w, "sand-render");
    assert!(
        part.fable_ask.is_some(),
        "Fable raises the user's approval ask"
    );
    assert_eq!(part.state, TaskState::Queued, "and waits for it");
    assert!(text.contains("approval"), "{text}");
}

#[test]
fn a_part_on_opus_needs_a_reason_and_it_is_recorded() {
    let mut w = world(LEADS, false);
    w.start();
    let (is_error, text) = part_spawn(
        &mut w,
        &json!({ "slug": "audit", "brief": "Audit it.", "model": "opus" }),
    );
    assert!(is_error && text.contains("reason"), "{text}");
    assert!(
        w.store.find_task("demo-sand-audit").unwrap().is_none(),
        "no part without a reason"
    );
    let (is_error, text) = part_spawn(
        &mut w,
        &json!({ "slug": "audit", "brief": "Audit it.", "model": "opus", "reason": "security audit" }),
    );
    assert!(!is_error, "{text}");
    assert_eq!(
        part_named(&w, "sand-audit").model,
        Model::Opus,
        "the model is kept"
    );
}

#[test]
fn part_spawn_after_names_another_part_of_the_same_lead() {
    let mut w = world(LEADS, false);
    w.start();
    let (is_error, text) = part_spawn(
        &mut w,
        &json!({ "slug": "render", "brief": "Render it.", "after": "review" }),
    );
    assert!(is_error && text.contains("not a part"), "{text}");
    let (is_error, text) = part_spawn(
        &mut w,
        &json!({ "slug": "render", "brief": "Render it.", "after": "demo-sand" }),
    );
    assert!(
        is_error && text.contains("not a part"),
        "the lead itself: {text}"
    );
    let (is_error, text) = part_spawn(&mut w, &json!({ "slug": "review", "brief": "Review it." }));
    assert!(!is_error, "{text}");
    let review = part_named(&w, "sand-review");
    let (is_error, text) = part_spawn(
        &mut w,
        &json!({ "slug": "render", "brief": "Render it.", "after": "review" }),
    );
    assert!(!is_error, "{text}");
    assert_eq!(
        part_named(&w, "sand-render").depends_on,
        Some(review.id),
        "after becomes the dependency"
    );
}

#[test]
fn part_drop_drops_only_the_leads_own_parts() {
    let mut w = world(LEADS, false);
    w.start();
    let identity = worker_identity(&w);
    let (is_error, text) = w.tool(
        identity.clone(),
        "part_drop",
        &json!({ "name": "demo-sand" }),
    );
    assert!(
        is_error && text.contains("not a part"),
        "the lead itself: {text}"
    );
    let (is_error, text) = part_spawn(&mut w, &json!({ "slug": "review", "brief": "Review it." }));
    assert!(!is_error, "{text}");
    let (is_error, text) = w.tool(
        identity,
        "part_drop",
        &json!({ "name": "review", "reason": "not needed" }),
    );
    assert!(!is_error, "{text}");
    let part = part_named(&w, "sand-review");
    assert_eq!(part.state, TaskState::Closed, "the part closed");
    assert_eq!(w.state(), TaskState::Working, "the lead goes on");
}

#[test]
fn a_leads_board_lists_only_its_own_project() {
    let mut w = world_in(
        LEADS,
        "
[project.other]
main = \"@DIR@/other\"
worktree_new = \"builtin\"
worktree_remove = \"builtin\"
worktree_path = \"{main}-{slug}\"
deliver = \"scripts/ship.sh\"
merge = \"deliver\"
",
        false,
    );
    w.start();
    let projects = |text: &str| -> Vec<String> {
        let board: Value = serde_json::from_str(text).unwrap();
        board["projects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|project| project["name"].as_str().unwrap().to_owned())
            .collect()
    };
    let (_, text) = w.tool(worker_identity(&w), "board", &json!({}));
    assert_eq!(projects(&text), ["demo"], "the lead's board");
    let (_, text) = w.tool(orchestrator_identity(), "board", &json!({}));
    let mut all = projects(&text);
    all.sort();
    assert_eq!(
        all,
        ["demo", "other"],
        "the orchestrator's board lists every project"
    );
}

#[test]
fn a_leads_board_and_inbox_show_only_the_lead_and_its_parts() {
    let mut w = world(LEADS, false);
    w.start();
    let (is_error, text) = part_spawn(&mut w, &json!({ "slug": "review", "brief": "Review it." }));
    assert!(!is_error, "{text}");
    let (is_error, text) = w.tool(
        orchestrator_identity(),
        "spawn",
        &json!({ "project": "demo", "slug": "other", "brief": "Something else." }),
    );
    assert!(!is_error, "{text}");
    let names = |text: &str| -> Vec<String> {
        let board: Value = serde_json::from_str(text).unwrap();
        board["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| task["name"].as_str().unwrap().to_owned())
            .collect()
    };
    let (_, text) = w.tool(worker_identity(&w), "board", &json!({}));
    let mut lead = names(&text);
    lead.sort();
    assert_eq!(lead, ["demo-sand", "demo-sand-review"], "the lead's board");
    let (_, text) = w.tool(orchestrator_identity(), "board", &json!({}));
    assert!(
        names(&text).contains(&"demo-other".to_owned()),
        "the orchestrator's board is the machine's"
    );
    let other = part_named(&w, "other");
    w.store
        .open_ask(
            &headwork_store::NewAsk {
                task_id: Some(other.id),
                session_id: None,
                kind: headwork_core::AskKind::Tool,
                question: "Other?".to_owned(),
                options: vec!["1".to_owned(), "2".to_owned()],
            },
            Actor::Hook,
        )
        .unwrap();
    let (_, text) = w.tool(worker_identity(&w), "inbox", &json!({}));
    assert!(
        !text.contains("Other?"),
        "another task's ask stays out: {text}"
    );
    let (_, text) = w.tool(orchestrator_identity(), "inbox", &json!({}));
    assert!(
        text.contains("Other?"),
        "the orchestrator's inbox has it: {text}"
    );
}

#[test]
fn a_lead_with_a_live_part_may_stop_and_wait() {
    let mut w = world(LEADS, false);
    w.start();
    let stop = json!({ "last_assistant_message": "The part runs." });
    w.hook(HookEvent::Stop, &stop);
    assert!(
        w.denies().iter().any(|(_, rule)| rule == "stop-unfinished"),
        "without a part the silent stop is unfinished"
    );
    w.hook(HookEvent::UserPromptSubmit, &json!({ "prompt": "go on" }));
    let before = w.denies().len();
    let (is_error, text) = part_spawn(&mut w, &json!({ "slug": "review", "brief": "Review it." }));
    assert!(!is_error, "{text}");
    w.hook(HookEvent::Stop, &stop);
    assert_eq!(
        w.denies().len(),
        before,
        "a lead waiting on its part may stop"
    );
}

#[test]
fn the_rules_follow_the_tasks_shape() {
    let mut w = world(LEADS, false);
    let lead_rules = context(&w.start());
    assert!(
        lead_rules.contains("You are the lead of this task"),
        "{lead_rules}"
    );
    assert!(
        !lead_rules.contains(PLAIN_LEAD_HEADING),
        "a lead drops the plain section"
    );
    assert!(
        lead_rules.contains("short report to the session named `orchestrator`"),
        "a lead reports to the orchestrator"
    );
    assert!(
        !lead_rules.contains("{parent}") && !lead_rules.contains("{orchestrator}"),
        "{lead_rules}"
    );
    assert!(
        lead_rules.contains(ASK_BEFORE_CLOSE_HEADING),
        "a lead asks before it closes"
    );
    let mut off = world(&format!("{LEADS}ask_before_close = false\n"), false);
    let off_rules = context(&off.start());
    assert!(
        off_rules.contains("You are the lead of this task")
            && !off_rules.contains(ASK_BEFORE_CLOSE_HEADING),
        "with ask_before_close off the section is gone: {off_rules}"
    );
    let (is_error, text) = part_spawn(&mut w, &json!({ "slug": "review", "brief": "Review it." }));
    assert!(!is_error, "{text}");
    let part = part_named(&w, "sand-review");
    w.session = w.store.session_of(part.id).unwrap().unwrap().id;
    let part_rules = context(&w.start());
    assert!(
        part_rules.contains("short report to the session named `demo-sand`"),
        "a part reports to its lead: {part_rules}"
    );
    assert!(part_rules.contains("tell `demo-sand`"), "{part_rules}");
    assert!(
        !part_rules.contains("You are the lead"),
        "a part is no lead"
    );
    assert!(
        !part_rules.contains(PLAIN_LEAD_HEADING),
        "a part drops the plain section"
    );
    let mut plain = world("", false);
    let plain_rules = context(&plain.start());
    assert!(
        plain_rules.contains(PLAIN_LEAD_HEADING),
        "a plain worker keeps it"
    );
    assert!(
        !plain_rules.contains("You are the lead"),
        "a plain worker is no lead"
    );
    assert!(
        plain_rules.contains("short report to the session named `orchestrator`"),
        "{plain_rules}"
    );
    for text in [&lead_rules, &part_rules, &plain_rules] {
        assert!(text.len() < 10_000, "{}", text.len());
    }
}

#[test]
fn the_orchestrator_rules_dispatch() {
    let w = world("", false);
    let files = RuleFiles::load(&w.home, &w.config, &[], "orchestrator").unwrap();
    let text = files.render(Role::Orchestrator, &w.config, Path::new(EXE), None);
    assert!(text.contains("## Dispatching"), "{text}");
    assert!(!text.contains("Briefing a lead"), "{text}");
    assert!(text.contains("Never relay an ask's text"), "{text}");
    assert!(text.contains("finishes or fails"), "{text}");
}

#[test]
fn a_lead_hears_on_its_next_prompt_which_part_waits_for_the_user() {
    let mut w = world(LEADS, false);
    w.start();
    let part = w.part("review");
    let prompt = json!({ "prompt": "how is it going" });
    assert_eq!(
        w.hook(HookEvent::UserPromptSubmit, &prompt).stdout,
        None,
        "no part waits"
    );
    let ask = headwork_store::NewAsk {
        task_id: Some(part.id),
        session_id: None,
        kind: headwork_core::AskKind::Review,
        question: "Ship it?".to_owned(),
        options: vec!["Ship it".to_owned(), "Drop it".to_owned()],
    };
    w.store.open_ask(&ask, Actor::Hook).unwrap();
    let text = context(&w.hook(HookEvent::UserPromptSubmit, &prompt));
    assert!(
        text.contains("part `demo-sand-review` waits for the user in its tab"),
        "{text}"
    );
    let (is_error, status) = w.tool(orchestrator_identity(), "status", &json!({}));
    assert!(!is_error, "{status}");
    assert!(
        status.contains("\"line\": \"demo-sand: needs you (in demo-sand-review)\""),
        "rolled up into the lead's line: {status}"
    );
    assert!(status.contains("\"status\": \"needs-you\""), "{status}");
    assert!(
        w.tool_names(orchestrator_identity())
            .contains(&"status".to_owned()),
        "the orchestrator's tool"
    );
    assert!(
        !w.tool_names(worker_identity(&w))
            .contains(&"status".to_owned()),
        "not a worker's"
    );
}

fn all_events(w: &World, kind: &str) -> Vec<Value> {
    w.store
        .events(EventFilter {
            task: None,
            limit: 1000,
        })
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == kind)
        .map(|event| event.data)
        .collect()
}

#[test]
fn the_update_tool_starts_a_detached_update_for_the_orchestrator_only() {
    let mut w = world("", false);
    let (is_error, text) = w.tool(
        orchestrator_identity(),
        "update",
        &json!({ "to": "v0.1.7" }),
    );
    assert!(is_error, "{EXE} does not exist: {text}");
    assert!(text.contains("update --to v0.1.7"), "{text}");
    let triggered = all_events(&w, "update-triggered");
    assert_eq!(triggered.len(), 1, "{triggered:?}");
    assert_eq!(triggered[0]["by"], "orchestrator", "by");
    assert_eq!(
        triggered[0]["args"],
        json!(["update", "--to", "v0.1.7"]),
        "args"
    );
    let (is_error, text) = w.tool(
        orchestrator_identity(),
        "update",
        &json!({ "to": "latest" }),
    );
    assert!(is_error && text.contains("not a release tag"), "{text}");
    let (is_error, text) = w.tool(worker_identity(&w), "update", &json!({}));
    assert!(
        is_error && text.contains("tool of the orchestrator role"),
        "{text}"
    );
    assert_eq!(
        all_events(&w, "update-triggered").len(),
        1,
        "neither a bad tag nor a worker starts anything"
    );
}

#[test]
fn the_update_tools_dry_run_starts_nothing() {
    let mut w = world("", false);
    w.tool(
        orchestrator_identity(),
        "update",
        &json!({ "dry_run": true }),
    );
    assert!(
        all_events(&w, "update-triggered").is_empty(),
        "a dry run answers in-process"
    );
}

#[test]
fn update_notices_reach_the_orchestrators_next_prompt_once() {
    let mut w = world("", false);
    UpdateState {
        notices: vec![Notice {
            at: "2026-11-22T12:00:00Z".to_owned(),
            text: "update to v0.1.9 failed at smoke (deterministic, attempt 1): boom; the installed build runs on"
                .to_owned(),
            source: None,
        }],
        ..UpdateState::default()
    }
    .save(&w.home)
    .unwrap();
    assert!(
        headwork::ops::update::leave_tick_notice(
            &w.home,
            1_000,
            "the tick failed demo-sand.".to_owned()
        )
        .unwrap(),
        "left"
    );
    w.start();
    w.hook(HookEvent::UserPromptSubmit, &json!({ "prompt": "go" }));
    assert_eq!(
        UpdateState::load(&w.home).notices.len(),
        2,
        "a worker's prompt leaves the notices"
    );
    w.store
        .record_session(
            &NewSession {
                id: "orch".to_owned(),
                task_id: None,
                role: Role::Orchestrator,
                rc_name: "orchestrator".to_owned(),
                host: "wmux".to_owned(),
                host_workspace: None,
                host_surface: None,
            },
            Actor::User,
        )
        .unwrap();
    w.session = "orch".to_owned();
    let text = context(&w.hook(HookEvent::UserPromptSubmit, &json!({ "prompt": "hi" })));
    assert!(
        text.contains("headwork update: update to v0.1.9 failed at smoke")
            && text.contains("headwork tick: the tick failed demo-sand.")
            && text.contains("Tell the user with a push notification."),
        "{text}"
    );
    assert!(UpdateState::load(&w.home).notices.is_empty(), "taken");
    let again = w.hook(HookEvent::UserPromptSubmit, &json!({ "prompt": "more" }));
    assert!(
        again.stdout.is_none_or(|out| !out.contains("v0.1.9")),
        "told once"
    );
}

fn ask_and_board(w: &mut World) -> (u32, u32) {
    w.pre("AskUserQuestion", &ask_input(&["Ship", "Hold"], "Ship"));
    let board = w.store.board(false).unwrap();
    (board.live_workers, board.waiting_workers)
}

#[test]
fn a_worker_waiting_on_the_user_with_nothing_running_frees_its_slot() {
    let mut w = world("", false);
    w.start();
    assert_eq!(w.store.board(false).unwrap().live_workers, 1, "working");
    assert_eq!(ask_and_board(&mut w), (0, 1), "the ask frees the slot");
    w.post("AskUserQuestion", &json!({ "answers": "Ship" }));
    let board = w.store.board(false).unwrap();
    assert_eq!(
        (board.live_workers, board.waiting_workers),
        (1, 0),
        "the answer takes it back"
    );
    let marks: Vec<bool> = w
        .events()
        .into_iter()
        .filter(|event| event.kind == "session-waiting")
        .map(|event| event.data["waiting"].as_bool().unwrap())
        .collect();
    assert_eq!(marks, [true, false], "marked, then cleared");
}

#[test]
fn a_worker_typed_past_its_ask_takes_its_slot_back() {
    let mut w = world("", false);
    w.start();
    assert_eq!(ask_and_board(&mut w), (0, 1), "waiting");
    w.hook(
        HookEvent::UserPromptSubmit,
        &json!({ "prompt": "do it another way" }),
    );
    let board = w.store.board(false).unwrap();
    assert_eq!(
        (board.live_workers, board.waiting_workers),
        (1, 0),
        "working again"
    );
}

#[test]
fn a_hook_call_clears_the_idle_mark() {
    let mut w = world("", false);
    w.start();
    let task = w.store.tasks_in(TaskState::Working).unwrap()[0];
    let session = w.store.session_of(task).unwrap().unwrap().id;
    assert!(
        w.store
            .set_session_idle(&session, true, Actor::Tick)
            .unwrap(),
        "marked"
    );
    w.hook(
        HookEvent::UserPromptSubmit,
        &json!({ "prompt": "back again" }),
    );
    assert!(
        !w.store
            .set_session_idle(&session, false, Actor::Tick)
            .unwrap(),
        "the hook call already cleared it"
    );
    let cleared = w
        .store
        .events(EventFilter {
            task: None,
            limit: 1000,
        })
        .unwrap()
        .iter()
        .any(|event| {
            event.kind == headwork_store::EVENT_SESSION_IDLE && event.data["idle"] == false
        });
    assert!(cleared, "a session-idle event with idle false was written");
}

#[test]
fn a_worker_running_a_shell_or_a_background_agent_keeps_its_slot() {
    let mut w = world("", false);
    w.start();
    w.slots.set_shell(true);
    assert_eq!(ask_and_board(&mut w), (1, 0), "a background shell is work");
    w.post("AskUserQuestion", &json!({ "answers": "Ship" }));
    w.slots.set_shell(false);
    w.post(
        "Agent",
        &json!({ "status": "async_launched", "agentId": "a1" }),
    );
    assert_eq!(
        ask_and_board(&mut w),
        (1, 0),
        "a background agent this turn is work"
    );
}

#[test]
fn the_tick_frees_the_slot_once_the_shell_ends() {
    let mut w = world("", false);
    w.start();
    w.slots.set_shell(true);
    assert_eq!(ask_and_board(&mut w), (1, 0), "busy");
    w.slots.set_shell(false);
    let freed = headwork::ops::waiting::recheck_all(&mut w.store, &w.slots, Actor::Tick).unwrap();
    assert_eq!(freed, 1, "one session freed");
    let board = w.store.board(false).unwrap();
    assert_eq!(
        (board.live_workers, board.waiting_workers),
        (0, 1),
        "waiting now"
    );
}

#[test]
fn a_freed_slot_runs_the_queue_when_a_task_waits() {
    let mut w = world_in("", "max_workers = 1", false);
    w.start();
    let brief = w.dir.path().join("brief.md");
    let request = SpawnRequest {
        request: TaskRequest {
            project: ProjectName::new("demo").unwrap(),
            slug: Slug::new("next").unwrap(),
            kind: "task".to_owned(),
            model: Some(Model::Haiku),
            effort: None,
            in_main: false,
            keep: false,
            close: CloseMode::Confirm,
            review: None,
            decision: None,
        },
        brief: &brief,
        depends_on: None,
        permission_mode: PermissionMode::Auto,
        allow_ask: &[],
    };
    let env = Env {
        home: &w.home,
        config: &w.config,
        host: &w.host,
        exe: Path::new(EXE),
        actor: Actor::User,
    };
    let (next, _) = spawn(&mut w.store, &env, &request).unwrap();
    assert_eq!(
        w.store.task(next.id).unwrap().state,
        TaskState::Queued,
        "the project is full"
    );
    assert_eq!(ask_and_board(&mut w), (0, 1), "waiting");
    assert_eq!(w.slots.pumps(), 1, "the queue runs");
    let env = Env {
        home: &w.home,
        config: &w.config,
        host: &w.host,
        exe: Path::new(EXE),
        actor: Actor::User,
    };
    let started = headwork::ops::spawn::pump(&mut w.store, &env).unwrap();
    assert_eq!(started.len(), 1, "one queued task tried");
    assert_eq!(
        w.store.task(next.id).unwrap().state,
        TaskState::Spawning,
        "it takes the freed slot"
    );
}

fn response(id: &str, input: u64) -> String {
    json!({
        "type": "assistant",
        "timestamp": "2026-11-22T10:00:00.000Z",
        "message": { "id": id, "model": "claude-haiku-5-5", "usage": { "input_tokens": input, "output_tokens": 1 } }
    })
    .to_string()
        + "\n"
}

#[test]
fn stop_and_session_end_read_the_sessions_usage() {
    let mut w = world("", false);
    w.start();
    let folder = w.dir.path().join(".claude/projects/S--demo");
    let subagents = folder.join(&w.session).join("subagents");
    std::fs::create_dir_all(&subagents).unwrap();
    let transcript = folder.join(format!("{}.jsonl", w.session));
    std::fs::write(&transcript, response("m1", 10)).unwrap();
    std::fs::write(subagents.join("agent-a1.jsonl"), response("m2", 20)).unwrap();
    w.hook(
        HookEvent::Stop,
        &json!({ "last_assistant_message": "done", "transcript_path": transcript.display().to_string() }),
    );
    let board = w.store.board(false).unwrap();
    assert_eq!(
        board.tasks[0].tokens, 32,
        "the session and its subagent after one turn"
    );

    let mut more = std::fs::read_to_string(&transcript).unwrap();
    more.push_str(&response("m3", 100));
    std::fs::write(&transcript, more).unwrap();
    w.hook(HookEvent::SessionEnd, &json!({ "reason": "other" }));
    let board = w.store.board(true).unwrap();
    assert_eq!(board.tasks[0].tokens, 133, "found by session id at the end");
}

#[test]
fn a_helper_claude_gets_a_session_id_and_counts_under_the_task() {
    let mut w = world("", false);
    w.start();
    let identity = worker_identity(&w);
    let (error, text) = w.tool(
        identity,
        "helper_tab",
        &json!({ "argv": ["claude", "--model", "haiku"] }),
    );
    assert!(!error, "{text}");
    let spawn = headwork_adapters::host::TerminalHost::panes(&w.host)
        .unwrap()
        .into_iter()
        .find(|pane| pane.label == "headwork:helper")
        .unwrap()
        .command;
    assert!(
        spawn.contains("--session-id"),
        "the helper is named: {spawn}"
    );
    let events = w
        .store
        .events(EventFilter {
            task: Some(w.task.id),
            limit: 50,
        })
        .unwrap();
    let helper = events
        .iter()
        .find(|event| event.kind == "helper-session")
        .unwrap();
    let id = helper.session_id.clone().unwrap();
    assert!(
        spawn.contains(&id),
        "the recorded id is the one passed: {spawn}"
    );
    assert_eq!(
        w.store.usage_tag(&id).unwrap(),
        Some(UsageTag {
            task: Some(w.task.id),
            role: headwork_core::UsageRole::Helper
        }),
        "its transcript counts under the task"
    );

    let identity = worker_identity(&w);
    let (error, text) = w.tool(
        identity,
        "helper_tab",
        &json!({ "argv": ["claude", "--resume", "x"] }),
    );
    assert!(!error, "{text}");
    let helpers = w
        .store
        .events(EventFilter {
            task: Some(w.task.id),
            limit: 50,
        })
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "helper-session")
        .count();
    assert_eq!(helpers, 1, "a resumed claude keeps its own session");
}

const REVIEWS: &str = "\n[leads]\nenabled = true\nmax_parts = 1\n\n[reviews]\nenabled = true\nlead_model = \"haiku\"\n";

#[test]
fn the_user_s_review_command_in_a_lead_s_tab_requests_a_review() {
    let mut w = world(REVIEWS, false);
    w.start();
    let reply = w.hook(
        HookEvent::UserPromptSubmit,
        &json!({ "prompt": "review 2 the park path" }),
    );
    let text = context(&reply);
    assert!(
        text.contains("level-2 review") && text.contains("`review_start`"),
        "{text}"
    );
    let review = w.store.open_review(w.task.id).unwrap().unwrap();
    assert_eq!(
        (review.state, review.flow.as_str(), review.focus.as_str()),
        (ReviewState::Requested, "code", "the park path"),
        "{review:?}"
    );

    let again = context(&w.hook(
        HookEvent::UserPromptSubmit,
        &json!({ "prompt": "review 1" }),
    ));
    assert!(again.contains("still requested"), "{again}");
    let near = context(&w.hook(
        HookEvent::UserPromptSubmit,
        &json!({ "prompt": "review 4" }),
    ));
    assert!(near.contains("no review started"), "{near}");
    let unknown = context(&w.hook(
        HookEvent::UserPromptSubmit,
        &json!({ "prompt": "review nosuch 1" }),
    ));
    assert!(unknown.contains("no flow `nosuch`"), "{unknown}");
    assert_eq!(
        w.store.reviews_of(w.task.id).unwrap().len(),
        1,
        "only the first request is stored"
    );
}

#[test]
fn a_review_command_from_another_session_or_with_reviews_off_does_nothing() {
    let mut w = world(REVIEWS, false);
    w.start();
    let message = json!({
        "prompt": concat!(
            "<cross-session-message from=\"uds:x\" from-name=\"orchestrator\" from-mode=\"prompting\">",
            "review 3</cross-session-message>",
        )
    });
    w.hook(HookEvent::UserPromptSubmit, &message);
    assert!(
        w.store.open_review(w.task.id).unwrap().is_none(),
        "a message is not the user's word"
    );
    let mut off = world(LEADS, false);
    off.start();
    off.hook(
        HookEvent::UserPromptSubmit,
        &json!({ "prompt": "review 2" }),
    );
    assert!(
        off.store.open_review(off.task.id).unwrap().is_none(),
        "reviews are off"
    );
    let names = off.tool_names(worker_identity(&off));
    assert!(
        names.contains(&"review_start".to_owned()),
        "the tool is listed for a lead: {names:?}"
    );
    let (is_error, text) = off.tool(worker_identity(&off), "review_start", &json!({}));
    assert!(is_error && text.contains("reviews are off"), "{text}");
}

#[test]
fn review_start_needs_the_user_s_request_and_a_clean_tree_then_starts_the_reviewer_lead() {
    let mut w = world(
        "push_window_secs = 0\n[leads]\nenabled = true\nmax_parts = 1\n\n[reviews]\nenabled = true\nlead_model = \"haiku\"\n",
        false,
    );
    w.start();
    let (is_error, text) = w.tool(worker_identity(&w), "review_start", &json!({}));
    assert!(is_error && text.contains("review-unrequested"), "{text}");

    w.hook(
        HookEvent::UserPromptSubmit,
        &json!({ "prompt": "review 2" }),
    );
    w.hook(
        HookEvent::Stop,
        &json!({ "last_assistant_message": "Working on it." }),
    );
    let rules: Vec<String> = w.denies().into_iter().map(|(_, rule)| rule).collect();
    assert_eq!(
        rules,
        ["review-unstarted"],
        "the lead may not stop before it starts the review"
    );

    let tree = w.worktree();
    std::fs::write(tree.join("change.txt"), "new").unwrap();
    let (is_error, text) = w.tool(worker_identity(&w), "review_start", &json!({}));
    assert!(is_error && text.contains("review-dirty"), "{text}");
    git(&tree, &["add", "change.txt"]);
    git(&tree, &["commit", "-q", "-m", "change"]);

    let (is_error, text) = w.tool(worker_identity(&w), "review_start", &json!({}));
    assert!(
        !is_error && text.contains("Fix or dispute every finding"),
        "{text}"
    );
    let review = w.store.open_review(w.task.id).unwrap().unwrap();
    assert_eq!(review.state, ReviewState::Running, "{review:?}");
    let part = part_named(&w, "sand-review");
    assert_eq!(part.kind, "review", "a part of kind review");
    assert_eq!(review.part_id, Some(part.id), "the review knows its part");
    let head = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&tree)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(
        review.head_sha.as_deref(),
        Some(head.trim()),
        "the lead's HEAD is pinned"
    );
    let checkout = w.dir.path().join("demo-sand-review");
    assert!(
        checkout.join("change.txt").is_file(),
        "the review worktree is at the head"
    );
    let branch = Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(!branch.status.success(), "the review worktree is detached");
    let brief = std::fs::read_to_string(w.home.brief_path(&part.name)).unwrap();
    for wanted in [
        "level 2",
        "- `correctness`, model opus",
        "- `verify`, model sonnet, after",
        "`review_next`",
        "`demo-sand`",
    ] {
        assert!(
            brief.contains(wanted),
            "{wanted} missing from the brief:\n{brief}"
        );
    }
    let (is_error, text) = w.tool(worker_identity(&w), "review_start", &json!({}));
    assert!(is_error && text.contains("started already"), "{text}");
    let identity = worker_identity(&w);
    let (is_error, text) = w.tool(identity, "part_drop", &json!({ "name": "review" }));
    assert!(!is_error, "{text}");
    assert!(
        !checkout.exists(),
        "the review worktree goes with its part: {text}"
    );
}

#[test]
fn reviewer_rules_reach_a_review_part_and_no_one_else() {
    let mut lead = world(REVIEWS, false);
    let text = context(&lead.start());
    assert!(
        !text.contains("You are a reviewer lead")
            && !text.contains("longer than a hook may inject"),
        "a lead gets its full rules and no reviewer rules: {text}"
    );
    let mut reviewer = world_of("\n[reviews]\nenabled = true\n", "", false, "review");
    let text = context(&reviewer.start());
    assert!(
        text.contains("## You are a reviewer lead")
            && !text.contains("longer than a hook may inject"),
        "a review task gets its full rules with the reviewer rules: {text}"
    );
}

/// A lead world with a review of `level` started on a committed change; returns the reviewer lead's part.
fn started_review(w: &mut World, level: u8) -> Task {
    w.start();
    w.hook(
        HookEvent::UserPromptSubmit,
        &json!({ "prompt": format!("review {level}") }),
    );
    let tree = w.worktree();
    std::fs::write(tree.join("change.txt"), "new").unwrap();
    git(&tree, &["add", "change.txt"]);
    git(&tree, &["commit", "-q", "-m", "change"]);
    let (is_error, text) = w.tool(worker_identity(w), "review_start", &json!({}));
    assert!(!is_error, "{text}");
    part_named(w, "sand-review")
}

fn reviewer(w: &World, part: &Task) -> Identity {
    Identity {
        session: Some(w.store.session_of(part.id).unwrap().unwrap().id),
        role: Some(Role::Worker),
    }
}

fn json_of(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn finding(summary: &str) -> Value {
    json!({ "file": "change.txt", "line": 1, "severity": "important", "summary": summary, "scenario": "it breaks" })
}

fn node_done(node: &str, extra: &Value) -> Value {
    let mut args = json!({ "node": node });
    args.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    args
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one level-1 review walked from start to finish"
)]
fn a_level_one_review_runs_its_nodes_delivers_rechecks_the_fix_and_finishes() {
    let mut w = world(REVIEWS, false);
    let part = started_review(&mut w, 1);
    let me = reviewer(&w, &part);
    let names = w.tool_names(me.clone());
    assert!(
        names.contains(&"review_next".to_owned()) && !names.contains(&"review_resolve".to_owned()),
        "{names:?}"
    );
    let lead_names = w.tool_names(worker_identity(&w));
    assert!(
        !lead_names.contains(&"review_next".to_owned()),
        "{lead_names:?}"
    );

    let (is_error, text) = w.tool(me.clone(), "review_next", &json!({}));
    assert!(!is_error, "{text}");
    let next = json_of(&text);
    let nodes: Vec<&str> = next["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["node"].as_str().unwrap())
        .collect();
    assert_eq!(
        nodes,
        ["correctness", "tests", "rules"],
        "level 1 runs three reviewers"
    );
    let prompt = next["nodes"][0]["prompt"].as_str().unwrap();
    assert!(
        prompt.contains("demo-sand-review") && prompt.contains("Answer with a JSON array"),
        "{prompt}"
    );
    assert_eq!(
        next["nodes"][0]["model"], "sonnet",
        "the node's model at level 1"
    );

    let (is_error, text) = w.tool(me.clone(), "review_deliver", &json!({}));
    assert!(is_error && text.contains("review-nodes-open"), "{text}");
    for args in [
        node_done(
            "correctness",
            &json!({ "findings": [finding("off by one")] }),
        ),
        node_done("tests", &json!({ "findings": [] })),
        node_done("rules", &json!({ "failed": "the subagent timed out" })),
    ] {
        let (is_error, text) = w.tool(me.clone(), "review_node_done", &args);
        assert!(!is_error, "{text}");
    }
    let (_, text) = w.tool(me.clone(), "review_next", &json!({}));
    assert!(
        json_of(&text)["next"]
            .as_str()
            .unwrap()
            .contains("review_deliver"),
        "{text}"
    );
    let (is_error, text) = w.tool(me.clone(), "review_deliver", &json!({}));
    assert!(!is_error, "{text}");
    let delivered = json_of(&text);
    assert_eq!(
        delivered["tell"], "demo-sand",
        "the implementing lead is told"
    );
    let id = delivered["findings"][0]["id"].as_i64().unwrap();
    assert_eq!(delivered["findings"].as_array().unwrap().len(), 1, "{text}");

    let lead = worker_identity(&w);
    let (is_error, text) = w.tool(lead.clone(), "review_resolve", &json!({ "items": [] }));
    assert!(is_error && text.contains("missing"), "{text}");
    let tree = w.worktree();
    std::fs::write(tree.join("change.txt"), "fixed").unwrap();
    let fix =
        json!({ "items": [{ "id": id, "action": "fixed", "note": "fixed in the next commit" }] });
    let (is_error, text) = w.tool(lead.clone(), "review_resolve", &fix);
    assert!(is_error && text.contains("review-dirty"), "{text}");
    git(&tree, &["commit", "-q", "-am", "fix"]);
    let (is_error, text) = w.tool(lead.clone(), "review_resolve", &fix);
    assert!(!is_error, "{text}");
    assert_eq!(
        json_of(&text)["tell"],
        "demo-sand-review",
        "the reviewer lead is told"
    );
    let checkout = w.dir.path().join("demo-sand-review");
    assert_eq!(
        std::fs::read_to_string(checkout.join("change.txt")).unwrap(),
        "fixed",
        "the review worktree follows"
    );

    let (is_error, text) = w.tool(lead, "done", &json!({ "summary": "x" }));
    assert!(is_error && text.contains("review-open"), "{text}");

    let (is_error, text) = w.tool(
        me.clone(),
        "review_verdict",
        &json!({ "items": [{ "id": id, "dispute": "kept" }] }),
    );
    assert!(is_error && text.contains("give `fix`"), "{text}");
    let (is_error, text) = w.tool(
        me.clone(),
        "review_verdict",
        &json!({ "items": [{ "id": id, "fix": "accepted" }] }),
    );
    assert!(
        !is_error
            && json_of(&text)["next"]
                .as_str()
                .unwrap()
                .contains("review_finish"),
        "{text}"
    );
    let (is_error, text) = w.tool(me, "review_finish", &json!({}));
    assert!(!is_error && text.contains("1 fixed"), "{text}");
    let review = w.store.reviews_of(w.task.id).unwrap().pop().unwrap();
    assert_eq!(review.state, ReviewState::Done, "{review:?}");
}

#[test]
fn a_level_two_dispute_the_reviewer_keeps_goes_to_the_user_and_their_answer_settles_it() {
    let mut w = world(REVIEWS, false);
    let part = started_review(&mut w, 2);
    let me = reviewer(&w, &part);
    let (_, text) = w.tool(me.clone(), "review_next", &json!({}));
    let nodes: Vec<String> = json_of(&text)["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["node"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        nodes.len(),
        6,
        "six reviewers before the verifier: {nodes:?}"
    );
    for node in &nodes {
        let findings = if node == "design" {
            json!([finding("wrong layer"), finding("not a bug")])
        } else {
            json!([])
        };
        let (is_error, text) = w.tool(
            me.clone(),
            "review_node_done",
            &json!({ "node": node, "findings": findings }),
        );
        assert!(!is_error, "{text}");
    }
    let (_, text) = w.tool(me.clone(), "review_next", &json!({}));
    let verify = json_of(&text);
    let items: Vec<i64> = verify["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["item"].as_i64().unwrap())
        .collect();
    assert_eq!(items.len(), 2, "one verifier per finding: {text}");
    assert!(
        verify["nodes"][0]["prompt"]
            .as_str()
            .unwrap()
            .contains("wrong layer"),
        "the verifier sees its finding"
    );
    for (item, verdict) in items.iter().zip(["confirmed", "rejected"]) {
        let args = json!({ "node": "verify", "item": item, "verdict": verdict });
        let (is_error, text) = w.tool(me.clone(), "review_node_done", &args);
        assert!(!is_error, "{text}");
    }
    let (_, text) = w.tool(me.clone(), "review_deliver", &json!({}));
    let delivered = json_of(&text);
    assert_eq!(
        delivered["findings"].as_array().unwrap().len(),
        1,
        "the rejected finding stays back: {text}"
    );
    let id = items[0];
    let dispute = json!({ "items": [{ "id": id, "action": "disputed", "note": "the layer is right: docs/design.md 3.2" }] });
    let (is_error, text) = w.tool(worker_identity(&w), "review_resolve", &dispute);
    assert!(!is_error, "{text}");
    let (is_error, text) = w.tool(
        me.clone(),
        "review_verdict",
        &json!({ "items": [{ "id": id, "dispute": "kept" }] }),
    );
    assert!(!is_error, "{text}");
    assert!(
        json_of(&text)["next"]
            .as_str()
            .unwrap()
            .contains("headed `Dispute`"),
        "{text}"
    );
    let review = w.store.reviews_of(w.task.id).unwrap().pop().unwrap();
    assert_eq!(review.state, ReviewState::Asking, "{review:?}");

    let question = format!("Finding {id}: wrong layer. The lead says the layer is right. Keep it?");
    let asked = json!({ "questions": [{ "question": question, "header": "Dispute", "options": [
        { "label": "Keep the finding" }, { "label": "Drop it" }
    ] }] });
    let lead_session = std::mem::replace(&mut w.session, me.session.clone().unwrap());
    let mut answers = serde_json::Map::new();
    answers.insert(question, json!("Drop it"));
    w.hook(
        HookEvent::PostToolUse,
        &json!({ "tool_name": "AskUserQuestion", "tool_input": asked, "tool_response": { "answers": answers } }),
    );
    w.session = lead_session;
    let review = w.store.reviews_of(w.task.id).unwrap().pop().unwrap();
    assert_eq!(
        review.state,
        ReviewState::Rechecking,
        "the answer settles the dispute: {review:?}"
    );
    let (is_error, text) = w.tool(me, "review_finish", &json!({}));
    assert!(
        !is_error && text.contains("1 dropped by the user"),
        "{text}"
    );
}

#[test]
fn closing_the_reviewer_lead_s_part_cancels_its_review() {
    let mut w = world(REVIEWS, false);
    let _part = started_review(&mut w, 1);
    let (is_error, text) = w.tool(
        worker_identity(&w),
        "part_drop",
        &json!({ "name": "review" }),
    );
    assert!(!is_error, "{text}");
    let review = w.store.reviews_of(w.task.id).unwrap().pop().unwrap();
    assert_eq!(review.state, ReviewState::Cancelled, "{review:?}");
}

#[test]
fn kind_silent_still_means_a_silent_close() {
    let w = world_of(LEADS, "", false, SILENT_KIND);
    assert_eq!(w.task.close, CloseMode::Silent);
    let plain = world(LEADS, false);
    assert_eq!(plain.task.close, CloseMode::Confirm);
}

#[test]
fn a_silent_flag_on_an_ordinary_kind_closes_without_the_users_go() {
    let mut w = world_flagged(LEADS, "", false, "task", CloseMode::Silent, None);
    w.start();
    w.post("SendMessage", &json!({}));
    let identity = worker_identity(&w);
    let (is_error, text) = w.tool(identity, "done", &json!({ "summary": "built" }));
    assert!(!is_error, "a silent lead closes without asking: {text}");
}

#[test]
fn done_with_a_planned_review_requests_it_and_is_refused() {
    let mut w = world_flagged(
        REVIEWS,
        "",
        false,
        "task",
        CloseMode::Silent,
        Some(Level::Standard),
    );
    w.start();
    assert_eq!(
        w.store.task(w.task.id).unwrap().review_plan,
        Some(Level::Standard)
    );
    let (is_error, text) = w.tool(worker_identity(&w), "done", &json!({ "summary": "built" }));
    assert!(is_error && text.contains("review-planned"), "{text}");
    let review = w.store.open_review(w.task.id).unwrap().unwrap();
    assert_eq!(
        (review.state, review.level, review.flow.as_str()),
        (ReviewState::Requested, Level::Standard, "code"),
        "{review:?}"
    );
    assert_eq!(
        w.store.task(w.task.id).unwrap().review_plan,
        None,
        "the plan is spent"
    );
    assert!(
        w.events()
            .iter()
            .any(|event| event.kind == "review-planned"),
        "the request is logged"
    );
    let (is_error, text) = w.tool(worker_identity(&w), "done", &json!({ "summary": "built" }));
    assert!(is_error && text.contains("review-open"), "{text}");
}

#[test]
fn a_planned_review_is_ignored_while_reviews_are_disabled() {
    let mut w = world_flagged(
        LEADS,
        "",
        false,
        "task",
        CloseMode::Silent,
        Some(Level::Light),
    );
    w.start();
    w.post("SendMessage", &json!({}));
    let (is_error, text) = w.tool(worker_identity(&w), "done", &json!({ "summary": "built" }));
    assert!(!is_error, "{text}");
    assert!(w.store.open_review(w.task.id).unwrap().is_none());
    assert_eq!(
        w.store.task(w.task.id).unwrap().review_plan,
        Some(Level::Light)
    );
}

#[test]
fn the_user_s_flag_words_in_a_lead_s_tab_change_its_flags() {
    let mut w = world_flagged(
        REVIEWS,
        "",
        false,
        "task",
        CloseMode::Confirm,
        Some(Level::Standard),
    );
    w.start();
    let prompt = |w: &mut World, text: &str| {
        context(&w.hook(HookEvent::UserPromptSubmit, &json!({ "prompt": text })))
    };
    let text = prompt(&mut w, "Silent.");
    assert!(text.contains("the user set this task to silent"), "{text}");
    assert_eq!(w.store.task(w.task.id).unwrap().close, CloseMode::Silent);
    let text = prompt(&mut w, "confirm");
    assert!(text.contains("the user set this task to confirm"), "{text}");
    assert_eq!(w.store.task(w.task.id).unwrap().close, CloseMode::Confirm);
    let text = prompt(&mut w, "no review");
    assert!(
        text.contains("the user set this task to no review"),
        "{text}"
    );
    assert_eq!(w.store.task(w.task.id).unwrap().review_plan, None);
    assert!(
        w.events()
            .iter()
            .filter(|event| event.kind == "flags-set")
            .count()
            >= 3,
        "each change is logged"
    );
}

#[test]
fn the_user_s_review_command_clears_the_plan() {
    let mut w = world_flagged(
        REVIEWS,
        "",
        false,
        "task",
        CloseMode::Confirm,
        Some(Level::Light),
    );
    w.start();
    w.hook(
        HookEvent::UserPromptSubmit,
        &json!({ "prompt": "review 3" }),
    );
    assert_eq!(w.store.task(w.task.id).unwrap().review_plan, None);
}

#[test]
fn the_orchestrators_session_start_ends_with_the_digest_within_the_hook_limit() {
    let mut w = world("", false);
    w.store
        .add_backlog_item(
            &digest_backlog_item("Review the invented draft"),
            Actor::Orchestrator,
        )
        .unwrap();
    w.store
        .record_session(
            &NewSession {
                id: "orch".to_owned(),
                task_id: None,
                role: Role::Orchestrator,
                rc_name: "orchestrator".to_owned(),
                host: "wmux".to_owned(),
                host_workspace: None,
                host_surface: None,
            },
            Actor::User,
        )
        .unwrap();
    let worker = context(&w.start());
    assert!(!worker.contains("Needs you"), "a worker gets no digest");
    w.session = "orch".to_owned();
    let text = context(&w.start());
    assert!(text.len() <= 10_000, "{}", text.len());
    let prompt = w.hook(HookEvent::UserPromptSubmit, &json!({ "prompt": "hi" }));
    let first = prompt.stdout.unwrap_or_default();
    let at_start = text.contains("Review the invented draft");
    let at_prompt = first.contains("Review the invented draft");
    assert!(
        at_start != at_prompt,
        "the digest comes with the start when it fits, else with the first prompt, never both: {at_start} {at_prompt}"
    );
}

fn digest_backlog_item(text: &str) -> headwork_core::backlog::BacklogDraft {
    let mut draft = headwork_core::backlog::parse(&format!("## demo\n\n- [ ] For you: {text}\n"))
        .items
        .remove(0);
    draft.state = headwork_core::backlog::BacklogState::Open;
    draft
}

#[test]
fn the_orchestrators_board_and_status_tools_start_with_the_digest_and_a_workers_board_does_not() {
    let mut w = world("", false);
    w.store
        .add_backlog_item(
            &digest_backlog_item("Sign the invented form"),
            Actor::Orchestrator,
        )
        .unwrap();
    for tool in ["board", "status"] {
        let (is_error, text) = w.tool(orchestrator_identity(), tool, &json!({}));
        assert!(!is_error, "{text}");
        assert!(
            text.starts_with("Needs you\n- backlog #1: Sign the invented form\n"),
            "{tool}: {text}"
        );
    }
    let worker = Identity {
        session: Some(w.session.clone()),
        role: Some(Role::Worker),
    };
    let (is_error, text) = w.tool(worker, "board", &json!({}));
    assert!(!is_error && !text.contains("Needs you"), "{text}");
}

fn record_spend(w: &mut World, session: &str, id: &str, tokens: Tokens) {
    let line = UsageLine {
        message_id: id.to_owned(),
        at: "2026-11-26T10:00:00Z".to_owned(),
        model: "claude-sonnet-4-6".to_owned(),
        effort: None,
        speed: None,
        service_tier: None,
        cwd: None,
        tokens,
    };
    let tag = w
        .store
        .usage_tag(session)
        .unwrap()
        .unwrap_or(UsageTag::UNMANAGED);
    w.store
        .record_usage(&UsageBatch {
            path: &format!("{id}.jsonl"),
            session_id: session,
            agent_id: None,
            agent_meta: None,
            tag,
            dir: "S:/Code/x",
            lines: &[line],
            mark: FileMark {
                read_to: 1,
                size: 1,
                mtime_ms: 1,
            },
        })
        .unwrap();
}

#[test]
fn board_and_status_show_a_running_review_under_its_lead() {
    let mut w = world(REVIEWS, false);
    let _part = started_review(&mut w, 1);
    let summary = w.store.review_summary(w.task.id).unwrap().unwrap();
    assert_eq!(
        summary.to_string(),
        "review 1 code, level 1, round 1: running, 0 node(s) over, 0 running, 0 finding(s) · 0 tokens",
        "the board line"
    );
    let board = w.store.board(false).unwrap();
    let lead = board
        .tasks
        .iter()
        .find(|entry| entry.task.id == w.task.id)
        .unwrap();
    assert_eq!(
        lead.review.as_ref().map(|review| review.id),
        Some(summary.id),
        "on the lead's board entry"
    );
    let part_entry = board
        .tasks
        .iter()
        .find(|entry| entry.task.kind == "review")
        .unwrap();
    assert!(part_entry.review.is_none(), "not repeated on the part");
    let lines = headwork::ops::status::lines(&w.store, &board, &w.config.usage).unwrap();
    let line = lines.iter().find(|line| line.task == "demo-sand").unwrap();
    assert_eq!(
        line.line, "demo-sand: working, in review (level 1)",
        "the lead's own line"
    );
    assert_eq!(
        line.review.as_ref().map(|review| review.state.as_str()),
        Some("running"),
        "the JSON review comes from the same review"
    );
}

#[test]
fn status_says_review_requested_until_the_review_starts() {
    let mut w = world(REVIEWS, false);
    w.start();
    w.hook(
        HookEvent::UserPromptSubmit,
        &json!({ "prompt": "review 2" }),
    );
    let board = w.store.board(false).unwrap();
    let lines = headwork::ops::status::lines(&w.store, &board, &w.config.usage).unwrap();
    let line = lines.iter().find(|line| line.task == "demo-sand").unwrap();
    assert_eq!(line.line, "demo-sand: working, review requested (level 2)");
}

#[test]
fn a_reviews_summary_counts_nodes_and_findings_over_its_rounds_and_ends_with_the_review() {
    let mut w = world(REVIEWS, false);
    let part = started_review(&mut w, 3);
    let me = reviewer(&w, &part);
    let summary = |w: &World| w.store.review_summary(w.task.id).unwrap().unwrap();
    let mut over = 0;
    let mut running = 0;
    let mut seen_running = false;
    loop {
        let (is_error, text) = w.tool(me.clone(), "review_next", &json!({}));
        assert!(!is_error, "{text}");
        let batch: Vec<Value> = json_of(&text)["nodes"].as_array().unwrap().clone();
        if batch.is_empty() {
            break;
        }
        running += batch.len();
        if !seen_running {
            seen_running = true;
            let now = summary(&w);
            assert_eq!(
                (now.nodes_over, now.nodes_running, now.findings),
                (0, u32::try_from(running).unwrap(), 0),
                "nodes taken and running"
            );
        }
        for node in batch {
            let name = node["node"].as_str().unwrap();
            let args = match name {
                "correctness" => json!({ "node": name, "findings": [finding("off by one")] }),
                "security" => json!({ "node": name, "failed": "the subagent timed out" }),
                "verify" => json!({ "node": name, "item": node["item"], "verdict": "confirmed" }),
                _ => json!({ "node": name, "findings": [] }),
            };
            let (is_error, text) = w.tool(me.clone(), "review_node_done", &args);
            assert!(!is_error, "{text}");
            running -= 1;
            over += 1;
        }
    }
    let now = summary(&w);
    assert_eq!(
        (now.round, now.nodes_over, now.nodes_running, now.findings),
        (1, u32::try_from(over).unwrap(), 0, 1),
        "round 1: done and failed nodes both over, one finding"
    );
    assert!(
        now.to_string()
            .contains(&format!("{over} node(s) over, 0 running, 1 finding(s)")),
        "{now}"
    );
    let (is_error, text) = w.tool(me.clone(), "review_deliver", &json!({}));
    assert!(!is_error, "{text}");
    let id = json_of(&text)["findings"][0]["id"].as_i64().unwrap();
    let tree = w.worktree();
    std::fs::write(tree.join("change.txt"), "fixed").unwrap();
    git(&tree, &["commit", "-q", "-am", "fix"]);
    let fix = json!({ "items": [{ "id": id, "action": "fixed", "note": "fixed" }] });
    let (is_error, text) = w.tool(worker_identity(&w), "review_resolve", &fix);
    assert!(!is_error, "{text}");
    let (is_error, text) = w.tool(
        me.clone(),
        "review_verdict",
        &json!({ "items": [{ "id": id, "fix": "accepted" }] }),
    );
    assert!(!is_error, "{text}");
    let now = summary(&w);
    assert_eq!(
        (now.round, now.nodes_over, now.nodes_running, now.findings),
        (2, 0, 0, 1),
        "round 2 starts with no nodes and keeps round 1's finding"
    );
    let (_, text) = w.tool(me.clone(), "review_next", &json!({}));
    let batch = json_of(&text)["nodes"].as_array().unwrap().len();
    let args = json!({ "node": "correctness", "findings": [finding_of("important", "again", 2)] });
    let (is_error, text) = w.tool(me, "review_node_done", &args);
    assert!(!is_error, "{text}");
    let now = summary(&w);
    assert_eq!(
        (now.round, now.nodes_over, now.nodes_running, now.findings),
        (2, 1, u32::try_from(batch).unwrap() - 1, 2),
        "round 2: one node over, the rest running, findings over both rounds"
    );
    let review = w.store.open_review(w.task.id).unwrap().unwrap();
    w.store
        .move_review(
            review.id,
            ReviewTrigger::Cancel,
            false,
            false,
            Actor::Orchestrator,
        )
        .unwrap();
    assert!(
        w.store.review_summary(w.task.id).unwrap().is_none(),
        "a cancelled review has no summary"
    );
}

#[test]
fn a_review_s_fresh_tokens_show_on_board_and_status_and_stay_after_it() {
    let mut w = world(REVIEWS, false);
    let part = started_review(&mut w, 1);
    let session = w.store.session_of(part.id).unwrap().unwrap().id;
    record_spend(
        &mut w,
        &session,
        "r1",
        Tokens {
            input: 1_000,
            cache_write_5m: 200_000,
            cache_read: 5_000_000,
            output: 50_000,
            ..Tokens::default()
        },
    );
    let lead_session = w.session.clone();
    record_spend(
        &mut w,
        &lead_session,
        "l1",
        Tokens {
            input: 9_000_000,
            ..Tokens::default()
        },
    );
    let summary = w.store.review_summary(w.task.id).unwrap().unwrap();
    assert_eq!(
        summary.tokens, 251_000,
        "fresh tokens, cache reads left out"
    );
    assert!(
        summary.to_string().ends_with("0 finding(s) · 251k tokens"),
        "{summary}"
    );
    let board = w.store.board(false).unwrap();
    let lines = headwork::ops::status::lines(&w.store, &board, &w.config.usage).unwrap();
    assert!(
        lines[0]
            .line
            .ends_with(", in review (level 1, 251k review tokens)"),
        "{}",
        lines[0].line
    );
    assert_eq!(
        lines[0].review_tokens, 251_000,
        "the JSON field, the lead's own spend left out"
    );

    let (is_error, text) = w.tool(
        worker_identity(&w),
        "part_drop",
        &json!({ "name": "review" }),
    );
    assert!(!is_error, "{text}");
    let board = w.store.board(false).unwrap();
    let lines = headwork::ops::status::lines(&w.store, &board, &w.config.usage).unwrap();
    assert!(
        lines[0].line.ends_with(", reviews 251k tokens"),
        "a finished review's cost stays: {}",
        lines[0].line
    );
}

fn finding_of(severity: &str, summary: &str, line: u32) -> Value {
    json!({ "file": "change.txt", "line": line, "severity": severity, "summary": summary, "scenario": "it breaks" })
}

/// Runs a review round to its delivery: each plain node returns its findings in `found` (none when absent), each
/// verify instance takes the next of `verdicts` (else `confirmed`). Returns the delivery and every `review_node_done`
/// answer.
fn run_round(
    w: &mut World,
    me: &Identity,
    found: &[(&str, Value)],
    verdicts: &[&str],
) -> (Value, Vec<String>) {
    let mut verdicts = verdicts.iter();
    let mut answers = Vec::new();
    let mut verified = 0;
    loop {
        let (is_error, text) = w.tool(me.clone(), "review_next", &json!({}));
        assert!(!is_error, "{text}");
        let next = json_of(&text);
        let nodes = next["nodes"].as_array().unwrap().clone();
        if nodes.is_empty() {
            assert!(
                next["next"].as_str().unwrap().contains("review_deliver"),
                "{text}"
            );
            break;
        }
        for node in nodes {
            let name = node["node"].as_str().unwrap();
            let item = node["item"].as_i64().unwrap();
            let args = if item > 0 {
                verified += 1;
                let verdict = verdicts.next().copied().unwrap_or("confirmed");
                json!({ "node": name, "item": item, "verdict": verdict })
            } else {
                let findings = found
                    .iter()
                    .find(|(node, _)| *node == name)
                    .map_or_else(|| json!([]), |(_, findings)| findings.clone());
                json!({ "node": name, "findings": findings })
            };
            let (is_error, text) = w.tool(me.clone(), "review_node_done", &args);
            assert!(!is_error, "{text}");
            answers.push(text);
        }
    }
    let (is_error, text) = w.tool(me.clone(), "review_deliver", &json!({}));
    assert!(!is_error, "{text}");
    let mut delivered = json_of(&text);
    delivered["verified"] = json!(verified);
    (delivered, answers)
}

fn ids_of(findings: &Value) -> Vec<i64> {
    findings
        .as_array()
        .map(|list| {
            list.iter()
                .map(|finding| finding["id"].as_i64().unwrap())
                .collect()
        })
        .unwrap_or_default()
}

fn fix_all(w: &mut World, me: &Identity, ids: &[i64], round: usize) -> Value {
    let tree = w.worktree();
    std::fs::write(tree.join("change.txt"), format!("fixed {round}")).unwrap();
    git(&tree, &["commit", "-q", "-am", "fix"]);
    let items: Vec<Value> = ids
        .iter()
        .map(|id| json!({ "id": id, "action": "fixed", "note": "fixed" }))
        .collect();
    let (is_error, text) = w.tool(
        worker_identity(w),
        "review_resolve",
        &json!({ "items": items }),
    );
    assert!(!is_error, "{text}");
    let verdicts: Vec<Value> = ids
        .iter()
        .map(|id| json!({ "id": id, "fix": "accepted" }))
        .collect();
    let (is_error, text) = w.tool(me.clone(), "review_verdict", &json!({ "items": verdicts }));
    assert!(!is_error, "{text}");
    json_of(&text)
}

#[test]
fn a_nit_below_the_floor_is_listed_not_delivered_and_gets_no_verifier() {
    let mut w = world(REVIEWS, false);
    let part = started_review(&mut w, 2);
    let me = reviewer(&w, &part);
    let found = [(
        "design",
        json!([
            finding_of("important", "wrong layer", 1),
            finding_of("nit", "odd name", 2)
        ]),
    )];
    let (delivered, _) = run_round(&mut w, &me, &found, &[]);
    assert_eq!(delivered["verified"], 1, "no verify subagent for the nit");
    let findings = w
        .store
        .review_findings(w.store.reviews_of(w.task.id).unwrap()[0].id)
        .unwrap();
    let nit = findings
        .iter()
        .find(|row| row.summary == "odd name")
        .unwrap();
    let important = findings
        .iter()
        .find(|row| row.summary == "wrong layer")
        .unwrap();
    assert_eq!(
        ids_of(&delivered["findings"]),
        [important.id],
        "{delivered}"
    );
    assert_eq!(
        delivered["listed"],
        json!([{ "finding": nit, "reason": "below-floor" }]),
        "{delivered}"
    );
    assert_eq!(nit.resolution, None, "the nit never waits on the lead");
    assert!(
        delivered["next"]
            .as_str()
            .unwrap()
            .contains("for information"),
        "{delivered}"
    );
}

#[test]
fn the_round_cap_lists_the_overflow_by_severity_then_verdict_then_number() {
    let mut w = world(&format!("{REVIEWS}max_round_findings = 2\n"), false);
    let part = started_review(&mut w, 2);
    let me = reviewer(&w, &part);
    let found = [(
        "design",
        json!([
            finding_of("important", "a", 1),
            finding_of("important", "b", 2),
            finding_of("doubt", "c", 3),
            finding_of("important", "d", 4)
        ]),
    )];
    let (delivered, _) = run_round(
        &mut w,
        &me,
        &found,
        &["plausible", "confirmed", "confirmed", "confirmed"],
    );
    let rows = w
        .store
        .review_findings(w.store.reviews_of(w.task.id).unwrap()[0].id)
        .unwrap();
    let id = |summary: &str| rows.iter().find(|row| row.summary == summary).unwrap().id;
    assert_eq!(
        ids_of(&delivered["findings"]),
        [id("b"), id("d")],
        "the two confirmed important findings: {delivered}"
    );
    let listed: Vec<(i64, &str)> = delivered["listed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["finding"]["id"].as_i64().unwrap(),
                entry["reason"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [(id("a"), "over-round-cap"), (id("c"), "over-round-cap")],
        "the plausible important one, then the doubt"
    );
}

#[test]
fn a_finding_repeated_in_round_two_is_dropped() {
    let mut w = world(REVIEWS, false);
    let part = started_review(&mut w, 3);
    let me = reviewer(&w, &part);
    let found = [(
        "correctness",
        json!([finding_of("important", "Off by one", 1)]),
    )];
    let (delivered, _) = run_round(&mut w, &me, &found, &[]);
    let first = ids_of(&delivered["findings"]);
    assert_eq!(first.len(), 1, "{delivered}");
    let after = fix_all(&mut w, &me, &first, 1);
    assert!(
        after["next"].as_str().unwrap().contains("Round 2"),
        "an important fix starts round 2: {after}"
    );

    let found = [
        (
            "correctness",
            json!([
                finding_of("important", "off-by-one!", 9),
                finding_of("important", "a new defect", 5)
            ]),
        ),
        (
            "tests",
            json!([finding_of("important", "untested branch", 1)]),
        ),
    ];
    let (delivered, answers) = run_round(&mut w, &me, &found, &[]);
    let dropped = answers
        .iter()
        .find(|text| text.contains("`correctness`"))
        .unwrap();
    assert!(
        dropped.contains("1 dropped as repeats") && dropped.contains(&format!("[{}]", first[0])),
        "{dropped}"
    );
    let rows = w
        .store
        .review_findings(w.store.reviews_of(w.task.id).unwrap()[0].id)
        .unwrap();
    let second: Vec<&str> = rows
        .iter()
        .filter(|row| row.round == 2)
        .map(|row| row.summary.as_str())
        .collect();
    assert_eq!(
        second,
        ["a new defect", "untested branch"],
        "the repeat is not recorded; the same line from another node is"
    );
    assert_eq!(ids_of(&delivered["findings"]).len(), 2, "{delivered}");
    let logged = w
        .events()
        .into_iter()
        .find(|event| {
            event.kind == "review-node"
                && event.data["node"] == "correctness"
                && event.data["round"] == 2
        })
        .unwrap();
    assert_eq!(logged.data["repeats"], json!([first[0]]), "{logged:?}");
}

#[test]
fn a_fixed_doubt_alone_does_not_start_a_second_round() {
    let mut w = world(REVIEWS, false);
    let part = started_review(&mut w, 3);
    let me = reviewer(&w, &part);
    let found = [(
        "challenge-scope",
        json!([finding_of("doubt", "smaller change would do", 1)]),
    )];
    let (delivered, _) = run_round(&mut w, &me, &found, &[]);
    let ids = ids_of(&delivered["findings"]);
    assert_eq!(ids.len(), 1, "a doubt meets the default floor: {delivered}");
    let after = fix_all(&mut w, &me, &ids, 1);
    assert!(
        after["next"].as_str().unwrap().contains("review_finish"),
        "{after}"
    );
    let review = w.store.reviews_of(w.task.id).unwrap().pop().unwrap();
    assert_eq!(
        (review.round, review.state),
        (1, ReviewState::Rechecking),
        "level 3 allows two rounds, but only an important fix earns the second"
    );
    let (is_error, text) = w.tool(me, "review_finish", &json!({}));
    assert!(!is_error && text.contains("1 fixed"), "{text}");
}
