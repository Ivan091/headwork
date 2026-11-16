//! `import`: adopt what today's orchestrate scripts started, so headwork can shadow the current setup.
//!
//! Workers are found two ways: running wmux agents labelled `<project>/<slug>` (what the old spawn script makes), and
//! Claude panes wmux restored after a restart, which are no longer agents. For those, wmux's `agent-state` gives the
//! session id, and the session's transcript gives the name (from the first prompt, "You are the worker session …", or
//! "child" in the scripts' wording) and the folder it runs in. Projects come from `projects.conf`; worktrees from
//! `git worktree list`.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::Result;
use headwork_adapters::host::{Pane, SurfaceId, SurfaceState, WorkspaceId};
use headwork_adapters::worktree::{Worktrees, same_path};
use headwork_core::config::TaskVars;
use headwork_core::{Config, Model, ProjectName, TaskName};
use headwork_store::{AdoptTask, Store, StoreError};
use serde::Serialize;

use super::{Env, slashed};

/// The words of the first prompt a worker gets: from headwork, and from the orchestrate scripts, which call it a child.
const WORKER_PROMPTS: &[&str] = &["You are the worker session ", "You are the child session "];

/// The heading the old spawn script appends to every brief except a keep task's.
const CLOSING_HEADING: &str = "## Closing yourself";

/// How far into a transcript the name and folder are looked for.
const TRANSCRIPT_LINES: usize = 400;

/// Where today's state lives.
#[derive(Debug, Clone, Copy)]
pub struct Sources<'a> {
    /// The Claude config folder, holding `projects/<folder>/<session>.jsonl`.
    pub claude_dir: &'a Path,
    /// The orchestrate skill's brief copies.
    pub briefs_dir: &'a Path,
}

/// What an import found and did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    /// Tasks adopted as `working`.
    pub adopted: Vec<String>,
    /// Workers left alone, with the reason.
    pub skipped: Vec<String>,
    /// Worktrees no running worker stands in.
    pub stray_worktrees: Vec<String>,
}

/// A running worker found on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Found {
    name: TaskName,
    surface: SurfaceId,
    workspace: Option<WorkspaceId>,
    pid: Option<u32>,
    session_id: Option<String>,
    cwd: Option<PathBuf>,
    model: Option<Model>,
    source: &'static str,
}

/// The projects of an orchestrate `projects.conf` (`name=path` lines) that the config lacks.
#[must_use]
pub fn missing_projects(conf: &str, config: &Config) -> Vec<(String, String)> {
    conf.lines()
        .filter_map(|line| line.trim().split_once('='))
        .map(|(name, path)| (name.trim().to_owned(), path.trim().to_owned()))
        .filter(|(name, path)| !name.is_empty() && !path.is_empty() && !name.starts_with('#'))
        .filter(|(name, _)| {
            ProjectName::new(name.as_str()).is_ok_and(|name| config.project(&name).is_none())
        })
        .collect()
}

/// A `[project.<name>]` section for a project found in `projects.conf`.
///
/// It uses what the main checkout shows: its own worktree and ship scripts when it has them, else the built-in
/// worktrees and the pull request flow, and its `origin` as the allowed repository.
#[must_use]
pub fn config_section(name: &str, main: &str, origin: Option<&str>) -> String {
    let dir = Path::new(main);
    let script = |file: &str| dir.join("scripts").join(file).is_file();
    let (new, remove) = if script("worktree.sh") {
        (
            "scripts/worktree.sh new {slug}",
            "scripts/worktree.sh remove {slug}",
        )
    } else {
        ("builtin", "builtin")
    };
    let deliver = if script("ship.sh") {
        "scripts/ship.sh"
    } else {
        "pr"
    };
    let repos = origin
        .and_then(repo_of_remote)
        .map(|repo| format!("allowed_repos = [\"{repo}\"]\n"))
        .unwrap_or_default();
    format!(
        "\n[project.{name}]\nmain = \"{main}\"\nworktree_new = \"{new}\"\nworktree_remove = \"{remove}\"\n\
         worktree_path = \"{{main}}-{{slug}}\"\ndeliver = \"{deliver}\"\nmerge = \"deliver\"\n{repos}"
    )
}

/// `owner/name` from a remote URL such as `git@github.com:owner/name.git`.
#[must_use]
pub fn repo_of_remote(url: &str) -> Option<String> {
    let mut parts = url.trim().trim_end_matches(".git").rsplit(['/', ':']);
    let name = parts.next()?;
    let owner = parts.next()?;
    (!owner.is_empty() && !name.is_empty()).then(|| format!("{owner}/{name}"))
}

/// Adopts every running worker of a configured project that has no live task yet.
///
/// # Errors
///
/// Fails when the host or the store fails.
pub fn import(
    store: &mut Store,
    env: &Env<'_>,
    sources: Sources<'_>,
    dry_run: bool,
) -> Result<ImportReport> {
    let mut report = ImportReport::default();
    let found = discover(env, sources)?;
    let mut used_worktrees = BTreeSet::new();
    for child in found {
        let name = child.name.to_string();
        let adopt = match plan(env, sources, &child) {
            Ok(adopt) => adopt,
            Err(reason) => {
                report.skipped.push(format!("{name}: {reason}"));
                continue;
            }
        };
        if let Some((path, _)) = &adopt.worktree {
            used_worktrees.insert(path.to_lowercase());
        }
        if dry_run {
            if store
                .find_task(&name)?
                .is_some_and(|task| task.state.holds_name())
            {
                report
                    .skipped
                    .push(format!("{name}: a live task already has the name"));
            } else {
                report.adopted.push(name);
            }
            continue;
        }
        match store.adopt_task(env.config, &adopt, env.actor) {
            Ok(_) => report.adopted.push(name),
            Err(StoreError::NameTaken(_)) => report
                .skipped
                .push(format!("{name}: a live task already has the name")),
            Err(error) => return Err(error.into()),
        }
    }
    report.stray_worktrees = stray_worktrees(env, &used_worktrees);
    Ok(report)
}

fn discover(env: &Env<'_>, sources: Sources<'_>) -> Result<Vec<Found>> {
    let projects: Vec<ProjectName> = env.config.projects.keys().cloned().collect();
    let states = env.host.surface_states()?;
    let session_of = |surface: &SurfaceId| {
        states
            .iter()
            .find(|state| &state.surface == surface)
            .and_then(|state| state.session_id.clone())
    };
    let mut live = Vec::new();
    for ws in env.host.workspaces()? {
        for surface in env.host.surfaces(&ws.id)? {
            live.push((surface, ws.id.clone()));
        }
    }
    let workspace_of = |surface: &SurfaceId| {
        live.iter()
            .find(|(s, _)| s == surface)
            .map(|(_, ws)| ws.clone())
    };
    let mut found = Vec::new();
    for pane in env.host.panes()?.into_iter().filter(|pane| pane.running) {
        if workspace_of(&pane.surface).is_none() {
            continue;
        }
        if let Some(name) = TaskName::resolve(&pane.label, &projects) {
            found.push(from_agent(&pane, name, session_of(&pane.surface)));
        }
    }
    for state in &states {
        let Some(workspace) = workspace_of(&state.surface) else {
            continue;
        };
        if found.iter().any(|f| f.surface == state.surface) {
            continue;
        }
        if let Some(child) = from_transcript(sources, state, workspace, &projects) {
            found.push(child);
        }
    }
    Ok(found)
}

fn from_agent(pane: &Pane, name: TaskName, session_id: Option<String>) -> Found {
    let model = pane
        .command
        .split_whitespace()
        .skip_while(|word| *word != "--model")
        .nth(1)
        .and_then(|word| word.parse().ok());
    Found {
        name,
        surface: pane.surface.clone(),
        workspace: pane.workspace.clone(),
        pid: pane.pid,
        session_id,
        cwd: None,
        model,
        source: "wmux agent label",
    }
}

fn from_transcript(
    sources: Sources<'_>,
    state: &SurfaceState,
    workspace: WorkspaceId,
    projects: &[ProjectName],
) -> Option<Found> {
    let session_id = state.session_id.as_ref()?;
    let transcript = find_transcript(sources.claude_dir, session_id)?;
    let facts = read_transcript(&transcript);
    let name = facts
        .name
        .as_deref()
        .and_then(|n| TaskName::resolve(n, projects))?;
    Some(Found {
        name,
        surface: state.surface.clone(),
        workspace: Some(workspace),
        pid: None,
        session_id: Some(session_id.clone()),
        cwd: facts.cwd,
        model: facts.model,
        source: "wmux restored pane and its transcript",
    })
}

fn find_transcript(claude_dir: &Path, session_id: &str) -> Option<PathBuf> {
    let file = format!("{session_id}.jsonl");
    std::fs::read_dir(claude_dir.join("projects"))
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(&file))
        .find(|path| path.is_file())
}

/// What a transcript says about its session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscriptFacts {
    /// The worker name from the first prompt.
    pub name: Option<String>,
    /// The folder the session runs in.
    pub cwd: Option<PathBuf>,
    /// The model of its first answer.
    pub model: Option<Model>,
}

/// Reads the start of a transcript for the worker name, the folder and the model.
#[must_use]
pub fn read_transcript(path: &Path) -> TranscriptFacts {
    let mut facts = TranscriptFacts::default();
    let Ok(file) = std::fs::File::open(path) else {
        return facts;
    };
    for line in BufReader::new(file)
        .lines()
        .take(TRANSCRIPT_LINES)
        .map_while(Result::ok)
    {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if facts.cwd.is_none() {
            facts.cwd = value["cwd"].as_str().map(PathBuf::from);
        }
        if facts.model.is_none() {
            facts.model = value["message"]["model"].as_str().and_then(model_of);
        }
        if facts.name.is_none() && value["type"] == "user" {
            facts.name = text_of(&value["message"]["content"]).and_then(|text| worker_name(&text));
        }
        if facts.name.is_some() && facts.cwd.is_some() && facts.model.is_some() {
            break;
        }
    }
    facts
}

fn text_of(content: &serde_json::Value) -> Option<String> {
    content.as_str().map(str::to_owned).or_else(|| {
        content.as_array().map(|parts| {
            parts
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
    })
}

/// The name in a first prompt such as "You are the worker session me-sim-sand. Read your brief …".
#[must_use]
pub fn worker_name(text: &str) -> Option<String> {
    let start = WORKER_PROMPTS
        .iter()
        .find_map(|prompt| text.find(prompt).map(|at| at + prompt.len()))?;
    let name: String = text
        .get(start..)?
        .chars()
        .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect();
    (!name.is_empty()).then_some(name)
}

fn model_of(id: &str) -> Option<Model> {
    Model::ALL
        .iter()
        .copied()
        .find(|model| id.contains(model.as_str()))
}

/// Turns a found worker into a task to adopt, or says why not.
fn plan(env: &Env<'_>, sources: Sources<'_>, child: &Found) -> Result<AdoptTask, String> {
    let project = env
        .config
        .project(child.name.project())
        .ok_or_else(|| "the project is not configured".to_owned())?;
    let main = env.home.expand(&project.main);
    let slug = child.name.slug().as_str();
    let main_text = slashed(&main);
    let vars = TaskVars {
        slug,
        branch: slug,
        main: &main_text,
        worktree: "",
    };
    let expected = vars
        .expand(&project.worktree_path)
        .map(|path| env.home.expand(&path))
        .map_err(|error| error.to_string())?;
    let in_main = child
        .cwd
        .as_ref()
        .map_or_else(|| !expected.is_dir(), |cwd| same_path(cwd, &main));
    let worktree = if in_main {
        None
    } else {
        let path = child.cwd.clone().unwrap_or(expected);
        let branch = Worktrees::new(headwork_adapters::shell::default_shell(), &main)
            .list()
            .ok()
            .and_then(|list| {
                list.into_iter()
                    .find(|(listed, _)| same_path(listed, &path))
                    .and_then(|(_, branch)| branch)
            })
            .unwrap_or_else(|| slug.to_owned());
        Some((slashed(&path), branch))
    };
    let brief = sources.briefs_dir.join(format!("{}.md", child.name));
    let brief_text = std::fs::read_to_string(&brief).ok();
    let keep = brief_text
        .as_deref()
        .is_some_and(|text| !text.contains(CLOSING_HEADING));
    Ok(AdoptTask {
        project: child.name.project().clone(),
        slug: child.name.slug().clone(),
        kind: if in_main {
            slug.to_owned()
        } else {
            "task".to_owned()
        },
        model: child.model.unwrap_or(env.config.models.default),
        in_main,
        keep,
        brief_path: brief_text.map(|_| slashed(&brief)).unwrap_or_default(),
        session_id: child
            .session_id
            .clone()
            .unwrap_or_else(|| format!("wmux:{}", child.surface)),
        host: env.config.machine.host.as_str().to_owned(),
        host_workspace: child.workspace.as_ref().map(ToString::to_string),
        host_surface: Some(child.surface.to_string()),
        claude_pid: child.pid,
        worktree,
        source: child.source.to_owned(),
    })
}

fn stray_worktrees(env: &Env<'_>, used: &BTreeSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    for project in env.config.projects.values() {
        let main = env.home.expand(&project.main);
        let Ok(list) = Worktrees::new(headwork_adapters::shell::default_shell(), &main).list()
        else {
            continue;
        };
        for (path, _) in list {
            let text = slashed(&path);
            if !same_path(&path, &main) && !used.contains(&text.to_lowercase()) {
                out.push(text);
            }
        }
    }
    out
}
