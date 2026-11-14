//! The hard rules hooks enforce (design section 3.2): the tool guard by role, the `AskUserQuestion` format checks and
//! the `Stop` gate.
//!
//! Every check is pure: the binary gathers the facts (paths, the current branch, remotes, recent tool calls) and asks
//! for a [`Decision`]. Whether a deny blocks or is only logged (shadow mode) is the caller's business.

pub mod ask;
pub mod paths;
pub mod shell;
pub mod stop;

use serde::Serialize;

use crate::{Role, RuleId};
pub use paths::GuardPath;
pub use shell::{SimpleCommand, commands};

/// The global skills that deliver a change; a project with its own delivery command forbids them.
pub const DELIVERY_SKILLS: &[&str] = &["commit", "push", "create-pr", "contribute", "ship"];

/// Build tools the root may not run.
pub const BUILD_TOOLS: &[&str] = &[
    "cargo", "mvnd", "mvn", "npm", "pnpm", "yarn", "gradle", "gradlew", "make", "rustc", "dotnet",
    "go",
];

/// Git subcommands the root may not run.
const ROOT_GIT: &[&str] = &["commit", "push", "merge", "rebase", "cherry-pick", "am"];

/// Branch names that count as a default branch when the project does not say otherwise.
pub const DEFAULT_BRANCHES: &[&str] = &["main", "master"];

/// A rule's verdict against a call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verdict {
    /// The rule.
    pub rule: RuleId,
    /// Why, in words the model sees.
    pub reason: String,
}

/// What a hook decides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "lowercase")]
pub enum Decision {
    /// Nothing against it.
    Allow,
    /// A rule forbids it.
    Deny(Verdict),
}

impl Decision {
    fn deny(rule: RuleId, reason: impl Into<String>) -> Self {
        Self::Deny(Verdict {
            rule,
            reason: reason.into(),
        })
    }

    /// The verdict of a deny.
    #[must_use]
    pub const fn verdict(&self) -> Option<&Verdict> {
        match self {
            Self::Allow => None,
            Self::Deny(verdict) => Some(verdict),
        }
    }
}

/// One tool call, as far as the guard reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCall<'a> {
    /// Edit, Write, `MultiEdit` or `NotebookEdit` on a file.
    Write {
        /// The tool.
        tool: &'a str,
        /// The file, as given.
        path: &'a str,
    },
    /// A Bash command.
    Bash {
        /// The command line.
        command: &'a str,
        /// Started with `run_in_background`.
        background: bool,
    },
    /// A skill.
    Skill {
        /// Its name, perhaps `plugin:name`.
        name: &'a str,
    },
    /// A subagent or a workflow.
    Agent {
        /// `Agent`, `Task` or `Workflow`.
        tool: &'a str,
        /// The model it names, if any.
        model: Option<&'a str>,
    },
    /// Anything the guard does not read.
    Other,
}

/// What the guard knows about the calling session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scope {
    /// The role; `None` for an unmanaged session, which the guard always allows.
    pub role: Option<Role>,
    /// The session's working folder.
    pub cwd: String,
    /// Folders the session may write: a child's worktree or main checkout, the temp folder, the headwork home for the
    /// root, and the configured extras.
    pub writable: Vec<GuardPath>,
    /// Folders a child may read and never write.
    pub read_only: Vec<GuardPath>,
    /// Delivery and worktree scripts the root may not run, such as `scripts/ship.sh`.
    pub scripts: Vec<String>,
    /// Commands only the user runs.
    pub user_only: Vec<String>,
    /// Commands that must run as registered jobs, never in the background.
    pub long_jobs: Vec<String>,
    /// The child's project names its own delivery command.
    pub own_delivery: bool,
    /// Repositories a child may push to, `owner/name`; empty allows any.
    pub allowed_repos: Vec<String>,
    /// The branch checked out in the session's folder.
    pub branch: Option<String>,
    /// The branches that count as default.
    pub default_branches: Vec<String>,
    /// Git remotes of the session's folder: name and `owner/name`.
    pub remotes: Vec<(String, String)>,
    /// The user allowed Fable for this task.
    pub fable_allowed: bool,
}

/// Decides a tool call by the session's role.
#[must_use]
pub fn check_tool(scope: &Scope, call: &ToolCall<'_>) -> Decision {
    let Some(role) = scope.role else {
        return Decision::Allow;
    };
    match *call {
        ToolCall::Write { tool, path } => check_write(scope, role, tool, path),
        ToolCall::Bash {
            command,
            background,
        } => check_bash(scope, role, command, background),
        ToolCall::Skill { name } => check_skill(scope, role, name),
        ToolCall::Agent { tool, model } => check_agent(scope, role, tool, model),
        ToolCall::Other => Decision::Allow,
    }
}

fn check_write(scope: &Scope, role: Role, tool: &str, path: &str) -> Decision {
    let target = GuardPath::resolve(&scope.cwd, path);
    let inside = scope.writable.iter().any(|root| target.is_within(root));
    match role {
        Role::Root if !inside => Decision::deny(
            RuleId::RootEdit,
            format!(
                "the root owns no code: {tool} on {path} is outside the headwork home and the scratchpad; spawn a child \
                 for project work"
            ),
        ),
        Role::Child if scope.read_only.iter().any(|root| target.is_within(root)) => Decision::deny(
            RuleId::ChildReadOnly,
            format!("{path} is under a read-only path of this project; read it, never write it"),
        ),
        Role::Child if !inside => Decision::deny(
            RuleId::ChildEditOutside,
            format!(
                "{tool} on {path} is outside this task's folder ({}); edit only your own worktree",
                scope.cwd
            ),
        ),
        Role::Root | Role::Child => Decision::Allow,
    }
}

fn check_bash(scope: &Scope, role: Role, line: &str, background: bool) -> Decision {
    for command in commands(line) {
        let decision = match role {
            Role::Root => check_root_command(scope, &command),
            Role::Child => check_child_command(scope, &command, background),
        };
        if decision != Decision::Allow {
            return decision;
        }
    }
    Decision::Allow
}

fn user_only(scope: &Scope, command: &SimpleCommand) -> Option<Decision> {
    scope
        .user_only
        .iter()
        .find(|pattern| command.runs(pattern))
        .map(|pattern| {
            Decision::deny(
                RuleId::UserOnly,
                format!("`{pattern}` is the user's alone (publishing, uploads, releases, logins); ask the user to run it"),
            )
        })
}

fn check_root_command(scope: &Scope, command: &SimpleCommand) -> Decision {
    if let Some(decision) = user_only(scope, command) {
        return decision;
    }
    let name = command.program_name();
    if name == "git"
        && let Some((sub, _)) = git_subcommand(command)
        && ROOT_GIT.contains(&sub.as_str())
    {
        return Decision::deny(
            RuleId::RootGit,
            format!("the root owns no code: `git {sub}` belongs in a child session"),
        );
    }
    if name == "gh" {
        let args: Vec<&str> = command.args().iter().map(String::as_str).collect();
        if let ["pr", verb @ ("merge" | "create"), ..] = args.as_slice() {
            return Decision::deny(
                RuleId::RootGit,
                format!("the root owns no code: `gh pr {verb}` belongs in a child session"),
            );
        }
    }
    if BUILD_TOOLS.contains(&name.as_str()) {
        return Decision::deny(
            RuleId::RootBuild,
            format!("the root owns no code: `{name}` builds belong in a child session"),
        );
    }
    if let Some(script) = scope.scripts.iter().find(|script| command.runs(script)) {
        return Decision::deny(
            RuleId::RootDelivery,
            format!("the root owns no code: `{script}` runs in a child session"),
        );
    }
    Decision::Allow
}

fn check_child_command(scope: &Scope, command: &SimpleCommand, background: bool) -> Decision {
    if let Some(decision) = user_only(scope, command) {
        return decision;
    }
    if background
        && let Some(pattern) = scope.long_jobs.iter().find(|pattern| command.runs(pattern))
    {
        return Decision::deny(
            RuleId::ChildLongJob,
            format!(
                "`{pattern}` is a long job: start it with the headwork `job_start` tool (or register a running one with \
                 `job_register`) so a park can manage it"
            ),
        );
    }
    if command.program_name() != "git" {
        return Decision::Allow;
    }
    let Some((sub, args)) = git_subcommand(command) else {
        return Decision::Allow;
    };
    let on_default = scope
        .branch
        .as_deref()
        .is_some_and(|branch| is_default(scope, branch));
    match sub.as_str() {
        "commit" if on_default => Decision::deny(
            RuleId::ChildDefaultBranch,
            "never commit on the default branch: create a kebab-case branch first",
        ),
        "push" => check_push(scope, &args, on_default),
        _ => Decision::Allow,
    }
}

fn check_push(scope: &Scope, args: &[String], on_default: bool) -> Decision {
    let positional: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    let remote = positional.first().copied().unwrap_or("origin");
    let refspecs = positional.get(1..).unwrap_or_default();
    let targets_default = refspecs.iter().any(|spec| {
        let target = spec.rsplit(':').next().unwrap_or(spec);
        let target = target
            .trim_start_matches('+')
            .trim_start_matches("refs/heads/");
        is_default(scope, target) || (target == "HEAD" && on_default)
    });
    if targets_default || (refspecs.is_empty() && on_default) {
        return Decision::deny(
            RuleId::ChildDefaultBranch,
            "never push to the default branch: push your task branch and open a pull request",
        );
    }
    if scope.allowed_repos.is_empty() {
        return Decision::Allow;
    }
    let repo = repo_from_url(remote).or_else(|| {
        scope
            .remotes
            .iter()
            .find(|(name, _)| name == remote)
            .map(|(_, repo)| repo.clone())
    });
    match repo {
        Some(repo)
            if !scope
                .allowed_repos
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(&repo)) =>
        {
            Decision::deny(
                RuleId::ChildPushRepo,
                format!(
                    "{repo} is not one of this project's repositories ({})",
                    scope.allowed_repos.join(", ")
                ),
            )
        }
        Some(_) | None => Decision::Allow,
    }
}

fn is_default(scope: &Scope, branch: &str) -> bool {
    scope.default_branches.iter().any(|name| name == branch)
}

/// The git subcommand and its arguments, past global options such as `-C <dir>` and `-c <key=value>`.
fn git_subcommand(command: &SimpleCommand) -> Option<(String, Vec<String>)> {
    let mut args = command.args().iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" => {
                args.next();
            }
            flag if flag.starts_with('-') => {}
            sub => return Some((sub.to_owned(), args.cloned().collect())),
        }
    }
    None
}

fn check_skill(scope: &Scope, role: Role, name: &str) -> Decision {
    let base = name.rsplit(':').next().unwrap_or(name);
    if role == Role::Child && scope.own_delivery && DELIVERY_SKILLS.contains(&base) {
        return Decision::deny(
            RuleId::ChildGlobalSkill,
            format!(
                "this project delivers with its own command; do not use the global `{base}` skill"
            ),
        );
    }
    Decision::Allow
}

fn check_agent(scope: &Scope, role: Role, tool: &str, model: Option<&str>) -> Decision {
    let fable = model.is_some_and(|model| model.eq_ignore_ascii_case("fable"));
    if role == Role::Child && fable && !scope.fable_allowed {
        return Decision::deny(
            RuleId::ChildFable,
            format!(
                "{tool} with Fable needs the user's approval for this task; use opus, or ask the user first"
            ),
        );
    }
    Decision::Allow
}

/// `owner/name` from a git remote URL: `git@github.com:owner/name.git`, `ssh://git@host/owner/name`, or
/// `https://host/owner/name.git`. `None` for a remote name or anything else.
#[must_use]
pub fn repo_from_url(url: &str) -> Option<String> {
    let path = if let Some((_, rest)) = url.split_once("://") {
        rest.split_once('/')?.1
    } else if let Some((user_host, rest)) = url.split_once(':') {
        if user_host.contains('/') || !user_host.contains('@') {
            return None;
        }
        rest
    } else {
        return None;
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.rsplit('/');
    let name = parts.next().filter(|name| !name.is_empty())?;
    let owner = parts.next().filter(|owner| !owner.is_empty())?;
    Some(format!("{owner}/{name}"))
}
