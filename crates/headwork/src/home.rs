//! The headwork home folder: `config.toml`, `state.db`, briefs and the project-local overlays.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use headwork_core::config::expand_home;
use headwork_core::{Config, ProjectOverlay, TaskName};
use headwork_store::Store;

/// The environment variable that overrides the home folder.
pub const HOME_VAR: &str = headwork_adapters::runtime::ENV_HOME;

/// The environment variable that names Claude Code's state file instead of the usual place; tests point it at a
/// temporary file.
pub const CLAUDE_JSON_VAR: &str = "HEADWORK_CLAUDE_JSON";

/// The environment variable that names the user's Claude folder (`settings.json`) instead of the usual place; tests
/// point it at a temporary folder.
pub const CLAUDE_DIR_VAR: &str = "HEADWORK_CLAUDE_DIR";

/// The name of a project-local overlay in a project's main checkout.
pub const OVERLAY_FILE: &str = ".headwork.toml";

/// A located home folder.
#[derive(Debug, Clone)]
pub struct Home {
    root: PathBuf,
    user_dir: PathBuf,
    claude_json: PathBuf,
    claude_dir: PathBuf,
}

impl Home {
    /// `$HEADWORK_HOME`, else `~/.headwork`.
    ///
    /// # Errors
    ///
    /// Fails when the user's home folder is unknown.
    pub fn locate() -> Result<Self> {
        let root = std::env::var_os(HOME_VAR).map(PathBuf::from);
        Self::at(root)
    }

    /// The home at `root`, else as [`Home::locate`] finds it.
    ///
    /// # Errors
    ///
    /// Fails when the user's home folder is unknown.
    pub fn at(root: Option<PathBuf>) -> Result<Self> {
        let user_dir = std::env::home_dir().context("the user's home folder is unknown")?;
        let root = root.unwrap_or_else(|| {
            std::env::var_os(HOME_VAR).map_or_else(|| user_dir.join(".headwork"), PathBuf::from)
        });
        let claude_json = std::env::var_os(CLAUDE_JSON_VAR)
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("CLAUDE_CONFIG_DIR")
                    .map(|dir| PathBuf::from(dir).join(".claude.json"))
            })
            .unwrap_or_else(|| user_dir.join(".claude.json"));
        let claude_dir = std::env::var_os(CLAUDE_DIR_VAR)
            .or_else(|| std::env::var_os("CLAUDE_CONFIG_DIR"))
            .map_or_else(|| user_dir.join(".claude"), PathBuf::from);
        Ok(Self {
            root,
            user_dir,
            claude_json,
            claude_dir,
        })
    }

    /// The same home with Claude Code's state file somewhere else.
    #[must_use]
    pub fn with_claude_json(mut self, file: PathBuf) -> Self {
        self.claude_json = file;
        self
    }

    /// The same home with the user's Claude folder somewhere else.
    #[must_use]
    pub fn with_claude_dir(mut self, dir: PathBuf) -> Self {
        self.claude_dir = dir;
        self
    }

    /// The user's Claude folder, which holds `settings.json`: `$HEADWORK_CLAUDE_DIR`, else `$CLAUDE_CONFIG_DIR`, else
    /// `~/.claude`.
    #[must_use]
    pub fn claude_dir(&self) -> &Path {
        &self.claude_dir
    }

    /// Claude Code's state file, where folder trust is kept: `$HEADWORK_CLAUDE_JSON`, else
    /// `$CLAUDE_CONFIG_DIR/.claude.json`, else `~/.claude.json`.
    #[must_use]
    pub fn claude_json(&self) -> &Path {
        &self.claude_json
    }

    /// The home folder.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Expands a leading `~` in a configured path.
    #[must_use]
    pub fn expand(&self, path: &str) -> PathBuf {
        expand_home(path, &self.user_dir)
    }

    /// Where a task's brief is kept.
    #[must_use]
    pub fn brief_path(&self, name: &TaskName) -> PathBuf {
        self.root.join("briefs").join(format!("{name}.md"))
    }

    /// Where a task's handoff is kept: the recap its next session reads.
    #[must_use]
    pub fn handoff_path(&self, name: &TaskName) -> PathBuf {
        self.root.join("handoffs").join(format!("{name}.md"))
    }

    /// Where the summary of a resume is written.
    #[must_use]
    pub fn recaps_dir(&self) -> PathBuf {
        self.root.join("recaps")
    }

    /// Where registered jobs write their output.
    #[must_use]
    pub fn jobs_dir(&self) -> PathBuf {
        self.root.join("jobs")
    }

    /// The machine configuration file.
    #[must_use]
    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    /// Reads `config.toml` and adds each project's `.headwork.toml` where the main checkout has one.
    ///
    /// # Errors
    ///
    /// Fails when the file is missing or invalid.
    pub fn load_config(&self) -> Result<Config> {
        let path = self.config_path();
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "no config at {}; write one as in docs/design.md, section 5",
                path.display()
            )
        })?;
        let config = Config::parse(&text, &path.display().to_string())?;
        self.with_overlays(config)
    }

    /// Where the repository and machine a synced config comes from are named (`docs/config-sync.md`).
    #[must_use]
    pub fn sources_path(&self) -> PathBuf {
        self.root.join("sources.toml")
    }

    /// The untracked layer of a synced config: secrets and local tweaks.
    #[must_use]
    pub fn local_config_path(&self) -> PathBuf {
        self.root.join("config.local.toml")
    }

    /// The effective config before the last sync.
    #[must_use]
    pub fn prev_config_path(&self) -> PathBuf {
        self.root.join("config.toml.prev")
    }

    /// Where each key of the last synced config came from.
    #[must_use]
    pub fn origin_path(&self) -> PathBuf {
        self.root.join("config.origin.json")
    }

    /// Adds each project's `.headwork.toml` where the main checkout has one.
    ///
    /// # Errors
    ///
    /// Fails when an overlay is invalid.
    pub fn with_overlays(&self, mut config: Config) -> Result<Config> {
        for project in config.projects.values_mut() {
            let overlay = self.expand(&project.main).join(OVERLAY_FILE);
            if let Some(overlay) = read_overlay(&overlay)? {
                *project = project.clone().with_overlay(overlay);
            }
        }
        Ok(config)
    }

    /// The store file, `state.db`.
    #[must_use]
    pub fn store_path(&self) -> PathBuf {
        self.root.join("state.db")
    }

    /// Opens `state.db`, creating the folder and the database when missing.
    ///
    /// # Errors
    ///
    /// Fails when the folder or the database cannot be created or opened.
    pub fn open_store(&self) -> Result<Store> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("creating {}", self.root.display()))?;
        let path = self.store_path();
        Store::open(&path).with_context(|| format!("opening {}", path.display()))
    }
}

fn read_overlay(path: &Path) -> Result<Option<ProjectOverlay>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(ProjectOverlay::parse(
            &text,
            &path.display().to_string(),
        )?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}
