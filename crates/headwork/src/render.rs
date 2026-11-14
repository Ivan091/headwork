//! Plain-text views of the board and the event log.

use std::fmt::Write;

use headwork::ops::import::ImportReport;
use headwork::ops::park::{ParkReport, RecapSource};
use headwork::ops::resume::{ResumeReport, TaskOutcome};
use headwork::ops::spawn::{Outcome, Pumped};
use headwork::ops::tick::TickReport;
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
    let _ = writeln!(text, "{verb} {} running children", report.adopted.len());
    for name in &report.adopted {
        let _ = writeln!(text, "  {name}");
    }
    for line in &report.skipped {
        let _ = writeln!(text, "  skipped {line}");
    }
    if !report.stray_worktrees.is_empty() {
        text.push_str("worktrees without a running child\n");
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
pub(crate) fn board(board: &Board, max_children: u32) -> String {
    let asks = board
        .tasks
        .iter()
        .map(|task| task.asks.len())
        .sum::<usize>()
        + board.other_asks.len();
    let mut text = format!(
        "machine {} · {} of {max_children} slots · {asks} {} waiting\n",
        board.machine,
        board.live_children,
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
        for task in tasks {
            task_line(&mut text, task, width);
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

fn task_line(text: &mut String, task: &BoardTask, width: usize) {
    let t = &task.task;
    let name = t.name.to_string();
    let _ = write!(
        text,
        "  #{:<3} {name:<width$}  {:<11} {:<6} {}",
        t.id.0,
        t.state.as_str(),
        t.model,
        t.kind
    );
    if t.in_main {
        text.push_str(" (main)");
    }
    if t.keep {
        text.push_str(" (keep)");
    }
    if let Some(session) = &task.session {
        let _ = write!(text, " · session {}", session.state);
        if let Some(surface) = &session.host_surface {
            let _ = write!(text, " {surface}");
        }
    }
    if let Some(pr) = &task.pr {
        let _ = write!(text, " · PR {} {}", pr.state, pr.url);
    }
    text.push('\n');
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
            "{}  {:<5} {task:<width$}  {:<16} {}",
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
    if report.lines.is_empty() && report.started.is_empty() {
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
    for job in &report.jobs {
        let _ = writeln!(text, "  job {}: {}", job.label, job.outcome);
    }
    if report.sessions.is_empty() && report.jobs.is_empty() {
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
    for task in &report.tasks {
        let line = match &task.outcome {
            TaskOutcome::Resumed { surface } => format!("resumed in {surface}"),
            TaskOutcome::Fresh { surface, reason } => {
                format!("started fresh in {surface} ({reason})")
            }
            TaskOutcome::Skipped { reason } => format!("skipped: {reason}"),
            TaskOutcome::Failed { error } => format!("FAILED: {error}"),
        };
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
    let _ = writeln!(text, "summary for the root: {}", report.summary);
    text
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

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]
mod tests {
    use std::path::Path;

    use headwork_core::{
        Actor, AskKind, Config, PermissionMode, ProjectName, Slug, TaskRequest, Trigger,
    };
    use headwork_store::{EventFilter, NewAsk, NewTask, Store};

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
                in_main: false,
                keep: false,
            },
            brief_path: "brief.md".to_owned(),
            depends_on: None,
            permission_mode: PermissionMode::Auto,
        };
        store.create_task(config, &new, Actor::Root).unwrap()
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
        let text = super::board(&store.board(false).unwrap(), 10);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.first().copied(),
            Some("machine normal · 0 of 10 slots · 1 ask waiting"),
            "{text}"
        );
        assert!(text.contains("me-sim (S:/Code/me-sim/me-sim)"), "{text}");
        assert!(
            text.contains("#1   me-sim-sand-cascade  queued      sonnet scene"),
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
                .transition(sand.id, &Trigger::Park, Actor::Root)
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
            text.contains("root  me-sim-sand  refused          park refused in queued:"),
            "{text}"
        );
        assert!(
            text.contains("tick  me-sim-sand  transition       start: queued -> spawning"),
            "{text}"
        );
        assert_eq!(super::events(&[]), "no events\n", "empty log");
    }
}
