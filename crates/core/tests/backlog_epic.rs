//! The epic explorer's guard rows, the task-end rows of an epic and the spin-up's brief (`docs/backlog.md`, section 12).

use headwork_core::TaskState;
use headwork_core::backlog::{
    BacklogActor as A, BacklogStatus, Block, BlockReason as R, DateEffect, EpicBrief, ItemKind,
    ItemStatus, LinkEffect, MoveFacts, MoveKind, MoveRefusal, ReturnTo, Target, TaskEndEffect,
    TaskOutcome, epic_brief, epic_slug, goal_line, on_task_end, plan_file_name, short_slug,
    transition,
};

const PLAIN: MoveFacts<'static> = MoveFacts {
    live_task: None,
    linked_to_actor: false,
    in_actor_epic: false,
    is_actor_epic: false,
    open_children: 0,
    has_project: true,
    links_another_task: false,
    open_dependencies: 0,
    cancelled_dependencies: 0,
    reason: None,
};

const OWN_EPIC: MoveFacts<'static> = MoveFacts {
    live_task: Some("headwork-epic-7"),
    is_actor_epic: true,
    ..PLAIN
};

const CHILD: MoveFacts<'static> = MoveFacts {
    in_actor_epic: true,
    ..PLAIN
};

const fn depends() -> Target {
    Target::Blocked {
        reason: R::Depends,
        note: None,
    }
}

fn blocked(reason: R, before: ReturnTo) -> BacklogStatus {
    BacklogStatus::Blocked(Block {
        reason,
        note: Some("why".to_owned()),
        before,
    })
}

#[test]
fn an_explorer_finishes_its_own_epic_once_no_child_is_open_and_the_link_ends() {
    for facts in [
        OWN_EPIC,
        MoveFacts {
            live_task: None,
            ..OWN_EPIC
        },
    ] {
        let step = transition(
            Some(&BacklogStatus::InProgress),
            &Target::Done,
            A::Explorer,
            &MoveFacts {
                reason: Some("nothing left"),
                ..facts
            },
        );
        assert_eq!(
            step.map(|step| (step.from, step.to, step.kind, step.link, step.dates)),
            Ok((
                Some(ItemStatus::InProgress),
                BacklogStatus::Done,
                MoveKind::Finish,
                LinkEffect::Clear,
                DateEffect::Stamp
            )),
            "{facts:?}"
        );
    }
}

#[test]
fn an_explorer_may_not_finish_its_epic_while_children_are_open() {
    let refused = transition(
        Some(&BacklogStatus::InProgress),
        &Target::Done,
        A::Explorer,
        &MoveFacts {
            open_children: 2,
            ..OWN_EPIC
        },
    );
    assert_eq!(refused, Err(MoveRefusal::ChildrenOpen(2)));
    assert_eq!(
        MoveRefusal::ChildrenOpen(2).to_string(),
        "the epic has 2 open children: finish or cancel them first"
    );
}

#[test]
fn every_other_explorer_move_of_its_epic_itself_is_refused() {
    use BacklogStatus as S;
    let not = |from: &str, to| MoveRefusal::NotAllowed {
        actor: A::Explorer,
        from: from.to_owned(),
        to,
    };
    let rows: Vec<(BacklogStatus, Target, MoveRefusal)> = vec![
        (S::Backlog, Target::Done, not("backlog", "done")),
        (
            S::Backlog,
            Target::InProgress,
            not("backlog", "in_progress"),
        ),
        (
            S::InProgress,
            Target::Cancelled,
            not("in_progress", "cancelled"),
        ),
        (S::Backlog, Target::Cancelled, not("backlog", "cancelled")),
        (S::Backlog, depends(), not("backlog", "blocked")),
        (
            S::InProgress,
            Target::Unlink,
            not("in_progress", "unlinked"),
        ),
        (
            blocked(R::Decision, ReturnTo::InProgress),
            Target::Unblock,
            not("blocked", "unblocked"),
        ),
        (
            blocked(R::Decision, ReturnTo::InProgress),
            Target::Done,
            not("blocked", "done"),
        ),
        (S::Done, Target::Backlog, not("done", "backlog")),
        (S::Done, Target::Done, not("done", "done")),
    ];
    for (from, to, want) in rows {
        assert_eq!(
            transition(Some(&from), &to, A::Explorer, &OWN_EPIC),
            Err(want),
            "{from:?} -> {to:?}"
        );
    }
}

#[test]
fn an_explorer_may_not_touch_an_epic_or_item_that_is_not_its_own() {
    for to in [Target::Done, Target::Cancelled, depends()] {
        assert_eq!(
            transition(Some(&BacklogStatus::InProgress), &to, A::Explorer, &PLAIN),
            Err(MoveRefusal::OutsideEpic),
            "{to:?}"
        );
    }
    assert_eq!(
        transition(
            Some(&BacklogStatus::Backlog),
            &Target::Cancelled,
            A::Explorer,
            &PLAIN
        ),
        Err(MoveRefusal::OutsideEpic)
    );
}

#[test]
fn the_explorer_rows_for_its_epic_s_children_stay() {
    use BacklogStatus as S;
    let cancel = transition(Some(&S::Backlog), &Target::Cancelled, A::Explorer, &CHILD);
    assert_eq!(cancel.map(|step| step.kind), Ok(MoveKind::Cancel));
    let cancel_blocked = transition(
        Some(&blocked(R::Depends, ReturnTo::Backlog)),
        &Target::Cancelled,
        A::Explorer,
        &CHILD,
    );
    assert_eq!(cancel_blocked.map(|step| step.kind), Ok(MoveKind::Cancel));
    let block = transition(Some(&S::Backlog), &depends(), A::Explorer, &CHILD);
    assert_eq!(block.map(|step| step.kind), Ok(MoveKind::Block));
    let live = MoveFacts {
        live_task: Some("headwork-x"),
        ..CHILD
    };
    assert_eq!(
        transition(Some(&S::Backlog), &Target::Cancelled, A::Explorer, &live),
        Err(MoveRefusal::DropTaskFirst("headwork-x".to_owned())),
        "a child with a live task is never cancelled by the explorer"
    );
    assert_eq!(
        transition(
            Some(&S::InProgress),
            &Target::Cancelled,
            A::Explorer,
            &CHILD
        ),
        Err(MoveRefusal::NotAllowed {
            actor: A::Explorer,
            from: "in_progress".to_owned(),
            to: "cancelled",
        })
    );
    assert_eq!(
        transition(Some(&S::InProgress), &Target::Done, A::Explorer, &CHILD),
        Err(MoveRefusal::NotAllowed {
            actor: A::Explorer,
            from: "in_progress".to_owned(),
            to: "done",
        }),
        "an explorer finishes only its epic, never a child"
    );
}

#[test]
fn the_task_end_table_of_an_epic() {
    let urls = vec!["u/1".to_owned()];
    let release = || {
        Some(TaskEndEffect::Release {
            note: "task t ended, the epic stays in progress".to_owned(),
        })
    };
    let lost = |how: &str| {
        Some(TaskEndEffect::Lost {
            note: format!("task t {how}"),
        })
    };
    let cases: Vec<(TaskState, TaskOutcome, &[String], Option<TaskEndEffect>)> = vec![
        (TaskState::Merged, TaskOutcome::Done, &[], release()),
        (TaskState::Merged, TaskOutcome::Partial, &urls, release()),
        (TaskState::Closed, TaskOutcome::Done, &[], release()),
        (TaskState::Closed, TaskOutcome::Done, &urls, release()),
        (TaskState::Closed, TaskOutcome::Partial, &[], release()),
        (TaskState::Dropped, TaskOutcome::Done, &[], lost("dropped")),
        (
            TaskState::Dropped,
            TaskOutcome::Done,
            &urls,
            lost("dropped"),
        ),
        (TaskState::Failed, TaskOutcome::Done, &urls, lost("failed")),
        (TaskState::Queued, TaskOutcome::Done, &[], None),
        (TaskState::Spawning, TaskOutcome::Done, &[], None),
        (TaskState::Working, TaskOutcome::Partial, &[], None),
        (TaskState::NeedsUser, TaskOutcome::Done, &[], None),
        (TaskState::InReview, TaskOutcome::Done, &urls, None),
        (TaskState::Closing, TaskOutcome::Done, &[], None),
        (TaskState::Interrupted, TaskOutcome::Done, &[], None),
        (TaskState::Parking, TaskOutcome::Done, &[], None),
        (TaskState::Parked, TaskOutcome::Done, &[], None),
    ];
    for (state, outcome, merged, want) in cases {
        assert_eq!(
            on_task_end(state, outcome, ItemKind::Epic, merged, "t"),
            want,
            "{state} {outcome:?}"
        );
    }
    assert_eq!(
        on_task_end(
            TaskState::Closed,
            TaskOutcome::Done,
            ItemKind::Item,
            &[],
            "t"
        ),
        Some(TaskEndEffect::Finish {
            note: "task t, no pull request".to_owned()
        }),
        "a plain item still finishes"
    );
}

#[test]
fn releasing_an_epic_unlinks_it_in_progress_only_for_its_own_ending_task() {
    let effect = TaskEndEffect::Release {
        note: "task t ended, the epic stays in progress".to_owned(),
    };
    assert_eq!(effect.target(), Target::Unlink);
    for live_task in [Some("headwork-epic-7"), None] {
        let step = transition(
            Some(&BacklogStatus::InProgress),
            &effect.target(),
            A::Headwork,
            &MoveFacts {
                live_task,
                linked_to_actor: true,
                reason: Some("task t ended, the epic stays in progress"),
                ..PLAIN
            },
        );
        assert_eq!(
            step.map(|step| (step.to, step.kind, step.link, step.dates)),
            Ok((
                BacklogStatus::InProgress,
                MoveKind::Unlink,
                LinkEffect::Clear,
                DateEffect::Keep
            )),
            "{live_task:?}"
        );
    }
    let refused = transition(
        Some(&BacklogStatus::InProgress),
        &Target::Unlink,
        A::Headwork,
        &PLAIN,
    );
    assert!(refused.is_err(), "not the ending task's item: {refused:?}");
    let by_user = transition(
        Some(&BacklogStatus::InProgress),
        &Target::Unlink,
        A::User,
        &MoveFacts {
            linked_to_actor: true,
            ..PLAIN
        },
    );
    assert!(by_user.is_err(), "only headwork unlinks: {by_user:?}");
}

#[test]
fn the_explorer_slug_and_plan_file_come_from_the_epic_s_first_words() {
    let cases = [
        ("Backlog workflow v2: epics!", "backlog-workflow-v2"),
        ("Épic façade (big) one two three", "pic-fa-ade-big"),
        (
            "A very-long-word-like-supercalifragilistic thing",
            "a-very-long-word",
        ),
        ("Abcdefghij klmnopqrst uvwxyz", "abcdefghij-klmnopqrst"),
        (
            "Supercalifragilisticexpialidocious",
            "supercalifragilisticexpi",
        ),
        ("!!!", ""),
        ("", ""),
    ];
    for (text, want) in cases {
        let short = short_slug(text);
        assert_eq!(short, want, "{text}");
        assert!(short.len() <= 24 && !short.ends_with('-'), "{short}");
    }
    assert_eq!(
        epic_slug(7, "Backlog workflow v2"),
        "epic-7-backlog-workflow-v2"
    );
    assert_eq!(epic_slug(7, "!!!"), "epic-7");
    assert_eq!(
        plan_file_name(7, "Backlog workflow v2"),
        "7-backlog-workflow-v2.md"
    );
    assert_eq!(plan_file_name(7, ""), "7.md");
    assert!(headwork_core::Slug::new(epic_slug(12, "Épic, façade (big)!")).is_ok());
}

#[test]
fn the_explorer_brief_holds_the_goal_the_epic_the_project_and_the_writes_line() {
    let notes = vec!["first note".to_owned(), "second note".to_owned()];
    let brief = epic_brief(&EpicBrief {
        id: 7,
        text: "Backlog workflow v2",
        notes: &notes,
        project: "headwork",
        plan_path: "S:/Code/notes/epics/7-backlog-workflow-v2.md",
    });
    assert!(
        brief.starts_with("Goal: Plan epic #7: Backlog workflow v2\n"),
        "{brief}"
    );
    assert_eq!(
        goal_line(&brief).as_deref(),
        Some("Plan epic #7: Backlog workflow v2")
    );
    for part in [
        "#7 Backlog workflow v2",
        "- first note\n- second note\n",
        "Project: headwork\n",
        "\nWrites: S:/Code/notes/epics/7-backlog-workflow-v2.md\n",
        "plan_add",
    ] {
        assert!(brief.contains(part), "{part:?} in {brief}");
    }
    let bare = epic_brief(&EpicBrief {
        id: 8,
        text: "Bare",
        notes: &[],
        project: "demo",
        plan_path: "/n/epics/8-bare.md",
    });
    assert!(!bare.contains("\n- "), "{bare}");
}
