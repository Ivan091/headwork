//! Token usage read from Claude Code transcripts (`docs/usage.md`): one transcript line in, the numbers out.
//!
//! Every assistant line of a transcript carries the API response's `usage`. A response with several content blocks is
//! written once per block with the same numbers, so the response's id is the key that makes every read idempotent.

use std::ops::AddAssign;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::vocabulary::{Role, UsageRole};

/// The model Claude Code names on lines it wrote itself (an interrupted turn, an error), which spent nothing.
const SYNTHETIC_MODEL: &str = "<synthetic>";

/// The folder Claude Code writes a session's subagent transcripts under: `<session id>/subagents/`.
const SUBAGENTS_DIR: &str = "subagents";

/// The prefix of a subagent transcript's file name: `agent-<id>.jsonl`.
const AGENT_PREFIX: &str = "agent-";

/// The token counts of one response, or the sum of many.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Tokens {
    /// Uncached input.
    pub input: u64,
    /// Cache writes that live five minutes.
    pub cache_write_5m: u64,
    /// Cache writes that live an hour.
    pub cache_write_1h: u64,
    /// Cache reads.
    pub cache_read: u64,
    /// Output, thinking included.
    pub output: u64,
    /// The thinking part of the output.
    pub thinking: u64,
    /// Server web searches, as requests.
    pub web_searches: u64,
    /// Server web fetches, as requests.
    pub web_fetches: u64,
}

impl Tokens {
    /// Every token, input of each kind plus output; web requests are not tokens and thinking is inside output.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.input
            .saturating_add(self.cache_write_5m)
            .saturating_add(self.cache_write_1h)
            .saturating_add(self.cache_read)
            .saturating_add(self.output)
    }

    /// Cache reads per thousand input tokens of every kind, none when there was no input.
    #[must_use]
    pub const fn cache_hit_permille(&self) -> Option<u64> {
        let all_input = self
            .input
            .saturating_add(self.cache_write_5m)
            .saturating_add(self.cache_write_1h)
            .saturating_add(self.cache_read);
        if all_input == 0 {
            return None;
        }
        Some(self.cache_read.saturating_mul(1000) / all_input)
    }
}

impl AddAssign for Tokens {
    fn add_assign(&mut self, other: Self) {
        self.input = self.input.saturating_add(other.input);
        self.cache_write_5m = self.cache_write_5m.saturating_add(other.cache_write_5m);
        self.cache_write_1h = self.cache_write_1h.saturating_add(other.cache_write_1h);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.output = self.output.saturating_add(other.output);
        self.thinking = self.thinking.saturating_add(other.thinking);
        self.web_searches = self.web_searches.saturating_add(other.web_searches);
        self.web_fetches = self.web_fetches.saturating_add(other.web_fetches);
    }
}

/// What one response spent, read from its transcript line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageLine {
    /// The API response's id (`message.id`, else `requestId`, else the line's `uuid`).
    pub message_id: String,
    /// When the line was written, as the transcript gives it (RFC 3339, UTC).
    pub at: String,
    /// The model that answered.
    pub model: String,
    /// The turn's effort level.
    pub effort: Option<String>,
    /// `standard` or `fast`.
    pub speed: Option<String>,
    /// The service tier.
    pub service_tier: Option<String>,
    /// The session's folder.
    pub cwd: Option<String>,
    /// The numbers.
    pub tokens: Tokens,
}

fn count(value: &Value, path: &[&str]) -> u64 {
    path.iter()
        .try_fold(value, |value, key| value.get(key))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn text(value: &Value, path: &[&str]) -> Option<String> {
    path.iter()
        .try_fold(value, |value, key| value.get(key))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Reads one transcript line: the numbers of an assistant response, or none for any other line, a line without usage,
/// a line Claude Code wrote itself, or a half-written last line.
#[must_use]
pub fn parse_line(line: &str) -> Option<UsageLine> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let message = value.get("message")?;
    let usage = message.get("usage")?.as_object()?;
    let usage = Value::Object(usage.clone());
    let model = text(message, &["model"])?;
    if model == SYNTHETIC_MODEL {
        return None;
    }
    let message_id = text(message, &["id"])
        .or_else(|| text(&value, &["requestId"]))
        .or_else(|| text(&value, &["uuid"]))?;
    let at = text(&value, &["timestamp"])?;
    let cache_writes = if usage.get("cache_creation").is_some_and(Value::is_object) {
        [
            count(&usage, &["cache_creation", "ephemeral_5m_input_tokens"]),
            count(&usage, &["cache_creation", "ephemeral_1h_input_tokens"]),
        ]
    } else {
        [count(&usage, &["cache_creation_input_tokens"]), 0]
    };
    Some(UsageLine {
        message_id,
        at,
        model,
        effort: text(&value, &["effort"]),
        speed: text(&usage, &["speed"]),
        service_tier: text(&usage, &["service_tier"]),
        cwd: text(&value, &["cwd"]),
        tokens: Tokens {
            input: count(&usage, &["input_tokens"]),
            cache_write_5m: cache_writes[0],
            cache_write_1h: cache_writes[1],
            cache_read: count(&usage, &["cache_read_input_tokens"]),
            output: count(&usage, &["output_tokens"]),
            thinking: count(&usage, &["output_tokens_details", "thinking_tokens"]),
            web_searches: count(&usage, &["server_tool_use", "web_search_requests"]),
            web_fetches: count(&usage, &["server_tool_use", "web_fetch_requests"]),
        },
    })
}

/// Whose transcript a file is, told by its path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptFile {
    /// The Claude session id: the file's name, or for a subagent the folder above `subagents`.
    pub session_id: String,
    /// The subagent's id, from `agent-<id>.jsonl`; none for the session's own transcript.
    pub agent_id: Option<String>,
}

/// Tells whose transcript `path` is: `<folder key>/<session id>.jsonl`, or a subagent's
/// `<folder key>/<session id>/subagents/[workflows/<run>/]agent-<id>.jsonl`. None for any other file.
#[must_use]
pub fn transcript_of(path: &Path) -> Option<TranscriptFile> {
    if path.extension()? != "jsonl" {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let parts: Vec<&str> = path
        .parent()?
        .components()
        .filter_map(|part| part.as_os_str().to_str())
        .collect();
    match stem.strip_prefix(AGENT_PREFIX) {
        Some(agent) => {
            let below = parts.iter().rposition(|part| *part == SUBAGENTS_DIR)?;
            let session = parts.get(below.checked_sub(1)?)?;
            Some(TranscriptFile {
                session_id: (*session).to_owned(),
                agent_id: Some(agent.to_owned()),
            })
        }
        None => Some(TranscriptFile {
            session_id: stem.to_owned(),
            agent_id: None,
        }),
    }
}

/// The subagent type in an `agent-<id>.meta.json` file (`agentType`), none when the file says none.
#[must_use]
pub fn agent_type(meta: &str) -> Option<String> {
    let value: Value = serde_json::from_str(meta).ok()?;
    text(&value, &["agentType"])
}

/// How many leading bytes of `bytes` are whole lines: up to and including the last newline.
#[must_use]
pub fn complete_len(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |at| at.saturating_add(1))
}

/// The usage role of a session headwork knows: the orchestrator, or by its task's shape a lead, a part or a worker.
#[must_use]
pub const fn usage_role(session: Role, lead: bool, part: bool) -> UsageRole {
    match session {
        Role::Orchestrator => UsageRole::Orchestrator,
        Role::Worker if lead => UsageRole::Lead,
        Role::Worker if part => UsageRole::Part,
        Role::Worker => UsageRole::Worker,
    }
}

/// Shortens a token count for a narrow column: `950`, `12k`, `1.2M`, `3.4B`.
#[must_use]
pub fn short_count(count: u64) -> String {
    const UNITS: [(u64, &str); 3] = [(1_000_000_000, "B"), (1_000_000, "M"), (1_000, "k")];
    for (size, unit) in UNITS {
        if count >= size {
            let tenths = count / (size / 10);
            return if tenths >= 100 {
                format!("{}{unit}", tenths / 10)
            } else {
                format!("{}.{}{unit}", tenths / 10, tenths % 10)
            };
        }
    }
    count.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESPONSE: &str = r#"{"parentUuid":"p","isSidechain":false,"message":{"model":"claude-opus-5-5","id":"msg_1","type":"message","role":"assistant","content":[],"usage":{"input_tokens":2,"cache_creation_input_tokens":34277,"cache_read_input_tokens":35483,"output_tokens":75,"output_tokens_details":{"thinking_tokens":20},"server_tool_use":{"web_search_requests":1,"web_fetch_requests":2},"service_tier":"standard","cache_creation":{"ephemeral_1h_input_tokens":34000,"ephemeral_5m_input_tokens":277},"speed":"standard"}},"requestId":"req_1","type":"assistant","uuid":"u1","timestamp":"2026-11-22T20:03:15.027Z","effort":"medium","sessionId":"s1","cwd":"S:\\Code\\x"}"#;

    #[test]
    fn a_response_line_gives_every_field() {
        let line = parse_line(RESPONSE);
        assert_eq!(
            line,
            Some(UsageLine {
                message_id: "msg_1".to_owned(),
                at: "2026-11-22T20:03:15.027Z".to_owned(),
                model: "claude-opus-5-5".to_owned(),
                effort: Some("medium".to_owned()),
                speed: Some("standard".to_owned()),
                service_tier: Some("standard".to_owned()),
                cwd: Some("S:\\Code\\x".to_owned()),
                tokens: Tokens {
                    input: 2,
                    cache_write_5m: 277,
                    cache_write_1h: 34000,
                    cache_read: 35483,
                    output: 75,
                    thinking: 20,
                    web_searches: 1,
                    web_fetches: 2,
                },
            }),
            "every field read"
        );
    }

    #[test]
    fn lines_without_a_response_give_none() {
        let cases = [
            (
                "user line",
                r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
            ),
            (
                "no usage",
                r#"{"type":"assistant","message":{"model":"m","id":"x"},"timestamp":"t"}"#,
            ),
            (
                "synthetic",
                r#"{"type":"assistant","message":{"model":"<synthetic>","id":"x","usage":{"input_tokens":0}},"timestamp":"t"}"#,
            ),
            (
                "half written",
                r#"{"type":"assistant","message":{"model":"claude"#,
            ),
            ("not json", "garbage"),
            ("empty", ""),
            (
                "no time",
                r#"{"type":"assistant","message":{"model":"m","id":"x","usage":{"input_tokens":1}}}"#,
            ),
        ];
        for (name, line) in cases {
            assert_eq!(parse_line(line), None, "{name}");
        }
    }

    #[test]
    fn missing_fields_count_as_zero_and_old_cache_writes_are_five_minute() {
        let line = parse_line(
            r#"{"type":"assistant","requestId":"req_9","timestamp":"t","message":{"model":"m","usage":{"input_tokens":5,"cache_creation_input_tokens":7,"output_tokens":3}}}"#,
        );
        let line = line.unwrap_or_else(|| unreachable!("the line parses"));
        assert_eq!(
            line.message_id, "req_9",
            "request id stands in for the message id"
        );
        assert_eq!(
            line.tokens,
            Tokens {
                input: 5,
                cache_write_5m: 7,
                output: 3,
                ..Tokens::default()
            },
            "missing counts are zero"
        );
        assert_eq!(line.effort, None, "no effort");
        assert_eq!(line.cwd, None, "no folder");
    }

    #[test]
    fn copies_of_one_response_share_the_id() {
        let other_block = RESPONSE.replace("\"uuid\":\"u1\"", "\"uuid\":\"u2\"");
        let first = parse_line(RESPONSE).map(|line| line.message_id);
        let second = parse_line(&other_block).map(|line| line.message_id);
        assert_eq!(first, second, "one key for both blocks");
    }

    #[test]
    fn sidechain_lines_are_read_like_any_other() {
        let sidechain = RESPONSE.replace("\"isSidechain\":false", "\"isSidechain\":true");
        assert!(
            parse_line(&sidechain).is_some(),
            "a sidechain response counts"
        );
    }

    #[test]
    fn transcript_paths_tell_the_session_and_agent() {
        let cases = [
            ("p/k/0a.jsonl", Some(("0a", None))),
            ("p/k/0a/subagents/agent-a1.jsonl", Some(("0a", Some("a1")))),
            (
                "p/k/0a/subagents/workflows/wf_1/agent-a2.jsonl",
                Some(("0a", Some("a2"))),
            ),
            ("p/k/0a/subagents/agent-a1.meta.json", None),
            ("agent-a1.jsonl", None),
            ("p/k/notes.txt", None),
        ];
        for (path, expected) in cases {
            let found = transcript_of(Path::new(path));
            let expected = expected.map(|(session, agent)| TranscriptFile {
                session_id: session.to_owned(),
                agent_id: agent.map(str::to_owned),
            });
            assert_eq!(found, expected, "{path}");
        }
    }

    #[test]
    fn agent_meta_gives_its_type() {
        assert_eq!(
            agent_type(r#"{"agentType":"Explore","model":"haiku"}"#),
            Some("Explore".to_owned()),
            "type read"
        );
        assert_eq!(agent_type("{}"), None, "no type");
        assert_eq!(agent_type("nope"), None, "not json");
    }

    #[test]
    fn only_whole_lines_are_taken() {
        assert_eq!(complete_len(b"a\nb\nc"), 4, "the half line is left");
        assert_eq!(complete_len(b"a\n"), 2, "all whole");
        assert_eq!(complete_len(b"abc"), 0, "nothing whole");
        assert_eq!(complete_len(b""), 0, "empty");
    }

    #[test]
    fn roles_follow_the_task_shape() {
        assert_eq!(
            usage_role(Role::Orchestrator, false, false),
            UsageRole::Orchestrator,
            "orchestrator"
        );
        assert_eq!(
            usage_role(Role::Worker, true, false),
            UsageRole::Lead,
            "lead"
        );
        assert_eq!(
            usage_role(Role::Worker, false, true),
            UsageRole::Part,
            "part"
        );
        assert_eq!(
            usage_role(Role::Worker, false, false),
            UsageRole::Worker,
            "worker"
        );
    }

    #[test]
    fn sums_and_ratios() {
        let mut sum = Tokens {
            input: 10,
            cache_read: 90,
            output: 5,
            ..Tokens::default()
        };
        sum += Tokens {
            cache_write_1h: 100,
            output: 5,
            thinking: 2,
            ..Tokens::default()
        };
        assert_eq!(sum.total(), 210, "total");
        assert_eq!(sum.cache_hit_permille(), Some(450), "90 of 200 input");
        assert_eq!(Tokens::default().cache_hit_permille(), None, "no input");
    }

    #[test]
    fn counts_shorten() {
        let cases = [
            (0, "0"),
            (950, "950"),
            (1_000, "1.0k"),
            (12_345, "12k"),
            (1_234_567, "1.2M"),
            (3_400_000_000, "3.4B"),
        ];
        for (count, expected) in cases {
            assert_eq!(short_count(count), expected, "{count}");
        }
    }
}
