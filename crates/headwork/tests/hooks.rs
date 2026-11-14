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
use headwork::ops::rules::{CLOSING_HEADING, RuleFiles};
use headwork::ops::spawn::{SpawnRequest, spawn};
use headwork_adapters::host::fake::FakeHost;
use headwork_core::{
    Actor, Config, Model, PermissionMode, ProjectName, Role, SessionState, Slug, TaskRequest,
    TaskState,
};
use headwork_store::{EventFilter, EventRecord, Store, Task};
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
    task: Task,
    session: String,
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
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("demo");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "master"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let main_text = main.display().to_string().replace('\\', "/");
    let text = format!(
        "[machine]\nhost = \"wmux\"\n\n[hooks]\nplugin_dir = \"S:/plugin\"\n{hooks}\n\n[project.demo]\nmain = \"{main_text}\"\n\
         worktree_new = \"builtin\"\nworktree_remove = \"builtin\"\nworktree_path = \"{{main}}-{{slug}}\"\n\
         deliver = \"scripts/ship.sh\"\nmerge = \"deliver\"\nallowed_repos = [\"Ivan091/demo\"]\n\
         user_only = [\"scripts/publish.sh\"]\n"
    );
    let config = Config::parse(&text, "test").unwrap();
    let home = Home::at(Some(dir.path().join("home"))).unwrap();
    let mut store = home.open_store().unwrap();
    store.sync_projects(&config).unwrap();
    let brief = dir.path().join("brief.md");
    std::fs::write(&brief, "# Task\n\nDo the thing.\n").unwrap();
    let host = FakeHost::new();
    let request = SpawnRequest {
        request: TaskRequest {
            project: ProjectName::new("demo").unwrap(),
            slug: Slug::new("sand").unwrap(),
            kind: "task".to_owned(),
            model: Some(Model::Haiku),
            in_main: false,
            keep,
        },
        brief: &brief,
        depends_on: None,
        permission_mode: PermissionMode::Auto,
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
        task,
        session,
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
    assert!(text.contains("child session `demo-sand`"), "{text}");
    assert!(
        text.contains(&format!("`\"{EXE}\" close demo-sand`")),
        "{text}"
    );
    assert!(
        text.contains("`scripts/ship.sh` from your folder"),
        "{text}"
    );
    assert!(text.contains("in shadow mode"), "{text}");
    assert_eq!(w.state(), TaskState::Working, "after");
    let session = w.store.hook_session(&w.session).unwrap().unwrap();
    assert_eq!(session.rules_seen, 1, "rules seen");
    assert_eq!(session.state, SessionState::Live, "live");
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
        "child-default-branch",
        "child-edit-outside",
        "user-only",
        "child-global-skill",
    ]
    .iter()
    .map(|rule| ("would-deny".to_owned(), (*rule).to_owned()))
    .collect();
    assert_eq!(w.denies(), rules, "every rule logged once, allows silent");
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
        reason.starts_with("headwork rule child-default-branch: "),
        "{reason}"
    );
    assert_eq!(
        w.denies(),
        [("deny".to_owned(), "child-default-branch".to_owned())],
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

#[test]
fn the_stop_gate_wants_an_ask_or_a_report() {
    let mut w = world("", false);
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
    let files = RuleFiles::load(&w.home, &w.config).unwrap();
    let text = files.render(Role::Root, &w.config, Path::new(EXE), None);
    assert!(text.contains(&format!("\"{EXE}\" spawn")), "{text}");
    assert!(text.len() < 10_000, "{}", text.len());
}
