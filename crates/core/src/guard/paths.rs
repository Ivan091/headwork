//! Path comparison for the edit guards, by text only.
//!
//! Slashes go forward, letters lower case, `.` and `..` are resolved, and Git Bash's `/c/` becomes `c:/`. Windows and
//! macOS file systems are case-insensitive by default, so lower case is safe there.

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
