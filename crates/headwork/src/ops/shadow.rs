//! `shadow-report`: the would-deny events the hooks logged in shadow mode, grouped by rule id.
//!
//! The user reads it to see which denies are right and which would have been false before `hooks.enforce` is turned on
//! (design section 7.3, step 2).

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use headwork_core::RuleId;
use headwork_core::config::HooksConfig;
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
    /// The task, none for the orchestrator.
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
    /// Whether a deny by this rule blocks now: `hooks.enforce`, or the rule is in `hooks.enforce_rules`.
    pub enforced: bool,
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
    /// Whether the hooks enforce every rule now.
    pub enforcing: bool,
    /// The rules that block while `enforcing` is off.
    pub enforce_rules: Vec<String>,
    /// The start of the window, as an instant in UTC; none when the whole log is read.
    pub since: Option<String>,
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

/// Turns `--since` into an instant in UTC: a span back from now (`90m`, `24h`, `7d`), or an RFC 3339 date and time or
/// a plain date.
fn since_instant(store: &Store, text: &str) -> Result<String> {
    let text = text.trim();
    let span = text.char_indices().last().and_then(|(at, unit)| {
        let seconds: u64 = match unit {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            _ => return None,
        };
        text.get(..at)?
            .parse::<u64>()
            .ok()
            .and_then(|amount| amount.checked_mul(seconds))
    });
    if let Some(seconds) = span {
        return Ok(store.instant_ago(seconds)?);
    }
    match store.instant_of(text)? {
        Some(instant) => Ok(instant),
        None => bail!(
            "cannot read `{text}` as a time: use YYYY-MM-DD, an RFC 3339 date and time, or a span such as 24h or 7d"
        ),
    }
}

/// Reads the log and groups the would-deny events by rule, from `since` on when given.
///
/// # Errors
///
/// Fails when the log cannot be read or `since` is not a time.
pub fn shadow_report(
    store: &Store,
    hooks: &HooksConfig,
    since: Option<&str>,
) -> Result<ShadowReport> {
    let since = since.map(|text| since_instant(store, text)).transpose()?;
    let mut events = store.events(EventFilter {
        task: None,
        limit: u32::MAX,
    })?;
    if let Some(since) = &since {
        events.retain(|event| event.at.as_str() >= since.as_str());
    }
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
        let rule = text("rule");
        let rule = rule
            .parse::<RuleId>()
            .map_or(rule, |id| id.as_str().to_owned());
        let enforced = rule.parse::<RuleId>().is_ok_and(|id| hooks.enforces(id));
        let hits = by_rule.entry(rule).or_insert_with_key(|rule| RuleHits {
            rule: rule.clone(),
            enforced,
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
        let task = event
            .task
            .clone()
            .unwrap_or_else(|| "orchestrator".to_owned());
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
        enforcing: hooks.enforce,
        enforce_rules: hooks
            .enforce_rules
            .iter()
            .map(|rule| rule.as_str().to_owned())
            .collect(),
        since,
        events: events.len(),
        days: days.into_iter().collect(),
        sessions: sessions.len(),
        would_deny,
        rules,
    })
}
