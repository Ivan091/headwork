//! Review flows (`docs/review-flows.md`, section 5).
//!
//! The built-in ones are embedded from the repository's `flows/`; a `<home>/flows/<name>.toml` file overrides or adds
//! one by name, its prompts read from beside it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use headwork_core::Model;
use headwork_core::review::{Flow, FlowError, Node};
use serde::Serialize;

/// Every built-in flow file and prompt, by its path under `flows/`.
pub const BUILTIN_FILES: &[(&str, &str)] = &[
    ("code.toml", include_str!("../../../../flows/code.toml")),
    (
        "code/correctness.md",
        include_str!("../../../../flows/code/correctness.md"),
    ),
    (
        "code/tests.md",
        include_str!("../../../../flows/code/tests.md"),
    ),
    (
        "code/rules.md",
        include_str!("../../../../flows/code/rules.md"),
    ),
    (
        "code/errors.md",
        include_str!("../../../../flows/code/errors.md"),
    ),
    (
        "code/design.md",
        include_str!("../../../../flows/code/design.md"),
    ),
    (
        "code/docs.md",
        include_str!("../../../../flows/code/docs.md"),
    ),
    (
        "code/concurrency.md",
        include_str!("../../../../flows/code/concurrency.md"),
    ),
    (
        "code/security.md",
        include_str!("../../../../flows/code/security.md"),
    ),
    (
        "code/challenge-approach.md",
        include_str!("../../../../flows/code/challenge-approach.md"),
    ),
    (
        "code/challenge-scope.md",
        include_str!("../../../../flows/code/challenge-scope.md"),
    ),
    (
        "code/verify.md",
        include_str!("../../../../flows/code/verify.md"),
    ),
    ("plan.toml", include_str!("../../../../flows/plan.toml")),
    (
        "plan/feasibility.md",
        include_str!("../../../../flows/plan/feasibility.md"),
    ),
    (
        "plan/consistency.md",
        include_str!("../../../../flows/plan/consistency.md"),
    ),
    (
        "plan/open-points.md",
        include_str!("../../../../flows/plan/open-points.md"),
    ),
    (
        "plan/effort.md",
        include_str!("../../../../flows/plan/effort.md"),
    ),
    (
        "plan/challenge-scope.md",
        include_str!("../../../../flows/plan/challenge-scope.md"),
    ),
    (
        "plan/verify.md",
        include_str!("../../../../flows/plan/verify.md"),
    ),
];

/// The folder of a home's own flows.
#[must_use]
pub fn home_flows_dir(home: &Path) -> PathBuf {
    home.join("flows")
}

/// Where a flow comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "path", rename_all = "lowercase")]
pub enum Origin {
    /// Built into the binary.
    Builtin,
    /// A file in the home's `flows/` folder.
    Home(PathBuf),
}

/// A flow with the place its prompts are read from.
#[derive(Debug, Clone)]
pub struct LoadedFlow {
    /// The validated flow.
    pub flow: Flow,
    /// Where it came from.
    pub origin: Origin,
}

impl LoadedFlow {
    /// A node's prompt text.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::PromptMissing`] when the file is gone or unreadable.
    pub fn prompt(&self, node: &Node) -> Result<String, FlowError> {
        let missing = || FlowError::PromptMissing {
            node: node.id.to_string(),
            prompt: node.prompt.clone(),
        };
        match &self.origin {
            Origin::Builtin => builtin(&node.prompt).map(str::to_owned).ok_or_else(missing),
            Origin::Home(file) => {
                let dir = file.parent().ok_or_else(missing)?;
                std::fs::read_to_string(dir.join(&node.prompt)).map_err(|_| missing())
            }
        }
    }

    fn prompt_exists(&self, prompt: &str) -> bool {
        match &self.origin {
            Origin::Builtin => builtin(prompt).is_some(),
            Origin::Home(file) => file.parent().is_some_and(|dir| dir.join(prompt).is_file()),
        }
    }
}

fn builtin(path: &str) -> Option<&'static str> {
    BUILTIN_FILES
        .iter()
        .find(|(name, _)| *name == path)
        .map(|(_, text)| *text)
}

/// One flow's check: its name, where it comes from, and every problem found; empty when it is good.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FlowReport {
    /// The flow's name, from its file name.
    pub name: String,
    /// Where it comes from.
    pub origin: Origin,
    /// What the flow is for, when it parsed.
    pub description: Option<String>,
    /// Every problem; empty for a good flow.
    pub problems: Vec<String>,
}

impl FlowReport {
    /// Whether the flow is good.
    #[must_use]
    pub const fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

fn sources(home: &Path) -> BTreeMap<String, (Origin, Result<String, String>)> {
    let mut found = BTreeMap::new();
    for (path, text) in BUILTIN_FILES {
        if let Some(name) = path.strip_suffix(".toml") {
            found.insert(name.to_owned(), (Origin::Builtin, Ok((*text).to_owned())));
        }
    }
    let dir = home_flows_dir(home);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return found;
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    files.sort();
    for file in files {
        let Some(name) = file
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_owned)
        else {
            continue;
        };
        let text = std::fs::read_to_string(&file)
            .map_err(|error| format!("reading {}: {error}", file.display()));
        found.insert(name, (Origin::Home(file), text));
    }
    found
}

fn load(
    name: &str,
    origin: Origin,
    text: Result<String, String>,
    allowed: &[Model],
) -> (FlowReport, Option<LoadedFlow>) {
    let mut report = FlowReport {
        name: name.to_owned(),
        origin: origin.clone(),
        description: None,
        problems: Vec::new(),
    };
    let flow = match text.and_then(|text| Flow::parse(&text).map_err(|error| error.to_string())) {
        Ok(flow) => flow,
        Err(problem) => {
            report.problems.push(problem);
            return (report, None);
        }
    };
    report.description = Some(flow.description().to_owned());
    if flow.name().as_str() != name {
        report.problems.push(format!(
            "the file is named `{name}` but the flow calls itself `{}`",
            flow.name()
        ));
    }
    let loaded = LoadedFlow { flow, origin };
    report.problems.extend(
        loaded
            .flow
            .check(|prompt| loaded.prompt_exists(prompt), allowed)
            .iter()
            .map(ToString::to_string),
    );
    let good = report.ok();
    (report, good.then_some(loaded))
}

/// Checks every flow, built-in and the home's, by name; a home file replaces the built-in of the same name.
#[must_use]
pub fn check_all(home: &Path, allowed: &[Model]) -> Vec<FlowReport> {
    sources(home)
        .into_iter()
        .map(|(name, (origin, text))| load(&name, origin, text, allowed).0)
        .collect()
}

/// A flow by name, validated, the home's file first.
///
/// # Errors
///
/// Returns every problem of the flow joined into one message, or that no flow has the name.
pub fn load_flow(home: &Path, name: &str, allowed: &[Model]) -> Result<LoadedFlow, String> {
    let mut all = sources(home);
    let Some((origin, text)) = all.remove(name) else {
        let known: Vec<String> = all.into_keys().collect();
        return Err(format!("no flow `{name}`; known: {}", known.join(", ")));
    };
    match load(name, origin, text, allowed) {
        (_, Some(loaded)) => Ok(loaded),
        (report, None) => Err(format!("flow `{name}`: {}", report.problems.join("; "))),
    }
}
