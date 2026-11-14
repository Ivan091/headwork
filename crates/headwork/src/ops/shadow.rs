//! `shadow-report`: the would-deny events the hooks logged in shadow mode, grouped by rule id.
//!
//! The user reads it to see which denies are right and which would have been false before `hooks.enforce` is turned on
//! (design section 7.3, step 2).

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use headwork_store::{EVENT_WOULD_DENY, EventFilter, Store};
use serde::Serialize;
use serde_json::Value;

/// How many sample inputs each rule shows.
const SAMPLES: usize = 3;

/// One logged would-deny.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Sample {
    /// When.
    pub at: String,
    /// The task, none for the root.
    pub task: Option<String>,
    /// The tool.
    pub tool: String,
    /// What the call was, shortened.
    pub input: String,
}

/// What one rule would have denied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleHits {
    /// The rule id.
    pub rule: String,
    /// How many times.
    pub count: usize,
    /// The reason the rule gives.
    pub reason: String,
    /// The tools it fired on.
    pub tools: Vec<String>,
    /// The tasks it fired in.
    pub tasks: Vec<String>,
    /// The newest calls, newest first.
    pub samples: Vec<Sample>,
}

/// The shadow log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShadowReport {
    /// Whether the hooks enforce now.
    pub enforcing: bool,
    /// Events in the log.
    pub events: usize,
    /// Days with at least one event, as `YYYY-MM-DD`.
    pub days: Vec<String>,
    /// Sessions that wrote events.
    pub sessions: usize,
    /// Would-deny events in all.
    pub would_deny: usize,
    /// By rule, most frequent first.
    pub rules: Vec<RuleHits>,
}

/// Reads the log and groups the would-deny events by rule.
///
/// # Errors
///
/// Fails when the log cannot be read.
pub fn shadow_report(store: &Store, enforcing: bool) -> Result<ShadowReport> {
    let events = store.events(EventFilter {
        task: None,
        limit: u32::MAX,
    })?;
    let days: BTreeSet<String> = events
        .iter()
        .filter_map(|event| event.at.get(..10).map(str::to_owned))
        .collect();
    let sessions: BTreeSet<&str> = events
        .iter()
        .filter_map(|event| event.session_id.as_deref())
        .collect();
    let mut by_rule: BTreeMap<String, RuleHits> = BTreeMap::new();
    let mut would_deny = 0;
    for event in events
        .iter()
        .rev()
        .filter(|event| event.kind == EVENT_WOULD_DENY)
    {
        would_deny += 1;
        let text = |key: &str| {
            event
                .data
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let hits = by_rule
            .entry(text("rule"))
            .or_insert_with_key(|rule| RuleHits {
                rule: rule.clone(),
                count: 0,
                reason: text("reason"),
                tools: Vec::new(),
                tasks: Vec::new(),
                samples: Vec::new(),
            });
        hits.count += 1;
        let tool = text("tool");
        if !hits.tools.contains(&tool) {
            hits.tools.push(tool.clone());
        }
        let task = event.task.clone().unwrap_or_else(|| "root".to_owned());
        if !hits.tasks.contains(&task) {
            hits.tasks.push(task);
        }
        if hits.samples.len() < SAMPLES {
            hits.samples.push(Sample {
                at: event.at.clone(),
                task: event.task.clone(),
                tool,
                input: text("input"),
            });
        }
    }
    let mut rules: Vec<RuleHits> = by_rule.into_values().collect();
    rules.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.rule.cmp(&b.rule)));
    Ok(ShadowReport {
        enforcing,
        events: events.len(),
        days: days.into_iter().collect(),
        sessions: sessions.len(),
        would_deny,
        rules,
    })
}
