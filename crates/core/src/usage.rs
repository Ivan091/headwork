//! Token usage read from Claude Code transcripts (`docs/usage.md`): one transcript line in, the numbers out.
//!
//! Every assistant line of a transcript carries the API response's `usage`. A response with several content blocks is
//! written once per block with the same numbers, so the response's id is the key that makes every read idempotent.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::AddAssign;
use std::path::Path;

use serde::{Deserialize, Serialize};
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

/// What a subagent's `agent-<id>.meta.json` says about it: its type, the short label its caller gave it, the model it
/// asked for and how deep it was nested.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentMeta {
    /// Such as `general-purpose` or `Explore`.
    pub agent_type: Option<String>,
    /// The `Agent` call's description, never its prompt.
    pub description: Option<String>,
    /// The model it asked for.
    pub model: Option<String>,
    /// 1 for a session's own subagent, more for nested ones.
    pub spawn_depth: Option<i64>,
}

/// The longest subagent id the agent graph records.
const AGENT_ID_MAX: usize = 64;

/// Whether a transcript's agent id is one Claude Code writes (letters, digits, `-` and `_`, at most 64), so it can
/// stand as a row key and a graph node id.
#[must_use]
pub fn is_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= AGENT_ID_MAX
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Reads a subagent's meta file; none when it is not a JSON object.
#[must_use]
pub fn agent_meta(meta: &str) -> Option<AgentMeta> {
    let value: Value = serde_json::from_str(meta).ok()?;
    value.is_object().then(|| AgentMeta {
        agent_type: text(&value, &["agentType"]),
        description: text(&value, &["description"]),
        model: text(&value, &["model"]),
        spawn_depth: value.get("spawnDepth").and_then(Value::as_i64),
    })
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

/// Millionths of a dollar per million tokens: a list price such as `15.0`, kept as an integer so the config stays
/// comparable and sums stay exact.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Price(u64);

/// The most a price may be, in dollars per million tokens; anything above is a typo.
const MAX_PRICE: f64 = 1_000_000.0;

impl Price {
    /// A price in millionths of a dollar per million tokens.
    #[must_use]
    pub const fn from_micros(micros: u64) -> Self {
        Self(micros)
    }

    /// Millionths of a dollar per million tokens.
    #[must_use]
    pub const fn micros(self) -> u64 {
        self.0
    }

    /// Millionths of a dollar for `count` tokens at this price.
    #[must_use]
    pub fn of(self, count: u64) -> u64 {
        let micros = u128::from(count).saturating_mul(u128::from(self.0)) / 1_000_000;
        u64::try_from(micros).unwrap_or(u64::MAX)
    }
}

/// A price that is not a number of dollars from 0 to a million.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a price must be dollars per million tokens, from 0 to 1000000")]
pub struct BadPrice;

impl TryFrom<f64> for Price {
    type Error = BadPrice;

    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is checked to be finite and within 0..=1e12 before the cast"
    )]
    fn try_from(dollars: f64) -> Result<Self, Self::Error> {
        if !dollars.is_finite() || !(0.0..=MAX_PRICE).contains(&dollars) {
            return Err(BadPrice);
        }
        Ok(Self((dollars * 1_000_000.0).round() as u64))
    }
}

impl From<Price> for f64 {
    fn from(price: Price) -> Self {
        let whole = u32::try_from(price.0 / 1_000_000).unwrap_or(u32::MAX);
        let part = u32::try_from(price.0 % 1_000_000).unwrap_or(0);
        Self::from(whole) + Self::from(part) / 1_000_000.0
    }
}

/// `[usage.prices."<model>"]`: dollars per million tokens of each type, and per thousand web searches.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ModelPrices {
    /// Uncached input.
    pub input: Price,
    /// Five-minute cache writes.
    pub cache_write_5m: Price,
    /// One-hour cache writes.
    pub cache_write_1h: Price,
    /// Cache reads.
    pub cache_read: Price,
    /// Output, thinking included.
    pub output: Price,
    /// Dollars per thousand web searches.
    pub web_search_per_1000: Price,
}

impl ModelPrices {
    /// Millionths of a dollar for `tokens` at these prices.
    #[must_use]
    pub fn cost(&self, tokens: &Tokens) -> u64 {
        let searches = u128::from(tokens.web_searches)
            .saturating_mul(u128::from(self.web_search_per_1000.micros()))
            / 1000;
        [
            self.input.of(tokens.input),
            self.cache_write_5m.of(tokens.cache_write_5m),
            self.cache_write_1h.of(tokens.cache_write_1h),
            self.cache_read.of(tokens.cache_read),
            self.output.of(tokens.output),
            u64::try_from(searches).unwrap_or(u64::MAX),
        ]
        .into_iter()
        .fold(0, u64::saturating_add)
    }
}

/// `[usage]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct UsageConfig {
    /// Days the per-response rows stay before the tick sums them per day.
    pub keep_days: u32,
    /// Prices by model name or model name prefix.
    pub prices: BTreeMap<String, ModelPrices>,
}

impl Default for UsageConfig {
    fn default() -> Self {
        Self {
            keep_days: 90,
            prices: BTreeMap::new(),
        }
    }
}

impl UsageConfig {
    /// The prices of `model`: its own entry, else the longest configured prefix of its name (`claude-opus` covers every
    /// Opus).
    #[must_use]
    pub fn price_for(&self, model: &str) -> Option<&ModelPrices> {
        self.prices.get(model).or_else(|| {
            self.prices
                .iter()
                .filter(|(name, _)| model.starts_with(name.as_str()))
                .max_by_key(|(name, _)| name.len())
                .map(|(_, prices)| prices)
        })
    }

    /// The estimate for tokens spent by several models.
    #[must_use]
    pub fn estimate<'a>(
        &self,
        by_model: impl IntoIterator<Item = (&'a str, &'a Tokens)>,
    ) -> Estimate {
        let mut estimate = Estimate::default();
        for (model, tokens) in by_model {
            match self.price_for(model) {
                Some(prices) => {
                    estimate.micros = estimate.micros.saturating_add(prices.cost(tokens));
                }
                None if tokens.total() > 0 => estimate.unpriced.push(model.to_owned()),
                None => {}
            }
        }
        estimate.unpriced.sort();
        estimate.unpriced.dedup();
        estimate
    }
}

/// An estimate at API list prices, never a bill.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Estimate {
    /// Millionths of a dollar, for the priced models.
    pub micros: u64,
    /// Models that spent tokens and have no price.
    pub unpriced: Vec<String>,
}

impl Estimate {
    /// Adds another estimate.
    pub fn add(&mut self, other: &Self) {
        self.micros = self.micros.saturating_add(other.micros);
        self.unpriced.extend(other.unpriced.iter().cloned());
        self.unpriced.sort();
        self.unpriced.dedup();
    }
}

impl fmt::Display for Estimate {
    /// `$4.10`; `?` when nothing was priced but something spent; `$4.10+?` when part was not priced.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cents = self.micros.saturating_add(5000) / 10_000;
        match (self.micros, self.unpriced.is_empty()) {
            (0, false) => f.write_str("?"),
            (_, true) => write!(f, "${}.{:02}", cents / 100, cents % 100),
            (_, false) => write!(f, "${}.{:02}+?", cents / 100, cents % 100),
        }
    }
}

/// The `claude` flags that pick the session's id themselves: a helper started with one keeps that session's own tag.
const SESSION_FLAGS: [&str; 5] = ["--session-id", "--resume", "-r", "--continue", "-c"];

/// Whether `argv` starts a fresh `claude` session that a `--session-id` can name: its program is `claude` (any folder,
/// any extension) and no flag already picks the session.
#[must_use]
pub fn fresh_claude(argv: &[String]) -> bool {
    let Some(program) = argv.first() else {
        return false;
    };
    let is_claude = Path::new(program)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("claude"));
    is_claude
        && !argv.iter().skip(1).any(|arg| {
            SESSION_FLAGS.iter().any(|flag| {
                arg == flag
                    || arg
                        .strip_prefix(flag)
                        .is_some_and(|rest| rest.starts_with('='))
            })
        })
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

    fn response() -> String {
        serde_json::json!({
            "parentUuid": "p",
            "isSidechain": false,
            "message": {
                "model": "claude-opus-5-5",
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "content": [],
                "usage": {
                    "input_tokens": 2,
                    "cache_creation_input_tokens": 34277,
                    "cache_read_input_tokens": 35483,
                    "output_tokens": 75,
                    "output_tokens_details": { "thinking_tokens": 20 },
                    "server_tool_use": { "web_search_requests": 1, "web_fetch_requests": 2 },
                    "service_tier": "standard",
                    "cache_creation": { "ephemeral_1h_input_tokens": 34000, "ephemeral_5m_input_tokens": 277 },
                    "speed": "standard"
                }
            },
            "requestId": "req_1",
            "type": "assistant",
            "uuid": "u1",
            "timestamp": "2026-11-22T20:03:15.027Z",
            "effort": "medium",
            "sessionId": "s1",
            "cwd": r"S:\Code\x"
        })
        .to_string()
    }

    #[test]
    fn a_response_line_gives_every_field() {
        let line = parse_line(&response());
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
        let line = parse_line(concat!(
            r#"{"type":"assistant","requestId":"req_9","timestamp":"t","message":{"model":"m","#,
            r#""usage":{"input_tokens":5,"cache_creation_input_tokens":7,"output_tokens":3}}}"#
        ));
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
        let other_block = response().replace("\"uuid\":\"u1\"", "\"uuid\":\"u2\"");
        let first = parse_line(&response()).map(|line| line.message_id);
        let second = parse_line(&other_block).map(|line| line.message_id);
        assert_eq!(first, second, "one key for both blocks");
    }

    #[test]
    fn sidechain_lines_are_read_like_any_other() {
        let sidechain = response().replace("\"isSidechain\":false", "\"isSidechain\":true");
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
    fn agent_ids_are_short_plain_words() {
        let table = [
            ("a0e783e096e8bfe7b", true),
            ("acompact-1_x", true),
            ("", false),
            ("a/b", false),
            ("a b", false),
            ("é", false),
        ];
        for (id, ok) in table {
            assert_eq!(is_agent_id(id), ok, "{id:?}");
        }
        assert!(!is_agent_id(&"a".repeat(65)), "too long");
    }

    #[test]
    fn agent_meta_reads_type_label_model_and_depth() {
        let meta = agent_meta(
            r#"{"agentType":"general-purpose","description":"Build the graph","model":"opus","spawnDepth":2}"#,
        );
        assert_eq!(
            meta,
            Some(AgentMeta {
                agent_type: Some("general-purpose".to_owned()),
                description: Some("Build the graph".to_owned()),
                model: Some("opus".to_owned()),
                spawn_depth: Some(2),
            }),
            "every field"
        );
        assert_eq!(
            agent_meta("{}"),
            Some(AgentMeta::default()),
            "an empty object"
        );
        assert_eq!(agent_meta("[1]"), None, "not an object");
        assert_eq!(agent_meta("nope"), None, "not json");
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

    fn prices(input: f64, output: f64) -> ModelPrices {
        ModelPrices {
            input: Price::try_from(input).unwrap_or_default(),
            output: Price::try_from(output).unwrap_or_default(),
            cache_read: Price::try_from(1.5).unwrap_or_default(),
            web_search_per_1000: Price::try_from(10.0).unwrap_or_default(),
            ..ModelPrices::default()
        }
    }

    #[test]
    fn prices_are_read_and_bad_ones_refused() {
        assert_eq!(
            Price::try_from(15.0).map(Price::micros),
            Ok(15_000_000),
            "whole"
        );
        assert_eq!(
            Price::try_from(0.3).map(Price::micros),
            Ok(300_000),
            "fraction"
        );
        assert_eq!(Price::try_from(-1.0), Err(BadPrice), "negative");
        assert_eq!(Price::try_from(f64::NAN), Err(BadPrice), "not a number");
        assert_eq!(Price::try_from(2e6), Err(BadPrice), "a typo");
        assert!(
            (f64::from(Price::from_micros(3_750_000)) - 3.75).abs() < 1e-9,
            "back to dollars"
        );
    }

    #[test]
    fn costs_sum_every_type() {
        let tokens = Tokens {
            input: 1_000_000,
            cache_read: 2_000_000,
            output: 100_000,
            web_searches: 3,
            ..Tokens::default()
        };
        assert_eq!(
            prices(15.0, 75.0).cost(&tokens),
            15_000_000 + 3_000_000 + 7_500_000 + 30_000,
            "every type"
        );
    }

    #[test]
    fn models_match_exactly_then_by_the_longest_prefix() {
        let config = UsageConfig {
            keep_days: 90,
            prices: [
                ("claude".to_owned(), prices(1.0, 1.0)),
                ("claude-opus".to_owned(), prices(15.0, 75.0)),
                ("claude-opus-5-5".to_owned(), prices(5.0, 25.0)),
            ]
            .into(),
        };
        let input = |model: &str| config.price_for(model).map(|prices| prices.input.micros());
        assert_eq!(input("claude-opus-5-5"), Some(5_000_000), "exact");
        assert_eq!(input("claude-opus-4-1"), Some(15_000_000), "longest prefix");
        assert_eq!(input("claude-haiku-5-5"), Some(1_000_000), "shorter prefix");
        assert_eq!(input("gpt"), None, "none");
    }

    #[test]
    fn estimates_list_unpriced_models_and_print_dollars() {
        let config = UsageConfig {
            keep_days: 90,
            prices: [("claude-opus".to_owned(), prices(15.0, 75.0))].into(),
        };
        let spent = Tokens {
            input: 100_000,
            ..Tokens::default()
        };
        let priced = config.estimate([("claude-opus-5-5", &spent)]);
        assert_eq!(priced.to_string(), "$1.50", "priced");
        let mixed = config.estimate([("claude-opus-5-5", &spent), ("claude-haiku-5-5", &spent)]);
        assert_eq!(mixed.to_string(), "$1.50+?", "part priced");
        assert_eq!(mixed.unpriced, ["claude-haiku-5-5"], "listed");
        let none = config.estimate([("claude-haiku-5-5", &spent)]);
        assert_eq!(none.to_string(), "?", "nothing priced");
        let idle = config.estimate([("claude-haiku-5-5", &Tokens::default())]);
        assert_eq!(
            idle.to_string(),
            "$0.00",
            "a model that spent nothing needs no price"
        );
    }

    #[test]
    fn only_a_fresh_claude_gets_a_session_id() {
        let argv = |words: &[&str]| {
            words
                .iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>()
        };
        let cases = [
            (argv(&["claude"]), true),
            (argv(&["C:/bin/claude.exe", "--model", "haiku"]), true),
            (argv(&["Claude"]), true),
            (argv(&["claude", "--resume", "abc"]), false),
            (argv(&["claude", "--session-id=abc"]), false),
            (argv(&["claude", "-c"]), false),
            (argv(&["bash", "-c", "claude"]), false),
            (argv(&[]), false),
        ];
        for (argv, expected) in cases {
            assert_eq!(fresh_claude(&argv), expected, "{argv:?}");
        }
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
