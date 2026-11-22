//! Which node instances of a round are ready, from the flow, the level and what has run so far.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::Level;
use super::flow::{Flow, Node, NodeId};
use crate::vocabulary::NodeState;

/// One run of a node: a plain node runs once (item 0); an `each` node once per finding of its inputs (item = the
/// finding's id).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Instance {
    /// The node.
    pub node: NodeId,
    /// 0 for a plain node, the finding's id for an `each` node.
    pub item: i64,
}

/// What a round has run so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    /// Every instance taken so far, with its state.
    pub instances: BTreeMap<Instance, NodeState>,
    /// The ids of the findings each node returned.
    pub findings: BTreeMap<NodeId, Vec<i64>>,
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

    fn items(&self, flow: &Flow, node: &Node, level: Level) -> BTreeSet<i64> {
        flow.inputs(node, level)
            .into_iter()
            .filter(|input| input.each.is_none())
            .flat_map(|input| self.findings.get(&input.id).into_iter().flatten().copied())
            .collect()
    }

    fn complete(&self, flow: &Flow, node: &Node, level: Level) -> bool {
        if node.each.is_none() {
            return self.state(&node.id, 0).is_some_and(NodeState::is_over);
        }
        self.inputs_complete(flow, node, level)
            && self
                .items(flow, node, level)
                .into_iter()
                .all(|item| self.state(&node.id, item).is_some_and(NodeState::is_over))
    }

    fn inputs_complete(&self, flow: &Flow, node: &Node, level: Level) -> bool {
        flow.inputs(node, level)
            .into_iter()
            .all(|input| self.complete(flow, input, level))
    }
}

/// The instances that may start now: not yet taken, every input complete.
///
/// A plain node is complete when its one instance is done or failed; an `each` node when its inputs are and every
/// finding of theirs has its instance over.
#[must_use]
pub fn ready(flow: &Flow, level: Level, progress: &Progress) -> Vec<Instance> {
    let mut found = Vec::new();
    for node in flow.nodes_at(level) {
        if !progress.inputs_complete(flow, node, level) {
            continue;
        }
        if node.each.is_none() {
            if progress.state(&node.id, 0).is_none() {
                found.push(Instance {
                    node: node.id.clone(),
                    item: 0,
                });
            }
            continue;
        }
        found.extend(
            progress
                .items(flow, node, level)
                .into_iter()
                .filter(|item| progress.state(&node.id, *item).is_none())
                .map(|item| Instance {
                    node: node.id.clone(),
                    item,
                }),
        );
    }
    found
}

/// Whether every node that runs at the level is complete, so the round may deliver.
#[must_use]
pub fn round_complete(flow: &Flow, level: Level, progress: &Progress) -> bool {
    flow.nodes_at(level)
        .all(|node| progress.complete(flow, node, level))
}
