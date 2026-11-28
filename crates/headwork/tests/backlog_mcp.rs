//! The orchestrator's backlog tools and the `backlog` argument of `spawn`, called through the MCP server against a temporary
//! home, the fake host and an in-process store.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;
use std::process::Command;

use headwork::home::Home;
use headwork::ops::Env;
use headwork::ops::mcp::{Identity, Server};
use headwork_adapters::git::FakeGit;
use headwork_adapters::host::fake::FakeHost;
use headwork_core::TaskState;
use headwork_core::backlog::{BacklogOwner, BacklogStatus, BlockReason, ItemStatus};
use headwork_core::lifecycle::{SessionFacts, Trigger};
use headwork_core::{Actor, Config, Role};
use headwork_store::NewSession;
use headwork_store::{BacklogFilter, Store};
use serde_json::{Value, json};

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

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("demo");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "master"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let main_text = main.display().to_string().replace('\\', "/");
    let text = format!(
        "[machine]\nhost = \"wmux\"\nmax_workers = 4\n\n[project.demo]\nmain = \"{main_text}\"\n\
         worktree_new = \"builtin\"\nworktree_remove = \"builtin\"\nworktree_path = \"{{main}}-{{slug}}\"\n\
         deliver = \"pr\"\nmerge = \"deliver\"\nallowed_repos = [\"acme/demo\"]\n"
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

impl World {
    fn tool(&mut self, name: &str, arguments: &Value) -> (bool, String) {
        let env = Env {
            home: &self.home,
            config: &self.config,
            host: &self.host,
            exe: Path::new("S:/bin/headwork.exe"),
            actor: Actor::User,
        };
        let identity = Identity {
            session: Some("root-session".to_owned()),
            role: Some(Role::Orchestrator),
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

    fn ok(&mut self, name: &str, arguments: &Value) -> Value {
        let (is_error, text) = self.tool(name, arguments);
        assert!(!is_error, "{name}: {text}");
        serde_json::from_str(&text).unwrap()
    }

    fn listed(&mut self, arguments: &Value) -> Vec<String> {
        serde_json::from_value(self.ok("backlog_list", arguments)).unwrap()
    }

    fn add(&mut self, project: &str, text: &str) -> i64 {
        self.ok("backlog_add", &json!({ "project": project, "text": text }));
        self.store
            .backlog_items(&BacklogFilter::default())
            .unwrap()
            .last()
            .unwrap()
            .id
    }
}

#[test]
fn the_list_shows_open_items_by_default_and_filters_by_project_owner_and_state() {
    let mut w = world();
    let first = w.add("alpha", "First item");
    w.add("beta", "For you: Second item");
    let third = w.add("alpha", "Third item");
    w.ok("backlog_done", &json!({ "id": first, "note": "finished" }));
    w.ok(
        "backlog_done",
        &json!({ "id": third, "drop": true, "note": "the user said no" }),
    );
    let open = w.listed(&json!({}));
    assert_eq!(open.len(), 1, "{open:?}");
    assert!(open[0].contains("Second item"), "{open:?}");
    assert_eq!(w.listed(&json!({ "state": "open" })), open);
    assert_eq!(w.listed(&json!({ "state": "all" })).len(), 3);
    let done = w.listed(&json!({ "state": "done" }));
    assert!(
        done.len() == 1 && done[0].contains("First item"),
        "{done:?}"
    );
    let dropped = w.listed(&json!({ "state": "dropped" }));
    assert!(
        dropped.len() == 1 && dropped[0].contains("Third item"),
        "{dropped:?}"
    );
    let alpha = w.listed(&json!({ "state": "all", "project": "alpha" }));
    assert_eq!(alpha.len(), 2, "{alpha:?}");
    let user = w.listed(&json!({ "state": "all", "owner": "user" }));
    assert!(
        user.len() == 1 && user[0].contains("Second item"),
        "{user:?}"
    );
    let (is_error, _) = w.tool("backlog_list", &json!({ "state": "nonsense" }));
    assert!(is_error);
}

#[test]
fn an_added_item_goes_last_with_its_fields_and_says_what_the_export_did() {
    let mut w = world();
    let change = w.ok(
        "backlog_add",
        &json!({
            "project": "alpha",
            "text": "Plan it (added 2026-11-02)",
            "section": "Audit",
            "not_before": "2026-12-01",
            "due": "2026-12-24",
            "notes": ["- a note"],
        }),
    );
    assert_eq!(change["export"]["export"], "unconfigured");
    let item = w
        .store
        .backlog_items(&BacklogFilter::default())
        .unwrap()
        .remove(0);
    assert_eq!(item.draft.text, "Plan it");
    assert_eq!(item.draft.section.as_deref(), Some("Audit"));
    assert_eq!(item.draft.notes, ["a note"]);
    assert_eq!(
        item.draft.due.map(|date| date.to_string()).as_deref(),
        Some("2026-12-24")
    );
    assert_eq!(
        item.draft.added.map(|date| date.to_string()).as_deref(),
        Some("2026-11-02")
    );
    let (is_error, text) = w.tool("backlog_add", &json!({ "project": "Done", "text": "x" }));
    assert!(
        is_error && text.contains("Done, Cancelled or Dropped"),
        "{text}"
    );
}

#[test]
fn done_finishes_an_item_and_drop_cancels_it() {
    let mut w = world();
    let finished = w.add("alpha", "Finish me");
    let given_up = w.add("alpha", "Drop me");
    w.ok(
        "backlog_done",
        &json!({ "id": finished, "note": "all good" }),
    );
    w.ok(
        "backlog_done",
        &json!({ "id": given_up, "drop": true, "note": "not needed" }),
    );
    let finished = w.store.backlog_item(finished).unwrap().draft;
    assert_eq!(finished.status, BacklogStatus::Done);
    assert_eq!(finished.done_note.as_deref(), Some("all good"));
    let given_up = w.store.backlog_item(given_up).unwrap().draft;
    assert_eq!(given_up.status, BacklogStatus::Cancelled);
    assert_eq!(given_up.done_note.as_deref(), Some("not needed"));
    let (is_error, _) = w.tool("backlog_done", &json!({ "id": 999 }));
    assert!(is_error, "an unknown id is refused");
    let (is_error, _) = w.tool("backlog_done", &json!({ "id": "1" }));
    assert!(is_error, "an id must be an integer");
}

#[test]
fn an_edit_changes_what_it_names_and_none_clears_a_date_or_a_section() {
    let mut w = world();
    w.ok(
        "backlog_add",
        &json!({ "project": "alpha", "text": "Edit me", "due": "2026-12-24", "section": "Audit" }),
    );
    let id = w.store.backlog_items(&BacklogFilter::default()).unwrap()[0].id;
    w.ok(
        "backlog_edit",
        &json!({ "id": id, "text": "Edited", "owner": "user", "not_before": "2026-12-01", "append": "more" }),
    );
    let item = w.store.backlog_item(id).unwrap().draft;
    assert_eq!(item.text, "Edited");
    assert_eq!(item.project, "alpha", "untouched");
    assert_eq!(item.notes, ["more"]);
    assert_eq!(
        item.not_before.map(|date| date.to_string()).as_deref(),
        Some("2026-12-01")
    );
    w.ok(
        "backlog_edit",
        &json!({ "id": id, "due": "none", "section": "none", "not_before": "none" }),
    );
    let item = w.store.backlog_item(id).unwrap().draft;
    assert_eq!(
        (item.due, item.not_before, item.section),
        (None, None, None)
    );
    let (is_error, text) = w.tool("backlog_edit", &json!({ "id": id, "project": "# bad" }));
    assert!(
        is_error && text.contains("must not start with `#`"),
        "{text}"
    );
}

#[test]
fn spawn_takes_an_integer_item_id_and_refuses_a_string_or_an_unknown_id() {
    let mut w = world();
    let id = w.add("demo", "Do the work");
    let (is_error, text) = w.tool(
        "spawn",
        &json!({ "project": "demo", "slug": "text", "brief": "# Task\n", "backlog": id.to_string() }),
    );
    assert!(is_error && text.contains("an integer"), "{text}");
    let (is_error, text) = w.tool(
        "spawn",
        &json!({ "project": "demo", "slug": "nobody", "brief": "# Task\n", "backlog": 4242 }),
    );
    assert!(is_error && text.contains("no backlog item"), "{text}");
    assert!(w.store.find_task("demo-text").unwrap().is_none());
    assert!(w.store.find_task("demo-nobody").unwrap().is_none());
    let spawned = w.ok(
        "spawn",
        &json!({ "project": "demo", "slug": "linked", "brief": "# Task\n", "backlog": id }),
    );
    assert_eq!(spawned["task"]["name"], "demo-linked");
    assert_eq!(spawned["backlog_unlinked"], Value::Null);
    assert_eq!(
        spawned["todo_unlinked"],
        Value::Null,
        "the old key for one release"
    );
    assert_eq!(
        w.store.backlog_item(id).unwrap().task.map(|task| task.name),
        Some("demo-linked".to_owned())
    );
    assert!(w.dir.path().exists());
}

#[test]
fn the_old_todo_tool_names_and_spawn_argument_still_work() {
    let mut w = world();
    let added = w.add("demo", "Do the old way");
    let listed: Vec<String> = serde_json::from_value(w.ok("todo_list", &json!({}))).unwrap();
    assert!(
        listed.len() == 1 && listed[0].contains("Do the old way"),
        "{listed:?}"
    );
    w.ok(
        "todo_add",
        &json!({ "project": "demo", "text": "Added by the old name" }),
    );
    w.ok(
        "todo_edit",
        &json!({ "id": added, "text": "Edited by the old name" }),
    );
    let spawned = w.ok(
        "spawn",
        &json!({ "project": "demo", "slug": "old", "brief": "# Task\n", "backlog": null, "todo": added }),
    );
    assert_eq!(spawned["task"]["name"], "demo-old");
    assert_eq!(
        w.store
            .backlog_item(added)
            .unwrap()
            .task
            .map(|task| task.name),
        Some("demo-old".to_owned())
    );
    let (is_error, text) = w.tool("todo_done", &json!({ "id": added }));
    assert!(
        is_error && text.contains("only headwork finishes it"),
        "the old name reaches the guard: {text}"
    );
    let items = w.store.backlog_items(&BacklogFilter::default()).unwrap();
    assert_eq!(items.len(), 2, "{items:?}");
}

#[test]
fn the_status_tools_start_block_unblock_cancel_and_reopen() {
    let mut w = world();
    let id = w.add("demo", "Spin me up");
    let elsewhere = w.add("alpha", "No project here");
    let (is_error, text) = w.tool("backlog_start", &json!({ "id": elsewhere }));
    assert!(is_error && text.contains("not one headwork runs"), "{text}");
    w.ok("backlog_start", &json!({ "id": id }));
    assert_eq!(
        w.store.backlog_item(id).unwrap().draft.status,
        BacklogStatus::InProgress
    );
    let progress = w.listed(&json!({ "status": "in_progress" }));
    assert!(
        progress.len() == 1 && progress[0].contains("in_progress"),
        "{progress:?}"
    );
    w.ok(
        "backlog_block",
        &json!({ "id": id, "reason": "decision", "note": "which engine" }),
    );
    let blocked = w.listed(&json!({ "status": "blocked" }));
    assert!(
        blocked.len() == 1 && blocked[0].contains("blocked (decision: which engine)"),
        "{blocked:?}"
    );
    let (is_error, _) = w.tool(
        "backlog_block",
        &json!({ "id": elsewhere, "reason": "task_lost", "note": "x" }),
    );
    assert!(
        is_error,
        "the orchestrator blocks only for a decision or an event"
    );
    w.ok("backlog_unblock", &json!({ "id": id }));
    assert_eq!(
        w.store.backlog_item(id).unwrap().draft.status,
        BacklogStatus::InProgress
    );
    let (is_error, text) = w.tool("backlog_cancel", &json!({ "id": id }));
    assert!(is_error && text.contains("reason"), "{text}");
    let (is_error, text) = w.tool("backlog_cancel", &json!({ "id": id, "reason": "  " }));
    assert!(is_error && text.contains("only with a reason"), "{text}");
    w.ok(
        "backlog_cancel",
        &json!({ "id": id, "reason": "the user dropped it" }),
    );
    let item = w.store.backlog_item(id).unwrap().draft;
    assert_eq!(
        (item.status, item.done_note.as_deref()),
        (BacklogStatus::Cancelled, Some("the user dropped it"))
    );
    assert_eq!(w.listed(&json!({ "status": "cancelled" })).len(), 1);
    w.ok("backlog_reopen", &json!({ "id": id }));
    assert_eq!(
        w.store.backlog_item(id).unwrap().draft.status,
        BacklogStatus::Backlog
    );
    let spawned = w.ok(
        "spawn",
        &json!({ "project": "demo", "slug": "spun", "brief": "# Task
", "backlog": id }),
    );
    assert_eq!(spawned["backlog_unlinked"], Value::Null);
    assert_eq!(
        w.store.backlog_item(id).unwrap().draft.status,
        BacklogStatus::InProgress
    );
    let (is_error, text) = w.tool("backlog_cancel", &json!({ "id": id, "reason": " " }));
    assert!(
        is_error && text.contains("only with a reason"),
        "a refused cancel drops nothing: {text}"
    );
    let task = w.store.find_task("demo-spun").unwrap().unwrap();
    assert!(task.state != TaskState::Dropped, "{:?}", task.state);
    w.ok("backlog_cancel", &json!({ "id": id, "reason": "no" }));
    assert_eq!(
        w.store.backlog_item(id).unwrap().draft.status,
        BacklogStatus::Cancelled
    );
    let task = w.store.find_task("demo-spun").unwrap().unwrap();
    assert!(
        task.state == TaskState::Closed,
        "cancelling the item drops and closes its live task: {:?}",
        task.state
    );
}

impl World {
    fn worker_tool(&mut self, task: &str, name: &str, arguments: &Value) -> (bool, String) {
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
        let env = Env {
            home: &self.home,
            config: &self.config,
            host: &self.host,
            exe: Path::new("S:/bin/headwork.exe"),
            actor: Actor::Worker,
        };
        let identity = Identity {
            session: Some(session),
            role: Some(Role::Worker),
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

    fn spawn_plain(&mut self, slug: &str, brief: &str) -> i64 {
        self.ok(
            "spawn",
            &json!({ "project": "demo", "slug": slug, "brief": brief }),
        );
        let task = self
            .store
            .find_task(&format!("demo-{slug}"))
            .unwrap()
            .unwrap();
        self.store
            .backlog_items_of_task(task.id)
            .unwrap()
            .first()
            .unwrap()
            .id
    }
}

#[test]
fn a_spawn_without_a_backlog_item_makes_one_in_progress_from_the_briefs_goal_line() {
    let mut w = world();
    let id = w.spawn_plain(
        "plain",
        "# Goal: Build the thing

More words.
",
    );
    let item = w.store.backlog_item(id).unwrap();
    assert_eq!(item.draft.text, "Build the thing");
    assert_eq!(item.draft.status, BacklogStatus::InProgress);
    assert_eq!(item.draft.owner, BacklogOwner::Agent);
    assert_eq!(item.draft.project, "demo");
    assert_eq!(item.task.unwrap().name, "demo-plain");
}

#[test]
fn a_worker_blocks_and_unblocks_its_own_item_and_nobody_elses() {
    let mut w = world();
    let own = w.spawn_plain(
        "own",
        "Wait for the release
",
    );
    let other = w.spawn_plain(
        "other",
        "Another thing
",
    );
    let (is_error, text) = w.worker_tool(
        "demo-own",
        "item_block",
        &json!({ "reason": "event", "note": "the release is out Friday" }),
    );
    assert!(!is_error, "{text}");
    let status = w.store.backlog_item(own).unwrap().draft.status;
    assert_eq!(status.kind(), ItemStatus::Blocked, "{status:?}");
    assert_eq!(status.block().unwrap().reason, BlockReason::Event);
    let (is_error, text) = w.worker_tool(
        "demo-own",
        "item_block",
        &json!({ "id": other, "reason": "event", "note": "x" }),
    );
    assert!(is_error && text.contains("its own task"), "{text}");
    let (is_error, text) = w.worker_tool(
        "demo-own",
        "item_block",
        &json!({ "reason": "task_lost", "note": "x" }),
    );
    assert!(is_error, "{text}");
    let (is_error, text) = w.worker_tool("demo-own", "item_unblock", &json!({}));
    assert!(!is_error, "{text}");
    assert_eq!(
        w.store.backlog_item(own).unwrap().draft.status,
        BacklogStatus::InProgress
    );
    let (is_error, _) = w.tool("item_block", &json!({ "reason": "event", "note": "x" }));
    assert!(is_error, "the orchestrator has no item tools");
}

#[test]
fn a_partial_done_blocks_the_item_instead_of_finishing_it() {
    let mut w = world();
    let id = w.spawn_plain(
        "half", "Do half
",
    );
    let task = w.store.find_task("demo-half").unwrap().unwrap();
    let blocked = w
        .store
        .record_partial_outcome(
            &task,
            "session-demo-half",
            Some(
                "the rest [later]
next",
            ),
            Actor::Worker,
        )
        .unwrap();
    assert_eq!(blocked, vec![id]);
    let status = w.store.backlog_item(id).unwrap().draft.status;
    let block = status.block().unwrap();
    assert_eq!(block.reason, BlockReason::Partial);
    assert_eq!(
        block.note.as_deref(),
        Some("task demo-half partial: the rest later next")
    );
}

#[test]
fn the_done_tool_with_outcome_partial_blocks_the_item_and_a_bad_outcome_is_refused() {
    let mut w = world();
    let id = w.spawn_plain(
        "tool", "Do some
",
    );
    let (is_error, text) = w.worker_tool(
        "demo-tool",
        "done",
        &json!({ "summary": "half done", "outcome": "finished" }),
    );
    assert!(is_error && text.contains("task outcome"), "{text}");
    assert_eq!(
        w.store.backlog_item(id).unwrap().draft.status,
        BacklogStatus::InProgress,
        "a refused outcome changes nothing"
    );
    let task = w.store.find_task("demo-tool").unwrap().unwrap();
    w.store
        .append_event(
            Actor::Hook,
            Some(task.id),
            None,
            "tool-used",
            &json!({ "tool": "SendMessage" }),
        )
        .unwrap();
    let (is_error, text) = w.worker_tool(
        "demo-tool",
        "done",
        &json!({ "summary": "half done", "outcome": "partial" }),
    );
    assert!(!is_error, "{text}");
    assert!(text.contains("partial"), "{text}");
    let item = w.store.backlog_item(id).unwrap();
    let block = item.draft.status.block().unwrap();
    assert_eq!(block.reason, BlockReason::Partial);
    assert_eq!(
        block.note.as_deref(),
        Some("task demo-tool partial: half done")
    );
}

#[test]
fn the_tools_depend_undepend_and_add_epics_and_children() {
    let mut w = world();
    let first = w.add("demo", "first");
    let second = w.add("demo", "second");
    w.ok("backlog_depend", &json!({ "id": second, "on": [first] }));
    let item = w.store.backlog_item(second).unwrap();
    assert_eq!(item.deps, [first]);
    assert_eq!(item.draft.status.kind(), ItemStatus::Blocked);
    let (is_error, text) = w.tool("backlog_depend", &json!({ "id": first, "on": [second] }));
    assert!(is_error && text.contains("cycle"), "{text}");
    w.ok("backlog_undepend", &json!({ "id": second, "on": first }));
    assert_eq!(
        w.store.backlog_item(second).unwrap().draft.status,
        BacklogStatus::Backlog
    );
    w.ok(
        "backlog_add",
        &json!({ "project": "demo", "text": "the epic", "epic": true }),
    );
    let epic = w
        .store
        .backlog_items(&BacklogFilter::default())
        .unwrap()
        .last()
        .unwrap()
        .id;
    w.ok(
        "backlog_add",
        &json!({ "project": "demo", "text": "child", "parent": epic, "after": [first] }),
    );
    let children = w.listed(&json!({ "epic": epic }));
    assert!(
        children.len() == 1 && children[0].contains("child"),
        "{children:?}"
    );
    assert!(
        children[0].contains(&format!("after #{first}")),
        "{children:?}"
    );
    let (is_error, text) = w.tool(
        "backlog_add",
        &json!({ "project": "demo", "text": "deep", "parent": second }),
    );
    assert!(is_error && text.contains("not an epic"), "{text}");
}

#[test]
fn spawn_with_after_is_refused_for_an_item_that_has_item_dependencies() {
    let mut w = world();
    let first = w.add("demo", "first");
    let second = w.add("demo", "second");
    w.ok("backlog_depend", &json!({ "id": second, "on": [first] }));
    w.ok("backlog_done", &json!({ "id": first }));
    w.ok(
        "spawn",
        &json!({ "project": "demo", "slug": "early", "brief": "# Task\n" }),
    );
    let (is_error, text) = w.tool(
        "spawn",
        &json!({
            "project": "demo",
            "slug": "late",
            "brief": "# Task\n",
            "backlog": second,
            "depends_on": "demo-early",
        }),
    );
    assert!(is_error, "{text}");
    assert!(
        text.contains(&format!("depends on #{first}")) && text.contains("--after"),
        "{text}"
    );
    let spawned = w.ok(
        "spawn",
        &json!({ "project": "demo", "slug": "plain", "brief": "# Task\n", "backlog": second }),
    );
    assert_eq!(spawned["backlog_unlinked"], Value::Null);
}

#[test]
fn the_dependency_and_epic_arguments_must_have_their_types() {
    let mut w = world();
    let first = w.add("demo", "first");
    for (tool, arguments, named) in [
        (
            "backlog_add",
            json!({ "project": "demo", "text": "x", "parent": "7" }),
            "parent",
        ),
        (
            "backlog_add",
            json!({ "project": "demo", "text": "x", "epic": "yes" }),
            "epic",
        ),
        ("backlog_list", json!({ "epic": "7" }), "epic"),
        ("backlog_depend", json!({ "id": first, "on": [] }), "on"),
    ] {
        let (is_error, text) = w.tool(tool, &arguments);
        assert!(is_error && text.contains(named), "{tool}: {text}");
    }
    assert_eq!(
        w.store
            .backlog_items(&BacklogFilter::default())
            .unwrap()
            .len(),
        1,
        "nothing was added"
    );
}
