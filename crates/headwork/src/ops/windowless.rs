//! The windowless tick on Windows: a launcher that runs `headwork tick` with no console window.
//!
//! Task Scheduler opens a console window for a console program on every run. The launcher, `headworkw.exe`, is a copy
//! of `headwork.exe` whose PE header names the GUI subsystem ([`gui_subsystem`]), so Windows gives it no console.
//! When a program named `headworkw` starts, it does nothing but [`forward`]: it runs the `headwork` beside it with the
//! same arguments under `CREATE_NO_WINDOW`, which gives that process a console of its own that is never shown. Every
//! console program the tick starts (git, gh, bash, wmux) shares that hidden console, so none opens a window either.
//! Output goes to `<home>/tick.log`, as on macOS.
//!
//! `tick --install` writes the launcher ([`write_launcher`]) beside the program the task runs. It forwards to whatever
//! `headwork` sits beside it, so hot reload and `headwork update` need no new launcher. Why not `conhost --headless`:
//! `docs/decisions.md`, 2026-11-20 and 2026-11-22.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

/// The launcher's file stem.
pub const LAUNCHER_STEM: &str = "headworkw";

/// The log the launcher appends the tick's output to, in the home.
pub const LOG_NAME: &str = "tick.log";

/// The log is started afresh once it grows past this many bytes.
pub const LOG_LIMIT: u64 = 1024 * 1024;

const SUBSYSTEM_GUI: u16 = 2;
const SUBSYSTEM_CONSOLE: u16 = 3;
const PE_POINTER_AT: usize = 0x3c;
const OPTIONAL_HEADER_AFTER_PE: usize = 24;
const SUBSYSTEM_IN_OPTIONAL_HEADER: usize = 68;
const PE32_MAGIC: u16 = 0x10b;
const PE32_PLUS_MAGIC: u16 = 0x20b;

/// The launcher's file name in its folder.
#[must_use]
pub fn launcher_name() -> String {
    format!("{LAUNCHER_STEM}{}", std::env::consts::EXE_SUFFIX)
}

/// Whether the program at `exe` is the launcher.
#[must_use]
pub fn is_launcher(exe: &Path) -> bool {
    exe.file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case(LAUNCHER_STEM))
}

fn u16_at(image: &[u8], at: usize) -> Option<u16> {
    let bytes = image.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([*bytes.first()?, *bytes.get(1)?]))
}

fn u32_at(image: &[u8], at: usize) -> Option<u32> {
    let bytes: [u8; 4] = image.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

/// Where the subsystem field of the PE image `image` is, after checking its headers.
fn subsystem_offset(image: &[u8]) -> Result<usize> {
    if image.get(..2) != Some(b"MZ".as_slice()) {
        bail!("not a Windows program: no MZ header");
    }
    let pe = u32_at(image, PE_POINTER_AT)
        .and_then(|pe| usize::try_from(pe).ok())
        .context("not a Windows program: no PE header pointer")?;
    if image.get(pe..pe.saturating_add(4)) != Some(b"PE\0\0".as_slice()) {
        bail!("not a Windows program: no PE signature");
    }
    let optional = pe
        .checked_add(OPTIONAL_HEADER_AFTER_PE)
        .context("PE header out of range")?;
    match u16_at(image, optional) {
        Some(PE32_MAGIC | PE32_PLUS_MAGIC) => {}
        _ => bail!("not a Windows program: unknown optional header"),
    }
    optional
        .checked_add(SUBSYSTEM_IN_OPTIONAL_HEADER)
        .filter(|&at| u16_at(image, at).is_some())
        .context("PE optional header is cut short")
}

/// Marks the PE image `image` as a GUI program, so Windows starts it without a console.
///
/// # Errors
///
/// Fails when `image` is not a Windows program, or names a subsystem other than console or GUI.
pub fn gui_subsystem(image: &mut [u8]) -> Result<()> {
    let at = subsystem_offset(image)?;
    match u16_at(image, at) {
        Some(SUBSYSTEM_CONSOLE | SUBSYSTEM_GUI) => {}
        other => bail!("unexpected PE subsystem {other:?}"),
    }
    let field = image
        .get_mut(at..at.saturating_add(2))
        .context("PE optional header is cut short")?;
    field.copy_from_slice(&SUBSYSTEM_GUI.to_le_bytes());
    Ok(())
}

/// The subsystem the PE image `image` names: 2 GUI, 3 console.
///
/// # Errors
///
/// Fails when `image` is not a Windows program.
pub fn subsystem(image: &[u8]) -> Result<u16> {
    let at = subsystem_offset(image)?;
    u16_at(image, at).context("PE optional header is cut short")
}

/// Writes the launcher beside `program`, from a copy of `program`, replacing an older one; returns its path.
///
/// A launcher that runs at that moment is moved aside as an old copy, deleted by this or a later install
/// ([`super::install::clean`]).
///
/// # Errors
///
/// Fails when `program` cannot be read or is not a Windows program, or when the launcher cannot be written.
pub fn write_launcher(program: &Path) -> Result<PathBuf> {
    let dir = program
        .parent()
        .with_context(|| format!("{} has no folder", program.display()))?;
    let mut image =
        std::fs::read(program).with_context(|| format!("reading {}", program.display()))?;
    gui_subsystem(&mut image)
        .with_context(|| format!("making a launcher of {}", program.display()))?;
    let target = dir.join(launcher_name());
    let staged = dir.join(format!(
        "{LAUNCHER_STEM}.new{}",
        std::env::consts::EXE_SUFFIX
    ));
    std::fs::write(&staged, &image).with_context(|| format!("writing {}", staged.display()))?;
    if target.exists() && std::fs::remove_file(&target).is_err() {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis());
        let aside = dir.join(format!(
            "headwork.old-w-{millis}{}",
            std::env::consts::EXE_SUFFIX
        ));
        std::fs::rename(&target, &aside)
            .with_context(|| format!("moving the running {} aside", target.display()))?;
    }
    std::fs::rename(&staged, &target)
        .with_context(|| format!("renaming {} to {}", staged.display(), target.display()))?;
    super::install::clean(dir)?;
    Ok(target)
}

/// The `headwork` beside the launcher at `launcher`.
#[must_use]
pub fn target_of(launcher: &Path) -> PathBuf {
    launcher.with_file_name(super::install::program_name())
}

/// Opens the log in `home_root` for appending, started afresh once it is over [`LOG_LIMIT`].
///
/// # Errors
///
/// Fails when the log cannot be opened.
pub fn open_log(home_root: &Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(home_root)
        .with_context(|| format!("creating {}", home_root.display()))?;
    let path = home_root.join(LOG_NAME);
    let full = std::fs::metadata(&path).is_ok_and(|meta| meta.len() > LOG_LIMIT);
    std::fs::OpenOptions::new()
        .create(true)
        .append(!full)
        .write(true)
        .truncate(full)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))
}

/// Runs the `headwork` beside `launcher` with `args` and no visible window, its output appended to the log in
/// `home_root`; returns its exit code.
///
/// # Errors
///
/// Fails when the log cannot be opened or the program cannot start.
pub fn forward(launcher: &Path, args: &[OsString], home_root: &Path) -> Result<ExitCode> {
    let program = target_of(launcher);
    let log = open_log(home_root)?;
    let mut command = std::process::Command::new(&program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone().context("sharing the log")?)
        .stderr(log);
    hide(&mut command);
    let status = command
        .status()
        .with_context(|| format!("running {}", program.display()))?;
    Ok(status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .map_or(ExitCode::FAILURE, ExitCode::from))
}

#[cfg(windows)]
fn hide(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
const fn hide(_command: &mut std::process::Command) {}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "tests")]

    use super::*;

    fn image(magic: u16, subsystem: u16) -> Vec<u8> {
        let pe = 0x80_usize;
        let mut image = vec![0_u8; 0x200];
        image[..2].copy_from_slice(b"MZ");
        image[PE_POINTER_AT..PE_POINTER_AT + 4].copy_from_slice(&0x80_u32.to_le_bytes());
        image[pe..pe + 4].copy_from_slice(b"PE\0\0");
        let optional = pe + OPTIONAL_HEADER_AFTER_PE;
        image[optional..optional + 2].copy_from_slice(&magic.to_le_bytes());
        let at = optional + SUBSYSTEM_IN_OPTIONAL_HEADER;
        image[at..at + 2].copy_from_slice(&subsystem.to_le_bytes());
        image
    }

    #[test]
    fn a_console_program_becomes_a_gui_one() {
        for magic in [PE32_MAGIC, PE32_PLUS_MAGIC] {
            let mut bytes = image(magic, SUBSYSTEM_CONSOLE);
            let before = bytes.clone();
            assert_eq!(subsystem(&bytes).unwrap(), SUBSYSTEM_CONSOLE, "before");
            gui_subsystem(&mut bytes).unwrap();
            assert_eq!(subsystem(&bytes).unwrap(), SUBSYSTEM_GUI, "after");
            let changed = before.iter().zip(&bytes).filter(|(a, b)| a != b).count();
            assert_eq!(changed, 1, "only the subsystem byte changes");
        }
    }

    #[test]
    fn what_is_not_a_console_or_gui_program_is_refused() {
        assert!(
            gui_subsystem(&mut b"#!/bin/sh\n".to_vec()).is_err(),
            "script"
        );
        assert!(gui_subsystem(&mut b"MZ".to_vec()).is_err(), "cut short");
        assert!(
            gui_subsystem(&mut image(0x999, SUBSYSTEM_CONSOLE)).is_err(),
            "magic"
        );
        assert!(
            gui_subsystem(&mut image(PE32_PLUS_MAGIC, 10)).is_err(),
            "EFI"
        );
        let mut bad_pointer = image(PE32_PLUS_MAGIC, SUBSYSTEM_CONSOLE);
        bad_pointer[PE_POINTER_AT..PE_POINTER_AT + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            gui_subsystem(&mut bad_pointer).is_err(),
            "pointer past the end"
        );
    }

    #[test]
    fn the_launcher_is_told_by_its_name() {
        assert!(is_launcher(Path::new("C:/bin/headworkw.exe")), "exe");
        assert!(is_launcher(Path::new("C:/bin/HeadworkW.EXE")), "case");
        assert!(!is_launcher(Path::new("C:/bin/headwork.exe")), "headwork");
        assert_eq!(
            target_of(Path::new("C:/bin/headworkw.exe")),
            Path::new("C:/bin").join(super::super::install::program_name()),
            "the headwork beside it"
        );
    }

    #[test]
    fn the_launcher_is_written_beside_the_program_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("headwork.exe");
        std::fs::write(&program, image(PE32_PLUS_MAGIC, SUBSYSTEM_CONSOLE)).unwrap();
        let launcher = write_launcher(&program).unwrap();
        assert_eq!(launcher, dir.path().join(launcher_name()), "beside");
        assert_eq!(
            subsystem(&std::fs::read(&launcher).unwrap()).unwrap(),
            SUBSYSTEM_GUI,
            "gui"
        );
        assert_eq!(
            subsystem(&std::fs::read(&program).unwrap()).unwrap(),
            SUBSYSTEM_CONSOLE,
            "the program itself stays a console program"
        );
        write_launcher(&program).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["headwork.exe", launcher_name().as_str()],
            "no leftovers"
        );
    }

    #[test]
    fn the_log_starts_afresh_when_full() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOG_NAME);
        std::fs::write(&path, b"old\n").unwrap();
        drop(open_log(dir.path()).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"old\n", "appended to");
        std::fs::write(&path, vec![b'x'; usize::try_from(LOG_LIMIT).unwrap() + 1]).unwrap();
        drop(open_log(dir.path()).unwrap());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0, "started afresh");
    }

    #[cfg(windows)]
    #[test]
    fn a_real_build_can_be_made_a_launcher() {
        let mut bytes = std::fs::read(std::env::current_exe().unwrap()).unwrap();
        assert_eq!(
            subsystem(&bytes).unwrap(),
            SUBSYSTEM_CONSOLE,
            "a test binary is a console program"
        );
        gui_subsystem(&mut bytes).unwrap();
        assert_eq!(subsystem(&bytes).unwrap(), SUBSYSTEM_GUI, "patched");
    }
}
