//! The store's error type.

use headwork_core::{AdmissionRefusal, NameError, Refusal, ShapeError, UnknownWord};

/// Everything that can go wrong in the store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite failed.
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// The database was written by a newer headwork.
    #[error(
        "the database schema is version {found}, newer than the {known} this headwork knows; update headwork"
    )]
    SchemaTooNew {
        /// The database's version.
        found: i64,
        /// The newest version this build knows.
        known: usize,
    },
    /// A read-only open found a database older than this build, which only a writable open migrates.
    #[error(
        "the database schema is version {found}, older than the {known} this headwork knows; run `headwork board` once to migrate it"
    )]
    SchemaBehind {
        /// The database's version.
        found: i64,
        /// The newest version this build knows.
        known: usize,
    },
    /// The lifecycle refused a transition.
    #[error("task `{task}`: {refusal}")]
    Refused {
        /// The task's name.
        task: String,
        /// Why.
        #[source]
        refusal: Refusal,
    },
    /// A new task was refused.
    #[error(transparent)]
    NotAdmitted(#[from] AdmissionRefusal),
    /// No task with this id.
    #[error("no task with id {0}")]
    UnknownTask(i64),
    /// A stored word is not one this build knows.
    #[error("stored value: {0}")]
    UnknownWord(#[from] UnknownWord),
    /// A stored word in a named row is not one this build knows.
    #[error("{row}: {source}")]
    BadRow {
        /// The row, such as `task 12` or `event 345`.
        row: String,
        /// The word.
        #[source]
        source: UnknownWord,
    },
    /// A stored name is not valid.
    #[error("stored name: {0}")]
    BadName(#[from] NameError),
    /// A stored number does not fit.
    #[error("stored number {0} is out of range")]
    OutOfRange(i64),
    /// A task meant to be queued is not.
    #[error("task `{0}` is not queued")]
    NotQueued(String),
    /// A live task already has the name.
    #[error("a live task `{0}` already exists")]
    NameTaken(String),
    /// No active rule has this id.
    #[error("no active rule with id {0}")]
    UnknownRule(i64),
    /// A stored task's `parent_id` and `lead` do not make a shape.
    #[error("stored task shape: {0}")]
    BadShape(#[from] ShapeError),
    /// A part may not be created under this task.
    #[error("no part under `{lead}`: {reason}")]
    NotALead {
        /// The task named as the lead.
        lead: String,
        /// Why not.
        reason: &'static str,
    },
    /// A stored review node id is not valid.
    #[error("stored review node: {0}")]
    BadNode(#[from] headwork_core::review::FlowError),
    /// Event data is not valid JSON.
    #[error("event data: {0}")]
    Json(#[from] serde_json::Error),
}
