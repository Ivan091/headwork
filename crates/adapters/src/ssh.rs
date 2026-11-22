//! The ssh command git runs for a fetch no one watches.
//!
//! Setting `GIT_SSH_COMMAND` replaces the user's `core.sshCommand`. On Windows that setting usually names the system's
//! OpenSSH, the one that reaches the user's ssh agent, while a bare `ssh` resolves to Git Bash's own client, which has
//! no agent and fails with "Permission denied (publickey)". So the non-interactive command keeps the program git would
//! have used and only adds options.

use std::path::Path;
use std::process::{Command, Stdio};

/// What makes ssh fail at once instead of prompting, and give up on a host it cannot reach.
pub const BATCH_OPTIONS: &str = "-o BatchMode=yes -o ConnectTimeout=10";

/// The `GIT_SSH_COMMAND` for a git run no one watches, with [`BATCH_OPTIONS`] added.
///
/// The program is the one git itself would pick, in its order: the `GIT_SSH_COMMAND` variable, the `core.sshCommand`
/// setting, the `GIT_SSH` program, else `ssh`.
#[must_use]
pub fn batch_command(
    env_command: Option<&str>,
    configured: Option<&str>,
    env_program: Option<&str>,
) -> String {
    let base = given(env_command)
        .or_else(|| given(configured))
        .map(str::to_owned)
        .or_else(|| given(env_program).map(quoted))
        .unwrap_or_else(|| "ssh".to_owned());
    format!("{base} {BATCH_OPTIONS}")
}

/// [`batch_command`] for the repository at `dir`, reading the variables and `git config core.sshCommand` there.
#[must_use]
pub fn batch_command_in(dir: &Path) -> String {
    let configured = Command::new("git")
        .args(["config", "--get", "core.sshCommand"])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    let env_command = std::env::var("GIT_SSH_COMMAND").ok();
    let env_program = std::env::var("GIT_SSH").ok();
    batch_command(
        env_command.as_deref(),
        configured.as_deref(),
        env_program.as_deref(),
    )
}

fn given(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn quoted(program: &str) -> String {
    if program.contains([' ', '\t']) {
        format!("\"{program}\"")
    } else {
        program.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_ssh_when_nothing_is_set() {
        assert_eq!(
            batch_command(None, None, None),
            "ssh -o BatchMode=yes -o ConnectTimeout=10",
            "the default client"
        );
    }

    #[test]
    fn the_configured_program_is_kept() {
        assert_eq!(
            batch_command(None, Some("C:/WINDOWS/System32/OpenSSH/ssh.exe"), None),
            "C:/WINDOWS/System32/OpenSSH/ssh.exe -o BatchMode=yes -o ConnectTimeout=10",
            "core.sshCommand must survive"
        );
    }

    #[test]
    fn the_variable_wins_over_the_setting_as_in_git() {
        assert_eq!(
            batch_command(Some("ssh -i key"), Some("other"), Some("third")),
            "ssh -i key -o BatchMode=yes -o ConnectTimeout=10",
            "GIT_SSH_COMMAND first"
        );
    }

    #[test]
    fn a_program_with_spaces_is_quoted() {
        assert_eq!(
            batch_command(None, Some("  "), Some("C:/Program Files/ssh.exe")),
            "\"C:/Program Files/ssh.exe\" -o BatchMode=yes -o ConnectTimeout=10",
            "GIT_SSH is a program, not a shell line"
        );
    }
}
