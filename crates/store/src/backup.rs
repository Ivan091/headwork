//! A copy of the store taken before a migration changes it (`docs/autoupdate.md`, "The store backup belongs in the
//! store").
//!
//! A migration cannot be undone, and a build older than the schema is refused by the store, so the copy is the only
//! way back after a bad migration. It is taken by whichever process opens an older store first: a hand install, a hook
//! of a running session right after a swap, or an update. Two processes may race; one that finds the store already
//! migrated by the time its copy is made throws its copy away.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::migrate::MIGRATIONS;

/// The folder beside the store that holds the copies.
pub const BACKUP_DIR: &str = "backups";

/// How many copies are kept; older ones are deleted after a new one is made.
pub const BACKUPS_KEPT: usize = 3;

const PREFIX: &str = "state-";
const SUFFIX: &str = ".db";

/// What opening the store did about a copy before migrating it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backup {
    /// The copy was made, holding the store as it was before the migrations.
    Made {
        /// The copy.
        path: PathBuf,
        /// The schema version copied.
        from: i64,
        /// The schema version the store was migrated to.
        to: i64,
    },
    /// No copy could be made; the migrations went ahead.
    Failed {
        /// The schema version found.
        from: i64,
        /// The schema version the store was migrated to.
        to: i64,
        /// Why the copy failed.
        reason: String,
    },
}

impl std::fmt::Display for Backup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Made { path, from, to } => write!(
                f,
                "copied the store (schema {from}) to {} before migrating it to {to}",
                path.display()
            ),
            Self::Failed { from, to, reason } => write!(
                f,
                "could not copy the store before migrating it from schema {from} to {to}: {reason}"
            ),
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum CopyError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Copies the store at `path`, open on `conn`, when it is older than this build and not new, before anything migrates
/// it; `None` when there is nothing to copy or another process migrated it first.
pub(crate) fn before_migrating(conn: &Connection, path: &Path) -> Option<Backup> {
    let from: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .ok()?;
    let to = i64::try_from(MIGRATIONS.len()).ok()?;
    if from <= 0 || from >= to {
        return None;
    }
    let dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(BACKUP_DIR);
    match copy(conn, &dir, from, to) {
        Ok(Some(copied)) => {
            prune(&dir);
            Some(Backup::Made {
                path: copied,
                from,
                to,
            })
        }
        Ok(None) => None,
        Err(error) => Some(Backup::Failed {
            from,
            to,
            reason: error.to_string(),
        }),
    }
}

fn copy(conn: &Connection, dir: &Path, from: i64, to: i64) -> Result<Option<PathBuf>, CopyError> {
    std::fs::create_dir_all(dir)?;
    let target = dir.join(format!(
        "{PREFIX}{from}-to-{to}-{}-{}{SUFFIX}",
        millis(),
        std::process::id()
    ));
    conn.execute("VACUUM INTO ?1", [target.to_string_lossy()])?;
    let copied: i64 =
        Connection::open(&target)?.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if copied == from {
        Ok(Some(target))
    } else {
        std::fs::remove_file(&target)?;
        Ok(None)
    }
}

/// Deletes all but the newest [`BACKUPS_KEPT`] copies in `dir`; a copy that cannot be deleted stays.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut copies: Vec<(u128, PathBuf)> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter_map(|path| made_at(&path).map(|at| (at, path)))
        .collect();
    copies.sort_by(|a, b| b.cmp(a));
    for (_, path) in copies.into_iter().skip(BACKUPS_KEPT) {
        let _ignored: std::io::Result<()> = std::fs::remove_file(path);
    }
}

/// When a copy was made, from its name `state-<from>-to-<to>-<millis>-<pid>.db`; `None` for any other file.
fn made_at(path: &Path) -> Option<u128> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    let mut fields = stem.split('-');
    let (_from, to_word, _to, at, _pid) = (
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
    );
    if to_word != "to" || fields.next().is_some() {
        return None;
    }
    at.parse().ok()
}

fn millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis())
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "tests fail loudly on an unexpected error"
    )]

    use super::*;

    #[test]
    fn only_copy_names_are_read() {
        for (name, at) in [
            ("state-13-to-14-1700-42.db", Some(1700)),
            ("state-13-to-14-1700.db", None),
            ("state-13-14-1700-42.db", None),
            ("state-13-to-14-1700-42-9.db", None),
            ("state.db", None),
            ("state-13-to-14-x-42.db", None),
            ("other-13-to-14-1700-42.db", None),
        ] {
            assert_eq!(made_at(Path::new(name)), at, "{name}");
        }
    }

    #[test]
    fn pruning_keeps_the_newest_copies_and_other_files() {
        let dir = tempfile::tempdir().unwrap();
        for at in [100, 400, 200, 300, 500] {
            std::fs::write(dir.path().join(format!("state-1-to-2-{at}-7.db")), "").unwrap();
        }
        std::fs::write(dir.path().join("notes.txt"), "").unwrap();
        prune(dir.path());
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                "notes.txt".to_owned(),
                "state-1-to-2-300-7.db".to_owned(),
                "state-1-to-2-400-7.db".to_owned(),
                "state-1-to-2-500-7.db".to_owned(),
            ],
            "the three newest copies and the unrelated file stay"
        );
    }
}
