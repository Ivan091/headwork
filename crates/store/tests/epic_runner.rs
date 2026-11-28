//! The epic runner in the store (`docs/backlog.md`, section 13): the facts of an epic'db step, the claim of a child, the
//! re-plan child with its `epic_replans` rows and the churn block.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use headwork_core::backlog::{
    BacklogActor, BacklogDraft, BacklogItem, BacklogStatus, BlockReason, ItemKind, Target, parse,
};
use headwork_core::{Actor, EXPLORE_KIND};
use headwork_store::{
    BacklogMove, EVENT_EPIC_CHURN, EVENT_EPIC_REPLAN, EpicClaim, ItemPlan, ItemShape, NewReplan,
    ReplanRow, Store,
};

fn store() -> Store {
    Store::open_in_memory().unwrap()
}

fn draft(text: &str) -> BacklogDraft {
    let mut draft = parse(&format!("## demo\n\n- [ ] {text}\n")).items.remove(0);
    draft.status = BacklogStatus::Backlog;
    draft
}

fn shaped(store: &mut Store, text: &str, shape: &ItemShape) -> BacklogItem {
    store
        .add_backlog_item_shaped(
            &draft(text),
            Actor::Orchestrator,
            BacklogActor::Orchestrator,
            None,
            shape,
        )
        .unwrap()
}

fn epic(store: &mut Store) -> i64 {
    let id = shaped(
        store,
        "The epic",
        &ItemShape {
            kind: Some(ItemKind::Epic),
            ..ItemShape::default()
        },
    )
    .id;
    let start = BacklogMove::new(Target::InProgress, BacklogActor::User, Actor::User, true);
    store.move_backlog_item(id, &start).unwrap();
    id
}

fn child(store: &mut Store, epic: i64, text: &str, kind: &str, after: &[i64]) -> i64 {
    shaped(
        store,
        text,
        &ItemShape {
            parent: Some(epic),
            after: after.to_vec(),
            plan: ItemPlan {
                task_kind: Some(kind.to_owned()),
                ..ItemPlan::default()
            },
            ..ItemShape::default()
        },
    )
    .id
}

const fn claimed(claim: &EpicClaim) -> bool {
    matches!(claim, EpicClaim::Claimed(_))
}

fn lose(store: &mut Store, id: i64) {
    let lost = BacklogMove::new(
        Target::Blocked {
            reason: BlockReason::TaskLost,
            note: Some("task demo-x failed".to_owned()),
        },
        BacklogActor::Headwork,
        Actor::Tick,
        true,
    );
    store.move_backlog_item(id, &lost).unwrap();
}

fn replan_shape(epic: i64) -> ItemShape {
    ItemShape {
        parent: Some(epic),
        plan: ItemPlan {
            task_kind: Some(EXPLORE_KIND.to_owned()),
            brief: Some("Goal: re-plan".to_owned()),
            ..ItemPlan::default()
        },
        ..ItemShape::default()
    }
}

#[test]
fn a_claim_moves_a_backlog_child_once_and_honours_max_parallel() {
    let mut db = store();
    let ep = epic(&mut db);
    let a = child(&mut db, ep, "A", "code", &[]);
    let b = child(&mut db, ep, "B", "code", &[a]);
    let c = child(&mut db, ep, "C", "code", &[]);
    let claim = db.claim_epic_child(a, 1, Actor::Tick).unwrap();
    let EpicClaim::Claimed(item) = claim else {
        unreachable!("{claim:?}")
    };
    assert_eq!(item.draft.status, BacklogStatus::InProgress);
    assert!(item.task.is_none(), "no task yet");
    assert!(
        !claimed(&db.claim_epic_child(a, 1, Actor::Tick).unwrap()),
        "a second claim finds it in progress"
    );
    assert!(
        !claimed(&db.claim_epic_child(c, 1, Actor::Tick).unwrap()),
        "max_parallel 1 is reached"
    );
    assert!(
        !claimed(&db.claim_epic_child(b, 0, Actor::Tick).unwrap()),
        "b waits for a"
    );
    assert!(
        claimed(&db.claim_epic_child(c, 0, Actor::Tick).unwrap()),
        "0 is no limit"
    );
    let relink = BacklogMove {
        link: None,
        ..BacklogMove::new(
            Target::InProgress,
            BacklogActor::Headwork,
            Actor::Tick,
            true,
        )
    };
    assert!(
        db.check_backlog_move(a, &relink).is_ok(),
        "headwork may relink the claimed child to its task"
    );
}

#[test]
fn an_explore_child_is_claimed_beside_others_but_never_beside_another_explore_child() {
    let mut db = store();
    let ep = epic(&mut db);
    let a = child(&mut db, ep, "A", "code", &[]);
    let x = child(&mut db, ep, "X", EXPLORE_KIND, &[]);
    let y = child(&mut db, ep, "Y", EXPLORE_KIND, &[]);
    assert!(claimed(&db.claim_epic_child(a, 1, Actor::Tick).unwrap()));
    assert!(
        claimed(&db.claim_epic_child(x, 1, Actor::Tick).unwrap()),
        "explore children do not count against max_parallel"
    );
    assert!(!claimed(&db.claim_epic_child(y, 1, Actor::Tick).unwrap()));
}

#[test]
fn a_claim_skips_a_child_whose_epic_is_not_in_progress() {
    let mut db = store();
    let ep = shaped(
        &mut db,
        "Idle epic",
        &ItemShape {
            kind: Some(ItemKind::Epic),
            ..ItemShape::default()
        },
    )
    .id;
    let a = child(&mut db, ep, "A", "code", &[]);
    let claim = db.claim_epic_child(a, 3, Actor::Tick).unwrap();
    assert!(
        matches!(&claim, EpicClaim::Skipped(why) if why.contains("not in progress")),
        "{claim:?}"
    );
    assert_eq!(
        db.backlog_item(a).unwrap().draft.status,
        BacklogStatus::Backlog
    );
}

#[test]
fn the_facts_carry_kinds_projects_block_causes_and_counted_re_plans() {
    let mut db = store();
    let ep = epic(&mut db);
    let a = child(&mut db, ep, "A", "code", &[]);
    let x = child(&mut db, ep, "X", EXPLORE_KIND, &[]);
    assert!(claimed(&db.claim_epic_child(a, 3, Actor::Tick).unwrap()));
    lose(&mut db, a);
    let facts = db.epic_facts(ep, |project| project == "demo").unwrap();
    assert_eq!(facts.epic, ep);
    assert!(!facts.explorer_live);
    assert_eq!(facts.replans, 0);
    let ids: Vec<(i64, bool, bool)> = facts
        .children
        .iter()
        .map(|child| (child.id, child.explore, child.has_project))
        .collect();
    assert_eq!(ids, vec![(a, false, true), (x, true, true)]);
    let lost = &facts.children[0];
    assert!(
        lost.block_event.is_some(),
        "the move into the block is found"
    );
    assert!(!lost.block_replanned);
    let cause = ReplanRow {
        item: a,
        event: lost.block_event,
    };
    let shape = replan_shape(ep);
    let cancel = BacklogMove::new(Target::Cancelled, BacklogActor::User, Actor::User, true);
    db.move_backlog_item(x, &cancel).unwrap();
    let added = db
        .add_epic_replan(
            &NewReplan {
                epic: ep,
                draft: &draft("Re-plan: lost"),
                shape: &shape,
                reason: "lost",
                causes: &[cause],
            },
            Actor::Tick,
        )
        .unwrap()
        .unwrap();
    assert_eq!(added.parent, Some(ep));
    let facts = db.epic_facts(ep, |_| false).unwrap();
    assert_eq!(facts.replans, 1);
    assert!(facts.children[0].block_replanned, "the cause is recorded");
    assert!(!facts.children[0].has_project);
    assert!(
        db.add_epic_replan(
            &NewReplan {
                epic: ep,
                draft: &draft("Re-plan: again"),
                shape: &shape,
                reason: "again",
                causes: &[],
            },
            Actor::Tick,
        )
        .unwrap()
        .is_none(),
        "an open explore child stops a second re-plan"
    );
    assert!(db.item_event_logged("backlog-moved", a).unwrap());
    assert!(!db.item_event_logged("epic-child-skipped", a).unwrap());
    let events = db
        .events(headwork_store::EventFilter {
            task: None,
            limit: 100,
        })
        .unwrap();
    assert!(events.iter().any(|event| event.kind == EVENT_EPIC_REPLAN
        && event.data["child"] == serde_json::json!(added.id)));
}

#[test]
fn a_block_again_later_is_a_new_cause() {
    let mut db = store();
    let ep = epic(&mut db);
    let a = child(&mut db, ep, "A", "code", &[]);
    assert!(claimed(&db.claim_epic_child(a, 3, Actor::Tick).unwrap()));
    lose(&mut db, a);
    let first = db.epic_facts(ep, |_| true).unwrap().children[0].block_event;
    let shape = replan_shape(ep);
    let replan = db
        .add_epic_replan(
            &NewReplan {
                epic: ep,
                draft: &draft("Re-plan: once"),
                shape: &shape,
                reason: "once",
                causes: &[ReplanRow {
                    item: a,
                    event: first,
                }],
            },
            Actor::Tick,
        )
        .unwrap()
        .unwrap();
    let cancel = BacklogMove::new(Target::Cancelled, BacklogActor::User, Actor::User, true);
    db.move_backlog_item(replan.id, &cancel).unwrap();
    let respawn = BacklogMove::new(Target::InProgress, BacklogActor::User, Actor::User, true);
    db.move_backlog_item(a, &respawn).unwrap();
    lose(&mut db, a);
    let facts = db.epic_facts(ep, |_| true).unwrap();
    assert_ne!(facts.children[0].block_event, first);
    assert!(
        !facts.children[0].block_replanned,
        "a new block is a new cause"
    );
}

#[test]
fn the_churn_block_holds_the_epic_for_a_decision_and_clears_the_count() {
    let mut db = store();
    let ep = epic(&mut db);
    let shape = replan_shape(ep);
    let replan = db
        .add_epic_replan(
            &NewReplan {
                epic: ep,
                draft: &draft("Re-plan: plan complete?"),
                shape: &shape,
                reason: "plan complete?",
                causes: &[],
            },
            Actor::Tick,
        )
        .unwrap()
        .unwrap();
    let cancel = BacklogMove::new(Target::Cancelled, BacklogActor::User, Actor::User, true);
    db.move_backlog_item(replan.id, &cancel).unwrap();
    assert_eq!(db.epic_facts(ep, |_| true).unwrap().replans, 1);
    let blocked = db.block_epic_churn(ep, 1, Actor::Tick).unwrap().unwrap();
    let block = blocked.draft.status.block().unwrap();
    assert_eq!(block.reason, BlockReason::Decision);
    assert_eq!(block.note.as_deref(), Some("epic-churn: 1 re-plans"));
    assert!(
        db.block_epic_churn(ep, 1, Actor::Tick).unwrap().is_none(),
        "a second runner finds it blocked"
    );
    let unblock = BacklogMove::new(Target::Unblock, BacklogActor::User, Actor::User, true);
    let back = db.move_backlog_item(ep, &unblock).unwrap();
    assert_eq!(back.draft.status, BacklogStatus::InProgress);
    assert_eq!(
        db.epic_facts(ep, |_| true).unwrap().replans,
        0,
        "a fresh count"
    );
    let events = db
        .events(headwork_store::EventFilter {
            task: None,
            limit: 100,
        })
        .unwrap();
    assert!(events.iter().any(|event| event.kind == EVENT_EPIC_CHURN));
}
