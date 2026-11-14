//! The headwork home folder: `config.toml`, `state.db`, briefs and the project-local overlays.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use headwork_core::config::expand_home;
use headwork_core::{Config, ProjectOverlay, TaskName};
use headwork_store::Store;

/// The environment variable that overrides the home folder.
pub const HOME_VAR: &str = headwork_adapters::runtime::ENV_HOME;

/// The name of a project-local overlay in a project's main checkout.
pub const OVERLAY_FILE: &str = ".headwork.toml";

/// A located home folder.
#[derive(Debug, Clone)]
pub struct Home {
    root: PathBuf,
    user_home: PathBuf,
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
        let user_home = std::env::home_dir().context("the user's home folder is unknown")?;
        let root = root.unwrap_or_else(|| {
            std::env::var_os(HOME_VAR).map_or_else(|| user_home.join(".headwork"), PathBuf::from)
        });
        Ok(Self { root, user_home })
    }

    /// The home folder.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Expands a leading `~` in a configured path.
    #[must_use]
    pub fn expand(&self, path: &str) -> PathBuf {
        expand_home(path, &self.user_home)
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

    fn config_path(&self) -> PathBuf {
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
        let mut config = Config::parse(&text, &path.display().to_string())?;
        for project in config.projects.values_mut() {
            let overlay = self.expand(&project.main).join(OVERLAY_FILE);
            if let Some(overlay) = read_overlay(&overlay)? {
                *project = project.clone().with_overlay(overlay);
            }
        }
        Ok(config)
    }

    /// Opens `state.db`, creating the folder and the database when missing.
    ///
    /// # Errors
    ///
    /// Fails when the folder or the database cannot be created or opened.
    pub fn open_store(&self) -> Result<Store> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("creating {}", self.root.display()))?;
        let path = self.root.join("state.db");
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
