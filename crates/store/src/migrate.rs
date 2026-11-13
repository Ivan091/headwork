//! Numbered schema migrations, tracked in SQLite's `user_version`.

use rusqlite::Connection;

use crate::error::StoreError;

/// Every migration in order; the schema version is the number applied.
pub const MIGRATIONS: &[&str] = &[include_str!("../migrations/0001_initial.sql")];

/// Applies the migrations the database lacks, each in its own transaction.
///
/// # Errors
///
/// Returns [`StoreError::SchemaTooNew`] when the database is newer than [`MIGRATIONS`], or the SQLite error of a
/// failing migration, which is rolled back.
pub fn migrate(conn: &mut Connection) -> Result<i64, StoreError> {
    let found: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let applied = usize::try_from(found).map_err(|_| StoreError::OutOfRange(found))?;
    if applied > MIGRATIONS.len() {
        return Err(StoreError::SchemaTooNew {
            found,
            known: MIGRATIONS.len(),
        });
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
        let version = i64::try_from(index + 1).map_err(|_| StoreError::OutOfRange(found))?;
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok(conn.pragma_query_value(None, "user_version", |row| row.get(0))?)
}
