//! The epic explorer (`docs/backlog.md`, section 12): the explore task a spin-up of an epic spawns, and the `plan_*`
//! tools it plans the epic's children with.
//!
//! An epic explorer is a task of kind `explore`, not a part, whose row records the epic it was spun up for
//! (`tasks.epic_id`, written once at spawn from [`SpawnRequest::epic`]). Its tools act as [`BacklogActor::Explorer`] with the
//! explorer's task and its epic, so the status guard keeps them to that epic's children, and the epic itself only for
//! `plan_finish`; the tools that change anything need the epic in progress.
//!
//! The epic runner ([`run`], `docs/backlog.md`, section 13) starts the children once the plan is written.

use std::fmt::Write as _;

use anyhow::{Context, Result, anyhow, bail};
use headwork_core::backlog::{
    BacklogActor, BacklogItem, BacklogOwner, BacklogStatus, EpicBrief, ItemKind, ItemStatus,
    LinkedTask, Target, epic_brief, epic_slug, plan_file_name,
};
use headwork_core::{
    CloseMode, EXPLORE_KIND, Model, PermissionMode, ProjectName, Slug, TaskRequest, TaskState,
};
use headwork_store::{BacklogEdit, BacklogFilter, BacklogMove, ItemPlan, ItemShape, Store, Task};
use serde::Serialize;

use super::backlog::{AddRequest, Change, add_for, changed, edit_for, spawn_linked};
use super::explore::is_explorer;
use super::spawn::{Pumped, SpawnRequest};
use super::{Env, slashed};

mod runner;

pub use runner::{EVENT_CHILD_SKIPPED, EVENT_HELD, EVENT_RUNNER_ERROR, run};

/// The folder under the first `[hooks] brief_writable` root that holds the epics' plan notes.
pub const PLAN_DIR: &str = "epics";

/// The task kind of a child whose explorer names none.
pub const DEFAULT_CHILD_KIND: &str = "code";

/// The refusal of a spin-up when no `[hooks] brief_writable` root can hold the plan notes.
pub const NO_PLAN_PATH: &str = "epic-no-plan-path";

/// The epic `task` explores: the epic recorded on it at spawn, when it is an explorer (kind `explore`, not a part).
#[must_use]
pub fn epic_of(task: &Task) -> Option<i64> {
    task.epic.filter(|_| is_explorer(task))
}

/// How many explorer slugs a spin-up tries before it gives up: `epic-<id>-<words>`, then `-2` up to this.
const SLUG_TRIES: usize = 100;

/// What the spin-up of an epic started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StartedExplorer {
    /// The epic.
    pub epic: i64,
    /// The explorer task's name.
    pub task: String,
    /// Its state: queued, or already spawning.
    pub state: TaskState,
    /// The plan notes file on its brief's `Writes:` line.
    pub plan: String,
    /// The queued tasks the spawn started.
    pub queue: Vec<Pumped>,
    /// Why the epic was not linked to the task, when the link was refused after the task started.
    pub unlinked: Option<String>,
    /// Whether the epic moved to in progress, linked to the task; false exactly when `unlinked` says why.
    pub epic_in_progress: bool,
}

impl StartedExplorer {
    /// The spin-up as text lines.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = format!(
            "explorer {} {} for epic #{}, plan {}\n",
            self.task, self.state, self.epic, self.plan
        );
        if let Some(why) = &self.unlinked {
            let _infallible = writeln!(
                out,
                "epic #{} was NOT moved to in progress and is not linked to {}; the explorer runs anyway: {why}",
                self.epic, self.task
            );
        }
        out
    }
}

/// The plan notes file of an epic: `<first [hooks] brief_writable root>/epics/<id>-<slug>.md`.
///
/// # Errors
///
/// Refuses with [`NO_PLAN_PATH`] when `[hooks] brief_writable` is empty.
pub fn plan_path(env: &Env<'_>, epic: &BacklogItem) -> Result<String> {
    let root = env
        .config
        .hooks
        .brief_writable
        .first()
        .map(|dir| slashed(&env.home.expand(dir)))
        .ok_or_else(|| {
            anyhow!(
                "{NO_PLAN_PATH}: an epic's explorer writes its plan under the first `[hooks] brief_writable` folder, and none \
                 is set; set it, then spin the epic up again"
            )
        })?;
    Ok(format!(
        "{}/{PLAN_DIR}/{}",
        root.trim_end_matches('/'),
        plan_file_name(epic.id, &epic.draft.text)
    ))
}

/// Spins an epic up: spawns its explorer, linked to the epic.
///
/// The explorer is a task of kind `explore` with the kind's model and close mode and a brief made from the epic; the epic
/// moves to in progress through the same guard as any spawn for an item.
///
/// # Errors
///
/// Refuses before anything moves when no `[hooks] brief_writable` folder is set ([`NO_PLAN_PATH`]), the epic's project is
/// not one headwork runs, or the guard refuses the start; fails as [`spawn_linked`] does.
pub fn spin_up(store: &mut Store, env: &Env<'_>, epic: &BacklogItem) -> Result<StartedExplorer> {
    if epic.kind != ItemKind::Epic {
        bail!("#{} is not an epic", epic.id);
    }
    let plan = plan_path(env, epic)?;
    let project = ProjectName::new(epic.draft.project.clone())
        .ok()
        .filter(|project| env.config.project(project).is_some())
        .ok_or_else(|| {
            anyhow!(
                "epic #{} is in `{}`, which is not a project headwork runs: no explorer can start",
                epic.id,
                epic.draft.project
            )
        })?;
    let in_main = env
        .config
        .project(&project)
        .is_some_and(|config| config.is_main_only(EXPLORE_KIND));
    let brief = epic_brief(&EpicBrief {
        id: epic.id,
        text: &epic.draft.text,
        notes: &epic.draft.notes,
        project: project.as_str(),
        plan_path: &plan,
    });
    let slug = free_slug(store, &project, &epic_slug(epic.id, &epic.draft.text))?;
    let incoming = env.home.root().join("incoming");
    std::fs::create_dir_all(&incoming).context("creating the incoming briefs folder")?;
    let file = write_new(&incoming, &format!("{project}-{slug}"), &brief)?;
    let request = SpawnRequest {
        request: TaskRequest {
            project,
            slug,
            kind: EXPLORE_KIND.to_owned(),
            model: None,
            effort: None,
            in_main,
            keep: false,
            close: CloseMode::default_for(EXPLORE_KIND),
            review: None,
            decision: None,
        },
        brief: &file,
        depends_on: None,
        permission_mode: PermissionMode::Auto,
        allow_ask: &[],
        epic: Some(epic.id),
    };
    let spawned = spawn_linked(store, env, &request, Some(epic.id));
    std::fs::remove_file(&file).ok();
    let spawned = spawned?;
    Ok(StartedExplorer {
        epic: epic.id,
        task: spawned.task.name.to_string(),
        state: spawned.task.state,
        plan,
        queue: spawned.pumped,
        epic_in_progress: spawned.unlinked.is_none(),
        unlinked: spawned.unlinked,
    })
}

/// The first of `base`, `base-2`, `base-3`, ... that no task of `project` has ever used, so each spin-up of an epic gets a
/// task name (and brief file) of its own.
fn free_slug(store: &Store, project: &ProjectName, base: &str) -> Result<Slug> {
    for n in 1..=SLUG_TRIES {
        let text = if n == 1 {
            base.to_owned()
        } else {
            format!("{base}-{n}")
        };
        if store.find_task(&format!("{project}/{text}"))?.is_none() {
            return Ok(Slug::new(text)?);
        }
    }
    bail!("every explorer slug from `{base}` to `{base}-{SLUG_TRIES}` is taken in `{project}`")
}

/// Writes `text` to a file in `dir` named after `stem` that did not exist before (`<stem>.md`, else `<stem>-2.md`, ...),
/// so the removal after the spawn only ever removes a file this spin-up wrote.
fn write_new(dir: &std::path::Path, stem: &str, text: &str) -> Result<std::path::PathBuf> {
    use std::io::Write as _;
    for n in 1..=SLUG_TRIES {
        let name = if n == 1 {
            format!("{stem}.md")
        } else {
            format!("{stem}-{n}.md")
        };
        let path = dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(text.as_bytes())
                    .with_context(|| format!("writing {}", path.display()))?;
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| format!("creating {}", path.display()));
            }
        }
    }
    bail!("no free brief file name for `{stem}` in {}", dir.display())
}

/// The epic an explorer plans, with the explorer's task.
#[derive(Debug, Clone, Copy)]
pub struct Explorer<'a> {
    /// The explorer's own task.
    pub task: &'a Task,
    /// The epic it explores.
    pub epic: i64,
}

impl Explorer<'_> {
    fn moving<'r>(
        &self,
        env: &Env<'_>,
        target: Target,
        reason: Option<&'r str>,
    ) -> BacklogMove<'r> {
        BacklogMove {
            actor_task: Some(self.task.id),
            actor_epic: Some(self.epic),
            ..BacklogMove::new(target, BacklogActor::Explorer, env.actor, true).with_reason(reason)
        }
    }

    /// Refuses a change while the epic is not in progress: finished, cancelled, blocked or back in the backlog.
    fn in_progress(&self, store: &Store) -> Result<()> {
        let status = store.backlog_item(self.epic)?.draft.status;
        if status != BacklogStatus::InProgress {
            bail!(
                "epic #{} is {status}, not in progress: its plan changes only while it is in progress (`plan_list` still \
                 shows it)",
                self.epic
            );
        }
        Ok(())
    }

    /// Refuses the epic itself as a dependency of its child: the epic is done only after its children.
    fn not_on_itself(&self, on: &[i64]) -> Result<()> {
        if on.contains(&self.epic) {
            bail!(
                "a child cannot wait for its own epic #{}: the epic is done only after its children",
                self.epic
            );
        }
        Ok(())
    }

    fn child(&self, store: &Store, id: i64) -> Result<BacklogItem> {
        let item = store.backlog_item(id)?;
        if item.parent != Some(self.epic) {
            bail!(
                "#{id} is not a child of epic #{}: an explorer changes only its own epic's children",
                self.epic
            );
        }
        Ok(item)
    }
}

/// The epic in [`PlanView`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanEpic {
    /// Its id.
    pub id: i64,
    /// Its text.
    pub text: String,
    /// Its project.
    pub project: String,
    /// Where it stands.
    #[serde(flatten)]
    pub status: BacklogStatus,
}

/// One child in [`PlanView`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanChild {
    /// Its id.
    pub id: i64,
    /// Its text.
    pub text: String,
    /// Its project.
    pub project: String,
    /// Where it stands, with its block's reason and note.
    #[serde(flatten)]
    pub status: BacklogStatus,
    /// The items it depends on.
    pub deps: Vec<i64>,
    /// The task kind its spawn takes.
    pub task_kind: Option<String>,
    /// The model its spawn takes.
    pub model: Option<String>,
    /// Its planned review level.
    pub review: Option<u8>,
    /// Whether a brief is stored for it.
    pub has_brief: bool,
    /// The task linked to it, with its state.
    pub task: Option<LinkedTask>,
}

/// What `plan_list` shows: the epic and every child, in export order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanView {
    /// The epic.
    pub epic: PlanEpic,
    /// Its children.
    pub children: Vec<PlanChild>,
}

/// The epic and its children with their statuses, dependencies, plans and tasks.
///
/// # Errors
///
/// Fails when the store does.
pub fn list(store: &Store, explorer: &Explorer<'_>) -> Result<PlanView> {
    let epic = store.backlog_item(explorer.epic)?;
    let items = store.backlog_items(&BacklogFilter {
        epic: Some(explorer.epic),
        ..BacklogFilter::default()
    })?;
    let mut children = Vec::with_capacity(items.len());
    for item in items {
        let plan = store.backlog_plan(item.id)?;
        children.push(PlanChild {
            id: item.id,
            text: item.draft.text,
            project: item.draft.project,
            status: item.draft.status,
            deps: item.deps,
            task_kind: plan.task_kind,
            model: plan.model,
            review: plan.review,
            has_brief: plan.brief.is_some_and(|brief| !brief.trim().is_empty()),
            task: item.task,
        });
    }
    Ok(PlanView {
        epic: PlanEpic {
            id: epic.id,
            text: epic.draft.text,
            project: epic.draft.project,
            status: epic.draft.status,
        },
        children,
    })
}

/// What an explorer plans for a child's spawn, as the tools take it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanFields {
    /// The brief.
    pub brief: Option<String>,
    /// The task kind.
    pub kind: Option<String>,
    /// The model.
    pub model: Option<String>,
    /// The review level.
    pub review: Option<u64>,
}

impl PlanFields {
    fn checked(&self) -> Result<ItemPlan> {
        let brief = self
            .brief
            .as_deref()
            .map(|brief| {
                if brief.trim().is_empty() {
                    bail!("`brief` is empty: a child's brief stands alone as a worker brief");
                }
                Ok(brief.trim_end().to_owned())
            })
            .transpose()?;
        let task_kind = self
            .kind
            .as_deref()
            .map(|kind| {
                Slug::new(kind.trim())
                    .map(|kind| kind.as_str().to_owned())
                    .map_err(|error| anyhow!("`kind` `{kind}` is not a task kind: {error}"))
            })
            .transpose()?;
        let model = self
            .model
            .as_deref()
            .map(|model| {
                model
                    .trim()
                    .parse::<Model>()
                    .map(|model| model.as_str().to_owned())
                    .map_err(|error| anyhow!("`model`: {error}"))
            })
            .transpose()?;
        let review = self
            .review
            .map(|level| {
                u8::try_from(level)
                    .ok()
                    .filter(|level| *level <= 3)
                    .ok_or_else(|| anyhow!("`review` must be 0, 1, 2 or 3, not {level}"))
            })
            .transpose()?;
        Ok(ItemPlan {
            brief,
            task_kind,
            model,
            review,
        })
    }
}

/// A child to add.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanAdd {
    /// The child's text.
    pub text: String,
    /// Its brief, kind, model and review level; the brief is required, the kind defaults to `code`.
    pub plan: PlanFields,
    /// The items it depends on, inside the epic or not.
    pub after: Vec<i64>,
    /// Its project; the epic's when absent.
    pub project: Option<String>,
    /// Its section; the epic's when absent.
    pub section: Option<String>,
    /// Who acts on it; an agent when absent.
    pub owner: Option<BacklogOwner>,
}

/// Adds a child to the explorer's epic with its stored plan, and exports.
///
/// With a dependency not done it starts blocked for `depends`. The `Backlog` pick cap does not apply: the spin-up was the
/// user's go for the whole epic.
///
/// # Errors
///
/// Fails when the brief is missing or empty, a plan field is invalid, the dependency guard refuses (a cancelled
/// dependency, a cycle), or the store fails; nothing is added then.
pub fn add(
    store: &mut Store,
    env: &Env<'_>,
    explorer: &Explorer<'_>,
    request: PlanAdd,
) -> Result<Change> {
    explorer.in_progress(store)?;
    if request.plan.brief.is_none() {
        bail!("`brief` is required: a child carries everything its spawn needs");
    }
    explorer.not_on_itself(&request.after)?;
    let mut plan = request.plan.checked()?;
    plan.task_kind = plan
        .task_kind
        .or_else(|| Some(DEFAULT_CHILD_KIND.to_owned()));
    let epic = store.backlog_item(explorer.epic)?;
    let project = request.project.unwrap_or(epic.draft.project);
    let runs =
        ProjectName::new(project.trim()).is_ok_and(|name| env.config.project(&name).is_some());
    if !runs {
        bail!("`{project}` is not a project headwork runs: a child is work a task can do");
    }
    let add = AddRequest {
        project,
        text: request.text,
        section: request.section.or(epic.draft.section),
        owner: request.owner,
        not_before: None,
        due: None,
        notes: Vec::new(),
        shape: ItemShape {
            kind: None,
            parent: Some(explorer.epic),
            after: request.after,
            plan,
        },
    };
    add_for(
        store,
        env,
        &add,
        BacklogActor::Explorer,
        Some(explorer.task.id),
    )
}

/// Changes a child of the explorer's epic that is in the backlog or blocked with no live task: its text and its plan.
///
/// # Errors
///
/// Fails when the item is not a child of the epic, has started (in progress, done, cancelled, or a live task), a field is
/// invalid, or the store fails.
pub fn edit(
    store: &mut Store,
    env: &Env<'_>,
    explorer: &Explorer<'_>,
    id: i64,
    text: Option<String>,
    fields: &PlanFields,
) -> Result<Change> {
    explorer.in_progress(store)?;
    let item = explorer.child(store, id)?;
    if !matches!(
        item.draft.status.kind(),
        ItemStatus::Backlog | ItemStatus::Blocked
    ) {
        bail!(
            "#{id} is {}: an explorer edits only a child in the backlog or blocked",
            item.draft.status.kind()
        );
    }
    if let Some(task) = item.task.as_ref().filter(|task| task.is_live()) {
        bail!(
            "#{id} has a live task, {}: an explorer edits only a child no task works on",
            task.name
        );
    }
    let edit = BacklogEdit {
        text,
        plan: fields.checked()?,
        ..BacklogEdit::default()
    };
    edit_for(store, env, id, &edit, Some(explorer.task.id))
}

/// Makes a child of the explorer's epic depend on each of `on` (any item; all or nothing), and exports.
///
/// # Errors
///
/// Fails when `id` is not a child of the epic, the dependency guard refuses, or the store fails.
pub fn depend(
    store: &mut Store,
    env: &Env<'_>,
    explorer: &Explorer<'_>,
    id: i64,
    on: &[i64],
) -> Result<Change> {
    explorer.in_progress(store)?;
    explorer.child(store, id)?;
    explorer.not_on_itself(on)?;
    let item = store.add_backlog_dependencies(id, on, env.actor, Some(explorer.task.id))?;
    Ok(changed(store, env, &[item]))
}

/// Removes a child's dependency on `on`, and exports.
///
/// # Errors
///
/// Fails when `id` is not a child of the epic or does not depend on `on`, or the store fails.
pub fn undepend(
    store: &mut Store,
    env: &Env<'_>,
    explorer: &Explorer<'_>,
    id: i64,
    on: i64,
) -> Result<Change> {
    explorer.in_progress(store)?;
    explorer.child(store, id)?;
    let item = store.remove_backlog_dependency(id, on, env.actor, Some(explorer.task.id))?;
    Ok(changed(store, env, &[item]))
}

/// Cancels a child of the explorer's epic in the backlog or blocked, through the status guard; it never drops a task.
///
/// # Errors
///
/// Fails when `id` is not a child of the epic, the guard refuses (in progress, done, a live task), or the store fails.
pub fn cancel(
    store: &mut Store,
    env: &Env<'_>,
    explorer: &Explorer<'_>,
    id: i64,
    reason: Option<&str>,
) -> Result<Change> {
    explorer.in_progress(store)?;
    explorer.child(store, id)?;
    let reason = reason.map(str::trim).filter(|reason| !reason.is_empty());
    let item = store.move_backlog_item(id, &explorer.moving(env, Target::Cancelled, reason))?;
    Ok(changed(store, env, &[item]))
}

/// Finishes the explorer's epic: in progress to done, its link ended; refused while a child is open, naming them.
///
/// # Errors
///
/// Fails when a child is open, the guard refuses, or the store fails.
pub fn finish(
    store: &mut Store,
    env: &Env<'_>,
    explorer: &Explorer<'_>,
    note: Option<&str>,
) -> Result<Change> {
    explorer.in_progress(store)?;
    let open = store.backlog_open_children(explorer.epic)?;
    if !open.is_empty() {
        let mut named = Vec::with_capacity(open.len());
        for id in &open {
            let item = store.backlog_item(*id)?;
            named.push(format!("#{id} ({})", item.draft.status.kind()));
        }
        bail!(
            "epic #{} has {} open children: {}; finish or cancel them first, or leave the epic in progress",
            explorer.epic,
            open.len(),
            named.join(", ")
        );
    }
    let note = note.map(str::trim).filter(|note| !note.is_empty());
    let item = store.move_backlog_item(explorer.epic, &explorer.moving(env, Target::Done, note))?;
    Ok(changed(store, env, &[item]))
}
