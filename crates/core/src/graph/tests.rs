#![expect(clippy::unwrap_used, reason = "tests fail loudly on a bad fixture")]

use std::collections::BTreeSet;

use proptest::prelude::*;

use super::build::{ASK_LABEL_CHARS, external_node};
use super::*;

fn task(
    id: i64,
    name: &str,
    state: TaskState,
    parent: Option<i64>,
    lead: bool,
    at: &str,
) -> TaskFact {
    TaskFact {
        id,
        name: name.to_owned(),
        project: "headwork".to_owned(),
        kind: "feature".to_owned(),
        state,
        model: Model::Opus,
        effort: Some(Effort::Medium),
        parent,
        depends_on: None,
        lead,
        created_by: Actor::Orchestrator,
        queued_at: at.to_owned(),
        last_report: None,
    }
}

fn session(
    id: &str,
    task: Option<i64>,
    role: Role,
    name: &str,
    started: &str,
    ended: Option<&str>,
) -> SessionFact {
    SessionFact {
        id: id.to_owned(),
        task,
        role,
        rc_name: name.to_owned(),
        state: if ended.is_some() {
            SessionState::Ended
        } else {
            SessionState::Live
        },
        started_at: started.to_owned(),
        ended_at: ended.map(str::to_owned),
    }
}

fn change<S>(key: i64, from: Option<S>, to: S, at: &str) -> Change<S> {
    Change {
        key,
        from,
        to,
        at: at.to_owned(),
    }
}

fn message(from: &str, to: &str, summary: Option<&str>, at: &str) -> MessageFact {
    MessageFact {
        from: from.to_owned(),
        to: to.to_owned(),
        summary: summary.map(str::to_owned),
        at: at.to_owned(),
    }
}

fn received(session: &str, from: &str, at: &str) -> ReceivedFact {
    ReceivedFact {
        session: session.to_owned(),
        from: from.to_owned(),
        at: at.to_owned(),
    }
}

fn report(status: &str, at: &str) -> ReportFact {
    ReportFact {
        session: "s1".to_owned(),
        status: status.to_owned(),
        at: at.to_owned(),
    }
}

fn ask(
    id: i64,
    task: i64,
    session: Option<&str>,
    header: &str,
    created: &str,
    closed: Option<&str>,
) -> AskFact {
    AskFact {
        id,
        task: Some(task),
        session: session.map(str::to_owned),
        header: Some(header.to_owned()),
        question: "Which?".to_owned(),
        state: if closed.is_some() {
            AskState::Answered
        } else {
            AskState::Open
        },
        created_at: created.to_owned(),
        closed_at: closed.map(str::to_owned),
    }
}

fn agent(id: &str, session: &str, first: Option<&str>, last: Option<&str>) -> AgentFact {
    AgentFact {
        id: id.to_owned(),
        session: session.to_owned(),
        agent_type: Some("Explore".to_owned()),
        description: Some("find the hook".to_owned()),
        model: Some("haiku".to_owned()),
        depth: Some(1),
        parent: None,
        finished: None,
        first_at: first.map(str::to_owned),
        last_at: last.map(str::to_owned),
    }
}

/// A lead (1) with a part (2), a closed plain task (3), the orchestrator, and one of everything else.
fn facts() -> Facts {
    Facts {
        tasks: vec![
            task(1, "headwork-lead", TaskState::Working, None, true, "T01"),
            task(
                2,
                "headwork-lead-review",
                TaskState::Working,
                Some(1),
                false,
                "T03",
            ),
            task(3, "headwork-old", TaskState::Closed, None, false, "T00"),
        ],
        sessions: vec![
            session("o1", None, Role::Orchestrator, "orchestrator", "T00", None),
            session("s1", Some(1), Role::Worker, "headwork-lead", "T02", None),
            session(
                "s2",
                Some(2),
                Role::Worker,
                "headwork-lead-review",
                "T04",
                None,
            ),
            session(
                "s3",
                Some(3),
                Role::Worker,
                "headwork-old",
                "T00",
                Some("T05"),
            ),
        ],
        agents: vec![agent("a1", "s1", Some("T05"), Some("T06"))],
        helpers: vec![HelperFact {
            session: "h1".to_owned(),
            task: 1,
            at: "T05".to_owned(),
        }],
        jobs: vec![JobFact {
            id: 7,
            task: Some(1),
            session: Some("s1".to_owned()),
            label: "render".to_owned(),
            state: JobState::Done,
            started_at: "T05".to_owned(),
            ended_at: Some("T08".to_owned()),
        }],
        prs: vec![PrFact {
            id: 9,
            task: 1,
            number: 41,
            url: "https://github.com/o/r/pull/41".to_owned(),
            state: PullState::Open,
            first_at: "T06".to_owned(),
        }],
        asks: vec![
            ask(4, 1, Some("s1"), "Scope", "T02", Some("T03")),
            ask(5, 2, Some("s2"), "Dispute", "T07", None),
        ],
        reviews: vec![ReviewFact {
            id: 1,
            task: 1,
            part: Some(2),
            flow: "code".to_owned(),
            level: Level::Strict,
            state: ReviewState::Running,
            requested_at: "T03".to_owned(),
            finished_at: None,
        }],
        review_nodes: vec![ReviewNodeFact {
            review: 1,
            round: 1,
            node: "correctness".to_owned(),
            item: 0,
            state: NodeState::Done,
            started_at: Some("T05".to_owned()),
            finished_at: Some("T06".to_owned()),
        }],
        messages: vec![
            message("s2", "headwork-lead", Some("first"), "T05"),
            message("s2", "headwork-lead [ab12]", Some("second"), "T06"),
            message("s1", "root", None, "T06"),
            message("s1", "stranger", None, "T06"),
        ],
        received: vec![
            received("s1", "headwork-lead-review", "T05"),
            received("s1", "headwork-lead-review", "T06"),
            received("o1", "headwork-lead", "T06"),
        ],
        reports: vec![report("progress", "T02"), report("review", "T06")],
        transitions: vec![
            change(1, Some(TaskState::Queued), TaskState::Working, "T02"),
            change(3, Some(TaskState::Working), TaskState::Closed, "T05"),
        ],
        pr_changes: Vec::new(),
        job_changes: Vec::new(),
        review_changes: Vec::new(),
        skipped: Vec::new(),
    }
}

fn edge<'g>(graph: &'g Graph, kind: EdgeKind, source: &str, target: &str) -> Option<&'g Edge> {
    graph
        .edges_of(kind)
        .find(|edge| edge.source == source && edge.target == target)
}

fn state(graph: &Graph, id: &str) -> Option<&'static str> {
    graph
        .node(id)
        .and_then(|node| node.state)
        .map(Status::as_str)
}

#[test]
fn every_node_kind_is_drawn_now() {
    let stranger = external_node("stranger");
    let graph = build(&facts(), None, Scope::Live);
    let table = [
        ("user", NodeKind::User),
        ("orchestrator", NodeKind::Orchestrator),
        ("task:1", NodeKind::Task),
        ("session:s1", NodeKind::Lead),
        ("task:2", NodeKind::Task),
        ("session:s2", NodeKind::Part),
        ("agent:a1", NodeKind::Subagent),
        ("helper:h1", NodeKind::Helper),
        ("job:7", NodeKind::Job),
        ("pr:9", NodeKind::Pr),
        ("ask:5", NodeKind::Ask),
        ("review:1", NodeKind::Review),
        ("review-node:1:1:correctness:0", NodeKind::ReviewNode),
        (stranger.as_str(), NodeKind::External),
    ];
    for (id, kind) in table {
        assert_eq!(
            graph.node(id).map(|node| node.kind),
            Some(kind),
            "node {id}"
        );
    }
    let all = build(&facts(), None, Scope::All);
    assert_eq!(
        all.node("session:s3").map(|node| node.kind),
        Some(NodeKind::Worker),
        "a plain task's session"
    );
}

#[test]
fn every_drawn_state_fits_its_node_kind() {
    for at in [None, Some("T04"), Some("T09")] {
        let graph = build(&facts(), at, Scope::All);
        for node in &graph.nodes {
            if let Some(state) = node.state {
                assert!(
                    state.fits(node.kind),
                    "{} is {:?} with {state:?}",
                    node.id,
                    node.kind
                );
            }
        }
    }
}

#[test]
fn nesting_puts_parts_and_satellites_in_their_boxes() {
    let graph = build(&facts(), None, Scope::Live);
    let table = [
        ("task:1", None),
        ("task:2", Some("task:1")),
        ("session:s1", Some("task:1")),
        ("session:s2", Some("task:2")),
        ("agent:a1", Some("task:1")),
        ("helper:h1", Some("task:1")),
        ("job:7", Some("task:1")),
        ("pr:9", Some("task:1")),
        ("review:1", Some("task:2")),
        ("review-node:1:1:correctness:0", Some("task:2")),
    ];
    for (id, parent) in table {
        assert_eq!(
            graph.node(id).unwrap().parent.as_deref(),
            parent,
            "parent of {id}"
        );
    }
}

#[test]
fn every_edge_kind_is_drawn_now() {
    let stranger = external_node("stranger");
    let graph = build(&facts(), None, Scope::Live);
    let table = [
        (EdgeKind::Spawned, "orchestrator", "task:1"),
        (EdgeKind::PartOf, "task:2", "task:1"),
        (EdgeKind::Called, "session:s1", "agent:a1"),
        (EdgeKind::Opened, "session:s1", "helper:h1"),
        (EdgeKind::Opened, "session:s1", "job:7"),
        (EdgeKind::Message, "session:s2", "session:s1"),
        (EdgeKind::Message, "session:s1", "orchestrator"),
        (EdgeKind::Message, "session:s1", stranger.as_str()),
        (EdgeKind::Report, "session:s1", "orchestrator"),
        (EdgeKind::Asked, "session:s1", "user"),
        (EdgeKind::Asked, "session:s2", "ask:5"),
        (EdgeKind::Asked, "ask:5", "user"),
        (EdgeKind::Reviews, "review:1", "session:s1"),
        (EdgeKind::Ran, "review:1", "review-node:1:1:correctness:0"),
        (EdgeKind::Delivered, "task:1", "pr:9"),
    ];
    for (kind, source, target) in table {
        assert!(
            edge(&graph, kind, source, target).is_some(),
            "{kind:?} {source} -> {target}"
        );
    }
}

#[test]
fn depends_on_joins_two_drawn_tasks() {
    let mut facts = facts();
    facts.tasks[1].depends_on = Some(1);
    let graph = build(&facts, None, Scope::Live);
    assert!(
        edge(&graph, EdgeKind::DependsOn, "task:2", "task:1").is_some(),
        "dependency drawn"
    );
}

#[test]
fn a_message_between_sessions_counts_once_from_its_receiver_with_the_senders_summary() {
    let graph = build(&facts(), None, Scope::Live);
    let sent = edge(&graph, EdgeKind::Message, "session:s2", "session:s1").unwrap();
    assert_eq!(
        (sent.weight, sent.label.as_deref()),
        (2, Some("second")),
        "two arrivals, the last summary"
    );
    let reported = edge(&graph, EdgeKind::Report, "session:s1", "orchestrator").unwrap();
    assert_eq!(
        (reported.weight, reported.label.as_deref()),
        (2, Some("review")),
        "two reports, the last status"
    );
}

#[test]
fn a_message_from_outside_headwork_reaches_its_receiver_from_an_external_node() {
    let mut facts = facts();
    facts.received.push(received("s1", "somebody [x9]", "T06"));
    let graph = build(&facts, None, Scope::Live);
    let id = external_node("somebody");
    assert_eq!(
        graph.node(&id).map(|node| node.label.as_str()),
        Some("somebody"),
        "labelled by its name"
    );
    assert!(
        edge(&graph, EdgeKind::Message, &id, "session:s1").is_some(),
        "to its receiver"
    );
}

#[test]
fn one_unknown_name_with_different_refs_is_one_external_node() {
    let mut facts = facts();
    facts.messages.push(message("s1", "peer [a1]", None, "T06"));
    facts.messages.push(message("s1", "peer [b7]", None, "T06"));
    let graph = build(&facts, None, Scope::Live);
    let sent = edge(
        &graph,
        EdgeKind::Message,
        "session:s1",
        &external_node("peer"),
    )
    .unwrap();
    assert_eq!(sent.weight, 2, "one node, two messages");
}

#[test]
fn scopes_choose_the_tasks() {
    let table: [(Scope, &[&str], &[&str]); 3] = [
        (Scope::Live, &["task:1", "task:2"], &["task:3"]),
        (Scope::All, &["task:1", "task:2", "task:3"], &[]),
        (Scope::Task(2), &["task:1", "task:2"], &["task:3"]),
    ];
    for (scope, present, absent) in table {
        let graph = build(&facts(), None, scope);
        for id in present {
            assert!(graph.node(id).is_some(), "{scope:?} draws {id}");
        }
        for id in absent {
            assert!(graph.node(id).is_none(), "{scope:?} leaves out {id}");
        }
    }
}

#[test]
fn edges_never_point_at_nodes_left_out() {
    let graph = build(&facts(), None, Scope::Task(3));
    for edge in &graph.edges {
        assert!(
            graph.node(&edge.source).is_some() && graph.node(&edge.target).is_some(),
            "{edge:?}"
        );
    }
}

#[test]
fn the_graph_at_an_instant_holds_what_existed_then() {
    let table: [(&str, &[&str], &[&str]); 3] = [
        (
            "T01",
            &["task:1", "task:3"],
            &["task:2", "session:s1", "agent:a1"],
        ),
        (
            "T04",
            &["task:1", "task:2", "session:s1", "session:s2", "task:3"],
            &["agent:a1", "pr:9"],
        ),
        ("T07", &["agent:a1", "pr:9", "job:7", "ask:5"], &["task:3"]),
    ];
    for (at, present, absent) in table {
        let graph = build(&facts(), Some(at), Scope::Live);
        for id in present {
            assert!(graph.node(id).is_some(), "at {at} draws {id}");
        }
        for id in absent {
            assert!(graph.node(id).is_none(), "at {at} leaves out {id}");
        }
    }
}

#[test]
fn states_at_an_instant_follow_the_logs() {
    assert_eq!(
        state(&build(&facts(), Some("T01"), Scope::All), "task:1"),
        Some("queued"),
        "before its transition"
    );
    let graph = build(&facts(), Some("T07"), Scope::All);
    let table = [
        ("task:1", "working"),
        ("task:3", "closed"),
        ("agent:a1", "done"),
        ("job:7", "running"),
        ("review-node:1:1:correctness:0", "done"),
    ];
    for (id, word) in table {
        assert_eq!(state(&graph, id), Some(word), "state of {id}");
    }
}

#[test]
fn past_states_of_pull_requests_jobs_and_reviews_come_from_their_logs() {
    let mut facts = facts();
    facts.prs[0].state = PullState::Merged;
    facts
        .pr_changes
        .push(change(9, Some(PullState::Open), PullState::Merged, "T08"));
    facts.jobs[0].ended_at = None;
    facts.jobs[0].state = JobState::Failed;
    facts
        .job_changes
        .push(change(7, None, JobState::Failed, "T08"));
    facts.reviews[0].state = ReviewState::Done;
    facts.review_changes.push(change(
        1,
        Some(ReviewState::Requested),
        ReviewState::Running,
        "T04",
    ));
    facts.review_changes.push(change(
        1,
        Some(ReviewState::Running),
        ReviewState::Done,
        "T08",
    ));
    let table = [
        ("T07", "pr:9", "open"),
        ("T09", "pr:9", "merged"),
        ("T07", "job:7", "running"),
        ("T09", "job:7", "failed"),
        ("T03", "review:1", "requested"),
        ("T05", "review:1", "running"),
        ("T09", "review:1", "done"),
    ];
    for (at, id, word) in table {
        assert_eq!(
            state(&build(&facts, Some(at), Scope::All), id),
            Some(word),
            "{id} at {at}"
        );
    }
    assert_eq!(
        state(&build(&facts, None, Scope::All), "pr:9"),
        Some("merged"),
        "now"
    );
}

#[test]
fn change_at_reads_before_after_first_and_now() {
    let stopping = change(1, None, JobState::Stopping, "T05");
    let done = change(1, Some(JobState::Stopping), JobState::Done, "T07");
    let changes = [&stopping, &done];
    let table = [
        ("T01", JobState::Running),
        ("T05", JobState::Stopping),
        ("T06", JobState::Stopping),
        ("T09", JobState::Done),
    ];
    for (at, expected) in table {
        assert_eq!(
            change_at(&changes, at, JobState::Running, JobState::Parked),
            expected,
            "at {at}"
        );
    }
    assert_eq!(
        change_at::<JobState>(&[], "T01", JobState::Running, JobState::Parked),
        JobState::Parked,
        "no log: now"
    );
}

proptest! {
    #[test]
    fn change_at_replays_the_log_up_to_the_instant(steps in proptest::collection::vec((0_u8..20, 0_usize..5), 0..12), at in 0_u8..250) {
        let words = [JobState::Running, JobState::Stopping, JobState::Parked, JobState::Done, JobState::Failed];
        let mut log: Vec<Change<JobState>> = Vec::new();
        let mut time = 0_u8;
        let mut current = JobState::Running;
        for (gap, word) in steps {
            time = time.saturating_add(gap);
            let to = words[word % words.len()];
            log.push(change(1, Some(current), to, &format!("T{time:03}")));
            current = to;
        }
        let refs: Vec<&Change<JobState>> = log.iter().collect();
        let instant = format!("T{at:03}");
        let replayed = log.iter().rfind(|step| step.at <= instant).map_or(JobState::Running, |step| step.to);
        prop_assert_eq!(change_at(&refs, &instant, JobState::Running, current), replayed);
    }
}

#[test]
fn an_answered_ask_folds_into_an_edge() {
    assert!(
        build(&facts(), Some("T02"), Scope::Live)
            .node("ask:4")
            .is_some(),
        "open at T02"
    );
    let graph = build(&facts(), Some("T04"), Scope::Live);
    assert!(graph.node("ask:4").is_none(), "answered by T04");
    assert!(
        edge(&graph, EdgeKind::Asked, "session:s1", "user").is_some(),
        "folded into a line"
    );
}

#[test]
fn a_superseded_ask_was_open_until_it_was_superseded() {
    let mut facts = facts();
    facts.asks[0].state = AskState::Superseded;
    facts.asks[0].closed_at = Some("T05".to_owned());
    assert!(
        build(&facts, Some("T04"), Scope::Live)
            .node("ask:4")
            .is_some(),
        "open before"
    );
    assert!(
        build(&facts, Some("T05"), Scope::Live)
            .node("ask:4")
            .is_none(),
        "closed at"
    );
    facts.asks[0].closed_at = None;
    assert!(
        build(&facts, Some("T02"), Scope::Live)
            .node("ask:4")
            .is_none(),
        "no time: closed when made"
    );
}

#[test]
fn a_pending_ask_keeps_its_word_and_one_headwork_raised_comes_from_the_task() {
    let mut facts = facts();
    facts.asks[1].state = AskState::Pending;
    facts.asks[1].session = None;
    let graph = build(&facts, None, Scope::Live);
    assert_eq!(state(&graph, "ask:5"), Some("pending"), "pending");
    assert!(
        edge(&graph, EdgeKind::Asked, "task:2", "ask:5").is_some(),
        "from the task's box"
    );
    assert!(
        edge(&graph, EdgeKind::Asked, "session:s2", "ask:5").is_none(),
        "not from its session"
    );
}

#[test]
fn an_ask_without_a_header_takes_the_start_of_its_question() {
    let mut facts = facts();
    facts.asks[1].header = None;
    facts.asks[1].question = "q".repeat(60);
    let graph = build(&facts, None, Scope::Live);
    assert_eq!(
        graph.node("ask:5").unwrap().label.chars().count(),
        ASK_LABEL_CHARS,
        "cut"
    );
}

#[test]
fn the_orchestrator_is_one_node_across_restarts_and_knows_only_its_sessions_so_far() {
    let mut facts = facts();
    facts.sessions[0].ended_at = Some("T03".to_owned());
    facts.sessions.push(session(
        "o2",
        None,
        Role::Orchestrator,
        "orch-2",
        "T04",
        None,
    ));
    facts.received.push(received("s1", "orchestrator", "T02"));
    let graph = build(&facts, None, Scope::Live);
    assert_eq!(
        graph
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Orchestrator)
            .count(),
        1,
        "one node"
    );
    assert_eq!(
        graph.node(ORCHESTRATOR_NODE).unwrap().label,
        "orch-2",
        "the newest live session names it"
    );
    assert!(
        edge(&graph, EdgeKind::Message, "orchestrator", "session:s1").is_some(),
        "the old session's talk"
    );
    let between = build(&facts, Some("T03"), Scope::Live);
    let node = between.node(ORCHESTRATOR_NODE).unwrap();
    assert_eq!(
        (node.label.as_str(), node.state.map(Status::as_str)),
        ("orchestrator", Some("ended")),
        "o1 only"
    );
    assert_eq!(
        node.details.get("sessions").map(String::as_str),
        Some("o1"),
        "o2 had not started"
    );
}

#[test]
fn the_orchestrator_is_ended_now_when_every_session_ended() {
    let mut facts = facts();
    facts.sessions[0].ended_at = Some("T05".to_owned());
    assert_eq!(
        state(&build(&facts, None, Scope::Live), ORCHESTRATOR_NODE),
        Some("ended"),
        "now"
    );
    assert_eq!(
        state(&build(&facts, Some("T04"), Scope::Live), ORCHESTRATOR_NODE),
        Some("live"),
        "before it ended"
    );
}

#[test]
fn a_message_to_the_orchestrators_own_name_reaches_it() {
    let mut facts = facts();
    facts.sessions[0].rc_name = "boss".to_owned();
    facts
        .messages
        .push(message("s1", "boss", Some("hello"), "T07"));
    facts.received.push(received("o1", "headwork-lead", "T07"));
    let graph = build(&facts, None, Scope::Live);
    let sent = edge(&graph, EdgeKind::Message, "session:s1", ORCHESTRATOR_NODE).unwrap();
    assert_eq!(
        sent.label.as_deref(),
        Some("hello"),
        "by its name, with the summary"
    );
    assert!(
        graph.node(&external_node("boss")).is_none(),
        "not a stranger"
    );
}

#[test]
fn a_message_to_something_known_but_not_drawn_is_left_out() {
    let mut facts = facts();
    facts
        .messages
        .push(message("s1", "headwork-old", None, "T06"));
    facts.received.push(received("s3", "headwork-lead", "T04"));
    let graph = build(&facts, None, Scope::Live);
    assert!(
        graph.node(&external_node("headwork-old")).is_none(),
        "a known task is no stranger"
    );
    let graph = build(&facts, None, Scope::All);
    assert!(
        edge(&graph, EdgeKind::Message, "session:s1", "session:s3").is_some(),
        "drawn when its task is"
    );
}

#[test]
fn a_name_taken_again_points_at_the_task_the_view_draws() {
    let mut facts = facts();
    facts.tasks.push(task(
        9,
        "headwork-old",
        TaskState::Working,
        None,
        false,
        "T06",
    ));
    facts.sessions.push(session(
        "s9",
        Some(9),
        Role::Worker,
        "headwork-old",
        "T06",
        None,
    ));
    facts
        .messages
        .push(message("s1", "stranger-free", None, "T07"));
    facts.received.push(received("s9", "headwork-lead", "T07"));
    facts.received.push(received("s3", "headwork-lead", "T04"));
    let now = build(&facts, None, Scope::Live);
    assert!(
        edge(&now, EdgeKind::Message, "session:s1", "session:s9").is_some(),
        "the new task's session"
    );
    let then = build(&facts, Some("T04"), Scope::All);
    assert!(
        edge(&then, EdgeKind::Message, "session:s1", "session:s3").is_some(),
        "the old one at its time"
    );
    assert!(
        task_named(&facts, "headwork-old", None).is_some_and(|found| found.id == 9),
        "now: the live one"
    );
    assert!(
        task_named(&facts, "headwork-old", Some("T04")).is_some_and(|found| found.id == 3),
        "then: the old one"
    );
    assert!(
        task_named(&facts, "headwork-lead", Some("T00")).is_none(),
        "not queued yet"
    );
}

#[test]
fn a_subagent_id_never_takes_a_session_or_task_name() {
    let mut facts = facts();
    facts
        .agents
        .push(agent("headwork-lead", "s2", Some("T05"), None));
    facts
        .received
        .push(received("s1", "headwork-lead-review", "T07"));
    facts
        .messages
        .push(message("s2", "headwork-lead", Some("third"), "T07"));
    let graph = build(&facts, None, Scope::Live);
    assert!(
        edge(
            &graph,
            EdgeKind::Message,
            "session:s2",
            "agent:headwork-lead"
        )
        .is_none(),
        "not the subagent"
    );
    assert_eq!(
        edge(&graph, EdgeKind::Message, "session:s2", "session:s1")
            .unwrap()
            .label
            .as_deref(),
        Some("third"),
        "the session"
    );
}

#[test]
fn a_message_to_a_subagent_reaches_it() {
    let mut facts = facts();
    facts.messages.push(message("s1", "a1", None, "T06"));
    let graph = build(&facts, None, Scope::Live);
    assert!(
        edge(&graph, EdgeKind::Message, "session:s1", "agent:a1").is_some(),
        "by its id"
    );
}

#[test]
fn a_helpers_subagents_and_messages_hang_off_the_helper() {
    let mut facts = facts();
    facts.agents.push(agent("a7", "h1", Some("T06"), None));
    facts.messages.push(message("h1", "stranger", None, "T06"));
    let graph = build(&facts, None, Scope::Live);
    assert!(
        edge(&graph, EdgeKind::Called, "helper:h1", "agent:a7").is_some(),
        "its subagent"
    );
    assert_eq!(
        graph.node("agent:a7").unwrap().parent.as_deref(),
        Some("task:1"),
        "in the task's box"
    );
    assert!(
        edge(
            &graph,
            EdgeKind::Message,
            "helper:h1",
            &external_node("stranger")
        )
        .is_some(),
        "its message"
    );
}

#[test]
fn a_task_the_user_spawned_is_spawned_from_the_user() {
    let mut facts = facts();
    facts.tasks[0].created_by = Actor::User;
    let graph = build(&facts, None, Scope::Live);
    assert!(
        edge(&graph, EdgeKind::Spawned, USER_NODE, "task:1").is_some(),
        "from the user"
    );
    assert!(
        edge(&graph, EdgeKind::Spawned, ORCHESTRATOR_NODE, "task:1").is_none(),
        "not the orchestrator"
    );
}

#[test]
fn a_live_part_brings_its_closed_lead() {
    let mut facts = facts();
    facts.tasks[0].state = TaskState::Closed;
    let graph = build(&facts, None, Scope::Live);
    assert!(graph.node("task:1").is_some(), "the lead is drawn");
    assert_eq!(
        graph.node("task:2").unwrap().parent.as_deref(),
        Some("task:1"),
        "in its box"
    );
}

#[test]
fn a_task_scope_leaves_out_the_orchestrators_subagents_and_asks() {
    let mut facts = facts();
    facts.agents.push(agent("a2", "o1", Some("T01"), None));
    facts.asks.push(AskFact {
        id: 6,
        task: None,
        session: Some("o1".to_owned()),
        header: Some("Spawn".to_owned()),
        question: "Go?".to_owned(),
        state: AskState::Open,
        created_at: "T02".to_owned(),
        closed_at: None,
    });
    let live = build(&facts, None, Scope::Live);
    assert!(
        live.node("agent:a2").is_some() && live.node("ask:6").is_some(),
        "live draws them"
    );
    let one = build(&facts, None, Scope::Task(1));
    assert!(
        one.node("agent:a2").is_none() && one.node("ask:6").is_none(),
        "task scope leaves them out"
    );
    assert!(one.node("agent:a1").is_some(), "its own subagent stays");
}

#[test]
fn the_live_view_holds_only_subagents_that_still_run() {
    let mut facts = facts();
    facts.agents.push(AgentFact {
        finished: Some(true),
        ..agent("a8", "s1", Some("T05"), Some("T06"))
    });
    facts.agents.push(AgentFact {
        finished: Some(false),
        ..agent("a9", "s1", Some("T05"), None)
    });
    facts
        .agents
        .push(agent("a10", "s3", Some("T01"), Some("T02")));
    let live = build(&facts, None, Scope::Live);
    assert!(live.node("agent:a8").is_none(), "finished");
    assert_eq!(
        state(&live, "agent:a9"),
        Some("running"),
        "its meta says it runs"
    );
    let all = build(&facts, None, Scope::All);
    assert_eq!(state(&all, "agent:a8"), Some("done"), "all keeps it, done");
    assert!(
        all.node("agent:a10").is_some(),
        "all keeps an ended caller's subagent"
    );
}

#[test]
fn a_subagent_without_times_shows_only_now() {
    let mut facts = facts();
    facts.agents[0].first_at = None;
    facts.agents[0].last_at = None;
    assert_eq!(
        state(&build(&facts, None, Scope::Live), "agent:a1"),
        Some("seen"),
        "now"
    );
    assert!(
        build(&facts, Some("T09"), Scope::Live)
            .node("agent:a1")
            .is_none(),
        "never in a past view"
    );
}

#[test]
fn a_nested_subagent_is_called_by_its_parent_agent_of_the_same_session() {
    let mut facts = facts();
    facts.agents.push(AgentFact {
        depth: Some(2),
        parent: Some("a1".to_owned()),
        ..agent("a2", "s1", Some("T05"), None)
    });
    let graph = build(&facts, None, Scope::Live);
    assert!(
        edge(&graph, EdgeKind::Called, "agent:a1", "agent:a2").is_some(),
        "from its parent"
    );
    assert!(
        edge(&graph, EdgeKind::Called, "session:s1", "agent:a2").is_none(),
        "not from the session"
    );
    let fallbacks = [
        ("gone", "s1", None, "an unknown parent"),
        ("a1", "s2", None, "a parent of another session"),
        ("a1", "s1", Some(()), "a parent not drawn"),
    ];
    for (parent, session_id, hide_parent, case) in fallbacks {
        let mut facts = facts.clone();
        facts.agents[1].parent = Some(parent.to_owned());
        facts.agents[1].session = session_id.to_owned();
        if hide_parent.is_some() {
            facts.agents[0].first_at = None;
        }
        let graph = build(&facts, Some("T09"), Scope::All);
        let caller = format!("session:{session_id}");
        assert!(
            edge(&graph, EdgeKind::Called, &caller, "agent:a2").is_some(),
            "{case}: from the session"
        );
    }
}

#[test]
fn a_resumed_task_shows_its_newest_session() {
    let mut facts = facts();
    facts.sessions[1].ended_at = Some("T04".to_owned());
    facts.sessions.push(session(
        "s1b",
        Some(1),
        Role::Worker,
        "headwork-lead",
        "T04",
        None,
    ));
    let graph = build(&facts, None, Scope::All);
    assert!(
        graph.node("session:s1").is_none(),
        "the older session folds into the newest"
    );
    let node = graph.node("session:s1b").unwrap();
    assert_eq!(
        node.details.get("earlier_sessions").map(String::as_str),
        Some("s1"),
        "listed in details"
    );
    assert!(
        edge(&graph, EdgeKind::Called, "session:s1b", "agent:a1").is_some(),
        "the older session's agents"
    );
}

#[test]
fn session_states_are_the_stored_word_now_and_live_or_ended_then() {
    let mut facts = facts();
    facts.sessions[1].state = SessionState::Starting;
    assert_eq!(
        state(&build(&facts, None, Scope::Live), "session:s1"),
        Some("starting"),
        "now"
    );
    assert_eq!(
        state(&build(&facts, Some("T03"), Scope::Live), "session:s1"),
        Some("live"),
        "then"
    );
}

#[test]
fn an_external_node_id_never_holds_the_address() {
    let mut facts = facts();
    let address = "<img src=x onerror=alert(1)>/../../x";
    facts.messages.push(message("s1", address, None, "T06"));
    let graph = build(&facts, None, Scope::Live);
    let node = graph
        .nodes
        .iter()
        .find(|node| node.label == address)
        .unwrap();
    assert_eq!(node.id, external_node(address), "a stable id");
    let hash = node.id.strip_prefix("external:").unwrap();
    assert!(
        hash.len() == 16 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "only a hash: {}",
        node.id
    );
    assert_ne!(
        external_node("a"),
        external_node("b"),
        "different addresses, different ids"
    );
}

#[test]
fn skipped_events_are_carried_to_the_graph() {
    let mut facts = facts();
    facts
        .skipped
        .push("event 9: unknown task state `sleeping`".to_owned());
    assert_eq!(
        build(&facts, None, Scope::Live).skipped,
        facts.skipped,
        "carried"
    );
}

#[test]
fn words_serialise_as_the_kinds_and_states_read() {
    for kind in NodeKind::ALL {
        assert_eq!(
            serde_json::to_value(kind).unwrap(),
            kind.as_str(),
            "{kind:?}"
        );
    }
    for kind in EdgeKind::ALL {
        assert_eq!(
            serde_json::to_value(kind).unwrap(),
            kind.as_str(),
            "{kind:?}"
        );
    }
    assert_eq!(
        serde_json::to_value(Status::ReviewNode(NodeState::Running)).unwrap(),
        "running",
        "a state word"
    );
    let kinds: BTreeSet<&str> = NodeKind::ALL.iter().map(|kind| kind.as_str()).collect();
    assert!(
        kinds.contains("review-node") && kinds.contains("subagent"),
        "{kinds:?}"
    );
}
