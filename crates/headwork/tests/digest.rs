//! The "Needs you" digest: the `digest` command, where it shows (board, status, recap), the daily push, and what the
//! orchestrator's start does when its rules leave no room.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::PathBuf;
use std::process::{Command, Output};

use headwork::ops::digest::{self, EVENT_DEFERRED, EVENT_DELIVERED};
use headwork_core::{Actor, Config};
use headwork_store::{EVENT_DIGEST_PUSHED, EventFilter, Store};

struct Machine {
    dir: tempfile::TempDir,
}

impl Machine {
    fn new(extra: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(
            home.join("config.toml"),
            format!("[machine]\nhost = \"wmux\"\nmax_workers = 4\n{extra}"),
        )
        .unwrap();
        Self { dir }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_headwork"))
            .args(args)
            .env("HEADWORK_HOME", self.home())
            .env("HEADWORK_CLAUDE_JSON", self.dir.path().join(".claude.json"))
            .env("HEADWORK_CLAUDE_DIR", self.dir.path().join(".claude"))
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn store(&self) -> Store {
        Store::open(&self.home().join("state.db")).unwrap()
    }

    fn config(&self) -> Config {
        Config::parse(
            &std::fs::read_to_string(self.home().join("config.toml")).unwrap(),
            "config.toml",
        )
        .unwrap()
    }

    fn add_user_item(&self, text: &str) {
        self.ok(&["backlog", "add", "demo", text, "--owner", "user"]);
    }
}

fn kinds(store: &Store) -> Vec<String> {
    store
        .events(EventFilter {
            task: None,
            limit: 100,
        })
        .unwrap()
        .into_iter()
        .map(|event| event.kind)
        .collect()
}

#[test]
fn an_empty_digest_says_so_and_adds_nothing_to_board_or_status() {
    let m = Machine::new("");
    assert_eq!(m.ok(&["digest"]), "nothing waits on you\n", "empty");
    assert!(!m.ok(&["board"]).contains("Needs you"), "board");
    assert!(!m.ok(&["status"]).contains("Needs you"), "status");
    let json = m.ok(&["digest", "--json"]);
    assert!(json.contains("\"lines\": []"), "{json}");
}

#[test]
fn due_and_user_items_show_on_the_digest_the_board_and_the_status() {
    let m = Machine::new("");
    m.ok(&[
        "backlog",
        "add",
        "demo",
        "Renew the invented licence",
        "--owner",
        "user",
        "--due",
        "2020-01-02",
    ]);
    m.ok(&[
        "backlog",
        "add",
        "demo",
        "Tidy the invented shelf",
        "--owner",
        "agent",
    ]);
    let digest = m.ok(&["digest"]);
    assert!(
        digest
            .starts_with("Needs you\n- backlog #1: Renew the invented licence (due 2020-01-02)\n"),
        "{digest}"
    );
    assert!(!digest.contains("shelf"), "agent items stay out: {digest}");
    for command in ["board", "status"] {
        let text = m.ok(&[command]);
        assert!(
            text.starts_with("Needs you\n- backlog #1"),
            "{command}: {text}"
        );
    }
    let json = m.ok(&["digest", "--json"]);
    assert!(json.contains("Renew the invented licence"), "{json}");
    assert!(
        !m.ok(&["board", "--json"]).contains("Needs you"),
        "json stays JSON"
    );
}

#[test]
fn a_digest_that_does_not_fit_the_rules_is_deferred_and_delivered_once_with_the_first_prompt() {
    let m = Machine::new("");
    m.add_user_item("Look at the invented plan");
    let store = m.store();
    let config = m.config();
    let at_start = digest::after_rules(&store, &config, "orch", Actor::Hook, "r".repeat(9_990));
    assert!(at_start.len() <= 10_000, "{}", at_start.len());
    assert!(
        !at_start.contains("invented plan"),
        "no room for the line itself"
    );
    assert!(kinds(&store).contains(&EVENT_DEFERRED.to_owned()), "logged");
    let first = digest::with_first_prompt(&store, &config, "orch", Actor::Hook);
    assert!(first.contains("Look at the invented plan"), "{first}");
    assert!(
        kinds(&store).contains(&EVENT_DELIVERED.to_owned()),
        "logged"
    );
    assert_eq!(
        digest::with_first_prompt(&store, &config, "orch", Actor::Hook),
        "",
        "once"
    );
    assert_eq!(
        digest::with_first_prompt(&store, &config, "another", Actor::Hook),
        "",
        "only a session whose start deferred it"
    );
}

#[test]
fn the_push_goes_out_once_a_day_through_the_notification_and_is_logged() {
    let m = Machine::new("\n[digest]\npush_hour = 0\n");
    let store = m.store();
    let config = m.config();
    let mut sent = Vec::new();
    let nothing = digest::push_due(&store, &config, Actor::Tick, |text| {
        sent.push(text.to_owned());
    })
    .unwrap();
    assert!(nothing.is_none(), "nothing waits, nothing is pushed");
    m.add_user_item("Check the invented mail");
    let text = digest::push_due(&store, &config, Actor::Tick, |text| {
        sent.push(text.to_owned());
    })
    .unwrap()
    .unwrap();
    assert!(text.starts_with("Needs you\n"), "{text}");
    assert_eq!(sent, [text], "sent through the notification");
    let again = digest::push_due(&store, &config, Actor::Tick, |text| {
        sent.push(text.to_owned());
    })
    .unwrap();
    assert!(again.is_none(), "once today");
    let logged = kinds(&store)
        .into_iter()
        .filter(|kind| kind == EVENT_DIGEST_PUSHED)
        .count();
    assert_eq!(logged, 1, "one digest-pushed event");
}

#[test]
fn a_disabled_push_sends_nothing() {
    let m = Machine::new("\n[digest]\npush = false\npush_hour = 0\n");
    m.add_user_item("Check the invented mail");
    let (store, config) = (m.store(), m.config());
    let mut sent = 0;
    let pushed = digest::push_due(&store, &config, Actor::Tick, |_| sent += 1).unwrap();
    assert!(pushed.is_none() && sent == 0, "off");
}

#[test]
fn a_push_hour_not_reached_sends_nothing() {
    let m = Machine::new("\n[digest]\npush_hour = 23\n");
    m.add_user_item("Check the invented mail");
    let (store, config) = (m.store(), m.config());
    let reached = store.digest_push_due(23).unwrap();
    let pushed = digest::push_due(&store, &config, Actor::Tick, |_| ()).unwrap();
    assert_eq!(pushed.is_some(), reached, "only from the hour on");
}
