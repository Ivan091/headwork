//! Every lifecycle transition and guard, as a table, plus property tests over random trigger sequences.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected refusal"
)]

use headwork_core::lifecycle::{
    CleanupFacts, CloseCause, CloseFacts, Dependency, FableGate, FinishFacts, MergePolicy,
    MergeSource, ParkedFacts, PrEnd, SessionFacts, StartFacts, WorktreeEnd,
};
use headwork_core::{MachineMode, PartSlots, Refusal, TaskSnapshot, TaskState, Trigger, apply};
use proptest::prelude::*;

use TaskState as S;

const FREE: StartFacts = StartFacts {
    live_workers: 2,
    max_workers: 10,
    project_live: 1,
    project_cap: Some(3),
    main_only_kind_busy: false,
    part_slots: None,
    dependency: Dependency::Merged,
    fable: FableGate::NotNeeded,
    machine: MachineMode::Normal,
};

const LIVE: SessionFacts = SessionFacts {
    session_matches: true,
    launcher_alive: true,
};

const READY: CloseFacts = CloseFacts {
    cause: CloseCause::Done,
    open_ask: false,
    running_job: false,
    report_sent: true,
    live_parts: 0,
};

const FINISHED: FinishFacts = FinishFacts {
    open_ask: false,
    running_job: false,
    report_sent: true,
    live_parts: 0,
    pr: PrEnd::None,
};

const CLEAN: CleanupFacts = CleanupFacts {
    pane_gone: true,
    processes_gone: true,
    worktree: WorktreeEnd::Removed,
};

const HANDED_OFF: ParkedFacts = ParkedFacts {
    handoff_written: true,
    deadline_forced: false,
};

const fn task(state: TaskState) -> TaskSnapshot {
    TaskSnapshot {
        state,
        state_before_park: None,
        keep: false,
    }
}

/// One passing trigger of each kind.
fn passing_triggers() -> Vec<Trigger> {
    vec![
        Trigger::Start(FREE),
        Trigger::SessionStarted(LIVE),
        Trigger::SpawnTimedOut,
        Trigger::LaunchFailed,
        Trigger::AskOpened,
        Trigger::AskAnswered,
        Trigger::ReviewOpened { repo_allowed: true },
        Trigger::ChangesRequested,
        Trigger::Merged {
            source: MergeSource::GitHost,
            policy: MergePolicy::Human,
        },
        Trigger::Close(READY),
        Trigger::Finish(FINISHED),
        Trigger::Drop,
        Trigger::BeginClosing,
        Trigger::ForceClose,
        Trigger::Cleaned(CLEAN),
        Trigger::ProcessLost {
            machine: MachineMode::Normal,
        },
        Trigger::Park,
        Trigger::Parked(HANDED_OFF),
        Trigger::Resume {
            machine: MachineMode::Resuming,
        },
    ]
}

/// The design's transition table: (trigger name, from, to).
const TABLE: &[(&str, TaskState, TaskState)] = &[
    ("start", S::Queued, S::Spawning),
    ("session-started", S::Spawning, S::Working),
    ("spawn-timed-out", S::Spawning, S::Failed),
    ("launch-failed", S::Spawning, S::Failed),
    ("ask-opened", S::Working, S::NeedsUser),
    ("ask-answered", S::NeedsUser, S::Working),
    ("ask-opened", S::InReview, S::InReview),
    ("ask-answered", S::InReview, S::InReview),
    ("ask-opened", S::Merged, S::Merged),
    ("ask-answered", S::Merged, S::Merged),
    ("review-opened", S::Working, S::InReview),
    ("review-opened", S::Merged, S::InReview),
    ("changes-requested", S::InReview, S::Working),
    ("merged", S::InReview, S::Merged),
    ("close", S::Merged, S::Closing),
    ("finish", S::Working, S::Closing),
    ("drop", S::Queued, S::Dropped),
    ("drop", S::Spawning, S::Dropped),
    ("drop", S::Working, S::Dropped),
    ("drop", S::NeedsUser, S::Dropped),
    ("drop", S::InReview, S::Dropped),
    ("drop", S::Merged, S::Dropped),
    ("drop", S::Failed, S::Dropped),
    ("drop", S::Interrupted, S::Dropped),
    ("drop", S::Parking, S::Dropped),
    ("drop", S::Parked, S::Dropped),
    ("begin-closing", S::Dropped, S::Closing),
    ("force-close", S::Queued, S::Closing),
    ("force-close", S::Spawning, S::Closing),
    ("force-close", S::Working, S::Closing),
    ("force-close", S::NeedsUser, S::Closing),
    ("force-close", S::InReview, S::Closing),
    ("force-close", S::Merged, S::Closing),
    ("force-close", S::Failed, S::Closing),
    ("force-close", S::Interrupted, S::Closing),
    ("force-close", S::Parking, S::Closing),
    ("force-close", S::Parked, S::Closing),
    ("cleaned", S::Closing, S::Closed),
    ("process-lost", S::Working, S::Interrupted),
    ("process-lost", S::NeedsUser, S::Interrupted),
    ("process-lost", S::InReview, S::Interrupted),
    ("process-lost", S::Merged, S::Interrupted),
    ("park", S::Working, S::Parking),
    ("park", S::NeedsUser, S::Parking),
    ("park", S::InReview, S::Parking),
    ("parked", S::Parking, S::Parked),
    ("resume", S::Parked, S::Spawning),
    ("resume", S::Interrupted, S::Spawning),
];

#[test]
fn every_pair_follows_the_table() {
    for trigger in passing_triggers() {
        for &state in S::ALL {
            let expected = TABLE
                .iter()
                .find(|(name, from, _)| *name == trigger.name() && *from == state);
            let result = apply(&task(state), &trigger);
            match expected {
                Some(&(_, _, to)) => {
                    assert_eq!(
                        result.map(|step| step.to),
                        Ok(to),
                        "{} from {state}",
                        trigger.name()
                    );
                }
                None => assert_eq!(
                    result,
                    Err(Refusal::WrongState {
                        trigger: trigger.name(),
                        state
                    }),
                    "{} must not apply to {state}",
                    trigger.name()
                ),
            }
        }
    }
}

#[test]
fn closed_is_final() {
    for trigger in passing_triggers() {
        assert!(
            apply(&task(S::Closed), &trigger).is_err(),
            "{} moved a closed task",
            trigger.name()
        );
    }
}

fn refused_start(facts: StartFacts) -> Refusal {
    apply(&task(S::Queued), &Trigger::Start(facts)).unwrap_err()
}

#[test]
fn start_guards() {
    assert_eq!(
        refused_start(StartFacts {
            live_workers: 10,
            ..FREE
        }),
        Refusal::NoFreeSlot { max: 10 },
        "slots"
    );
    assert_eq!(
        refused_start(StartFacts {
            project_live: 3,
            ..FREE
        }),
        Refusal::ProjectCapReached { cap: 3 },
        "project cap"
    );
    assert_eq!(
        refused_start(StartFacts {
            main_only_kind_busy: true,
            ..FREE
        }),
        Refusal::MainOnlyKindBusy,
        "main"
    );
    assert_eq!(
        refused_start(StartFacts {
            dependency: Dependency::Waiting,
            ..FREE
        }),
        Refusal::DependencyNotMerged,
        "dependency"
    );
    assert_eq!(
        refused_start(StartFacts {
            fable: FableGate::Waiting,
            ..FREE
        }),
        Refusal::FableNotAllowed,
        "fable"
    );
    assert_eq!(
        refused_start(StartFacts {
            machine: MachineMode::Parking,
            ..FREE
        }),
        Refusal::MachineMode(MachineMode::Parking),
        "parking"
    );
    assert_eq!(
        refused_start(StartFacts {
            part_slots: Some(PartSlots {
                live: 2,
                max_parts: 2,
            }),
            ..FREE
        }),
        Refusal::PartCapReached { max: 2 },
        "a part beyond its lead's max_parts waits"
    );
    let part_fits = StartFacts {
        part_slots: Some(PartSlots {
            live: 1,
            max_parts: 2,
        }),
        ..FREE
    };
    assert!(
        apply(&task(S::Queued), &Trigger::Start(part_fits)).is_ok(),
        "a part within max_parts starts"
    );
    assert_eq!(
        refused_start(StartFacts {
            live_workers: 10,
            part_slots: Some(PartSlots {
                live: 0,
                max_parts: 2,
            }),
            ..FREE
        }),
        Refusal::NoFreeSlot { max: 10 },
        "a part still takes a machine slot"
    );
    let uncapped = StartFacts {
        project_cap: None,
        project_live: 50,
        dependency: Dependency::None,
        ..FREE
    };
    assert!(
        apply(&task(S::Queued), &Trigger::Start(uncapped)).is_ok(),
        "no project cap"
    );
    let allowed = StartFacts {
        fable: FableGate::Allowed,
        ..FREE
    };
    assert!(
        apply(&task(S::Queued), &Trigger::Start(allowed)).is_ok(),
        "fable allowed"
    );
}

#[test]
fn session_start_guards() {
    let spawning = task(S::Spawning);
    let wrong_id = SessionFacts {
        session_matches: false,
        ..LIVE
    };
    let dead = SessionFacts {
        launcher_alive: false,
        ..LIVE
    };
    assert_eq!(
        apply(&spawning, &Trigger::SessionStarted(wrong_id)),
        Err(Refusal::SessionMismatch),
        "id"
    );
    assert_eq!(
        apply(&spawning, &Trigger::SessionStarted(dead)),
        Err(Refusal::LauncherDead),
        "launcher"
    );
}

#[test]
fn review_and_merge_guards() {
    let refused = apply(
        &task(S::Working),
        &Trigger::ReviewOpened {
            repo_allowed: false,
        },
    );
    assert_eq!(refused, Err(Refusal::RepoNotAllowed), "foreign repo");
    let child_human = Trigger::Merged {
        source: MergeSource::Worker,
        policy: MergePolicy::Human,
    };
    assert_eq!(
        apply(&task(S::InReview), &child_human),
        Err(Refusal::HumanMergeOnly),
        "human merge"
    );
    let child_deliver = Trigger::Merged {
        source: MergeSource::Worker,
        policy: MergePolicy::Deliver,
    };
    assert!(
        apply(&task(S::InReview), &child_deliver).is_ok(),
        "the worker may report a delivered merge"
    );
}

#[test]
fn close_guards() {
    let merged = task(S::Merged);
    let keep = TaskSnapshot {
        keep: true,
        ..merged
    };
    assert_eq!(
        apply(&keep, &Trigger::Close(READY)),
        Err(Refusal::KeepTask),
        "keep"
    );
    let cases = [
        (
            CloseFacts {
                open_ask: true,
                ..READY
            },
            Refusal::OpenAsk,
        ),
        (
            CloseFacts {
                running_job: true,
                ..READY
            },
            Refusal::RunningJob,
        ),
        (
            CloseFacts {
                report_sent: false,
                ..READY
            },
            Refusal::ReportMissing,
        ),
        (
            CloseFacts {
                live_parts: 2,
                ..READY
            },
            Refusal::LeadPartsLive { live: 2 },
        ),
    ];
    for (facts, refusal) in cases {
        assert_eq!(
            apply(&merged, &Trigger::Close(facts)),
            Err(refusal.clone()),
            "{refusal}"
        );
    }
    let grace = CloseFacts {
        cause: CloseCause::GraceElapsed,
        ..READY
    };
    assert!(
        apply(&merged, &Trigger::Close(grace)).is_ok(),
        "grace closes too"
    );
}

#[test]
fn finish_guards() {
    let working = task(S::Working);
    let keep = TaskSnapshot {
        keep: true,
        ..working
    };
    assert_eq!(
        apply(&keep, &Trigger::Finish(FINISHED)),
        Err(Refusal::KeepTask),
        "keep"
    );
    let cases = [
        (
            FinishFacts {
                open_ask: true,
                ..FINISHED
            },
            Refusal::OpenAsk,
        ),
        (
            FinishFacts {
                running_job: true,
                ..FINISHED
            },
            Refusal::RunningJob,
        ),
        (
            FinishFacts {
                report_sent: false,
                ..FINISHED
            },
            Refusal::ReportMissing,
        ),
        (
            FinishFacts {
                live_parts: 1,
                ..FINISHED
            },
            Refusal::LeadPartsLive { live: 1 },
        ),
        (
            FinishFacts {
                pr: PrEnd::Open,
                ..FINISHED
            },
            Refusal::PrOpen,
        ),
        (
            FinishFacts {
                pr: PrEnd::Merged,
                ..FINISHED
            },
            Refusal::PrMerged,
        ),
    ];
    for (facts, refusal) in cases {
        assert_eq!(
            apply(&working, &Trigger::Finish(facts)),
            Err(refusal.clone()),
            "{refusal}"
        );
    }
    let abandoned = FinishFacts {
        pr: PrEnd::Closed,
        ..FINISHED
    };
    assert!(
        apply(&working, &Trigger::Finish(abandoned)).is_ok(),
        "a pull request closed unmerged finishes"
    );
    for state in [S::NeedsUser, S::InReview, S::Merged, S::Parked] {
        assert!(
            matches!(
                apply(&task(state), &Trigger::Finish(FINISHED)),
                Err(Refusal::WrongState { .. })
            ),
            "finish applies only to working, not {state}"
        );
    }
}

#[test]
fn cleanup_guards() {
    let closing = task(S::Closing);
    let cases = [
        (
            CleanupFacts {
                pane_gone: false,
                ..CLEAN
            },
            Refusal::PaneOpen,
        ),
        (
            CleanupFacts {
                processes_gone: false,
                ..CLEAN
            },
            Refusal::ProcessesAlive,
        ),
        (
            CleanupFacts {
                worktree: WorktreeEnd::Present,
                ..CLEAN
            },
            Refusal::WorktreePresent,
        ),
    ];
    for (facts, refusal) in cases {
        assert_eq!(
            apply(&closing, &Trigger::Cleaned(facts)),
            Err(refusal.clone()),
            "{refusal}"
        );
    }
    for worktree in [WorktreeEnd::Kept, WorktreeEnd::NoWorktree] {
        let facts = CleanupFacts { worktree, ..CLEAN };
        assert!(
            apply(&closing, &Trigger::Cleaned(facts)).is_ok(),
            "{worktree:?} is settled"
        );
    }
}

#[test]
fn park_and_interrupt_guards() {
    let lost_while_parking = apply(
        &task(S::Working),
        &Trigger::ProcessLost {
            machine: MachineMode::Parking,
        },
    );
    assert_eq!(
        lost_while_parking,
        Err(Refusal::MachineMode(MachineMode::Parking)),
        "parking is not a crash"
    );
    let neither = ParkedFacts {
        handoff_written: false,
        deadline_forced: false,
    };
    assert_eq!(
        apply(&task(S::Parking), &Trigger::Parked(neither)),
        Err(Refusal::NoHandoff),
        "no handoff"
    );
    let forced = ParkedFacts {
        handoff_written: false,
        deadline_forced: true,
    };
    assert!(
        apply(&task(S::Parking), &Trigger::Parked(forced)).is_ok(),
        "deadline forces"
    );
    let resume = apply(
        &task(S::Parked),
        &Trigger::Resume {
            machine: MachineMode::Parked,
        },
    );
    assert_eq!(
        resume,
        Err(Refusal::MachineMode(MachineMode::Parked)),
        "resume needs resuming or normal"
    );
}

fn run(start: TaskSnapshot, triggers: &[Trigger]) -> TaskSnapshot {
    triggers.iter().fold(start, |task, trigger| {
        let step = apply(&task, trigger).unwrap();
        TaskSnapshot {
            state: step.to,
            state_before_park: step.state_before_park,
            ..task
        }
    })
}

#[test]
fn park_and_resume_return_to_the_earlier_state() {
    let resume = [
        Trigger::Park,
        Trigger::Parked(HANDED_OFF),
        Trigger::Resume {
            machine: MachineMode::Resuming,
        },
        Trigger::SessionStarted(LIVE),
    ];
    for (before, after) in [
        (S::Working, S::Working),
        (S::InReview, S::InReview),
        (S::NeedsUser, S::Working),
    ] {
        let end = run(task(before), &resume);
        assert_eq!(end.state, after, "parked from {before}");
        assert_eq!(
            end.state_before_park, None,
            "cleared after the session starts"
        );
    }
    let crash = [
        Trigger::ProcessLost {
            machine: MachineMode::Normal,
        },
        Trigger::Resume {
            machine: MachineMode::Normal,
        },
        Trigger::SessionStarted(LIVE),
    ];
    assert_eq!(
        run(task(S::Merged), &crash).state,
        S::Merged,
        "interrupted after merge comes back merged"
    );
}

#[test]
fn happy_path_reaches_closed() {
    let path = [
        Trigger::Start(FREE),
        Trigger::SessionStarted(LIVE),
        Trigger::AskOpened,
        Trigger::AskAnswered,
        Trigger::ReviewOpened { repo_allowed: true },
        Trigger::Merged {
            source: MergeSource::GitHost,
            policy: MergePolicy::Deliver,
        },
        Trigger::Close(READY),
        Trigger::Cleaned(CLEAN),
    ];
    assert_eq!(
        run(task(S::Queued), &path).state,
        S::Closed,
        "spawn to close"
    );
    let dropped = [
        Trigger::Drop,
        Trigger::BeginClosing,
        Trigger::Cleaned(CLEAN),
    ];
    assert_eq!(
        run(task(S::NeedsUser), &dropped).state,
        S::Closed,
        "drop to close"
    );
    let finished = [
        Trigger::Start(FREE),
        Trigger::SessionStarted(LIVE),
        Trigger::Finish(FINISHED),
        Trigger::Cleaned(CLEAN),
    ];
    assert_eq!(
        run(task(S::Queued), &finished).state,
        S::Closed,
        "a task without a pull request finishes"
    );
}

fn machine_mode() -> impl Strategy<Value = MachineMode> {
    proptest::sample::select(MachineMode::ALL.to_vec())
}

fn start_facts() -> impl Strategy<Value = StartFacts> {
    let dependency = prop_oneof![
        Just(Dependency::None),
        Just(Dependency::Merged),
        Just(Dependency::Waiting)
    ];
    let fable = prop_oneof![
        Just(FableGate::NotNeeded),
        Just(FableGate::Allowed),
        Just(FableGate::Waiting)
    ];
    (
        0..12_u32,
        0..5_u32,
        proptest::option::of(1..4_u32),
        any::<bool>(),
        proptest::option::of((0..4_u32, 1..4_u32)),
        dependency,
        fable,
        machine_mode(),
    )
        .prop_map(
            |(
                live_workers,
                project_live,
                project_cap,
                main_only_kind_busy,
                part_slots,
                dependency,
                fable,
                machine,
            )| StartFacts {
                live_workers,
                max_workers: 10,
                project_live,
                project_cap,
                main_only_kind_busy,
                part_slots: part_slots.map(|(live, max_parts)| PartSlots { live, max_parts }),
                dependency,
                fable,
                machine,
            },
        )
}

fn worktree_end() -> impl Strategy<Value = WorktreeEnd> {
    prop_oneof![
        Just(WorktreeEnd::NoWorktree),
        Just(WorktreeEnd::Removed),
        Just(WorktreeEnd::Kept),
        Just(WorktreeEnd::Present)
    ]
}

fn pr_end() -> impl Strategy<Value = PrEnd> {
    prop_oneof![
        Just(PrEnd::None),
        Just(PrEnd::Open),
        Just(PrEnd::Merged),
        Just(PrEnd::Closed)
    ]
}

fn trigger() -> impl Strategy<Value = Trigger> {
    let merge_source = prop_oneof![Just(MergeSource::GitHost), Just(MergeSource::Worker)];
    let merge_policy = prop_oneof![Just(MergePolicy::Deliver), Just(MergePolicy::Human)];
    let cause = prop_oneof![Just(CloseCause::Done), Just(CloseCause::GraceElapsed)];
    prop_oneof![
        start_facts().prop_map(Trigger::Start),
        (any::<bool>(), any::<bool>()).prop_map(|(session_matches, launcher_alive)| {
            Trigger::SessionStarted(SessionFacts {
                session_matches,
                launcher_alive,
            })
        }),
        Just(Trigger::SpawnTimedOut),
        Just(Trigger::LaunchFailed),
        Just(Trigger::ForceClose),
        Just(Trigger::AskOpened),
        Just(Trigger::AskAnswered),
        any::<bool>().prop_map(|repo_allowed| Trigger::ReviewOpened { repo_allowed }),
        Just(Trigger::ChangesRequested),
        (merge_source, merge_policy)
            .prop_map(|(source, policy)| Trigger::Merged { source, policy }),
        (cause, any::<bool>(), any::<bool>(), any::<bool>(), 0..3_u32).prop_map(
            |(cause, open_ask, running_job, report_sent, live_parts)| {
                Trigger::Close(CloseFacts {
                    cause,
                    open_ask,
                    running_job,
                    report_sent,
                    live_parts,
                })
            }
        ),
        (
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            0..3_u32,
            pr_end()
        )
            .prop_map(|(open_ask, running_job, report_sent, live_parts, pr)| {
                Trigger::Finish(FinishFacts {
                    open_ask,
                    running_job,
                    report_sent,
                    live_parts,
                    pr,
                })
            }),
        Just(Trigger::Drop),
        Just(Trigger::BeginClosing),
        (any::<bool>(), any::<bool>(), worktree_end()).prop_map(
            |(pane_gone, processes_gone, worktree)| {
                Trigger::Cleaned(CleanupFacts {
                    pane_gone,
                    processes_gone,
                    worktree,
                })
            }
        ),
        machine_mode().prop_map(|machine| Trigger::ProcessLost { machine }),
        Just(Trigger::Park),
        (any::<bool>(), any::<bool>()).prop_map(|(handoff_written, deadline_forced)| {
            Trigger::Parked(ParkedFacts {
                handoff_written,
                deadline_forced,
            })
        }),
        machine_mode().prop_map(|machine| Trigger::Resume { machine }),
    ]
}

/// The guard each accepted trigger must have satisfied, written independently of the implementation.
fn guard_held(task: TaskSnapshot, trigger: &Trigger) -> bool {
    match *trigger {
        Trigger::Start(f) => {
            matches!(f.machine, MachineMode::Normal | MachineMode::Resuming)
                && f.live_workers < f.max_workers
                && f.project_cap.is_none_or(|cap| f.project_live < cap)
                && !f.main_only_kind_busy
                && f.part_slots
                    .is_none_or(|slots| slots.live < slots.max_parts)
                && f.dependency != Dependency::Waiting
                && f.fable != FableGate::Waiting
        }
        Trigger::SessionStarted(f) => f.session_matches && f.launcher_alive,
        Trigger::ReviewOpened { repo_allowed } => repo_allowed,
        Trigger::Merged { source, policy } => {
            !(source == MergeSource::Worker && policy == MergePolicy::Human)
        }
        Trigger::Close(f) => {
            !task.keep && !f.open_ask && !f.running_job && f.live_parts == 0 && f.report_sent
        }
        Trigger::Finish(f) => {
            !task.keep
                && !f.open_ask
                && !f.running_job
                && f.live_parts == 0
                && f.report_sent
                && matches!(f.pr, PrEnd::None | PrEnd::Closed)
        }
        Trigger::Cleaned(f) => {
            f.pane_gone && f.processes_gone && f.worktree != WorktreeEnd::Present
        }
        Trigger::ProcessLost { machine } => machine != MachineMode::Parking,
        Trigger::Parked(f) => f.handoff_written || f.deadline_forced,
        Trigger::Resume { machine } => {
            matches!(machine, MachineMode::Normal | MachineMode::Resuming)
        }
        Trigger::SpawnTimedOut
        | Trigger::LaunchFailed
        | Trigger::ForceClose
        | Trigger::AskOpened
        | Trigger::AskAnswered
        | Trigger::ChangesRequested
        | Trigger::Drop
        | Trigger::BeginClosing
        | Trigger::Park => true,
    }
}

proptest! {
    #[test]
    fn random_sequences_keep_the_invariants(keep: bool, triggers in proptest::collection::vec(trigger(), 0..60)) {
        let mut current = TaskSnapshot { state: S::Queued, state_before_park: None, keep };
        for trigger in &triggers {
            let was_closed = current.state == S::Closed;
            match apply(&current, trigger) {
                Ok(step) => {
                    prop_assert!(!was_closed, "a closed task moved");
                    prop_assert_eq!(step.from, current.state, "a step starts at the current state");
                    prop_assert!(guard_held(current, trigger), "{} passed a failing guard", trigger.name());
                    if step.to == S::Closed {
                        prop_assert_eq!(current.state, S::Closing, "closed only after closing");
                        prop_assert!(matches!(trigger, Trigger::Cleaned(_)), "closed only after verification");
                    }
                    if step.to == S::Closing && matches!(trigger, Trigger::Close(_) | Trigger::Finish(_)) {
                        prop_assert!(!keep, "a keep task closed on its own");
                    }
                    current = TaskSnapshot { state: step.to, state_before_park: step.state_before_park, keep };
                }
                Err(Refusal::WrongState { state, .. }) => {
                    prop_assert_eq!(state, current.state, "the refusal names the current state");
                    prop_assert!(!trigger.applies_to(current.state), "applies_to agrees with apply");
                }
                Err(_) => prop_assert!(!guard_held(current, trigger), "{} refused a passing guard", trigger.name()),
            }
            let parked_like = matches!(current.state, S::Parking | S::Parked | S::Interrupted);
            if parked_like {
                prop_assert!(current.state_before_park.is_some(), "{} keeps where to return", current.state);
            }
            if current.state_before_park.is_some() {
                prop_assert!(
                    parked_like || matches!(current.state, S::Spawning | S::Failed),
                    "state_before_park lingers in {}",
                    current.state
                );
            }
        }
    }
}
