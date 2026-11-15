//! The shell that runs project command templates, such as a worktree script.
//!
//! On Windows a bare `bash` is a trap: process creation searches `System32` before `PATH`, so it starts WSL's
//! `bash.exe`, whose git has none of the user's SSH keys and waits forever on a prompt. The default is therefore Git
//! Bash, found beside the `git` on `PATH`, in Git's install folders, or on `PATH`, and never WSL's.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The shell to run templates with: the configured one, else [`default_shell`].
#[must_use]
pub fn resolve(configured: Option<&str>) -> PathBuf {
    configured.map_or_else(default_shell, PathBuf::from)
}

/// Git Bash on Windows when it can be found, else plain `bash`.
#[must_use]
pub fn default_shell() -> PathBuf {
    if cfg!(windows) {
        git_bash().unwrap_or_else(|| PathBuf::from("bash"))
    } else {
        PathBuf::from("bash")
    }
}

/// Whether `shell` is WSL's `bash`: `System32\bash.exe` or the `WindowsApps` alias.
#[must_use]
pub fn is_wsl_bash(shell: &Path) -> bool {
    let text = shell.to_string_lossy().replace('\\', "/").to_lowercase();
    let name = text.rsplit('/').next().unwrap_or_default();
    let bash = matches!(name, "bash" | "bash.exe" | "wsl" | "wsl.exe");
    bash && (text.contains("/system32/")
        || text.contains("/sysnative/")
        || text.contains("/windowsapps/"))
}

/// Whether `shell` names a program without a folder, so the system's search order picks it.
#[must_use]
pub fn is_bare(shell: &Path) -> bool {
    shell.components().count() == 1 && !shell.has_root()
}

fn git_bash() -> Option<PathBuf> {
    beside_git()
        .into_iter()
        .chain(install_folders())
        .chain(on_path())
        .find(|candidate| candidate.is_file() && !is_wsl_bash(candidate))
}

/// `bin/bash.exe` in an ancestor of `git --exec-path`, such as `C:/Program Files/Git/mingw64/libexec/git-core`.
fn beside_git() -> Vec<PathBuf> {
    let Ok(output) = Command::new("git")
        .arg("--exec-path")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let exec = PathBuf::from(text.trim());
    exec.ancestors()
        .skip(1)
        .take(4)
        .map(|dir| dir.join("bin").join("bash.exe"))
        .collect()
}

fn install_folders() -> Vec<PathBuf> {
    ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|dir| PathBuf::from(dir).join("Git").join("bin").join("bash.exe"))
        .chain(std::env::var_os("LOCALAPPDATA").map(|dir| {
            PathBuf::from(dir)
                .join("Programs")
                .join("Git")
                .join("bin")
                .join("bash.exe")
        }))
        .collect()
}

fn on_path() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("bash.exe"))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_bash_is_recognized() {
        for wsl in [
            "C:\\Windows\\System32\\bash.exe",
            "c:/windows/system32/BASH.EXE",
            "C:/Users/u/AppData/Local/Microsoft/WindowsApps/bash.exe",
            "C:/Windows/Sysnative/wsl.exe",
        ] {
            assert!(is_wsl_bash(Path::new(wsl)), "{wsl}");
        }
        for other in [
            "C:/Program Files/Git/bin/bash.exe",
            "C:/Windows/System32/cmd.exe",
            "bash",
            "/usr/bin/bash",
        ] {
            assert!(!is_wsl_bash(Path::new(other)), "{other}");
        }
    }

    #[test]
    fn bare_names_have_no_folder() {
        assert!(is_bare(Path::new("bash")), "bare");
        assert!(
            !is_bare(Path::new("C:/Program Files/Git/bin/bash.exe")),
            "absolute"
        );
        assert!(!is_bare(Path::new("bin/bash")), "relative path");
    }

    #[test]
    fn the_default_is_never_wsl() {
        assert!(
            !is_wsl_bash(&default_shell()),
            "{}",
            default_shell().display()
        );
    }
}
