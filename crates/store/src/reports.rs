//! What the MCP tools write: child reports, pull requests of a task and the soft rules added at run time.

use headwork_core::Actor;
use rusqlite::params;
use serde::Serialize;

use crate::queries::insert_event;
use crate::records::TaskId;
use crate::{NOW, Store, StoreError};

/// The event a `report` call records.
pub const EVENT_REPORT: &str = "report";

/// A pull request a report names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPr {
    /// `owner/name`.
    pub repo: String,
    /// The pull request number.
    pub number: i64,
    /// The URL.
    pub url: String,
}

/// A pull request recorded for a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedPr {
    /// `owner/name`.
    pub repo: String,
    /// The pull request number.
    pub number: i64,
    /// The URL.
    pub url: String,
    /// `open`, `merged` or `closed`, as last seen.
    pub state: String,
}

/// A soft rule added with `rule_add`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AddedRule {
    /// The row id.
    pub id: i64,
    /// `global`, `root`, `child` or a project name.
    pub scope: String,
    /// The rule's text.
    pub body: String,
    /// The rules version it came with.
    pub version: i64,
}

impl Store {
    /// Records a report: an event, the task's last report, and the pull request it names.
    ///
    /// A pull request already recorded for the task keeps the state last seen: a report never takes a merged one back
    /// to `open`. A pull request another task recorded gets a row of this task's own.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn record_report(
        &mut self,
        task: TaskId,
        session: &str,
        status: &str,
        summary: &str,
        links: &[String],
        pr: Option<&NewPr>,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            &format!("UPDATE tasks SET last_report = ?1, updated_at = {NOW} WHERE id = ?2"),
            params![summary, task.0],
        )?;
        if let Some(pr) = pr {
            tx.execute(
                "INSERT INTO prs (task_id, repo, number, url, state) VALUES (?1, ?2, ?3, ?4, 'open')
                 ON CONFLICT (task_id, url) DO UPDATE SET repo = excluded.repo, number = excluded.number",
                params![task.0, pr.repo, pr.number, pr.url],
            )?;
        }
        let data = serde_json::json!({
            "status": status, "summary": summary, "links": links, "pr": pr.map(|pr| pr.url.clone()),
        });
        insert_event(
            &tx,
            Actor::Worker,
            Some(task),
            Some(session),
            EVENT_REPORT,
            &data,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Every pull request recorded for the task, oldest first.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn recorded_prs(&self, task: TaskId) -> Result<Vec<RecordedPr>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT repo, number, url, state FROM prs WHERE task_id = ?1 ORDER BY id")?;
        let rows = statement.query_map(params![task.0], |row| {
            Ok(RecordedPr {
                repo: row.get(0)?,
                number: row.get(1)?,
                url: row.get(2)?,
                state: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The rules added at run time that are still active, oldest first.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn active_rules(&self) -> Result<Vec<AddedRule>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT id, scope, body, version FROM rules WHERE active = 1 ORDER BY id")?;
        let rows = statement
            .query_map([], |row| {
                Ok(AddedRule {
                    id: row.get(0)?,
                    scope: row.get(1)?,
                    body: row.get(2)?,
                    version: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Adds a rule, tagged with the rules version that will hold once it is observed.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn add_rule(&mut self, scope: &str, body: &str, actor: Actor) -> Result<i64, StoreError> {
        let tx = self.conn.transaction()?;
        let version: i64 = tx.query_row(
            "SELECT rules_version FROM machine WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO rules (scope, body, version, source, active) VALUES (?1, ?2, ?3, ?4, 1)",
            params![scope, body, version + 1, actor.as_str()],
        )?;
        let id = tx.last_insert_rowid();
        let data = serde_json::json!({ "id": id, "scope": scope, "body": body });
        insert_event(&tx, actor, None, None, "rule-added", &data)?;
        tx.commit()?;
        Ok(id)
    }

    /// Retires an active rule.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::UnknownRule`] when no active rule has the id.
    pub fn retire_rule(&mut self, id: i64, actor: Actor) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        let changed = tx.execute(
            "UPDATE rules SET active = 0 WHERE id = ?1 AND active = 1",
            params![id],
        )?;
        if changed == 0 {
            return Err(StoreError::UnknownRule(id));
        }
        insert_event(
            &tx,
            actor,
            None,
            None,
            "rule-retired",
            &serde_json::json!({ "id": id }),
        )?;
        tx.commit()?;
        Ok(())
    }
}
