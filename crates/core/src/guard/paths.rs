//! Path comparison for the edit guards, by text only.
//!
//! Slashes go forward, letters lower case, `.` and `..` are resolved, and Git Bash's `/c/` becomes `c:/`. Windows and
//! macOS file systems are case-insensitive by default, so lower case is safe there.

/// Shell spellings that name the user's home folder.
const HOME_SPELLINGS: &[&str] = &["~", "$HOME", "${HOME}"];

/// Shell spellings that name the temp folder; Git Bash mounts `/tmp` on the user's temp folder.
const TEMP_SPELLINGS: &[&str] = &[
    "/tmp",
    "$TMPDIR",
    "${TMPDIR}",
    "$TEMP",
    "${TEMP}",
    "$TMP",
    "${TMP}",
];

/// The folders that `~`, `$HOME` and `/tmp` stand for, so a path spelled with them compares like the real one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Places {
    /// The user's home folder; `None` leaves `~` and `$HOME` unexpanded.
    pub home: Option<String>,
    /// The temp folder; `None` leaves `/tmp` unexpanded.
    pub temp: Option<String>,
}

impl Places {
    /// `path` with a leading home or temp spelling replaced by its folder.
    #[must_use]
    pub fn expand(&self, path: &str) -> String {
        let replaced = [(HOME_SPELLINGS, &self.home), (TEMP_SPELLINGS, &self.temp)]
            .into_iter()
            .filter_map(|(spellings, folder)| folder.as_deref().map(|folder| (spellings, folder)))
            .find_map(|(spellings, folder)| {
                spellings.iter().find_map(|spelling| {
                    let rest = path.strip_prefix(spelling)?;
                    (rest.is_empty() || rest.starts_with(['/', '\\']))
                        .then(|| format!("{}{rest}", folder.trim_end_matches(['/', '\\'])))
                })
            });
        replaced.unwrap_or_else(|| path.to_owned())
    }

    /// Normalizes `path` after expanding it, joined to `cwd` first when it is relative.
    #[must_use]
    pub fn resolve(&self, cwd: &str, path: &str) -> GuardPath {
        GuardPath::resolve(cwd, &self.expand(path))
    }
}

/// A path in the guard's comparable form.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GuardPath(String);

impl GuardPath {
    /// Normalizes `path`, joined to `cwd` first when it is relative.
    #[must_use]
    pub fn resolve(cwd: &str, path: &str) -> Self {
        let path = path.replace('\\', "/");
        let joined = if is_absolute(&path) {
            path
        } else {
            format!("{}/{path}", cwd.replace('\\', "/"))
        };
        Self::new(&joined)
    }

    /// Normalizes an absolute path.
    #[must_use]
    pub fn new(path: &str) -> Self {
        let path = msys_to_drive(&path.replace('\\', "/")).to_lowercase();
        let (prefix, rest) = split_root(&path);
        let mut parts: Vec<&str> = Vec::new();
        for part in rest.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                part => parts.push(part),
            }
        }
        Self(format!("{prefix}{}", parts.join("/")))
    }

    /// The normalized text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this path is `root` or lies below it.
    #[must_use]
    pub fn is_within(&self, root: &Self) -> bool {
        let root = root.0.trim_end_matches('/');
        self.0 == root
            || self
                .0
                .strip_prefix(root)
                .is_some_and(|rest| rest.starts_with('/'))
    }
}

impl std::fmt::Display for GuardPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn is_absolute(path: &str) -> bool {
    path.starts_with('/') || path.starts_with('~') || has_drive(path)
}

fn has_drive(path: &str) -> bool {
    let mut chars = path.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && chars.next() == Some(':')
}

fn msys_to_drive(path: &str) -> String {
    let mut chars = path.chars();
    match (chars.next(), chars.next(), chars.next()) {
        (Some('/'), Some(drive), Some('/')) if drive.is_ascii_alphabetic() => {
            format!("{drive}:/{}", chars.as_str())
        }
        (Some('/'), Some(drive), None) if drive.is_ascii_alphabetic() => format!("{drive}:/"),
        _ => path.to_owned(),
    }
}

fn split_root(path: &str) -> (String, &str) {
    if has_drive(path) {
        let (drive, rest) = path.split_at(2);
        (format!("{drive}/"), rest)
    } else if let Some(rest) = path.strip_prefix('/') {
        ("/".to_owned(), rest)
    } else {
        (String::new(), path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spellings_of_one_path_agree() {
        let forms = [
            "S:\\Code\\me-sim\\me-sim\\src\\a.rs",
            "s:/code/me-sim/me-sim/src/a.rs",
            "/s/Code/me-sim/me-sim/src/./b/../a.rs",
        ];
        let normal: Vec<GuardPath> = forms.iter().map(|form| GuardPath::new(form)).collect();
        assert!(
            normal.windows(2).all(|pair| pair.first() == pair.get(1)),
            "{normal:?}"
        );
    }

    #[test]
    fn relative_paths_join_the_cwd() {
        let path = GuardPath::resolve("S:/Code/me-sim/me-sim-x", "src/../Cargo.toml");
        assert_eq!(
            path.as_str(),
            "s:/code/me-sim/me-sim-x/cargo.toml",
            "joined"
        );
    }

    fn places() -> Places {
        Places {
            home: Some("C:\\Users\\User".to_owned()),
            temp: Some("C:/Users/User/AppData/Local/Temp/".to_owned()),
        }
    }

    #[test]
    fn home_and_temp_spellings_expand() {
        let places = places();
        let cases = [
            ("~", "c:/users/user"),
            ("~/.claude-x/a.md", "c:/users/user/.claude-x/a.md"),
            ("$HOME/.claude", "c:/users/user/.claude"),
            ("${HOME}\\.claude", "c:/users/user/.claude"),
            ("/tmp", "c:/users/user/appdata/local/temp"),
            (
                "/tmp/before.txt",
                "c:/users/user/appdata/local/temp/before.txt",
            ),
            ("$TMPDIR/x", "c:/users/user/appdata/local/temp/x"),
        ];
        for (path, expected) in cases {
            assert_eq!(places.resolve("S:/Code", path).as_str(), expected, "{path}");
        }
    }

    #[test]
    fn lookalikes_stay_as_they_are() {
        let places = places();
        assert_eq!(
            places.resolve("S:/Code", "~user/a").as_str(),
            "~user/a",
            "another user's home"
        );
        assert_eq!(
            places.resolve("S:/Code", "$HOMEDIR/a").as_str(),
            "s:/code/$homedir/a",
            "another variable"
        );
        assert_eq!(
            places.resolve("S:/Code", "/tmpx").as_str(),
            "/tmpx",
            "another root folder"
        );
        assert_eq!(
            Places::default().resolve("S:/Code", "~/a").as_str(),
            "~/a",
            "no home known"
        );
    }

    #[test]
    fn within_needs_a_whole_segment() {
        let root = GuardPath::new("S:/Code/me-sim/me-sim");
        assert!(
            GuardPath::new("S:/Code/me-sim/me-sim/a").is_within(&root),
            "below"
        );
        assert!(
            GuardPath::new("S:/Code/me-sim/me-sim").is_within(&root),
            "itself"
        );
        assert!(
            !GuardPath::new("S:/Code/me-sim/me-sim-x/a").is_within(&root),
            "a sibling worktree"
        );
    }
}
