//! Which running sessions still run an older `headwork` build, and what refreshes them (docs/hot-reload.md).
//!
//! A launcher records a [`EVENT_BUILD`] event when it starts: the program its hooks and MCP server call, and that
//! file's [`Stamp`]. When the file there changes, everything started per call (hooks, the CLI, finishers) already runs
//! the new build. A session whose launcher recorded the event also has an MCP relay that switches to the new build
//! at its next tool call, and a launcher that hands its exit work to the new build. A session without the event was
//! started before hot reload existed: its MCP server keeps the old code until it is reconnected.

use std::path::Path;

use serde::Serialize;
use serde_json::{Value, json};

use super::install::Stamp;
use super::slashed;

/// The event a launcher records at start.
pub const EVENT_BUILD: &str = "build";

/// The data of a [`EVENT_BUILD`] event for the program at `exe`.
#[must_use]
pub fn build_event(exe: &Path) -> Value {
    json!({
        "exe": slashed(exe),
        "stamp": Stamp::of(exe).ok().map(|stamp| stamp.to_string()),
    })
}

/// How a session's build compares with the program its hooks call now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Freshness {
    /// It runs the installed build.
    Current,
    /// Its launcher runs an older build; its MCP relay switches at the next tool call and its exit is handled by the
    /// installed build. Nothing to do.
    Reloading,
    /// Started before hot reload: its MCP server runs the older build until it is reconnected.
    Pinned,
}

/// One live session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionBuild {
    /// Its Remote Control name.
    pub name: String,
    /// How its build compares.
    pub freshness: Freshness,
}

/// Compares each live session's recorded build with the stamp `now` reads for its program today.
#[must_use]
pub fn classify(
    sessions: &[(String, Option<Value>)],
    now: impl Fn(&Path) -> Option<Stamp>,
) -> Vec<SessionBuild> {
    sessions
        .iter()
        .map(|(name, build)| {
            let freshness = build.as_ref().map_or(Freshness::Pinned, |build| {
                let exe = build.get("exe").and_then(Value::as_str).map(Path::new);
                let recorded = build.get("stamp").and_then(Value::as_str);
                let current = exe.and_then(&now).map(|stamp| stamp.to_string());
                if recorded.is_some() && recorded == current.as_deref() {
                    Freshness::Current
                } else {
                    Freshness::Reloading
                }
            });
            SessionBuild {
                name: name.clone(),
                freshness,
            }
        })
        .collect()
}

/// The names of the sessions with this freshness.
#[must_use]
pub fn named(sessions: &[SessionBuild], freshness: Freshness) -> Vec<&str> {
    sessions
        .iter()
        .filter(|session| session.freshness == freshness)
        .map(|session| session.name.as_str())
        .collect()
}

/// What to do about pinned sessions, in one sentence.
pub const PINNED_FIX: &str = "in each pinned session run /mcp, choose headwork and Reconnect (its tools then run the new build), \
    or restart it with park and resume; hooks already use the new build";

/// One line about every live session's build.
#[must_use]
pub fn summary(sessions: &[SessionBuild]) -> String {
    let list = |freshness| {
        let names = named(sessions, freshness);
        if names.is_empty() {
            "none".to_owned()
        } else {
            names.join(", ")
        }
    };
    format!(
        "{} live sessions; on the installed build: {}; switching at their next tool call: {}; pinned to an older MCP \
         server: {}",
        sessions.len(),
        list(Freshness::Current),
        list(Freshness::Reloading),
        list(Freshness::Pinned),
    )
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "tests fail loudly on an unexpected error"
    )]

    use super::*;

    #[test]
    fn sessions_are_classified_by_their_recorded_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("headwork.exe");
        std::fs::write(&exe, "v1").unwrap();
        let old = build_event(&exe);
        std::fs::write(&exe, "v2 is longer").unwrap();
        let new = build_event(&exe);
        let sessions = vec![
            ("a".to_owned(), Some(new)),
            ("b".to_owned(), Some(old)),
            ("c".to_owned(), None),
            (
                "d".to_owned(),
                Some(json!({ "exe": "/gone", "stamp": null })),
            ),
        ];
        let found = classify(&sessions, |path| Stamp::of(path).ok());
        let freshness: Vec<Freshness> = found.iter().map(|session| session.freshness).collect();
        assert_eq!(
            freshness,
            vec![
                Freshness::Current,
                Freshness::Reloading,
                Freshness::Pinned,
                Freshness::Reloading
            ],
            "current, older, before hot reload, unknown stamp"
        );
        let text = summary(&found);
        assert!(
            text.starts_with("4 live sessions; on the installed build: a;"),
            "{text}"
        );
        assert!(text.ends_with("pinned to an older MCP server: c"), "{text}");
    }
}
