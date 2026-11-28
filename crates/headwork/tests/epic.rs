//! The epic explorer (`docs/backlog.md`, section 12): a spin-up of an epic spawns its explorer, and the `plan_*` tools only
//! an epic explorer sees, called through the MCP server against a temporary home, the fake host and an in-process store.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;
use std::process::Command;

use headwork::home::Home;
use headwork::ops::Env;
use headwork::ops::mcp::{Identity, Server};
use headwork::ops::rules::{Card, EPIC_EXPLORER_HEADING, EXPLORER_REPORT_HEADING, RuleFiles};
use headwork_adapters::git::FakeGit;
use headwork_adapters::host::fake::FakeHost;
use headwork_core::backlog::{BacklogStatus, BlockReason, ItemStatus};
use headwork_core::lifecycle::{SessionFacts, Trigger};
use headwork_core::{Actor, CloseMode, Config, Model, Role, TaskShape, TaskState};
use headwork_store::{NewSession, Store, Task};
use serde_json::{Value, json};

const EXE: &str = "S:/bin/headwork.exe";

struct World {
    dir: tempfile::TempDir,
    home: Home,
    config: Config,
    store: Store,
    host: FakeHost,
    prs: FakeGit,
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

fn world_with(writable: bool) -> World {
    let dir = tempfile::tempdir().unwrap();
    let dir_text = dir.path().display().to_string().replace('\\', "/");
    let main = dir.path().join("demo");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "master"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let main_text = main.display().to_string().replace('\\', "/");
    let hooks = if writable {
        format!("[hooks]\nbrief_writable = [\"{dir_text}/notes\", \"{dir_text}/other\"]\n\n")
    } else {
        String::new()
    };
    let text = format!(
        "[machine]\nhost = \"wmux\"\nmax_workers = 8\n\n{hooks}[project.demo]\nmain = \"{main_text}\"\n\
         worktree_new = \"builtin\"\nworktree_remove = \"builtin\"\nworktree_path = \"{{main}}-{{slug}}\"\n\
         deliver = \"pr\"\nmerge = \"deliver\"\nallowed_repos = [\"acme/demo\"]\n\n\
         [project.beta]\nmain = \"{main_text}-beta\"\nworktree_new = \"builtin\"\nworktree_remove = \"builtin\"\n\
         worktree_path = \"{{main}}-{{slug}}\"\ndeliver = \"pr\"\nmerge = \"deliver\"\nallowed_repos = [\"acme/beta\"]\n"
    );
    let config = Config::parse(&text, "test").unwrap();
    let home = Home::at(Some(dir.path().join("home")))
        .unwrap()
        .with_claude_json(dir.path().join(".claude.json"))
        .with_claude_dir(dir.path().join(".claude"));
    let mut store = home.open_store().unwrap();
    store.sync_projects(&config).unwrap();
    World {
        dir,
        home,
        config,
        store,
        host: FakeHost::new(),
        prs: FakeGit::new(),
    }
}

fn world() -> World {
    world_with(true)
}

impl World {
    fn notes(&self) -> String {
        self.dir
            .path()
            .join("notes")
            .display()
            .to_string()
            .replace('\\', "/")
    }

    fn call(
        &mut self,
        identity: Identity,
        actor: Actor,
        name: &str,
        arguments: &Value,
    ) -> (bool, String) {
        let env = Env {
            home: &self.home,
            config: &self.config,
            host: &self.host,
            exe: Path::new(EXE),
            actor,
        };
        let message = json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        let reply = Server::new(&mut self.store, env, &self.prs, identity)
            .handle(&message)
            .unwrap();
        let result = reply["result"].clone();
        let text = result["content"][0]["text"].as_str().unwrap().to_owned();
        (result["isError"].as_bool().unwrap(), text)
    }

    fn tool(&mut self, name: &str, arguments: &Value) -> (bool, String) {
        let identity = Identity {
            session: Some("root-session".to_owned()),
            role: Some(Role::Orchestrator),
        };
        self.call(identity, Actor::Orchestrator, name, arguments)
    }

    fn ok(&mut self, name: &str, arguments: &Value) -> Value {
        let (is_error, text) = self.tool(name, arguments);
        assert!(!is_error, "{name}: {text}");
        serde_json::from_str(&text).unwrap()
    }

    fn last_item(&self) -> i64 {
        self.store
            .backlog_items(&headwork_store::BacklogFilter::default())
            .unwrap()
            .last()
            .unwrap()
            .id
    }

    fn add_epic(&mut self, text: &str) -> i64 {
        self.ok(
            "backlog_add",
            &json!({ "project": "demo", "text": text, "epic": true, "section": "Plans" }),
        );
        self.last_item()
    }

    fn worker_identity(&mut self, task: &str) -> Identity {
        let id = self.store.find_task(task).unwrap().unwrap().id;
        let session = format!("session-{task}");
        if self.store.session(&session).unwrap().is_none() {
            self.store
                .record_session(
                    &NewSession {
                        id: session.clone(),
                        task_id: Some(id),
                        role: Role::Worker,
                        rc_name: task.to_owned(),
                        host: "wmux".to_owned(),
                        host_workspace: None,
                        host_surface: None,
                    },
                    Actor::Hook,
                )
                .unwrap();
        }
        if self.store.task(id).unwrap().state == TaskState::Spawning {
            self.store
                .transition(
                    id,
                    &Trigger::SessionStarted(SessionFacts {
                        session_matches: true,
                        launcher_alive: true,
                    }),
                    Actor::Hook,
                )
                .unwrap();
        }
        Identity {
            session: Some(session),
            role: Some(Role::Worker),
        }
    }

    fn worker(&mut self, task: &str, name: &str, arguments: &Value) -> (bool, String) {
        let identity = self.worker_identity(task);
        self.call(identity, Actor::Worker, name, arguments)
    }

    fn worker_ok(&mut self, task: &str, name: &str, arguments: &Value) -> Value {
        let (is_error, text) = self.worker(task, name, arguments);
        assert!(!is_error, "{name}: {text}");
        serde_json::from_str(&text).unwrap()
    }

    fn tool_names(&mut self, task: &str) -> Vec<String> {
        let identity = self.worker_identity(task);
        let env = Env {
            home: &self.home,
            config: &self.config,
            host: &self.host,
            exe: Path::new(EXE),
            actor: Actor::Worker,
        };
        let message = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
        let reply = Server::new(&mut self.store, env, &self.prs, identity)
            .handle(&message)
            .unwrap();
        reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect()
    }

    fn status(&self, id: i64) -> BacklogStatus {
        self.store.backlog_item(id).unwrap().draft.status
    }

    fn explorer_of(&self, epic: i64) -> Task {
        let id = self.store.backlog_item(epic).unwrap().task.unwrap().id;
        self.store.task(headwork_store::TaskId(id)).unwrap()
    }

    fn spin_up(&mut self, text: &str) -> (i64, String) {
        let epic = self.add_epic(text);
        let started = self.ok("backlog_start", &json!({ "id": epic }));
        let task = started["spawned"]["task"].as_str().unwrap().to_owned();
        (epic, task)
    }
}

const PLAN: &[&str] = &[
    "plan_list",
    "plan_add",
    "plan_edit",
    "plan_depend",
    "plan_undepend",
    "plan_cancel",
    "plan_finish",
];

#[test]
fn spinning_up_an_epic_spawns_its_explorer_linked_to_it_with_the_plan_file_on_its_writes_line() {
    let mut w = world();
    let epic = w.add_epic("Backlog workflow v2");
    w.ok(
        "backlog_edit",
        &json!({ "id": epic, "append": "keep the store pure" }),
    );
    let started = w.ok("backlog_start", &json!({ "id": epic }));
    let name = format!("demo-epic-{epic}-backlog-workflow-v2");
    let plan = format!("{}/epics/{epic}-backlog-workflow-v2.md", w.notes());
    assert_eq!(started["spawned"]["task"], json!(name), "{started}");
    assert_eq!(started["spawned"]["epic"], json!(epic));
    assert_eq!(started["spawned"]["plan"], json!(plan));
    assert_eq!(started["spawned"]["unlinked"], Value::Null);
    let task = w.store.find_task(&name).unwrap().unwrap();
    assert_eq!(task.kind, "explore");
    assert_eq!(task.model, Model::Opus, "the kind's default model");
    assert_eq!(task.close, CloseMode::Silent, "the kind's default close");
    let item = w.store.backlog_item(epic).unwrap();
    assert_eq!(item.draft.status, BacklogStatus::InProgress);
    assert_eq!(item.task.map(|task| task.name), Some(name));
    let brief = std::fs::read_to_string(&task.brief_path).unwrap();
    assert!(
        brief.starts_with(&format!("Goal: Plan epic #{epic}: Backlog workflow v2")),
        "{brief}"
    );
    assert!(brief.contains(&format!("Writes: {plan}")), "{brief}");
    assert!(brief.contains("- keep the store pure"), "{brief}");
    assert!(brief.contains("Project: demo"), "{brief}");
    assert_eq!(started["spawned"]["epic_in_progress"], json!(true));
    assert_eq!(task.epic, Some(epic), "the role is recorded on the task");
    assert_eq!(headwork::ops::epic::epic_of(&task), Some(epic));
}

#[test]
fn the_cli_start_of_an_epic_spawns_its_explorer_as_the_user_and_a_plain_item_only_moves() {
    let mut w = world();
    let epic = w.add_epic("Epic by hand");
    w.ok(
        "backlog_add",
        &json!({ "project": "demo", "text": "Plain" }),
    );
    let plain = w.last_item();
    let env = Env {
        home: &w.home,
        config: &w.config,
        host: &w.host,
        exe: Path::new(EXE),
        actor: Actor::User,
    };
    let change = headwork::ops::backlog::start(&mut w.store, &env, epic).unwrap();
    let spawned = change.spawned.as_ref().unwrap();
    assert_eq!(spawned.task, format!("demo-epic-{epic}-epic-by-hand"));
    assert!(
        change.text().contains("explorer demo-epic-"),
        "{}",
        change.text()
    );
    let plain_change = headwork::ops::backlog::start(&mut w.store, &env, plain).unwrap();
    assert!(plain_change.spawned.is_none());
    assert_eq!(w.status(plain), BacklogStatus::InProgress);
    assert!(w.store.backlog_item(plain).unwrap().task.is_none());
    let started = w
        .store
        .events(headwork_store::EventFilter {
            task: None,
            limit: 200,
        })
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "backlog-moved" && event.data["item"] == json!(epic))
        .unwrap();
    assert_eq!(started.data["actor"], json!("user"));
}

#[test]
fn a_spin_up_without_a_brief_writable_folder_is_refused_before_anything_moves() {
    let mut w = world_with(false);
    let epic = w.add_epic("Nowhere to write");
    let (is_error, text) = w.tool("backlog_start", &json!({ "id": epic }));
    assert!(
        is_error && text.contains("epic-no-plan-path") && text.contains("brief_writable"),
        "{text}"
    );
    assert_eq!(w.status(epic), BacklogStatus::Backlog);
    assert!(w.store.backlog_item(epic).unwrap().task.is_none());
    assert!(
        w.store
            .find_task(&format!("demo-epic-{epic}-nowhere-to-write"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn only_an_epic_explorer_sees_and_calls_the_plan_tools() {
    let mut w = world();
    let (_, explorer) = w.spin_up("Visible plan");
    let names = w.tool_names(&explorer);
    for tool in PLAN {
        assert!(names.contains(&(*tool).to_owned()), "{tool} in {names:?}");
    }
    let notes = w.notes();
    w.ok(
        "spawn",
        &json!({ "project": "demo", "slug": "look", "kind": "explore", "brief": format!("Look.\nWrites: {notes}/look.md") }),
    );
    w.ok(
        "spawn",
        &json!({ "project": "demo", "slug": "work", "brief": "# Goal: Work\n" }),
    );
    for task in ["demo-look", "demo-work"] {
        let names = w.tool_names(task);
        assert!(
            !names.iter().any(|name| name.starts_with("plan_")),
            "{task}: {names:?}"
        );
        let (is_error, text) = w.worker(task, "plan_list", &json!({}));
        assert!(is_error && text.contains("epic explorer"), "{task}: {text}");
    }
    let (is_error, text) = w.tool("plan_list", &json!({}));
    assert!(is_error && text.contains("worker"), "{text}");
}

#[test]
#[expect(clippy::too_many_lines, reason = "one plan walked through every tool")]
fn an_epic_explorer_plans_children_orders_them_and_finishes_the_epic_when_none_is_open() {
    let mut w = world();
    let (epic, explorer) = w.spin_up("Full plan");
    w.ok(
        "backlog_add",
        &json!({ "project": "demo", "text": "Outside item" }),
    );
    let outside = w.last_item();
    let first = w.worker_ok(
        &explorer,
        "plan_add",
        &json!({ "text": "Store piece", "brief": "Goal: add the store piece\n\nDone when tests pass.", "model": "sonnet", "review": 2 }),
    );
    assert!(
        first["items"][0]
            .as_str()
            .unwrap()
            .contains(&format!("in epic #{epic}")),
        "{first}"
    );
    let first = w.last_item();
    let child = w.store.backlog_item(first).unwrap();
    assert_eq!(
        (
            child.parent,
            child.draft.project.as_str(),
            child.draft.section.as_deref()
        ),
        (Some(epic), "demo", Some("Plans")),
        "project and section come from the epic"
    );
    let plan = w.store.backlog_plan(first).unwrap();
    assert_eq!(
        (
            plan.task_kind.as_deref(),
            plan.model.as_deref(),
            plan.review
        ),
        (Some("code"), Some("sonnet"), Some(2))
    );
    w.worker_ok(
        &explorer,
        "plan_add",
        &json!({ "text": "Core piece", "brief": "Goal: core", "kind": "explore", "after": [first, outside] }),
    );
    let second = w.last_item();
    assert_eq!(
        w.status(second).kind(),
        ItemStatus::Blocked,
        "a child with an open dependency starts blocked"
    );
    assert_eq!(
        w.status(second).block().map(|block| block.reason),
        Some(BlockReason::Depends)
    );
    let listed = w.worker_ok(&explorer, "plan_list", &json!({}));
    assert_eq!(listed["epic"]["id"], json!(epic));
    assert_eq!(listed["epic"]["status"], json!("in_progress"));
    let children = listed["children"].as_array().unwrap();
    assert_eq!(children.len(), 2, "{listed}");
    assert_eq!(children[0]["has_brief"], json!(true));
    assert_eq!(children[0]["review"], json!(2));
    assert_eq!(children[1]["task_kind"], json!("explore"));
    assert_eq!(
        children[1]["deps"],
        json!([outside, first]),
        "ascending ids"
    );
    assert_eq!(children[1]["blocked_reason"], json!("depends"));
    w.worker_ok(
        &explorer,
        "plan_edit",
        &json!({ "id": second, "text": "Core piece, smaller", "model": "haiku", "review": 1 }),
    );
    let edited = w.store.backlog_plan(second).unwrap();
    assert_eq!(
        (
            edited.model.as_deref(),
            edited.review,
            edited.brief.as_deref()
        ),
        (Some("haiku"), Some(1), Some("Goal: core"))
    );
    assert_eq!(
        w.store.backlog_item(second).unwrap().draft.text,
        "Core piece, smaller"
    );
    w.worker_ok(
        &explorer,
        "plan_undepend",
        &json!({ "id": second, "on": outside }),
    );
    assert_eq!(w.store.backlog_item(second).unwrap().deps, [first]);
    w.worker_ok(
        &explorer,
        "plan_depend",
        &json!({ "id": first, "on": [outside] }),
    );
    assert_eq!(w.store.backlog_item(first).unwrap().deps, [outside]);
    let (is_error, text) = w.worker(&explorer, "plan_finish", &json!({}));
    assert!(
        is_error
            && text.contains(&format!("#{first} (blocked)"))
            && text.contains(&format!("#{second} (blocked)")),
        "{text}"
    );
    w.worker_ok(
        &explorer,
        "plan_cancel",
        &json!({ "id": second, "reason": "folded into the store piece" }),
    );
    w.worker_ok(&explorer, "plan_cancel", &json!({ "id": first }));
    assert_eq!(w.status(first), BacklogStatus::Cancelled);
    let done = w.worker_ok(&explorer, "plan_finish", &json!({ "note": "nothing left" }));
    assert!(
        done["items"][0].as_str().unwrap().contains("done"),
        "{done}"
    );
    let item = w.store.backlog_item(epic).unwrap();
    assert_eq!((item.draft.status, item.task), (BacklogStatus::Done, None));
    let task = w.store.find_task(&explorer).unwrap().unwrap();
    let logged = w
        .store
        .events(headwork_store::EventFilter {
            task: Some(task.id),
            limit: 500,
        })
        .unwrap();
    for kind in [
        "backlog-added",
        "backlog-moved",
        "backlog-edited",
        "backlog-depended",
        "backlog-undepended",
    ] {
        assert!(
            logged.iter().any(|event| event.kind == kind),
            "{kind} is logged with the explorer's task"
        );
    }
}

#[test]
fn the_plan_tools_refuse_items_outside_the_epic_started_children_and_bad_fields() {
    let mut w = world();
    let (_, explorer) = w.spin_up("Guarded plan");
    let (other, _) = w.spin_up("Other plan");
    let (is_error, text) = w.worker(&explorer, "plan_add", &json!({ "text": "No brief" }));
    assert!(is_error && text.contains("brief"), "{text}");
    for (field, value) in [
        ("review", json!(5)),
        ("model", json!("gpt")),
        ("kind", json!("Not A Kind")),
    ] {
        let mut args = json!({ "text": "Bad", "brief": "Goal: x" });
        args[field] = value;
        let (is_error, text) = w.worker(&explorer, "plan_add", &args);
        assert!(is_error, "{field}: {text}");
    }
    let (is_error, text) = w.worker(
        &explorer,
        "plan_add",
        &json!({ "text": "Empty", "brief": "  " }),
    );
    assert!(is_error && text.contains("empty"), "{text}");
    let other_explorer = w.explorer_of(other).name.to_string();
    w.worker_ok(
        &other_explorer,
        "plan_add",
        &json!({ "text": "Theirs", "brief": "Goal: theirs" }),
    );
    let theirs = w.last_item();
    for (tool, args) in [
        ("plan_edit", json!({ "id": theirs, "text": "Mine now" })),
        ("plan_cancel", json!({ "id": theirs })),
        ("plan_depend", json!({ "id": theirs, "on": [other] })),
        ("plan_undepend", json!({ "id": theirs, "on": other })),
    ] {
        let (is_error, text) = w.worker(&explorer, tool, &args);
        assert!(
            is_error && text.contains("not a child of epic"),
            "{tool}: {text}"
        );
    }
    w.worker_ok(
        &explorer,
        "plan_add",
        &json!({ "text": "Started child", "brief": "# Goal: started\n" }),
    );
    let started = w.last_item();
    w.ok(
        "spawn",
        &json!({ "project": "demo", "slug": "started", "brief": "# Goal: started\n", "backlog": started }),
    );
    assert_eq!(w.status(started), BacklogStatus::InProgress);
    let (is_error, text) = w.worker(
        &explorer,
        "plan_edit",
        &json!({ "id": started, "brief": "Goal: other" }),
    );
    assert!(is_error && text.contains("in_progress"), "{text}");
    let (is_error, text) = w.worker(&explorer, "plan_cancel", &json!({ "id": started }));
    assert!(is_error && text.contains("may not move"), "{text}");
    assert_eq!(
        w.status(started),
        BacklogStatus::InProgress,
        "a started child is never cancelled by the explorer"
    );
    let (is_error, text) = w.worker(&explorer, "plan_finish", &json!({}));
    assert!(
        is_error && text.contains(&format!("#{started} (in_progress)")),
        "{text}"
    );
}

#[test]
fn the_end_of_the_explorer_s_task_leaves_its_epic_in_progress_for_a_later_explorer() {
    let mut w = world();
    let (epic, explorer) = w.spin_up("Long epic");
    let task = w.store.find_task(&explorer).unwrap().unwrap();
    let blocked = w
        .store
        .record_partial_outcome(&task, "session", Some("planned"), Actor::Worker)
        .unwrap();
    assert!(blocked.is_empty(), "{blocked:?}");
    assert_eq!(
        w.status(epic),
        BacklogStatus::InProgress,
        "an epic is never blocked partial by its explorer"
    );
    w.store.drop_task(task.id, Actor::Orchestrator).unwrap();
    assert_eq!(
        w.status(epic).block().map(|block| block.reason),
        Some(BlockReason::TaskLost),
        "a dropped explorer blocks its epic as task_lost"
    );
}

#[test]
fn an_epic_explorer_s_rules_give_the_epic_section_instead_of_the_report_one() {
    let mut w = world();
    let (_, explorer) = w.spin_up("Ruled epic");
    let task = w.store.find_task(&explorer).unwrap().unwrap();
    let files = RuleFiles::load(&w.home, &w.config, &[], "orchestrator").unwrap();
    let dir = w.dir.path().join("demo");
    let render = |epic| {
        files.render(
            Role::Worker,
            &w.config,
            Path::new(EXE),
            Some(&Card {
                task: &task,
                dir: &dir,
                parent: None,
                epic,
            }),
        )
    };
    let epic_rules = render(true);
    assert!(epic_rules.contains(EPIC_EXPLORER_HEADING), "{epic_rules}");
    assert!(
        !epic_rules.contains(EXPLORER_REPORT_HEADING),
        "{epic_rules}"
    );
    assert!(epic_rules.contains("plan_finish"), "{epic_rules}");
    assert!(
        epic_rules.contains("send `orchestrator` one line"),
        "{epic_rules}"
    );
    let plain_rules = render(false);
    assert!(
        plain_rules.contains(EXPLORER_REPORT_HEADING),
        "{plain_rules}"
    );
    assert!(
        !plain_rules.contains(EPIC_EXPLORER_HEADING),
        "{plain_rules}"
    );
}

#[test]
fn a_respawn_after_a_lost_explorer_gets_a_task_name_of_its_own() {
    let mut w = world();
    let (epic, first) = w.spin_up("Lost once");
    let task = w.store.find_task(&first).unwrap().unwrap();
    w.store.drop_task(task.id, Actor::Orchestrator).unwrap();
    assert_eq!(
        w.status(epic).block().map(|block| block.reason),
        Some(BlockReason::TaskLost)
    );
    let again = w.ok("backlog_start", &json!({ "id": epic }));
    let second = again["spawned"]["task"].as_str().unwrap().to_owned();
    assert_eq!(second, format!("{first}-2"), "{again}");
    assert_eq!(w.status(epic), BacklogStatus::InProgress);
    assert_eq!(w.explorer_of(epic).name.to_string(), second);
    assert_eq!(w.explorer_of(epic).epic, Some(epic));
}

#[test]
fn the_role_survives_the_finish_and_the_changing_tools_need_the_epic_in_progress() {
    let mut w = world();
    let (epic, explorer) = w.spin_up("Short epic");
    w.worker_ok(
        &explorer,
        "plan_finish",
        &json!({ "note": "nothing to do" }),
    );
    assert_eq!(w.status(epic), BacklogStatus::Done);
    let names = w.tool_names(&explorer);
    assert!(names.contains(&"plan_list".to_owned()), "{names:?}");
    let listed = w.worker_ok(&explorer, "plan_list", &json!({}));
    assert_eq!(listed["epic"]["status"], json!("done"));
    for (tool, args) in [
        ("plan_add", json!({ "text": "Late", "brief": "Goal: late" })),
        ("plan_edit", json!({ "id": epic, "text": "x" })),
        ("plan_depend", json!({ "id": epic, "on": [epic] })),
        ("plan_undepend", json!({ "id": epic, "on": epic })),
        ("plan_cancel", json!({ "id": epic })),
        ("plan_finish", json!({})),
    ] {
        let (is_error, text) = w.worker(&explorer, tool, &args);
        assert!(
            is_error && text.contains("not in progress"),
            "{tool}: {text}"
        );
    }
}

#[test]
fn plan_edit_refuses_done_cancelled_and_blocked_children_a_live_task_works_on() {
    let mut w = world();
    let (_, explorer) = w.spin_up("Edit guard");
    let child = |w: &mut World, text: &str| {
        w.worker_ok(
            &explorer,
            "plan_add",
            &json!({ "text": text, "brief": format!("# Goal: {text}\n") }),
        );
        w.last_item()
    };
    let done = child(&mut w, "Done child");
    let cancelled = child(&mut w, "Cancelled child");
    let held = child(&mut w, "Held child");
    w.ok("backlog_done", &json!({ "id": done, "note": "by hand" }));
    w.worker_ok(&explorer, "plan_cancel", &json!({ "id": cancelled }));
    w.ok(
        "spawn",
        &json!({ "project": "demo", "slug": "held", "brief": "# Goal: held\n", "backlog": held }),
    );
    w.worker_ok(
        "demo-held",
        "item_block",
        &json!({ "reason": "decision", "note": "which way" }),
    );
    assert_eq!(w.status(held).kind(), ItemStatus::Blocked);
    for (id, word) in [(done, "done"), (cancelled, "cancelled")] {
        let (is_error, text) = w.worker(&explorer, "plan_edit", &json!({ "id": id, "text": "x" }));
        assert!(is_error && text.contains(word), "{word}: {text}");
    }
    let (is_error, text) = w.worker(&explorer, "plan_edit", &json!({ "id": held, "text": "x" }));
    assert!(
        is_error && text.contains("live task") && text.contains("demo-held"),
        "{text}"
    );
    assert_eq!(w.store.backlog_item(held).unwrap().draft.text, "Held child");
}

#[test]
fn plan_add_refuses_bad_dependencies_and_takes_project_section_and_owner_overrides() {
    let mut w = world();
    let (epic, explorer) = w.spin_up("Add guard");
    w.ok("backlog_add", &json!({ "project": "demo", "text": "Gone" }));
    let gone = w.last_item();
    w.ok(
        "backlog_cancel",
        &json!({ "id": gone, "reason": "the user said no" }),
    );
    let count = |w: &World| {
        w.store
            .backlog_items(&headwork_store::BacklogFilter {
                epic: Some(epic),
                ..headwork_store::BacklogFilter::default()
            })
            .unwrap()
            .len()
    };
    for (after, why) in [
        (json!([gone]), "cancelled"),
        (json!([9999]), "9999"),
        (json!([epic]), "its own epic"),
    ] {
        let (is_error, text) = w.worker(
            &explorer,
            "plan_add",
            &json!({ "text": "Waits", "brief": "Goal: w", "after": after }),
        );
        assert!(is_error && text.contains(why), "{why}: {text}");
        assert_eq!(count(&w), 0, "nothing added for {why}");
    }
    let (is_error, text) = w.worker(
        &explorer,
        "plan_add",
        &json!({ "text": "Elsewhere", "brief": "Goal: e", "project": "alpha" }),
    );
    assert!(
        is_error && text.contains("not a project headwork runs"),
        "{text}"
    );
    w.worker_ok(
        &explorer,
        "plan_add",
        &json!({ "text": "First", "brief": "Goal: first", "project": "beta", "section": "Custom", "owner": "user" }),
    );
    let first = w.last_item();
    let item = w.store.backlog_item(first).unwrap();
    assert_eq!(
        (
            item.draft.project.as_str(),
            item.draft.section.as_deref(),
            item.draft.owner.as_str()
        ),
        ("beta", Some("Custom"), "user")
    );
    w.worker_ok(
        &explorer,
        "plan_add",
        &json!({ "text": "Second", "brief": "Goal: second", "after": [first] }),
    );
    let second = w.last_item();
    let (is_error, text) = w.worker(
        &explorer,
        "plan_depend",
        &json!({ "id": first, "on": [second] }),
    );
    assert!(is_error && text.contains("cycle"), "{text}");
    let (is_error, text) = w.worker(
        &explorer,
        "plan_depend",
        &json!({ "id": first, "on": [epic] }),
    );
    assert!(is_error && text.contains("its own epic"), "{text}");
    assert_eq!(
        w.store.backlog_item(first).unwrap().deps,
        Vec::<i64>::new(),
        "nothing added"
    );
}

#[test]
fn an_explore_part_or_an_explorer_only_linked_to_a_child_gets_no_plan_tools() {
    let mut w = world();
    let (epic, explorer) = w.spin_up("Scoped epic");
    w.worker_ok(
        &explorer,
        "plan_add",
        &json!({ "text": "Child", "brief": "# Goal: child\n" }),
    );
    let child = w.last_item();
    let notes = w.notes();
    w.ok(
        "spawn",
        &json!({ "project": "demo", "slug": "replan", "kind": "explore", "backlog": child,
                 "brief": format!("Look again.\nWrites: {notes}/replan.md") }),
    );
    let linked = w.store.find_task("demo-replan").unwrap().unwrap();
    assert_eq!(linked.epic, None);
    assert_eq!(headwork::ops::epic::epic_of(&linked), None);
    let names = w.tool_names("demo-replan");
    assert!(
        !names.iter().any(|name| name.starts_with("plan_")),
        "{names:?}"
    );
    let lead = w
        .store
        .create_task(
            &w.config,
            &new_task("lead", "task", TaskShape::Lead),
            Actor::Orchestrator,
        )
        .unwrap();
    let part = w
        .store
        .create_task(
            &w.config,
            &new_task("lead-part", "explore", TaskShape::Part { parent: lead.id }),
            Actor::Orchestrator,
        )
        .unwrap();
    w.store
        .set_task_epic(part.id, epic, Actor::Orchestrator)
        .unwrap();
    let part = w.store.task(part.id).unwrap();
    assert_eq!(
        headwork::ops::epic::epic_of(&part),
        None,
        "a part is no explorer"
    );
    let names = w.tool_names("demo-lead-part");
    assert!(
        !names.iter().any(|name| name.starts_with("plan_")),
        "{names:?}"
    );
    let files = RuleFiles::load(&w.home, &w.config, &[], "orchestrator").unwrap();
    let dir = w.dir.path().join("demo");
    let rules = files.render(
        Role::Worker,
        &w.config,
        Path::new(EXE),
        Some(&Card {
            task: &linked,
            dir: &dir,
            parent: None,
            epic: headwork::ops::epic::epic_of(&linked).is_some(),
        }),
    );
    assert!(rules.contains(EXPLORER_REPORT_HEADING), "{rules}");
    assert!(!rules.contains(EPIC_EXPLORER_HEADING), "{rules}");
}

fn new_task(
    slug: &str,
    kind: &str,
    shape: TaskShape<headwork_store::TaskId>,
) -> headwork_store::NewTask {
    headwork_store::NewTask {
        request: headwork_core::TaskRequest {
            project: headwork_core::ProjectName::new("demo").unwrap(),
            slug: headwork_core::Slug::new(slug).unwrap(),
            kind: kind.to_owned(),
            model: Some(Model::Sonnet),
            effort: None,
            in_main: false,
            keep: false,
            close: CloseMode::Silent,
            review: None,
            decision: None,
        },
        brief_path: format!("briefs/demo-{slug}.md"),
        depends_on: None,
        permission_mode: headwork_core::PermissionMode::Auto,
        shape,
    }
}
