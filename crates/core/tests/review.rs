//! Reviews: the command parser, flow validation, ready-node scheduling, the state machine and the dispute rule, as
//! tables plus property tests.

#![expect(clippy::unwrap_used, clippy::expect_used, reason = "tests fail loudly")]

use std::collections::BTreeSet;

use headwork_core::review::{
    After, DisputeCall, DisputeRule, Flow, FlowError, FlowName, Instance, Level, LevelSpec,
    ParsedCommand, Progress, ReviewCommand, ReviewFacts, ReviewRefusal, ReviewTrigger,
    ReviewerView, Unsettled, apply, parse_command, ready, round_complete, round_due,
    settle_dispute,
};
use headwork_core::{Model, NodeState, ReviewState};
use proptest::prelude::*;

const CODE: &str = include_str!("../../../flows/code.toml");

fn code() -> Flow {
    Flow::parse(CODE).expect("the built-in code flow is valid")
}

fn review(flow: Option<&str>, level: u8, focus: &str) -> ParsedCommand {
    ParsedCommand::Review(ReviewCommand {
        flow: flow.map(|name| FlowName::new(name).unwrap()),
        level: Level::from_number(level).unwrap(),
        focus: focus.to_owned(),
    })
}

#[test]
fn prompts_parse_as_review_commands_or_not() {
    let near = |text: &str| matches!(parse_command(text), ParsedCommand::NearMiss(_));
    let table: [(&str, ParsedCommand); 14] = [
        ("review 2", review(None, 2, "")),
        ("review [3]", review(None, 3, "")),
        ("  Review 1  ", review(None, 1, "")),
        ("review plan 1", review(Some("plan"), 1, "")),
        ("review Plan [2]", review(Some("plan"), 2, "")),
        (
            "review 2 focus on the park path",
            review(None, 2, "focus on the park path"),
        ),
        (
            "review code 3 the guard\nand the hook",
            review(Some("code"), 3, "the guard\nand the hook"),
        ),
        ("review the docs", ParsedCommand::Other),
        ("please review 2 things", ParsedCommand::Other),
        ("reviewer", ParsedCommand::Other),
        ("reviewed 2", ParsedCommand::Other),
        ("", ParsedCommand::Other),
        ("review plan docs", ParsedCommand::Other),
        ("review 2x", ParsedCommand::Other),
    ];
    for (text, want) in table {
        assert_eq!(parse_command(text), want, "{text:?}");
    }
    for text in [
        "review",
        "review 4",
        "review [0]",
        "review plan 7",
        "review 999",
    ] {
        assert!(near(text), "{text:?} is a near miss");
    }
}

#[test]
fn levels_round_trip_through_their_numbers() {
    for level in Level::ALL {
        assert_eq!(Level::try_from(level.number()), Ok(level), "{level}");
    }
    assert!(Level::try_from(4).is_err(), "4 is not a level");
    assert!(Level::Light < Level::Strict, "levels order lightest first");
}

#[test]
fn the_code_flow_runs_more_nodes_at_each_level() {
    let flow = code();
    let ids = |level| -> Vec<String> {
        flow.nodes_at(level)
            .map(|node| node.id.to_string())
            .collect()
    };
    assert_eq!(
        ids(Level::Light),
        ["correctness", "tests", "rules"],
        "level 1"
    );
    assert_eq!(
        ids(Level::Standard).len(),
        7,
        "level 2: six reviewers and verify"
    );
    assert_eq!(ids(Level::Strict).len(), 11, "level 3: every node");
    let verify = flow.node("verify").unwrap();
    assert_eq!(
        verify.after,
        After::Earlier,
        "verify waits for every earlier node"
    );
    assert_eq!(
        flow.inputs(verify, Level::Standard).len(),
        6,
        "verify's inputs at level 2"
    );
    assert_eq!(
        flow.node("correctness").unwrap().model_at(Level::Light),
        Some(Model::Sonnet),
        "sonnet at 1"
    );
    assert_eq!(
        flow.node("correctness").unwrap().model_at(Level::Strict),
        Some(Model::Opus),
        "opus at 3"
    );
    assert_eq!(
        flow.level(Level::Strict).rounds,
        2,
        "level 3 runs two rounds"
    );
    assert!(
        flow.check(|_| true, Model::ALL).is_empty(),
        "every model allowed, every prompt present"
    );
}

#[test]
fn the_code_flow_names_prompts_that_exist() {
    let flow = code();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../flows");
    let problems = flow.check(|prompt| root.join(prompt).is_file(), Model::ALL);
    assert!(problems.is_empty(), "{problems:?}");
}

#[test]
fn check_reports_missing_prompts_and_refused_models() {
    let flow = code();
    let problems = flow.check(|prompt| prompt != "code/tests.md", &[Model::Sonnet]);
    assert!(
        problems.contains(&FlowError::PromptMissing {
            node: "tests".to_owned(),
            prompt: "code/tests.md".to_owned()
        }),
        "{problems:?}"
    );
    assert!(
        problems.contains(&FlowError::ModelNotAllowed {
            node: "correctness".to_owned(),
            model: Model::Opus,
            level: Level::Standard
        }),
        "{problems:?}"
    );
}

const LEVELS: &str = r#"
[levels.1]
pushback = "a"
dispute = "reason"
unsettled = "accept"
rounds = 1
[levels.2]
pushback = "b"
dispute = "evidence"
unsettled = "ask"
rounds = 1
[levels.3]
pushback = "c"
dispute = "evidence"
unsettled = "ask"
rounds = 2
"#;

fn flow_with(nodes: &str) -> Result<Flow, FlowError> {
    Flow::parse(&format!(
        "name = \"t\"\ndescription = \"d\"\ntarget = \"diff\"\n{LEVELS}\n{nodes}"
    ))
}

#[test]
fn broken_flows_are_refused_with_the_reason() {
    let node = |id: &str, extra: &str| {
        format!("[[node]]\nid = \"{id}\"\nprompt = \"p.md\"\nmodel = \"sonnet\"\n{extra}\n")
    };
    let table: [(String, FlowError); 9] = [
        (
            node("a", "") + &node("a", ""),
            FlowError::DuplicateNode("a".to_owned()),
        ),
        (
            node("a", "after = [\"b\"]"),
            FlowError::UnknownAfter {
                node: "a".to_owned(),
                target: "b".to_owned(),
            },
        ),
        (
            node("a", "after = [\"a\"]"),
            FlowError::UnknownAfter {
                node: "a".to_owned(),
                target: "a".to_owned(),
            },
        ),
        (
            node("a", "after = [\"b\"]") + &node("b", "after = [\"a\"]"),
            FlowError::Cycle("a".to_owned()),
        ),
        (
            node("a", "") + &node("v", "each = \"finding\""),
            FlowError::EachWithoutAfter("v".to_owned()),
        ),
        (node("a", "from_level = 2"), FlowError::NoNodes),
        (
            node("a", "from_level = 4"),
            FlowError::FromLevel {
                node: "a".to_owned(),
                value: 4,
            },
        ),
        (node("Bad", ""), FlowError::BadName("Bad".to_owned())),
        (
            "[[node]]\nid = \"a\"\nprompt = \"p.md\"\nmodel = { 1 = \"sonnet\", 2 = \"opus\" }\n"
                .to_owned(),
            FlowError::ModelMissing {
                node: "a".to_owned(),
                level: Level::Strict,
            },
        ),
    ];
    for (nodes, want) in table {
        assert_eq!(flow_with(&nodes), Err(want), "{nodes}");
    }
    let missing = Flow::parse(
        "name = \"t\"\ndescription = \"d\"\ntarget = \"diff\"\n[levels.1]\npushback = \"a\"\ndispute = \"reason\"\nunsettled = \"accept\"\nrounds = 1\n",
    );
    assert!(matches!(missing, Err(FlowError::Levels(_))), "{missing:?}");
    let rounds = Flow::parse(&format!(
        "name = \"t\"\ndescription = \"d\"\ntarget = \"diff\"\n{}",
        LEVELS.replace("rounds = 2", "rounds = 0")
    ));
    assert_eq!(
        rounds,
        Err(FlowError::Rounds {
            level: Level::Strict,
            rounds: 0
        }),
        "rounds 0"
    );
    assert!(
        matches!(
            flow_with("[[node]]\nid = \"a\"\n"),
            Err(FlowError::Parse(_))
        ),
        "a node without its fields"
    );
}

#[test]
fn a_model_map_needs_only_the_levels_the_node_runs_at() {
    let flow = flow_with(
        "[[node]]\nid = \"a\"\nprompt = \"p.md\"\nmodel = \"sonnet\"\n[[node]]\nid = \"b\"\nprompt = \"p.md\"\nfrom_level = 2\nmodel = { 2 = \"sonnet\", 3 = \"opus\" }\n",
    );
    assert!(flow.is_ok(), "{flow:?}");
}

fn finish(progress: &mut Progress, node: &str, item: i64, findings: &[i64]) {
    let instance = Instance {
        node: headwork_core::review::NodeId::new(node).unwrap(),
        item,
    };
    progress.instances.insert(instance.clone(), NodeState::Done);
    if item == 0 {
        progress.findings.insert(instance.node, findings.to_vec());
    }
}

fn names(instances: &[Instance]) -> Vec<String> {
    instances
        .iter()
        .map(|instance| format!("{}#{}", instance.node, instance.item))
        .collect()
}

#[test]
fn verify_fans_out_once_every_reviewer_is_done() {
    let flow = code();
    let mut progress = Progress::default();
    let first = ready(&flow, Level::Standard, &progress);
    assert_eq!(
        first.len(),
        6,
        "six reviewers start at once: {:?}",
        names(&first)
    );
    for (n, instance) in first.iter().enumerate() {
        let id = i64::try_from(n).unwrap() + 1;
        let findings: &[i64] = if n < 2 { &[id] } else { &[] };
        assert!(
            !round_complete(&flow, Level::Standard, &progress),
            "not complete before {}",
            instance.node
        );
        finish(&mut progress, instance.node.as_str(), 0, findings);
    }
    let verify = ready(&flow, Level::Standard, &progress);
    assert_eq!(
        names(&verify),
        ["verify#1", "verify#2"],
        "one verifier per finding"
    );
    finish(&mut progress, "verify", 1, &[]);
    assert_eq!(
        names(&ready(&flow, Level::Standard, &progress)),
        ["verify#2"],
        "the other verifier stays ready"
    );
    assert!(
        !round_complete(&flow, Level::Standard, &progress),
        "a verifier is still due"
    );
    finish(&mut progress, "verify", 2, &[]);
    assert!(
        round_complete(&flow, Level::Standard, &progress),
        "every node is over"
    );
    assert!(
        ready(&flow, Level::Standard, &progress).is_empty(),
        "nothing is ready after the round"
    );
}

#[test]
fn a_round_without_findings_needs_no_verifier() {
    let flow = code();
    let mut progress = Progress::default();
    for instance in ready(&flow, Level::Standard, &progress) {
        finish(&mut progress, instance.node.as_str(), 0, &[]);
    }
    assert!(
        round_complete(&flow, Level::Standard, &progress),
        "no finding, no verifier"
    );
}

#[test]
fn a_running_or_failed_node_counts_as_taken() {
    let flow = code();
    let mut progress = Progress::default();
    let correctness = Instance {
        node: headwork_core::review::NodeId::new("correctness").unwrap(),
        item: 0,
    };
    progress
        .instances
        .insert(correctness.clone(), NodeState::Running);
    assert_eq!(
        ready(&flow, Level::Light, &progress).len(),
        2,
        "a running node is not ready again"
    );
    assert!(
        !round_complete(&flow, Level::Light, &progress),
        "a running node keeps the round open"
    );
    progress.instances.insert(correctness, NodeState::Failed);
    finish(&mut progress, "tests", 0, &[]);
    finish(&mut progress, "rules", 0, &[]);
    assert!(
        round_complete(&flow, Level::Light, &progress),
        "a failed node is over"
    );
}

const fn facts() -> ReviewFacts {
    ReviewFacts {
        round_complete: true,
        awaiting_lead: 0,
        awaiting_verdict: 0,
        asked: 0,
        round_due: false,
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one table holds every transition and guard"
)]
fn every_transition_and_guard() {
    use ReviewState as S;
    use ReviewTrigger as T;
    let open = ReviewFacts {
        round_complete: false,
        ..facts()
    };
    let lead = ReviewFacts {
        awaiting_lead: 2,
        ..facts()
    };
    let verdicts = ReviewFacts {
        awaiting_verdict: 1,
        ..facts()
    };
    let asked = ReviewFacts {
        asked: 1,
        ..facts()
    };
    let due = ReviewFacts {
        round_due: true,
        ..facts()
    };
    let table: [(S, T, ReviewFacts, Result<S, ReviewRefusal>); 24] = [
        (S::Requested, T::Start, facts(), Ok(S::Running)),
        (
            S::Requested,
            T::Deliver,
            facts(),
            Err(ReviewRefusal::WrongState {
                from: S::Requested,
                trigger: "deliver",
            }),
        ),
        (S::Running, T::Deliver, facts(), Ok(S::Delivered)),
        (S::Running, T::Deliver, open, Err(ReviewRefusal::NodesOpen)),
        (
            S::Running,
            T::Finish,
            facts(),
            Err(ReviewRefusal::WrongState {
                from: S::Running,
                trigger: "finish",
            }),
        ),
        (S::Delivered, T::Resolve, facts(), Ok(S::Rechecking)),
        (
            S::Delivered,
            T::Resolve,
            lead,
            Err(ReviewRefusal::FindingsOpen(2)),
        ),
        (S::Delivered, T::Finish, facts(), Ok(S::Done)),
        (
            S::Delivered,
            T::Finish,
            lead,
            Err(ReviewRefusal::FindingsOpen(2)),
        ),
        (S::Rechecking, T::Verdict, facts(), Ok(S::Rechecking)),
        (
            S::Rechecking,
            T::Verdict,
            verdicts,
            Err(ReviewRefusal::VerdictsOpen(1)),
        ),
        (S::Rechecking, T::Verdict, asked, Ok(S::Asking)),
        (S::Rechecking, T::Verdict, lead, Ok(S::Delivered)),
        (S::Rechecking, T::Verdict, due, Ok(S::Running)),
        (S::Rechecking, T::Finish, facts(), Ok(S::Done)),
        (S::Rechecking, T::Finish, due, Err(ReviewRefusal::RoundDue)),
        (
            S::Rechecking,
            T::Finish,
            verdicts,
            Err(ReviewRefusal::VerdictsOpen(1)),
        ),
        (
            S::Asking,
            T::Answer,
            asked,
            Err(ReviewRefusal::AskWaiting(1)),
        ),
        (S::Asking, T::Answer, lead, Ok(S::Delivered)),
        (S::Asking, T::Answer, facts(), Ok(S::Rechecking)),
        (
            S::Asking,
            T::Finish,
            facts(),
            Err(ReviewRefusal::WrongState {
                from: S::Asking,
                trigger: "finish",
            }),
        ),
        (S::Asking, T::Cancel, facts(), Ok(S::Cancelled)),
        (
            S::Done,
            T::Cancel,
            facts(),
            Err(ReviewRefusal::WrongState {
                from: S::Done,
                trigger: "cancel",
            }),
        ),
        (
            S::Cancelled,
            T::Start,
            facts(),
            Err(ReviewRefusal::WrongState {
                from: S::Cancelled,
                trigger: "start",
            }),
        ),
    ];
    for (from, trigger, given, want) in table {
        assert_eq!(
            apply(from, trigger, given),
            want,
            "{from} {trigger:?} {given:?}"
        );
    }
}

const fn spec(dispute: DisputeRule, unsettled: Unsettled, argue_back: bool) -> LevelSpec {
    LevelSpec {
        pushback: String::new(),
        dispute,
        unsettled,
        rounds: 1,
        argue_back,
    }
}

#[test]
fn disputes_settle_by_the_level() {
    use DisputeCall as C;
    use ReviewerView as V;
    let light = spec(DisputeRule::Reason, Unsettled::Accept, false);
    let standard = spec(DisputeRule::Evidence, Unsettled::Ask, false);
    let strict = spec(DisputeRule::Evidence, Unsettled::Ask, true);
    let lenient = spec(DisputeRule::Evidence, Unsettled::Accept, false);
    let table: [(&LevelSpec, V, bool, C); 8] = [
        (&light, V::Keeps, false, C::Accept),
        (&light, V::Accepts, false, C::Accept),
        (&standard, V::Accepts, false, C::Accept),
        (&standard, V::Keeps, false, C::AskUser),
        (&strict, V::Keeps, false, C::ArgueBack),
        (&strict, V::Keeps, true, C::AskUser),
        (&strict, V::Accepts, false, C::Accept),
        (&lenient, V::Keeps, false, C::Accept),
    ];
    for (level, view, argued, want) in table {
        assert_eq!(
            settle_dispute(level, view, argued),
            want,
            "{level:?} {view:?} argued {argued}"
        );
    }
}

#[test]
fn another_round_is_due_only_after_fixes_and_below_the_level_s_rounds() {
    let flow = code();
    let strict = flow.level(Level::Strict);
    assert!(
        round_due(strict, 1, true),
        "level 3 runs round 2 after fixes"
    );
    assert!(
        !round_due(strict, 1, false),
        "nothing fixed, nothing to re-review"
    );
    assert!(!round_due(strict, 2, true), "two rounds at most");
    assert!(
        !round_due(flow.level(Level::Standard), 1, true),
        "level 2 runs one round"
    );
}

fn trigger() -> impl Strategy<Value = ReviewTrigger> {
    prop_oneof![
        Just(ReviewTrigger::Start),
        Just(ReviewTrigger::Deliver),
        Just(ReviewTrigger::Resolve),
        Just(ReviewTrigger::Verdict),
        Just(ReviewTrigger::Answer),
        Just(ReviewTrigger::Finish),
        Just(ReviewTrigger::Cancel),
    ]
}

fn random_facts() -> impl Strategy<Value = ReviewFacts> {
    (any::<bool>(), 0..3_u32, 0..3_u32, 0..3_u32, any::<bool>()).prop_map(
        |(round_complete, awaiting_lead, awaiting_verdict, asked, round_due)| ReviewFacts {
            round_complete,
            awaiting_lead,
            awaiting_verdict,
            asked,
            round_due,
        },
    )
}

proptest! {
    #[test]
    fn random_review_sequences_keep_the_invariants(steps in proptest::collection::vec((trigger(), random_facts()), 0..60)) {
        let mut state = ReviewState::Requested;
        for (trigger, given) in steps {
            let before = state;
            if let Ok(next) = apply(state, trigger, given) {
                prop_assert!(before.is_open(), "a finished review moved");
                if next == ReviewState::Delivered && before == ReviewState::Running {
                    prop_assert!(given.round_complete, "delivered with a node open");
                }
                if next == ReviewState::Done {
                    prop_assert_eq!(
                        (given.awaiting_lead, given.awaiting_verdict, given.asked, given.round_due),
                        (0, 0, 0, false),
                        "finished with work left"
                    );
                }
                if before == ReviewState::Requested {
                    prop_assert!(matches!(next, ReviewState::Running | ReviewState::Cancelled), "started out of order");
                }
                state = next;
            }
        }
    }

    #[test]
    fn nodes_never_run_before_their_inputs(level in 1..=3_u8, picks in proptest::collection::vec((any::<prop::sample::Index>(), 0..3_usize, any::<bool>()), 0..80)) {
        let flow = code();
        let level = Level::from_number(level).unwrap();
        let mut progress = Progress::default();
        let mut next_finding = 1_i64;
        let mut order: Vec<Instance> = Vec::new();
        for (pick, findings, fail) in picks {
            let candidates = ready(&flow, level, &progress);
            if candidates.is_empty() {
                break;
            }
            let instance = pick.get(&candidates).clone();
            let node = flow.node(instance.node.as_str()).unwrap();
            for input in flow.inputs(node, level) {
                prop_assert!(
                    order.iter().any(|done| done.node == input.id),
                    "{} ran before its input {}", instance.node, input.id
                );
            }
            let state = if fail { NodeState::Failed } else { NodeState::Done };
            progress.instances.insert(instance.clone(), state);
            if instance.item == 0 && !fail {
                let ids: Vec<i64> = (0..findings).map(|n| next_finding + i64::try_from(n).unwrap()).collect();
                next_finding += i64::try_from(findings).unwrap();
                progress.findings.insert(instance.node.clone(), ids);
            }
            order.push(instance);
        }
        let taken: BTreeSet<&Instance> = order.iter().collect();
        prop_assert_eq!(taken.len(), order.len(), "an instance ran twice");
        if ready(&flow, level, &progress).is_empty() {
            prop_assert!(round_complete(&flow, level, &progress), "nothing ready yet the round is open");
        } else {
            prop_assert!(!round_complete(&flow, level, &progress), "complete with a node ready");
        }
    }
}
