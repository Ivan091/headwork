//! The headwork home folder: `config.toml`, `state.db` and the project-local overlays.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use headwork_core::config::expand_home;
use headwork_core::{Config, ProjectOverlay};
use headwork_store::Store;

/// The environment variable that overrides the home folder.
pub(crate) const HOME_VAR: &str = "HEADWORK_HOME";

/// The name of a project-local overlay in a project's main checkout.
pub(crate) const OVERLAY_FILE: &str = ".headwork.toml";

/// A located home folder.
#[derive(Debug, Clone)]
pub(crate) struct Home {
    root: PathBuf,
    user_home: PathBuf,
}

impl Home {
    /// `$HEADWORK_HOME`, else `~/.headwork`.
    pub(crate) fn locate() -> Result<Self> {
        let user_home = std::env::home_dir().context("the user's home folder is unknown")?;
        let root =
            std::env::var_os(HOME_VAR).map_or_else(|| user_home.join(".headwork"), PathBuf::from);
        Ok(Self { root, user_home })
    }

    fn config_path(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    /// Reads `config.toml` and adds each project's `.headwork.toml` where the main checkout has one.
    pub(crate) fn load_config(&self) -> Result<Config> {
        let path = self.config_path();
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "no config at {}; write one as in docs/design.md, section 5",
                path.display()
            )
        })?;
        let mut config = Config::parse(&text, &path.display().to_string())?;
        for project in config.projects.values_mut() {
            let overlay = expand_home(&project.main, &self.user_home).join(OVERLAY_FILE);
            if let Some(overlay) = read_overlay(&overlay)? {
                *project = project.clone().with_overlay(overlay);
            }
        }
        Ok(config)
    }

    /// Opens `state.db`, creating the folder and the database when missing.
    pub(crate) fn open_store(&self) -> Result<Store> {
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
