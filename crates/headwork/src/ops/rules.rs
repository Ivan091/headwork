//! The soft rules hooks inject: `global.md`, the role's `root.md` or `child.md`, and the project's `<project>.md`.
//!
//! Each file comes from the rules folder (`machine.rules_dir`, else `<home>/rules`); `root.md` and `child.md` fall
//! back to the copies built into the binary from the repository's `rules/`. Placeholders such as `{name}` and
//! `{close}` are filled per session. A keep task's rules lose their "Closing yourself" section.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use headwork_core::config::Delivery;
use headwork_core::{Config, Role};
use headwork_store::{AddedRule, Task};

use super::slashed;
use crate::home::Home;

/// The built-in `root.md`.
pub const ROOT_RULES: &str = include_str!("../../../../rules/root.md");
/// The built-in `child.md`.
pub const CHILD_RULES: &str = include_str!("../../../../rules/child.md");

/// The heading of the section a keep task's rules leave out.
pub const CLOSING_HEADING: &str = "## Closing yourself";

/// The most `additionalContext` a hook may return before Claude moves it to a file.
pub const CONTEXT_LIMIT: usize = 10_000;

/// The rules files as read, with a hash of all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleFiles {
    dir: PathBuf,
    global: Option<String>,
    root: String,
    child: String,
    projects: Vec<(String, String)>,
    added: Vec<AddedRule>,
    hash: String,
}

/// What a child's rules are filled with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card<'a> {
    /// The task.
    pub task: &'a Task,
    /// Its folder: the worktree, or the main checkout.
    pub dir: &'a Path,
}

impl RuleFiles {
    /// Reads the rules folder; `added` are the rules added with `rule_add`, which join the files.
    ///
    /// # Errors
    ///
    /// Fails when a file exists and cannot be read.
    pub fn load(home: &Home, config: &Config, added: &[AddedRule]) -> Result<Self> {
        let dir = config
            .machine
            .rules_dir
            .as_deref()
            .map_or_else(|| home.root().join("rules"), |dir| home.expand(dir));
        let read = |name: &str| -> Result<Option<String>> {
            let path = dir.join(name);
            match std::fs::read_to_string(&path) {
                Ok(text) => Ok(Some(text)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
            }
        };
        let global = read("global.md")?;
        let root = read("root.md")?.unwrap_or_else(|| ROOT_RULES.to_owned());
        let child = read("child.md")?.unwrap_or_else(|| CHILD_RULES.to_owned());
        let mut projects = Vec::new();
        for name in config.projects.keys() {
            if let Some(text) = read(&format!("{name}.md"))? {
                projects.push((name.to_string(), text));
            }
        }
        let mut all = format!(
            "{}\u{0}{root}\u{0}{child}",
            global.as_deref().unwrap_or_default()
        );
        for (name, text) in &projects {
            all.extend(["\u{0}", name.as_str(), "\u{0}", text.as_str()]);
        }
        for rule in added {
            all.extend(["\u{0}", rule.scope.as_str(), "\u{0}", rule.body.as_str()]);
        }
        Ok(Self {
            dir,
            global,
            root,
            child,
            projects,
            added: added.to_vec(),
            hash: format!("{:016x}", fnv1a(all.as_bytes())),
        })
    }

    /// The hash of every rules file; a change bumps the rules version.
    #[must_use]
    pub fn hash(&self) -> &str {
        &self.hash
    }

    /// The rules a session gets, placeholders filled. A text over [`CONTEXT_LIMIT`] becomes a short pointer to the
    /// files instead.
    #[must_use]
    pub fn render(
        &self,
        role: Role,
        config: &Config,
        exe: &Path,
        card: Option<&Card<'_>>,
    ) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(global) = &self.global {
            parts.push(global.clone());
        }
        match role {
            Role::Root => parts.push(self.root.clone()),
            Role::Child => {
                let keep = card.is_some_and(|card| card.task.keep);
                parts.push(if keep {
                    without_closing(&self.child)
                } else {
                    self.child.clone()
                });
                if let Some(card) = card
                    && let Some((_, text)) = self
                        .projects
                        .iter()
                        .find(|(name, _)| name == card.task.project().as_str())
                {
                    parts.push(text.clone());
                }
            }
        }
        let project = card.map(|card| card.task.project().as_str());
        let scopes = ["global", role.as_str()].into_iter().chain(project);
        let added: Vec<String> = scopes
            .flat_map(|scope| self.added.iter().filter(move |rule| rule.scope == scope))
            .map(|rule| format!("- {}", rule.body.trim()))
            .collect();
        if !added.is_empty() {
            parts.push(format!(
                "## Rules added by the root\n\n{}",
                added.join("\n")
            ));
        }
        let text = fill(&parts.join("\n\n"), config, exe, card);
        if text.len() <= CONTEXT_LIMIT {
            return text;
        }
        format!(
            "headwork: your rules are longer than a hook may inject. Read them now, in this order: {}/global.md (if \
             present), {}/{}.md, and your project's file there.",
            slashed(&self.dir),
            slashed(&self.dir),
            role.as_str()
        )
    }
}

/// Drops the "Closing yourself" section, up to the next heading of the same level or the end.
#[must_use]
pub fn without_closing(text: &str) -> String {
    let mut out = Vec::new();
    let mut skipping = false;
    for line in text.lines() {
        if line.trim_end() == CLOSING_HEADING {
            skipping = true;
            continue;
        }
        if skipping && line.starts_with("## ") {
            skipping = false;
        }
        if !skipping {
            out.push(line);
        }
    }
    let mut joined = out.join("\n").trim_end().to_owned();
    joined.push('\n');
    joined
}

fn fill(text: &str, config: &Config, exe: &Path, card: Option<&Card<'_>>) -> String {
    let exe_text = slashed(exe);
    let mode = if config.hooks.enforce {
        "enforced"
    } else {
        "in shadow mode"
    };
    let mut out = text
        .replace(&slot("exe"), &exe_text)
        .replace(&slot("mode"), mode);
    if let Some(card) = card {
        let task = card.task;
        let name = task.name.to_string();
        let deliver = match config.project(task.project()).map(|project| &project.deliver) {
            Some(Delivery::Command(command)) => format!(
                "`{command}` from your folder; it replaces the global commit, push, create-pr, contribute and ship skills"
            ),
            Some(Delivery::Pr) | None => {
                "the global `contribute` and `ship` skills: branch, commit, push, pull request, merge".to_owned()
            }
        };
        out = out
            .replace(&slot("name"), &name)
            .replace(&slot("project"), task.project().as_str())
            .replace(&slot("brief"), &task.brief_path)
            .replace(&slot("dir"), &slashed(card.dir))
            .replace(&slot("deliver"), &deliver)
            .replace(&slot("close"), &format!("\"{exe_text}\" close {name}"));
    }
    out
}

/// FNV-1a, 64 bits: enough to notice a changed file.
#[must_use]
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A placeholder as the rules files spell it: `{name}`.
fn slot(name: &str) -> String {
    ["{", name, "}"].concat()
}
