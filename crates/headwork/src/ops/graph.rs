//! `graph`: the agents working together, as nodes and edges (`docs/dashboard.md`), drawn by `headwork_core::graph`
//! from what the store holds.

use std::fmt::Write as _;

use anyhow::{Result, bail};
use headwork_core::graph::{Graph, Scope, build};
use headwork_store::Store;

use crate::ops::shadow::since_instant;

/// What to draw: the instant and the tasks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphQuery {
    /// An RFC 3339 time, a date, or a span back from now such as `2h`; now when none.
    pub at: Option<String>,
    /// Every task, closed ones too.
    pub all: bool,
    /// Only this task (with its lead and parts), by name.
    pub task: Option<String>,
}

/// The graph the query asks for.
///
/// # Errors
///
/// Fails on a time that does not parse, a task name the store does not know, or a read error.
pub fn graph(store: &Store, query: &GraphQuery) -> Result<Graph> {
    let at = query
        .at
        .as_deref()
        .map(|text| since_instant(store, text))
        .transpose()?;
    let scope = match &query.task {
        Some(name) => match store.find_task(name)? {
            Some(task) => Scope::Task(task.id.0),
            None => bail!("no task named `{name}`"),
        },
        None if query.all => Scope::All,
        None => Scope::Live,
    };
    Ok(build(&store.graph_facts()?, at.as_deref(), scope))
}

/// The graph as text: one line per node, indented under the box it sits in, then one line per edge.
#[must_use]
pub fn render(graph: &Graph) -> String {
    let mut text = String::new();
    if let Some(at) = &graph.at {
        let _ = writeln!(text, "at {at}");
    }
    let mut stack: Vec<(&str, usize)> = graph
        .nodes
        .iter()
        .filter(|node| node.parent.is_none())
        .rev()
        .map(|node| (node.id.as_str(), 0))
        .collect();
    while let Some((id, depth)) = stack.pop() {
        let Some(node) = graph.node(id) else {
            continue;
        };
        let state = node
            .state
            .as_deref()
            .map_or_else(String::new, |state| format!(" [{state}]"));
        let kind = serde_json::to_value(node.kind)
            .ok()
            .and_then(|kind| kind.as_str().map(str::to_owned))
            .unwrap_or_default();
        let _ = writeln!(
            text,
            "{:indent$}{kind} {}{state}  {}",
            "",
            node.label,
            node.id,
            indent = depth * 2
        );
        stack.extend(
            graph
                .nodes
                .iter()
                .filter(|child| child.parent.as_deref() == Some(id))
                .rev()
                .map(|child| (child.id.as_str(), depth + 1)),
        );
    }
    for edge in &graph.edges {
        let kind = serde_json::to_value(edge.kind)
            .ok()
            .and_then(|kind| kind.as_str().map(str::to_owned))
            .unwrap_or_default();
        let label = edge
            .label
            .as_deref()
            .map_or_else(String::new, |label| format!(" \"{label}\""));
        let times = if edge.weight > 1 {
            format!(" x{}", edge.weight)
        } else {
            String::new()
        };
        let _ = writeln!(
            text,
            "{} -{kind}-> {}{times}{label}",
            edge.source, edge.target
        );
    }
    text
}
