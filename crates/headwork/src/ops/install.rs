//! `install`: puts a new `headwork` build in place while the old one may still be running (docs/hot-reload.md).
//!
//! Windows refuses to overwrite or delete a running program but lets it be renamed, and a running process keeps the
//! path it was started from. So the installed copy is moved aside to `headwork.old-<millis>`, the new build is copied
//! next to it under a temporary name and renamed into place, and the new copy is checked by running it. Old copies
//! that no process runs any more are deleted; the others stay until a later install deletes them. Every program that
//! starts after the swap, such as a hook call, gets the new build; long-lived processes are listed by
//! [`super::reload`].

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Serialize;

/// The identity of one build file: its length and its modification time, which a copy keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    len: u64,
    modified_nanos: u128,
}

impl Stamp {
    /// The stamp of the file at `path`.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be read.
    pub fn of(path: &Path) -> io::Result<Self> {
        let meta = std::fs::metadata(path)?;
        let modified_nanos = meta
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        Ok(Self {
            len: meta.len(),
            modified_nanos,
        })
    }
}

impl fmt::Display for Stamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.len, self.modified_nanos)
    }
}

impl Serialize for Stamp {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// The installed program's file name in its folder.
#[must_use]
pub fn program_name() -> String {
    format!("headwork{}", std::env::consts::EXE_SUFFIX)
}

const OLD_PREFIX: &str = "headwork.old";

/// What an install did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallReport {
    /// The installed program.
    pub target: PathBuf,
    /// Its stamp.
    pub stamp: Stamp,
    /// Where the copy it replaced went, when there was one and it could not be deleted at once.
    pub moved_aside: Option<PathBuf>,
    /// Old copies deleted.
    pub removed: Vec<PathBuf>,
    /// Old copies still in use, deleted by a later install.
    pub kept: Vec<PathBuf>,
}

/// Installs the build at `source` as `headwork` in `dir`, then runs `verify` on the installed copy; a copy that fails
/// it is taken out again and the old one put back.
///
/// # Errors
///
/// Fails when `source` is the installed copy itself, when a file operation fails, or when `verify` fails.
pub fn install(
    source: &Path,
    dir: &Path,
    verify: impl FnOnce(&Path) -> Result<()>,
) -> Result<InstallReport> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let target = dir.join(program_name());
    if same_file(source, &target) {
        bail!(
            "{} is the installed copy; run `install` from a build, such as target/release/{}",
            source.display(),
            program_name()
        );
    }
    let staged = dir.join(format!("headwork.new{}", std::env::consts::EXE_SUFFIX));
    if staged.exists() {
        std::fs::remove_file(&staged)
            .with_context(|| format!("removing the leftover {}", staged.display()))?;
    }
    std::fs::copy(source, &staged)
        .with_context(|| format!("copying {} to {}", source.display(), staged.display()))?;
    let aside = if target.exists() {
        let aside = dir.join(format!(
            "{OLD_PREFIX}-{}{}",
            millis(),
            std::env::consts::EXE_SUFFIX
        ));
        std::fs::rename(&target, &aside).with_context(|| {
            format!(
                "moving the installed {} aside to {}",
                target.display(),
                aside.display()
            )
        })?;
        Some(aside)
    } else {
        None
    };
    if let Err(error) = std::fs::rename(&staged, &target) {
        restore(aside.as_deref(), &target);
        return Err(error)
            .with_context(|| format!("renaming {} to {}", staged.display(), target.display()));
    }
    if let Err(error) = verify(&target) {
        if std::fs::rename(&target, &staged).is_ok() {
            restore(aside.as_deref(), &target);
        }
        return Err(error.context(format!(
            "the new {} failed its check; the old one is back",
            target.display()
        )));
    }
    let stamp = Stamp::of(&target).with_context(|| format!("reading {}", target.display()))?;
    let (removed, kept) = clean(dir)?;
    let moved_aside = aside.filter(|aside| kept.contains(aside));
    Ok(InstallReport {
        target,
        stamp,
        moved_aside,
        removed,
        kept,
    })
}

fn restore(aside: Option<&Path>, target: &Path) {
    if let Some(aside) = aside
        && std::fs::rename(aside, target).is_err()
    {
        let _ignored: io::Result<u64> = std::fs::copy(aside, target);
    }
}

/// Deletes every old copy in `dir` that can be deleted, the hand-made `headwork.old.exe` too; returns what was
/// deleted and what is still in use.
///
/// # Errors
///
/// Fails when `dir` cannot be listed.
pub fn clean(dir: &Path) -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut old: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("listing {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| is_old_copy(path))
        .collect();
    old.sort();
    let (removed, kept) = old
        .into_iter()
        .partition(|path| std::fs::remove_file(path).is_ok());
    Ok((removed, kept))
}

fn is_old_copy(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.starts_with(OLD_PREFIX) && name.ends_with(std::env::consts::EXE_SUFFIX)
        })
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
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

    fn build(dir: &Path, content: &str) -> PathBuf {
        let path = dir.join(format!("build{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&path, content).unwrap();
        path
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_first_install_copies_the_build() {
        let src = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let report = install(&build(src.path(), "v1"), bin.path(), |_| Ok(())).unwrap();
        assert_eq!(
            std::fs::read_to_string(&report.target).unwrap(),
            "v1",
            "the build is installed"
        );
        assert_eq!(report.moved_aside, None, "nothing was replaced");
        assert_eq!(names(bin.path()), vec![program_name()], "no leftovers");
        assert_eq!(
            report.stamp,
            Stamp::of(&report.target).unwrap(),
            "the stamp is the installed file's"
        );
    }

    #[test]
    fn a_second_install_replaces_the_first_and_cleans_old_copies() {
        let src = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        install(&build(src.path(), "v1"), bin.path(), |_| Ok(())).unwrap();
        std::fs::write(
            bin.path()
                .join(format!("headwork.old{}", std::env::consts::EXE_SUFFIX)),
            "v0",
        )
        .unwrap();
        let report = install(&build(src.path(), "v2"), bin.path(), |_| Ok(())).unwrap();
        assert_eq!(
            std::fs::read_to_string(&report.target).unwrap(),
            "v2",
            "the new build is installed"
        );
        assert_eq!(
            report.removed.len(),
            2,
            "the replaced copy and the hand-made old copy are deleted"
        );
        assert!(report.kept.is_empty(), "nothing is in use");
        assert_eq!(names(bin.path()), vec![program_name()], "no leftovers");
    }

    #[test]
    fn a_failed_check_puts_the_old_copy_back() {
        let src = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        install(&build(src.path(), "v1"), bin.path(), |_| Ok(())).unwrap();
        let error = install(&build(src.path(), "v2"), bin.path(), |_| {
            bail!("broken build")
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("broken build"), "{error:#}");
        let target = bin.path().join(program_name());
        assert_eq!(
            std::fs::read_to_string(target).unwrap(),
            "v1",
            "the old build is back"
        );
    }

    #[test]
    fn installing_the_installed_copy_is_refused() {
        let bin = tempfile::tempdir().unwrap();
        let target = bin.path().join(program_name());
        std::fs::write(&target, "v1").unwrap();
        let error = install(&target, bin.path(), |_| Ok(())).unwrap_err();
        assert!(
            error.to_string().contains("is the installed copy"),
            "{error}"
        );
        assert_eq!(
            std::fs::read_to_string(target).unwrap(),
            "v1",
            "nothing changed"
        );
    }

    #[test]
    fn only_old_copies_are_cleaned() {
        for (name, old) in [
            (
                format!("headwork.old{}", std::env::consts::EXE_SUFFIX),
                true,
            ),
            (
                format!("headwork.old-17{}", std::env::consts::EXE_SUFFIX),
                true,
            ),
            (program_name(), false),
            ("headwork.new".to_owned(), false),
        ] {
            assert_eq!(is_old_copy(Path::new(&name)), old, "{name}");
        }
    }
}
