//! `graph`: the agents working together, as nodes and edges (`docs/dashboard.md`), drawn by `headwork_core::graph`
//! from what the store holds.

use anyhow::{Result, bail};
use headwork_core::graph::{Graph, Scope, build, task_named};
use headwork_store::Store;

/// Which tasks to draw.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Which {
    /// The tasks not over, with the lead of every live part.
    #[default]
    Live,
    /// Every task, closed ones too.
    All,
    /// One task by name, with its lead and parts: the one of that name that existed at the instant.
    Task(String),
}

/// What to draw: the instant and the tasks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphQuery {
    /// An RFC 3339 time, a date, or a span back from now such as `2h`; now when none.
    pub at: Option<String>,
    /// Which tasks.
    pub which: Which,
}

/// The graph the query asks for.
///
/// # Errors
///
/// Fails on a time that does not parse or lies in the future, a task name no task had by the instant, or a read
/// error.
pub fn graph(store: &Store, query: &GraphQuery) -> Result<Graph> {
    let at = query
        .at
        .as_deref()
        .map(|text| super::instant(store, text))
        .transpose()?;
    if let Some(at) = &at {
        let now = store.instant_ago(0)?;
        if at.as_str() > now.as_str() {
            bail!("{at} is in the future (now is {now})");
        }
    }
    let facts = store.graph_facts()?;
    let scope = match &query.which {
        Which::Live => Scope::Live,
        Which::All => Scope::All,
        Which::Task(name) => match task_named(&facts, name, at.as_deref()) {
            Some(task) => Scope::Task(task.id),
            None => match &at {
                Some(at) => bail!("no task named `{name}` was queued by {at}"),
                None => bail!("no task named `{name}`"),
            },
        },
    };
    Ok(build(&facts, at.as_deref(), scope))
}
