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
use headwork::ops::rules::{CLOSING_HEADING, RuleFiles};
use headwork::ops::spawn::{SpawnRequest, spawn};
use headwork_adapters::git::{FakeGit, PrInfo, PrState};
use headwork_adapters::host::fake::FakeHost;
use headwork_core::{
    Actor, Config, Model, PermissionMode, ProjectName, Role, SessionState, Slug, TaskRequest,
    TaskState, Trigger,
};
use headwork_store::{EventFilter, EventRecord, NewSession, Store, Task};
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
        .with_claude_json(dir.path().join(".claude.json"));
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
            effort: None,
            in_main: false,
            keep,
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
            "inbox",
            "spawn",
            "drop",
            "close",
            "queue",
            "clean",
            "reap",
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
                "pr": { "type": "string", "description": "A GitHub pull request URL, for status review." },
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
    let mut w = world("", false);
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
    let mut w = world("", false);
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
fn enforce_rules_block_only_the_listed_rule_in_shadow_mode() {
    let mut w = world("enforce_rules = [\"stop-unfinished\"]", false);
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
