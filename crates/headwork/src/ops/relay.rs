//! The MCP relay: what `headwork mcp` runs, so a session's MCP server picks up a newly installed build without a
//! restart (docs/hot-reload.md).
//!
//! Claude starts the relay once per session and keeps it for the whole session, so the relay itself stays as small as
//! possible: it starts `headwork mcp --serve` (the real server, [`super::mcp::Server`]) from the program file and
//! passes messages through line by line. Before each request it compares that file's [`Stamp`] with the one the
//! inner server started from. When the file changed, it starts a new inner server, replays the session's
//! `initialize` handshake to it, retires the old one, and after the response tells Claude the tool list changed. A new
//! server that does not start leaves the old one serving.
//!
//! Every later build must keep accepting `mcp --serve`, since relays of older builds start it.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

use super::install::Stamp;

/// The flag that makes `headwork mcp` the server instead of the relay.
pub const SERVE_FLAG: &str = "--serve";

/// How long a retired inner server may take to finish before it is killed.
const RETIRE_GRACE: Duration = Duration::from_secs(10);

/// What a line from Claude is, as far as the relay cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    /// The `initialize` request, which opens the session.
    Initialize,
    /// Any other request: it gets exactly one response line.
    Request,
    /// A notification or a response: it gets none.
    Notification,
    /// Not JSON: the server answers with a parse error.
    Unreadable,
}

impl Message {
    /// Whether the server writes one line back.
    #[must_use]
    pub const fn answered(self) -> bool {
        !matches!(self, Self::Notification)
    }
}

/// Sorts a line the way [`super::mcp::Server`] treats it: a message with an id and a method is answered.
#[must_use]
pub fn classify(line: &str) -> Message {
    let Ok(message) = serde_json::from_str::<Value>(line) else {
        return Message::Unreadable;
    };
    let method = message.get("method").and_then(Value::as_str);
    match (message.get("id").is_some(), method) {
        (true, Some("initialize")) => Message::Initialize,
        (true, Some(_)) => Message::Request,
        _ => Message::Notification,
    }
}

fn is_initialized(line: &str) -> bool {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|message| {
            message
                .get("method")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|method| method == "notifications/initialized")
}

fn error_reply(line: &str, message: &str) -> Value {
    let id = serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|request| request.get("id").cloned())
        .unwrap_or(Value::Null);
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32603, "message": message } })
}

/// The notification that makes Claude list the tools again.
#[must_use]
pub fn list_changed() -> Value {
    json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" })
}

#[derive(Debug)]
struct Inner {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    stamp: Option<Stamp>,
}

impl Inner {
    fn start(exe: &Path, handshake: &[String]) -> Result<Self> {
        let stamp = Stamp::of(exe).ok();
        let mut child = Command::new(exe)
            .args(["mcp", SERVE_FLAG])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("starting {} mcp {SERVE_FLAG}", exe.display()))?;
        let input = child.stdin.take().context("the server's stdin")?;
        let output = BufReader::new(child.stdout.take().context("the server's stdout")?);
        let mut inner = Self {
            child,
            input,
            output,
            stamp,
        };
        for line in handshake {
            if let Err(error) = inner.replay(line) {
                inner.retire();
                return Err(error);
            }
        }
        Ok(inner)
    }

    fn replay(&mut self, line: &str) -> Result<()> {
        if let Some(reply) = self.exchange(line, classify(line))?
            && serde_json::from_str::<Value>(&reply)
                .map_or(true, |reply| reply.get("error").is_some())
        {
            return Err(anyhow!("the new server refused the handshake: {reply}"));
        }
        Ok(())
    }

    fn exchange(&mut self, line: &str, kind: Message) -> Result<Option<String>> {
        writeln!(self.input, "{line}").context("writing to the server")?;
        self.input.flush().context("flushing to the server")?;
        if !kind.answered() {
            return Ok(None);
        }
        let mut reply = String::new();
        let read = self
            .output
            .read_line(&mut reply)
            .context("reading from the server")?;
        if read == 0 {
            return Err(anyhow!("the server ended"));
        }
        Ok(Some(reply.trim_end().to_owned()))
    }

    fn retire(self) {
        let Self {
            mut child, input, ..
        } = self;
        drop(input);
        let deadline = Instant::now() + RETIRE_GRACE;
        while Instant::now() < deadline {
            if !matches!(child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if child.kill().is_ok() {
            let _reaped: std::io::Result<std::process::ExitStatus> = child.wait();
        }
    }
}

/// The relay of one session.
#[derive(Debug)]
pub struct Relay {
    exe: PathBuf,
    inner: Option<Inner>,
    handshake: Vec<String>,
}

impl Relay {
    /// A relay that starts its servers from the program at `exe`.
    #[must_use]
    pub const fn new(exe: PathBuf) -> Self {
        Self {
            exe,
            inner: None,
            handshake: Vec::new(),
        }
    }

    /// Passes messages from `input` to the inner server and its answers to `output` until `input` ends.
    ///
    /// # Errors
    ///
    /// Fails when reading `input` or writing `output` fails; a failing inner server is answered with an error and
    /// started again at the next request.
    pub fn serve(&mut self, input: impl BufRead, mut output: impl Write) -> Result<()> {
        for line in input.lines() {
            let line = line.context("reading a message")?;
            if line.trim().is_empty() {
                continue;
            }
            let kind = classify(&line);
            if kind == Message::Initialize {
                self.handshake.clear();
                if let Some(inner) = self.inner.take() {
                    inner.retire();
                }
            }
            let swapped = self.refresh(kind == Message::Request);
            let reply = self.inner.as_mut().map_or_else(
                || Err(anyhow!("the headwork MCP server did not start")),
                |inner| inner.exchange(&line, kind),
            );
            let reply = match reply {
                Ok(reply) => reply,
                Err(error) => {
                    if let Some(inner) = self.inner.take() {
                        inner.retire();
                    }
                    kind.answered()
                        .then(|| error_reply(&line, &format!("{error:#}")).to_string())
                }
            };
            if kind == Message::Initialize || is_initialized(&line) {
                self.handshake.push(line);
            }
            if let Some(reply) = reply {
                writeln!(output, "{reply}").context("writing a response")?;
            }
            if swapped {
                writeln!(output, "{}", list_changed()).context("writing a notification")?;
            }
            output.flush().context("flushing a response")?;
        }
        if let Some(inner) = self.inner.take() {
            inner.retire();
        }
        Ok(())
    }

    /// Starts an inner server when there is none, or a new one when `check` is set and the program file changed.
    /// Returns whether a running server was replaced.
    fn refresh(&mut self, check: bool) -> bool {
        let Some(current) = self.inner.as_ref() else {
            self.inner = Inner::start(&self.exe, &self.handshake).ok();
            return false;
        };
        if !check {
            return false;
        }
        let Ok(now) = Stamp::of(&self.exe) else {
            return false;
        };
        if current.stamp == Some(now) {
            return false;
        }
        let Ok(fresh) = Inner::start(&self.exe, &self.handshake) else {
            return false;
        };
        if let Some(old) = self.inner.replace(fresh) {
            old.retire();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_classified_like_the_server_answers_them() {
        for (line, kind) in [
            (
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
                Message::Initialize,
            ),
            (
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
                Message::Request,
            ),
            (
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                Message::Notification,
            ),
            (
                r#"{"jsonrpc":"2.0","id":3,"result":{}}"#,
                Message::Notification,
            ),
            ("[1, 2]", Message::Notification),
            ("not json", Message::Unreadable),
        ] {
            assert_eq!(classify(line), kind, "{line}");
        }
        assert!(
            is_initialized(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            "the handshake's end"
        );
        assert!(
            !Message::Notification.answered(),
            "notifications get no answer"
        );
        assert!(Message::Unreadable.answered(), "garbage gets a parse error");
    }

    #[test]
    fn an_error_reply_keeps_the_request_id() {
        let reply = error_reply(r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#, "down");
        assert_eq!(reply["id"], 7, "{reply}");
        assert_eq!(reply["error"]["message"], "down", "{reply}");
        assert_eq!(
            error_reply("garbage", "down")["id"],
            Value::Null,
            "no id to keep"
        );
    }
}
