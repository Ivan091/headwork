//! Plain-text views of the board and the event log.

use std::fmt::Write;

use headwork::ops::clean::{BranchOutcome, CleanReport, GcOutcome, Outcome as CleanOutcome};
use headwork::ops::doctor::{DoctorReport, Status};
use headwork::ops::flows::{FlowReport, Origin as FlowOrigin};
use headwork::ops::import::ImportReport;
use headwork::ops::install::InstallReport;
use headwork::ops::park::{ParkReport, RecapSource};
use headwork::ops::reap::{Outcome as ReapOutcome, ReapReport};
use headwork::ops::reload::{self, Freshness, SessionBuild};
use headwork::ops::resume::{ResumeReport, TaskOutcome};
use headwork::ops::schedule::{Change, ScheduleReport};
use headwork::ops::shadow::ShadowReport;
use headwork::ops::slashed;
use headwork::ops::spawn::{Outcome, Pumped};
use headwork::ops::status::StatusLine;
use headwork::ops::tick::TickReport;
use headwork::ops::usage::{UsageReport, UsageRow};
use headwork_core::reap::{Reason as ReapReason, Spare};
use headwork_core::usage::{UsageConfig, short_count};
use headwork_core::{PlaceKind, TaskShape};
use headwork_store::{AskRow, Board, BoardTask, EventRecord, JobRow};

/// What an import found and did.
#[must_use]
pub(crate) fn imported(added: &[String], report: &ImportReport, dry_run: bool) -> String {
    let verb = if dry_run { "would adopt" } else { "adopted" };
    let mut text = String::new();
    if !added.is_empty() {
        let what = if dry_run { "would add" } else { "added" };
        let _ = writeln!(text, "{what} to config.toml: {}", added.join(", "));
    }
    let _ = writeln!(text, "{verb} {} running workers", report.adopted.len());
    for name in &report.adopted {
        let _ = writeln!(text, "  {name}");
    }
    for line in &report.skipped {
        let _ = writeln!(text, "  skipped {line}");
    }
    if !report.stray_worktrees.is_empty() {
        text.push_str("worktrees without a running worker\n");
        for path in &report.stray_worktrees {
            let _ = writeln!(text, "  {path}");
        }
    }
    text
}

/// What a queue run did, one line per queued task.
#[must_use]
pub(crate) fn pumped(pumped: &[Pumped]) -> String {
    let mut text = String::new();
    for item in pumped {
        let _ = match &item.outcome {
            Outcome::Started { surface } => writeln!(text, "  {} started in {surface}", item.name),
            Outcome::Waiting { reason } => writeln!(text, "  {} waits: {reason}", item.name),
            Outcome::Failed { error } => writeln!(text, "  {} failed: {error}", item.name),
        };
    }
    text
}

/// The board as text: a header line, then each project with its tasks, sessions and waiting asks.
#[must_use]
pub(crate) fn board(board: &Board, max_workers: u32, prices: &UsageConfig) -> String {
    let asks = board
        .tasks
        .iter()
        .map(|task| task.asks.len())
        .sum::<usize>()
        + board.other_asks.len();
    let idle = if board.waiting_workers == 0 {
        String::new()
    } else {
        format!(
            " (+{} waiting on the user, slot freed)",
            board.waiting_workers
        )
    };
    let mut text = format!(
        "machine {} · {} of {max_workers} slots{idle} · {asks} {} waiting\n",
        board.machine,
        board.live_workers,
        if asks == 1 { "ask" } else { "asks" },
    );
    let width = board
        .tasks
        .iter()
        .map(|task| task.task.name.to_string().len())
        .max()
        .unwrap_or(0);
    for project in &board.projects {
        let tasks: Vec<&BoardTask> = board
            .tasks
            .iter()
            .filter(|task| task.task.project() == &project.name)
            .collect();
        if !project.enabled && tasks.is_empty() {
            continue;
        }
        let disabled = if project.enabled { "" } else { ", disabled" };
        let _ = write!(
            text,
            "\n{} ({}{disabled})\n",
            project.name, project.main_path
        );
        if tasks.is_empty() {
            text.push_str("  no tasks\n");
        }
        for (task, depth) in tree(&tasks) {
            task_line(&mut text, task, width, depth, prices);
        }
    }
    if !board.other_asks.is_empty() {
        text.push_str("\nasks without a task\n");
        for ask in &board.other_asks {
            ask_line(&mut text, ask);
        }
    }
    text
}

/// The tasks of one project as a tree of depth one: each lead or plain task, then its parts indented under it. A part
/// whose lead is not listed (closed, while closed tasks are hidden) stays at the top and names its lead.
fn tree<'a>(tasks: &[&'a BoardTask]) -> Vec<(&'a BoardTask, usize)> {
    let listed = |id| tasks.iter().any(|task| task.task.id == id);
    let mut rows = Vec::with_capacity(tasks.len());
    for &task in tasks {
        if task.task.shape.parent().is_some_and(listed) {
            continue;
        }
        rows.push((task, 0));
        for &part in tasks {
            if part.task.shape.parent() == Some(task.task.id) {
                rows.push((part, 1));
            }
        }
    }
    rows
}

fn task_line(
    text: &mut String,
    task: &BoardTask,
    width: usize,
    depth: usize,
    prices: &UsageConfig,
) {
    let t = &task.task;
    let name = t.name.to_string();
    let (indent, width) = if depth == 0 {
        ("", width)
    } else {
        ("  └ ", width.saturating_sub(4))
    };
    let _ = write!(
        text,
        "  {indent}#{:<3} {name:<width$}  {:<11} {:<6} {:<6} {}",
        t.id.0,
        t.state.as_str(),
        t.model,
        t.effort_label(),
        t.kind
    );
    if t.in_main {
        text.push_str(" (main)");
    }
    if t.keep {
        text.push_str(" (keep)");
    }
    match t.shape {
        TaskShape::Lead => text.push_str(" (lead)"),
        TaskShape::Part { parent } if depth == 0 => {
            let _ = write!(text, " (part of #{})", parent.0);
        }
        TaskShape::Part { .. } | TaskShape::Plain => {}
    }
    if let Some(session) = &task.session {
        let _ = write!(text, " · session {}", session.state);
        if let Some(surface) = &session.host_surface {
            let _ = write!(text, " {surface}");
        }
        match (session.host_place, &session.host_pane) {
            (Some(PlaceKind::Pane), _)
                if t.shape == TaskShape::Lead && session.host_workspace.is_some() =>
            {
                let _ = write!(
                    text,
                    " in its own workspace {}",
                    session.host_workspace.as_deref().unwrap_or_default()
                );
            }
            (Some(PlaceKind::Pane), Some(pane)) => {
                let _ = write!(text, " in its own pane {pane}");
            }
            (Some(PlaceKind::Tab), Some(pane)) => {
                let _ = write!(text, " as a tab of {pane}");
            }
            (Some(PlaceKind::Tab), None) => text.push_str(" as a tab"),
            (Some(PlaceKind::Pane), None) | (None, _) => {}
        }
    }
    if let Some(pr) = &task.pr {
        let _ = write!(text, " · PR {} {}", pr.state, pr.url);
    }
    if task.tokens > 0 {
        let _ = write!(text, " · {} tokens", short_count(task.tokens));
        if let Some(estimate) = headwork::ops::status::estimate(prices, &task.models) {
            let _ = write!(text, " {estimate}");
        }
    }
    text.push('\n');
    if let Some(review) = &task.review {
        let _ = writeln!(text, "      {review}");
    }
    for ask in &task.asks {
        ask_line(text, ask);
    }
}

fn ask_line(text: &mut String, ask: &AskRow) {
    let options: Vec<String> = ask
        .options
        .iter()
        .enumerate()
        .map(|(index, option)| format!("[{} {option}]", index + 1))
        .collect();
    let _ = writeln!(
        text,
        "       ask {} {} {}: {} {}",
        ask.id.0,
        ask.kind,
        ask.state,
        ask.question,
        options.join(" ")
    );
}

/// One line per top-level task, as `status` prints it.
#[must_use]
pub(crate) fn statuses(lines: &[StatusLine]) -> String {
    let mut text = String::new();
    for line in lines {
        if line.tokens > 0 {
            let estimate = line
                .estimate
                .as_ref()
                .map_or_else(String::new, |estimate| format!(" {estimate}"));
            let _ = writeln!(
                text,
                "{} · {} tokens{estimate}",
                line.line,
                short_count(line.tokens)
            );
        } else {
            let _ = writeln!(text, "{}", line.line);
        }
    }
    if lines.is_empty() {
        text.push_str("no tasks\n");
    }
    text
}

/// The event log as text, one line per event.
#[must_use]
pub(crate) fn events(events: &[EventRecord]) -> String {
    if events.is_empty() {
        return "no events\n".to_owned();
    }
    let width = events
        .iter()
        .filter_map(|event| event.task.as_ref().map(String::len))
        .max()
        .unwrap_or(1);
    events.iter().fold(String::new(), |mut text, event| {
        let task = event.task.as_deref().unwrap_or("-");
        let _ = writeln!(
            text,
            "{}  {:<12} {task:<width$}  {:<16} {}",
            event.at,
            event.actor,
            event.kind,
            describe(event)
        );
        text
    })
}

fn describe(event: &EventRecord) -> String {
    let field = |key: &str| {
        event
            .data
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
            .to_owned()
    };
    match event.kind.as_str() {
        "transition" => format!("{}: {} -> {}", field("trigger"), field("from"), field("to")),
        "refused" => format!(
            "{} refused in {}: {}",
            field("trigger"),
            field("state"),
            field("reason")
        ),
        "task-created" => format!("model {}, effort {}", field("model"), field("effort")),
        _ => event.data.to_string(),
    }
}

/// What a tick found and did.
#[must_use]
pub(crate) fn ticked(report: &TickReport) -> String {
    let mode = if report.enforce {
        "enforcing"
    } else {
        "shadow"
    };
    let mut text = format!("tick ({mode})");
    if !report.host_up {
        text.push_str(", terminal host not running");
    }
    if report.queue_paused {
        text.push_str(", queue paused: disk nearly full");
    }
    text.push('\n');
    if report.lines.is_empty() && report.started.is_empty() && report.update.is_none() {
        text.push_str("nothing to report\n");
    }
    for line in &report.lines {
        let task = line.task.as_deref().unwrap_or("-");
        let _ = writeln!(
            text,
            "  {:<9} {} {task}: {}",
            format!("{:?}", line.status).to_lowercase(),
            line.rule,
            line.detail
        );
    }
    text.push_str(&pumped(&report.started));
    if let Some(usage) = report.usage.as_ref().filter(|usage| usage.inserted > 0) {
        let _ = writeln!(
            text,
            "  usage     {} new responses from {} transcripts",
            usage.inserted, usage.read
        );
    }
    if let Some(rolled) = &report.usage_rolled_up {
        let _ = writeln!(
            text,
            "  usage     summed {} responses before {} into {} daily rows",
            rolled.rows, rolled.before_day, rolled.days
        );
    }
    if let Some(update) = &report.update {
        let _ = match (&update.started, &update.error) {
            (Some(started), _) => writeln!(
                text,
                "  started   headwork {} (pid {}): a poll was due",
                update.args.join(" "),
                started.pid
            ),
            (None, error) => writeln!(
                text,
                "  failed    headwork {}: {}",
                update.args.join(" "),
                error.as_deref().unwrap_or("it did not start")
            ),
        };
    }
    text
}

/// What a park did.
#[must_use]
pub(crate) fn parked(report: &ParkReport) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "park {}", report.park);
    for session in &report.sessions {
        let recap = match session.recap {
            RecapSource::Session => "own recap",
            RecapSource::Mechanical => "mechanical recap",
        };
        let _ = writeln!(
            text,
            "  parked {} ({recap}, was {})",
            session.name, session.was
        );
        if let Some(snapshot) = &session.snapshot {
            let _ = writeln!(text, "    uncommitted changes saved as {snapshot}");
        }
        if let Some(question) = &session.open_question {
            let _ = writeln!(
                text,
                "    open question, asked again after the resume: {question}"
            );
        }
        if let Some(problem) = &session.problem {
            let _ = writeln!(text, "    problem: {problem}");
        }
    }
    if let Some(orchestrator) = &report.orchestrator {
        let recap = match orchestrator.recap {
            RecapSource::Session => "own recap",
            RecapSource::Mechanical => "mechanical recap",
        };
        let _ = writeln!(
            text,
            "  orchestrator {} left running ({recap}); the resume brings it back",
            orchestrator.session
        );
        if let Some(problem) = &orchestrator.problem {
            let _ = writeln!(text, "    note: {problem}");
        }
    }
    for job in &report.jobs {
        let _ = writeln!(text, "  job {}: {}", job.label, job.outcome);
    }
    if report.sessions.is_empty() && report.jobs.is_empty() && report.orchestrator.is_none() {
        text.push_str("  nothing was running\n");
    }
    text.push_str(if report.safe_to_restart {
        "safe to restart\n"
    } else {
        "NOT safe to restart: see the problems and jobs left running above\n"
    });
    text
}

/// What a resume did.
#[must_use]
pub(crate) fn resumed(report: &ResumeReport) -> String {
    let mut text = String::new();
    let park = report
        .park
        .map_or_else(|| "none (crash)".to_owned(), |id| id.to_string());
    let _ = writeln!(text, "resume of park {park}");
    if let Some(orchestrator) = &report.orchestrator {
        let _ = writeln!(text, "  orchestrator: {}", outcome_line(orchestrator));
    }
    for task in &report.tasks {
        let line = outcome_line(&task.outcome);
        let _ = writeln!(text, "  {}: {line}", task.name);
        if let Some(question) = &task.open_question {
            let _ = writeln!(text, "    asks again: {question}");
        }
    }
    for job in &report.jobs {
        let _ = writeln!(text, "  job {}: {}", job.label, job.outcome);
    }
    if report.queue_started > 0 {
        let _ = writeln!(text, "  {} queued task(s) started", report.queue_started);
    }
    let _ = writeln!(text, "summary for the orchestrator: {}", report.summary);
    text
}

fn outcome_line(outcome: &TaskOutcome) -> String {
    match outcome {
        TaskOutcome::Resumed { surface } => format!("resumed in {surface}"),
        TaskOutcome::Fresh { surface, reason } => {
            format!("started fresh in {surface} ({reason})")
        }
        TaskOutcome::Skipped { reason } => format!("skipped: {reason}"),
        TaskOutcome::Failed { error } => format!("FAILED: {error}"),
    }
}

/// The registered jobs.
#[must_use]
pub(crate) fn jobs(jobs: &[JobRow]) -> String {
    if jobs.is_empty() {
        return "no registered jobs\n".to_owned();
    }
    let mut text = String::new();
    for job in jobs {
        let pid = job
            .pid
            .map_or_else(|| "-".to_owned(), |pid| pid.to_string());
        let _ = writeln!(
            text,
            "{:>3} {:<10} {:<10} pid {pid:<7} {} [{}]",
            job.id,
            job.state,
            job.park_policy,
            job.label,
            job.argv.join(" ")
        );
        if let Some(note) = &job.note {
            let _ = writeln!(text, "      {note}");
        }
    }
    text
}

/// What doctor found, one check per line with its fix beneath.
#[must_use]
pub(crate) fn doctored(report: &DoctorReport) -> String {
    let mut text = String::new();
    for check in &report.checks {
        let mark = match check.status {
            Status::Ok => "ok  ",
            Status::Warn => "warn",
            Status::Fail => "FAIL",
        };
        let _ = writeln!(text, "[{mark}] {:<14} {}", check.id, check.detail);
        if let Some(fix) = &check.fix {
            let _ = writeln!(text, "       fix: {fix}");
        }
    }
    let _ = writeln!(
        text,
        "\n{} ok, {} warnings, {} problems",
        report.count(Status::Ok),
        report.count(Status::Warn),
        report.count(Status::Fail)
    );
    text
}

/// Each flow with where it comes from, and its problems.
#[must_use]
pub(crate) fn flows_checked(reports: &[FlowReport]) -> String {
    let mut text = String::new();
    for report in reports {
        let mark = if report.ok() { "ok  " } else { "FAIL" };
        let origin = match &report.origin {
            FlowOrigin::Builtin => "built-in".to_owned(),
            FlowOrigin::Home(path) => path.display().to_string(),
        };
        let _ = writeln!(text, "[{mark}] {:<12} {origin}", report.name);
        if let Some(description) = &report.description {
            let _ = writeln!(text, "       {description}");
        }
        for problem in &report.problems {
            let _ = writeln!(text, "       {problem}");
        }
    }
    text
}

/// What an install did and how the running sessions pick it up.
#[must_use]
pub(crate) fn installed(report: &InstallReport, sessions: &[SessionBuild]) -> String {
    let mut text = String::new();
    let _ = writeln!(
        text,
        "installed {} ({})",
        slashed(&report.target),
        report.stamp
    );
    for path in &report.removed {
        let _ = writeln!(text, "  deleted {}", slashed(path));
    }
    for path in &report.kept {
        let _ = writeln!(
            text,
            "  kept {} (still running; the next install deletes it)",
            slashed(path)
        );
    }
    let _ = writeln!(
        text,
        "hooks, the CLI, finishers, park and resume use the new build from their next call"
    );
    if !sessions.is_empty() {
        let _ = writeln!(text, "{}", reload::summary(sessions));
        if !reload::named(sessions, Freshness::Pinned).is_empty() {
            let _ = writeln!(text, "  fix: {}", reload::PINNED_FIX);
        }
    }
    text
}

/// The shadow log by rule.
#[must_use]
pub(crate) fn shadowed(report: &ShadowReport) -> String {
    let mut text = String::new();
    let mode = if report.enforcing {
        "enforcing"
    } else {
        "shadow"
    };
    let window = report
        .since
        .as_ref()
        .map_or_else(String::new, |since| format!(" since {since}"));
    let _ = writeln!(
        text,
        "{mode}: {} events{window} from {} sessions over {} days ({}), {} would-deny",
        report.events,
        report.sessions,
        report.days.len(),
        match (report.days.first(), report.days.last()) {
            (Some(first), Some(last)) => format!("{first} to {last}"),
            _ => "no events".to_owned(),
        },
        report.would_deny
    );
    for rule in &report.rules {
        let _ = writeln!(
            text,
            "\n{}{} x{}  {}\n  tools: {}\n  tasks: {}",
            rule.rule,
            if rule.enforced { " [enforced]" } else { "" },
            rule.count,
            rule.reason,
            rule.tools.join(", "),
            rule.tasks.join(", ")
        );
        for sample in &rule.samples {
            let _ = writeln!(text, "  {} {} {}", sample.at, sample.tool, sample.input);
        }
    }
    if report.rules.is_empty() {
        text.push_str("no rule would have denied anything\n");
    }
    text
}

fn usage_row(text: &mut String, row: &UsageRow, width: usize) {
    let tokens = &row.total.tokens;
    let hit = tokens
        .cache_hit_permille()
        .map_or_else(|| "-".to_owned(), |permille| format!("{}%", permille / 10));
    let web = tokens.web_searches.saturating_add(tokens.web_fetches);
    let _ = writeln!(
        text,
        "{:<width$}  {:>6}  {:>7}  {:>7}  {:>7}  {:>7}  {:>7}  {:>7}  {:>7}  {:>4}  {:>4}  {:>9}",
        row.total.key,
        short_count(row.total.responses),
        short_count(tokens.input),
        short_count(tokens.cache_write_5m),
        short_count(tokens.cache_write_1h),
        short_count(tokens.cache_read),
        short_count(tokens.output),
        short_count(tokens.thinking),
        short_count(tokens.total()),
        hit,
        short_count(web),
        row.dollars,
    );
}

/// The token totals of `headwork usage`, one line per group and the sum.
#[must_use]
pub(crate) fn usage(report: &UsageReport) -> String {
    let mut text = String::new();
    if let Some(ingest) = &report.ingest {
        let _ = writeln!(
            text,
            "read {} of {} transcripts: {} response lines, {} new responses{}",
            ingest.read,
            ingest.files,
            ingest.responses,
            ingest.inserted,
            if ingest.retagged > 0 {
                format!(", {} re-tagged", ingest.retagged)
            } else {
                String::new()
            }
        );
        for error in &ingest.errors {
            let _ = writeln!(text, "  unreadable: {error}");
        }
    }
    let scope = [
        report
            .task
            .as_ref()
            .map(|task| format!("task {task} and its parts")),
        report.since.as_ref().map(|since| format!("since {since}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let _ = writeln!(
        text,
        "tokens by {}{}",
        report.by,
        if scope.is_empty() {
            String::new()
        } else {
            format!(", {}", scope.join(", "))
        }
    );
    if report.rows.is_empty() {
        text.push_str(
            "no usage recorded
",
        );
        return text;
    }
    let width = report
        .rows
        .iter()
        .map(|row| row.total.key.chars().count())
        .chain([report.by.as_str().len(), "total".len()])
        .max()
        .unwrap_or(0);
    let _ = writeln!(
        text,
        "{:<width$}  {:>6}  {:>7}  {:>7}  {:>7}  {:>7}  {:>7}  {:>7}  {:>7}  {:>4}  {:>4}  {:>9}",
        report.by.as_str(),
        "resp",
        "input",
        "cw5m",
        "cw1h",
        "cread",
        "output",
        "think",
        "total",
        "hit",
        "web",
        "estimate"
    );
    for row in &report.rows {
        usage_row(&mut text, row, width);
    }
    if report.rows.len() > 1 {
        usage_row(&mut text, &report.total, width);
    }
    if !report.unpriced.is_empty() {
        let _ = writeln!(
            text,
            "no price in [usage.prices] for: {} (estimates at API list prices, never a bill)",
            report.unpriced.join(", ")
        );
    }
    text
}

/// What transcripts cannot show.
#[must_use]
pub(crate) fn usage_gaps(gaps: &[&str]) -> String {
    let mut text = String::from(
        "
not counted:
",
    );
    for gap in gaps {
        let _ = writeln!(text, "  - {gap}");
    }
    text
}

/// What `clean` did, or would do, per project.
#[must_use]
pub(crate) fn cleaned(report: &CleanReport) -> String {
    let mut text = String::new();
    if report.dry_run {
        text.push_str("dry run: nothing changed\n");
    }
    for project in &report.projects {
        let _ = writeln!(text, "\n{} ({})", project.project, project.main);
        if let Some(error) = &project.error {
            let _ = writeln!(text, "  cannot read: {error}");
        }
        if let Some(error) = &project.fetch_error {
            let _ = writeln!(text, "  fetch failed, judged on older remote refs: {error}");
        }
        if project.error.is_none() && project.worktrees.is_empty() {
            text.push_str("  no other worktrees\n");
        }
        for worktree in &project.worktrees {
            let what = match &worktree.outcome {
                CleanOutcome::WouldRemove => "would remove".to_owned(),
                CleanOutcome::Removed => "removed".to_owned(),
                CleanOutcome::WouldPrune => "would forget (folder gone)".to_owned(),
                CleanOutcome::Pruned => "forgot (folder gone)".to_owned(),
                CleanOutcome::Left { reason } => format!("left: {reason}"),
                CleanOutcome::Failed { error } => format!("FAILED: {error}"),
            };
            let branch = worktree
                .branch
                .as_deref()
                .map_or_else(String::new, |branch| format!(" [{branch}]"));
            let _ = writeln!(
                text,
                "  {}{branch} ({})\n    {what}",
                worktree.path, worktree.owner
            );
            if let Some(note) = &worktree.note {
                let _ = writeln!(text, "    {note}");
            }
        }
        for branch in &project.branches {
            let what = match &branch.outcome {
                BranchOutcome::WouldDelete => "would delete: it landed".to_owned(),
                BranchOutcome::Deleted => "deleted: it landed".to_owned(),
                BranchOutcome::Left { reason } => format!("left: {reason}"),
                BranchOutcome::Failed { error } => format!("FAILED: {error}"),
            };
            let _ = writeln!(
                text,
                "  branch {} ({})\n    {what}",
                branch.branch, branch.owner
            );
        }
        if let Some(gc) = &project.gc {
            let line = match gc {
                GcOutcome::WouldRun => "git gc: would run".to_owned(),
                GcOutcome::Ran => "git gc: ran".to_owned(),
                GcOutcome::Skipped { reason } => format!("git gc: skipped, {reason}"),
                GcOutcome::Failed { error } => format!("git gc: FAILED: {error}"),
            };
            let _ = writeln!(text, "  {line}");
        }
    }
    text
}

/// What `reap` ended, or would end, and the leftovers it left running.
#[must_use]
pub(crate) fn reaped(report: &ReapReport) -> String {
    let mut text = String::new();
    let verb = if report.dry_run { "would end" } else { "ended" };
    if report.ended.is_empty() {
        let _ = writeln!(text, "nothing to reap");
    } else {
        let _ = writeln!(text, "{verb} {} processes", report.ended.len());
    }
    for reaped in &report.ended {
        let why = match &reaped.why {
            ReapReason::Session { name } => format!("left by {name}"),
            ReapReason::Orphan => "orphaned and idle".to_owned(),
        };
        let outcome = match &reaped.outcome {
            ReapOutcome::WouldEnd | ReapOutcome::Ended => String::new(),
            ReapOutcome::Gone => " (already gone)".to_owned(),
            ReapOutcome::Failed { error } => format!(" (failed: {error})"),
        };
        let _ = writeln!(
            text,
            "  {:>6} {:<16} {why}{outcome}",
            reaped.pid, reaped.image
        );
    }
    if !report.spared.is_empty() {
        let _ = writeln!(text, "left running:");
    }
    for spared in &report.spared {
        let why = match &spared.why {
            Spare::Protected {
                session: Some(name),
            } => {
                format!(
                    "left by {name}, but is, runs under or started claude, me-studio, headwork or a job"
                )
            }
            Spare::Protected { session: None } => {
                "orphaned, but is, runs under or started claude, me-studio, headwork or a job"
                    .to_owned()
            }
            Spare::RunsOthers => "orphaned shell running other programs".to_owned(),
            Spare::Busy => "orphaned shell, not idle for 10 minutes".to_owned(),
        };
        let _ = writeln!(text, "  {:>6} {:<16} {why}", spared.pid, spared.image);
    }
    if !report.shells_judged {
        let _ = writeln!(
            text,
            "orphaned shells not judged: Git Bash's process list (`ps -el`) could not be read"
        );
    }
    text
}

/// What `tick --install` or `--uninstall` did.
#[must_use]
pub(crate) fn scheduled(report: &ScheduleReport) -> String {
    let what = match report.change {
        Change::Created => "created",
        Change::Replaced => "replaced",
        Change::Removed => "removed",
        Change::Absent => "was not there; nothing removed",
    };
    let mut text = format!(
        "{} \"{}\" {what}\n",
        headwork_core::Platform::current().scheduler_entry(),
        report.task
    );
    if let Some(command) = &report.command {
        let _ = writeln!(
            text,
            "  every {} minutes, current user: {command}",
            headwork::ops::schedule::EVERY_MINUTES
        );
    }
    text
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]
mod tests {
    use std::path::Path;

    use headwork_core::{
        Actor, AskKind, Config, PermissionMode, PlaceKind, ProjectName, Role, Slug, TaskRequest,
        Trigger,
    };
    use headwork_store::{EventFilter, NewAsk, NewSession, NewTask, Place, Store};

    fn store() -> (Store, Config) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/config.toml");
        let config = Config::parse(&std::fs::read_to_string(path).unwrap(), "config.toml").unwrap();
        let mut store = Store::open_in_memory().unwrap();
        store.sync_projects(&config).unwrap();
        (store, config)
    }

    fn task(store: &mut Store, config: &Config, project: &str, slug: &str) -> headwork_store::Task {
        let new = NewTask {
            request: TaskRequest {
                project: ProjectName::new(project).unwrap(),
                slug: Slug::new(slug).unwrap(),
                kind: "scene".to_owned(),
                model: None,
                effort: None,
                in_main: false,
                keep: false,
            },
            brief_path: "brief.md".to_owned(),
            depends_on: None,
            permission_mode: PermissionMode::Auto,
            shape: headwork_core::TaskShape::Plain,
        };
        store
            .create_task(config, &new, Actor::Orchestrator)
            .unwrap()
    }

    #[test]
    fn board_and_log_show_the_effort_next_to_the_model() {
        let (mut store, config) = store();
        let new = NewTask {
            request: TaskRequest {
                project: ProjectName::new("me-sim").unwrap(),
                slug: Slug::new("deep").unwrap(),
                kind: "scene".to_owned(),
                model: Some(headwork_core::Model::Opus),
                effort: Some(headwork_core::Effort::Xhigh),
                in_main: false,
                keep: false,
            },
            brief_path: "brief.md".to_owned(),
            depends_on: None,
            permission_mode: PermissionMode::Auto,
            shape: headwork_core::TaskShape::Plain,
        };
        store
            .create_task(&config, &new, Actor::Orchestrator)
            .unwrap();
        let text = super::board(
            &store.board(false).unwrap(),
            10,
            &headwork_core::usage::UsageConfig::default(),
        );
        assert!(text.contains("opus   xhigh  "), "{text}");
        let log = super::events(
            &store
                .events(EventFilter {
                    task: None,
                    limit: 50,
                })
                .unwrap(),
        );
        assert!(log.contains("model opus, effort xhigh"), "{log}");
    }

    fn shaped(
        store: &mut Store,
        config: &Config,
        slug: &str,
        shape: headwork_core::TaskShape<headwork_store::TaskId>,
    ) -> headwork_store::Task {
        let new = NewTask {
            request: TaskRequest {
                project: ProjectName::new("me-sim").unwrap(),
                slug: Slug::new(slug).unwrap(),
                kind: "task".to_owned(),
                model: None,
                effort: None,
                in_main: false,
                keep: false,
            },
            brief_path: "brief.md".to_owned(),
            depends_on: None,
            permission_mode: PermissionMode::Auto,
            shape,
        };
        store
            .create_task(config, &new, Actor::Orchestrator)
            .unwrap()
    }

    #[test]
    fn board_prints_each_lead_with_its_parts_under_it() {
        let (mut store, config) = store();
        let lead = shaped(&mut store, &config, "big", headwork_core::TaskShape::Lead);
        task(&mut store, &config, "me-sim", "other");
        let part = headwork_core::TaskShape::Part { parent: lead.id };
        shaped(&mut store, &config, "big-review", part);
        let text = super::board(
            &store.board(false).unwrap(),
            10,
            &headwork_core::usage::UsageConfig::default(),
        );
        let rows: Vec<&str> = text.lines().filter(|line| line.contains(" #")).collect();
        assert_eq!(rows.len(), 3, "{text}");
        assert!(
            rows[0].contains("me-sim-big ") && rows[0].contains("(lead)"),
            "{text}"
        );
        assert!(
            rows[1].trim_start().starts_with("└ #3") && rows[1].contains("me-sim-big-review"),
            "{text}"
        );
        assert!(rows[2].contains("me-sim-other"), "{text}");
        let lines = super::statuses(
            &headwork::ops::status::status(
                &store,
                false,
                &headwork_core::usage::UsageConfig::default(),
            )
            .unwrap(),
        );
        assert_eq!(
            lines, "me-sim-big: queued\nme-sim-other: queued\n",
            "one line per top-level task"
        );
    }

    #[test]
    fn board_shows_whether_a_session_has_a_pane_or_a_tab() {
        let (mut store, config) = store();
        let mut place = |slug: &str, pane: Option<&str>, kind: Option<PlaceKind>| {
            let made = task(&mut store, &config, "me-sim", slug);
            let id = format!("00000000-0000-0000-0000-00000000000{}", made.id.0);
            store
                .record_session(
                    &NewSession {
                        id: id.clone(),
                        task_id: Some(made.id),
                        role: Role::Worker,
                        rc_name: made.name.to_string(),
                        host: "wmux".to_owned(),
                        host_workspace: None,
                        host_surface: None,
                    },
                    Actor::User,
                )
                .unwrap();
            let surface = format!("surf-{slug}");
            store
                .set_session_place(
                    &id,
                    &Place {
                        workspace: "ws-1",
                        surface: &surface,
                        pane,
                        kind,
                    },
                    Actor::User,
                )
                .unwrap();
        };
        place("own", Some("pane-1"), Some(PlaceKind::Pane));
        place("stacked", Some("pane-1"), Some(PlaceKind::Tab));
        place("tabbed", None, Some(PlaceKind::Tab));
        let text = super::board(
            &store.board(false).unwrap(),
            10,
            &headwork_core::usage::UsageConfig::default(),
        );
        assert!(text.contains("surf-own in its own pane pane-1"), "{text}");
        assert!(text.contains("surf-stacked as a tab of pane-1"), "{text}");
        assert!(text.contains("surf-tabbed as a tab\n"), "{text}");
    }

    #[test]
    fn board_shows_a_lead_in_its_own_workspace() {
        let (mut store, config) = store();
        let lead = shaped(&mut store, &config, "big", headwork_core::TaskShape::Lead);
        let id = "00000000-0000-0000-0000-0000000000aa".to_owned();
        store
            .record_session(
                &NewSession {
                    id: id.clone(),
                    task_id: Some(lead.id),
                    role: Role::Worker,
                    rc_name: lead.name.to_string(),
                    host: "wmux".to_owned(),
                    host_workspace: None,
                    host_surface: None,
                },
                Actor::User,
            )
            .unwrap();
        store
            .set_session_place(
                &id,
                &Place {
                    workspace: "ws-lead",
                    surface: "surf-lead",
                    pane: Some("pane-9"),
                    kind: Some(PlaceKind::Pane),
                },
                Actor::User,
            )
            .unwrap();
        let text = super::board(
            &store.board(false).unwrap(),
            10,
            &headwork_core::usage::UsageConfig::default(),
        );
        assert!(
            text.contains("surf-lead in its own workspace ws-lead"),
            "{text}"
        );
        assert!(!text.contains("pane-9"), "{text}");
    }

    #[test]
    fn board_lists_projects_tasks_and_asks() {
        let (mut store, config) = store();
        let sand = task(&mut store, &config, "me-sim", "sand-cascade");
        let ask = NewAsk {
            task_id: Some(sand.id),
            session_id: None,
            kind: AskKind::Review,
            question: "Ship it?".to_owned(),
            options: vec!["Good, ship it".to_owned(), "Drop it".to_owned()],
        };
        store.open_ask(&ask, Actor::Hook).unwrap();
        let text = super::board(
            &store.board(false).unwrap(),
            10,
            &headwork_core::usage::UsageConfig::default(),
        );
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.first().copied(),
            Some("machine normal · 0 of 10 slots · 1 ask waiting"),
            "{text}"
        );
        assert!(text.contains("me-sim (S:/Code/me-sim/me-sim)"), "{text}");
        assert!(
            text.contains("#1   me-sim-sand-cascade  queued      sonnet ?      scene"),
            "{text}"
        );
        assert!(
            text.contains("ask 1 review open: Ship it? [1 Good, ship it] [2 Drop it]"),
            "{text}"
        );
        assert!(
            text.contains("work-service (~/work/service)\n  no tasks"),
            "{text}"
        );
    }

    #[test]
    fn log_describes_transitions_and_refusals() {
        let (mut store, config) = store();
        let sand = task(&mut store, &config, "me-sim", "sand");
        assert!(
            store
                .transition(sand.id, &Trigger::Park, Actor::Orchestrator)
                .is_err(),
            "queued cannot park"
        );
        store.start_task(&config, sand.id, Actor::Tick).unwrap();
        let text = super::events(
            &store
                .events(EventFilter {
                    task: Some(sand.id),
                    limit: 10,
                })
                .unwrap(),
        );
        assert!(
            text.contains("orchestrator me-sim-sand  refused          park refused in queued:"),
            "{text}"
        );
        assert!(
            text.contains("tick         me-sim-sand  transition       start: queued -> spawning"),
            "{text}"
        );
        assert_eq!(super::events(&[]), "no events\n", "empty log");
    }
}
