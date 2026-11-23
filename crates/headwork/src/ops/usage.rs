//! `usage`: tokens spent by every Claude session, read from Claude Code's transcripts (`docs/usage.md`).
//!
//! [`ingest`] reads each transcript from where the last read stopped to its last whole line and stores one row per API
//! response; [`report`] sums them by a grouping. Parsing is pure, in `headwork_core::usage`.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail};
use headwork_core::UsageGroup;
use headwork_core::usage::{
    AgentMeta, Estimate, UsageConfig, agent_meta, parse_line, transcript_of,
};
use headwork_store::{
    FileMark, NewAgent, Store, TaskId, UsageBatch, UsageQuery, UsageTag, UsageTotal,
};
use serde::Serialize;

use crate::ops::shadow::since_instant;
use crate::ops::slashed;

/// The folder under `~/.claude` that holds every transcript, one folder per working folder.
const PROJECTS_DIR: &str = "projects";

/// What a read of the transcripts did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct IngestReport {
    /// Transcripts looked at.
    pub files: usize,
    /// Transcripts that had new lines.
    pub read: usize,
    /// Response lines found in the new lines; a response with several content blocks is written once per block.
    pub responses: usize,
    /// Responses stored for the first time.
    pub inserted: usize,
    /// Rows moved from `unmanaged` to the task of a session the store learned since.
    pub retagged: usize,
    /// Transcripts that could not be read, with why.
    pub errors: Vec<String>,
}

/// Every transcript under `<claude_dir>/projects`.
///
/// Each session's own (`<folder>/<session>.jsonl`) and its subagents' (`<folder>/<session>/subagents/**/agent-<id>.jsonl`).
/// A folder that cannot be listed is skipped.
#[must_use]
pub fn transcripts(claude_dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for folder in entries(&claude_dir.join(PROJECTS_DIR)) {
        for entry in entries(&folder) {
            if entry.is_dir() {
                subagent_files(&entry.join("subagents"), &mut found);
            } else if transcript_of(&entry).is_some() {
                found.push(entry);
            }
        }
    }
    found.sort();
    found
}

/// The transcripts of one session: its own and its subagents'.
#[must_use]
pub fn session_transcripts(claude_dir: &Path, session: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for folder in entries(&claude_dir.join(PROJECTS_DIR)) {
        let own = folder.join(format!("{session}.jsonl"));
        if own.is_file() {
            found.push(own);
        }
        subagent_files(&folder.join(session).join("subagents"), &mut found);
    }
    found.sort();
    found
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).map_or_else(
        |_| Vec::new(),
        |read| {
            read.filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .collect()
        },
    )
}

fn subagent_files(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in entries(dir) {
        if entry.is_dir() {
            subagent_files(&entry, found);
        } else if transcript_of(&entry).is_some_and(|file| file.agent_id.is_some()) {
            found.push(entry);
        }
    }
}

fn mark_of(path: &Path) -> Result<FileMark> {
    let meta = std::fs::metadata(path)?;
    let mtime_ms = meta
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        });
    Ok(FileMark {
        read_to: 0,
        size: meta.len(),
        mtime_ms,
    })
}

/// Where to start reading a file now at `now`, last read as `before`: where it stopped, from the start when it shrank
/// or its time went back (rewritten), or none when it has not changed.
const fn start_at(before: Option<FileMark>, now: FileMark, from_start: bool) -> Option<u64> {
    let Some(before) = before else {
        return Some(0);
    };
    if from_start || now.size < before.size || now.mtime_ms < before.mtime_ms {
        return Some(0);
    }
    if now.size == before.size && now.mtime_ms == before.mtime_ms {
        return None;
    }
    Some(before.read_to)
}

/// Whether a response at `at` lies on or before the last day the roll-up summed, so it is counted in `usage_daily`
/// already and a reread must skip it.
fn rolled_up(rolled_to: Option<&str>, at: &str) -> bool {
    rolled_to.is_some_and(|day| at.get(..10).is_some_and(|line_day| line_day <= day))
}

/// Reads one transcript's new whole lines and stores their responses; returns the responses found and stored, none
/// when the file has not changed.
///
/// # Errors
///
/// Fails when the file cannot be read or the store cannot be written.
pub fn ingest_file(
    store: &mut Store,
    path: &Path,
    from_start: bool,
) -> Result<Option<(usize, usize)>> {
    let Some(file) = transcript_of(path) else {
        bail!("{} is not a transcript", path.display());
    };
    let key = slashed(path);
    let now = mark_of(path)?;
    let Some(start) = start_at(store.usage_file(&key)?, now, from_start) else {
        if let Some(agent) = file.agent_id.as_deref()
            && !store.agent_known(agent)?
        {
            record_agent(store, path, &file.session_id, agent)?;
        }
        return Ok(None);
    };
    let rolled_to = store.usage_rolled_to()?;
    let mut reader = BufReader::new(File::open(path)?);
    reader.seek(SeekFrom::Start(start))?;
    let mut read_to = start;
    let mut lines = Vec::new();
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let taken = reader.read_until(b'\n', &mut buffer)?;
        if taken == 0 || buffer.last() != Some(&b'\n') {
            break;
        }
        read_to = read_to.saturating_add(u64::try_from(taken)?);
        if let Some(line) = parse_line(&String::from_utf8_lossy(&buffer))
            .filter(|line| !rolled_up(rolled_to.as_deref(), &line.at))
        {
            lines.push(line);
        }
    }
    let meta = file.agent_id.as_ref().and_then(|_| meta_of(path));
    let agent_type = meta.as_ref().and_then(|meta| meta.agent_type.clone());
    let tag = store
        .usage_tag(&file.session_id)?
        .unwrap_or(UsageTag::UNMANAGED);
    let dir = path
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let inserted = store.record_usage(&UsageBatch {
        path: &key,
        session_id: &file.session_id,
        agent_id: file.agent_id.as_deref(),
        agent_type: agent_type.as_deref(),
        tag,
        dir: &dir,
        lines: &lines,
        mark: FileMark { read_to, ..now },
    })?;
    if let Some(agent) = file.agent_id.as_deref() {
        store.record_agent(&NewAgent {
            agent_id: agent,
            session_id: &file.session_id,
            task: tag.task,
            meta: &meta.unwrap_or_default(),
        })?;
    }
    Ok(Some((lines.len(), inserted)))
}

/// What a subagent transcript's `agent-<id>.meta.json` beside it says; none when it is missing or unreadable.
fn meta_of(transcript: &Path) -> Option<AgentMeta> {
    std::fs::read_to_string(transcript.with_extension("meta.json"))
        .ok()
        .and_then(|text| agent_meta(&text))
}

/// Records a subagent whose transcript was read before the `agents` table existed.
fn record_agent(store: &Store, path: &Path, session: &str, agent: &str) -> Result<()> {
    let task = store.usage_tag(session)?.and_then(|tag| tag.task);
    store.record_agent(&NewAgent {
        agent_id: agent,
        session_id: session,
        task,
        meta: &meta_of(path).unwrap_or_default(),
    })?;
    Ok(())
}

/// Reads every file in `files` (see [`ingest_file`]), then re-tags the rows of sessions the store learned since. A
/// file that fails is listed in the report and the rest go on.
///
/// # Errors
///
/// Fails only when the re-tagging cannot write.
pub fn ingest(store: &mut Store, files: &[PathBuf], from_start: bool) -> Result<IngestReport> {
    let mut report = IngestReport {
        files: files.len(),
        ..IngestReport::default()
    };
    for path in files {
        match ingest_file(store, path, from_start) {
            Ok(Some((responses, inserted))) => {
                report.read += 1;
                report.responses += responses;
                report.inserted += inserted;
            }
            Ok(None) => {}
            Err(error) => report.errors.push(format!("{}: {error:#}", slashed(path))),
        }
    }
    report.retagged = store.retag_usage().context("re-tagging usage rows")?;
    Ok(report)
}

/// Reads one session's transcript and its subagents', as the `Stop` and `SessionEnd` hooks do: the subagents' sit in
/// `<transcript without .jsonl>/subagents`.
///
/// # Errors
///
/// Fails only when the re-tagging cannot write; unreadable files are listed in the report.
pub fn read_transcript(store: &mut Store, transcript: &Path) -> Result<IngestReport> {
    let mut files = vec![transcript.to_path_buf()];
    subagent_files(&transcript.with_extension("").join("subagents"), &mut files);
    ingest(store, &files, false)
}

/// The tokens a task and its parts spent.
///
/// # Errors
///
/// Fails when the store cannot be read.
pub fn task_total(store: &Store, task: TaskId) -> Result<UsageTotal> {
    let mut tasks = vec![task];
    tasks.extend(store.parts(task)?);
    let rows = store.usage_totals(&UsageQuery {
        by: UsageGroup::Role,
        since: None,
        tasks: Some(tasks),
        unmanaged: false,
    })?;
    Ok(sum(rows.iter()))
}

fn sum<'a>(rows: impl Iterator<Item = &'a UsageTotal>) -> UsageTotal {
    let mut total = UsageTotal::named("total");
    for row in rows {
        total.add(row);
    }
    total
}

/// Sums the per-response rows older than `keep_days` into daily rows (`docs/usage.md`, section 3); `0` keeps them.
/// The cut is a UTC day boundary, so a day is never split between the two tables.
///
/// # Errors
///
/// Fails when the store cannot be written.
pub fn roll_up(
    store: &mut Store,
    keep_days: u32,
    actor: headwork_core::Actor,
) -> Result<Option<headwork_store::RolledUp>> {
    if keep_days == 0 {
        return Ok(None);
    }
    let cut = store.instant_ago(u64::from(keep_days).saturating_mul(86_400))?;
    let Some(day) = cut.get(..10) else {
        bail!("the store gave an instant without a date: {cut}");
    };
    Ok(store.roll_up_usage(day, actor)?)
}

/// What `headwork usage` was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageOptions {
    /// The grouping.
    pub by: UsageGroup,
    /// `--since`: a span back from now, a date or an RFC 3339 time.
    pub since: Option<String>,
    /// `--task`: one task and its parts.
    pub task: Option<String>,
    /// Break unmanaged sessions down.
    pub unmanaged: bool,
}

/// The totals `headwork usage` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsageReport {
    /// The grouping.
    pub by: UsageGroup,
    /// The start of the window, UTC.
    pub since: Option<String>,
    /// The task asked for.
    pub task: Option<String>,
    /// One line per group, largest first.
    pub rows: Vec<UsageRow>,
    /// Every row summed.
    pub total: UsageRow,
    /// Models that spent tokens and have no price in `[usage.prices]`.
    pub unpriced: Vec<String>,
    /// What the read before the report did, when one ran.
    pub ingest: Option<IngestReport>,
}

/// One group's total with its estimate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsageRow {
    /// The numbers.
    #[serde(flatten)]
    pub total: UsageTotal,
    /// The estimate at API list prices.
    pub estimate: Estimate,
    /// The estimate as printed: `$4.10`, `$4.10+?` or `?`.
    pub dollars: String,
}

impl UsageRow {
    /// Prices a total.
    #[must_use]
    pub fn priced(total: UsageTotal, prices: &UsageConfig) -> Self {
        let estimate = prices.estimate(
            total
                .models
                .iter()
                .map(|(model, tokens)| (model.as_str(), tokens)),
        );
        Self {
            dollars: estimate.to_string(),
            total,
            estimate,
        }
    }
}

/// Sums the stored usage as `options` asks.
///
/// # Errors
///
/// Fails when `since` is not a time, the task is unknown or the store cannot be read.
pub fn report(store: &Store, options: &UsageOptions, prices: &UsageConfig) -> Result<UsageReport> {
    let since = options
        .since
        .as_deref()
        .map(|text| since_instant(store, text))
        .transpose()?;
    let tasks = match &options.task {
        Some(name) => {
            let Some(task) = store.find_task(name)? else {
                bail!("no task named `{name}`");
            };
            let mut ids: Vec<TaskId> = vec![task.id];
            ids.extend(store.parts(task.id)?);
            Some(ids)
        }
        None => None,
    };
    let rows = store.usage_totals(&UsageQuery {
        by: options.by,
        since: since.clone(),
        tasks,
        unmanaged: options.unmanaged,
    })?;
    let total = UsageRow::priced(sum(rows.iter()), prices);
    Ok(UsageReport {
        by: options.by,
        since,
        task: options.task.clone(),
        unpriced: total.estimate.unpriced.clone(),
        rows: rows
            .into_iter()
            .map(|row| UsageRow::priced(row, prices))
            .collect(),
        total,
        ingest: None,
    })
}

/// What transcripts cannot show, so totals are not read as complete (`docs/usage.md`, section 2).
pub const GAPS: &[&str] = &[
    "the auto mode classifier's own calls: transcripts name them (serverClassifierRequest) but write no usage",
    "the summary call of a compaction: the summary lands as a user line, without usage",
    "helper tabs running anything but a fresh `claude` (a `claude --resume` keeps the resumed session's own tag)",
    "transcripts Claude deleted after cleanupPeriodDays (default 30) before headwork read them",
    "service tier and fast mode prices: not applied until a priced tier is used here",
];

#[cfg(test)]
mod tests {
    use super::*;

    const fn mark(read_to: u64, size: u64, mtime_ms: i64) -> FileMark {
        FileMark {
            read_to,
            size,
            mtime_ms,
        }
    }

    #[test]
    fn rereads_skip_days_the_roll_up_summed() {
        assert!(
            rolled_up(Some("2026-08-02"), "2026-08-02T23:59:59.000Z"),
            "the last summed day"
        );
        assert!(
            rolled_up(Some("2026-08-02"), "2026-07-30T10:00:00.000Z"),
            "before it"
        );
        assert!(
            !rolled_up(Some("2026-08-02"), "2026-08-03T00:00:00.000Z"),
            "after it"
        );
        assert!(
            !rolled_up(None, "2026-08-02T10:00:00.000Z"),
            "nothing summed yet"
        );
    }

    #[test]
    fn reads_start_where_the_last_stopped() {
        let cases = [
            ("never read", None, mark(0, 10, 5), false, Some(0)),
            (
                "unchanged",
                Some(mark(8, 10, 5)),
                mark(0, 10, 5),
                false,
                None,
            ),
            (
                "grown",
                Some(mark(8, 10, 5)),
                mark(0, 20, 6),
                false,
                Some(8),
            ),
            (
                "shrunk",
                Some(mark(8, 10, 5)),
                mark(0, 4, 6),
                false,
                Some(0),
            ),
            (
                "time went back",
                Some(mark(8, 10, 5)),
                mark(0, 12, 4),
                false,
                Some(0),
            ),
            (
                "from the start",
                Some(mark(8, 10, 5)),
                mark(0, 10, 5),
                true,
                Some(0),
            ),
        ];
        for (name, before, now, from_start, expected) in cases {
            assert_eq!(start_at(before, now, from_start), expected, "{name}");
        }
    }
}
