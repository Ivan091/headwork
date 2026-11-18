//! Folder trust in Claude Code's state file (`~/.claude.json`).
//!
//! Claude Code keeps trust per repository under `projects["<path>"].hasTrustDialogAccepted`. A worktree inherits its
//! main repository's trust, a parent folder's trust does not cover a separate repository inside it. This module reads
//! that one field and sets it for one exact key, and touches nothing else in the file.
//!
//! The file is large, holds credentials and is rewritten by every running Claude session, so a change is made as
//! late and as briefly as possible: read, change the one field, write a temporary file beside it and rename it over
//! the original, after copying the previous version to [`backup_path`]. Errors never carry the file's contents.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value};

/// The field Claude Code reads.
pub const TRUST_FIELD: &str = "hasTrustDialogAccepted";

/// The object holding one entry per project path.
const PROJECTS: &str = "projects";

/// How often a rename is tried when another process holds the file open.
const RENAME_TRIES: u32 = 5;

/// What [`ensure_trusted`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// The folder was trusted already; the file was not written.
    Already,
    /// The folder was not trusted; the field is set now.
    Granted,
}

/// Why the state file could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    /// Reading, writing or renaming failed.
    #[error("{action} {path}: {source}")]
    Io {
        /// What was being done.
        action: &'static str,
        /// The file.
        path: String,
        /// The cause.
        source: std::io::Error,
    },
    /// The file is not JSON; only the position is kept, never the text.
    #[error("{path} is not valid JSON (line {line}, column {column})")]
    Parse {
        /// The file.
        path: String,
        /// Where parsing stopped.
        line: usize,
        /// Where parsing stopped.
        column: usize,
    },
    /// The file, its `projects` or the project's entry is not a JSON object.
    #[error("{path}: {what} is not a JSON object")]
    Shape {
        /// The file.
        path: String,
        /// Which part.
        what: &'static str,
    },
}

/// The key Claude Code uses for a folder: forward slashes, an upper-case drive letter, no trailing slash.
#[must_use]
pub fn project_key(path: &str) -> String {
    let mut key = path.replace('\\', "/");
    while key.len() > 1 && key.ends_with('/') && !key.ends_with(":/") {
        key.pop();
    }
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(drive), Some(':')) if drive.is_ascii_alphabetic() => {
            format!("{}:{}", drive.to_ascii_uppercase(), chars.as_str())
        }
        _ => key,
    }
}

/// Where the previous version is kept before each write.
#[must_use]
pub fn backup_path(file: &Path) -> PathBuf {
    sibling(file, "headwork-backup")
}

fn sibling(file: &Path, suffix: &str) -> PathBuf {
    let mut name = file
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(".");
    name.push(suffix);
    file.with_file_name(name)
}

/// Whether the field is `true` in a parsed state file.
#[must_use]
pub fn trusted_in(state: &Value, key: &str) -> bool {
    state
        .get(PROJECTS)
        .and_then(|projects| projects.get(key))
        .and_then(|entry| entry.get(TRUST_FIELD))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Sets the field for `key` in a parsed state file, creating `projects` or the entry when missing. Returns whether
/// anything changed.
///
/// # Errors
///
/// Fails when the file, `projects` or the entry is not an object; nothing is changed then.
pub fn grant_in(state: &mut Value, key: &str) -> Result<bool, &'static str> {
    if trusted_in(state, key) {
        return Ok(false);
    }
    let root = state.as_object_mut().ok_or("the file")?;
    let projects = root
        .entry(PROJECTS)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or("`projects`")?;
    let entry = projects
        .entry(key)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or("the project's entry")?;
    entry.insert(TRUST_FIELD.to_owned(), Value::Bool(true));
    Ok(true)
}

/// Whether the folder `key` is trusted. A missing file trusts nothing.
///
/// # Errors
///
/// Fails when the file cannot be read or is not JSON.
pub fn is_trusted(file: &Path, key: &str) -> Result<bool, TrustError> {
    Ok(read(file)?.is_some_and(|(_, state)| trusted_in(&state, key)))
}

/// Trusts the folder `key` unless it is trusted already. A missing file is created with that one entry.
///
/// # Errors
///
/// Fails when the file cannot be read, parsed or replaced; the original stays as it was then.
pub fn ensure_trusted(file: &Path, key: &str) -> Result<Trust, TrustError> {
    let (original, mut state) =
        read(file)?.unwrap_or_else(|| (Vec::new(), Value::Object(Map::new())));
    let changed = grant_in(&mut state, key).map_err(|what| TrustError::Shape {
        path: file.display().to_string(),
        what,
    })?;
    if !changed {
        return Ok(Trust::Already);
    }
    let mut text = serde_json::to_vec_pretty(&state).map_err(|error| TrustError::Io {
        action: "encoding",
        path: file.display().to_string(),
        source: error.into(),
    })?;
    text.push(b'\n');
    if !original.is_empty() {
        write(&backup_path(file), &original, "backing up")?;
    }
    let temp = sibling(file, &format!("headwork-{}.tmp", std::process::id()));
    write(&temp, &text, "writing")?;
    rename(&temp, file).inspect_err(|_| {
        drop(std::fs::remove_file(&temp));
    })?;
    Ok(Trust::Granted)
}

fn read(file: &Path) -> Result<Option<(Vec<u8>, Value)>, TrustError> {
    let bytes = match std::fs::read(file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(TrustError::Io {
                action: "reading",
                path: file.display().to_string(),
                source,
            });
        }
    };
    let state = serde_json::from_slice(&bytes).map_err(|error| TrustError::Parse {
        path: file.display().to_string(),
        line: error.line(),
        column: error.column(),
    })?;
    Ok(Some((bytes, state)))
}

fn write(path: &Path, bytes: &[u8], action: &'static str) -> Result<(), TrustError> {
    std::fs::write(path, bytes).map_err(|source| TrustError::Io {
        action,
        path: path.display().to_string(),
        source,
    })
}

fn rename(from: &Path, to: &Path) -> Result<(), TrustError> {
    let mut tries = 0;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(_) if tries + 1 < RENAME_TRIES => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(source) => {
                return Err(TrustError::Io {
                    action: "replacing",
                    path: to.display().to_string(),
                    source,
                });
            }
        }
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    const KEY: &str = "S:/Code/site/site";

    fn state_file(dir: &tempfile::TempDir, text: &str) -> PathBuf {
        let file = dir.path().join(".claude.json");
        std::fs::write(&file, text).unwrap();
        file
    }

    fn parsed(file: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap()
    }

    #[test]
    fn keys_take_the_form_claude_code_writes() {
        for (path, key) in [
            ("S:/Code/a/a", "S:/Code/a/a"),
            (r"s:\Code\a\a", "S:/Code/a/a"),
            (r"C:\Users\User/.claude/", "C:/Users/User/.claude"),
            ("C:/", "C:/"),
            ("/home/u/code/a", "/home/u/code/a"),
        ] {
            assert_eq!(project_key(path), key, "{path}");
        }
    }

    #[test]
    fn an_untrusted_folder_is_granted_and_nothing_else_changes() {
        let dir = tempfile::tempdir().unwrap();
        let before = concat!(
            r#"{"oauthAccount":{"token":"secret"},"numStartups":7,"projects":{"#,
            r#""S:/Code":{"hasTrustDialogAccepted":true,"allowedTools":[]},"#,
            r#""S:/Code/site/site":{"hasTrustDialogAccepted":false,"lastCost":1.5}}}"#
        );
        let file = state_file(&dir, before);
        assert_eq!(
            ensure_trusted(&file, KEY).unwrap(),
            Trust::Granted,
            "granted"
        );
        let mut expected: Value = serde_json::from_str(before).unwrap();
        expected["projects"][KEY][TRUST_FIELD] = Value::Bool(true);
        assert_eq!(parsed(&file), expected, "only the one field changed");
        assert_eq!(
            std::fs::read_to_string(backup_path(&file)).unwrap(),
            before,
            "backup holds the previous version"
        );
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no temp file left");
    }

    #[test]
    fn a_missing_entry_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let file = state_file(&dir, r#"{"projects":{}}"#);
        assert_eq!(
            ensure_trusted(&file, KEY).unwrap(),
            Trust::Granted,
            "granted"
        );
        assert!(is_trusted(&file, KEY).unwrap(), "trusted now");
        assert!(!is_trusted(&file, "S:/Code").unwrap(), "no other key");
    }

    #[test]
    fn a_trusted_folder_leaves_the_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let text = r#"{"projects":{"S:/Code/site/site":{"hasTrustDialogAccepted":true}}}"#;
        let file = state_file(&dir, text);
        assert_eq!(
            ensure_trusted(&file, KEY).unwrap(),
            Trust::Already,
            "already"
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text, "untouched");
        assert!(!backup_path(&file).exists(), "no backup");
    }

    #[test]
    fn a_missing_file_is_created_with_the_one_entry() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".claude.json");
        assert!(!is_trusted(&file, KEY).unwrap(), "nothing trusted");
        assert_eq!(
            ensure_trusted(&file, KEY).unwrap(),
            Trust::Granted,
            "granted"
        );
        assert_eq!(
            parsed(&file),
            serde_json::json!({ "projects": { KEY: { TRUST_FIELD: true } } }),
            "one entry"
        );
    }

    #[test]
    fn a_broken_file_is_refused_without_its_text() {
        let dir = tempfile::tempdir().unwrap();
        let text = r#"{"oauthAccount":"secret-token", oops"#;
        let file = state_file(&dir, text);
        let error = ensure_trusted(&file, KEY).unwrap_err().to_string();
        assert!(!error.contains("secret"), "no contents in {error}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text, "untouched");
        let file = state_file(&dir, r#"{"projects":[]}"#);
        assert!(
            matches!(ensure_trusted(&file, KEY), Err(TrustError::Shape { .. })),
            "projects must be an object"
        );
    }
}
