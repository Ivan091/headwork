//! Token usage rows: idempotent inserts, tags fixed from the session, re-tagging and totals per group.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;

use headwork_core::lifecycle::SessionFacts;
use headwork_core::usage::{Tokens, UsageLine};
use headwork_core::{
    Actor, Config, PermissionMode, ProjectName, Role, Slug, TaskRequest, TaskShape, Trigger,
    UsageGroup, UsageRole,
};
use headwork_store::{
    FileMark, NewSession, NewTask, Store, Task, TaskId, UsageBatch, UsageQuery, UsageTag,
    all_models,
};

fn config() -> Config {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/config.toml");
    Config::parse(&std::fs::read_to_string(path).unwrap(), "config.toml").unwrap()
}

fn store() -> (Store, Config) {
    let config = config();
    let mut store = Store::open_in_memory().unwrap();
    store.sync_projects(&config).unwrap();
    (store, config)
}

fn task(store: &mut Store, config: &Config, slug: &str, shape: TaskShape<TaskId>) -> Task {
    let new = NewTask {
        request: TaskRequest {
            project: ProjectName::new("me-sim").unwrap(),
            slug: Slug::new(slug).unwrap(),
            kind: "scene".to_owned(),
            model: None,
            effort: None,
            in_main: false,
            keep: false,
        },
        brief_path: format!("briefs/{slug}.md"),
        depends_on: None,
        permission_mode: PermissionMode::Auto,
        shape,
    };
    store
        .create_task(config, &new, Actor::Orchestrator)
        .unwrap()
}

fn session(store: &mut Store, id: &str, task: Option<TaskId>, role: Role) {
    store
        .record_session(
            &NewSession {
                id: id.to_owned(),
                task_id: task,
                role,
                rc_name: id.to_owned(),
                host: "wmux".to_owned(),
                host_workspace: None,
                host_surface: None,
            },
            Actor::User,
        )
        .unwrap();
}

fn line(id: &str, at: &str, model: &str, input: u64, output: u64) -> UsageLine {
    UsageLine {
        message_id: id.to_owned(),
        at: at.to_owned(),
        model: model.to_owned(),
        effort: Some("medium".to_owned()),
        speed: None,
        service_tier: None,
        cwd: Some("S:/Code/x".to_owned()),
        tokens: Tokens {
            input,
            cache_read: 10,
            output,
            ..Tokens::default()
        },
    }
}

fn record(store: &mut Store, session: &str, agent: Option<&str>, lines: &[UsageLine]) -> usize {
    let tag = store
        .usage_tag(session)
        .unwrap()
        .unwrap_or(UsageTag::UNMANAGED);
    store
        .record_usage(&UsageBatch {
            path: &format!("{session}-{agent:?}.jsonl"),
            session_id: session,
            agent_id: agent,
            agent_type: agent.map(|_| "Explore"),
            tag,
            dir: "S:/Code/x",
            lines,
            mark: FileMark {
                read_to: 100,
                size: 100,
                mtime_ms: 1,
            },
        })
        .unwrap()
}

const fn query(by: UsageGroup) -> UsageQuery {
    UsageQuery {
        by,
        since: None,
        tasks: None,
        unmanaged: false,
    }
}

fn keys(store: &Store, query: &UsageQuery) -> Vec<(String, u64, u64)> {
    store
        .usage_totals(query)
        .unwrap()
        .into_iter()
        .map(|total| (total.key, total.responses, total.tokens.total()))
        .collect()
}

#[test]
fn inserts_are_idempotent_and_marks_kept() {
    let (mut store, config) = store();
    let sand = task(&mut store, &config, "sand", TaskShape::Plain);
    session(&mut store, "s1", Some(sand.id), Role::Worker);
    let lines = [
        line("m1", "2026-11-22T10:00:00.000Z", "claude-opus-5-5", 5, 5),
        line("m1", "2026-11-22T10:00:00.000Z", "claude-opus-5-5", 5, 5),
        line("m2", "2026-11-22T11:00:00.000Z", "claude-opus-5-5", 1, 1),
    ];
    assert_eq!(
        record(&mut store, "s1", None, &lines),
        2,
        "copies of one response count once"
    );
    assert_eq!(
        record(&mut store, "s1", None, &lines),
        0,
        "a reread adds nothing"
    );
    assert_eq!(
        store.usage_file("s1-None.jsonl").unwrap(),
        Some(FileMark {
            read_to: 100,
            size: 100,
            mtime_ms: 1
        }),
        "the mark is kept"
    );
    assert_eq!(store.usage_file("other").unwrap(), None, "never read");
    assert_eq!(
        keys(&store, &query(UsageGroup::Task)),
        [("me-sim-sand".to_owned(), 2, 32)],
        "one task, two responses"
    );
}

#[test]
fn roles_come_from_the_session_and_task_shape() {
    let (mut store, config) = store();
    let lead = task(&mut store, &config, "big", TaskShape::Lead);
    store.start_task(&config, lead.id, Actor::Tick).unwrap();
    store
        .transition(
            lead.id,
            &Trigger::SessionStarted(SessionFacts {
                session_matches: true,
                launcher_alive: true,
            }),
            Actor::Hook,
        )
        .unwrap();
    let part = task(
        &mut store,
        &config,
        "big-review",
        TaskShape::Part { parent: lead.id },
    );
    let plain = task(&mut store, &config, "sand", TaskShape::Plain);
    session(&mut store, "lead", Some(lead.id), Role::Worker);
    session(&mut store, "part", Some(part.id), Role::Worker);
    session(&mut store, "plain", Some(plain.id), Role::Worker);
    session(&mut store, "orch", None, Role::Orchestrator);
    let cases = [
        (
            "lead",
            Some(UsageTag {
                task: Some(lead.id),
                role: UsageRole::Lead,
            }),
        ),
        (
            "part",
            Some(UsageTag {
                task: Some(part.id),
                role: UsageRole::Part,
            }),
        ),
        (
            "plain",
            Some(UsageTag {
                task: Some(plain.id),
                role: UsageRole::Worker,
            }),
        ),
        (
            "orch",
            Some(UsageTag {
                task: None,
                role: UsageRole::Orchestrator,
            }),
        ),
        ("stranger", None),
    ];
    for (id, expected) in cases {
        assert_eq!(store.usage_tag(id).unwrap(), expected, "{id}");
    }
    for (index, id) in ["lead", "part", "orch", "stranger"].iter().enumerate() {
        record(
            &mut store,
            id,
            None,
            &[line(
                &format!("m{index}"),
                "2026-11-22T10:00:00.000Z",
                "m",
                1,
                0,
            )],
        );
    }
    let roles = keys(&store, &query(UsageGroup::Role));
    let mut names: Vec<_> = roles.iter().map(|(key, _, _)| key.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["lead", "orchestrator", "part", "unmanaged"],
        "one line per role"
    );
    let projects = keys(&store, &query(UsageGroup::Project));
    let mut names: Vec<_> = projects.iter().map(|(key, _, _)| key.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["me-sim", "orchestrator", "unmanaged"],
        "taskless rows group by role"
    );
    let tree = UsageQuery {
        tasks: Some(vec![lead.id, part.id]),
        ..query(UsageGroup::Task)
    };
    assert_eq!(keys(&store, &tree).len(), 2, "the lead and its part only");
}

#[test]
fn unmanaged_rows_are_one_line_unless_asked_and_retagged_once_known() {
    let (mut store, config) = store();
    record(
        &mut store,
        "u1",
        None,
        &[line("m1", "2026-11-21T10:00:00.000Z", "m", 3, 0)],
    );
    record(
        &mut store,
        "u2",
        Some("a1"),
        &[line("m2", "2026-11-22T10:00:00.000Z", "m", 4, 0)],
    );
    assert_eq!(
        keys(&store, &query(UsageGroup::Folder)),
        [("unmanaged".to_owned(), 2, 27)],
        "one unmanaged line"
    );
    let broken = UsageQuery {
        unmanaged: true,
        ..query(UsageGroup::Task)
    };
    assert_eq!(
        keys(&store, &broken),
        [("S:/Code/x".to_owned(), 2, 27)],
        "by folder"
    );
    let agents = UsageQuery {
        unmanaged: true,
        ..query(UsageGroup::Agent)
    };
    assert_eq!(keys(&store, &agents).len(), 2, "main and Explore");
    let since = UsageQuery {
        since: Some("2026-11-22T00:00:00.000Z".to_owned()),
        ..query(UsageGroup::Day)
    };
    assert_eq!(
        keys(&store, &since),
        [("unmanaged".to_owned(), 1, 14)],
        "only the second day"
    );

    let sand = task(&mut store, &config, "sand", TaskShape::Plain);
    session(&mut store, "u1", Some(sand.id), Role::Worker);
    assert_eq!(
        store.retag_usage().unwrap(),
        1,
        "the adopted session's row moves"
    );
    assert_eq!(store.retag_usage().unwrap(), 0, "once");
    let tasks = keys(&store, &query(UsageGroup::Task));
    assert!(
        tasks.contains(&("me-sim-sand".to_owned(), 1, 13)),
        "now under its task: {tasks:?}"
    );
}

#[test]
fn a_leads_tokens_roll_up_its_parts() {
    let (mut store, config) = store();
    let lead = task(&mut store, &config, "big", TaskShape::Lead);
    store.start_task(&config, lead.id, Actor::Tick).unwrap();
    store
        .transition(
            lead.id,
            &Trigger::SessionStarted(SessionFacts {
                session_matches: true,
                launcher_alive: true,
            }),
            Actor::Hook,
        )
        .unwrap();
    let part = task(
        &mut store,
        &config,
        "big-review",
        TaskShape::Part { parent: lead.id },
    );
    session(&mut store, "lead", Some(lead.id), Role::Worker);
    session(&mut store, "part", Some(part.id), Role::Worker);
    record(
        &mut store,
        "lead",
        None,
        &[line("m1", "2026-11-22T10:00:00.000Z", "m", 5, 5)],
    );
    record(
        &mut store,
        "part",
        None,
        &[line("m2", "2026-11-22T10:00:00.000Z", "m", 1, 1)],
    );
    let tokens = store.usage_task_tokens().unwrap();
    assert_eq!(
        all_models(&tokens.own(lead.id)).total(),
        20,
        "the lead's own"
    );
    assert_eq!(
        all_models(&tokens.tree(lead.id)).total(),
        32,
        "with its part"
    );
    assert_eq!(
        all_models(&tokens.own(part.id)).total(),
        12,
        "the part's own"
    );
    let board = store.board(false).unwrap();
    let shown: Vec<(String, u64)> = board
        .tasks
        .iter()
        .map(|entry| (entry.task.name.to_string(), entry.tokens))
        .collect();
    assert_eq!(
        shown,
        [
            ("me-sim-big".to_owned(), 32),
            ("me-sim-big-review".to_owned(), 12)
        ],
        "the board rolls up the lead"
    );
}

#[test]
fn a_roll_up_keeps_every_total_and_runs_once() {
    let (mut store, config) = store();
    let sand = task(&mut store, &config, "sand", TaskShape::Plain);
    session(&mut store, "s1", Some(sand.id), Role::Worker);
    let lines = [
        line("m1", "2026-08-01T10:00:00.000Z", "a", 5, 5),
        line("m2", "2026-08-01T11:00:00.000Z", "a", 1, 1),
        line("m3", "2026-08-02T10:00:00.000Z", "b", 2, 2),
        line("m4", "2026-11-22T10:00:00.000Z", "a", 3, 3),
    ];
    record(&mut store, "s1", Some("x1"), &lines);
    let groupings = [
        UsageGroup::Task,
        UsageGroup::Day,
        UsageGroup::Model,
        UsageGroup::Agent,
        UsageGroup::Role,
    ];
    let before: Vec<_> = groupings
        .iter()
        .map(|by| keys(&store, &query(*by)))
        .collect();
    let window = UsageQuery {
        since: Some("2026-08-02T00:00:00.000Z".to_owned()),
        ..query(UsageGroup::Task)
    };
    let window_before = keys(&store, &window);

    let rolled = store
        .roll_up_usage("2026-11-01", Actor::Tick)
        .unwrap()
        .unwrap();
    assert_eq!((rolled.rows, rolled.days), (3, 2), "two August days summed");
    assert_eq!(
        store.usage_rolled_to().unwrap(),
        Some("2026-08-02".to_owned()),
        "the last summed day"
    );
    let after: Vec<_> = groupings
        .iter()
        .map(|by| keys(&store, &query(*by)))
        .collect();
    assert_eq!(before, after, "every total is the same");
    assert_eq!(
        keys(&store, &window),
        window_before,
        "a window that starts on a day boundary too"
    );
    assert_eq!(
        store.roll_up_usage("2026-11-01", Actor::Tick).unwrap(),
        None,
        "nothing left to sum"
    );

    record(
        &mut store,
        "s1",
        Some("x1"),
        &[line("m5", "2026-08-01T12:00:00.000Z", "a", 1, 0)],
    );
    let rolled = store
        .roll_up_usage("2026-11-01", Actor::Tick)
        .unwrap()
        .unwrap();
    assert_eq!(rolled.rows, 1, "a late row adds to its day");
    let days = keys(&store, &query(UsageGroup::Day));
    assert!(days.contains(&("2026-08-01".to_owned(), 3, 43)), "{days:?}");
}
