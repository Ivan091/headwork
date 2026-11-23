//! `graph`: the agents working together, as nodes and edges (`docs/dashboard.md`), drawn by `headwork_core::graph`
//! from what the store holds.

use std::fmt::Write as _;

use anyhow::{Result, bail};
use headwork_core::graph::{Graph, Scope, build, task_node};
use headwork_store::Store;

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
/// Fails on a time that does not parse, a task name the store does not know, a task that was not queued yet at the
/// time asked for, or a read error.
pub fn graph(store: &Store, query: &GraphQuery) -> Result<Graph> {
    let at = query
        .at
        .as_deref()
        .map(|text| super::instant(store, text))
        .transpose()?;
    let (scope, wanted) = match &query.task {
        Some(name) => match store.find_task(name)? {
            Some(task) => (Scope::Task(task.id.0), Some((name, task.id.0))),
            None => bail!("no task named `{name}`"),
        },
        None if query.all => (Scope::All, None),
        None => (Scope::Live, None),
    };
    let graph = build(&store.graph_facts()?, at.as_deref(), scope);
    if let (Some((name, id)), Some(at)) = (wanted, &graph.at)
        && graph.node(&task_node(id)).is_none()
    {
        bail!("task `{name}` was not queued yet at {at}");
    }
    Ok(graph)
}

/// `text` with control characters shown as escapes, so agent-written labels cannot move the terminal's cursor.
fn printable(text: &str) -> String {
    let mut shown = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_control() {
            shown.extend(character.escape_default());
        } else {
            shown.push(character);
        }
    }
    shown
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
        let _ = writeln!(
            text,
            "{:indent$}{} {}{state}  {}",
            "",
            node.kind.as_str(),
            printable(&node.label),
            printable(&node.id),
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
        let label = edge
            .label
            .as_deref()
            .map_or_else(String::new, |label| format!(" \"{}\"", printable(label)));
        let times = if edge.weight > 1 {
            format!(" x{}", edge.weight)
        } else {
            String::new()
        };
        let _ = writeln!(
            text,
            "{} -{}-> {}{times}{label}",
            printable(&edge.source),
            edge.kind.as_str(),
            printable(&edge.target)
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use headwork_core::graph::{Edge, EdgeKind, Node, NodeKind};

    use super::*;

    fn node(id: &str, kind: NodeKind, label: &str, parent: Option<&str>) -> Node {
        Node {
            id: id.to_owned(),
            kind,
            label: label.to_owned(),
            state: Some("working".to_owned()),
            parent: parent.map(str::to_owned),
            start: None,
            end: None,
            details: BTreeMap::new(),
        }
    }

    #[test]
    fn render_nests_boxes_counts_edges_and_escapes_control_characters() {
        let graph = Graph {
            at: Some("2026-11-23T08:00:00.000Z".to_owned()),
            nodes: vec![
                node("task:1", NodeKind::Task, "headwork-lead", None),
                node("session:s1", NodeKind::Lead, "opus medium", Some("task:1")),
                node("external:x", NodeKind::External, "evil\u{1b}[2J", None),
            ],
            edges: vec![Edge {
                id: "message:session:s1->external:x".to_owned(),
                kind: EdgeKind::Message,
                source: "session:s1".to_owned(),
                target: "external:x".to_owned(),
                weight: 3,
                label: Some("hi\nthere".to_owned()),
                at: None,
            }],
        };
        let text = render(&graph);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.first().copied(),
            Some("at 2026-11-23T08:00:00.000Z"),
            "the instant"
        );
        assert!(
            lines.contains(&"task headwork-lead [working]  task:1"),
            "{text}"
        );
        assert!(
            lines.contains(&"  lead opus medium [working]  session:s1"),
            "indented under its box: {text}"
        );
        assert!(
            lines.contains(&r"external evil\u{1b}[2J [working]  external:x"),
            "escaped: {text}"
        );
        assert!(
            lines.contains(&r#"session:s1 -message-> external:x x3 "hi\nthere""#),
            "weight and label: {text}"
        );
        assert!(!text.contains('\u{1b}'), "no raw escape");
        assert_eq!(printable("plain"), "plain", "plain text untouched");
        assert!(render(&Graph::default()).is_empty(), "nothing to draw");
    }
}
