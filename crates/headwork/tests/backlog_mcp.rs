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
use headwork_core::backlog::BacklogState;
use headwork_core::{Actor, Config, Role};
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
    w.ok("backlog_done", &json!({ "id": third, "drop": true }));
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
    assert!(is_error && text.contains("Done or Dropped"), "{text}");
}

#[test]
fn done_finishes_an_item_and_drop_gives_it_up() {
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
    assert_eq!(finished.state, BacklogState::Done);
    assert_eq!(finished.done_note.as_deref(), Some("all good"));
    let given_up = w.store.backlog_item(given_up).unwrap().draft;
    assert_eq!(given_up.state, BacklogState::Dropped);
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
        &json!({ "project": "demo", "slug": "old", "brief": "# Task\n", "todo": added }),
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
    w.ok("todo_done", &json!({ "id": added }));
    let items = w.store.backlog_items(&BacklogFilter::default()).unwrap();
    assert_eq!(items.len(), 2, "{items:?}");
}
