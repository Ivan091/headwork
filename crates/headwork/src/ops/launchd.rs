//! `tick --install` and `tick --uninstall` on macOS: the one user launch agent that runs `headwork tick`, the
//! counterpart of the Windows scheduled task in [`super::schedule`].
//!
//! The agent is fixed: labelled [`LABEL`], kept at `~/Library/LaunchAgents/<label>.plist`, run every
//! [`EVERY_MINUTES`] minutes in the user's `gui/<uid>` domain, so loading it needs no admin rights. It runs the
//! installed `headwork` (`~/.local/bin`), which hot reload replaces in place, and writes its output to `tick.log` in
//! headwork's home. launchd starts it with an almost empty environment, so the plist carries `PATH` (git, gh, the cmux
//! CLI), `HOME` and `HEADWORK_HOME`. A launchd job does not descend from cmux: cmux's socket must be in `automation`
//! mode (`docs/cmux.md`, "Outside a cmux terminal"). Sessions run these only after the user's `Allow` tap.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::schedule::{Change, EVERY_MINUTES, ScheduleReport};

/// The launch agent's label.
pub const LABEL: &str = "com.headwork.tick";

/// The folders the agent's `PATH` ends with, after the folders of the programs the tick runs.
pub const SYSTEM_PATH: &[&str] = &[
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
];

/// The programs the tick runs, whose folders go first on the agent's `PATH`.
pub const TICK_PROGRAMS: &[&str] = &["git", "gh", "cmux", "claude"];

/// How often `bootstrap` is tried after a `bootout`, which launchd finishes a moment later.
const BOOTSTRAP_TRIES: u32 = 5;

/// Runs `launchctl`, or a fake of it in tests.
pub trait Launchctl {
    /// Runs `launchctl` with `args`; returns whether it succeeded and what it printed.
    ///
    /// # Errors
    ///
    /// Fails when `launchctl` cannot start.
    fn launchctl(&self, args: &[String]) -> Result<(bool, String)>;

    /// The user's launchd domain, `gui/<uid>`.
    ///
    /// # Errors
    ///
    /// Fails when the user id cannot be told.
    fn domain(&self) -> Result<String>;

    /// Waits before the next `bootstrap` try.
    fn pause(&self);
}

/// The system's `launchctl`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemLaunchctl;

impl Launchctl for SystemLaunchctl {
    fn launchctl(&self, args: &[String]) -> Result<(bool, String)> {
        let output = Command::new("launchctl")
            .args(args)
            .output()
            .context("running `launchctl`")?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        Ok((output.status.success(), text.trim().to_owned()))
    }

    fn domain(&self) -> Result<String> {
        let output = Command::new("id")
            .arg("-u")
            .output()
            .context("running `id -u`")?;
        let uid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !output.status.success() || uid.is_empty() || !uid.bytes().all(|b| b.is_ascii_digit()) {
            bail!("`id -u` gave no user id");
        }
        Ok(format!("gui/{uid}"))
    }

    fn pause(&self) {
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// What the launch agent is made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    /// The plist file, `~/Library/LaunchAgents/<label>.plist`.
    pub plist: PathBuf,
    /// The program it runs with `tick`.
    pub exe: PathBuf,
    /// Where its output goes.
    pub log: PathBuf,
    /// The user's home folder, its `HOME`.
    pub user_home: PathBuf,
    /// headwork's home, its `HEADWORK_HOME`.
    pub headwork_home: PathBuf,
    /// Its `PATH`.
    pub path: String,
}

impl Agent {
    /// The agent for `exe`, with its plist under `user_home` and its log in `headwork_home`.
    #[must_use]
    pub fn new(exe: &Path, user_home: &Path, headwork_home: &Path, path: String) -> Self {
        Self {
            plist: plist_path(user_home),
            exe: exe.to_path_buf(),
            log: headwork_home.join("tick.log"),
            user_home: user_home.to_path_buf(),
            headwork_home: headwork_home.to_path_buf(),
            path,
        }
    }

    /// The command line the agent runs: the program, quoted, then `tick`.
    #[must_use]
    pub fn command(&self) -> String {
        format!("\"{}\" tick", self.exe.display())
    }

    /// The plist's text.
    #[must_use]
    pub fn plist_text(&self) -> String {
        let mut text = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n",
        );
        let _ = writeln!(text, "  <key>Label</key>\n  <string>{LABEL}</string>");
        let _ = writeln!(
            text,
            "  <key>ProgramArguments</key>\n  <array>\n    <string>{}</string>\n    <string>tick</string>\n  </array>",
            escape(&self.exe.display().to_string())
        );
        let _ = writeln!(
            text,
            "  <key>StartInterval</key>\n  <integer>{}</integer>",
            EVERY_MINUTES * 60
        );
        let log = escape(&self.log.display().to_string());
        let _ = writeln!(
            text,
            "  <key>StandardOutPath</key>\n  <string>{log}</string>"
        );
        let _ = writeln!(
            text,
            "  <key>StandardErrorPath</key>\n  <string>{log}</string>"
        );
        text.push_str("  <key>EnvironmentVariables</key>\n  <dict>\n");
        for (key, value) in [
            ("PATH", self.path.clone()),
            ("HOME", self.user_home.display().to_string()),
            ("HEADWORK_HOME", self.headwork_home.display().to_string()),
        ] {
            let _ = writeln!(
                text,
                "    <key>{key}</key>\n    <string>{}</string>",
                escape(&value)
            );
        }
        text.push_str("  </dict>\n</dict>\n</plist>\n");
        text
    }
}

/// Where the agent's plist lives for the user at `user_home`.
#[must_use]
pub fn plist_path(user_home: &Path) -> PathBuf {
    user_home
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

/// The agent's `PATH`: the folders `programs` were found in, then [`SYSTEM_PATH`], each once, in that order.
#[must_use]
pub fn agent_path(programs: &[PathBuf]) -> String {
    let mut dirs: Vec<String> = Vec::new();
    let found = programs
        .iter()
        .filter_map(|program| program.parent())
        .map(|dir| dir.display().to_string());
    for dir in found.chain(SYSTEM_PATH.iter().map(|dir| (*dir).to_owned())) {
        if !dir.is_empty() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs.join(":")
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn service(domain: &str) -> String {
    format!("{domain}/{LABEL}")
}

fn loaded(launchctl: &dyn Launchctl, domain: &str) -> Result<bool> {
    let (found, _) = launchctl.launchctl(&["print".to_owned(), service(domain)])?;
    Ok(found)
}

fn bootout(launchctl: &dyn Launchctl, domain: &str) -> Result<()> {
    let (removed, text) = launchctl.launchctl(&["bootout".to_owned(), service(domain)])?;
    if !removed {
        bail!("`launchctl bootout {}` refused: {text}", service(domain));
    }
    Ok(())
}

/// Writes the agent's plist and loads it, replacing a loaded or written one.
///
/// # Errors
///
/// Fails when the plist cannot be written, or `launchctl` cannot run or refuses.
pub fn install(launchctl: &dyn Launchctl, agent: &Agent) -> Result<ScheduleReport> {
    let domain = launchctl.domain()?;
    let was_loaded = loaded(launchctl, &domain)?;
    let existed = was_loaded || agent.plist.is_file();
    if was_loaded {
        bootout(launchctl, &domain)?;
    }
    if let Some(dir) = agent.plist.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    if let Some(dir) = agent.log.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&agent.plist, agent.plist_text())
        .with_context(|| format!("writing {}", agent.plist.display()))?;
    let args = [
        "bootstrap".to_owned(),
        domain.clone(),
        agent.plist.display().to_string(),
    ];
    let mut tries = 0;
    loop {
        tries += 1;
        let (started, text) = launchctl.launchctl(&args)?;
        if started {
            break;
        }
        if tries >= BOOTSTRAP_TRIES {
            bail!(
                "`launchctl bootstrap {domain} {}` refused: {text}",
                agent.plist.display()
            );
        }
        launchctl.pause();
    }
    Ok(ScheduleReport {
        task: LABEL,
        change: if existed {
            Change::Replaced
        } else {
            Change::Created
        },
        command: Some(agent.command()),
    })
}

/// Unloads the agent and deletes its plist; nothing to do when neither is there.
///
/// # Errors
///
/// Fails when `launchctl` cannot run or refuses, or the plist cannot be deleted.
pub fn uninstall(launchctl: &dyn Launchctl, plist: &Path) -> Result<ScheduleReport> {
    let domain = launchctl.domain()?;
    let was_loaded = loaded(launchctl, &domain)?;
    let written = plist.is_file();
    if !was_loaded && !written {
        return Ok(ScheduleReport {
            task: LABEL,
            change: Change::Absent,
            command: None,
        });
    }
    if was_loaded {
        bootout(launchctl, &domain)?;
    }
    if written {
        std::fs::remove_file(plist).with_context(|| format!("deleting {}", plist.display()))?;
    }
    Ok(ScheduleReport {
        task: LABEL,
        change: Change::Removed,
        command: None,
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "tests")]

    use std::cell::{Cell, RefCell};

    use super::*;

    struct Fake {
        loaded: bool,
        refuse: u32,
        refused: Cell<u32>,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl Fake {
        fn new(loaded: bool, refuse: u32) -> Self {
            Self {
                loaded,
                refuse,
                refused: Cell::new(0),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn verbs(&self) -> Vec<String> {
            self.calls
                .borrow()
                .iter()
                .map(|call| call[0].clone())
                .collect()
        }
    }

    impl Launchctl for Fake {
        fn launchctl(&self, args: &[String]) -> Result<(bool, String)> {
            self.calls.borrow_mut().push(args.to_vec());
            Ok(match args[0].as_str() {
                "print" => (self.loaded, String::new()),
                "bootstrap" if self.refused.get() < self.refuse => {
                    self.refused.set(self.refused.get() + 1);
                    (false, "Bootstrap failed: 5: Input/output error".to_owned())
                }
                _ => (true, String::new()),
            })
        }

        fn domain(&self) -> Result<String> {
            Ok("gui/501".to_owned())
        }

        fn pause(&self) {}
    }

    fn agent(dir: &Path) -> Agent {
        Agent::new(
            Path::new("/Users/me/.local/bin/headwork"),
            &dir.join("me"),
            &dir.join("me/.headwork"),
            "/Users/me/.local/bin:/usr/bin:/bin".to_owned(),
        )
    }

    #[test]
    fn the_plist_runs_the_tick_every_five_minutes_with_its_environment() {
        let dir = Path::new("/Users");
        let text = agent(dir).plist_text();
        for part in [
            "<key>Label</key>\n  <string>com.headwork.tick</string>",
            "<string>/Users/me/.local/bin/headwork</string>\n    <string>tick</string>",
            "<key>StartInterval</key>\n  <integer>300</integer>",
            "<key>StandardOutPath</key>\n  <string>/Users/me/.headwork/tick.log</string>",
            "<key>StandardErrorPath</key>\n  <string>/Users/me/.headwork/tick.log</string>",
            "<key>PATH</key>\n    <string>/Users/me/.local/bin:/usr/bin:/bin</string>",
            "<key>HOME</key>\n    <string>/Users/me</string>",
            "<key>HEADWORK_HOME</key>\n    <string>/Users/me/.headwork</string>",
        ] {
            assert!(text.contains(part), "{part}\n{text}");
        }
        assert_eq!(
            agent(dir).plist,
            Path::new("/Users/me/Library/LaunchAgents/com.headwork.tick.plist"),
            "a user agent"
        );
    }

    #[test]
    fn the_plist_escapes_xml() {
        let odd = Agent::new(
            Path::new("/a&b/<h>"),
            Path::new("/u"),
            Path::new("/u/h"),
            String::new(),
        );
        assert!(
            odd.plist_text()
                .contains("<string>/a&amp;b/&lt;h&gt;</string>"),
            "escaped"
        );
    }

    #[test]
    fn the_path_puts_the_programs_first_once() {
        let path = agent_path(&[
            PathBuf::from("/opt/homebrew/bin/git"),
            PathBuf::from("/Applications/cmux.app/Contents/Resources/bin/cmux"),
            PathBuf::from("/opt/homebrew/bin/gh"),
        ]);
        assert_eq!(
            path,
            "/opt/homebrew/bin:/Applications/cmux.app/Contents/Resources/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            "deduplicated, programs first"
        );
    }

    #[test]
    fn install_writes_and_loads_a_fresh_agent() {
        let dir = tempfile::tempdir().unwrap();
        let agent = agent(dir.path());
        let fresh = Fake::new(false, 0);
        let report = install(&fresh, &agent).unwrap();
        assert_eq!(report.change, Change::Created, "fresh");
        assert_eq!(report.task, LABEL, "label");
        assert_eq!(
            report.command.as_deref(),
            Some("\"/Users/me/.local/bin/headwork\" tick"),
            "command"
        );
        assert_eq!(fresh.verbs(), ["print", "bootstrap"], "no bootout");
        assert_eq!(
            fresh.calls.borrow()[1],
            [
                "bootstrap".to_owned(),
                "gui/501".to_owned(),
                agent.plist.display().to_string()
            ],
            "the user's gui domain"
        );
        assert_eq!(
            std::fs::read_to_string(&agent.plist).unwrap(),
            agent.plist_text(),
            "written"
        );
    }

    #[test]
    fn install_boots_out_a_loaded_agent_and_reports_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let agent = agent(dir.path());
        let again = Fake::new(true, 0);
        assert_eq!(
            install(&again, &agent).unwrap().change,
            Change::Replaced,
            "loaded"
        );
        assert_eq!(again.verbs(), ["print", "bootout", "bootstrap"], "calls");
        assert_eq!(
            again.calls.borrow()[1],
            ["bootout", "gui/501/com.headwork.tick"],
            "the service"
        );
        let written = Fake::new(false, 0);
        assert_eq!(
            install(&written, &agent).unwrap().change,
            Change::Replaced,
            "a plist already written"
        );
    }

    #[test]
    fn install_retries_a_bootstrap_then_gives_up() {
        let dir = tempfile::tempdir().unwrap();
        let agent = agent(dir.path());
        let slow = Fake::new(true, 2);
        install(&slow, &agent).unwrap();
        assert_eq!(
            slow.verbs(),
            ["print", "bootout", "bootstrap", "bootstrap", "bootstrap"],
            "tried until loaded"
        );
        let refused = install(&Fake::new(false, BOOTSTRAP_TRIES), &agent).unwrap_err();
        assert!(
            refused.to_string().contains("Input/output error"),
            "{refused}"
        );
    }

    #[test]
    fn uninstall_unloads_and_deletes_only_what_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let agent = agent(dir.path());
        let absent = Fake::new(false, 0);
        assert_eq!(
            uninstall(&absent, &agent.plist).unwrap().change,
            Change::Absent,
            "absent"
        );
        assert_eq!(absent.verbs(), ["print"], "nothing booted out");
        install(&Fake::new(false, 0), &agent).unwrap();
        let present = Fake::new(true, 0);
        assert_eq!(
            uninstall(&present, &agent.plist).unwrap().change,
            Change::Removed,
            "present"
        );
        assert_eq!(present.verbs(), ["print", "bootout"], "booted out");
        assert!(!agent.plist.exists(), "plist deleted");
        install(&Fake::new(false, 0), &agent).unwrap();
        let unloaded = Fake::new(false, 0);
        assert_eq!(
            uninstall(&unloaded, &agent.plist).unwrap().change,
            Change::Removed,
            "a written plist that was not loaded"
        );
        assert_eq!(unloaded.verbs(), ["print"], "no bootout");
        assert!(!agent.plist.exists(), "plist deleted");
    }
}
