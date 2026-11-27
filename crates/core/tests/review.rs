//! Reviews: the command parser, flow validation, ready-node scheduling, the state machine and the dispute rule, as
//! tables plus property tests.

#![expect(clippy::unwrap_used, clippy::expect_used, reason = "tests fail loudly")]

use std::collections::BTreeSet;

use headwork_core::review::{
    After, DisputeCall, DisputeRule, Each, FindingText, Flow, FlowError, FlowName, Instance, Level,
    LevelSpec, NodeId, ParsedCommand, Progress, ReviewCommand, ReviewFacts, ReviewRefusal,
    ReviewTarget, ReviewTrigger, ReviewerView, Target, Unsettled, VERDICTS_SHAPE, apply, compose,
    members, parse_command, ready, round_complete, round_due, round_scope, settle_dispute,
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
fn the_plan_flow_runs_more_nodes_at_each_level() {
    let flow = Flow::parse(include_str!("../../../flows/plan.toml"))
        .expect("the built-in plan flow is valid");
    let ids = |level| -> Vec<String> {
        flow.nodes_at(level)
            .map(|node| node.id.to_string())
            .collect()
    };
    assert_eq!(ids(Level::Light), ["feasibility", "consistency"], "level 1");
    assert_eq!(
        ids(Level::Standard),
        [
            "feasibility",
            "consistency",
            "open-points",
            "effort",
            "verify"
        ],
        "level 2"
    );
    assert_eq!(
        ids(Level::Strict),
        [
            "feasibility",
            "consistency",
            "open-points",
            "effort",
            "challenge-scope",
            "verify"
        ],
        "level 3"
    );
    let feasibility = flow.node("feasibility").unwrap();
    assert_eq!(
        feasibility.model_at(Level::Light),
        Some(Model::Sonnet),
        "sonnet at 1"
    );
    assert_eq!(
        feasibility.model_at(Level::Standard),
        Some(Model::Opus),
        "opus at 2"
    );
    assert_eq!(
        feasibility.model_at(Level::Strict),
        Some(Model::Opus),
        "opus at 3"
    );
    let verify = flow.node("verify").unwrap();
    assert_eq!(
        verify.after,
        After::Earlier,
        "verify waits for every earlier node"
    );
    assert_eq!(
        flow.inputs(verify, Level::Standard).len(),
        4,
        "verify's inputs at level 2"
    );
    assert_eq!(
        flow.inputs(verify, Level::Strict).len(),
        5,
        "verify's inputs at level 3"
    );
    assert_eq!(
        flow.level(Level::Strict).rounds,
        2,
        "level 3 runs two rounds"
    );
    assert!(
        flow.check(|_| true, Model::ALL).is_empty(),
        "the plan flow is valid"
    );
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
#[expect(clippy::too_many_lines, reason = "one table of every flow error")]
fn broken_flows_are_refused_with_the_reason() {
    let node = |id: &str, extra: &str| {
        format!("[[node]]\nid = \"{id}\"\nprompt = \"p.md\"\nmodel = \"sonnet\"\n{extra}\n")
    };
    let table: [(String, FlowError); 12] = [
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
        (
            node("a", "")
                + &node(
                    "v",
                    "after = [\"a\"]\nfrom_level = 2\neach = { 3 = \"file\" }",
                ),
            FlowError::EachMissing {
                node: "v".to_owned(),
                level: Level::Standard,
            },
        ),
        (
            node("a", "")
                + &node(
                    "v",
                    "after = [\"a\"]\neach = { 1 = \"node\", 2 = \"node\", 3 = \"node\", 4 = \"file\" }",
                ),
            FlowError::EachKey {
                node: "v".to_owned(),
                key: "4".to_owned(),
            },
        ),
        (
            node("a", "") + &node("v", "after = [\"a\"]\neach = \"node\"\nevery_round = true"),
            FlowError::EveryRoundEach("v".to_owned()),
        ),
    ];
    for (nodes, want) in table {
        assert_eq!(flow_with(&nodes), Err(want), "{nodes}");
    }
    let missing = Flow::parse(concat!(
        "name = \"t\"\ndescription = \"d\"\ntarget = \"diff\"\n",
        "[levels.1]\npushback = \"a\"\ndispute = \"reason\"\nunsettled = \"accept\"\nrounds = 1\n",
    ));
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
    let flow = flow_with(concat!(
        "[[node]]\nid = \"a\"\nprompt = \"p.md\"\nmodel = \"sonnet\"\n",
        "[[node]]\nid = \"b\"\nprompt = \"p.md\"\nfrom_level = 2\nmodel = { 2 = \"sonnet\", 3 = \"opus\" }\n",
    ));
    assert!(flow.is_ok(), "{flow:?}");
}

fn finish(progress: &mut Progress, node: &str, item: i64, findings: &[i64]) {
    let instance = Instance {
        node: NodeId::new(node).unwrap(),
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
        node: NodeId::new("correctness").unwrap(),
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
fn another_round_is_due_only_after_an_important_fix_and_below_the_level_s_rounds() {
    let flow = code();
    let strict = flow.level(Level::Strict);
    assert!(
        round_due(strict, 1, true, false),
        "level 3 runs round 2 after an important fix"
    );
    assert!(
        !round_due(strict, 1, false, false),
        "no important fix (only doubts or nits, or nothing), no second round"
    );
    assert!(!round_due(strict, 2, true, false), "two rounds at most");
    assert!(
        !round_due(flow.level(Level::Standard), 1, true, false),
        "level 2 runs one round"
    );
}

#[test]
fn late_commits_earn_exactly_one_more_round() {
    let flow = code();
    let table = [
        (Level::Light, 1, false, true, true),
        (Level::Light, 2, false, true, false),
        (Level::Light, 1, false, false, false),
        (Level::Strict, 1, false, true, true),
        (Level::Strict, 2, false, true, true),
        (Level::Strict, 3, false, true, false),
        (Level::Strict, 2, true, false, false),
        (Level::Strict, 1, true, true, true),
    ];
    for (level, round, fixed, late, want) in table {
        assert_eq!(
            round_due(flow.level(level), round, fixed, late),
            want,
            "{level:?} round {round} fixed {fixed} late {late}"
        );
    }
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
    fn nodes_never_run_before_their_inputs(
        level in 1..=3_u8,
        picks in proptest::collection::vec((any::<prop::sample::Index>(), 0..3_usize, any::<bool>()), 0..80),
    ) {
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
            if node.fans_out() {
                let group = members(&flow, level, &progress, &instance);
                prop_assert!(!group.is_empty(), "{}#{} checks nothing", instance.node, instance.item);
                let source = progress
                    .findings
                    .iter()
                    .find(|(_, ids)| ids.contains(&instance.item))
                    .map(|(source, _)| source.clone())
                    .unwrap();
                prop_assert!(
                    group.iter().all(|id| progress.findings[&source].contains(id)),
                    "{}#{} mixes nodes", instance.node, instance.item
                );
                prop_assert!(
                    order.iter().any(|done| done.node == source),
                    "{}#{} ran before its input {}", instance.node, instance.item, source
                );
            } else {
                for input in flow.inputs(node, level) {
                    prop_assert!(
                        order.iter().any(|done| done.node == input.id),
                        "{} ran before its input {}", instance.node, input.id
                    );
                }
            }
            let state = if fail { NodeState::Failed } else { NodeState::Done };
            progress.instances.insert(instance.clone(), state);
            if instance.item == 0 && !fail {
                let ids: Vec<i64> = (0..findings).map(|n| next_finding + i64::try_from(n).unwrap()).collect();
                next_finding += i64::try_from(findings).unwrap();
                for id in &ids {
                    let file = (id % 4 != 0).then(|| format!("f{}.rs", id % 3));
                    progress.files.insert(*id, file);
                }
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

fn id(name: &str) -> NodeId {
    NodeId::new(name).unwrap()
}

fn instance(node: &str, item: i64) -> Instance {
    Instance {
        node: id(node),
        item,
    }
}

#[test]
fn verify_starts_on_a_node_s_findings_as_soon_as_that_node_is_done() {
    let flow = code();
    let mut progress = Progress::default();
    finish(&mut progress, "correctness", 0, &[1, 2]);
    let now = ready(&flow, Level::Standard, &progress);
    assert!(
        names(&now).contains(&"verify#1".to_owned()),
        "verify runs on correctness's findings while the rest still run: {:?}",
        names(&now)
    );
    assert_eq!(
        now.len(),
        6,
        "five reviewers and one verifier: {:?}",
        names(&now)
    );
    assert_eq!(
        members(&flow, Level::Standard, &progress, &instance("verify", 1)),
        [1, 2],
        "level 2 checks all of a node's findings in one instance"
    );
    finish(&mut progress, "verify", 1, &[]);
    assert!(
        !round_complete(&flow, Level::Standard, &progress),
        "the other reviewers keep the round open"
    );
    for node in ["tests", "rules", "errors", "design"] {
        finish(&mut progress, node, 0, &[]);
    }
    assert!(
        !round_complete(&flow, Level::Standard, &progress),
        "docs still runs, and verify waits on it"
    );
    finish(&mut progress, "docs", 0, &[3]);
    assert_eq!(
        names(&ready(&flow, Level::Standard, &progress)),
        ["verify#3"],
        "the last reviewer's finding gets its own verifier"
    );
    finish(&mut progress, "verify", 3, &[]);
    assert!(
        round_complete(&flow, Level::Standard, &progress),
        "every input and every instance is over"
    );
}

#[test]
fn level_three_verifies_per_node_and_file() {
    let flow = code();
    let mut progress = Progress::default();
    finish(&mut progress, "correctness", 0, &[1, 2, 3, 4]);
    finish(&mut progress, "tests", 0, &[5]);
    for (finding, file) in [
        (1, Some("a.rs")),
        (2, Some("b.rs")),
        (3, Some("a.rs")),
        (4, None),
        (5, Some("a.rs")),
    ] {
        progress.files.insert(finding, file.map(str::to_owned));
    }
    let verify: Vec<String> = names(&ready(&flow, Level::Strict, &progress))
        .into_iter()
        .filter(|name| name.starts_with("verify"))
        .collect();
    assert_eq!(
        verify,
        ["verify#1", "verify#2", "verify#4", "verify#5"],
        "one instance per (node, file), findings without a file grouped per node"
    );
    let group = |item| members(&flow, Level::Strict, &progress, &instance("verify", item));
    assert_eq!(group(1), [1, 3], "correctness in a.rs");
    assert_eq!(group(2), [2], "correctness in b.rs");
    assert_eq!(group(4), [4], "correctness with no file");
    assert_eq!(
        group(5),
        [5],
        "tests in a.rs stays apart from correctness in a.rs"
    );
    assert!(group(3).is_empty(), "3 is a member, not an item");
}

#[test]
fn an_each_map_picks_the_fan_out_per_level() {
    let flow = flow_with(concat!(
        "[[node]]\nid = \"a\"\nprompt = \"p.md\"\nmodel = \"sonnet\"\n",
        "[[node]]\nid = \"v\"\nprompt = \"p.md\"\nmodel = \"sonnet\"\nafter = [\"a\"]\n",
        "each = { 1 = \"finding\", 2 = \"node\", 3 = \"file\" }\n",
    ))
    .unwrap();
    let mut progress = Progress::default();
    finish(&mut progress, "a", 0, &[1, 2]);
    progress.files.insert(1, Some("x.rs".to_owned()));
    progress.files.insert(2, Some("y.rs".to_owned()));
    let verify = |level| names(&ready(&flow, level, &progress));
    assert_eq!(verify(Level::Light), ["v#1", "v#2"], "per finding at 1");
    assert_eq!(verify(Level::Standard), ["v#1"], "per node at 2");
    assert_eq!(verify(Level::Strict), ["v#1", "v#2"], "per file at 3");
    let code = code();
    let node = code.node("verify").unwrap();
    assert_eq!(
        (node.each_at(Level::Standard), node.each_at(Level::Strict)),
        (Some(Each::Node), Some(Each::File)),
        "the code flow batches verify"
    );
}

#[test]
fn a_later_round_runs_the_every_round_nodes_and_those_with_a_fixed_important_finding() {
    let flow = code();
    assert!(
        flow.node("correctness").unwrap().every_round && !flow.node("tests").unwrap().every_round,
        "the code flow marks correctness only"
    );
    let fixed: BTreeSet<NodeId> = [id("design"), id("verify")].into_iter().collect();
    let scope = round_scope(&flow, Level::Strict, &fixed).unwrap();
    assert_eq!(
        scope,
        [id("correctness"), id("design")].into_iter().collect(),
        "correctness every round, design for its fixed important finding, verify is no plain node"
    );
    let plan = Flow::parse(include_str!("../../../flows/plan.toml")).unwrap();
    assert_eq!(
        round_scope(&plan, Level::Strict, &fixed),
        None,
        "a flow with no every_round node keeps whole rounds"
    );
    let marked = flow_with(concat!(
        "[[node]]\nid = \"a\"\nprompt = \"p.md\"\nmodel = \"sonnet\"\n",
        "[[node]]\nid = \"b\"\nprompt = \"p.md\"\nmodel = \"sonnet\"\nfrom_level = 3\nevery_round = true\n",
    ))
    .unwrap();
    assert_eq!(
        round_scope(&marked, Level::Standard, &fixed),
        None,
        "a marked node that does not run at the level narrows nothing"
    );
    assert_eq!(
        round_scope(&marked, Level::Strict, &BTreeSet::new()),
        Some(std::iter::once(id("b")).collect()),
        "only the marked node when nothing was fixed"
    );
    let mut progress = Progress {
        active: Some(scope),
        ..Progress::default()
    };
    assert_eq!(
        names(&ready(&flow, Level::Strict, &progress)),
        ["correctness#0", "design#0"],
        "inactive nodes never start"
    );
    finish(&mut progress, "correctness", 0, &[]);
    finish(&mut progress, "design", 0, &[9]);
    assert_eq!(
        names(&ready(&flow, Level::Strict, &progress)),
        ["verify#9"],
        "verify still runs, and the inactive nodes count as complete"
    );
    finish(&mut progress, "verify", 9, &[]);
    assert!(
        round_complete(&flow, Level::Strict, &progress),
        "the round is over"
    );
}

fn target(fixes_only: bool) -> ReviewTarget {
    ReviewTarget {
        kind: Target::Diff,
        base: "b1".to_owned(),
        head: "h1".to_owned(),
        worktree: "S:/w".to_owned(),
        changed: vec!["src/a.rs".to_owned(), "src/b.rs".to_owned()],
        fixes_only,
    }
}

fn finding_text(id: i64) -> FindingText {
    FindingText {
        id,
        node: "correctness".to_owned(),
        place: Some("src/a.rs:3".to_owned()),
        severity: "important".to_owned(),
        summary: "s".to_owned(),
        scenario: "sc".to_owned(),
    }
}

const LEVEL_ONE_PROMPT: &str = concat!(
    "# Correctness\n\nFind defects.\n\n",
    "## How hard to push (level 1)\n\n",
    "Report only defects you can show with a concrete failing scenario: the inputs or state, and the wrong result or ",
    "crash. No style remarks, no risks without a scenario. When unsure, leave it out.\n\n",
    "## What you review\n\n",
    "The change is the diff `b1..h1`. Read it with `git -C S:/w diff b1 h1` and the code around it in the worktree ",
    "`S:/w`, checked out at `h1`.\n\n",
    "Changed files:\n\n- src/a.rs\n- src/b.rs\n\n",
    "The user asked to look most at: the parser\n\n",
    "You only read: never edit, commit, check out, build or run tests; use Read, Grep, Glob and read-only git commands ",
    "in the worktree.\n\n",
    "## Your answer\n\n",
    "Answer with a JSON array and nothing else, one object per finding: [{\"file\": \"src/x.rs\", \"line\": 12, ",
    "\"severity\": \"important\", \"summary\": \"what is wrong\", \"scenario\": \"the concrete inputs or state and the ",
    "wrong result\"}]. `severity` is `important` (a defect), `nit` (a minor issue) or `doubt` (about the approach or the ",
    "scope); `file` and `line` may be null when the finding names no place. Answer [] when you find nothing.\n",
);

#[test]
fn level_one_is_unchanged_by_pipelining_batching_and_scoping() {
    let flow = code();
    let mut progress = Progress::default();
    assert_eq!(
        names(&ready(&flow, Level::Light, &progress)),
        ["correctness#0", "tests#0", "rules#0"],
        "level 1 starts its three reviewers"
    );
    finish(&mut progress, "correctness", 0, &[1, 2]);
    assert_eq!(
        names(&ready(&flow, Level::Light, &progress)),
        ["tests#0", "rules#0"],
        "no verifier at level 1"
    );
    finish(&mut progress, "tests", 0, &[]);
    finish(&mut progress, "rules", 0, &[3]);
    assert!(
        round_complete(&flow, Level::Light, &progress),
        "three nodes, one round"
    );
    let node = flow.node("correctness").unwrap();
    let text = compose(
        node,
        "# Correctness\n\nFind defects.\n",
        Level::Light,
        flow.level(Level::Light),
        &target(false),
        "the parser",
        &[],
    );
    assert_eq!(text, LEVEL_ONE_PROMPT, "the level 1 prompt, byte for byte");
}

#[test]
fn one_finding_keeps_the_single_verdict_prompt_and_several_ask_for_an_array() {
    let flow = code();
    let verify = flow.node("verify").unwrap();
    let spec = flow.level(Level::Standard);
    let one = compose(
        verify,
        "# Verifier\n",
        Level::Standard,
        spec,
        &target(false),
        "",
        &[finding_text(7)],
    );
    assert!(
        one.ends_with(concat!(
            "## The finding to check (number 7)\n\n",
            "- Reported by: correctness\n- Place: src/a.rs:3\n- Severity: important\n- Summary: s\n- Scenario: sc\n\n",
            "## Your answer\n\n",
            "Answer with a JSON object and nothing else: {\"verdict\": \"confirmed\", \"evidence\": \"the lines you read ",
            "and what they show\"}. `verdict` is `confirmed`, `plausible` or `rejected`.\n",
        )),
        "{one}"
    );
    let several = compose(
        verify,
        "# Verifier\n",
        Level::Standard,
        spec,
        &target(false),
        "",
        &[finding_text(3), finding_text(5)],
    );
    assert!(
        several.contains("## The findings to check (numbers 3, 5)\n"),
        "{several}"
    );
    assert!(
        several.contains("### Finding 3\n") && several.contains("### Finding 5\n"),
        "{several}"
    );
    assert!(
        several.ends_with(&format!("## Your answer\n\n{VERDICTS_SHAPE}\n")),
        "{several}"
    );
    assert!(!several.contains("number 3)"), "{several}");
}

#[test]
fn a_fix_round_names_its_scope_and_round_one_does_not() {
    let flow = code();
    let node = flow.node("correctness").unwrap();
    let spec = flow.level(Level::Strict);
    let scoped = compose(node, "# C\n", Level::Strict, spec, &target(true), "", &[]);
    assert!(
        scoped.contains("This round checks only the fix commits `b1..h1`"),
        "{scoped}"
    );
    assert!(scoped.contains("direct callers"), "{scoped}");
    let full = compose(node, "# C\n", Level::Strict, spec, &target(false), "", &[]);
    assert!(!full.contains("fix commits"), "{full}");
}
