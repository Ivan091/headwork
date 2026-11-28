//! The epic runner's pure parts (`docs/backlog.md`, section 13): what one epic does in one pass, the slugs and briefs of
//! the tasks it spawns, and the guard row that lets headwork block an epic for a decision.

#![expect(clippy::unwrap_used, reason = "tests fail loudly on a bad date")]

use headwork_core::backlog::{
    BacklogActor as A, BacklogDate, BacklogStatus, Block, BlockReason as R, ChildBrief, EpicChild,
    EpicFacts, EpicLimits, EpicStep, MoveFacts, MoveRefusal, ReplanBrief, ReplanCause,
    ReplanTrigger, ReturnTo, Target, child_brief, child_slug, epic_step, goal_line, replan_brief,
    transition, with_writes_line,
};

fn date(text: &str) -> BacklogDate {
    BacklogDate::parse(text).unwrap()
}

fn child(id: i64, status: BacklogStatus) -> EpicChild {
    EpicChild {
        id,
        text: format!("child {id}"),
        status,
        explore: false,
        agent: true,
        not_before: None,
        has_project: true,
        block_event: None,
        block_replanned: false,
    }
}

fn explore(id: i64, status: BacklogStatus) -> EpicChild {
    EpicChild {
        explore: true,
        ..child(id, status)
    }
}

fn blocked(reason: R) -> BacklogStatus {
    BacklogStatus::Blocked(Block {
        reason,
        note: Some(format!("{reason} note")),
        before: ReturnTo::InProgress,
    })
}

fn facts(children: Vec<EpicChild>) -> EpicFacts {
    EpicFacts {
        epic: 1,
        explorer_live: false,
        children,
        replans: 0,
        today: date("2026-11-28"),
    }
}

const LIMITS: EpicLimits = EpicLimits {
    max_parallel: 3,
    max_replans: 5,
    room: None,
};

fn run(starts: &[i64]) -> EpicStep {
    EpicStep::Run {
        starts: starts.to_vec(),
        skipped: Vec::new(),
        held: false,
    }
}

#[test]
fn a_live_explorer_holds_starts_and_re_plans() {
    let mut held = facts(vec![child(2, BacklogStatus::Backlog)]);
    held.explorer_live = true;
    assert_eq!(epic_step(&held, &LIMITS), EpicStep::Hold);
    let mut lost = facts(vec![child(2, blocked(R::TaskLost))]);
    lost.explorer_live = true;
    assert_eq!(
        epic_step(&lost, &LIMITS),
        EpicStep::Hold,
        "no re-plan either"
    );
    let mut empty = facts(Vec::new());
    empty.explorer_live = true;
    assert_eq!(epic_step(&empty, &LIMITS), EpicStep::Hold);
}

#[test]
fn ready_children_start_in_order_up_to_max_parallel_and_explore_children_do_not_count() {
    let children = vec![
        child(2, BacklogStatus::InProgress),
        explore(3, BacklogStatus::InProgress),
        child(4, BacklogStatus::Backlog),
        child(5, blocked(R::Depends)),
        child(6, BacklogStatus::Backlog),
        child(7, BacklogStatus::Backlog),
        child(8, BacklogStatus::Done),
    ];
    let cases = [
        (0, vec![4, 6, 7]),
        (1, vec![]),
        (2, vec![4]),
        (3, vec![4, 6]),
        (9, vec![4, 6, 7]),
    ];
    for (max_parallel, starts) in cases {
        let limits = EpicLimits {
            max_parallel,
            ..LIMITS
        };
        assert_eq!(
            epic_step(&facts(children.clone()), &limits),
            run(&starts),
            "max_parallel {max_parallel}"
        );
    }
}

#[test]
fn a_ready_explore_child_starts_alone_and_never_beside_a_running_one() {
    let children = vec![
        child(2, BacklogStatus::Backlog),
        explore(3, BacklogStatus::Backlog),
        child(4, BacklogStatus::Backlog),
    ];
    assert_eq!(epic_step(&facts(children.clone()), &LIMITS), run(&[3]));
    let mut both = children;
    both.push(explore(5, BacklogStatus::InProgress));
    assert_eq!(
        epic_step(&facts(both), &LIMITS),
        run(&[]),
        "an explore child in progress keeps the next one, and the rest, waiting"
    );
}

#[test]
fn not_before_owner_and_project_decide_which_backlog_children_are_ready() {
    let later = EpicChild {
        not_before: Some(date("2026-11-29")),
        ..child(2, BacklogStatus::Backlog)
    };
    let today = EpicChild {
        not_before: Some(date("2026-11-28")),
        ..child(3, BacklogStatus::Backlog)
    };
    let users = EpicChild {
        agent: false,
        ..child(4, BacklogStatus::Backlog)
    };
    let elsewhere = EpicChild {
        has_project: false,
        ..child(5, BacklogStatus::Backlog)
    };
    assert_eq!(
        epic_step(&facts(vec![later, today, users, elsewhere]), &LIMITS),
        EpicStep::Run {
            starts: vec![3],
            skipped: vec![5],
            held: false,
        }
    );
}

#[test]
fn the_session_room_caps_the_starts_and_says_when_it_held_one() {
    let children = vec![
        child(2, BacklogStatus::Backlog),
        child(3, BacklogStatus::Backlog),
    ];
    let cases = [
        (Some(0), vec![], true),
        (Some(1), vec![2], true),
        (Some(2), vec![2, 3], false),
        (None, vec![2, 3], false),
    ];
    for (room, starts, held) in cases {
        let limits = EpicLimits { room, ..LIMITS };
        assert_eq!(
            epic_step(&facts(children.clone()), &limits),
            EpicStep::Run {
                starts,
                skipped: Vec::new(),
                held
            },
            "room {room:?}"
        );
    }
    let limits = EpicLimits {
        room: Some(0),
        ..LIMITS
    };
    assert_eq!(
        epic_step(&facts(vec![explore(2, BacklogStatus::Backlog)]), &limits),
        EpicStep::Run {
            starts: Vec::new(),
            skipped: Vec::new(),
            held: true
        },
        "an explore child waits for room too"
    );
    let full = EpicLimits {
        max_parallel: 1,
        room: Some(0),
        ..LIMITS
    };
    let running = vec![
        child(2, BacklogStatus::InProgress),
        child(3, BacklogStatus::Backlog),
    ];
    assert_eq!(
        epic_step(&facts(running), &full),
        run(&[]),
        "nothing held when max_parallel already stops the start"
    );
}

#[test]
fn a_blocked_child_triggers_a_re_plan_before_any_start_and_only_once_per_block() {
    let lost = EpicChild {
        block_event: Some(40),
        ..child(2, blocked(R::TaskLost))
    };
    let partial = EpicChild {
        block_event: Some(41),
        ..child(3, blocked(R::Partial))
    };
    let cancelled = child(4, blocked(R::DepCancelled));
    let waiting = child(5, blocked(R::Depends));
    let decision = child(6, blocked(R::Decision));
    let ready = child(7, BacklogStatus::Backlog);
    let step = epic_step(
        &facts(vec![
            lost.clone(),
            partial,
            cancelled,
            waiting.clone(),
            decision.clone(),
            ready.clone(),
        ]),
        &LIMITS,
    );
    let EpicStep::Replan(ReplanTrigger::Blocked(causes)) = step else {
        unreachable!("{step:?}")
    };
    let seen: Vec<(i64, R, Option<i64>)> = causes
        .iter()
        .map(|cause| (cause.child, cause.reason, cause.event))
        .collect();
    assert_eq!(
        seen,
        vec![
            (2, R::TaskLost, Some(40)),
            (3, R::Partial, Some(41)),
            (4, R::DepCancelled, None)
        ]
    );
    assert_eq!(causes[0].note.as_deref(), Some("task_lost note"));
    let replanned = EpicChild {
        block_replanned: true,
        ..lost
    };
    assert_eq!(
        epic_step(&facts(vec![replanned, waiting, decision, ready]), &LIMITS),
        run(&[7]),
        "a block that already caused a re-plan triggers none"
    );
}

#[test]
fn no_open_child_asks_whether_the_plan_is_complete() {
    let finished = vec![
        child(2, BacklogStatus::Done),
        child(3, BacklogStatus::Cancelled),
        explore(4, BacklogStatus::Done),
    ];
    assert_eq!(
        epic_step(&facts(finished), &LIMITS),
        EpicStep::Replan(ReplanTrigger::PlanComplete)
    );
    assert_eq!(
        epic_step(&facts(Vec::new()), &LIMITS),
        EpicStep::Replan(ReplanTrigger::PlanComplete)
    );
}

#[test]
fn an_open_explore_child_suppresses_every_trigger() {
    for status in [
        BacklogStatus::Backlog,
        BacklogStatus::InProgress,
        blocked(R::TaskLost),
    ] {
        let children = vec![child(2, blocked(R::TaskLost)), explore(3, status.clone())];
        let step = epic_step(&facts(children), &LIMITS);
        assert!(
            !matches!(step, EpicStep::Replan(_) | EpicStep::Churn { .. }),
            "{status}: {step:?}"
        );
    }
}

#[test]
fn a_trigger_at_max_replans_blocks_for_churn_and_zero_means_no_limit() {
    let mut at_cap = facts(vec![child(2, blocked(R::TaskLost))]);
    at_cap.replans = 5;
    assert_eq!(epic_step(&at_cap, &LIMITS), EpicStep::Churn { replans: 5 });
    let mut below = at_cap.clone();
    below.replans = 4;
    assert!(matches!(epic_step(&below, &LIMITS), EpicStep::Replan(_)));
    let unlimited = EpicLimits {
        max_replans: 0,
        ..LIMITS
    };
    at_cap.replans = 500;
    assert!(matches!(
        epic_step(&at_cap, &unlimited),
        EpicStep::Replan(_)
    ));
    let mut quiet = facts(vec![child(2, BacklogStatus::InProgress)]);
    quiet.replans = 9;
    assert_eq!(
        epic_step(&quiet, &LIMITS),
        run(&[]),
        "the count alone blocks nothing"
    );
}

#[test]
fn child_slugs_name_the_epic_and_the_child_and_are_valid_slugs() {
    assert_eq!(
        child_slug(7, 12, "Store piece: add rows"),
        "e7-12-store-piece-add-rows"
    );
    assert_eq!(child_slug(7, 12, "!!"), "e7-12");
    assert!(headwork_core::Slug::new(child_slug(7, 12, "Épic, façade (big)!")).is_ok());
}

#[test]
fn a_child_brief_is_made_from_its_text_notes_and_epic() {
    let notes = vec!["keep it small".to_owned()];
    let brief = child_brief(&ChildBrief {
        id: 12,
        text: "Store piece",
        notes: &notes,
        epic: 7,
        epic_text: "Backlog v2",
        project: "headwork",
    });
    assert_eq!(goal_line(&brief).as_deref(), Some("Store piece"));
    for part in [
        "#12",
        "epic #7: Backlog v2",
        "- keep it small\n",
        "Project: headwork\n",
    ] {
        assert!(brief.contains(part), "{part:?} in {brief}");
    }
}

#[test]
fn a_writes_line_is_added_only_when_missing() {
    assert_eq!(
        with_writes_line("Goal: x\n", "/n/p.md"),
        "Goal: x\n\nWrites: /n/p.md\n"
    );
    let has = "Goal: x\n\nWrites: /n/other.md\n";
    assert_eq!(with_writes_line(has, "/n/p.md"), has);
}

#[test]
fn a_re_plan_brief_names_the_reason_each_cause_and_the_plan_file() {
    let trigger = ReplanTrigger::Blocked(vec![
        ReplanCause {
            child: 12,
            text: "Store piece".to_owned(),
            reason: R::TaskLost,
            note: Some("task demo-x failed".to_owned()),
            event: Some(40),
        },
        ReplanCause {
            child: 13,
            text: "Core piece".to_owned(),
            reason: R::DepCancelled,
            note: None,
            event: None,
        },
    ]);
    assert_eq!(
        trigger.child_text(),
        "Re-plan: #12 task_lost, #13 dep_cancelled"
    );
    let brief = replan_brief(&ReplanBrief {
        epic: 7,
        epic_text: "Backlog v2",
        project: "headwork",
        trigger: &trigger,
        plan_path: "/n/epics/7-backlog-v2.md",
    });
    assert_eq!(
        goal_line(&brief).as_deref(),
        Some("Re-plan epic #7: #12 task_lost, #13 dep_cancelled")
    );
    for part in [
        "- #12 Store piece: task_lost (task demo-x failed)\n",
        "- #13 Core piece: dep_cancelled\n",
        "plan_finish",
        "Project: headwork\n",
        "\nWrites: /n/epics/7-backlog-v2.md\n",
    ] {
        assert!(brief.contains(part), "{part:?} in {brief}");
    }
    assert_eq!(
        ReplanTrigger::PlanComplete.child_text(),
        "Re-plan: plan complete?"
    );
    let complete = replan_brief(&ReplanBrief {
        trigger: &ReplanTrigger::PlanComplete,
        epic: 7,
        epic_text: "Backlog v2",
        project: "headwork",
        plan_path: "/n/p.md",
    });
    assert!(!complete.contains("## Blocked children"), "{complete}");
}

const PLAIN: MoveFacts<'static> = MoveFacts {
    live_task: None,
    linked_to_actor: false,
    in_actor_epic: false,
    is_actor_epic: false,
    open_children: 0,
    is_epic: false,
    has_project: true,
    links_another_task: false,
    open_dependencies: 0,
    cancelled_dependencies: 0,
    reason: None,
};

const EPIC: MoveFacts<'static> = MoveFacts {
    is_epic: true,
    open_children: 2,
    ..PLAIN
};

fn decision() -> Target {
    Target::Blocked {
        reason: R::Decision,
        note: Some("epic-churn: 5 re-plans".to_owned()),
    }
}

#[test]
fn headwork_blocks_an_in_progress_epic_and_only_an_epic_for_a_decision() {
    let step = transition(
        Some(&BacklogStatus::InProgress),
        &decision(),
        A::Headwork,
        &EPIC,
    )
    .unwrap_or_else(|refusal| unreachable!("{refusal}"));
    assert_eq!(
        step.to,
        BacklogStatus::Blocked(Block {
            reason: R::Decision,
            note: Some("epic-churn: 5 re-plans".to_owned()),
            before: ReturnTo::InProgress,
        })
    );
    let unblocked = transition(Some(&step.to), &Target::Unblock, A::User, &EPIC)
        .unwrap_or_else(|refusal| unreachable!("{refusal}"));
    assert_eq!(
        unblocked.to,
        BacklogStatus::InProgress,
        "back in progress on the user's unblock"
    );
    assert_eq!(
        transition(
            Some(&BacklogStatus::InProgress),
            &decision(),
            A::Headwork,
            &PLAIN
        ),
        Err(MoveRefusal::Reason {
            actor: A::Headwork,
            reason: R::Decision
        }),
        "a plain item is never blocked for a decision by headwork"
    );
    assert_eq!(
        transition(
            Some(&BacklogStatus::Backlog),
            &decision(),
            A::Headwork,
            &EPIC
        ),
        Err(MoveRefusal::Reason {
            actor: A::Headwork,
            reason: R::Decision
        }),
        "only from in progress"
    );
    for reason in [R::Event, R::Depends] {
        let target = Target::Blocked { reason, note: None };
        assert!(
            transition(
                Some(&BacklogStatus::InProgress),
                &target,
                A::Headwork,
                &EPIC
            )
            .is_err(),
            "{reason}"
        );
    }
}
