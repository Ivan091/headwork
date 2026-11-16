//! The configuration model (design section 5): the machine's `config.toml` with its projects, and the project-local
//! `.headwork.toml` that may add to a project but never widen it.

use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::lifecycle::MergePolicy;
use crate::names::ProjectName;
use crate::vocabulary::Model;

/// Placeholders a command template may use.
pub const PLACEHOLDERS: &[&str] = &["slug", "branch", "main", "worktree", "out", "temp"];

/// Keys a project-local file may set.
pub const OVERLAY_KEYS: &[&str] = &["long_jobs", "user_only"];

/// Why a configuration is not valid.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The TOML does not parse or does not match the model.
    #[error("{origin}: {source}")]
    Parse {
        /// Which file or text.
        origin: String,
        /// The parser's error.
        #[source]
        source: Box<toml::de::Error>,
    },
    /// The model's default is not in its allowed set.
    #[error("models.default `{0}` is not in models.allowed")]
    DefaultModelNotAllowed(Model),
    /// An allowed model set is empty.
    #[error("{0}: the allowed model set is empty")]
    NoModels(String),
    /// A project allows a model the machine does not.
    #[error("project `{project}` allows `{model}`, which models.allowed does not")]
    ProjectWidensModels {
        /// The project.
        project: ProjectName,
        /// The model.
        model: Model,
    },
    /// The stop threshold is not below the warning threshold.
    #[error("watch.disk_stop_gb ({stop}) must be below watch.disk_warn_gb ({warn})")]
    DiskThresholds {
        /// `disk_stop_gb`.
        stop: u32,
        /// `disk_warn_gb`.
        warn: u32,
    },
    /// A template names an unknown placeholder or leaves a brace open.
    #[error("project `{project}`, {field}: {problem}")]
    Template {
        /// The project.
        project: ProjectName,
        /// The field holding the template.
        field: String,
        /// What is wrong.
        problem: TemplateProblem,
    },
    /// A repository is not `owner/name`.
    #[error("project `{project}`: `{repo}` in allowed_repos is not `owner/name`")]
    BadRepo {
        /// The project.
        project: ProjectName,
        /// The entry.
        repo: String,
    },
    /// A project-local file sets a key it may not.
    #[error("{origin}: `{key}` may not be set in a project-local file; only {allowed} may", allowed = OVERLAY_KEYS.join(", "))]
    OverlayKey {
        /// Which file.
        origin: String,
        /// The key.
        key: String,
    },
}

/// What is wrong with a template.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateProblem {
    /// `{name}` is not a known placeholder.
    #[error("unknown placeholder `{{{0}}}`")]
    Unknown(String),
    /// A `{` without its `}`.
    #[error("unclosed `{{`")]
    Unclosed,
}

/// The terminal multiplexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostKind {
    /// wmux on Windows.
    Wmux,
    /// cmux on macOS.
    Cmux,
}

impl HostKind {
    /// The config spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wmux => "wmux",
            Self::Cmux => "cmux",
        }
    }
}

/// Who pushes notifications to the phone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PushMode {
    /// The agent's own PushNotification, required before an ask.
    Claude,
    /// No push.
    None,
}

/// What resume does after a crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CrashResume {
    /// The orchestrator asks the user.
    #[default]
    Ask,
    /// Resume at logon.
    Always,
    /// Never resume by itself.
    Never,
}

/// What a job does when the machine parks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParkPolicy {
    /// Wait for a fresh checkpoint, then stop.
    Checkpoint,
    /// Stop now, run again after resume.
    Rerun,
    /// Let it finish.
    Finish,
}

impl ParkPolicy {
    /// The stored and configured spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Checkpoint => "checkpoint",
            Self::Rerun => "rerun",
            Self::Finish => "finish",
        }
    }
}

impl fmt::Display for ParkPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

impl std::str::FromStr for ParkPolicy {
    type Err = crate::vocabulary::UnknownWord;

    fn from_str(word: &str) -> Result<Self, Self::Err> {
        match word {
            "checkpoint" => Ok(Self::Checkpoint),
            "rerun" => Ok(Self::Rerun),
            "finish" => Ok(Self::Finish),
            other => Err(crate::vocabulary::UnknownWord {
                kind: "park policy",
                word: other.to_owned(),
            }),
        }
    }
}

/// A worktree command: the built-in git worktree handling or a project script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum WorktreeCommand {
    /// `git worktree add -b {slug}` and `git worktree remove`.
    Builtin,
    /// A command template run in the main checkout.
    Command(String),
}

impl From<String> for WorktreeCommand {
    fn from(text: String) -> Self {
        if text == "builtin" {
            Self::Builtin
        } else {
            Self::Command(text)
        }
    }
}

impl From<WorktreeCommand> for String {
    fn from(command: WorktreeCommand) -> Self {
        match command {
            WorktreeCommand::Builtin => "builtin".to_owned(),
            WorktreeCommand::Command(text) => text,
        }
    }
}

/// How a project delivers a change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Delivery {
    /// The global skills' flow: branch, Conventional Commit, push, pull request.
    Pr,
    /// The project's own delivery command.
    Command(String),
}

impl From<String> for Delivery {
    fn from(text: String) -> Self {
        if text == "pr" {
            Self::Pr
        } else {
            Self::Command(text)
        }
    }
}

impl From<Delivery> for String {
    fn from(delivery: Delivery) -> Self {
        match delivery {
            Delivery::Pr => "pr".to_owned(),
            Delivery::Command(text) => text,
        }
    }
}

/// A GitHub-style repository, `owner/name`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Repo(String);

impl Repo {
    /// The repository as `owner/name`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn is_valid(&self) -> bool {
        self.0.split_once('/').is_some_and(|(owner, name)| {
            !owner.is_empty() && !name.is_empty() && !name.contains('/')
        })
    }
}

impl fmt::Display for Repo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The whole machine configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Machine-wide settings.
    pub machine: MachineConfig,
    /// Watchdog thresholds.
    #[serde(default)]
    pub watch: WatchConfig,
    /// Models.
    #[serde(default)]
    pub models: ModelsConfig,
    /// The Claude Code hooks.
    #[serde(default)]
    pub hooks: HooksConfig,
    /// Where new workers appear in their project's workspace.
    #[serde(default)]
    pub layout: LayoutConfig,
    /// Projects by name.
    #[serde(default, rename = "project")]
    pub projects: BTreeMap<ProjectName, ProjectConfig>,
}

/// `[machine]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineConfig {
    /// The terminal host.
    pub host: HostKind,
    /// At most this many worker sessions at once. `max_children` is its deprecated spelling.
    #[serde(default = "default_max_workers", alias = "max_children")]
    pub max_workers: NonZeroU32,
    /// Sessions start with Remote Control.
    #[serde(default = "yes")]
    pub remote_control: bool,
    /// Who pushes notifications.
    #[serde(default = "default_push")]
    pub push: PushMode,
    /// An ntfy topic URL, empty for none.
    #[serde(default)]
    pub notify_ntfy: String,
    /// Where the soft rules live; `~/.headwork/rules` when absent.
    #[serde(default)]
    pub rules_dir: Option<String>,
    /// What resume does after a crash.
    #[serde(default)]
    pub resume_after_crash: CrashResume,
    /// The shell that runs project command templates, such as a worktree script. When absent it is Git Bash on
    /// Windows (beside the `git` on `PATH`, in Git's install folders, or on `PATH`, never WSL's `System32/bash.exe`)
    /// and `bash` elsewhere.
    #[serde(default)]
    pub shell: Option<String>,
    /// The terminal host's command line program; found on `PATH` or in its default install folder when absent.
    #[serde(default)]
    pub host_bin: Option<String>,
}

/// `[watch]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WatchConfig {
    /// Free space that raises a warning.
    pub disk_warn_gb: u32,
    /// Free space that pauses the queue.
    pub disk_stop_gb: u32,
    /// Growth per minute that counts as runaway output.
    pub growth_limit_gb_per_min: u32,
    /// A single file this large counts as runaway output.
    pub file_limit_gb: u32,
    /// Minutes without a hook event before a session counts as stuck.
    pub stuck_after_min: u32,
    /// Seconds a spawning task may take to report `SessionStart` before it counts as failed.
    pub spawn_timeout_s: u32,
    /// Minutes an ask may wait on the user before the next reminder.
    pub ask_remind_min: u32,
    /// Minutes a merged task lingers before it starts closing.
    pub merged_grace_min: u32,
    /// Seconds a closing task may keep processes alive before the tick ends them.
    pub closing_grace_s: u32,
    /// Folders the runaway check watches; `{temp}` stands for the temp folder, and folders of live tasks are always watched.
    pub roots: Vec<String>,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            disk_warn_gb: 15,
            disk_stop_gb: 5,
            growth_limit_gb_per_min: 1,
            file_limit_gb: 20,
            stuck_after_min: 30,
            spawn_timeout_s: 120,
            ask_remind_min: 120,
            merged_grace_min: 10,
            closing_grace_s: 60,
            roots: Vec::new(),
        }
    }
}

/// `[models]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ModelsConfig {
    /// The model when a spawn names none.
    pub default: Model,
    /// The models a session may run.
    pub allowed: Vec<Model>,
    /// Fable needs the user's approval per task.
    pub fable_needs_user: bool,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            default: Model::Sonnet,
            allowed: Model::ALL.to_vec(),
            fable_needs_user: true,
        }
    }
}

/// `[hooks]`: how the Claude Code hooks act.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HooksConfig {
    /// Denies block the call. Off is shadow mode: every hook decides and logs what it would deny, and allows.
    pub enforce: bool,
    /// The plugin folder the launcher passes to `claude --plugin-dir`; none starts sessions without the plugin, and
    /// the launcher then reports the session as started itself.
    pub plugin_dir: Option<String>,
    /// Folders every managed session may write besides its own (Claude's memory and plans).
    pub writable: Vec<String>,
    /// Folders the orchestrator may also write, such as the orchestrate skill's brief cache. `root_writable` is its
    /// deprecated spelling.
    #[serde(alias = "root_writable")]
    pub orchestrator_writable: Vec<String>,
    /// A push notification counts for an ask within this many seconds.
    pub push_window_secs: u32,
    /// A sent file counts for a review ask within this many seconds.
    pub file_window_secs: u32,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            enforce: false,
            plugin_dir: None,
            writable: vec![
                "~/.claude/projects".to_owned(),
                "~/.claude/plans".to_owned(),
            ],
            orchestrator_writable: vec!["~/.claude/cache/orchestrate".to_owned()],
            push_window_secs: 120,
            file_window_secs: 300,
        }
    }
}

/// How a project's workspace shows its workers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayoutMode {
    /// Each worker gets a pane of its own, up to `layout.panes`; later ones stack as tabs.
    #[default]
    Panes,
    /// Every worker is a tab, as the host places it.
    Tabs,
}

/// `[layout]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LayoutConfig {
    /// Panes or tabs.
    pub mode: LayoutMode,
    /// The most panes headwork splits a workspace into before workers stack as tabs.
    pub panes: NonZeroU32,
    /// The title of a worker's pane or tab. `{name}` is `<project>-<slug>`, `{model}` the model, `{effort}` the effort
    /// word, or `?` when it is unknown.
    pub title: String,
}

/// The default `[layout] title`.
pub const DEFAULT_TITLE: &str = "{name}";

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            mode: LayoutMode::Panes,
            panes: NonZeroU32::new(4).unwrap_or(NonZeroU32::MIN),
            title: DEFAULT_TITLE.to_owned(),
        }
    }
}

impl LayoutConfig {
    /// The title of a worker's pane or tab: the template with `{name}`, `{model}` and `{effort}` filled in.
    #[must_use]
    #[expect(
        clippy::literal_string_with_formatting_args,
        reason = "the placeholders are the config template's own, not format arguments"
    )]
    pub fn title_for(&self, name: &str, model: &str, effort: Option<&str>) -> String {
        self.title
            .replace("{name}", name)
            .replace("{model}", model)
            .replace("{effort}", effort.unwrap_or("?"))
    }
}

/// `[project.<name>]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    /// The main checkout.
    pub main: String,
    /// A disabled project takes no new tasks.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Creates a task's worktree.
    pub worktree_new: WorktreeCommand,
    /// Removes it.
    pub worktree_remove: WorktreeCommand,
    /// Where it lives.
    pub worktree_path: String,
    /// How a change is delivered.
    pub deliver: Delivery,
    /// Who merges.
    pub merge: MergePolicy,
    /// At most this many live tasks of the project. `max_children` is its deprecated spelling.
    #[serde(default, alias = "max_children")]
    pub max_workers: Option<NonZeroU32>,
    /// Task kinds that run in the main checkout, one at a time.
    #[serde(default)]
    pub main_only: Vec<String>,
    /// Commands only the user runs.
    #[serde(default)]
    pub user_only: Vec<String>,
    /// Repositories a worker may push to.
    #[serde(default)]
    pub allowed_repos: Vec<Repo>,
    /// Heavy job slots.
    #[serde(default)]
    pub heavy_slots: Option<u32>,
    /// The project's own heavy slot owner.
    #[serde(default)]
    pub heavy_gate: Option<String>,
    /// Paths a worker may read and never write.
    #[serde(default)]
    pub read_only_paths: Vec<String>,
    /// Review requirements.
    #[serde(default)]
    pub review: ReviewConfig,
    /// Commands that must run as registered jobs.
    #[serde(default)]
    pub long_jobs: Vec<String>,
    /// Park handling per job kind.
    #[serde(default)]
    pub jobs: BTreeMap<String, JobConfig>,
    /// Overrides `machine.remote_control`.
    #[serde(default)]
    pub remote_control: Option<bool>,
    /// Overrides `machine.push`.
    #[serde(default)]
    pub push: Option<PushMode>,
    /// Narrows the models.
    #[serde(default)]
    pub models: Option<ProjectModels>,
}

/// `review` of a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ReviewConfig {
    /// A review ask needs a SendUserFile first.
    pub send_file: bool,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self { send_file: true }
    }
}

/// `jobs.<kind>` of a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobConfig {
    /// What a park does with it.
    pub park_policy: ParkPolicy,
    /// The checkpoint file for `checkpoint`.
    #[serde(default)]
    pub checkpoint: Option<String>,
}

/// `models` of a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectModels {
    /// The models the project's sessions may run.
    pub allowed: Vec<Model>,
}

/// A project-local `.headwork.toml`: it may add commands, never widen what the machine config allows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectOverlay {
    /// More commands that must run as registered jobs.
    #[serde(default)]
    pub long_jobs: Vec<String>,
    /// More commands only the user runs.
    #[serde(default)]
    pub user_only: Vec<String>,
}

const fn default_max_workers() -> NonZeroU32 {
    match NonZeroU32::new(10) {
        Some(ten) => ten,
        None => NonZeroU32::MIN,
    }
}

const fn yes() -> bool {
    true
}

const fn default_push() -> PushMode {
    PushMode::Claude
}

impl Config {
    /// Parses and validates a machine configuration.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] when the text does not parse, does not match the model, or breaks a rule:
    /// the default model outside the allowed set, a project allowing a model the machine does not, disk thresholds
    /// out of order, an unknown template placeholder, or a malformed repository.
    pub fn parse(text: &str, origin: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(text).map_err(|source| ConfigError::Parse {
            origin: origin.to_owned(),
            source: Box::new(source),
        })?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.models.allowed.is_empty() {
            return Err(ConfigError::NoModels("models.allowed".to_owned()));
        }
        if !self.models.allowed.contains(&self.models.default) {
            return Err(ConfigError::DefaultModelNotAllowed(self.models.default));
        }
        if self.watch.disk_stop_gb >= self.watch.disk_warn_gb {
            return Err(ConfigError::DiskThresholds {
                stop: self.watch.disk_stop_gb,
                warn: self.watch.disk_warn_gb,
            });
        }
        self.projects
            .iter()
            .try_for_each(|(name, project)| project.validate(name, &self.models))
    }

    /// The deprecated keys a machine configuration still uses, each with the key that replaces it, such as
    /// `machine.max_children` and `machine.max_workers`. They load as their new spelling; text that does not parse has
    /// none.
    #[must_use]
    pub fn deprecated_keys(text: &str) -> Vec<(String, String)> {
        const RENAMED: &[(&str, &str, &str)] = &[
            ("machine", "max_children", "max_workers"),
            ("hooks", "root_writable", "orchestrator_writable"),
        ];
        let Ok(table) = toml::from_str::<toml::Table>(text) else {
            return Vec::new();
        };
        let mut found: Vec<(String, String)> = RENAMED
            .iter()
            .filter(|(section, old, _)| {
                table
                    .get(*section)
                    .and_then(toml::Value::as_table)
                    .is_some_and(|keys| keys.contains_key(*old))
            })
            .map(|(section, old, new)| (format!("{section}.{old}"), format!("{section}.{new}")))
            .collect();
        if let Some(projects) = table.get("project").and_then(toml::Value::as_table) {
            found.extend(
                projects
                    .iter()
                    .filter(|(_, project)| {
                        project
                            .as_table()
                            .is_some_and(|keys| keys.contains_key("max_children"))
                    })
                    .map(|(name, _)| {
                        (
                            format!("project.{name}.max_children"),
                            format!("project.{name}.max_workers"),
                        )
                    }),
            );
        }
        found
    }

    /// The project with this name.
    #[must_use]
    pub fn project(&self, name: &ProjectName) -> Option<&ProjectConfig> {
        self.projects.get(name)
    }

    /// The models a project's sessions may run: the project's own set when it has one, else the machine's.
    #[must_use]
    pub fn allowed_models(&self, project: &ProjectConfig) -> Vec<Model> {
        project.models.as_ref().map_or_else(
            || self.models.allowed.clone(),
            |models| models.allowed.clone(),
        )
    }
}

impl ProjectConfig {
    fn validate(&self, name: &ProjectName, models: &ModelsConfig) -> Result<(), ConfigError> {
        if let Some(own) = &self.models {
            if own.allowed.is_empty() {
                return Err(ConfigError::NoModels(format!("project `{name}`")));
            }
            if let Some(model) = own
                .allowed
                .iter()
                .find(|model| !models.allowed.contains(model))
            {
                return Err(ConfigError::ProjectWidensModels {
                    project: name.clone(),
                    model: *model,
                });
            }
        }
        if let Some(repo) = self.allowed_repos.iter().find(|repo| !repo.is_valid()) {
            return Err(ConfigError::BadRepo {
                project: name.clone(),
                repo: repo.0.clone(),
            });
        }
        self.templates().try_for_each(|(field, template)| {
            check_template(template).map_err(|problem| ConfigError::Template {
                project: name.clone(),
                field,
                problem,
            })
        })
    }

    fn templates(&self) -> impl Iterator<Item = (String, &str)> {
        fn command<'a>(field: &str, command: &'a WorktreeCommand) -> Option<(String, &'a str)> {
            match command {
                WorktreeCommand::Builtin => None,
                WorktreeCommand::Command(text) => Some((field.to_owned(), text.as_str())),
            }
        }
        let deliver = match &self.deliver {
            Delivery::Pr => None,
            Delivery::Command(text) => Some(("deliver".to_owned(), text.as_str())),
        };
        let checkpoints = self.jobs.iter().filter_map(|(kind, job)| {
            job.checkpoint
                .as_deref()
                .map(|text| (format!("jobs.{kind}.checkpoint"), text))
        });
        [
            command("worktree_new", &self.worktree_new),
            command("worktree_remove", &self.worktree_remove),
            Some(("worktree_path".to_owned(), self.worktree_path.as_str())),
            deliver,
            self.heavy_gate
                .as_deref()
                .map(|text| ("heavy_gate".to_owned(), text)),
        ]
        .into_iter()
        .flatten()
        .chain(checkpoints)
    }

    /// Whether a task kind must run in the main checkout.
    #[must_use]
    pub fn is_main_only(&self, kind: &str) -> bool {
        self.main_only.iter().any(|main_only| main_only == kind)
    }

    /// Adds a project-local overlay's commands.
    #[must_use]
    pub fn with_overlay(mut self, overlay: ProjectOverlay) -> Self {
        self.long_jobs.extend(overlay.long_jobs);
        self.user_only.extend(overlay.user_only);
        self
    }
}

impl ProjectOverlay {
    /// Parses a project-local `.headwork.toml`.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::OverlayKey`] for any key other than [`OVERLAY_KEYS`], such as `allowed_repos` or
    /// `models`, and [`ConfigError::Parse`] when the text does not parse.
    pub fn parse(text: &str, origin: &str) -> Result<Self, ConfigError> {
        let parse_error = |source| ConfigError::Parse {
            origin: origin.to_owned(),
            source: Box::new(source),
        };
        let table: toml::Table = toml::from_str(text).map_err(parse_error)?;
        if let Some(key) = table
            .keys()
            .find(|key| !OVERLAY_KEYS.contains(&key.as_str()))
        {
            return Err(ConfigError::OverlayKey {
                origin: origin.to_owned(),
                key: key.clone(),
            });
        }
        toml::from_str(text).map_err(parse_error)
    }
}

/// Checks that every `{placeholder}` in a template is known.
///
/// # Errors
///
/// Returns a [`TemplateProblem`] for an unknown placeholder or an unclosed brace.
pub fn check_template(template: &str) -> Result<(), TemplateProblem> {
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = rest.get(open + 1..).unwrap_or_default();
        let close = after.find('}').ok_or(TemplateProblem::Unclosed)?;
        let name = after.get(..close).unwrap_or_default();
        if !PLACEHOLDERS.contains(&name) {
            return Err(TemplateProblem::Unknown(name.to_owned()));
        }
        rest = after.get(close + 1..).unwrap_or_default();
    }
    Ok(())
}

/// Fills a template's `{placeholders}` from `lookup`.
///
/// # Errors
///
/// Returns a [`TemplateProblem`] for an unclosed brace or a placeholder `lookup` has no value for.
pub fn expand_template<'a>(
    template: &str,
    lookup: impl Fn(&str) -> Option<&'a str>,
) -> Result<String, TemplateProblem> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(rest.get(..open).unwrap_or_default());
        let after = rest.get(open + 1..).unwrap_or_default();
        let close = after.find('}').ok_or(TemplateProblem::Unclosed)?;
        let name = after.get(..close).unwrap_or_default();
        out.push_str(lookup(name).ok_or_else(|| TemplateProblem::Unknown(name.to_owned()))?);
        rest = after.get(close + 1..).unwrap_or_default();
    }
    out.push_str(rest);
    Ok(out)
}

/// The values a task's command templates see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskVars<'a> {
    /// The task's slug.
    pub slug: &'a str,
    /// The task's branch.
    pub branch: &'a str,
    /// The project's main checkout.
    pub main: &'a str,
    /// The task's worktree, empty until it is known.
    pub worktree: &'a str,
}

impl<'a> TaskVars<'a> {
    /// The value of a placeholder.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&'a str> {
        match name {
            "slug" => Some(self.slug),
            "branch" => Some(self.branch),
            "main" => Some(self.main),
            "worktree" => Some(self.worktree),
            _ => None,
        }
    }

    /// Fills a template with these values.
    ///
    /// # Errors
    ///
    /// Returns a [`TemplateProblem`] for a placeholder a task has no value for, such as `{out}`.
    pub fn expand(&self, template: &str) -> Result<String, TemplateProblem> {
        expand_template(template, |name| self.get(name))
    }
}

/// Expands a leading `~` to the home folder.
#[must_use]
pub fn expand_home(path: &str, home: &Path) -> PathBuf {
    path.strip_prefix('~').map_or_else(
        || PathBuf::from(path),
        |rest| home.join(rest.trim_start_matches(['/', '\\'])),
    )
}
