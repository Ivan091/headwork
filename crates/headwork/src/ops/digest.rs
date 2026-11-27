//! The "Needs you" digest (`docs/backlog.md`): gathers the facts from the store, builds the digest with
//! `headwork_core::digest`, and decides when the tick pushes it.
//!
//! Nothing here fetches from the network or starts a process, so the hook, `board`, `status` and the recaps can show it
//! at no cost. Pending migration notes are the ones the tick last stored as findings.

use anyhow::Result;
use headwork_core::backlog::ItemStatus;
use headwork_core::digest::{Digest, Facts, build};
use headwork_core::{Actor, Config};
use headwork_store::{BacklogFilter, EVENT_DIGEST_PUSHED, Store};

use super::rules::CONTEXT_LIMIT;

/// The digest as the store and the config give it now.
///
/// # Errors
///
/// Returns the store's failure.
pub fn digest(store: &Store, config: &Config) -> Result<Digest> {
    let backlog_items = store.backlog_items(&BacklogFilter {
        statuses: ItemStatus::OPEN.to_vec(),
        ..BacklogFilter::default()
    })?;
    Ok(build(&Facts {
        today: store.backlog_today()?,
        asks: store.digest_asks()?,
        backlog_items,
        notes: store.digest_notes()?,
        prs: store.digest_prs()?,
        findings: store.digest_findings()?,
        finding_hours: config.digest.finding_hours,
    }))
}

/// The digest as text to put on top of another output, with a blank line after it.
///
/// Empty when nothing waits on the user or when the store cannot be read, since a digest must never keep a hook or a
/// recap from working.
#[must_use]
pub fn section(store: &Store, config: &Config) -> String {
    match digest(store, config) {
        Ok(digest) if !digest.is_empty() => format!("{}\n", digest.render()),
        Ok(_) | Err(_) => String::new(),
    }
}

/// The event that records a session's start left part of the digest out for want of room.
pub const EVENT_DEFERRED: &str = "digest-deferred";

/// The event that records the left-out digest was handed to the session with its first prompt.
pub const EVENT_DELIVERED: &str = "digest-delivered";

/// `rules` with the digest after them, cut to leave the whole under [`CONTEXT_LIMIT`], the most a hook may inject.
///
/// The orchestrator's rules fill most of that, so a digest that does not fit whole is logged as deferred for the
/// session and [`with_first_prompt`] hands it over complete. `rules` alone when nothing waits or the store cannot be
/// read.
#[must_use]
pub fn after_rules(
    store: &Store,
    config: &Config,
    session: &str,
    actor: Actor,
    rules: String,
) -> String {
    let Ok(digest) = digest(store, config) else {
        return rules;
    };
    let room = CONTEXT_LIMIT.saturating_sub(rules.len() + 2);
    let shown = digest.render_within(room);
    if shown != digest.render() {
        drop(store.append_event(
            actor,
            None,
            Some(session),
            EVENT_DEFERRED,
            &serde_json::json!({ "shown": shown.len() }),
        ));
    }
    if shown.is_empty() {
        rules
    } else {
        format!("{rules}\n\n{shown}")
    }
}

/// The whole digest for the first prompt of a session whose start could not hold it, once; empty otherwise.
#[must_use]
pub fn with_first_prompt(store: &Store, config: &Config, session: &str, actor: Actor) -> String {
    let waiting = matches!(store.session_event(session, EVENT_DEFERRED), Ok(Some(_)))
        && matches!(store.session_event(session, EVENT_DELIVERED), Ok(None));
    if !waiting {
        return String::new();
    }
    drop(store.append_event(
        actor,
        None,
        Some(session),
        EVENT_DELIVERED,
        &serde_json::json!({}),
    ));
    format!("\n{}", section(store, config))
}

/// Pushes the digest through `notify` when it is due, and returns what was sent.
///
/// Due means `[digest] push` is on, the local hour is `[digest] push_hour` or later, the digest holds lines and none was
/// pushed today. Logs `digest-pushed`.
///
/// # Errors
///
/// Returns the store's failure.
pub fn push_due(
    store: &Store,
    config: &Config,
    actor: Actor,
    notify: impl FnOnce(&str),
) -> Result<Option<String>> {
    if !config.digest.push || !store.digest_push_due(config.digest.push_hour)? {
        return Ok(None);
    }
    let digest = digest(store, config)?;
    if digest.is_empty() {
        return Ok(None);
    }
    let text = digest.render();
    notify(&text);
    store.append_event(
        actor,
        None,
        None,
        EVENT_DIGEST_PUSHED,
        &serde_json::json!({ "lines": digest.lines.len(), "more": digest.more }),
    )?;
    Ok(Some(text))
}
