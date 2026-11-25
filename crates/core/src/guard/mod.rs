//! The hard rules hooks enforce (design section 3.2): the tool guard by role, the `AskUserQuestion` format checks and
//! the `Stop` gate.
//!
//! Every check is pure: the binary gathers the facts (paths, the current branch, remotes, recent tool calls) and asks
//! for a [`Decision`]. Whether a deny blocks or is only logged (shadow mode) is the caller's business.

pub mod ask;
pub mod grants;
pub mod paths;
pub mod shell;
pub mod stop;

use serde::Serialize;

use crate::{ORCHESTRATOR_ADDRESS, Role, RuleId};
use grants::{ForceKind, GrantedGit, PushForm, classify_push, granted_git};
pub use paths::{GuardPath, Places};
pub use shell::{SimpleCommand, commands};

/// The header of the ask by which the user allows one task a user-only command.
pub const ALLOW_HEADER: &str = "Allow";

/// The header of the ask by which a lead asks the user before its task closes (`[leads] ask_before_close`).
pub const CLOSE_HEADER: &str = "Close";

/// The global skills that deliver a change; a project with its own delivery command forbids them.
pub const DELIVERY_SKILLS: &[&str] = &["commit", "push", "create-pr", "contribute", "ship"];

/// Build tools the orchestrator may not run.
pub const BUILD_TOOLS: &[&str] = &[
    "cargo", "mvnd", "mvn", "npm", "pnpm", "yarn", "gradle", "gradlew", "make", "rustc", "dotnet",
    "go",
];

/// Git subcommands the orchestrator may not run.
const ORCHESTRATOR_GIT: &[&str] = &["commit", "push", "merge", "rebase", "cherry-pick", "am"];

/// Subcommands of the headwork CLI that change the machine's state and belong to the orchestrator or the user.
///
/// A worker may run only `close` of its own task and `reap --dry-run` among them, and in a project with `install =
/// true` also `install` and a plain `update`.
pub const ADMIN_COMMANDS: &[&str] = &[
    "spawn",
    "queue",
    "drop",
    "close",
    "park",
    "resume",
    "import",
    "tick",
    "retitle",
    "orchestrator",
    "root",
    "launch",
    "finish",
    "exited",
    "install",
    "update",
    "reap",
];

/// Git subcommands that change a repository, its index or its working tree whatever their arguments.
pub const GIT_WRITES: &[&str] = &[
    "commit",
    "push",
    "pull",
    "reset",
    "checkout",
    "switch",
    "restore",
    "merge",
    "rebase",
    "cherry-pick",
    "revert",
    "am",
    "clean",
    "add",
    "rm",
    "mv",
    "init",
];

/// `git branch` and `git tag` options that delete, rename, copy or force.
const REF_WRITE_FLAGS: &[&str] = &[
    "-d",
    "-D",
    "--delete",
    "-m",
    "-M",
    "--move",
    "-c",
    "-C",
    "--copy",
    "-f",
    "--force",
    "--message",
];

/// `git tag` options that annotate or sign, so make a tag; for `git branch`, `-a` lists every branch instead.
const TAG_WRITE_FLAGS: &[&str] = &["-a", "--annotate", "-s", "--sign"];

/// `git branch` and `git tag` options that make a positional argument a filter of a listing.
const REF_LIST_FLAGS: &[&str] = &[
    "-l",
    "--list",
    "-r",
    "--remotes",
    "--all",
    "-n",
    "-v",
    "-vv",
    "--verbose",
    "--contains",
    "--no-contains",
    "--merged",
    "--no-merged",
    "--points-at",
    "--show-current",
    "--sort",
    "--format",
];

/// `git worktree` actions that add, move or remove worktrees.
const WORKTREE_WRITES: &[&str] = &["add", "remove", "prune", "move", "lock", "unlock", "repair"];

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
        /// Whether the prompt carries an `Opus because:` line.
        reason: bool,
    },
    /// Anything the guard does not read.
    Other,
}

/// What the guard knows about the calling session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent fact about the session"
)]
pub struct Scope {
    /// The role; `None` for an unmanaged session, which the guard always allows.
    pub role: Option<Role>,
    /// The session's working folder.
    pub cwd: String,
    /// Folders the session may write: a worker's worktree or main checkout, the temp folder, the headwork home for
    /// the orchestrator, and the configured extras.
    pub writable: Vec<GuardPath>,
    /// Folders a worker may read and never write.
    pub read_only: Vec<GuardPath>,
    /// Files and folders outside its folder a worker may write, and commit in the repository that holds them: its
    /// project's `extra_writable` for its task kind and the files its brief lists on a `Writes:` line (see [`grants`]).
    pub extra_writable: Vec<GuardPath>,
    /// Delivery and worktree scripts the orchestrator may not run, such as `scripts/ship.sh`.
    pub scripts: Vec<String>,
    /// Commands only the user runs.
    pub user_only: Vec<String>,
    /// Commands that must run as registered jobs, never in the background.
    pub long_jobs: Vec<String>,
    /// The worker's project names its own delivery command.
    pub own_delivery: bool,
    /// Repositories a worker may push to, `owner/name`; empty allows any.
    pub allowed_repos: Vec<String>,
    /// The branch checked out in the session's folder.
    pub branch: Option<String>,
    /// The branch the store records for the task's worktree; `branch` stands in when none is recorded.
    pub task_branch: Option<String>,
    /// The branches that count as default.
    pub default_branches: Vec<String>,
    /// Git remotes of the session's folder: name and `owner/name`.
    pub remotes: Vec<(String, String)>,
    /// The user allowed Fable for this task.
    pub fable_allowed: bool,
    /// A worker's own task name, `<project>-<slug>`, which it may close itself.
    pub task_name: Option<String>,
    /// What `~`, `$HOME` and `/tmp` stand for in paths the session names.
    pub places: Places,
    /// User-only commands the user allowed for this task in an `Allow` ask.
    pub allowed_user_only: Vec<String>,
    /// A worker's project main checkout, which it may fast-forward with `git pull --ff-only` after a merge.
    pub main_checkout: Option<GuardPath>,
    /// A worker may run `headwork install` and a plain `headwork update`: its project is headwork itself.
    pub may_install: bool,
    /// Notes repositories the orchestrator may commit and push in.
    pub orchestrator_repos: Vec<GuardPath>,
    /// Folders inside those repositories that hold project checkouts, where the orchestrator still owns no git.
    pub orchestrator_repo_excludes: Vec<GuardPath>,
    /// The session is a reviewer lead (a part of kind `review`): it reads, and writes nothing but the scratchpad.
    pub reviewer: bool,
}

impl Scope {
    fn in_orchestrator_repo(&self, folder: &GuardPath) -> bool {
        self.orchestrator_repos
            .iter()
            .any(|repo| folder.is_within(repo))
            && !self
                .orchestrator_repo_excludes
                .iter()
                .any(|excluded| folder.is_within(excluded))
    }
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
        ToolCall::Agent {
            tool,
            model,
            reason,
        } => check_agent(scope, role, tool, model, reason),
        ToolCall::Other => Decision::Allow,
    }
}

fn check_write(scope: &Scope, role: Role, tool: &str, path: &str) -> Decision {
    let target = scope.places.resolve(&scope.cwd, path);
    if role == Role::Worker && scope.reviewer {
        let scratch = scope
            .places
            .temp
            .as_deref()
            .is_some_and(|temp| target.is_within(&GuardPath::new(temp)));
        if !scratch {
            return Decision::deny(
                RuleId::ReviewerWrite,
                format!(
                    "a reviewer lead reads and never edits: {tool} on {path} is outside the scratchpad; findings go \
                     back through `review_node_done`, fixes are the implementing lead's"
                ),
            );
        }
        return Decision::Allow;
    }
    let granted = role == Role::Worker
        && scope
            .extra_writable
            .iter()
            .any(|grant| target.is_within(grant));
    let inside = granted || scope.writable.iter().any(|root| target.is_within(root));
    match role {
        Role::Orchestrator if !inside => Decision::deny(
            RuleId::OrchestratorEdit,
            format!(
                "the orchestrator owns no code: {tool} on {path} is outside the headwork home and the scratchpad; \
                 spawn a worker for project work"
            ),
        ),
        Role::Worker if scope.read_only.iter().any(|root| target.is_within(root)) => {
            Decision::deny(
                RuleId::WorkerReadOnly,
                format!(
                    "{path} is under a read-only path of this project; read it, never write it"
                ),
            )
        }
        Role::Worker if !inside => Decision::deny(
            RuleId::WorkerEditOutside,
            format!(
                "{tool} on {path} is outside this task's folder ({}); edit only your own worktree",
                scope.cwd
            ),
        ),
        Role::Orchestrator | Role::Worker => Decision::Allow,
    }
}

/// Checks each simple command of a line in turn. It follows `cd` and `pushd`, so a git command sees the folder it runs
/// in; `None` is a folder the guard cannot tell, such as after `cd -` or `popd`.
fn check_bash(scope: &Scope, role: Role, line: &str, background: bool) -> Decision {
    let mut cwd = Some(scope.cwd.clone());
    for command in commands(line) {
        match command.program_name().as_str() {
            "cd" | "pushd" => {
                cwd = cwd.and_then(|cwd| changed_dir(&scope.places, &cwd, &command));
                continue;
            }
            "popd" => {
                cwd = None;
                continue;
            }
            _ => {}
        }
        let decision = match role {
            Role::Orchestrator => check_orchestrator_command(scope, &command, cwd.as_deref()),
            Role::Worker => check_worker_command(scope, &command, cwd.as_deref(), background),
        };
        if decision != Decision::Allow {
            return decision;
        }
    }
    Decision::Allow
}

/// The folder a `cd` moves to from `cwd`: its first operand, the home folder without one, unknown for `cd -`.
fn changed_dir(places: &Places, cwd: &str, command: &SimpleCommand) -> Option<String> {
    let target = command
        .args()
        .iter()
        .map(String::as_str)
        .find(|arg| *arg == "-" || !arg.starts_with('-'))
        .unwrap_or("~");
    (target != "-").then(|| places.resolve(cwd, target).as_str().to_owned())
}

fn user_only(scope: &Scope, command: &SimpleCommand) -> Option<Decision> {
    scope
        .user_only
        .iter()
        .filter(|pattern| !scope.allowed_user_only.contains(pattern))
        .find(|pattern| command.runs(pattern))
        .map(|pattern| {
            let ask = if scope.task_name.is_some() {
                format!(
                    "ask the user to run it, or to allow it for this task with an `AskUserQuestion` headed `{ALLOW_HEADER}` \
                     that names `{pattern}` and offers an option starting with `Allow`"
                )
            } else {
                "ask the user to run it".to_owned()
            };
            Decision::deny(
                RuleId::UserOnly,
                format!("`{pattern}` is the user's alone (publishing, uploads, releases, logins); {ask}"),
            )
        })
}

fn check_orchestrator_command(
    scope: &Scope,
    command: &SimpleCommand,
    cwd: Option<&str>,
) -> Decision {
    if let Some(decision) = user_only(scope, command) {
        return decision;
    }
    let name = command.program_name();
    if name == "git"
        && let Some((sub, _)) = git_subcommand(command)
        && ORCHESTRATOR_GIT.contains(&sub.as_str())
        && !git_folder(scope, command, cwd)
            .is_some_and(|folder| scope.in_orchestrator_repo(&folder))
    {
        return Decision::deny(
            RuleId::OrchestratorGit,
            format!("the orchestrator owns no code: `git {sub}` belongs in a worker session"),
        );
    }
    if name == "gh" {
        let args: Vec<&str> = command.args().iter().map(String::as_str).collect();
        if let ["pr", verb @ ("merge" | "create"), ..] = args.as_slice() {
            return Decision::deny(
                RuleId::OrchestratorGit,
                format!(
                    "the orchestrator owns no code: `gh pr {verb}` belongs in a worker session"
                ),
            );
        }
    }
    if BUILD_TOOLS.contains(&name.as_str()) {
        return Decision::deny(
            RuleId::OrchestratorBuild,
            format!("the orchestrator owns no code: `{name}` builds belong in a worker session"),
        );
    }
    if let Some(script) = scope.scripts.iter().find(|script| command.runs(script)) {
        return Decision::deny(
            RuleId::OrchestratorDelivery,
            format!("the orchestrator owns no code: `{script}` runs in a worker session"),
        );
    }
    Decision::Allow
}

fn check_worker_command(
    scope: &Scope,
    command: &SimpleCommand,
    cwd: Option<&str>,
    background: bool,
) -> Decision {
    if let Some(decision) = user_only(scope, command) {
        return decision;
    }
    if let Some(decision) = check_admin(scope, command) {
        return decision;
    }
    if scope.reviewer
        && let Some(decision) = check_reviewer_command(command)
    {
        return decision;
    }
    if background
        && let Some(pattern) = scope.long_jobs.iter().find(|pattern| command.runs(pattern))
    {
        return Decision::deny(
            RuleId::WorkerLongJob,
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
    match check_git_outside(scope, command, cwd, &sub, &args) {
        GitPlace::Own => {}
        GitPlace::Granted => return Decision::Allow,
        GitPlace::Refused(decision) => return decision,
    }
    let on_default = scope
        .branch
        .as_deref()
        .is_some_and(|branch| is_default(scope, branch));
    match sub.as_str() {
        "commit" if on_default => Decision::deny(
            RuleId::WorkerDefaultBranch,
            "never commit on the default branch: create a kebab-case branch first",
        ),
        "push" => check_push(scope, &args, on_default),
        _ => Decision::Allow,
    }
}

/// A reviewer lead's command that changes something: a git write, a pull request, a build.
fn check_reviewer_command(command: &SimpleCommand) -> Option<Decision> {
    let name = command.program_name();
    let what = if name == "git" {
        let (sub, args) = git_subcommand(command)?;
        is_git_write(&sub, &args).then(|| format!("`git {sub}`"))
    } else if name == "gh" {
        let args: Vec<&str> = command.args().iter().map(String::as_str).collect();
        matches!(
            args.as_slice(),
            [
                "pr",
                "create" | "merge" | "edit" | "close" | "comment" | "review",
                ..
            ]
        )
        .then(|| format!("`gh {}`", args.join(" ")))
    } else {
        BUILD_TOOLS
            .contains(&name.as_str())
            .then(|| format!("`{name}` builds"))
    }?;
    Some(Decision::deny(
        RuleId::ReviewerWrite,
        format!(
            "a reviewer lead reads and never changes the repository: {what} is the implementing lead's; your subagents \
             read the review worktree and you hand their findings back with `review_node_done`"
        ),
    ))
}

/// A worker running an orchestrator command of the headwork CLI, other than closing its own task.
fn check_admin(scope: &Scope, command: &SimpleCommand) -> Option<Decision> {
    let args = headwork_args(command)?;
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-h" | "--help" | "-V" | "--version"))
    {
        return None;
    }
    let mut words = args
        .iter()
        .map(String::as_str)
        .filter(|arg| !arg.starts_with('-'));
    let sub = words.next()?;
    if !ADMIN_COMMANDS.contains(&sub) {
        return None;
    }
    if sub == "close"
        && let (Some(own), Some(name)) = (scope.task_name.as_deref(), words.next())
        && (name == own || name.replacen('/', "-", 1) == own)
    {
        return None;
    }
    if sub == "install" && scope.may_install {
        return None;
    }
    if sub == "update" && scope.may_install && plain_update(args) {
        return None;
    }
    if sub == "reap" && args.iter().any(|arg| arg == "--dry-run") {
        return None;
    }
    Some(Decision::deny(
        RuleId::WorkerAdmin,
        format!(
            "`headwork {sub}` is the orchestrator's: ask the orchestrator (session `{ORCHESTRATOR_ADDRESS}`) to do it; \
             a worker only closes itself, with `headwork close <its own name>`"
        ),
    ))
}

/// Whether `headwork update` runs with only `--dry-run`, `--json` and `--wait <minutes>`: what a worker of a project
/// that installs may run. `--to` (a chosen release, a rollback), `--dir` and `--after` stay the orchestrator's and the
/// user's.
fn plain_update(args: &[String]) -> bool {
    let Some(at) = args.iter().position(|arg| arg == "update") else {
        return false;
    };
    let mut rest = args.iter().skip(at + 1).map(String::as_str);
    while let Some(arg) = rest.next() {
        let minutes = match arg {
            "--dry-run" | "--json" => continue,
            "--wait" => rest.next(),
            other => other.strip_prefix("--wait="),
        };
        if !minutes.is_some_and(|text| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        {
            return false;
        }
    }
    true
}

/// The arguments a command passes to the headwork CLI: those of `headwork` under any path or extension, or those
/// after `--` of `cargo run -p headwork`. `None` for anything else.
fn headwork_args(command: &SimpleCommand) -> Option<&[String]> {
    match command.program_name().as_str() {
        "headwork" => Some(command.args()),
        "cargo" => {
            let args = command.args();
            let run = args.iter().find(|arg| !arg.starts_with('+'))?;
            let split = args.iter().position(|arg| arg == "--")?;
            let (before, after) = args.split_at(split);
            let named = before.windows(2).any(|pair| {
                matches!(pair, [flag, name]
                    if matches!(flag.as_str(), "-p" | "--package" | "--bin") && name == "headwork")
            }) || before.iter().any(|arg| {
                matches!(
                    arg.as_str(),
                    "-pheadwork" | "--package=headwork" | "--bin=headwork"
                )
            });
            (run == "run" && named).then(|| after.get(1..).unwrap_or_default())
        }
        _ => None,
    }
}

/// A worker's git write against a folder outside its writable folders ([`git_folder`]; an unknown folder is allowed).
/// A fast-forward of the project's main checkout (`git pull --ff-only`) is allowed, as the ship flow does after a
/// merge, and so is a write the worker's extra writable paths allow ([`grants`]).
fn check_git_outside(
    scope: &Scope,
    command: &SimpleCommand,
    cwd: Option<&str>,
    sub: &str,
    args: &[String],
) -> GitPlace {
    if !is_git_write(sub, args) {
        return GitPlace::Own;
    }
    let Some(folder) = git_folder(scope, command, cwd) else {
        return GitPlace::Own;
    };
    if scope.writable.iter().any(|root| folder.is_within(root)) {
        return GitPlace::Own;
    }
    let fast_forward = sub == "pull" && args.iter().any(|arg| arg == "--ff-only");
    if fast_forward
        && scope
            .main_checkout
            .as_ref()
            .is_some_and(|main| folder.is_within(main))
    {
        return GitPlace::Own;
    }
    let reason = match granted_git(&scope.extra_writable, &scope.places, &folder, sub, args) {
        GrantedGit::Allowed => return GitPlace::Granted,
        GrantedGit::Beyond(why) => format!(
            "`git {sub}` in {folder} reaches beyond the paths this task may write there: {why}; ask the orchestrator \
             (session `{ORCHESTRATOR_ADDRESS}`) for anything else"
        ),
        GrantedGit::Unrelated => format!(
            "`git {sub}` would change {folder}, outside this task's folder ({}); change only your own worktree, or \
             ask the orchestrator (session `{ORCHESTRATOR_ADDRESS}`)",
            scope.cwd
        ),
    };
    GitPlace::Refused(Decision::deny(RuleId::WorkerGitOutside, reason))
}

/// Where a worker's git command acts.
#[derive(Debug)]
enum GitPlace {
    /// A read, a folder the guard cannot tell, the worker's own folders, or a fast-forward of its main checkout: the
    /// other git rules decide.
    Own,
    /// A repository holding paths the worker was granted outside its folder, within those grants: allowed whatever the
    /// branch, since such a notes repository is committed on its default branch by design.
    Granted,
    /// Outside its folders.
    Refused(Decision),
}

/// The folder a git command acts on: its working folder with each `-C` applied in turn, as git stacks them. `None`
/// when it cannot be told: an unknown working folder, or `--git-dir` and `--work-tree`.
fn git_folder(scope: &Scope, command: &SimpleCommand, cwd: Option<&str>) -> Option<GuardPath> {
    let mut folder = scope.places.resolve(&scope.cwd, cwd?);
    let mut words = command.args().iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "-C" => folder = scope.places.resolve(folder.as_str(), words.next()?),
            "-c" | "--namespace" => {
                words.next();
            }
            flag if flag.starts_with("--git-dir") || flag.starts_with("--work-tree") => {
                return None;
            }
            flag if flag.starts_with('-') => {}
            _ => break,
        }
    }
    Some(folder)
}

/// Whether a git subcommand with these arguments changes a repository; listings and reads do not.
fn is_git_write(sub: &str, args: &[String]) -> bool {
    let has = |flags: &[&str]| args.iter().any(|arg| flags.contains(&arg.as_str()));
    let first = args.first().map(String::as_str);
    match sub {
        "branch" | "tag" => {
            let listing = has(REF_LIST_FLAGS) || (sub == "branch" && has(&["-a"]));
            has(REF_WRITE_FLAGS)
                || (sub == "tag" && has(TAG_WRITE_FLAGS))
                || (args.iter().any(|arg| !arg.starts_with('-')) && !listing)
        }
        "worktree" => first.is_some_and(|action| WORKTREE_WRITES.contains(&action)),
        "stash" => !matches!(first, Some("list" | "show")),
        sub => GIT_WRITES.contains(&sub),
    }
}

fn check_push(scope: &Scope, args: &[String], on_default: bool) -> Decision {
    let form = classify_push(args);
    let remote = form.positional.first().copied().unwrap_or("origin");
    let refspecs = form.positional.get(1..).unwrap_or_default();
    let targets_default = refspecs.iter().any(|spec| {
        let target = spec.rsplit(':').next().unwrap_or(spec);
        let target = target
            .trim_start_matches('+')
            .trim_start_matches("refs/heads/");
        is_default(scope, target) || (target == "HEAD" && on_default)
    });
    if targets_default || (refspecs.is_empty() && on_default) {
        return Decision::deny(
            RuleId::WorkerDefaultBranch,
            "never push to the default branch: push your task branch and open a pull request",
        );
    }
    if let Some(denied) = check_forced_push(scope, &form) {
        return denied;
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
                RuleId::WorkerPushRepo,
                format!(
                    "{repo} is not one of this project's repositories ({})",
                    scope.allowed_repos.join(", ")
                ),
            )
        }
        Some(_) | None => Decision::Allow,
    }
}

/// A push that rewrites a remote branch: a plain force or a `+refspec` is refused; `--force-with-lease` and
/// `--force-if-includes` are allowed only to the task's own branch, never with `--all`, `--mirror`, `--tags` or `--prune`.
fn check_forced_push(scope: &Scope, form: &PushForm<'_>) -> Option<Decision> {
    match form.force {
        ForceKind::None => return None,
        ForceKind::Plain => {
            return Some(Decision::deny(
                RuleId::WorkerForcePush,
                "a plain force push is refused: after a rebase use `git push --force-with-lease` to your own task branch",
            ));
        }
        ForceKind::Lease => {}
    }
    let own = scope.task_branch.as_deref().or(scope.branch.as_deref());
    let checked_out_is_own = own.is_some() && scope.branch.as_deref() == own;
    let refspecs = form.positional.get(1..).unwrap_or_default();
    let only_own = !form.wide
        && if refspecs.is_empty() {
            checked_out_is_own
        } else {
            refspecs.iter().all(|spec| {
                let target = spec.rsplit(':').next().unwrap_or(spec);
                let target = target.trim_start_matches("refs/heads/");
                (target == "HEAD" && checked_out_is_own)
                    || (target != "HEAD" && Some(target) == own)
            })
        };
    if only_own {
        None
    } else {
        Some(Decision::deny(
            RuleId::WorkerForcePush,
            "`--force-with-lease` is for your own task branch only, one branch at a time",
        ))
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
    if role == Role::Worker && scope.own_delivery && DELIVERY_SKILLS.contains(&base) {
        return Decision::deny(
            RuleId::WorkerGlobalSkill,
            format!(
                "this project delivers with its own command; do not use the global `{base}` skill"
            ),
        );
    }
    Decision::Allow
}

fn check_agent(
    scope: &Scope,
    role: Role,
    tool: &str,
    model: Option<&str>,
    reason: bool,
) -> Decision {
    let fable = model.is_some_and(|model| model.eq_ignore_ascii_case("fable"));
    if role == Role::Worker && fable && !scope.fable_allowed {
        return Decision::deny(
            RuleId::WorkerFable,
            format!(
                "{tool} with Fable needs the user's approval for this task; use opus, or ask the user first"
            ),
        );
    }
    let opus = model.is_some_and(|model| {
        let model = model.to_ascii_lowercase();
        model == "opus" || model.starts_with("claude-opus")
    });
    if role == Role::Worker && opus && !reason && !scope.reviewer && tool != "Workflow" {
        return Decision::deny(
            RuleId::SubagentOpus,
            format!(
                "{tool} with Opus needs a reason; use sonnet (the default: omit `model`), or add a line \
                 `Opus because: <why>` to the prompt for architecture, security, concurrency, audits or plans"
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
