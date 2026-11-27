//! The headwork side of the video flow (`docs/video-flow.md`).
//!
//! The phone push of me-studio's outbox and the done check of video tasks. Pure: the tick and `done` read the file and run
//! the command, these functions decide.

use serde::Deserialize;

/// The event that records the last outbox line pushed; its data holds that line's `id`.
pub const EVENT_OUTBOX_SENT: &str = "push-outbox-sent";

/// One line of me-studio's push outbox.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OutboxEntry {
    /// `<project>-runs_failing-<unix nanos>`.
    pub id: String,
    /// The project, `-` when the failing streak is host-wide.
    #[serde(default)]
    pub project: String,
    /// What failed, `runs_failing`.
    #[serde(default)]
    pub kind: String,
    /// How many runs failed in a row.
    #[serde(default)]
    pub streak: u32,
    /// The short title.
    #[serde(default)]
    pub title: String,
    /// The text to push.
    #[serde(default)]
    pub message: String,
}

impl OutboxEntry {
    /// The text pushed to the phone: the title and the message.
    #[must_use]
    pub fn text(&self) -> String {
        match (self.title.is_empty(), self.message.is_empty()) {
            (false, false) => format!("{}: {}", self.title, self.message),
            (false, true) => self.title.clone(),
            (true, false) => self.message.clone(),
            (true, true) => self.id.clone(),
        }
    }
}

/// The parseable lines of `content` after the line whose id is `last_sent`, found by position.
///
/// A last line without its newline may still be written and is ignored; a line that does not parse is skipped. With no
/// `last_sent`, or one the file no longer holds, every parseable line is new.
#[must_use]
pub fn new_entries(content: &str, last_sent: Option<&str>) -> Vec<OutboxEntry> {
    let complete = content.rsplit_once('\n').map_or("", |(head, _)| head);
    let entries: Vec<OutboxEntry> = complete
        .lines()
        .filter_map(|line| serde_json::from_str(line.trim()).ok())
        .collect();
    let after = last_sent
        .and_then(|id| entries.iter().rposition(|entry| entry.id == id))
        .map_or(0, |at| at + 1);
    entries.into_iter().skip(after).collect()
}

/// What the done check of a task says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoneVerdict {
    /// Every scene is acknowledged: `done` passes.
    Acknowledged,
    /// These scenes wait: `done` is refused.
    Waiting(Vec<String>),
    /// The check could not tell: `done` passes and says so.
    Unknown(String),
}

#[derive(Deserialize)]
struct AckReport {
    #[serde(default)]
    scenes: Vec<AckScene>,
}

#[derive(Deserialize)]
struct AckScene {
    scene: String,
    #[serde(default)]
    stage: Option<String>,
    #[serde(default)]
    on_master: bool,
    #[serde(default)]
    acknowledged: bool,
}

impl AckScene {
    fn line(&self) -> String {
        let stage = match (&self.stage, self.on_master) {
            (Some(stage), _) => stage.replace('_', " "),
            (None, _) => "not on master yet".to_owned(),
        };
        format!("{}: {stage}", self.scene)
    }
}

/// Reads the exit code and standard output of `me-studio pipeline-ack --json`: 0 acknowledged, 1 waiting (the waiting
/// scene lines come from the JSON), anything else, or no exit code, could not tell.
#[must_use]
pub fn done_verdict(exit: Option<i32>, stdout: &str, stderr: &str) -> DoneVerdict {
    match exit {
        Some(0) => DoneVerdict::Acknowledged,
        Some(1) => {
            let waiting: Vec<String> = serde_json::from_str::<AckReport>(stdout)
                .map(|report| {
                    report
                        .scenes
                        .iter()
                        .filter(|scene| !scene.acknowledged)
                        .map(AckScene::line)
                        .collect()
                })
                .unwrap_or_default();
            if waiting.is_empty() {
                DoneVerdict::Waiting(vec![
                    "(the check named no scene: read its output)".to_owned(),
                ])
            } else {
                DoneVerdict::Waiting(waiting)
            }
        }
        Some(code) => DoneVerdict::Unknown(format!("the check exited {code}: {}", stderr.trim())),
        None => DoneVerdict::Unknown("the check ended without an exit code".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: &str) -> String {
        format!(
            "{{\"id\":\"{id}\",\"at\":\"t\",\"project\":\"-\",\"kind\":\"runs_failing\",\"streak\":2,\"title\":\"T\",\"message\":\"M\"}}\n"
        )
    }

    #[test]
    fn partial_last_line_is_ignored_then_read_once_finished() {
        let mut content = line("a");
        content.push_str("{\"id\":\"b\",\"ti");
        let first = new_entries(&content, None);
        assert_eq!(first.len(), 1, "only the complete line counts");
        assert_eq!(first[0].id, "a", "the first line");
        let finished = format!("{}{}", line("a"), line("b"));
        let next = new_entries(&finished, Some("a"));
        assert_eq!(
            next.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["b"],
            "b arrives once finished"
        );
    }

    #[test]
    fn repeated_read_finds_nothing_new() {
        let content = format!("{}{}", line("a"), line("b"));
        assert!(
            new_entries(&content, Some("b")).is_empty(),
            "nothing after the last id"
        );
    }

    #[test]
    fn new_lines_after_the_cursor_and_garbage_skipped() {
        let content = format!("{}not json\n{}{}", line("a"), line("b"), line("c"));
        let ids: Vec<_> = new_entries(&content, Some("a"))
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(ids, ["b", "c"], "the garbage line is skipped");
    }

    #[test]
    fn unknown_cursor_sends_everything() {
        let content = format!("{}{}", line("a"), line("b"));
        assert_eq!(
            new_entries(&content, Some("gone")).len(),
            2,
            "a replaced file is read whole"
        );
        assert_eq!(
            new_entries(&content, None).len(),
            2,
            "no cursor reads whole"
        );
    }

    #[test]
    fn text_joins_title_and_message() {
        let entry = new_entries(&line("a"), None).remove(0);
        assert_eq!(entry.text(), "T: M", "title and message");
    }

    #[test]
    fn exit_zero_passes() {
        assert_eq!(
            done_verdict(Some(0), "", ""),
            DoneVerdict::Acknowledged,
            "all acknowledged"
        );
    }

    #[test]
    fn exit_one_lists_the_waiting_scenes() {
        let json = r#"{"project":"me-sim","all_acknowledged":false,"scenes":[
            {"scene":"a","stage":"public","on_master":true,"acknowledged":true},
            {"scene":"b","stage":"needs_review","on_master":true,"acknowledged":false},
            {"scene":"c","stage":null,"on_master":false,"acknowledged":false}]}"#;
        assert_eq!(
            done_verdict(Some(1), json, ""),
            DoneVerdict::Waiting(vec![
                "b: needs review".to_owned(),
                "c: not on master yet".to_owned()
            ]),
            "only the waiting scenes"
        );
    }

    #[test]
    fn exit_one_with_unreadable_output_still_refuses() {
        assert!(
            matches!(
                done_verdict(Some(1), "garbage", ""),
                DoneVerdict::Waiting(_)
            ),
            "refuses"
        );
    }

    #[test]
    fn exit_two_and_other_do_not_block() {
        assert!(
            matches!(done_verdict(Some(2), "", "boom"), DoneVerdict::Unknown(m) if m.contains("boom")),
            "exit 2"
        );
        assert!(
            matches!(done_verdict(Some(9), "", ""), DoneVerdict::Unknown(_)),
            "other code"
        );
        assert!(
            matches!(done_verdict(None, "", ""), DoneVerdict::Unknown(_)),
            "no code"
        );
    }
}
