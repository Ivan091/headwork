//! The daily copy of the store (`docs/store-backup.md`).
//!
//! A tick stage takes a consistent `VACUUM INTO` copy of `state.db` once a day into `<home>/backups/daily-<millis>.db`,
//! keeps the newest seven, and `headwork doctor` warns when the newest is older than two
//! days. The copies a migration takes (`state-*.db`) share the folder and are never
//! touched here.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use headwork_core::Actor;
use headwork_store::{BACKUP_DIR, Store};

/// How many daily copies are kept.
pub const DAILY_KEPT: usize = 7;

/// A new copy is made once the newest is this old, in seconds.
pub const DUE_AFTER_S: u64 = 24 * 3600;

/// `headwork doctor` warns once the newest copy is this old, in seconds.
pub const STALE_AFTER_S: u64 = 2 * 24 * 3600;

/// Event: a daily copy was made.
pub const EVENT_STORE_BACKUP: &str = "store-backup";

/// Event: a daily copy could not be made.
pub const EVENT_STORE_BACKUP_FAILED: &str = "store-backup-failed";

const PREFIX: &str = "daily-";
const SUFFIX: &str = ".db";
const PARTIAL: &str = ".part";

/// The folder of the copies under the headwork home.
#[must_use]
pub fn dir(root: &Path) -> PathBuf {
    root.join(BACKUP_DIR)
}

/// The milliseconds since the Unix epoch now.
#[must_use]
pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis())
}

/// When a daily copy was made, from its name `daily-<millis>.db`; `None` for any other file.
fn made_at(path: &Path) -> Option<u128> {
    path.file_name()?
        .to_str()?
        .strip_prefix(PREFIX)?
        .strip_suffix(SUFFIX)?
        .parse()
        .ok()
}

fn copies(dir: &Path) -> Vec<(u128, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(u128, PathBuf)> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter_map(|path| made_at(&path).map(|at| (at, path)))
        .collect();
    found.sort_by(|a, b| b.cmp(a));
    found
}

/// The age in seconds of the newest daily copy in `dir` at `now_ms`; `None` when there is none.
#[must_use]
pub fn newest_age_s(dir: &Path, now_ms: u128) -> Option<u64> {
    let (at, _) = copies(dir).into_iter().next()?;
    u64::try_from(now_ms.saturating_sub(at) / 1000).ok()
}

/// Makes the day's copy when the newest is older than [`DUE_AFTER_S`].
///
/// Keeps the newest [`DAILY_KEPT`] and returns the new copy, or `None` when one is not due. A made or failed copy is
/// logged, a skipped one is not.
///
/// # Errors
///
/// Fails when the copy cannot be made, after logging `store-backup-failed`.
pub fn daily(store: &Store, root: &Path, actor: Actor, now_ms: u128) -> Result<Option<PathBuf>> {
    let dir = dir(root);
    if newest_age_s(&dir, now_ms).is_some_and(|age| age < DUE_AFTER_S) {
        return Ok(None);
    }
    match make(store, &dir, now_ms) {
        Ok(path) => {
            prune(&dir);
            store.append_event(
                actor,
                None,
                None,
                EVENT_STORE_BACKUP,
                &serde_json::json!({ "path": path.to_string_lossy(), "kept": DAILY_KEPT }),
            )?;
            Ok(Some(path))
        }
        Err(error) => {
            store.append_event(
                actor,
                None,
                None,
                EVENT_STORE_BACKUP_FAILED,
                &serde_json::json!({ "error": format!("{error:#}") }),
            )?;
            Err(error)
        }
    }
}

fn make(store: &Store, dir: &Path, now_ms: u128) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let target = dir.join(format!("{PREFIX}{now_ms}{SUFFIX}"));
    let partial = dir.join(format!("{PREFIX}{now_ms}{SUFFIX}{PARTIAL}"));
    let _ignored: std::io::Result<()> = std::fs::remove_file(&partial);
    let schema = store.schema_version()?;
    let copied = store
        .copy_to(&partial)
        .context("copying the store")
        .and_then(|()| verify(&partial, schema));
    if let Err(error) = copied {
        let _ignored: std::io::Result<()> = std::fs::remove_file(&partial);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&partial, &target) {
        let _ignored: std::io::Result<()> = std::fs::remove_file(&partial);
        return Err(error).with_context(|| format!("renaming to {}", target.display()));
    }
    Ok(target)
}

fn verify(copy: &Path, schema: i64) -> Result<()> {
    let found = Store::open_read_only(copy)
        .context("opening the copy")?
        .schema_version()?;
    anyhow::ensure!(
        found == schema,
        "the copy has schema {found}, the store {schema}"
    );
    Ok(())
}

fn prune(dir: &Path) {
    for (_, path) in copies(dir).into_iter().skip(DAILY_KEPT) {
        let _ignored: std::io::Result<()> = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "tests fail loudly on an unexpected error"
    )]

    use super::*;

    const DAY_MS: u128 = 24 * 3600 * 1000;

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn only_daily_names_are_read() {
        for (name, at) in [
            ("daily-1700.db", Some(1700)),
            ("daily-1700.db.part", None),
            ("daily-x.db", None),
            ("state-1-to-2-1700-7.db", None),
        ] {
            assert_eq!(made_at(Path::new(name)), at, "{name}");
        }
    }

    #[test]
    fn a_copy_is_made_once_a_day_and_is_a_whole_store() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let start = 10 * DAY_MS;
        let first = daily(&store, home.path(), Actor::Tick, start)
            .unwrap()
            .unwrap();
        assert!(first.exists(), "the first copy");
        let copy = Store::open_read_only(&first).unwrap();
        assert_eq!(
            copy.schema_version().unwrap(),
            store.schema_version().unwrap(),
            "same schema"
        );
        assert!(
            daily(&store, home.path(), Actor::Tick, start + DAY_MS / 2)
                .unwrap()
                .is_none(),
            "not due"
        );
        assert!(
            daily(&store, home.path(), Actor::Tick, start + DAY_MS)
                .unwrap()
                .is_some(),
            "due"
        );
    }

    #[test]
    fn the_newest_seven_stay_and_migration_copies_are_left_alone() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let backups = dir(home.path());
        std::fs::create_dir_all(&backups).unwrap();
        std::fs::write(backups.join("state-1-to-2-5-7.db"), "").unwrap();
        for day in 1..=9 {
            daily(&store, home.path(), Actor::Tick, day * DAY_MS).unwrap();
        }
        let left = names(&backups);
        assert_eq!(
            left.len(),
            DAILY_KEPT + 1,
            "seven daily and the migration copy: {left:?}"
        );
        assert!(
            left.contains(&"state-1-to-2-5-7.db".to_owned()),
            "migration copy stays"
        );
        assert!(!left.contains(&format!("daily-{DAY_MS}.db")), "oldest gone");
        assert_eq!(
            newest_age_s(&backups, 9 * DAY_MS + 5000),
            Some(5),
            "age of the newest"
        );
    }

    #[test]
    fn a_copy_that_fails_after_it_started_leaves_no_partial_file_and_the_old_copy() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let backups = dir(home.path());
        std::fs::create_dir_all(&backups).unwrap();
        let old = backups.join(format!("daily-{DAY_MS}.db"));
        std::fs::write(&old, "old").unwrap();
        let now = 3 * DAY_MS;
        std::fs::create_dir(backups.join(format!("daily-{now}.db"))).unwrap();
        assert!(make(&store, &backups, now).is_err(), "the rename fails");
        let left = names(&backups);
        assert!(
            !left.iter().any(|name| name.ends_with(PARTIAL)),
            "no partial file: {left:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&old).unwrap(),
            "old",
            "the old copy is intact"
        );
    }

    #[test]
    fn a_copy_with_another_schema_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let copy = home.path().join("copy.db");
        store.copy_to(&copy).unwrap();
        let schema = store.schema_version().unwrap();
        assert!(verify(&copy, schema).is_ok(), "the same schema");
        assert!(verify(&copy, schema + 1).is_err(), "another schema");
    }

    #[test]
    fn no_copy_has_no_age() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(newest_age_s(&dir(home.path()), 1), None, "empty");
    }
}
