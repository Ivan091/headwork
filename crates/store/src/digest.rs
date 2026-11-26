//! What the "Needs you" digest (`docs/backlog.md`) reads from the store, as the plain facts `headwork_core::digest` builds
//! from. Every query is a read; none of them calls the network or a process.

use std::collections::BTreeMap;

use headwork_core::FindingId;
use headwork_core::digest::{AskFact, FindingFact, NoteFact, PrFact};
use rusqlite::params;

use crate::{Store, StoreError};

/// The event that records the daily push of the digest.
pub const EVENT_DIGEST_PUSHED: &str = "digest-pushed";

const AGE: &str = "CAST(strftime('%s', 'now') AS INTEGER) - CAST(strftime('%s', ";

fn age_of(column: &str) -> String {
    format!("{AGE}{column}) AS INTEGER)")
}

fn seconds(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

impl Store {
    /// The asks that wait on the user: pending or open, of a task that is not closed or of none.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn digest_asks(&self) -> Result<Vec<AskFact>, StoreError> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT a.id, t.project || '-' || t.slug, a.question, {}
             FROM asks a LEFT JOIN tasks t ON t.id = a.task_id
             WHERE a.state IN ('pending', 'open') AND (t.id IS NULL OR t.state <> 'closed')
             ORDER BY a.id",
            age_of("a.created_at")
        ))?;
        let rows = statement.query_map([], |row| {
            Ok(AskFact {
                id: row.get(0)?,
                task: row.get(1)?,
                question: row.get(2)?,
                waiting_s: seconds(row.get(3)?),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The tick findings that stand, except pending migration notes, which [`Store::digest_notes`] lists.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn digest_findings(&self) -> Result<Vec<FindingFact>, StoreError> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT f.rule, t.project || '-' || t.slug, {}
             FROM findings f LEFT JOIN tasks t ON t.id = f.task_id
             WHERE f.resolved_at IS NULL AND f.rule <> ?1 ORDER BY f.id",
            age_of("f.first_at")
        ))?;
        let rows = statement.query_map(params![FindingId::MigrationPending.as_str()], |row| {
            Ok(FindingFact {
                kind: row.get(0)?,
                task: row.get(1)?,
                open_s: seconds(row.get(2)?),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The migration notes that wait for this machine, as the tick last found them: its standing `migration-pending`
    /// findings. Nothing is fetched, so a machine whose tick never ran lists none.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn digest_notes(&self) -> Result<Vec<NoteFact>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT detail FROM findings WHERE rule = ?1 AND resolved_at IS NULL ORDER BY id",
        )?;
        let rows = statement.query_map(params![FindingId::MigrationPending.as_str()], |row| {
            row.get::<_, String>(0)
        })?;
        let mut notes = Vec::new();
        for detail in rows {
            let value: serde_json::Value = serde_json::from_str(&detail?)?;
            let field = |key: &str| {
                value
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            };
            if let Some(address) = field("note") {
                notes.push(NoteFact {
                    address,
                    title: field("title").unwrap_or_default(),
                });
            }
        }
        Ok(notes)
    }

    /// The open pull requests that wait for the user: those of a task in `in_review` whose newest session has ended
    /// (or never existed) or sits marked as waiting on the user, grouped by task in task order.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn digest_prs(&self) -> Result<Vec<PrFact>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT t.id, t.project || '-' || t.slug, p.url FROM prs p JOIN tasks t ON t.id = p.task_id
             WHERE p.state = 'open' AND t.state = 'in_review'
               AND COALESCE((SELECT s.state IN ('ended', 'gone') OR s.waiting_since IS NOT NULL FROM sessions s
                             WHERE s.task_id = t.id ORDER BY s.started_at DESC, s.rowid DESC LIMIT 1), 1)
             ORDER BY t.id, p.id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut grouped: BTreeMap<i64, PrFact> = BTreeMap::new();
        for row in rows {
            let (id, task, url) = row?;
            grouped
                .entry(id)
                .or_insert_with(|| PrFact {
                    task,
                    urls: Vec::new(),
                })
                .urls
                .push(url);
        }
        Ok(grouped.into_values().collect())
    }

    /// Whether the daily push is due: the local hour is `hour` or later and no `digest-pushed` event carries today's
    /// local date.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn digest_push_due(&self, hour: u8) -> Result<bool, StoreError> {
        Ok(self.conn.query_row(
            "SELECT CAST(strftime('%H', 'now', 'localtime') AS INTEGER) >= ?1
                AND NOT EXISTS (SELECT 1 FROM events WHERE type = ?2
                                AND date(at, 'localtime') = date('now', 'localtime'))",
            params![hour, EVENT_DIGEST_PUSHED],
            |row| row.get(0),
        )?)
    }
}
