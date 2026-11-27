//! The backlog status guard and the task-end table (`docs/backlog.md`, section 10), row by row.

use headwork_core::TaskState;
use headwork_core::backlog::{
    BacklogActor as A, BacklogStatus, Block, BlockReason as R, DateEffect, ItemStatus, LinkEffect,
    LinkedEnd, MoveFacts, MoveKind, MoveRefusal, ReturnTo, Target, TaskEndEffect, TaskOutcome,
    on_linked_end, on_task_end, transition,
};

fn blocked(reason: R, before: ReturnTo) -> BacklogStatus {
    BacklogStatus::Blocked(Block {
        reason,
        note: Some("why".to_owned()),
        before,
    })
}

fn block_with(note: &str) -> Target {
    Target::Blocked {
        reason: R::Event,
        note: Some(note.to_owned()),
    }
}

fn block(reason: R) -> Target {
    Target::Blocked {
        reason,
        note: Some("why".to_owned()),
    }
}

const PLAIN: MoveFacts<'static> = MoveFacts {
    live_task: None,
    linked_to_actor: false,
    in_actor_epic: false,
    has_project: true,
    links_another_task: false,
    reason: None,
};

const OWN: MoveFacts<'static> = MoveFacts {
    linked_to_actor: true,
    ..PLAIN
};

const EPIC: MoveFacts<'static> = MoveFacts {
    in_actor_epic: true,
    ..PLAIN
};

const LIVE: MoveFacts<'static> = MoveFacts {
    live_task: Some("headwork-x"),
    ..PLAIN
};

const REASON: MoveFacts<'static> = MoveFacts {
    reason: Some("the user said so"),
    ..PLAIN
};

type Row = (
    Option<BacklogStatus>,
    Target,
    A,
    MoveFacts<'static>,
    BacklogStatus,
    MoveKind,
);

#[expect(
    clippy::too_many_lines,
    reason = "one row per line of the transition table"
)]
fn allowed_rows() -> Vec<Row> {
    use BacklogStatus as S;
    let lost = || blocked(R::TaskLost, ReturnTo::InProgress);
    let partial = || blocked(R::Partial, ReturnTo::InProgress);
    let decided_backlog = || blocked(R::Decision, ReturnTo::Backlog);
    let event_progress = || blocked(R::Event, ReturnTo::InProgress);
    vec![
        (
            None,
            Target::Backlog,
            A::User,
            PLAIN,
            S::Backlog,
            MoveKind::Add,
        ),
        (
            None,
            Target::Backlog,
            A::Orchestrator,
            PLAIN,
            S::Backlog,
            MoveKind::Add,
        ),
        (
            None,
            Target::Backlog,
            A::Explorer,
            PLAIN,
            S::Backlog,
            MoveKind::Add,
        ),
        (
            None,
            Target::Backlog,
            A::Headwork,
            PLAIN,
            S::Backlog,
            MoveKind::Add,
        ),
        (
            None,
            block(R::Depends),
            A::Explorer,
            EPIC,
            blocked(R::Depends, ReturnTo::Backlog),
            MoveKind::Add,
        ),
        (
            Some(S::Backlog),
            Target::InProgress,
            A::User,
            PLAIN,
            S::InProgress,
            MoveKind::Start,
        ),
        (
            Some(S::Backlog),
            Target::InProgress,
            A::Orchestrator,
            PLAIN,
            S::InProgress,
            MoveKind::Start,
        ),
        (
            Some(S::Backlog),
            Target::InProgress,
            A::Headwork,
            PLAIN,
            S::InProgress,
            MoveKind::Start,
        ),
        (
            Some(S::InProgress),
            Target::InProgress,
            A::Orchestrator,
            PLAIN,
            S::InProgress,
            MoveKind::Relink,
        ),
        (
            Some(S::InProgress),
            Target::InProgress,
            A::User,
            PLAIN,
            S::InProgress,
            MoveKind::Relink,
        ),
        (
            Some(S::InProgress),
            Target::Done,
            A::Headwork,
            LIVE,
            S::Done,
            MoveKind::MergeFinish,
        ),
        (
            Some(S::Done),
            Target::Unlink,
            A::Headwork,
            PLAIN,
            S::Done,
            MoveKind::Unlink,
        ),
        (
            Some(lost()),
            Target::Unlink,
            A::Headwork,
            PLAIN,
            lost(),
            MoveKind::Unlink,
        ),
        (
            Some(S::InProgress),
            Target::Done,
            A::Headwork,
            PLAIN,
            S::Done,
            MoveKind::Finish,
        ),
        (
            Some(S::Backlog),
            Target::Done,
            A::User,
            PLAIN,
            S::Done,
            MoveKind::Finish,
        ),
        (
            Some(S::InProgress),
            Target::Done,
            A::Orchestrator,
            PLAIN,
            S::Done,
            MoveKind::Finish,
        ),
        (
            Some(lost()),
            Target::Done,
            A::User,
            PLAIN,
            S::Done,
            MoveKind::Finish,
        ),
        (
            Some(S::InProgress),
            block(R::Decision),
            A::Worker,
            OWN,
            blocked(R::Decision, ReturnTo::InProgress),
            MoveKind::Block,
        ),
        (
            Some(S::InProgress),
            block(R::Event),
            A::Worker,
            OWN,
            event_progress(),
            MoveKind::Block,
        ),
        (
            Some(S::InProgress),
            block(R::TaskLost),
            A::Headwork,
            PLAIN,
            lost(),
            MoveKind::Block,
        ),
        (
            Some(S::InProgress),
            block(R::Partial),
            A::Headwork,
            PLAIN,
            partial(),
            MoveKind::Block,
        ),
        (
            Some(S::InProgress),
            block(R::Event),
            A::Orchestrator,
            LIVE,
            event_progress(),
            MoveKind::Block,
        ),
        (
            Some(S::InProgress),
            block(R::Event),
            A::User,
            PLAIN,
            event_progress(),
            MoveKind::Block,
        ),
        (
            Some(S::Backlog),
            block(R::Depends),
            A::Explorer,
            EPIC,
            blocked(R::Depends, ReturnTo::Backlog),
            MoveKind::Block,
        ),
        (
            Some(S::Backlog),
            block(R::DepCancelled),
            A::Headwork,
            PLAIN,
            blocked(R::DepCancelled, ReturnTo::Backlog),
            MoveKind::Block,
        ),
        (
            Some(S::Backlog),
            block(R::Event),
            A::Orchestrator,
            PLAIN,
            blocked(R::Event, ReturnTo::Backlog),
            MoveKind::Block,
        ),
        (
            Some(S::Backlog),
            block(R::Decision),
            A::Orchestrator,
            PLAIN,
            decided_backlog(),
            MoveKind::Block,
        ),
        (
            Some(S::Backlog),
            block(R::Decision),
            A::User,
            PLAIN,
            decided_backlog(),
            MoveKind::Block,
        ),
        (
            Some(blocked(R::Depends, ReturnTo::Backlog)),
            Target::Unblock,
            A::Headwork,
            PLAIN,
            S::Backlog,
            MoveKind::Unblock,
        ),
        (
            Some(lost()),
            Target::Unblock,
            A::Headwork,
            PLAIN,
            S::InProgress,
            MoveKind::Unblock,
        ),
        (
            Some(event_progress()),
            Target::Unblock,
            A::Worker,
            OWN,
            S::InProgress,
            MoveKind::Unblock,
        ),
        (
            Some(decided_backlog()),
            Target::Unblock,
            A::Orchestrator,
            PLAIN,
            S::Backlog,
            MoveKind::Unblock,
        ),
        (
            Some(decided_backlog()),
            Target::Unblock,
            A::User,
            PLAIN,
            S::Backlog,
            MoveKind::Unblock,
        ),
        (
            Some(lost()),
            Target::InProgress,
            A::Headwork,
            PLAIN,
            S::InProgress,
            MoveKind::Respawn,
        ),
        (
            Some(partial()),
            Target::InProgress,
            A::Orchestrator,
            PLAIN,
            S::InProgress,
            MoveKind::Respawn,
        ),
        (
            Some(lost()),
            Target::InProgress,
            A::User,
            PLAIN,
            S::InProgress,
            MoveKind::Respawn,
        ),
        (
            Some(S::Backlog),
            Target::Cancelled,
            A::User,
            PLAIN,
            S::Cancelled,
            MoveKind::Cancel,
        ),
        (
            Some(S::InProgress),
            Target::Cancelled,
            A::User,
            PLAIN,
            S::Cancelled,
            MoveKind::Cancel,
        ),
        (
            Some(lost()),
            Target::Cancelled,
            A::User,
            PLAIN,
            S::Cancelled,
            MoveKind::Cancel,
        ),
        (
            Some(S::Backlog),
            Target::Cancelled,
            A::Orchestrator,
            REASON,
            S::Cancelled,
            MoveKind::Cancel,
        ),
        (
            Some(S::InProgress),
            Target::Cancelled,
            A::Orchestrator,
            REASON,
            S::Cancelled,
            MoveKind::Cancel,
        ),
        (
            Some(S::Backlog),
            Target::Cancelled,
            A::Explorer,
            EPIC,
            S::Cancelled,
            MoveKind::Cancel,
        ),
        (
            Some(blocked(R::Depends, ReturnTo::Backlog)),
            Target::Cancelled,
            A::Explorer,
            EPIC,
            S::Cancelled,
            MoveKind::Cancel,
        ),
        (
            Some(S::Done),
            Target::Backlog,
            A::User,
            PLAIN,
            S::Backlog,
            MoveKind::Reopen,
        ),
        (
            Some(S::Cancelled),
            Target::Backlog,
            A::User,
            PLAIN,
            S::Backlog,
            MoveKind::Reopen,
        ),
        (
            Some(S::Done),
            Target::Backlog,
            A::Orchestrator,
            PLAIN,
            S::Backlog,
            MoveKind::Reopen,
        ),
        (
            Some(S::Cancelled),
            Target::Backlog,
            A::Orchestrator,
            PLAIN,
            S::Backlog,
            MoveKind::Reopen,
        ),
        (
            Some(S::Done),
            Target::InProgress,
            A::Headwork,
            LIVE,
            S::InProgress,
            MoveKind::Unfinish,
        ),
    ]
}

#[test]
fn every_row_of_the_table_is_allowed_for_its_actor() {
    for (from, to, actor, facts, want, kind) in allowed_rows() {
        let found = transition(from.as_ref(), &to, actor, &facts);
        assert_eq!(
            found.as_ref().map(|step| (&step.to, step.kind)),
            Ok((&want, kind)),
            "{from:?} -> {to:?} by {actor}"
        );
        assert_eq!(
            found.map(|step| step.from).ok().flatten(),
            from.as_ref().map(BacklogStatus::kind),
            "the move names its start"
        );
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one row per line of the transition table"
)]
fn refused_rows() -> Vec<(
    Option<BacklogStatus>,
    Target,
    A,
    MoveFacts<'static>,
    MoveRefusal,
)> {
    use BacklogStatus as S;
    let not = |actor, from: &str, to| MoveRefusal::NotAllowed {
        actor,
        from: from.to_owned(),
        to,
    };
    let task = || "headwork-x".to_owned();
    let lost = || blocked(R::TaskLost, ReturnTo::InProgress);
    vec![
        (
            None,
            Target::Backlog,
            A::Worker,
            OWN,
            not(A::Worker, "new", "backlog"),
        ),
        (
            None,
            Target::Backlog,
            A::Worker,
            PLAIN,
            MoveRefusal::NotLinked,
        ),
        (
            None,
            block(R::Depends),
            A::Explorer,
            PLAIN,
            MoveRefusal::OutsideEpic,
        ),
        (
            None,
            block(R::Depends),
            A::Orchestrator,
            PLAIN,
            not(A::Orchestrator, "new", "blocked"),
        ),
        (
            None,
            Target::Done,
            A::User,
            PLAIN,
            not(A::User, "new", "done"),
        ),
        (
            Some(S::InProgress),
            block(R::Event),
            A::Worker,
            PLAIN,
            MoveRefusal::NotLinked,
        ),
        (
            Some(S::InProgress),
            Target::Done,
            A::Worker,
            PLAIN,
            MoveRefusal::NotLinked,
        ),
        (
            Some(S::Backlog),
            Target::Cancelled,
            A::Explorer,
            PLAIN,
            MoveRefusal::OutsideEpic,
        ),
        (
            Some(S::Backlog),
            Target::InProgress,
            A::Explorer,
            PLAIN,
            MoveRefusal::OutsideEpic,
        ),
        (
            Some(S::InProgress),
            Target::Done,
            A::User,
            LIVE,
            MoveRefusal::FinishedByTask(task()),
        ),
        (
            Some(S::InProgress),
            Target::Done,
            A::Orchestrator,
            LIVE,
            MoveRefusal::FinishedByTask(task()),
        ),
        (
            Some(lost()),
            Target::Done,
            A::Orchestrator,
            LIVE,
            MoveRefusal::FinishedByTask(task()),
        ),
        (
            Some(S::Backlog),
            Target::Done,
            A::Headwork,
            PLAIN,
            not(A::Headwork, "backlog", "done"),
        ),
        (
            Some(S::InProgress),
            Target::Done,
            A::Worker,
            OWN,
            not(A::Worker, "in_progress", "done"),
        ),
        (
            Some(S::InProgress),
            Target::Done,
            A::Explorer,
            EPIC,
            not(A::Explorer, "in_progress", "done"),
        ),
        (
            Some(S::Done),
            Target::Done,
            A::Headwork,
            PLAIN,
            MoveRefusal::Already(ItemStatus::Done),
        ),
        (
            Some(S::Cancelled),
            Target::Done,
            A::User,
            PLAIN,
            not(A::User, "cancelled", "done"),
        ),
        (
            Some(S::Backlog),
            Target::InProgress,
            A::User,
            MoveFacts {
                has_project: false,
                ..PLAIN
            },
            MoveRefusal::NoProject,
        ),
        (
            Some(S::InProgress),
            Target::InProgress,
            A::Orchestrator,
            LIVE,
            MoveRefusal::TaskWorks(task()),
        ),
        (
            Some(lost()),
            Target::InProgress,
            A::Orchestrator,
            LIVE,
            MoveRefusal::TaskWorks(task()),
        ),
        (
            Some(S::Backlog),
            Target::InProgress,
            A::Worker,
            OWN,
            not(A::Worker, "backlog", "in_progress"),
        ),
        (
            Some(blocked(R::Decision, ReturnTo::InProgress)),
            Target::InProgress,
            A::Orchestrator,
            PLAIN,
            not(A::Orchestrator, "blocked", "in_progress"),
        ),
        (
            Some(blocked(R::Event, ReturnTo::Backlog)),
            Target::InProgress,
            A::User,
            PLAIN,
            not(A::User, "blocked", "in_progress"),
        ),
        (
            Some(S::Done),
            Target::InProgress,
            A::Orchestrator,
            PLAIN,
            not(A::Orchestrator, "done", "in_progress"),
        ),
        (
            Some(S::Cancelled),
            Target::InProgress,
            A::User,
            PLAIN,
            not(A::User, "cancelled", "in_progress"),
        ),
        (
            Some(S::InProgress),
            block(R::TaskLost),
            A::Worker,
            OWN,
            MoveRefusal::Reason {
                actor: A::Worker,
                reason: R::TaskLost,
            },
        ),
        (
            Some(S::InProgress),
            block(R::Depends),
            A::Orchestrator,
            PLAIN,
            MoveRefusal::Reason {
                actor: A::Orchestrator,
                reason: R::Depends,
            },
        ),
        (
            Some(S::InProgress),
            block(R::Partial),
            A::User,
            PLAIN,
            MoveRefusal::Reason {
                actor: A::User,
                reason: R::Partial,
            },
        ),
        (
            Some(S::InProgress),
            block(R::Decision),
            A::Headwork,
            PLAIN,
            MoveRefusal::Reason {
                actor: A::Headwork,
                reason: R::Decision,
            },
        ),
        (
            Some(S::Backlog),
            block(R::TaskLost),
            A::Headwork,
            PLAIN,
            MoveRefusal::Reason {
                actor: A::Headwork,
                reason: R::TaskLost,
            },
        ),
        (
            Some(S::Backlog),
            block(R::Event),
            A::Explorer,
            EPIC,
            MoveRefusal::Reason {
                actor: A::Explorer,
                reason: R::Event,
            },
        ),
        (
            Some(S::InProgress),
            block(R::Depends),
            A::Explorer,
            EPIC,
            not(A::Explorer, "in_progress", "blocked"),
        ),
        (
            Some(S::Backlog),
            block(R::Event),
            A::Worker,
            OWN,
            not(A::Worker, "backlog", "blocked"),
        ),
        (
            Some(lost()),
            block(R::Event),
            A::User,
            PLAIN,
            MoveRefusal::Already(ItemStatus::Blocked),
        ),
        (
            Some(S::Done),
            block(R::Event),
            A::User,
            PLAIN,
            not(A::User, "done", "blocked"),
        ),
        (
            Some(S::Cancelled),
            block(R::Event),
            A::Orchestrator,
            PLAIN,
            not(A::Orchestrator, "cancelled", "blocked"),
        ),
        (
            Some(S::Backlog),
            Target::Unblock,
            A::User,
            PLAIN,
            not(A::User, "backlog", "unblocked"),
        ),
        (
            Some(S::InProgress),
            Target::Unblock,
            A::Headwork,
            PLAIN,
            not(A::Headwork, "in_progress", "unblocked"),
        ),
        (
            Some(blocked(R::Depends, ReturnTo::Backlog)),
            Target::Unblock,
            A::Explorer,
            EPIC,
            not(A::Explorer, "blocked", "unblocked"),
        ),
        (
            Some(lost()),
            Target::Unblock,
            A::Worker,
            PLAIN,
            MoveRefusal::NotLinked,
        ),
        (
            Some(S::InProgress),
            Target::Cancelled,
            A::User,
            LIVE,
            MoveRefusal::DropTaskFirst(task()),
        ),
        (
            Some(S::InProgress),
            Target::Cancelled,
            A::Orchestrator,
            MoveFacts {
                reason: Some("the user said so"),
                ..LIVE
            },
            MoveRefusal::DropTaskFirst(task()),
        ),
        (
            Some(S::Backlog),
            Target::Cancelled,
            A::Orchestrator,
            PLAIN,
            MoveRefusal::ReasonRequired,
        ),
        (
            Some(S::Done),
            Target::Cancelled,
            A::User,
            PLAIN,
            MoveRefusal::CancelDone,
        ),
        (
            Some(S::Done),
            Target::Cancelled,
            A::Orchestrator,
            REASON,
            MoveRefusal::CancelDone,
        ),
        (
            Some(S::Cancelled),
            Target::Cancelled,
            A::User,
            PLAIN,
            MoveRefusal::Already(ItemStatus::Cancelled),
        ),
        (
            Some(S::InProgress),
            Target::Cancelled,
            A::Explorer,
            EPIC,
            not(A::Explorer, "in_progress", "cancelled"),
        ),
        (
            Some(S::Backlog),
            Target::Cancelled,
            A::Headwork,
            PLAIN,
            not(A::Headwork, "backlog", "cancelled"),
        ),
        (
            Some(S::Backlog),
            Target::Cancelled,
            A::Worker,
            OWN,
            not(A::Worker, "backlog", "cancelled"),
        ),
        (
            Some(S::Backlog),
            Target::Backlog,
            A::User,
            PLAIN,
            MoveRefusal::Already(ItemStatus::Backlog),
        ),
        (
            Some(S::InProgress),
            Target::Backlog,
            A::Orchestrator,
            PLAIN,
            not(A::Orchestrator, "in_progress", "backlog"),
        ),
        (
            Some(lost()),
            Target::Backlog,
            A::User,
            PLAIN,
            not(A::User, "blocked", "backlog"),
        ),
        (
            Some(S::Done),
            Target::Backlog,
            A::Headwork,
            PLAIN,
            not(A::Headwork, "done", "backlog"),
        ),
        (
            Some(S::Cancelled),
            Target::Backlog,
            A::Explorer,
            EPIC,
            not(A::Explorer, "cancelled", "backlog"),
        ),
        (
            Some(S::Done),
            Target::Backlog,
            A::Worker,
            OWN,
            not(A::Worker, "done", "backlog"),
        ),
        (
            Some(S::Done),
            Target::InProgress,
            A::Headwork,
            PLAIN,
            not(A::Headwork, "done", "in_progress"),
        ),
        (
            Some(S::Done),
            Target::InProgress,
            A::Orchestrator,
            LIVE,
            not(A::Orchestrator, "done", "in_progress"),
        ),
        (
            Some(S::Backlog),
            block_with(
                "a
b",
            ),
            A::User,
            PLAIN,
            MoveRefusal::MultiLine("note"),
        ),
        (
            Some(S::Backlog),
            Target::Cancelled,
            A::User,
            MoveFacts {
                reason: Some(
                    "a
b",
                ),
                ..PLAIN
            },
            MoveRefusal::MultiLine("reason"),
        ),
        (
            Some(S::Backlog),
            Target::Done,
            A::Orchestrator,
            MoveFacts {
                reason: Some(
                    "one
two",
                ),
                ..PLAIN
            },
            MoveRefusal::MultiLine("reason"),
        ),
        (
            Some(S::Backlog),
            block_with("see [docs]"),
            A::Orchestrator,
            PLAIN,
            MoveRefusal::Bracket("note"),
        ),
        (
            Some(S::Backlog),
            Target::Done,
            A::User,
            MoveFacts {
                reason: Some("fixed :)"),
                ..PLAIN
            },
            MoveRefusal::Parentheses("note"),
        ),
        (
            Some(S::Backlog),
            Target::Done,
            A::User,
            MoveFacts {
                reason: Some("see (later"),
                ..PLAIN
            },
            MoveRefusal::Parentheses("note"),
        ),
        (
            Some(S::Done),
            Target::InProgress,
            A::Headwork,
            MoveFacts {
                links_another_task: true,
                ..LIVE
            },
            MoveRefusal::TaskWorks("headwork-x".to_owned()),
        ),
        (
            Some(S::Done),
            Target::InProgress,
            A::Headwork,
            MoveFacts {
                has_project: false,
                ..LIVE
            },
            MoveRefusal::NoProject,
        ),
        (
            Some(S::Done),
            Target::Unlink,
            A::Headwork,
            LIVE,
            not(A::Headwork, "done", "unlinked"),
        ),
        (
            Some(S::InProgress),
            Target::Unlink,
            A::Headwork,
            PLAIN,
            not(A::Headwork, "in_progress", "unlinked"),
        ),
        (
            Some(S::Done),
            Target::Unlink,
            A::User,
            PLAIN,
            not(A::User, "done", "unlinked"),
        ),
        (
            Some(S::Backlog),
            Target::Cancelled,
            A::Orchestrator,
            MoveFacts {
                reason: Some("   "),
                ..PLAIN
            },
            MoveRefusal::ReasonRequired,
        ),
    ]
}

#[test]
fn every_refused_move_says_why() {
    for (from, to, actor, facts, want) in refused_rows() {
        assert_eq!(
            transition(from.as_ref(), &to, actor, &facts),
            Err(want.clone()),
            "{from:?} -> {to:?} by {actor}"
        );
        assert!(!want.to_string().is_empty(), "{want:?} has a message");
    }
}

#[test]
fn the_refusals_read_as_sentences() {
    assert_eq!(
        MoveRefusal::DropTaskFirst("headwork-x".to_owned()).to_string(),
        "task headwork-x works on it: drop the task first, then cancel the item"
    );
    assert_eq!(
        MoveRefusal::FinishedByTask("headwork-x".to_owned()).to_string(),
        "task headwork-x works on it: only headwork finishes it, when the task finishes"
    );
    assert_eq!(
        MoveRefusal::NotAllowed {
            actor: A::Worker,
            from: "done".to_owned(),
            to: "backlog",
        }
        .to_string(),
        "the worker may not move an item from done to backlog"
    );
}

#[test]
fn a_block_keeps_its_note_and_where_it_came_from() {
    let step = transition(
        Some(&BacklogStatus::Backlog),
        &Target::Blocked {
            reason: R::Event,
            note: Some("the release".to_owned()),
        },
        A::Orchestrator,
        &PLAIN,
    )
    .ok()
    .and_then(|step| step.to.block().cloned());
    assert_eq!(
        step,
        Some(Block {
            reason: R::Event,
            note: Some("the release".to_owned()),
            before: ReturnTo::Backlog,
        })
    );
}

#[test]
fn the_status_reads_back_from_its_columns() {
    let row = |status, reason, note: Option<&str>, before| {
        BacklogStatus::from_columns(status, reason, note.map(str::to_owned), before)
    };
    assert_eq!(row("backlog", None, None, None), Ok(BacklogStatus::Backlog));
    assert_eq!(
        row("in_progress", None, None, None),
        Ok(BacklogStatus::InProgress)
    );
    assert_eq!(
        row("dropped", None, None, None),
        Ok(BacklogStatus::Cancelled)
    );
    assert_eq!(
        row(
            "blocked",
            Some("task_lost"),
            Some("task t dropped"),
            Some("in_progress")
        ),
        Ok(BacklogStatus::Blocked(Block {
            reason: R::TaskLost,
            note: Some("task t dropped".to_owned()),
            before: ReturnTo::InProgress,
        }))
    );
    assert!(
        row("blocked", None, None, Some("backlog")).is_err(),
        "a block needs its reason"
    );
    assert!(
        row("blocked", Some("event"), None, None).is_err(),
        "a block needs its return"
    );
    assert!(
        row("open", None, None, None).is_err(),
        "the old word open is no status"
    );
}

#[test]
fn the_task_end_table() {
    let urls = ["u/1".to_owned(), "u/2".to_owned()];
    let finish = |note: &str| {
        Some(TaskEndEffect::Finish {
            note: note.to_owned(),
        })
    };
    let lost = |note: &str| {
        Some(TaskEndEffect::Lost {
            note: note.to_owned(),
        })
    };
    let cases = [
        (
            TaskState::Merged,
            &urls[..],
            finish("task t merged u/1, u/2"),
        ),
        (TaskState::Merged, &[][..], finish("task t merged")),
        (
            TaskState::Closed,
            &urls[..],
            finish("task t merged u/1, u/2"),
        ),
        (
            TaskState::Closed,
            &[][..],
            finish("task t, no pull request"),
        ),
        (TaskState::Dropped, &[][..], lost("task t dropped")),
        (TaskState::Failed, &[][..], lost("task t failed")),
        (
            TaskState::Failed,
            &urls[..],
            finish("task t merged u/1, u/2"),
        ),
        (
            TaskState::Dropped,
            &urls[..],
            finish("task t merged u/1, u/2"),
        ),
        (TaskState::Queued, &[][..], None),
        (TaskState::Spawning, &[][..], None),
        (TaskState::Working, &[][..], None),
        (TaskState::NeedsUser, &[][..], None),
        (TaskState::InReview, &urls[..], None),
        (TaskState::Closing, &[][..], None),
        (TaskState::Interrupted, &[][..], None),
        (TaskState::Parking, &[][..], None),
        (TaskState::Parked, &[][..], None),
    ];
    for (state, merged, want) in cases {
        assert_eq!(
            on_task_end(state, TaskOutcome::Done, merged, "t"),
            want,
            "{state}"
        );
    }
}

#[test]
fn each_task_end_effect_is_a_move_headwork_may_make() {
    for effect in [
        TaskEndEffect::Finish {
            note: "n".to_owned(),
        },
        TaskEndEffect::Lost {
            note: "n".to_owned(),
        },
    ] {
        let live = MoveFacts {
            live_task: Some("t"),
            ..PLAIN
        };
        assert!(
            transition(
                Some(&BacklogStatus::InProgress),
                &effect.target(),
                A::Headwork,
                &live
            )
            .is_ok(),
            "{effect:?}"
        );
    }
}

type EffectRow = (
    Option<BacklogStatus>,
    Target,
    A,
    MoveFacts<'static>,
    LinkEffect,
    DateEffect,
);

#[test]
#[expect(clippy::too_many_lines, reason = "one row per kind of move")]
fn each_move_says_what_it_does_to_the_link_and_the_done_date() {
    use BacklogStatus as S;
    let lost = blocked(R::TaskLost, ReturnTo::InProgress);
    let cases: &[EffectRow] = &[
        (
            None,
            Target::Backlog,
            A::User,
            PLAIN,
            LinkEffect::Keep,
            DateEffect::Keep,
        ),
        (
            Some(S::Backlog),
            Target::InProgress,
            A::User,
            PLAIN,
            LinkEffect::Set,
            DateEffect::Keep,
        ),
        (
            Some(S::InProgress),
            Target::InProgress,
            A::Orchestrator,
            PLAIN,
            LinkEffect::Set,
            DateEffect::Keep,
        ),
        (
            Some(lost.clone()),
            Target::InProgress,
            A::Orchestrator,
            PLAIN,
            LinkEffect::Set,
            DateEffect::Keep,
        ),
        (
            Some(S::InProgress),
            Target::Done,
            A::Headwork,
            LIVE,
            LinkEffect::Keep,
            DateEffect::Stamp,
        ),
        (
            Some(S::Done),
            Target::Unlink,
            A::Headwork,
            PLAIN,
            LinkEffect::Clear,
            DateEffect::Renote,
        ),
        (
            Some(blocked(R::Event, ReturnTo::Backlog)),
            Target::Unlink,
            A::Headwork,
            PLAIN,
            LinkEffect::Clear,
            DateEffect::Keep,
        ),
        (
            Some(S::InProgress),
            Target::Done,
            A::Headwork,
            PLAIN,
            LinkEffect::Clear,
            DateEffect::Stamp,
        ),
        (
            Some(S::Backlog),
            Target::Done,
            A::User,
            PLAIN,
            LinkEffect::Clear,
            DateEffect::Stamp,
        ),
        (
            Some(S::Backlog),
            Target::Cancelled,
            A::User,
            PLAIN,
            LinkEffect::Clear,
            DateEffect::Stamp,
        ),
        (
            Some(S::Done),
            Target::Backlog,
            A::User,
            LIVE,
            LinkEffect::Clear,
            DateEffect::Clear,
        ),
        (
            Some(S::Cancelled),
            Target::Backlog,
            A::Orchestrator,
            PLAIN,
            LinkEffect::Clear,
            DateEffect::Clear,
        ),
        (
            Some(S::InProgress),
            block(R::Partial),
            A::Headwork,
            LIVE,
            LinkEffect::Keep,
            DateEffect::Keep,
        ),
        (
            Some(S::Done),
            Target::InProgress,
            A::Headwork,
            LIVE,
            LinkEffect::Keep,
            DateEffect::Clear,
        ),
        (
            Some(S::InProgress),
            block(R::TaskLost),
            A::Headwork,
            PLAIN,
            LinkEffect::Clear,
            DateEffect::Keep,
        ),
        (
            Some(S::InProgress),
            block(R::Event),
            A::Orchestrator,
            LIVE,
            LinkEffect::Keep,
            DateEffect::Keep,
        ),
        (
            Some(lost),
            Target::Unblock,
            A::User,
            PLAIN,
            LinkEffect::Keep,
            DateEffect::Keep,
        ),
    ];
    for (from, to, actor, facts, link, dates) in cases {
        let step = transition(from.as_ref(), to, *actor, facts)
            .unwrap_or_else(|refusal| unreachable!("{from:?} -> {to:?} by {actor}: {refusal}"));
        assert_eq!(
            (step.link, step.dates),
            (*link, *dates),
            "{from:?} -> {to:?} by {actor}"
        );
    }
}

#[test]
fn the_linked_item_table() {
    let urls = ["u/1".to_owned(), "u/2".to_owned()];
    let unlink = |note: Option<&str>| {
        Some(LinkedEnd::Unlink {
            note: note.map(str::to_owned),
        })
    };
    let cases = [
        (
            TaskState::InReview,
            ItemStatus::Done,
            &urls[..],
            Some(LinkedEnd::Unfinish),
        ),
        (
            TaskState::Closed,
            ItemStatus::Done,
            &urls[..],
            unlink(Some("task t merged u/1, u/2")),
        ),
        (
            TaskState::Dropped,
            ItemStatus::Done,
            &urls[..],
            unlink(Some("task t merged u/1, u/2")),
        ),
        (
            TaskState::Failed,
            ItemStatus::Done,
            &[][..],
            unlink(Some("task t, no pull request")),
        ),
        (
            TaskState::Closed,
            ItemStatus::Blocked,
            &[][..],
            unlink(None),
        ),
        (
            TaskState::Dropped,
            ItemStatus::Blocked,
            &[][..],
            unlink(None),
        ),
        (
            TaskState::Failed,
            ItemStatus::Blocked,
            &[][..],
            unlink(None),
        ),
        (TaskState::InReview, ItemStatus::Blocked, &[][..], None),
        (TaskState::Merged, ItemStatus::Done, &urls[..], None),
        (TaskState::Working, ItemStatus::Done, &[][..], None),
        (TaskState::Closed, ItemStatus::InProgress, &[][..], None),
        (TaskState::Closed, ItemStatus::Cancelled, &[][..], None),
        (TaskState::Closed, ItemStatus::Backlog, &[][..], None),
    ];
    for (state, status, merged, want) in cases {
        assert_eq!(
            on_linked_end(state, status, merged, "t"),
            want,
            "{state} {status}"
        );
    }
}

#[test]
fn brackets_are_refused_only_in_block_notes_and_reasons_need_one_line() {
    use BacklogStatus as S;
    let allowed = |to: Target, reason: Option<&'static str>| {
        transition(
            Some(&S::Backlog),
            &to,
            A::User,
            &MoveFacts { reason, ..PLAIN },
        )
    };
    assert!(
        allowed(Target::Done, Some("fixed in [#123]")).is_ok(),
        "a done note may hold brackets"
    );
    assert!(
        allowed(Target::Done, Some("fixed (see #12)")).is_ok(),
        "paired parentheses"
    );
    assert!(
        allowed(Target::Cancelled, Some("not ]wanted :)")).is_ok(),
        "a cancel reason is never exported"
    );
    assert_eq!(
        allowed(Target::Cancelled, Some("a\nb")).map(|_| ()),
        Err(MoveRefusal::MultiLine("reason"))
    );
}

#[test]
fn a_generated_unlink_note_is_paren_checked_like_a_done_note() {
    let unlink = |note: &'static str| {
        transition(
            Some(&BacklogStatus::Done),
            &Target::Unlink,
            A::Headwork,
            &MoveFacts {
                reason: Some(note),
                ..PLAIN
            },
        )
        .map(|step| step.kind)
    };
    assert_eq!(
        unlink("task t merged https://x/pull/1, https://x/pull/2"),
        Ok(MoveKind::Unlink)
    );
    assert_eq!(
        unlink("task t merged https://x/wiki/a_(b"),
        Err(MoveRefusal::Parentheses("note"))
    );
}
