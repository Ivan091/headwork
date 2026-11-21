//! The operating system headwork runs on, and the words its rules and messages use for it.
//!
//! Windows and the Mac share one set of rules files; the parts that name system programs (the stray shells `reap`
//! ends, the tool it replaces, the scheduler `tick --install` uses) come from here, picked by the OS of the running
//! build. Windows schedules the tick with a Task Scheduler task, macOS with a launchd launch agent; other unix systems
//! have no tick scheduler, so their scheduler words are a neutral placeholder.

/// An operating system family headwork tells apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Windows: Git Bash shells, `taskkill`, Job Objects, the Task Scheduler.
    Windows,
    /// macOS: plain unix shells, `kill`, process groups, a launchd launch agent.
    MacOs,
    /// Any other unix: as macOS, without a tick scheduler.
    Unix,
}

impl Platform {
    /// The platform of this build.
    #[must_use]
    pub fn current() -> Self {
        Self::from_os(std::env::consts::OS)
    }

    /// The platform an OS name such as [`std::env::consts::OS`] gives.
    #[must_use]
    pub fn from_os(os: &str) -> Self {
        match os {
            "windows" => Self::Windows,
            "macos" => Self::MacOs,
            _ => Self::Unix,
        }
    }

    /// The leftover shells `reap` ends, as the rules name them.
    #[must_use]
    pub const fn stray_shells(self) -> &'static str {
        match self {
            Self::Windows => "a stray `tail.exe` or `bash.exe`",
            Self::MacOs | Self::Unix => "a stray `tail` or `bash`",
        }
    }

    /// The system tools that end processes by hand, which `reap` replaces.
    #[must_use]
    pub const fn kill_tools(self) -> &'static str {
        match self {
            Self::Windows => "`taskkill`",
            Self::MacOs | Self::Unix => "`kill`, `pkill` or a process-group kill",
        }
    }

    /// What `tick --install` does, as the rules say it.
    #[must_use]
    pub const fn tick_install(self) -> &'static str {
        match self {
            Self::Windows => {
                "creates the Windows scheduled task \"headwork tick\" (every 5 minutes, current user)"
            }
            Self::MacOs => {
                "loads the launchd agent `com.headwork.tick` (`~/Library/LaunchAgents/com.headwork.tick.plist`, every 5 \
                 minutes, current user; cmux's socket must be in `automation` mode)"
            }
            Self::Unix => {
                "would schedule `headwork tick` every 5 minutes for the current user (this system has no scheduler for \
                 it yet: it refuses and changes nothing)"
            }
        }
    }

    /// The help of `tick --install`.
    #[must_use]
    pub const fn tick_install_help(self) -> &'static str {
        match self {
            Self::Windows => {
                "Create (or replace) the Windows scheduled task \"headwork tick\", every 5 minutes for the current user, \
                 and run no tick. Only after the user's `Allow` tap."
            }
            Self::MacOs => {
                "Write and load (or replace) the launchd agent `com.headwork.tick` in ~/Library/LaunchAgents, every 5 \
                 minutes for the current user, and run no tick. Only after the user's `Allow` tap."
            }
            Self::Unix => {
                "Schedule `headwork tick` every 5 minutes for the current user, and run no tick; this system has no \
                 scheduler for it yet, so it refuses. Only after the user's `Allow` tap."
            }
        }
    }

    /// The help of `tick --uninstall`.
    #[must_use]
    pub const fn tick_uninstall_help(self) -> &'static str {
        match self {
            Self::Windows => {
                "Remove the Windows scheduled task \"headwork tick\", and run no tick."
            }
            Self::MacOs => {
                "Unload the launchd agent `com.headwork.tick` and delete its plist, and run no tick."
            }
            Self::Unix => {
                "Remove the scheduled `headwork tick`, and run no tick; this system has no scheduler for it yet, so it \
                 refuses."
            }
        }
    }

    /// What `tick --install` makes, as its report names it.
    #[must_use]
    pub const fn scheduler_entry(self) -> &'static str {
        match self {
            Self::Windows | Self::Unix => "scheduled task",
            Self::MacOs => "launch agent",
        }
    }

    /// Why `tick --install` and `--uninstall` refuse here, or `None` where they work.
    #[must_use]
    pub const fn no_scheduler(self) -> Option<&'static str> {
        match self {
            Self::Windows | Self::MacOs => None,
            Self::Unix => Some(
                "headwork has no tick scheduler on this system yet; nothing changed. Run `headwork tick` by hand or \
                 from the system's own scheduler",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Platform;

    #[test]
    fn the_os_name_picks_the_platform() {
        for (os, expected) in [
            ("windows", Platform::Windows),
            ("macos", Platform::MacOs),
            ("linux", Platform::Unix),
            ("freebsd", Platform::Unix),
        ] {
            assert_eq!(Platform::from_os(os), expected, "{os}");
        }
    }

    #[test]
    fn the_build_knows_its_own_platform() {
        let expected = if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Unix
        };
        assert_eq!(Platform::current(), expected, "this build's platform");
    }

    #[test]
    fn windows_keeps_its_terms() {
        let windows = Platform::Windows;
        assert!(
            windows.stray_shells().contains("`bash.exe`"),
            "Git Bash shells"
        );
        assert_eq!(windows.kill_tools(), "`taskkill`", "the Windows tool");
        assert!(
            windows.tick_install().contains("Windows scheduled task"),
            "the Task Scheduler"
        );
        assert_eq!(windows.no_scheduler(), None, "Windows schedules the tick");
    }

    #[test]
    fn unix_names_no_windows_program() {
        for platform in [Platform::MacOs, Platform::Unix] {
            let words = [
                platform.stray_shells(),
                platform.kill_tools(),
                platform.tick_install(),
                platform.tick_install_help(),
                platform.tick_uninstall_help(),
                platform.no_scheduler().unwrap_or_default(),
            ];
            for text in words {
                for windows_word in ["Windows", "schtasks", "taskkill", ".exe"] {
                    assert!(!text.contains(windows_word), "{platform:?}: {text}");
                }
            }
            assert!(platform.stray_shells().contains("`bash`"), "plain shells");
            assert!(
                platform.kill_tools().contains("process-group"),
                "process groups"
            );
        }
    }

    #[test]
    fn macos_schedules_the_tick_with_a_launch_agent() {
        let mac = Platform::MacOs;
        assert_eq!(mac.no_scheduler(), None, "launchd schedules the tick");
        for text in [
            mac.tick_install(),
            mac.tick_install_help(),
            mac.tick_uninstall_help(),
        ] {
            assert!(text.contains("launchd agent `com.headwork.tick`"), "{text}");
        }
        assert_eq!(mac.scheduler_entry(), "launch agent", "the report's noun");
    }

    #[test]
    fn other_unix_has_no_scheduler() {
        let unix = Platform::Unix;
        assert!(unix.no_scheduler().is_some(), "no scheduler");
        for text in [
            unix.tick_install(),
            unix.tick_install_help(),
            unix.tick_uninstall_help(),
        ] {
            assert!(!text.contains("launchd"), "{text}");
        }
    }
}
