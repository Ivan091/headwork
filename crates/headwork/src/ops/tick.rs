//! `tick`: the watchdog and reconciler of design section 3.4. One run compares the store with reality and records
//! what it finds.
//!
//! Recording what happened is bookkeeping and always runs: a spawn that timed out, a pane that vanished, a pull
//! request that merged, the queue, answers to the tick's own asks. Ending things (processes, merged tasks past their
//! grace, runaway writers) is enforcement and runs only with `hooks.enforce`; in shadow mode the tick reports each as
//! a `shadow` finding and acts on none. A finding that stands is logged once and cleared when it ends.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use headwork_adapters::disk::{Disks, large_files, volume_of};
use headwork_adapters::git::GitHost;
use headwork_adapters::host::{Choice, Key, SurfaceId, WorkspaceId};
use headwork_adapters::proc;
use headwork_core::lifecycle::{CloseCause, CloseFacts, MergeSource, PrEnd};
use headwork_core::update::{self, Version};
use headwork_core::watch::{self, DiskLevel, Limits, MergedSession, Runaway, Stuck};
use headwork_core::{
    Actor, AskState, FindingId, MachineMode, SessionState, TaskShape, TaskState, Trigger,
};
use headwork_store::{SessionRow, Store, Task, TaskId};
use serde::Serialize;

use super::close::{finish, remove_worktree, surface_present};
use super::lost::{self, LostCause, RUNNING};
use super::rules::{self, fnv1a};
use super::spawn::{Pumped, pump};
use super::update::{
    Clock as _, SystemClock, UpdateRequest, UpdateStarter, UpdateState, UpdateTrigger,
};
use super::{Env, slashed};

/// The topic of the ask about a worktree that cannot be removed.
pub(super) const WORKTREE_TOPIC: &str = "worktree";

/// The topic once its answer was carried out.
const WORKTREE_DONE: &str = "worktree-handled";

/// The answer that keeps the worktree.
pub const KEEP_WORKTREE: &str = "Keep the worktree (Recommended)";

/// The answer that deletes it, changes and all.
pub const DELETE_WORKTREE: &str = "Delete it";

/// The topic of the ask about a session stopped at Claude Code's folder trust prompt.
const TRUST_TOPIC: &str = "trust";

/// The topic once the prompt went away or its answer was carried out.
const TRUST_DONE: &str = "trust-handled";

/// The answer that trusts the folder; the pane's blocked state sends `1`, the dialog's "Yes".
pub const TRUST_FOLDER: &str = "Trust the folder (Recommended)";

/// The answer that does not; the pane's blocked state sends `2`, the dialog's "No, exit".
pub const DONT_TRUST_FOLDER: &str = "Do not trust it: the session exits";

/// Carries out the answer to a task's `worktree` ask, once.
///
/// `Keep` records the worktree `kept`, `Delete` removes it with its changes. Nothing else deletes a worktree that has
/// changes, and an ask with no answer changes nothing.
///
/// # Errors
///
/// Returns the store's or the remove command's failure.
pub fn carry_out_worktree_answer(store: &mut Store, env: &Env<'_>, id: TaskId) -> Result<()> {
    let Some(ask) = store.topic_ask(id, WORKTREE_TOPIC)? else {
        return Ok(());
    };
    let (AskState::Answered, Some(answer)) = (ask.state, ask.answer) else {
        return Ok(());
    };
    if answer.starts_with("Keep") {
        store.set_worktree_state(id, "kept", env.actor)?;
    } else if answer.starts_with("Delete") {
        remove_worktree(store, env, id, true)?;
    }
    store.set_ask_topic(ask.id, WORKTREE_DONE)?;
    Ok(())
}

/// What the tick acts with besides the store.
#[derive(Debug, Clone, Copy)]
pub struct TickEnv<'a> {
    /// The host, config and actor.
    pub env: &'a Env<'a>,
    /// The git host.
    pub git: &'a dyn GitHost,
    /// The machine's disks.
    pub disks: &'a dyn Disks,
    /// The temp folder.
    pub temp: &'a Path,
    /// The thresholds; [`Limits::from_config`] in production.
    pub limits: Limits,
    /// Starts `headwork update`; [`super::update::DetachedUpdates`] in production.
    pub updates: &'a dyn UpdateStarter,
}

/// What the tick did about a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// The tick acted, or recorded the change.
    Applied,
    /// Enforcement is off: the tick would have acted.
    Shadow,
    /// A condition the tick reports and leaves to someone else.
    Standing,
}

/// One finding of a tick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Line {
    /// The rule.
    pub rule: FindingId,
    /// The task, if the finding is about one.
    pub task: Option<String>,
    /// What was done.
    pub status: Status,
    /// The details.
    pub detail: String,
}

/// What one tick did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TickReport {
    /// Whether enforcement was on.
    pub enforce: bool,
    /// Whether the terminal host answered.
    pub host_up: bool,
    /// The queue was held back because a disk is nearly full.
    pub queue_paused: bool,
    /// The findings of this run.
    pub lines: Vec<Line>,
    /// What the queue started.
    pub started: Vec<Pumped>,
}

/// Runs one tick.
///
/// # Errors
///
/// Fails when the store fails; a failing host, git host or disk is reported as a finding instead.
pub fn tick(store: &mut Store, tick_env: &TickEnv<'_>) -> Result<TickReport> {
    let env = tick_env.env;
    let mut run = Run {
        store,
        t: tick_env,
        limits: tick_env.limits,
        enforce: env.config.hooks.enforce,
        seen: BTreeMap::new(),
        stopped: Vec::new(),
        report: TickReport {
            enforce: env.config.hooks.enforce,
            host_up: env.host.ping().is_ok(),
            queue_paused: false,
            lines: Vec::new(),
            started: Vec::new(),
        },
    };
    run.rules()?;
    run.trust_prompts()?;
    run.spawn_timeouts()?;
    run.lost_sessions()?;
    run.orphan_panes()?;
    run.titles()?;
    run.pull_requests()?;
    run.merged_grace()?;
    run.closing()?;
    run.parts()?;
    run.disks()?;
    run.runaway()?;
    run.stuck()?;
    run.asks()?;
    run.queue()?;
    run.updates()?;
    run.store.record_tick()?;
    Ok(run.report)
}

struct Run<'a, 'b> {
    store: &'a mut Store,
    t: &'a TickEnv<'b>,
    limits: Limits,
    enforce: bool,
    seen: BTreeMap<FindingId, Vec<String>>,
    stopped: Vec<String>,
    report: TickReport,
}

impl<'b> Run<'_, 'b> {
    const fn env(&self) -> &'b Env<'b> {
        self.t.env
    }

    const fn actor(&self) -> Actor {
        self.t.env.actor
    }

    fn name(&self, id: TaskId) -> Result<String> {
        Ok(self.store.task(id)?.name.to_string())
    }

    fn applied(
        &mut self,
        rule: FindingId,
        task: Option<TaskId>,
        detail: serde_json::Value,
    ) -> Result<()> {
        let name = task.map(|id| self.name(id)).transpose()?;
        let session = match task {
            Some(id) => self.store.session_of(id)?.map(|s| s.id),
            None => None,
        };
        let text = detail.to_string();
        let mut data = serde_json::Map::new();
        data.insert("rule".to_owned(), serde_json::to_value(rule)?);
        data.insert("detail".to_owned(), detail);
        let data = serde_json::Value::Object(data);
        self.store
            .append_event(self.actor(), task, session.as_deref(), "finding", &data)?;
        self.report.lines.push(Line {
            rule,
            task: name,
            status: Status::Applied,
            detail: text,
        });
        Ok(())
    }

    fn standing(
        &mut self,
        key: &str,
        rule: FindingId,
        task: Option<TaskId>,
        status: Status,
        detail: &serde_json::Value,
        notice: Option<&str>,
    ) -> Result<bool> {
        let name = task.map(|id| self.name(id)).transpose()?;
        self.seen.entry(rule).or_default().push(key.to_owned());
        let actor = self.actor();
        let is_new = self.store.raise_finding(key, rule, task, detail, actor)?;
        if is_new && let Some(text) = notice {
            self.notify(text);
        }
        self.report.lines.push(Line {
            rule,
            task: name,
            status,
            detail: detail.to_string(),
        });
        Ok(is_new)
    }

    fn sweep(&mut self, rule: FindingId) -> Result<()> {
        let still = self.seen.remove(&rule).unwrap_or_default();
        let actor = self.actor();
        self.store.resolve_findings(rule, &still, actor)?;
        Ok(())
    }

    fn notify(&self, text: &str) {
        if self.report.host_up {
            drop(self.env().host.notify(text));
        }
    }

    fn error(&mut self, key: &str, task: Option<TaskId>, message: &str) -> Result<()> {
        self.standing(
            &format!("tick-error:{key}"),
            FindingId::TickError,
            task,
            Status::Standing,
            &serde_json::json!({ "error": message }),
            None,
        )
        .map(drop)
    }

    fn end_processes(session: &SessionRow) -> Result<bool> {
        let mut ended = false;
        for pid in [session.claude_pid, session.launcher_pid]
            .into_iter()
            .flatten()
        {
            if proc::is_alive(pid)? {
                proc::kill_tree(pid)?;
                ended = true;
            }
        }
        Ok(ended)
    }

    fn rules(&mut self) -> Result<()> {
        let env = self.env();
        let files = rules::current(self.store, env.home, env.config, env.host)?;
        let actor = self.actor();
        self.store.observe_rules(files.hash(), actor)?;
        for (task, session, seen) in self.store.sessions_behind_rules()? {
            let version = self.store.rules_version()?.version;
            self.standing(
                &format!("rules-stale:{session}"),
                FindingId::RulesStale,
                Some(task),
                Status::Standing,
                &serde_json::json!({ "session": session, "seen": seen, "current": version }),
                None,
            )?;
        }
        self.sweep(FindingId::RulesStale)
    }

    fn spawn_timeouts(&mut self) -> Result<()> {
        for id in self.store.tasks_in(TaskState::Spawning)? {
            if self.waiting_on_trust(id)? {
                continue;
            }
            let session = self.store.session_of(id)?;
            let age = match &session {
                Some(session) => self.store.session_clock(&session.id)?.started_age_s,
                None => self.store.task_age_s(id)?,
            };
            if !watch::spawn_timed_out(age, &self.limits) {
                continue;
            }
            let actor = self.actor();
            if self
                .store
                .transition(id, &Trigger::SpawnTimedOut, actor)
                .is_err()
            {
                continue;
            }
            if let Some(session) = &session {
                self.store
                    .set_session_state(&session.id, SessionState::Gone, actor)?;
                if self.enforce {
                    let shape = self.store.task(id)?.shape;
                    self.end_session(shape, session)?;
                }
            }
            self.applied(
                FindingId::SpawnTimeout,
                Some(id),
                serde_json::json!({ "after_s": age }),
            )?;
        }
        Ok(())
    }

    fn waiting_on_trust(&self, id: TaskId) -> Result<bool> {
        Ok(self
            .store
            .topic_ask(id, TRUST_TOPIC)?
            .is_some_and(|ask| ask.state.is_waiting()))
    }

    /// A starting session (launched or not, its task still spawning) whose pane shows Claude Code's folder trust
    /// dialog waits on the user, unseen: headwork
    /// trusts every project's main checkout before a session starts, so this is the fallback. The tick raises it as
    /// a `trust` ask, a blocked state on the pane whose buttons answer the dialog, and a notification, and holds back
    /// the spawn timeout meanwhile. Once the dialog is gone the ask is retired; an answer given through the store is
    /// carried out by sending the dialog's key.
    fn trust_prompts(&mut self) -> Result<()> {
        if !self.report.host_up {
            return Ok(());
        }
        for (id, surface) in self.store.live_task_surfaces()? {
            let Some(session) = self.store.session_of(id)? else {
                continue;
            };
            let starting = session.state == SessionState::Starting
                || (session.state == SessionState::Live
                    && self.store.task(id)?.state == TaskState::Spawning);
            let surface = SurfaceId(surface);
            if !starting {
                self.retire_trust_ask(id, &surface)?;
                continue;
            }
            let screen = match self.env().host.read_screen(&surface, 40) {
                Ok(screen) => screen,
                Err(error) => {
                    self.error(
                        &format!("trust-screen:{}", session.id),
                        Some(id),
                        &error.to_string(),
                    )?;
                    continue;
                }
            };
            if !watch::trust_prompt(&screen) {
                self.retire_trust_ask(id, &surface)?;
                continue;
            }
            let ask = self.store.topic_ask(id, TRUST_TOPIC)?;
            if let Some(ask) = &ask
                && let (AskState::Answered, Some(answer)) = (ask.state, ask.answer.as_deref())
            {
                let key = if answer.starts_with("Trust") {
                    '1'
                } else {
                    '2'
                };
                self.env().host.send_key(&surface, Key::Char(key))?;
                self.store.set_ask_topic(ask.id, TRUST_DONE)?;
                drop(self.env().host.clear_blocked(&surface));
                continue;
            }
            let name = self.name(id)?;
            let dir = self.env().task_dir(&self.store.task(id)?)?;
            let question = format!(
                "{name} stopped at Claude Code's \"Do you trust this folder?\" prompt for {}. Trust it?",
                slashed(&dir)
            );
            if !ask.as_ref().is_some_and(|ask| ask.state.is_waiting()) {
                let options = [TRUST_FOLDER.to_owned(), DONT_TRUST_FOLDER.to_owned()];
                let actor = self.actor();
                self.store
                    .open_topic_ask(id, TRUST_TOPIC, &question, &options, actor)?;
                let choices = [
                    Choice {
                        id: "trust".to_owned(),
                        label: format!("1 {TRUST_FOLDER}"),
                        key: "1".to_owned(),
                    },
                    Choice {
                        id: "exit".to_owned(),
                        label: format!("2 {DONT_TRUST_FOLDER}"),
                        key: "2".to_owned(),
                    },
                ];
                drop(self.env().host.show_blocked(&surface, &question, &choices));
            }
            self.standing(
                &format!("trust-prompt:{}", session.id),
                FindingId::TrustPrompt,
                Some(id),
                Status::Standing,
                &serde_json::json!({ "dir": slashed(&dir) }),
                Some(&question),
            )?;
        }
        self.sweep(FindingId::TrustPrompt)
    }

    /// Retires a task's waiting `trust` ask and clears its pane's blocked state: the dialog is gone.
    fn retire_trust_ask(&mut self, id: TaskId, surface: &SurfaceId) -> Result<()> {
        if let Some(ask) = self
            .store
            .topic_ask(id, TRUST_TOPIC)?
            .filter(|ask| ask.state.is_waiting())
        {
            let actor = self.actor();
            self.store.supersede_ask(ask.id, id, actor)?;
            self.store.set_ask_topic(ask.id, TRUST_DONE)?;
            drop(self.env().host.clear_blocked(surface));
        }
        Ok(())
    }

    fn end_session(&self, shape: TaskShape<TaskId>, session: &SessionRow) -> Result<()> {
        Self::end_processes(session)?;
        if let Some(surface) = session.host_surface.as_deref() {
            let workspace = session
                .host_workspace
                .as_deref()
                .map(|w| WorkspaceId(w.to_owned()));
            let surface = SurfaceId(surface.to_owned());
            if self.report.host_up
                && surface_present(self.env().host, workspace.as_ref(), &surface)?
            {
                super::close::close_surface_or_workspace(
                    self.env(),
                    shape,
                    workspace.as_ref(),
                    &surface,
                )?;
            }
        }
        Ok(())
    }

    fn lost_sessions(&mut self) -> Result<()> {
        if self.store.machine_mode()? == MachineMode::Parking {
            return Ok(());
        }
        for state in RUNNING {
            for id in self.store.tasks_in(state)? {
                let Some(session) = self.store.session_of(id)? else {
                    continue;
                };
                let Some(cause) = self.lost_cause(id, &session)? else {
                    continue;
                };
                if !lost::mark(self.store, self.env(), id, &session, cause)? {
                    continue;
                }
                self.applied(
                    FindingId::PaneLost,
                    Some(id),
                    serde_json::json!({ "from": state, "lost": cause.as_str() }),
                )?;
            }
        }
        Ok(())
    }

    fn lost_cause(&self, id: TaskId, session: &SessionRow) -> Result<Option<LostCause>> {
        if let Some(cause) = lost::local_cause(session)? {
            return Ok(Some(cause));
        }
        if self.report.host_up
            && let Some(surface) = session.host_surface.as_deref()
        {
            let part = self.store.task(id)?.shape.is_part();
            let workspace = session
                .host_workspace
                .as_deref()
                .filter(|_| !part)
                .map(|w| WorkspaceId(w.to_owned()));
            let surface = SurfaceId(surface.to_owned());
            if !surface_present(self.env().host, workspace.as_ref(), &surface)? {
                return Ok(Some(LostCause::Pane));
            }
        }
        Ok(None)
    }

    fn titles(&self) -> Result<()> {
        if self.report.host_up {
            super::title::retitle(self.store, self.env(), None)?;
        }
        Ok(())
    }

    fn orphan_panes(&mut self) -> Result<()> {
        if !self.report.host_up {
            return Ok(());
        }
        let owned = self.store.owned_surfaces()?;
        for pane in self.env().host.panes()? {
            if !pane.running || owned.iter().any(|s| s == pane.surface.as_str()) {
                continue;
            }
            let Some((project, _)) = pane.label.split_once('/') else {
                continue;
            };
            if self
                .env()
                .config
                .projects
                .keys()
                .all(|name| name.as_str() != project)
            {
                continue;
            }
            self.standing(
                &format!("orphan-pane:{}", pane.surface),
                FindingId::OrphanPane,
                None,
                Status::Standing,
                &serde_json::json!({ "label": pane.label, "surface": pane.surface.as_str() }),
                Some(&format!(
                    "Pane {} belongs to no task: adopt or close it",
                    pane.label
                )),
            )?;
        }
        self.sweep(FindingId::OrphanPane)
    }

    fn pull_requests(&mut self) -> Result<()> {
        for state in [TaskState::Working, TaskState::InReview, TaskState::Merged] {
            for id in self.store.tasks_in(state)? {
                let task = self.store.task(id)?;
                if task.in_main {
                    continue;
                }
                if let Err(error) = self.reconcile_pr(&task) {
                    self.error(
                        &format!("pr:{}", task.name),
                        Some(id),
                        &format!("{error:#}"),
                    )?;
                }
            }
        }
        self.sweep(FindingId::TickError)
    }

    fn reconcile_pr(&mut self, task: &Task) -> Result<()> {
        let env = self.env();
        let git = self.t.git;
        let found = super::prs::lookup(self.store, env, git, task)?;
        let end = found.end();
        let Some(pr) = found.deciding() else {
            return Ok(());
        };
        let project = env.project(task)?;
        let repo_allowed = project
            .allowed_repos
            .iter()
            .any(|repo| repo.as_str().eq_ignore_ascii_case(&pr.repo));
        let policy = project.merge;
        let actor = self.actor();
        let mut state = task.state;
        let detail = serde_json::json!({ "url": pr.url, "number": pr.number });
        let opens = matches!(
            (state, end),
            (TaskState::Working, PrEnd::Open | PrEnd::Merged) | (TaskState::Merged, PrEnd::Open)
        );
        if opens {
            if self
                .store
                .transition(task.id, &Trigger::ReviewOpened { repo_allowed }, actor)
                .is_err()
            {
                return Ok(());
            }
            state = TaskState::InReview;
            if end == PrEnd::Open {
                self.applied(FindingId::PrOpened, Some(task.id), detail.clone())?;
            }
        }
        let merged = Trigger::Merged {
            source: MergeSource::GitHost,
            policy,
        };
        let moved = match (state, end) {
            (TaskState::InReview, PrEnd::Merged) => self
                .store
                .transition(task.id, &merged, actor)
                .is_ok()
                .then_some(FindingId::PrMerged),
            (TaskState::InReview, PrEnd::Closed) => self
                .store
                .transition(task.id, &Trigger::ChangesRequested, actor)
                .is_ok()
                .then_some(FindingId::PrClosed),
            _ => None,
        };
        if let Some(rule) = moved {
            self.applied(rule, Some(task.id), detail)?;
        }
        Ok(())
    }

    fn merged_grace(&mut self) -> Result<()> {
        for id in self.store.tasks_in(TaskState::Merged)? {
            let task = self.store.task(id)?;
            let age = self.store.task_age_s(id)?;
            let session = match self.store.session_of(id)? {
                Some(session)
                    if matches!(session.state, SessionState::Starting | SessionState::Live) =>
                {
                    let clock = self.store.session_clock(&session.id)?;
                    MergedSession::Live {
                        idle_s: clock.idle_age_s,
                        tool_in_flight: clock.tool_in_flight,
                    }
                }
                Some(_) | None => MergedSession::Gone,
            };
            if task.keep || !watch::merged_grace_over(age, session, &self.limits) {
                continue;
            }
            let facts = CloseFacts {
                cause: CloseCause::GraceElapsed,
                open_ask: self.store.has_waiting_ask(id)?,
                running_job: super::jobs::has_running_job(self.store, id)?,
                report_sent: self.store.report_sent(id)?,
                live_parts: self.store.live_parts(id)?,
                go: super::close::close_go(self.store, self.env().config, &task)?,
            };
            let key = format!("merged-grace:{}", task.name);
            if !self.enforce {
                let ready = headwork_core::apply(&task.snapshot(), &Trigger::Close(facts)).is_ok();
                if ready {
                    self.standing(
                        &key,
                        FindingId::MergedGrace,
                        Some(id),
                        Status::Shadow,
                        &serde_json::json!({ "would": "close", "age_s": age }),
                        None,
                    )?;
                }
                continue;
            }
            let actor = self.actor();
            if self
                .store
                .transition(id, &Trigger::Close(facts), actor)
                .is_err()
            {
                continue;
            }
            if let Some(session) = self.store.session_of(id)? {
                let target = session.claude_pid.or(session.launcher_pid);
                if let Some(pid) = target
                    && proc::is_alive(pid)?
                {
                    proc::kill_tree(pid)?;
                }
            }
            self.applied(
                FindingId::MergedGrace,
                Some(id),
                serde_json::json!({ "age_s": age }),
            )?;
        }
        self.sweep(FindingId::MergedGrace)
    }

    fn closing(&mut self) -> Result<()> {
        let actor = self.actor();
        if self.enforce {
            let dropped = self.store.tasks_in(TaskState::Dropped)?;
            let mut parts_first = Vec::with_capacity(dropped.len());
            for id in dropped {
                parts_first.push((!self.store.task(id)?.shape.is_part(), id));
            }
            parts_first.sort();
            for (_, id) in parts_first {
                if self
                    .store
                    .transition(id, &Trigger::BeginClosing, actor)
                    .is_ok()
                {
                    self.applied(
                        FindingId::LeftoverProcess,
                        Some(id),
                        serde_json::json!({ "dropped": true }),
                    )?;
                }
            }
        }
        for id in self.store.tasks_in(TaskState::Closing)? {
            self.close_one(id)?;
        }
        self.sweep(FindingId::LeftoverProcess)
    }

    fn close_one(&mut self, id: TaskId) -> Result<()> {
        let session = self.store.session_of(id)?;
        let alive = session.as_ref().is_some_and(|session| {
            [session.claude_pid, session.launcher_pid]
                .into_iter()
                .flatten()
                .any(|pid| proc::is_alive(pid).unwrap_or(true))
        });
        if alive {
            let age = self.store.task_age_s(id)?;
            if age < self.limits.closing_grace {
                return Ok(());
            }
            let detail = serde_json::json!({ "closing_for_s": age });
            if let Some(session) = session.filter(|_| self.enforce) {
                Self::end_processes(&session)?;
                return self.applied(FindingId::LeftoverProcess, Some(id), detail);
            }
            let key = format!("leftover:{}", self.name(id)?);
            let shadow = serde_json::json!({ "would": "end processes", "closing_for_s": age });
            return self
                .standing(
                    &key,
                    FindingId::LeftoverProcess,
                    Some(id),
                    Status::Shadow,
                    &shadow,
                    None,
                )
                .map(drop);
        }
        carry_out_worktree_answer(self.store, self.env(), id)?;
        let report = finish(self.store, self.env(), id)?;
        if report.pending.is_some() {
            self.ask_about_worktree(id)?;
        }
        Ok(())
    }

    /// Parts that lost their lead (`orphan-part`) and parts whose tab the user moved out of the lead's workspace
    /// (`stray-tab`). Both are reported, never acted on: headwork does not move a tab back.
    fn parts(&mut self) -> Result<()> {
        for state in TaskState::ALL {
            if matches!(state, TaskState::Closing | TaskState::Closed) {
                continue;
            }
            for id in self.store.tasks_in(*state)? {
                let part = self.store.task(id)?;
                let TaskShape::Part { parent } = part.shape else {
                    continue;
                };
                let lead = self.store.task(parent)?;
                if headwork_core::lead::orphaned(part.state, lead.state) {
                    self.standing(
                        &format!("orphan-part:{}", part.name),
                        FindingId::OrphanPart,
                        Some(id),
                        Status::Standing,
                        &serde_json::json!({ "lead": lead.name.to_string(), "lead_state": lead.state }),
                        Some(&format!(
                            "Part {} is still open while its lead {} is {}: close or drop it",
                            part.name, lead.name, lead.state
                        )),
                    )?;
                }
                if self.report.host_up && RUNNING.contains(&part.state) {
                    self.stray_tab(&part, parent)?;
                }
            }
        }
        self.sweep(FindingId::OrphanPart)?;
        self.sweep(FindingId::StrayTab)
    }

    fn stray_tab(&mut self, part: &Task, lead: TaskId) -> Result<()> {
        let Some(surface) = self
            .store
            .session_of(part.id)?
            .and_then(|session| session.host_surface)
            .map(SurfaceId)
        else {
            return Ok(());
        };
        let Ok(workspace) = super::place::lead_workspace(self.store, self.env(), lead) else {
            return Ok(());
        };
        let host = self.env().host;
        if host.surfaces(&workspace)?.contains(&surface) || !surface_present(host, None, &surface)?
        {
            return Ok(());
        }
        let lead_name = self.name(lead)?;
        self.standing(
            &format!("stray-tab:{}", part.name),
            FindingId::StrayTab,
            Some(part.id),
            Status::Standing,
            &serde_json::json!({
                "surface": surface.as_str(),
                "lead": lead_name,
                "workspace": workspace.as_str(),
            }),
            Some(&format!(
                "The tab of part {} is outside its lead's workspace {lead_name}; headwork does not move it back",
                part.name
            )),
        )
        .map(drop)
    }

    fn ask_about_worktree(&mut self, id: TaskId) -> Result<()> {
        let present = self
            .store
            .worktree_of(id)?
            .is_some_and(|w| w.state == "present" && Path::new(&w.path).exists());
        if !present || self.store.topic_ask(id, WORKTREE_TOPIC)?.is_some() {
            return Ok(());
        }
        let name = self.name(id)?;
        let question = format!(
            "Keep or delete the worktree of {name}? It has changes the remove command refused to lose."
        );
        let options = [KEEP_WORKTREE.to_owned(), DELETE_WORKTREE.to_owned()];
        let actor = self.actor();
        self.store
            .open_topic_ask(id, WORKTREE_TOPIC, &question, &options, actor)?;
        self.notify(&question);
        self.applied(
            FindingId::WorktreeRefused,
            Some(id),
            serde_json::json!({ "asked": true }),
        )
    }

    fn live_dirs(&self) -> Result<Vec<(PathBuf, Task)>> {
        let mut out = Vec::new();
        for state in [
            TaskState::Spawning,
            TaskState::Closing,
            TaskState::Failed,
            TaskState::Interrupted,
        ]
        .into_iter()
        .chain(RUNNING)
        {
            for id in self.store.tasks_in(state)? {
                let task = self.store.task(id)?;
                if !task.in_main {
                    out.push((self.env().task_dir(&task)?, task));
                }
            }
        }
        Ok(out)
    }

    /// Reports a failing update as a standing finding (with a host notification when it is new), and starts a
    /// detached `headwork update` when `[update]` is on and `poll_min` passed since GitHub was asked. Updating is not
    /// an enforcement action, so it runs in shadow mode too (`docs/autoupdate.md`, "What starts it").
    fn updates(&mut self) -> Result<()> {
        let env = self.env();
        let state = UpdateState::load(env.home);
        if let Some(failure) = state.failure_to_report() {
            let tag = failure.tag.map(Version::tag);
            let key = format!(
                "update-failed:{}",
                tag.clone().unwrap_or_else(|| failure.step.clone())
            );
            let detail = serde_json::json!({
                "tag": tag,
                "step": failure.step,
                "kind": failure.kind.as_str(),
                "attempts": failure.attempts,
                "error": failure.error,
            });
            let notice = format!(
                "headwork update failed at {} ({}): {}",
                failure.step,
                tag.as_deref().unwrap_or("the newest release"),
                failure.error
            );
            self.standing(
                &key,
                FindingId::UpdateFailed,
                None,
                Status::Standing,
                &detail,
                Some(&notice),
            )?;
        }
        self.sweep(FindingId::UpdateFailed)?;
        let config = &env.config.update;
        let last = state.last_poll.as_deref().and_then(update::parse_utc);
        if !update::tick_polls(config.enabled, config.poll_min, last, SystemClock.now_ms())
            || super::update::running(env.home)?
        {
            return Ok(());
        }
        match super::update::start_detached(
            self.store,
            self.t.updates,
            env.home,
            env.exe,
            UpdateTrigger::Tick,
            UpdateRequest::default(),
            self.actor(),
        ) {
            Ok(started) => self.applied(
                FindingId::UpdateStarted,
                None,
                serde_json::json!({ "pid": started.pid, "note": started.note }),
            ),
            Err(error) => self.error("update", None, &format!("{error:#}")),
        }
    }

    fn disks(&mut self) -> Result<()> {
        let env = self.env();
        let mut paths = vec![
            env.home.root().to_path_buf(),
            self.t.temp.to_path_buf(),
            env.home.expand("~/.claude"),
        ];
        paths.extend(
            env.config
                .projects
                .values()
                .map(|p| env.home.expand(&p.main)),
        );
        paths.extend(self.live_dirs()?.into_iter().map(|(dir, _)| dir));
        let mut volumes: BTreeMap<String, PathBuf> = BTreeMap::new();
        for path in paths.into_iter().filter(|p| p.exists()) {
            volumes.entry(volume_of(&path)).or_insert(path);
        }
        for (volume, path) in volumes {
            match self.t.disks.free_bytes(&path) {
                Ok(free) => self.disk_level(&volume, free)?,
                Err(error) => self.error(&format!("disk:{volume}"), None, &error.to_string())?,
            }
        }
        self.report.queue_paused = !self.stopped.is_empty();
        self.sweep(FindingId::DiskWarn)?;
        self.sweep(FindingId::DiskStop)
    }

    fn disk_level(&mut self, volume: &str, free: u64) -> Result<()> {
        let gb = free / (1024 * 1024 * 1024);
        let detail = serde_json::json!({ "volume": volume, "free_gb": gb });
        match watch::disk_level(free, &self.limits) {
            DiskLevel::Ok => {}
            DiskLevel::Warn => {
                let notice = format!("Disk {volume} has {gb} GB left");
                self.standing(
                    &format!("disk-warn:{volume}"),
                    FindingId::DiskWarn,
                    None,
                    Status::Standing,
                    &detail,
                    Some(&notice),
                )?;
            }
            DiskLevel::Stop => {
                let notice = format!("Disk {volume} has {gb} GB left: the queue is paused");
                self.standing(
                    &format!("disk-stop:{volume}"),
                    FindingId::DiskStop,
                    None,
                    Status::Standing,
                    &detail,
                    Some(&notice),
                )?;
                self.stopped.push(volume.to_owned());
            }
        }
        Ok(())
    }

    fn runaway(&mut self) -> Result<()> {
        let dirs = self.live_dirs()?;
        let mut roots: Vec<PathBuf> = self
            .env()
            .config
            .watch
            .roots
            .iter()
            .map(|root| {
                self.env()
                    .home
                    .expand(&root.replace("{temp}", &slashed(self.t.temp)))
            })
            .chain(dirs.iter().map(|(dir, _)| dir.clone()))
            .filter(|root| root.is_dir())
            .collect();
        roots.sort();
        roots.dedup();
        let before = self.store.file_samples()?;
        let elapsed = self
            .store
            .last_tick_age_s()?
            .unwrap_or(watch::MIN_INTERVAL_S);
        let mut after = BTreeMap::new();
        for root in &roots {
            for (path, size) in large_files(root, self.limits.sample_floor()) {
                after.insert(slashed(&path), size);
            }
        }
        for (path, size) in &after {
            let Some(why) = watch::runaway(*size, before.get(path).copied(), elapsed, &self.limits)
            else {
                continue;
            };
            self.runaway_file(path, why, &dirs)?;
        }
        self.store.replace_file_samples(&after)?;
        self.sweep(FindingId::Runaway)
    }

    fn runaway_file(&mut self, path: &str, why: Runaway, dirs: &[(PathBuf, Task)]) -> Result<()> {
        let lower = path.to_lowercase();
        let owner = dirs.iter().find(|(dir, _)| {
            let dir = slashed(dir).to_lowercase();
            lower.starts_with(&format!("{}/", dir.trim_end_matches('/')))
        });
        let volume_stopped = self.stopped.contains(&volume_of(Path::new(path)));
        let (live, task) = owner.map_or((false, None), |(_, task)| {
            (
                RUNNING.contains(&task.state) || task.state == TaskState::Spawning,
                Some(task.clone()),
            )
        });
        let wants_end = task.is_some() && (!live || volume_stopped);
        let (status, ended) = match (&task, wants_end, self.enforce) {
            (Some(task), true, true) => match self.store.session_of(task.id)? {
                Some(session) => (Status::Applied, Self::end_processes(&session)?),
                None => (Status::Standing, false),
            },
            (Some(_), true, false) => (Status::Shadow, false),
            _ => (Status::Standing, false),
        };
        let (kind, bytes) = match why {
            Runaway::Growth(bytes) => ("growth", bytes),
            Runaway::Size(bytes) => ("size", bytes),
        };
        let detail = serde_json::json!({
            "path": path, "kind": kind, "bytes": bytes, "ended_processes": ended,
            "task": task.as_ref().map(|t| t.name.to_string()),
        });
        let notice = format!(
            "Runaway file {path}: {kind} of {} MB",
            bytes / (1024 * 1024)
        );
        self.standing(
            &format!("runaway:{path}"),
            FindingId::Runaway,
            task.as_ref().map(|t| t.id),
            status,
            &detail,
            Some(&notice),
        )
        .map(drop)
    }

    fn stuck(&mut self) -> Result<()> {
        if !self.report.host_up {
            return Ok(());
        }
        for id in self.store.tasks_in(TaskState::Working)? {
            let Some(session) = self.store.session_of(id)? else {
                continue;
            };
            if session.state != SessionState::Live {
                continue;
            }
            let clock = self.store.session_clock(&session.id)?;
            let quiet = watch::stuck(clock.idle_age_s, clock.tool_in_flight, None, &self.limits);
            let Some(surface) = session
                .host_surface
                .as_deref()
                .filter(|_| quiet != Stuck::No)
            else {
                self.store.clear_screen_sample(&session.id)?;
                continue;
            };
            let screen = self
                .env()
                .host
                .read_screen(&SurfaceId(surface.to_owned()), 60)?;
            let hash = format!("{:016x}", fnv1a(screen.as_bytes()));
            let unchanged = self
                .store
                .screen_sample(&session.id)?
                .map(|before| before == hash);
            self.store.set_screen_sample(&session.id, &hash)?;
            let verdict = watch::stuck(
                clock.idle_age_s,
                clock.tool_in_flight,
                unchanged,
                &self.limits,
            );
            if verdict == Stuck::Yes {
                let name = self.name(id)?;
                self.standing(
                    &format!("stuck:{}", session.id),
                    FindingId::Stuck,
                    Some(id),
                    Status::Standing,
                    &serde_json::json!({ "idle_s": clock.idle_age_s }),
                    Some(&format!(
                        "{name} looks stuck: no activity and an unchanged screen"
                    )),
                )?;
            }
        }
        self.sweep(FindingId::Stuck)
    }

    fn asks(&mut self) -> Result<()> {
        for ask in self.store.overdue_asks(self.limits.ask_remind)? {
            if !watch::ask_due(ask.age_s, ask.since_notified_s, &self.limits) {
                continue;
            }
            let who = match ask.task_id {
                Some(id) => self.name(id)?,
                None => "orchestrator".to_owned(),
            };
            self.notify(&format!(
                "{who} has waited {} min for your answer: {}",
                ask.age_s / 60,
                ask.question
            ));
            self.store.mark_ask_notified(ask.id)?;
            self.applied(
                FindingId::AskStale,
                ask.task_id,
                serde_json::json!({ "ask": ask.id.0, "age_s": ask.age_s }),
            )?;
        }
        Ok(())
    }

    fn queue(&mut self) -> Result<()> {
        if self.report.queue_paused {
            return Ok(());
        }
        let machine = self.store.machine_mode()?;
        if machine == MachineMode::Normal || machine == MachineMode::Resuming {
            super::prs::refresh_dependencies(self.store, self.env(), self.t.git)?;
            self.report.started = pump(self.store, self.env())?;
        }
        Ok(())
    }
}
