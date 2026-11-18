//! The jobs registry (design sections 3.3 and 6.1): long-running processes a session registered, so a park can stop
//! them cleanly and a resume can run them again.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};
use headwork_adapters::proc::{self, Liveness};
use headwork_core::config::ParkPolicy;
use headwork_core::{Actor, JobState};
use headwork_store::{JobRow, NewJob, Store, Task, TaskId};

use super::Env;

/// What a session says about a job it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// What it is, in a few words.
    pub label: String,
    /// The command line that starts it again.
    pub argv: Vec<String>,
    /// The folder it runs in; the task's folder when absent.
    pub cwd: Option<String>,
    /// Its process id.
    pub pid: Option<u32>,
    /// What a park does with it; the project's `jobs.<label>` or `rerun` when absent.
    pub park_policy: Option<ParkPolicy>,
    /// The file a `checkpoint` park waits on; the project's when absent.
    pub checkpoint_path: Option<String>,
    /// Resume starts it again; true when absent.
    pub restart: Option<bool>,
    /// It holds a heavy slot.
    pub heavy: bool,
}

/// Registers a job of a task's session.
///
/// # Errors
///
/// Fails when the command line is empty, a `checkpoint` job names no checkpoint file, or the store fails.
pub fn register(
    store: &mut Store,
    env: &Env<'_>,
    task: &Task,
    session: &str,
    job: &Registration,
) -> Result<JobRow> {
    ensure!(
        !job.argv.is_empty(),
        "a job needs the command line that starts it"
    );
    ensure!(!job.label.trim().is_empty(), "a job needs a label");
    let configured = env.project(task)?.jobs.get(&job.label);
    let policy = job
        .park_policy
        .or_else(|| configured.map(|c| c.park_policy))
        .unwrap_or(ParkPolicy::Rerun);
    let checkpoint = job.checkpoint_path.clone().or_else(|| {
        configured
            .and_then(|c| c.checkpoint.clone())
            .filter(|path| !path.contains('{'))
    });
    ensure!(
        policy != ParkPolicy::Checkpoint || checkpoint.is_some(),
        "a `checkpoint` job needs the checkpoint file a park waits for; pass `checkpoint_path` (a configured path \
         with a `{{placeholder}}` cannot be used as it is)"
    );
    let cwd = match &job.cwd {
        Some(cwd) => cwd.clone(),
        None => super::slashed(&env.task_dir(task)?),
    };
    let log = env
        .home
        .jobs_dir()
        .join(format!("{}-{}.log", task.name, sanitize(&job.label)));
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let identity = job.pid.and_then(proc::identify);
    let id = store.register_job(
        &NewJob {
            task_id: Some(task.id),
            session_id: Some(session),
            project: task.project().as_str(),
            label: &job.label,
            park_policy: policy,
            argv: &job.argv,
            cwd: &cwd,
            pid: job.pid,
            identity: identity.as_ref(),
            checkpoint_path: checkpoint.as_deref(),
            heavy: job.heavy,
            restart: job.restart.unwrap_or(true),
            log_path: &super::slashed(&log),
        },
        env.actor,
    )?;
    store
        .job(id)?
        .with_context(|| format!("job {id} vanished after it was registered"))
}

/// A job log larger than this is rotated when its job starts.
pub const LOG_LIMIT_BYTES: u64 = 10 * 1024 * 1024;

/// How many rotated copies (`<log>.1` newest) of a job log are kept.
pub const LOG_KEEP: u32 = 5;

/// Rotates a log larger than `limit` bytes.
///
/// The log becomes `<log>.1`, older copies shift up and the one past `keep` is dropped.
/// Best effort: a file another process holds open stays where it is and the job appends to it.
pub fn rotate_log(log: &Path, limit: u64, keep: u32) {
    let large = std::fs::metadata(log).is_ok_and(|meta| meta.len() > limit);
    if !large || keep == 0 {
        return;
    }
    let numbered = |n: u32| {
        let mut name = log.as_os_str().to_owned();
        name.push(format!(".{n}"));
        PathBuf::from(name)
    };
    std::fs::remove_file(numbered(keep)).ok();
    for n in (1..keep).rev() {
        if numbered(n).exists() {
            std::fs::rename(numbered(n), numbered(n + 1)).ok();
        }
    }
    std::fs::rename(log, numbered(1)).ok();
}

fn sanitize(label: &str) -> String {
    label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Registers a job and starts it, detached, in its folder with its output in the job log.
///
/// # Errors
///
/// Fails as [`register`] and [`restart`] do; a job that does not start is recorded as failed.
pub fn start(
    store: &mut Store,
    env: &Env<'_>,
    task: &Task,
    session: &str,
    job: &Registration,
) -> Result<JobRow> {
    let row = register(
        store,
        env,
        task,
        session,
        &Registration {
            pid: None,
            ..job.clone()
        },
    )?;
    match restart(store, &row, &env.shell(), env.actor) {
        Ok(_) => store
            .job(row.id)?
            .with_context(|| format!("job {} vanished after it started", row.id)),
        Err(error) => {
            let reason = format!("{error:#}");
            store.set_job_state(row.id, JobState::Failed, Some(&reason), env.actor)?;
            Err(error)
        }
    }
}

/// What runs under a job's pid now, judged against the identity recorded with it; `None` for a job without a pid.
#[must_use]
pub fn liveness(job: &JobRow) -> Option<Liveness> {
    job.pid
        .map(|pid| proc::liveness(pid, job.identity.as_ref(), &job.argv))
}

/// Whether a task has a `running` job whose process is still alive (or has no process id to check).
///
/// # Errors
///
/// Fails when the store fails.
pub fn has_running_job(store: &Store, task: TaskId) -> Result<bool> {
    Ok(store
        .jobs_in(JobState::Running, Some(task))?
        .iter()
        .any(|job| liveness(job).is_none_or(|now| now.is_alive())))
}

/// Marks every running job whose process is gone, or whose pid another program now holds, as done.
///
/// # Errors
///
/// Fails when the store fails.
pub fn refresh(store: &mut Store, actor: Actor) -> Result<Vec<JobRow>> {
    let mut alive = Vec::new();
    for job in store.open_jobs(None)? {
        if job.state != JobState::Running {
            alive.push(job);
            continue;
        }
        match (job.pid, liveness(&job)) {
            (Some(pid), Some(now)) if !now.is_alive() => {
                let note = now.ended_note(pid).unwrap_or_default();
                store.set_job_state(job.id, JobState::Done, Some(&note), actor)?;
            }
            _ => alive.push(job),
        }
    }
    Ok(alive)
}

/// Ends a job's process tree when the pid still names the job's process, and never a program that took its pid over.
///
/// Returns why the process was left alone when it was: it had ended, or another program holds the pid.
///
/// # Errors
///
/// Fails when the process cannot be ended.
pub fn end_process(job: &JobRow) -> Result<Option<String>> {
    let (Some(pid), Some(now)) = (job.pid, liveness(job)) else {
        return Ok(None);
    };
    if now.is_alive() {
        proc::kill_tree(pid)?;
        Ok(None)
    } else {
        Ok(now.ended_note(pid))
    }
}

/// Ends a running job's process tree and records it as `parked` (to be restarted) or `done` (not restarted).
///
/// A job whose pid another program took over is recorded `done` and nothing is signalled: its own process ended
/// unseen, so there is nothing to park or restart.
///
/// # Errors
///
/// Fails when the process cannot be ended or the store fails.
pub fn stop(store: &mut Store, job: &JobRow, note: &str, actor: Actor) -> Result<()> {
    if let (Some(pid), Some(now)) = (job.pid, liveness(job)) {
        match now {
            Liveness::Alive => {
                store.set_job_state(job.id, JobState::Stopping, None, actor)?;
                proc::kill_tree(pid)?;
            }
            Liveness::Reused { .. } => {
                let ended = now.ended_note(pid).unwrap_or_default();
                store.set_job_state(job.id, JobState::Done, Some(&ended), actor)?;
                return Ok(());
            }
            Liveness::Gone => {}
        }
    }
    let state = if job.restart {
        JobState::Parked
    } else {
        JobState::Done
    };
    store.set_job_state(job.id, state, Some(note), actor)?;
    Ok(())
}

/// Marks an open job over without touching any process, for a job whose process ended unseen or was taken over.
///
/// # Errors
///
/// Fails when the job does not exist, the reason is empty, the job is already over, or the store fails.
pub fn forget(store: &mut Store, id: i64, reason: &str, actor: Actor) -> Result<JobRow> {
    ensure!(!reason.trim().is_empty(), "say why the job is forgotten");
    let job = store
        .job(id)?
        .with_context(|| format!("no job with id {id}"))?;
    ensure!(
        store.forget_job(id, reason.trim(), actor)?,
        "job {id} (`{}`) is already over ({})",
        job.label,
        job.state
    );
    store
        .job(id)?
        .with_context(|| format!("job {id} vanished after it was forgotten"))
}

/// Opens a job log for appending with write access, which programs built on MSYS (Git Bash) need to write to it.
///
/// Append-only handles, as `OpenOptions::append` makes them, silently lose the output of such programs.
fn open_log(path: &str) -> Result<std::fs::File> {
    use std::io::Seek;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("opening {path}"))?;
    file.seek(std::io::SeekFrom::End(0))
        .with_context(|| format!("opening {path}"))?;
    Ok(file)
}

/// The program a job runs: the configured `shell` for a bare `bash` or `sh` or WSL's `bash`, else `program` as given.
///
/// A bare `bash` makes Windows start WSL's `bash.exe` from `System32`, with another file system and no Git Bash tools.
#[must_use]
pub fn program_for(program: &str, shell: &Path) -> PathBuf {
    let path = Path::new(program);
    let bare_shell = headwork_adapters::shell::is_bare(path)
        && matches!(program, "bash" | "sh" | "bash.exe" | "sh.exe");
    if bare_shell || headwork_adapters::shell::is_wsl_bash(path) {
        shell.to_path_buf()
    } else {
        path.to_path_buf()
    }
}

/// Starts a parked job again with its stored command line, detached, appending to its log.
///
/// # Errors
///
/// Fails when the folder is gone, the log cannot be opened, or the program does not start.
pub fn restart(store: &mut Store, job: &JobRow, shell: &Path, actor: Actor) -> Result<u32> {
    let Some((program, args)) = job.argv.split_first() else {
        bail!("job `{}` has no command line", job.label);
    };
    ensure!(
        Path::new(&job.cwd).is_dir(),
        "the folder {} of job `{}` is gone",
        job.cwd,
        job.label
    );
    rotate_log(Path::new(&job.log_path), LOG_LIMIT_BYTES, LOG_KEEP);
    let log = open_log(&job.log_path)?;
    let err = log
        .try_clone()
        .with_context(|| format!("opening {}", job.log_path))?;
    let mut command = Command::new(program_for(program, shell));
    command
        .args(args)
        .current_dir(&job.cwd)
        .stdin(Stdio::null());
    let (pid, _) = proc::spawn_detached_to(&mut command, log.into(), err.into())?;
    let identity = proc::identify(pid);
    store.set_job_pid(job.id, pid, identity.as_ref(), actor)?;
    Ok(pid)
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests")]
mod tests {
    use std::path::Path;

    use super::{open_log, program_for, rotate_log};

    #[test]
    fn a_bare_shell_becomes_the_configured_one() {
        let shell = Path::new("C:/Program Files/Git/bin/bash.exe");
        for bare in ["bash", "sh", "bash.exe", "C:/Windows/System32/bash.exe"] {
            assert_eq!(program_for(bare, shell), shell, "{bare}");
        }
        for other in ["cargo", "C:/Program Files/Git/usr/bin/bash.exe", "python"] {
            assert_eq!(program_for(other, shell), Path::new(other), "{other}");
        }
    }

    #[test]
    fn the_shell_of_a_job_writes_its_output_to_the_log() {
        let shell = headwork_adapters::shell::default_shell();
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("job.log");
        let log_text = log.to_str().unwrap();
        let file = open_log(log_text).unwrap();
        let err = file.try_clone().unwrap();
        let mut command = std::process::Command::new(program_for("bash", &shell));
        command.args(["-c", "echo out-line; echo err-line 1>&2"]);
        let (pid, _) =
            headwork_adapters::proc::spawn_detached_to(&mut command, file.into(), err.into())
                .unwrap();
        let mut text = String::new();
        for _ in 0..100 {
            text = std::fs::read_to_string(&log).unwrap();
            if text.contains("err-line") && !headwork_adapters::proc::is_alive(pid).unwrap() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(
            text.contains("out-line") && text.contains("err-line"),
            "log: {text:?}"
        );
    }

    #[test]
    fn the_log_is_opened_for_writing_and_appends() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("job.log");
        std::fs::write(
            &log, "old
",
        )
        .unwrap();
        open_log(log.to_str().unwrap())
            .unwrap()
            .write_all(
                b"new
",
            )
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "old
new
",
            "appended"
        );
    }

    #[test]
    fn a_large_log_rotates_and_the_oldest_copy_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("job.log");
        std::fs::write(&log, "newest-content").unwrap();
        std::fs::write(dir.path().join("job.log.1"), "one").unwrap();
        std::fs::write(dir.path().join("job.log.2"), "two").unwrap();
        rotate_log(&log, 3, 2);
        assert!(!log.exists(), "the log moved aside");
        let read = |name: &str| std::fs::read_to_string(dir.path().join(name)).unwrap();
        assert_eq!(read("job.log.1"), "newest-content", "newest copy");
        assert_eq!(read("job.log.2"), "one", "shifted copy");
        assert!(!dir.path().join("job.log.3").exists(), "none past keep");
    }

    #[test]
    fn a_small_log_stays() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("job.log");
        std::fs::write(&log, "x").unwrap();
        rotate_log(&log, 10, 2);
        assert!(log.exists(), "under the limit");
    }
}
