//! The tick's lock: `tick.lock` keeps two runs from acting at once, and `tick.started` says who holds it and since
//! when, so a later run can tell a tick that is working from one that hangs.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use headwork_core::watch::TickStamp;

const LOCK_FILE: &str = "tick.lock";
const STAMP_FILE: &str = "tick.started";

/// The seconds since the Unix epoch now.
#[must_use]
pub fn now_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// The lock, held until this value is dropped.
#[derive(Debug)]
pub struct TickLock {
    file: File,
    stamp_path: PathBuf,
}

impl Drop for TickLock {
    fn drop(&mut self) {
        drop(std::fs::remove_file(&self.stamp_path));
        drop(self.file.unlock());
    }
}

/// What taking the lock came to.
#[derive(Debug)]
pub enum Acquired {
    /// This run holds the lock.
    Held(TickLock),
    /// Another run holds it, with the stamp it left when it took the lock (none when it left no readable one).
    Busy(Option<TickStamp>),
}

fn open(root: &Path) -> Result<(File, PathBuf)> {
    let path = root.join(LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    Ok((file, path))
}

fn read_stamp(root: &Path) -> Option<TickStamp> {
    TickStamp::parse(&std::fs::read_to_string(root.join(STAMP_FILE)).ok()?)
}

/// Takes the lock under `root` and stamps it with this process and `now_s`.
///
/// # Errors
///
/// Fails when the lock file cannot be opened, locked or stamped.
pub fn acquire(root: &Path, now_s: u64) -> Result<Acquired> {
    let (file, path) = open(root)?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(Acquired::Busy(read_stamp(root))),
        Err(TryLockError::Error(error)) => {
            return Err(error).with_context(|| format!("locking {}", path.display()));
        }
    }
    let stamp_path = root.join(STAMP_FILE);
    let stamp = TickStamp {
        pid: std::process::id(),
        started_s: now_s,
    };
    std::fs::write(&stamp_path, stamp.render())
        .with_context(|| format!("writing {}", stamp_path.display()))?;
    Ok(Acquired::Held(TickLock { file, stamp_path }))
}

/// The stamp of a tick that has held the lock for longer than the hung threshold at `now_s`, when one has. The lock
/// is only tried, never taken from a running process.
///
/// # Errors
///
/// Fails when the lock file cannot be opened or tried.
pub fn hung(root: &Path, now_s: u64) -> Result<Option<TickStamp>> {
    let (file, path) = open(root)?;
    match file.try_lock() {
        Ok(()) => {
            drop(file.unlock());
            Ok(None)
        }
        Err(TryLockError::WouldBlock) => Ok(read_stamp(root).filter(|stamp| stamp.hung(now_s))),
        Err(TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("locking {}", path.display()))
        }
    }
}
