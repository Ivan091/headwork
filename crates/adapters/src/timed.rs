//! Runs a program to its end or to a deadline, whichever comes first, and hands back what it printed.
//!
//! A program that outlives its deadline is ended with its whole tree, so a hung `git` or `curl` never holds up the
//! caller.

use std::io::{Read, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Why a timed run failed.
#[derive(Debug, thiserror::Error)]
pub enum TimedError {
    /// The program did not start.
    #[error("starting `{program}`: {source}")]
    Start {
        /// The program.
        program: String,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// The program ran past its deadline and was ended.
    #[error("`{program}` did not finish within {seconds} s: {output}")]
    TimedOut {
        /// The program.
        program: String,
        /// The deadline in seconds.
        seconds: u64,
        /// What it printed until then.
        output: String,
    },
}

/// What a finished program left.
#[derive(Debug, Clone)]
pub struct Ran {
    /// How it exited.
    pub status: ExitStatus,
    /// Its standard output.
    pub stdout: String,
    /// Its standard error.
    pub stderr: String,
}

impl Ran {
    /// Whether it exited successfully.
    #[must_use]
    pub fn success(&self) -> bool {
        self.status.success()
    }

    /// Its standard error and output, trimmed, for an error message.
    #[must_use]
    pub fn printed(&self) -> String {
        format!("{} {}", self.stderr.trim(), self.stdout.trim())
            .trim()
            .to_owned()
    }
}

/// Runs `command` with no input until it exits or `timeout` passes.
///
/// # Errors
///
/// Returns [`TimedError::Start`] when the program does not start and [`TimedError::TimedOut`] when it runs too long.
pub fn run(command: &mut Command, timeout: Duration) -> Result<Ran, TimedError> {
    run_fed(command, None, timeout)
}

/// Runs `command` with `input` on its standard input until it exits or `timeout` passes. The input never shows on the
/// command line, so a secret such as a login token stays out of the process list.
///
/// # Errors
///
/// Returns [`TimedError::Start`] when the program does not start and [`TimedError::TimedOut`] when it runs too long.
pub fn run_with_input(
    command: &mut Command,
    input: Vec<u8>,
    timeout: Duration,
) -> Result<Ran, TimedError> {
    run_fed(command, Some(input), timeout)
}

fn run_fed(
    command: &mut Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<Ran, TimedError> {
    let program = command.get_program().to_string_lossy().into_owned();
    let start_error = |source| TimedError::Start {
        program: program.clone(),
        source,
    };
    let stdin = if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    let mut child = command
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(start_error)?;
    if let (Some(input), Some(mut pipe)) = (input, child.stdin.take()) {
        std::thread::spawn(move || drop(pipe.write_all(&input)));
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(start_error)? {
            break Some(status);
        }
        if started.elapsed() >= timeout {
            drop(crate::proc::kill_tree(child.id()));
            drop(child.kill());
            drop(child.wait());
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let grace = Duration::from_secs(2);
    let stdout = stdout.recv_timeout(grace).unwrap_or_default();
    let stderr = stderr.recv_timeout(grace).unwrap_or_default();
    match status {
        Some(status) => Ok(Ran {
            status,
            stdout,
            stderr,
        }),
        None => Err(TimedError::TimedOut {
            program,
            seconds: timeout.as_secs(),
            output: format!("{} {}", stderr.trim(), stdout.trim())
                .trim()
                .to_owned(),
        }),
    }
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<String> {
    let (send, receive) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            drop(pipe.read_to_end(&mut bytes));
            drop(send.send(String::from_utf8_lossy(&bytes).into_owned()));
        });
    }
    receive
}
