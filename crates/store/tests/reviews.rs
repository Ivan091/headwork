//! Reviews in the store: requests, the state machine through `move_review`, node runs and findings.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;

use headwork_core::review::{
    Flow, Instance, Level, NodeId, ReviewRefusal, ReviewTrigger, members, ready, round_complete,
};
use headwork_core::{
    Actor, Config, FindingVerdict, NodeState, PermissionMode, ProjectName, Resolution, ReviewState,
    Severity, Slug, TaskRequest,
};
use headwork_store::{EventFilter, NewFinding, NewTask, ReviewError, Store, TaskId};

fn store_with_task() -> (Store, TaskId) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/config.toml");
    let config = Config::parse(&std::fs::read_to_string(path).unwrap(), "config.toml").unwrap();
    let mut store = Store::open_in_memory().unwrap();
    store.sync_projects(&config).unwrap();
    let request = NewTask {
        request: TaskRequest {
            project: ProjectName::new("me-sim").unwrap(),
            slug: Slug::new("feature").unwrap(),
            kind: "scene".to_owned(),
            model: None,
            effort: None,
            in_main: false,
            keep: false,
            close: headwork_core::CloseMode::Confirm,
            review: None,
            decision: None,
        },
        brief_path: "briefs/me-sim-feature.md".to_owned(),
        depends_on: None,
        permission_mode: PermissionMode::Auto,
        shape: headwork_core::TaskShape::Lead,
    };
    let task = store
        .create_task(&config, &request, Actor::Orchestrator)
        .unwrap();
    (store, task.id)
}

fn code() -> Flow {
    Flow::parse(include_str!("../../../flows/code.toml")).unwrap()
}

const fn finding(summary: &str) -> NewFinding<'_> {
    NewFinding {
        file: Some("src/lib.rs"),
        line: Some(7),
        severity: Severity::Important,
        summary,
        scenario: "x",
    }
}

#[test]
fn a_task_holds_one_open_review_at_a_time() {
    let (mut store, task) = store_with_task();
    let review = store
        .request_review(task, "code", Level::Standard, "the park path", Actor::Hook)
        .unwrap();
    assert_eq!(
        review.state,
        ReviewState::Requested,
        "a new review waits to be started"
    );
    assert_eq!(
        (review.round, review.level, review.focus.as_str()),
        (1, Level::Standard, "the park path"),
        "{review:?}"
    );
    let again = store.request_review(task, "code", Level::Light, "", Actor::Hook);
    assert!(
        matches!(again, Err(ReviewError::Running { id, state: ReviewState::Requested }) if id == review.id),
        "{again:?}"
    );
    assert_eq!(
        store.open_review(task).unwrap().map(|row| row.id),
        Some(review.id),
        "found from the task"
    );
    let events = store
        .events(EventFilter {
            task: Some(task),
            limit: 100,
        })
        .unwrap();
    assert!(
        events.iter().any(|event| event.kind == "review-requested"),
        "the request is logged"
    );
    store
        .move_review(review.id, ReviewTrigger::Cancel, false, false, Actor::User)
        .unwrap();
    assert!(
        store.open_review(task).unwrap().is_none(),
        "a cancelled review is not open"
    );
    let next = store
        .request_review(task, "code", Level::Light, "", Actor::Hook)
        .unwrap();
    assert_eq!(
        store.reviews_of(task).unwrap().len(),
        2,
        "both reviews are kept"
    );
    assert_ne!(next.id, review.id, "a new row");
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one review walked from request to finish"
)]
fn a_level_two_review_runs_its_nodes_verifies_and_finishes() {
    let (mut store, task) = store_with_task();
    let flow = code();
    let review = store
        .request_review(task, "code", Level::Standard, "", Actor::Hook)
        .unwrap();
    let review = store
        .move_review(review.id, ReviewTrigger::Start, false, false, Actor::Worker)
        .unwrap();
    let review = store
        .set_review_target(review.id, None, "base", "head")
        .unwrap();
    assert_eq!(
        review.head_sha.as_deref(),
        Some("head"),
        "the commit under review is pinned"
    );
    assert_eq!(
        review.reviewed_sha, None,
        "only the start (with its part) pins what round 1 reviews; a review without it keeps later rounds whole"
    );

    let first = ready(
        &flow,
        Level::Standard,
        &store.review_progress(&review).unwrap(),
    );
    store.take_review_nodes(&review, &first).unwrap();
    assert!(
        ready(
            &flow,
            Level::Standard,
            &store.review_progress(&review).unwrap()
        )
        .is_empty(),
        "taken nodes are not ready again"
    );
    let early = store.move_review(
        review.id,
        ReviewTrigger::Deliver,
        false,
        false,
        Actor::Worker,
    );
    assert!(
        matches!(
            early,
            Err(ReviewError::Refused {
                refusal: ReviewRefusal::NodesOpen,
                ..
            })
        ),
        "{early:?}"
    );

    let mut ids = Vec::new();
    for instance in &first {
        let findings: Vec<NewFinding<'_>> = if instance.node.as_str() == "correctness" {
            vec![finding("off by one"), finding("wrong default")]
        } else {
            Vec::new()
        };
        ids.extend(
            store
                .finish_review_node(
                    &review,
                    instance,
                    NodeState::Done,
                    &findings,
                    &[],
                    Actor::Worker,
                )
                .unwrap(),
        );
    }
    let verify = ready(
        &flow,
        Level::Standard,
        &store.review_progress(&review).unwrap(),
    );
    assert_eq!(
        verify
            .iter()
            .map(|instance| instance.item)
            .collect::<Vec<_>>(),
        [ids[0]],
        "one verifier for correctness's findings at level 2"
    );
    let progress = store.review_progress(&review).unwrap();
    assert_eq!(
        members(&flow, Level::Standard, &progress, &verify[0]),
        ids,
        "it checks both"
    );
    assert_eq!(
        progress.files.get(&ids[0]),
        Some(&Some("src/lib.rs".to_owned())),
        "the progress knows each finding's file"
    );
    for (id, verdict) in ids
        .iter()
        .zip([FindingVerdict::Confirmed, FindingVerdict::Rejected])
    {
        store.set_finding_verdict(*id, verdict).unwrap();
    }
    store
        .finish_review_node(
            &review,
            &verify[0],
            NodeState::Done,
            &[],
            &[],
            Actor::Worker,
        )
        .unwrap();
    assert!(
        round_complete(
            &flow,
            Level::Standard,
            &store.review_progress(&review).unwrap()
        ),
        "the round is complete"
    );
    let review = store
        .move_review(
            review.id,
            ReviewTrigger::Deliver,
            true,
            false,
            Actor::Worker,
        )
        .unwrap();
    assert_eq!(review.state, ReviewState::Delivered, "delivered");

    let findings = store.review_findings(review.id).unwrap();
    let passed: Vec<i64> = findings
        .iter()
        .filter(|f| f.verdict.is_none_or(FindingVerdict::passes))
        .map(|f| f.id)
        .collect();
    assert_eq!(passed, [ids[0]], "only the confirmed finding goes on");
    store
        .resolve_finding(passed[0], Resolution::Open, None, false)
        .unwrap();
    assert_eq!(
        store.delivered_findings(task).unwrap(),
        1,
        "a delivered finding counts against the task's cap"
    );
    let refused = store.move_review(
        review.id,
        ReviewTrigger::Resolve,
        true,
        false,
        Actor::Worker,
    );
    assert!(
        matches!(
            refused,
            Err(ReviewError::Refused {
                refusal: ReviewRefusal::FindingsOpen(1),
                ..
            })
        ),
        "{refused:?}"
    );

    store
        .resolve_finding(passed[0], Resolution::Fixed, Some("fixed in abc"), false)
        .unwrap();
    let review = store
        .move_review(
            review.id,
            ReviewTrigger::Resolve,
            true,
            false,
            Actor::Worker,
        )
        .unwrap();
    assert_eq!(
        review.state,
        ReviewState::Rechecking,
        "the fix waits on its re-check"
    );
    assert_eq!(
        store.finding_counts(&review).unwrap().awaiting_verdict,
        1,
        "one fix to re-check"
    );
    store
        .resolve_finding(passed[0], Resolution::FixAccepted, None, false)
        .unwrap();
    let counts = store.finding_counts(&review).unwrap();
    assert_eq!(
        (
            counts.awaiting_verdict,
            counts.fixed,
            counts.fixed_important
        ),
        (0, 1, 1),
        "{counts:?}"
    );
    let review = store
        .move_review(
            review.id,
            ReviewTrigger::Verdict,
            true,
            false,
            Actor::Worker,
        )
        .unwrap();
    let review = store
        .move_review(review.id, ReviewTrigger::Finish, true, false, Actor::Worker)
        .unwrap();
    assert_eq!(review.state, ReviewState::Done, "finished");
    assert!(review.finished_at.is_some(), "the finish time is kept");
    let row = &store.review_findings(review.id).unwrap()[0];
    assert_eq!(
        (row.resolution, row.note.as_deref()),
        (Some(Resolution::FixAccepted), Some("fixed in abc")),
        "{row:?}"
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one review walked through two rounds"
)]
fn a_due_round_starts_round_two_with_fresh_nodes() {
    let (mut store, task) = store_with_task();
    let review = store
        .request_review(task, "code", Level::Strict, "", Actor::Hook)
        .unwrap();
    let pinned = store
        .set_review_target(review.id, Some(task), "base", "head1")
        .unwrap();
    assert_eq!(
        (
            pinned.reviewed_sha.as_deref(),
            pinned.round_base_sha.as_deref()
        ),
        (Some("head1"), None),
        "the start pins the head round 1 reviews"
    );
    store
        .move_review(review.id, ReviewTrigger::Start, false, false, Actor::Worker)
        .unwrap();
    let review = store.review(review.id).unwrap().unwrap();
    assert_eq!(
        (
            review.reviewed_sha.as_deref(),
            review.round_base_sha.as_deref()
        ),
        (Some("head1"), None),
        "starting is no new round"
    );
    let correctness = Instance {
        node: NodeId::new("correctness").unwrap(),
        item: 0,
    };
    let ids = store
        .finish_review_node(
            &review,
            &correctness,
            NodeState::Done,
            &[finding("race")],
            &[],
            Actor::Worker,
        )
        .unwrap();
    store
        .move_review(
            review.id,
            ReviewTrigger::Deliver,
            true,
            false,
            Actor::Worker,
        )
        .unwrap();
    store
        .resolve_finding(ids[0], Resolution::Fixed, None, false)
        .unwrap();
    let resolved = store
        .set_review_target(review.id, None, "base", "head2")
        .unwrap();
    assert_eq!(
        (
            resolved.head_sha.as_deref(),
            resolved.reviewed_sha.as_deref()
        ),
        (Some("head2"), Some("head1")),
        "a resolve moves the head, not what round 1 reviewed"
    );
    store
        .move_review(
            review.id,
            ReviewTrigger::Resolve,
            true,
            false,
            Actor::Worker,
        )
        .unwrap();
    store
        .resolve_finding(ids[0], Resolution::FixAccepted, None, false)
        .unwrap();
    let review = store
        .move_review(review.id, ReviewTrigger::Verdict, true, true, Actor::Worker)
        .unwrap();
    assert_eq!(
        (review.state, review.round),
        (ReviewState::Running, 2),
        "round two runs"
    );
    assert_eq!(
        (
            review.round_base_sha.as_deref(),
            review.reviewed_sha.as_deref()
        ),
        (Some("head1"), Some("head2")),
        "round two reviews the fix commits head1..head2"
    );
    assert!(
        store.review_progress(&review).unwrap().instances.is_empty(),
        "round two starts with no node run"
    );
    assert_eq!(
        store.finding_counts(&review).unwrap().fixed,
        0,
        "round two has fixed nothing yet"
    );
}
