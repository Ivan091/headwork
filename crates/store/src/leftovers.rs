//! What the Remote Control leftovers list reads and records: how each ended session left, and the user's note that a
//! leftover was archived in the Claude app. The note is an event, so no schema change is needed.

use headwork_core::Actor;
use headwork_core::leftover::{EndedFacts, ExitTrail, GRACEFUL_END_REASON};
use rusqlite::params;

use crate::queries::insert_event;
use crate::records::TaskId;
use crate::{Store, StoreError};

/// The event that records the user archiving a session's Remote Control entry in the Claude app.
pub const EVENT_RC_ARCHIVED: &str = "rc-archived";

impl Store {
    /// The facts about every session that ended or was found gone, read in one query, oldest first.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn ended_session_facts(&self) -> Result<Vec<EndedFacts>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT s.id, s.rc_name, t.project || '-' || t.slug, s.role,
                    COALESCE(s.ended_at, s.last_event_at, s.started_at), s.exit_code,
                    EXISTS (SELECT 1 FROM events e WHERE e.session_id = s.id AND e.type = 'session-end'
                            AND json_extract(e.data, '$.reason') = ?1),
                    EXISTS (SELECT 1 FROM events e WHERE e.session_id = s.id AND e.type = 'exit-asked'),
                    EXISTS (SELECT 1 FROM events e WHERE e.session_id = s.id AND e.type = 'exit-ignored'),
                    EXISTS (SELECT 1 FROM events e WHERE e.session_id = s.id AND e.type = ?2)
             FROM sessions s LEFT JOIN tasks t ON t.id = s.task_id
             WHERE s.state IN ('ended', 'gone')
             ORDER BY s.started_at, s.rowid",
        )?;
        let rows = statement.query_map(params![GRACEFUL_END_REASON, EVENT_RC_ARCHIVED], |row| {
            Ok(EndedFacts {
                session: row.get(0)?,
                rc_name: row.get(1)?,
                task: row.get(2)?,
                role: row.get(3)?,
                ended_at: row.get(4)?,
                exit_code: row.get(5)?,
                trail: ExitTrail {
                    exited_on_command: row.get(6)?,
                    asked: row.get(7)?,
                    ignored: row.get(8)?,
                },
                archived: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Records that the user archived these sessions in the Claude app; returns how many it marked.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn mark_rc_archived(&mut self, sessions: &[String]) -> Result<usize, StoreError> {
        let tx = self.conn.transaction()?;
        for session in sessions {
            let task: Option<i64> = tx.query_row(
                "SELECT task_id FROM sessions WHERE id = ?1",
                params![session],
                |row| row.get(0),
            )?;
            insert_event(
                &tx,
                Actor::User,
                task.map(TaskId),
                Some(session),
                EVENT_RC_ARCHIVED,
                &serde_json::json!({}),
            )?;
        }
        tx.commit()?;
        Ok(sessions.len())
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]
mod tests {
    use headwork_core::{Actor, Role};
    use rusqlite::params;

    use super::EVENT_RC_ARCHIVED;
    use crate::{NewSession, Store};

    fn session(store: &mut Store, id: &str) {
        store
            .record_session(
                &NewSession {
                    id: id.to_owned(),
                    task_id: None,
                    role: Role::Orchestrator,
                    rc_name: format!("rc-{id}"),
                    host: "wmux".to_owned(),
                    host_workspace: None,
                    host_surface: None,
                },
                Actor::Hook,
            )
            .unwrap();
    }

    #[test]
    fn facts_carry_the_events_and_the_archive_mark() {
        let mut store = Store::open_in_memory().unwrap();
        for id in ["quit", "killed", "live"] {
            session(&mut store, id);
        }
        store.session_ended("quit", Some(1)).unwrap();
        store
            .append_event(
                Actor::Hook,
                None,
                Some("quit"),
                "session-end",
                &serde_json::json!({ "reason": "prompt_input_exit" }),
            )
            .unwrap();
        store.session_ended("killed", Some(1)).unwrap();
        for kind in ["exit-asked", "exit-ignored"] {
            store
                .append_event(
                    Actor::Worker,
                    None,
                    Some("killed"),
                    kind,
                    &serde_json::json!({}),
                )
                .unwrap();
        }
        let facts = store.ended_session_facts().unwrap();
        assert_eq!(facts.len(), 2, "the live session is not listed");
        let quit = facts.iter().find(|fact| fact.session == "quit").unwrap();
        assert!(quit.trail.exited_on_command, "exit on command");
        assert!(!quit.trail.ignored && !quit.archived, "nothing else");
        let killed = facts.iter().find(|fact| fact.session == "killed").unwrap();
        assert!(killed.trail.asked && killed.trail.ignored, "exit events");
        assert_eq!(killed.exit_code, Some(1), "exit code");
        assert_eq!(killed.task, None, "no task");

        let marked = store.mark_rc_archived(&["killed".to_owned()]).unwrap();
        assert_eq!(marked, 1, "marked");
        let facts = store.ended_session_facts().unwrap();
        let killed = facts.iter().find(|fact| fact.session == "killed").unwrap();
        assert!(killed.archived, "archived");
        let events: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE type = ?1 AND actor = 'user'",
                params![EVENT_RC_ARCHIVED],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(events, 1, "one user event");
    }
}
