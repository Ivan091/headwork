//! The layered merge of headwork configs: the shared file, the machine's file and the local file become one table.
//!
//! The merge is pure: it takes the text of each layer and returns the merged table with the layer that wrote each key.
//! `Config::parse` then checks the merged text exactly as it checks a hand-written config.

use std::collections::BTreeMap;
use std::fmt;

use thiserror::Error;
use toml::{Table, Value};

/// Key patterns only a machine's own file may set; `*` stands for one segment, and a pattern covers everything beneath it.
pub const MACHINE_ONLY: &[&str] = &[
    "machine",
    "project.*.main",
    "project.*.read_only_paths",
    "project.*.extra_writable",
    "hooks.orchestrator_writable",
    "hooks.orchestrator_repos",
    "hooks.brief_writable",
    "hooks.plugin_dir",
    "watch.roots",
];

/// Key patterns only the local file may set. An empty string value is refused too: the key must not appear at all.
pub const SECRET_KEYS: &[&str] = &["machine.notify_ntfy"];

const UNSET: &str = "unset";
const EXTEND: &str = "extend";

/// The layer a key came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LayerName {
    /// The file every machine shares.
    Shared,
    /// The file of one machine.
    Machine,
    /// The untracked file of this checkout.
    Local,
}

impl fmt::Display for LayerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Shared => "shared",
            Self::Machine => "machine",
            Self::Local => "local",
        })
    }
}

/// A dotted path to a key, one segment per table level.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyPath(Vec<String>);

/// A dotted key that does not parse.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum KeyPathError {
    /// A segment between dots (or the whole key) is empty.
    #[error("key `{0}` has an empty segment")]
    EmptySegment(String),
    /// A quoted segment is not closed.
    #[error("key `{0}` has an unterminated quoted segment")]
    Unterminated(String),
    /// A bare segment holds a character other than ASCII letters, digits, `_` and `-`.
    #[error("key `{0}` has a character that needs quotes")]
    NotBare(String),
}

fn is_bare(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl KeyPath {
    /// A path from its segments.
    #[must_use]
    pub const fn new(segments: Vec<String>) -> Self {
        Self(segments)
    }

    /// The segments, outermost first.
    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.0
    }

    /// Parses `usage.prices."claude-opus-5-5"`: bare segments joined by dots, a segment with other characters in quotes
    /// (`\"` and `\\` escape inside them).
    ///
    /// # Errors
    ///
    /// Returns [`KeyPathError`] for an empty segment, an unclosed quote or a bare segment with a character that needs quotes.
    pub fn parse(text: &str) -> Result<Self, KeyPathError> {
        let mut chars = text.chars().peekable();
        let mut segments = Vec::new();
        loop {
            let mut segment = String::new();
            if chars.peek() == Some(&'"') {
                chars.next();
                let mut closed = false;
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => match chars.next() {
                            Some(escaped) => segment.push(escaped),
                            None => return Err(KeyPathError::Unterminated(text.to_owned())),
                        },
                        '"' => {
                            closed = true;
                            break;
                        }
                        other => segment.push(other),
                    }
                }
                if !closed {
                    return Err(KeyPathError::Unterminated(text.to_owned()));
                }
                if segment.is_empty() {
                    return Err(KeyPathError::EmptySegment(text.to_owned()));
                }
            } else {
                while let Some(&c) = chars.peek() {
                    if c == '.' {
                        break;
                    }
                    segment.push(c);
                    chars.next();
                }
                if segment.is_empty() {
                    return Err(KeyPathError::EmptySegment(text.to_owned()));
                }
                if !is_bare(&segment) {
                    return Err(KeyPathError::NotBare(text.to_owned()));
                }
            }
            segments.push(segment);
            match chars.next() {
                None => return Ok(Self(segments)),
                Some('.') => {}
                Some(_) => return Err(KeyPathError::NotBare(text.to_owned())),
            }
        }
    }

    fn child(&self, segment: &str) -> Self {
        let mut segments = self.0.clone();
        segments.push(segment.to_owned());
        Self(segments)
    }

    fn starts_with(&self, prefix: &Self) -> bool {
        self.0.starts_with(&prefix.0)
    }

    fn matches(&self, pattern: &str) -> bool {
        let wanted: Vec<&str> = pattern.split('.').collect();
        self.0.len() >= wanted.len()
            && wanted
                .iter()
                .zip(&self.0)
                .all(|(want, have)| *want == "*" || *want == have)
    }
}

impl fmt::Display for KeyPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, segment) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(".")?;
            }
            if is_bare(segment) {
                f.write_str(segment)?;
            } else {
                f.write_str("\"")?;
                for c in segment.chars() {
                    if c == '"' || c == '\\' {
                        f.write_str("\\")?;
                    }
                    write!(f, "{c}")?;
                }
                f.write_str("\"")?;
            }
        }
        Ok(())
    }
}

/// Whether a key sits under one of the [`MACHINE_ONLY`] patterns.
#[must_use]
pub fn is_machine_only(path: &KeyPath) -> bool {
    MACHINE_ONLY.iter().any(|pattern| path.matches(pattern))
}

/// Whether a key sits under one of the [`SECRET_KEYS`] patterns.
#[must_use]
pub fn is_secret(path: &KeyPath) -> bool {
    SECRET_KEYS.iter().any(|pattern| path.matches(pattern))
}

/// One layer's text and where it came from.
#[derive(Debug, Clone, Copy)]
pub struct LayerInput<'a> {
    /// Which layer it is.
    pub name: LayerName,
    /// The file it was read from, for error messages.
    pub origin: &'a str,
    /// The TOML text.
    pub text: &'a str,
}

/// Why layers cannot be merged.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum LayerError {
    /// A layer is not valid TOML.
    #[error("{origin}: {message}")]
    Parse {
        /// The file.
        origin: String,
        /// The parser's message.
        message: String,
    },
    /// The shared file uses `unset` or `extend`.
    #[error("{origin}: `{key}` is only allowed in a machine or local file")]
    ReservedInShared {
        /// The file.
        origin: String,
        /// The reserved key.
        key: String,
    },
    /// `unset` or `extend` is not shaped as documented.
    #[error("{origin}: `{key}` is malformed: {reason}")]
    BadReserved {
        /// The file.
        origin: String,
        /// The offending entry.
        key: String,
        /// What is wrong.
        reason: String,
    },
    /// An `unset` entry names a key no lower layer sets.
    #[error("{origin}: `unset` names `{key}`, which no lower layer sets")]
    UnsetMissing {
        /// The file.
        origin: String,
        /// The key.
        key: String,
    },
    /// An `extend` entry has no array to append to.
    #[error("{origin}: `extend.{key}` needs an existing array at `{key}` to append to")]
    ExtendTarget {
        /// The file.
        origin: String,
        /// The key.
        key: String,
    },
    /// The shared file sets a key that belongs to one machine.
    #[error(
        "{origin}: `{key}` is machine-only and must be set in the machine's file, not the shared one"
    )]
    MachineOnlyInShared {
        /// The file.
        origin: String,
        /// The key.
        key: String,
    },
    /// A tracked file sets a secret.
    #[error(
        "{origin}: `{key}` is a secret and may only be set in the local file, not in a tracked one"
    )]
    SecretInTracked {
        /// The file.
        origin: String,
        /// The key.
        key: String,
    },
    /// The merged table cannot be written as TOML.
    #[error("the merged config cannot be written as TOML: {0}")]
    Serialize(String),
}

/// The merged config and where each key came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Merged {
    /// The merged table, without `unset` and `extend`.
    pub table: Table,
    /// The last layer that wrote each leaf (a non-table value).
    pub origins: BTreeMap<KeyPath, LayerName>,
    /// Projects the shared file describes but no machine gave a `main`, sorted.
    pub dropped_projects: Vec<String>,
}

impl Merged {
    /// The merged table as TOML text.
    ///
    /// # Errors
    ///
    /// Returns [`LayerError::Serialize`] when the table cannot be written.
    pub fn to_toml_string(&self) -> Result<String, LayerError> {
        toml::to_string(&self.table).map_err(|source| LayerError::Serialize(source.to_string()))
    }
}

type Origins = BTreeMap<KeyPath, LayerName>;

/// Merges the shared, machine and local layers, later layers winning; see `docs/config-sync.md`, section 2.
///
/// # Errors
///
/// Returns the first [`LayerError`] found: bad TOML, reserved keys in the shared layer, a machine-only key in the shared
/// layer, a secret in a tracked layer, an `unset` of a key that is not there or an `extend` without an array to extend.
pub fn merge(
    shared: LayerInput<'_>,
    machine: LayerInput<'_>,
    local: Option<LayerInput<'_>>,
) -> Result<Merged, LayerError> {
    let mut table = Table::new();
    let mut origins = Origins::new();
    for layer in [shared, machine].into_iter().chain(local) {
        apply_layer(&mut table, &mut origins, layer)?;
    }
    let dropped_projects = drop_projects(&mut table, &mut origins);
    Ok(Merged {
        table,
        origins,
        dropped_projects,
    })
}

fn apply_layer(
    table: &mut Table,
    origins: &mut Origins,
    layer: LayerInput<'_>,
) -> Result<(), LayerError> {
    let mut parsed: Table = toml::from_str(layer.text).map_err(|source| LayerError::Parse {
        origin: layer.origin.to_owned(),
        message: source.to_string(),
    })?;
    let unset = parsed.remove(UNSET);
    let extend = parsed.remove(EXTEND);
    let origin = layer.origin;
    if layer.name == LayerName::Shared {
        for (key, present) in [(UNSET, unset.is_some()), (EXTEND, extend.is_some())] {
            if present {
                return Err(LayerError::ReservedInShared {
                    origin: origin.to_owned(),
                    key: key.to_owned(),
                });
            }
        }
        check_leaves(&parsed, &KeyPath::default(), &mut |path| {
            if is_machine_only(path) {
                Err(LayerError::MachineOnlyInShared {
                    origin: origin.to_owned(),
                    key: path.to_string(),
                })
            } else {
                Ok(())
            }
        })?;
    }
    if layer.name != LayerName::Local {
        let mut secret_check = |path: &KeyPath| {
            if is_secret(path) {
                Err(LayerError::SecretInTracked {
                    origin: origin.to_owned(),
                    key: path.to_string(),
                })
            } else {
                Ok(())
            }
        };
        check_leaves(&parsed, &KeyPath::default(), &mut secret_check)?;
        if let Some(Value::Table(extend_table)) = &extend {
            check_leaves(extend_table, &KeyPath::default(), &mut secret_check)?;
        }
    }
    if let Some(unset) = unset {
        apply_unset(table, origins, origin, &unset)?;
    }
    merge_tables(table, parsed, &KeyPath::default(), layer.name, origins);
    if let Some(extend) = extend {
        apply_extend(table, origins, layer, &extend)?;
    }
    Ok(())
}

fn check_leaves(
    table: &Table,
    prefix: &KeyPath,
    check: &mut dyn FnMut(&KeyPath) -> Result<(), LayerError>,
) -> Result<(), LayerError> {
    if table.is_empty() && !prefix.0.is_empty() {
        return check(prefix);
    }
    for (key, value) in table {
        let path = prefix.child(key);
        match value {
            Value::Table(inner) => check_leaves(inner, &path, check)?,
            Value::String(_)
            | Value::Integer(_)
            | Value::Float(_)
            | Value::Boolean(_)
            | Value::Datetime(_)
            | Value::Array(_) => check(&path)?,
        }
    }
    Ok(())
}

fn bad_reserved(origin: &str, key: &str, reason: &str) -> LayerError {
    LayerError::BadReserved {
        origin: origin.to_owned(),
        key: key.to_owned(),
        reason: reason.to_owned(),
    }
}

fn apply_unset(
    table: &mut Table,
    origins: &mut Origins,
    origin: &str,
    unset: &Value,
) -> Result<(), LayerError> {
    let Value::Array(entries) = unset else {
        return Err(bad_reserved(
            origin,
            UNSET,
            "expected an array of key paths",
        ));
    };
    for entry in entries {
        let Value::String(text) = entry else {
            return Err(bad_reserved(origin, UNSET, "every entry must be a string"));
        };
        let path =
            KeyPath::parse(text).map_err(|error| bad_reserved(origin, text, &error.to_string()))?;
        if !remove_path(table, path.segments()) {
            return Err(LayerError::UnsetMissing {
                origin: origin.to_owned(),
                key: path.to_string(),
            });
        }
        origins.retain(|key, _| !key.starts_with(&path));
    }
    Ok(())
}

fn remove_path(table: &mut Table, segments: &[String]) -> bool {
    match segments {
        [] => false,
        [last] => table.remove(last).is_some(),
        [first, rest @ ..] => match table.get_mut(first) {
            Some(Value::Table(inner)) => remove_path(inner, rest),
            _ => false,
        },
    }
}

fn record(value: &Value, path: &KeyPath, layer: LayerName, origins: &mut Origins) {
    match value {
        Value::Table(inner) => {
            for (key, child) in inner {
                record(child, &path.child(key), layer, origins);
            }
        }
        Value::String(_)
        | Value::Integer(_)
        | Value::Float(_)
        | Value::Boolean(_)
        | Value::Datetime(_)
        | Value::Array(_) => {
            origins.insert(path.clone(), layer);
        }
    }
}

fn merge_tables(
    dst: &mut Table,
    src: Table,
    prefix: &KeyPath,
    layer: LayerName,
    origins: &mut Origins,
) {
    for (key, value) in src {
        let path = prefix.child(&key);
        match (dst.get_mut(&key), value) {
            (Some(Value::Table(existing)), Value::Table(incoming)) => {
                merge_tables(existing, incoming, &path, layer, origins);
            }
            (_, incoming) => {
                origins.retain(|known, _| !known.starts_with(&path));
                record(&incoming, &path, layer, origins);
                dst.insert(key, incoming);
            }
        }
    }
}

fn apply_extend(
    table: &mut Table,
    origins: &mut Origins,
    layer: LayerInput<'_>,
    extend: &Value,
) -> Result<(), LayerError> {
    let Value::Table(extend_table) = extend else {
        return Err(bad_reserved(
            layer.origin,
            EXTEND,
            "expected a table shaped like the config",
        ));
    };
    extend_into(table, extend_table, &KeyPath::default(), layer, origins)
}

fn extend_into(
    table: &mut Table,
    extend: &Table,
    prefix: &KeyPath,
    layer: LayerInput<'_>,
    origins: &mut Origins,
) -> Result<(), LayerError> {
    for (key, value) in extend {
        let path = prefix.child(key);
        let target_error = || LayerError::ExtendTarget {
            origin: layer.origin.to_owned(),
            key: path.to_string(),
        };
        match value {
            Value::Table(inner) => match table.get_mut(key) {
                Some(Value::Table(existing)) => {
                    extend_into(existing, inner, &path, layer, origins)?;
                }
                _ if inner.is_empty() => {}
                _ => return Err(target_error()),
            },
            Value::Array(items) => match table.get_mut(key) {
                Some(Value::Array(existing)) => {
                    existing.extend(items.iter().cloned());
                    origins.insert(path.clone(), layer.name);
                }
                _ => return Err(target_error()),
            },
            Value::String(_)
            | Value::Integer(_)
            | Value::Float(_)
            | Value::Boolean(_)
            | Value::Datetime(_) => {
                return Err(bad_reserved(
                    layer.origin,
                    &format!("{EXTEND}.{path}"),
                    "leaves must be arrays",
                ));
            }
        }
    }
    Ok(())
}

fn drop_projects(table: &mut Table, origins: &mut Origins) -> Vec<String> {
    let Some(Value::Table(projects)) = table.get_mut("project") else {
        return Vec::new();
    };
    let mut dropped: Vec<String> = projects
        .iter()
        .filter(|(_, value)| !matches!(value, Value::Table(inner) if inner.contains_key("main")))
        .map(|(name, _)| name.clone())
        .collect();
    dropped.sort();
    for name in &dropped {
        projects.remove(name);
        let path = KeyPath::new(vec!["project".to_owned(), name.clone()]);
        origins.retain(|key, _| !key.starts_with(&path));
    }
    dropped
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "tests fail loudly on a bad fixture")]

    use proptest::prelude::*;

    use super::*;
    use crate::config::Config;

    fn input(name: LayerName, text: &str) -> LayerInput<'_> {
        LayerInput {
            name,
            origin: match name {
                LayerName::Shared => "shared.toml",
                LayerName::Machine => "machine.toml",
                LayerName::Local => "local.toml",
            },
            text,
        }
    }

    fn run(shared: &str, machine: &str, local: Option<&str>) -> Result<Merged, LayerError> {
        merge(
            input(LayerName::Shared, shared),
            input(LayerName::Machine, machine),
            local.map(|text| input(LayerName::Local, text)),
        )
    }

    fn table(text: &str) -> Table {
        toml::from_str(text).unwrap()
    }

    fn path(text: &str) -> KeyPath {
        KeyPath::parse(text).unwrap()
    }

    #[test]
    fn tables_merge_key_by_key() {
        let merged = run("[hooks]\na = 1\nb = 2\n", "[hooks]\nb = 3\nc = 4\n", None).unwrap();
        assert_eq!(
            merged.table,
            table("[hooks]\na = 1\nb = 3\nc = 4\n"),
            "deep merge"
        );
    }

    #[test]
    fn scalars_and_arrays_are_replaced() {
        let merged = run("x = 1\nlist = [1, 2]\n", "x = 2\nlist = [3]\n", None).unwrap();
        assert_eq!(
            merged.table,
            table("x = 2\nlist = [3]\n"),
            "replaced as a whole"
        );
    }

    #[test]
    fn arrays_of_tables_are_replaced() {
        let merged = run("[[t]]\na = 1\n[[t]]\na = 2\n", "[[t]]\na = 9\n", None).unwrap();
        assert_eq!(
            merged.table,
            table("[[t]]\na = 9\n"),
            "array of tables replaced"
        );
    }

    #[test]
    fn later_layer_replaces_across_table_and_non_table() {
        let merged = run("a = 1\n[b]\nc = 1\n", "[a]\nz = 1\n", Some("b = 5\n")).unwrap();
        assert_eq!(merged.table, table("b = 5\n[a]\nz = 1\n"), "kind change");
        assert_eq!(
            merged.origins.get(&path("a.z")),
            Some(&LayerName::Machine),
            "table origin"
        );
        assert_eq!(
            merged.origins.get(&path("a")),
            None,
            "old scalar origin gone"
        );
        assert_eq!(
            merged.origins.get(&path("b")),
            Some(&LayerName::Local),
            "scalar origin"
        );
        assert_eq!(
            merged.origins.get(&path("b.c")),
            None,
            "old leaf origin gone"
        );
    }

    #[test]
    fn origins_name_the_last_writer_per_leaf() {
        let merged = run(
            "[hooks]\na = 1\nb = 2\n",
            "[hooks]\nb = 3\n",
            Some("[hooks]\nc = 4\n"),
        )
        .unwrap();
        assert_eq!(
            merged.origins.get(&path("hooks.a")),
            Some(&LayerName::Shared),
            "a"
        );
        assert_eq!(
            merged.origins.get(&path("hooks.b")),
            Some(&LayerName::Machine),
            "b"
        );
        assert_eq!(
            merged.origins.get(&path("hooks.c")),
            Some(&LayerName::Local),
            "c"
        );
        assert_eq!(merged.origins.len(), 3, "leaves only");
    }

    #[test]
    fn unset_removes_a_key_and_its_origins() {
        let merged = run("[hooks]\na = 1\nb = 2\n", "unset = [\"hooks.a\"]\n", None).unwrap();
        assert_eq!(merged.table, table("[hooks]\nb = 2\n"), "key removed");
        assert_eq!(merged.origins.get(&path("hooks.a")), None, "origin removed");
        assert!(!merged.table.contains_key("unset"), "reserved key gone");
    }

    #[test]
    fn unset_removes_a_whole_table() {
        let merged = run(
            "[x]\na = 1\n[x.y]\nb = 2\n[z]\nc = 3\n",
            "unset = [\"x\"]\n",
            None,
        )
        .unwrap();
        assert_eq!(merged.table, table("[z]\nc = 3\n"), "table removed");
        assert_eq!(merged.origins.len(), 1, "origins beneath removed");
    }

    #[test]
    fn unset_then_the_same_layer_sets_the_key_again() {
        let merged = run("[x]\na = 1\nb = 2\n", "unset = [\"x\"]\n[x]\nc = 3\n", None).unwrap();
        assert_eq!(
            merged.table,
            table("[x]\nc = 3\n"),
            "unset runs before the merge"
        );
    }

    #[test]
    fn unset_missing_is_an_error() {
        let error = run("a = 1\n", "unset = [\"b\"]\n", None).unwrap_err();
        assert_eq!(
            error,
            LayerError::UnsetMissing {
                origin: "machine.toml".to_owned(),
                key: "b".to_owned()
            },
            "missing"
        );
        assert!(error.to_string().contains("machine.toml"), "names the file");
        let through_scalar = run("a = 1\n", "unset = [\"a.b\"]\n", None).unwrap_err();
        assert!(
            matches!(through_scalar, LayerError::UnsetMissing { .. }),
            "through a scalar"
        );
    }

    #[test]
    fn unset_must_be_an_array_of_valid_paths() {
        for text in ["unset = \"a\"\n", "unset = [1]\n", "unset = [\"a..b\"]\n"] {
            assert!(
                matches!(
                    run("a = 1\n", text, None),
                    Err(LayerError::BadReserved { .. })
                ),
                "{text}"
            );
        }
    }

    #[test]
    fn extend_appends_to_an_existing_array() {
        let merged = run(
            "[hooks]\nrules = [\"a\"]\n",
            "[extend.hooks]\nrules = [\"b\"]\n",
            Some("[extend.hooks]\nrules = [\"c\"]\n"),
        )
        .unwrap();
        assert_eq!(
            merged.table,
            table("[hooks]\nrules = [\"a\", \"b\", \"c\"]\n"),
            "appended in order"
        );
        assert_eq!(
            merged.origins.get(&path("hooks.rules")),
            Some(&LayerName::Local),
            "extender marks the leaf"
        );
        assert!(!merged.table.contains_key("extend"), "reserved key gone");
    }

    #[test]
    fn extend_needs_an_existing_array() {
        let missing = run("a = 1\n", "[extend]\nb = [1]\n", None).unwrap_err();
        assert_eq!(
            missing,
            LayerError::ExtendTarget {
                origin: "machine.toml".to_owned(),
                key: "b".to_owned()
            },
            "missing target"
        );
        let scalar = run("a = 1\n", "[extend]\na = [1]\n", None).unwrap_err();
        assert!(
            matches!(scalar, LayerError::ExtendTarget { .. }),
            "scalar target"
        );
        let leaf = run("a = [1]\n", "[extend]\na = 1\n", None).unwrap_err();
        assert!(
            matches!(leaf, LayerError::BadReserved { .. }),
            "leaf not an array"
        );
    }

    #[test]
    fn reserved_keys_are_refused_in_shared() {
        for text in ["unset = [\"a\"]\n", "[extend]\na = [1]\n"] {
            let error = run(text, "", None).unwrap_err();
            assert!(
                matches!(error, LayerError::ReservedInShared { .. }),
                "{text}"
            );
        }
    }

    #[test]
    fn machine_only_keys_are_refused_in_shared() {
        let cases = [
            ("[machine]\nhost = \"wmux\"\n", "machine.host"),
            ("[project.me-sim]\nmain = \"S:/x\"\n", "project.me-sim.main"),
            (
                "[project.me-sim]\nread_only_paths = [\"a\"]\n",
                "project.me-sim.read_only_paths",
            ),
            (
                "[project.me-sim]\nextra_writable = []\n",
                "project.me-sim.extra_writable",
            ),
            (
                "[hooks]\norchestrator_writable = []\n",
                "hooks.orchestrator_writable",
            ),
            (
                "[hooks]\norchestrator_repos = []\n",
                "hooks.orchestrator_repos",
            ),
            ("[hooks]\nbrief_writable = []\n", "hooks.brief_writable"),
            ("[hooks]\nplugin_dir = \"x\"\n", "hooks.plugin_dir"),
            ("[watch]\nroots = []\n", "watch.roots"),
        ];
        for (text, key) in cases {
            let error = run(text, "", None).unwrap_err();
            assert_eq!(
                error,
                LayerError::MachineOnlyInShared {
                    origin: "shared.toml".to_owned(),
                    key: key.to_owned()
                },
                "{text}"
            );
        }
        assert!(
            run(
                "[hooks]\nenforce = true\n[project.me-sim]\nheavy_slots = 2\n",
                "",
                None
            )
            .is_ok(),
            "portable keys pass"
        );
    }

    #[test]
    fn machine_only_keys_are_allowed_in_machine_and_local() {
        let merged = run(
            "",
            "[machine]\nhost = \"wmux\"\n",
            Some("[watch]\nroots = [\"a\"]\n"),
        )
        .unwrap();
        assert_eq!(
            merged.origins.get(&path("machine.host")),
            Some(&LayerName::Machine),
            "machine"
        );
        assert_eq!(
            merged.origins.get(&path("watch.roots")),
            Some(&LayerName::Local),
            "local"
        );
    }

    #[test]
    fn secrets_are_refused_in_tracked_layers_but_allowed_locally() {
        let shared = run("[machine]\nnotify_ntfy = \"t\"\n", "", None).unwrap_err();
        assert!(
            matches!(shared, LayerError::MachineOnlyInShared { .. }),
            "shared refuses machine.* first: {shared}"
        );
        let machine = run("", "[machine]\nnotify_ntfy = \"t\"\n", None).unwrap_err();
        assert_eq!(
            machine,
            LayerError::SecretInTracked {
                origin: "machine.toml".to_owned(),
                key: "machine.notify_ntfy".to_owned()
            },
            "machine"
        );
        let empty = run("", "[machine]\nnotify_ntfy = \"\"\n", None).unwrap_err();
        assert!(
            matches!(empty, LayerError::SecretInTracked { .. }),
            "empty value still refused"
        );
        let local = run("", "", Some("[machine]\nnotify_ntfy = \"t\"\n")).unwrap();
        assert_eq!(
            local.origins.get(&path("machine.notify_ntfy")),
            Some(&LayerName::Local),
            "local allowed"
        );
    }

    #[test]
    fn secret_in_machine_extend_is_refused() {
        let error = run("", "[extend.machine]\nnotify_ntfy = [\"t\"]\n", None).unwrap_err();
        assert!(
            matches!(error, LayerError::SecretInTracked { .. }),
            "extend checked too"
        );
    }

    #[test]
    fn projects_without_a_main_are_dropped() {
        let shared = "[project.me-sim]\nheavy_slots = 2\n[project.me-studio]\nheavy_slots = 1\n[project.alpha]\nheavy_slots = 3\n";
        let machine = "[project.me-sim]\nmain = \"S:/x\"\n";
        let merged = run(shared, machine, None).unwrap();
        assert_eq!(
            merged.dropped_projects,
            vec!["alpha".to_owned(), "me-studio".to_owned()],
            "sorted"
        );
        assert_eq!(
            merged.table,
            table("[project.me-sim]\nheavy_slots = 2\nmain = \"S:/x\"\n"),
            "kept"
        );
        assert!(
            merged
                .origins
                .keys()
                .all(|key| key.segments()[1] == "me-sim"),
            "origins removed"
        );
    }

    #[test]
    fn parse_errors_name_the_file() {
        let error = run("a = \n", "", None).unwrap_err();
        assert!(
            matches!(&error, LayerError::Parse { origin, .. } if origin == "shared.toml"),
            "parse error"
        );
        assert!(
            error.to_string().starts_with("shared.toml:"),
            "message names the file"
        );
    }

    #[test]
    fn key_paths_round_trip() {
        for text in [
            "a",
            "usage.prices.claude-opus-5-5",
            "a.\"b.c\".d",
            "project.me-sim.main",
            "\"a b\"",
        ] {
            let parsed = KeyPath::parse(text).unwrap();
            assert_eq!(parsed.to_string(), text, "{text}");
        }
        let quoted = path("usage.prices.\"claude-opus-5-5\"");
        assert_eq!(
            quoted.segments(),
            ["usage", "prices", "claude-opus-5-5"],
            "quoted bare segment"
        );
        assert_eq!(
            quoted.to_string(),
            "usage.prices.claude-opus-5-5",
            "displayed bare"
        );
        assert_eq!(
            path("a.\"b.c\".d").segments(),
            ["a", "b.c", "d"],
            "quoted dot"
        );
        let odd = KeyPath::new(vec!["a\"b".to_owned(), "c\\d".to_owned()]);
        assert_eq!(KeyPath::parse(&odd.to_string()).unwrap(), odd, "escapes");
    }

    #[test]
    fn key_paths_reject_bad_input() {
        for text in ["", "a..b", ".a", "a.", "a.\"\"", "a.\"b", "a b", "a\"b\""] {
            assert!(KeyPath::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn layer_names_display() {
        assert_eq!(LayerName::Shared.to_string(), "shared", "shared");
        assert_eq!(LayerName::Machine.to_string(), "machine", "machine");
        assert_eq!(LayerName::Local.to_string(), "local", "local");
    }

    #[test]
    fn a_shared_and_machine_pair_yields_a_valid_config() {
        let shared = r#"
[models]
default = "sonnet"
allowed = ["haiku", "sonnet", "opus", "fable"]
fable_needs_user = true

[watch]
disk_warn_gb = 15
disk_stop_gb = 5

[project.me-sim]
worktree_new = "scripts/worktree.sh new {slug}"
worktree_remove = "scripts/worktree.sh remove {slug}"
worktree_path = "{main}-{slug}"
deliver = "scripts/ship.sh"
merge = "deliver"
allowed_repos = ["Ivan091/me-sim"]

[project.only-elsewhere]
deliver = "scripts/ship.sh"
"#;
        let machine = r#"
[machine]
host = "wmux"
max_workers = 10
rules_dir = "~/.headwork/rules"

[watch]
roots = ["S:/Code"]

[project.me-sim]
main = "S:/Code/me-sim/me-sim"
"#;
        let merged = run(shared, machine, None).unwrap();
        assert_eq!(
            merged.dropped_projects,
            vec!["only-elsewhere".to_owned()],
            "dropped"
        );
        let text = merged.to_toml_string().unwrap();
        let config = Config::parse(&text, "merged");
        assert!(config.is_ok(), "merged config parses: {config:?}");
    }

    fn integer() -> impl Strategy<Value = Value> {
        any::<i32>().prop_map(|n| Value::Integer(i64::from(n)))
    }

    fn leaf() -> impl Strategy<Value = Value> {
        prop_oneof![
            integer(),
            "[a-z]{0,6}".prop_map(Value::String),
            prop::collection::vec(integer(), 0..4).prop_map(Value::Array),
        ]
    }

    fn arbitrary_table() -> impl Strategy<Value = Table> {
        let inner = prop::collection::btree_map("[a-z]{1,4}", leaf(), 0..4)
            .prop_map(|map| Value::Table(map.into_iter().collect()));
        prop::collection::btree_map("[a-z]{1,4}", prop_oneof![leaf(), inner], 0..5).prop_map(
            |map| {
                map.into_iter()
                    .filter(|(key, _)| {
                        !matches!(
                            key.as_str(),
                            "machine" | "project" | "hooks" | "watch" | "unset" | "extend"
                        )
                    })
                    .collect()
            },
        )
    }

    proptest! {
        #[test]
        fn merging_a_layer_with_itself_changes_nothing(original in arbitrary_table()) {
            let text = toml::to_string(&original).unwrap();
            let merged = run(&text, &text, None).unwrap();
            prop_assert_eq!(merged.table, original);
        }
    }
}
