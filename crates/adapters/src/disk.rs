//! Disks: free space of a volume and the size of files under a folder, for the tick's disk and runaway checks.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

/// The most directory entries one scan visits, so a huge tree cannot stall a tick.
pub const SCAN_LIMIT: usize = 1_000_000;

/// Free space of volumes.
pub trait Disks: fmt::Debug {
    /// Bytes free to the user on the volume holding `path`.
    ///
    /// # Errors
    ///
    /// Returns the OS error.
    fn free_bytes(&self, path: &Path) -> io::Result<u64>;
}

/// The machine's disks.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemDisks;

impl Disks for SystemDisks {
    fn free_bytes(&self, path: &Path) -> io::Result<u64> {
        fs4::available_space(path)
    }
}

/// Disks in memory: a volume key maps to its free bytes.
#[derive(Debug, Default)]
pub struct FakeDisks {
    free: RefCell<BTreeMap<String, u64>>,
}

impl FakeDisks {
    /// Disks with no volume until one is set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the free bytes of the volume holding `path`.
    pub fn set(&self, path: &Path, free: u64) {
        self.free.borrow_mut().insert(volume_of(path), free);
    }
}

impl Disks for FakeDisks {
    fn free_bytes(&self, path: &Path) -> io::Result<u64> {
        self.free
            .borrow()
            .get(&volume_of(path))
            .copied()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such volume"))
    }
}

/// The volume a path lives on: its drive prefix on Windows, `/` elsewhere.
#[must_use]
pub fn volume_of(path: &Path) -> String {
    match path.components().next() {
        Some(Component::Prefix(prefix)) => prefix.as_os_str().to_string_lossy().to_uppercase(),
        Some(_) | None => "/".to_owned(),
    }
}

/// Every file under `root` of at least `floor` bytes, with its size. Links are not followed, unreadable entries are
/// skipped, and at most [`SCAN_LIMIT`] entries are visited.
#[must_use]
pub fn large_files(root: &Path, floor: u64) -> Vec<(PathBuf, u64)> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    let mut visited = 0_usize;
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > SCAN_LIMIT {
                return found;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file()
                && let Ok(meta) = entry.metadata()
                && meta.len() >= floor
            {
                found.push((entry.path(), meta.len()));
            }
        }
    }
    found
}
