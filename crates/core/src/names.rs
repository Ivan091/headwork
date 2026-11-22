//! Validated names: project names, task slugs and the task names built from them, and the orchestrator's address.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Why a string is not a valid name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    /// The string is empty.
    #[error("a name must not be empty")]
    Empty,
    /// The string does not start with a lowercase letter or a digit.
    #[error("`{0}` must start with a lowercase letter or a digit")]
    BadStart(String),
    /// The string holds a character other than a lowercase letter, a digit or `-`.
    #[error("`{0}` may hold only lowercase letters, digits and `-`")]
    BadCharacter(String),
    /// The string ends with `-`.
    #[error("`{0}` must not end with `-`")]
    TrailingHyphen(String),
}

fn check_kebab(text: &str) -> Result<(), NameError> {
    let mut chars = text.chars();
    let first = chars.next().ok_or(NameError::Empty)?;
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(NameError::BadStart(text.to_owned()));
    }
    if !text
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(NameError::BadCharacter(text.to_owned()));
    }
    if text.ends_with('-') {
        return Err(NameError::TrailingHyphen(text.to_owned()));
    }
    Ok(())
}

macro_rules! kebab_name {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Validates `text` as a kebab-case name.
            ///
            /// # Errors
            ///
            /// Returns a [`NameError`] when `text` is empty, does not start with a lowercase letter or digit,
            /// holds other characters than lowercase letters, digits and `-`, or ends with `-`.
            pub fn new(text: impl Into<String>) -> Result<Self, NameError> {
                let text = text.into();
                check_kebab(&text)?;
                Ok(Self(text))
            }

            /// The name as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = NameError;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                Self::new(text)
            }
        }

        impl TryFrom<String> for $name {
            type Error = NameError;

            fn try_from(text: String) -> Result<Self, Self::Error> {
                Self::new(text)
            }
        }

        impl From<$name> for String {
            fn from(name: $name) -> Self {
                name.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

kebab_name!(
    /// A project's name, the key of its `[project.<name>]` table, such as `me-sim`.
    ProjectName
);

impl ProjectName {
    /// `headwork`, the project `[update]` names when it names none.
    #[must_use]
    pub fn headwork() -> Self {
        Self("headwork".to_owned())
    }
}

kebab_name!(
    /// A task's slug within its project, such as `sand-cascade`.
    Slug
);

/// A task's full name, `<project>-<slug>`: the session and Remote Control name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskName {
    project: ProjectName,
    slug: Slug,
}

impl TaskName {
    /// Joins a project name and a slug.
    #[must_use]
    pub const fn new(project: ProjectName, slug: Slug) -> Self {
        Self { project, slug }
    }

    /// The project part.
    #[must_use]
    pub const fn project(&self) -> &ProjectName {
        &self.project
    }

    /// The slug part.
    #[must_use]
    pub const fn slug(&self) -> &Slug {
        &self.slug
    }

    /// Splits a full name against the known projects; the longest matching project name wins, so `me-sim-x` and
    /// `me-studio-x` resolve even though both projects start with `me-`. `project/slug` is accepted too.
    #[must_use]
    pub fn resolve<'a>(
        text: &str,
        projects: impl IntoIterator<Item = &'a ProjectName>,
    ) -> Option<Self> {
        if let Some((project, slug)) = text.split_once('/') {
            return Some(Self::new(
                ProjectName::new(project).ok()?,
                Slug::new(slug).ok()?,
            ));
        }
        projects
            .into_iter()
            .filter_map(|project| {
                let rest = text.strip_prefix(project.as_str())?.strip_prefix('-')?;
                Slug::new(rest)
                    .ok()
                    .map(|slug| Self::new(project.clone(), slug))
            })
            .max_by_key(|name| name.project.as_str().len())
    }
}

impl fmt::Display for TaskName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.project, self.slug)
    }
}

/// The address the orchestrator session answers to: its session and Remote Control name.
pub const ORCHESTRATOR_ADDRESS: &str = "orchestrator";

/// The orchestrator's address until 2026-11-14, when it was called the orchestrator; still an alias of the orchestrator.
pub const LEGACY_ORCHESTRATOR_ADDRESS: &str = "root";

/// Whether a session message to `name` means the orchestrator.
#[must_use]
pub fn is_orchestrator_address(name: &str) -> bool {
    name == ORCHESTRATOR_ADDRESS || name == LEGACY_ORCHESTRATOR_ADDRESS
}

/// The session name a `claude` command line sets with `-n` or `--name`, before any `--`.
#[must_use]
pub fn session_name_in(command: &str) -> Option<&str> {
    let mut words = command.split_whitespace();
    let program = words.next()?.trim_matches('"');
    let base = program.rsplit(['/', '\\']).next().unwrap_or(program);
    if !(base == "claude" || base.eq_ignore_ascii_case("claude.exe")) {
        return None;
    }
    while let Some(word) = words.next() {
        match word {
            "--" => return None,
            "-n" | "--name" => return words.next().map(|name| name.trim_matches('"')),
            other => {
                if let Some(name) = other.strip_prefix("--name=") {
                    return Some(name.trim_matches('"'));
                }
            }
        }
    }
    None
}

/// Where messages to the orchestrator go.
///
/// The managed orchestrator's name when the store has one running, else the name of an unmanaged one found among the
/// running command lines (`-n orchestrator` before `-n root`), else [`ORCHESTRATOR_ADDRESS`].
#[must_use]
pub fn orchestrator_address<'a>(
    managed: Option<&str>,
    running: impl IntoIterator<Item = &'a str>,
) -> String {
    if let Some(name) = managed {
        return name.to_owned();
    }
    let names: Vec<&str> = running
        .into_iter()
        .filter_map(session_name_in)
        .filter(|name| is_orchestrator_address(name))
        .collect();
    if names.contains(&LEGACY_ORCHESTRATOR_ADDRESS) && !names.contains(&ORCHESTRATOR_ADDRESS) {
        LEGACY_ORCHESTRATOR_ADDRESS.to_owned()
    } else {
        ORCHESTRATOR_ADDRESS.to_owned()
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests fail loudly on a bad fixture")]
mod tests {
    use super::*;

    #[test]
    fn accepts_kebab_case() {
        for good in ["me-sim", "a", "sand-cascade-2", "0day"] {
            assert!(Slug::new(good).is_ok(), "{good} should be valid");
        }
    }

    #[test]
    fn rejects_other_forms() {
        assert_eq!(Slug::new(""), Err(NameError::Empty));
        assert!(
            matches!(Slug::new("-a"), Err(NameError::BadStart(_))),
            "leading hyphen"
        );
        assert!(
            matches!(Slug::new("Sand"), Err(NameError::BadStart(_))),
            "uppercase start"
        );
        assert!(
            matches!(Slug::new("sand_cascade"), Err(NameError::BadCharacter(_))),
            "underscore"
        );
        assert!(
            matches!(Slug::new("sand-"), Err(NameError::TrailingHyphen(_))),
            "trailing hyphen"
        );
    }

    #[test]
    fn resolves_the_longest_project_prefix() {
        let projects = [
            ProjectName::new("me").unwrap(),
            ProjectName::new("me-sim").unwrap(),
        ];
        let name = TaskName::resolve("me-sim-sand", &projects).unwrap();
        assert_eq!(name.project().as_str(), "me-sim", "longest project wins");
        assert_eq!(name.slug().as_str(), "sand", "slug is the rest");
        assert_eq!(
            name.to_string(),
            "me-sim-sand",
            "display joins with a hyphen"
        );
    }

    #[test]
    fn resolves_slash_form_and_rejects_unknown() {
        let projects = [ProjectName::new("me-sim").unwrap()];
        let name = TaskName::resolve("other/x", &projects).unwrap();
        assert_eq!(
            name.to_string(),
            "other-x",
            "slash form needs no known project"
        );
        assert!(
            TaskName::resolve("other-x", &projects).is_none(),
            "unknown project"
        );
    }

    #[test]
    fn finds_the_orchestrator_address() {
        let root = "claude --agent root --remote-control root -n root";
        let worker = "claude --remote-control me-sim-x -n me-sim-x -- \"tell -n root\"";
        assert_eq!(
            orchestrator_address(Some("boss"), [root]),
            "boss",
            "the store wins"
        );
        assert_eq!(
            orchestrator_address(None, [worker, root]),
            "root",
            "an unmanaged root"
        );
        assert_eq!(
            orchestrator_address(None, [root, "claude -n orchestrator"]),
            "orchestrator",
            "the new name before the old one"
        );
        assert_eq!(
            orchestrator_address(None, [worker]),
            "orchestrator",
            "the default"
        );
        assert_eq!(session_name_in("wmux -n root"), None, "only claude");
        assert_eq!(
            session_name_in("C:/bin/claude.exe --name=\"root\""),
            Some("root"),
            "long form"
        );
        assert!(
            is_orchestrator_address("root") && is_orchestrator_address("orchestrator"),
            "both addresses"
        );
        assert!(!is_orchestrator_address("me-sim-x"), "a worker");
    }
}
