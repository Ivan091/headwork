//! Which node instances of a round are ready, from the flow, the level and what has run so far.
//!
//! A plain node waits for all its inputs. An `each` node is pipelined: the instances for one input's findings become
//! ready as soon as that input is complete, so verifying starts while slower reviewers still run. Its groups are
//! formed only from a completed input's findings, so they never change once ready.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::Level;
use super::flow::{Each, Flow, Node, NodeId};
use crate::vocabulary::NodeState;

/// The node every later round runs whatever the previous one found, when the flow has it.
pub const ANCHOR_NODE: &str = "correctness";

/// One run of a node: a plain node runs once (item 0); an `each` node once per group of its inputs' findings (item =
/// the smallest finding id of the group).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Instance {
    /// The node.
    pub node: NodeId,
    /// 0 for a plain node, the smallest finding id of its group for an `each` node.
    pub item: i64,
}

/// What a round has run so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    /// Every instance taken so far, with its state.
    pub instances: BTreeMap<Instance, NodeState>,
    /// The ids of the findings each node returned.
    pub findings: BTreeMap<NodeId, Vec<i64>>,
    /// The file each finding names, by finding id; a finding missing here names none.
    pub files: BTreeMap<i64, Option<String>>,
    /// The plain nodes that run this round; `None` runs every one. An inactive plain node counts as complete with no
    /// findings and is never ready; `each` nodes always run.
    pub active: Option<BTreeSet<NodeId>>,
}

impl Progress {
    fn state(&self, node: &NodeId, item: i64) -> Option<NodeState> {
        self.instances
            .get(&Instance {
                node: node.clone(),
                item,
            })
            .copied()
    }

    fn runs(&self, node: &Node) -> bool {
        node.fans_out()
            || self
                .active
                .as_ref()
                .is_none_or(|active| active.contains(&node.id))
    }

    /// The groups of an `each` node over the findings of its complete inputs, keyed by item.
    fn groups(&self, flow: &Flow, node: &Node, level: Level) -> BTreeMap<i64, Vec<i64>> {
        let mode = node.each_at(level).unwrap_or(Each::Finding);
        let mut groups = BTreeMap::new();
        for input in flow.inputs(node, level) {
            if input.fans_out() || !self.complete(flow, input, level) {
                continue;
            }
            let ids = self.findings.get(&input.id).map_or(&[][..], Vec::as_slice);
            let mut sets: Vec<Vec<i64>> = match mode {
                Each::Finding => ids.iter().map(|id| vec![*id]).collect(),
                Each::Node => vec![ids.to_vec()],
                Each::File => {
                    let mut by_file: BTreeMap<Option<&str>, Vec<i64>> = BTreeMap::new();
                    for id in ids {
                        let file = self.files.get(id).and_then(Option::as_deref);
                        by_file.entry(file).or_default().push(*id);
                    }
                    by_file.into_values().collect()
                }
            };
            for set in &mut sets {
                set.sort_unstable();
            }
            for set in sets {
                if let Some(first) = set.first() {
                    groups.insert(*first, set);
                }
            }
        }
        groups
    }

    fn complete(&self, flow: &Flow, node: &Node, level: Level) -> bool {
        if !self.runs(node) {
            return true;
        }
        if !node.fans_out() {
            return self.state(&node.id, 0).is_some_and(NodeState::is_over);
        }
        self.inputs_complete(flow, node, level)
            && self
                .groups(flow, node, level)
                .into_keys()
                .all(|item| self.state(&node.id, item).is_some_and(NodeState::is_over))
    }

    fn inputs_complete(&self, flow: &Flow, node: &Node, level: Level) -> bool {
        flow.inputs(node, level)
            .into_iter()
            .all(|input| self.complete(flow, input, level))
    }
}

/// The instances that may start now: not yet taken, and for a plain node every input complete, for an `each` node the
/// input its group comes from.
///
/// A plain node is complete when its one instance is done or failed, or when it does not run this round; an `each`
/// node when its inputs are and every group of theirs has its instance over.
#[must_use]
pub fn ready(flow: &Flow, level: Level, progress: &Progress) -> Vec<Instance> {
    let mut found = Vec::new();
    for node in flow.nodes_at(level) {
        if !progress.runs(node) {
            continue;
        }
        if node.fans_out() {
            found.extend(
                progress
                    .groups(flow, node, level)
                    .into_keys()
                    .filter(|item| progress.state(&node.id, *item).is_none())
                    .map(|item| Instance {
                        node: node.id.clone(),
                        item,
                    }),
            );
            continue;
        }
        if progress.inputs_complete(flow, node, level) && progress.state(&node.id, 0).is_none() {
            found.push(Instance {
                node: node.id.clone(),
                item: 0,
            });
        }
    }
    found
}

/// The findings an instance checks: its group for an `each` node, in id order; empty for a plain node or an item that
/// names no group of a complete input.
#[must_use]
pub fn members(flow: &Flow, level: Level, progress: &Progress, instance: &Instance) -> Vec<i64> {
    flow.node(instance.node.as_str())
        .filter(|node| node.fans_out() && node.runs_at(level))
        .and_then(|node| progress.groups(flow, node, level).remove(&instance.item))
        .unwrap_or_default()
}

/// The plain nodes a later round runs: those that reported an `important` finding in the previous round, plus
/// [`ANCHOR_NODE`]; `None` (every node) when the flow has no such node.
#[must_use]
pub fn round_scope(
    flow: &Flow,
    level: Level,
    important: &BTreeSet<NodeId>,
) -> Option<BTreeSet<NodeId>> {
    let anchor = flow
        .nodes_at(level)
        .find(|node| node.id.as_str() == ANCHOR_NODE && !node.fans_out())?;
    let mut active: BTreeSet<NodeId> = flow
        .nodes_at(level)
        .filter(|node| !node.fans_out() && important.contains(&node.id))
        .map(|node| node.id.clone())
        .collect();
    active.insert(anchor.id.clone());
    Some(active)
}

/// Whether every node that runs at the level is complete, so the round may deliver.
#[must_use]
pub fn round_complete(flow: &Flow, level: Level, progress: &Progress) -> bool {
    flow.nodes_at(level)
        .all(|node| progress.complete(flow, node, level))
}
