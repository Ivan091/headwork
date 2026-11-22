//! Starting a review the user asked for (`docs/review-flows.md`, sections 8 and 9).
//!
//! `review_start` pins the commit, makes a read-only detached worktree for it, and starts the reviewer lead as a part
//! of kind `review`.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};
use headwork_core::config::REVIEW_KIND;
use headwork_core::review::{
    DisputeRule, Each, Flow, Level, ReviewTarget, ReviewTrigger, Unsettled,
};
use headwork_core::{PermissionMode, ReviewState, Slug, TaskRequest, TaskShape};
use headwork_store::{ReviewRow, Store, Task};
use serde::Serialize;

use super::flows::{LoadedFlow, load_flow};
use super::spawn::{Pumped, SpawnRequest, spawn_shaped};
use super::{Env, slashed};

/// Branches tried, in order, for the base of a review's diff when the remote names no default.
const BASE_CANDIDATES: &[&str] = &[
    "origin/HEAD",
    "origin/master",
    "origin/main",
    "master",
    "main",
];

/// What the implementing lead does once its review runs; `review_start` returns it, since the lead's rules have no room
/// left under the hook's context limit.
pub const LEAD_NEXT: &str = "The reviewer lead runs now as your part `review`; you may stop and wait for its findings. \
     Fix or dispute every finding it sends: a dispute gives its reason (at level 2 and 3, evidence: a test, a code \
     reference, a doc line). Commit the fixes, then record every answer with `review_resolve` (each finding by its \
     number, `fixed` with the commit or `disputed` with the reason) and tell the reviewer lead with `SendMessage`; never \
     argue outside that answer. Your `done` waits until the review finishes; then tell the user its outcome in one \
     line.";

/// A review that started.
#[derive(Debug, Clone, Serialize)]
pub struct Started {
    /// The review, now `running`.
    pub review: ReviewRow,
    /// The reviewer lead's part.
    pub part: String,
    /// The read-only worktree at the commit under review.
    pub worktree: String,
    /// What the queue did; the part may wait there for a slot.
    pub queue: Vec<Pumped>,
    /// What the implementing lead does next.
    pub next: &'static str,
}

/// Starts the review the user asked for in `lead`'s tab.
///
/// # Errors
///
/// Refuses when reviews are off, when no review was requested (`review-unrequested`), when the lead's folder holds
/// uncommitted changes (`review-dirty`), when the flow does not load, or when git, the worktree or the spawn fail; a
/// worktree made before a failed spawn is removed again.
pub fn start(store: &mut Store, env: &Env<'_>, lead: &Task) -> Result<Started> {
    if !env.config.reviews.enabled {
        bail!("reviews are off (`[reviews] enabled = false`)");
    }
    let review = match store.open_review(lead.id)? {
        Some(review) if review.task_id == lead.id && review.state == ReviewState::Requested => {
            review
        }
        Some(review) => bail!(
            "review {} of this task is {}; it started already",
            review.id,
            review.state
        ),
        None => bail!(
            "review-unrequested: reviews start only on the user's word (`review <level>` typed in your tab), and none \
             waits"
        ),
    };
    let project = env.project(lead)?;
    let allowed = env.config.allowed_models(project);
    let loaded = load_flow(env.home.root(), &review.flow, &allowed).map_err(anyhow::Error::msg)?;
    let dir = env.task_dir(lead)?;
    let worktrees = env.worktrees(lead)?;
    if worktrees.is_dirty(&dir)? {
        bail!(
            "review-dirty: {} holds uncommitted changes; commit them first",
            dir.display()
        );
    }
    let head = worktrees.resolve(&dir, "HEAD")?;
    let base = base_of(env, lead, &dir, &head)?;
    let slug = part_slug(store, lead)?;
    let path = env.worktree_dir(lead, slug.as_str())?;
    let path_text = slashed(&path);
    if path.exists() {
        bail!("{path_text} exists already; remove it before the review can use it");
    }
    worktrees.add_detached(&path_text, &head)?;
    let changed = worktrees
        .changed_files(&dir, &base, &head)
        .unwrap_or_default();
    let target = ReviewTarget {
        kind: loaded.flow.target(),
        base: base.clone(),
        head: head.clone(),
        worktree: path_text.clone(),
        changed,
    };
    let result = spawn_reviewer(store, env, lead, &review, &loaded, &target, slug);
    let (part, queue) = match result {
        Ok(spawned) => spawned,
        Err(error) => {
            worktrees.remove_detached(&path_text).ok();
            return Err(error);
        }
    };
    store.set_review_target(review.id, Some(part.id), &base, &head)?;
    let review = store.move_review(review.id, ReviewTrigger::Start, false, false, env.actor)?;
    Ok(Started {
        review,
        part: part.name.to_string(),
        worktree: path_text,
        queue,
        next: LEAD_NEXT,
    })
}

fn base_of(env: &Env<'_>, lead: &Task, dir: &Path, head: &str) -> Result<String> {
    let worktrees = env.worktrees(lead)?;
    for candidate in BASE_CANDIDATES {
        if worktrees.resolve(dir, candidate).is_ok()
            && let Ok(base) = worktrees.merge_base(dir, candidate, head)
        {
            return Ok(base);
        }
    }
    bail!(
        "no default branch to diff against: tried {}",
        BASE_CANDIDATES.join(", ")
    )
}

/// `<lead slug>-review` for the first review of a lead, `-review-2` and on for later ones whose name is free.
fn part_slug(store: &Store, lead: &Task) -> Result<Slug> {
    let count = store.reviews_of(lead.id)?.len();
    let mut n = count.max(1);
    loop {
        let word = if n == 1 {
            "review".to_owned()
        } else {
            format!("review-{n}")
        };
        let slug = Slug::new(format!("{}-{word}", lead.slug()))?;
        let name = format!("{}-{slug}", lead.project());
        if store.find_task(&name)?.is_none() {
            return Ok(slug);
        }
        n += 1;
    }
}

fn spawn_reviewer(
    store: &mut Store,
    env: &Env<'_>,
    lead: &Task,
    review: &ReviewRow,
    loaded: &LoadedFlow,
    target: &ReviewTarget,
    slug: Slug,
) -> Result<(Task, Vec<Pumped>)> {
    let text = brief(loaded, review, target, &lead.name.to_string())?;
    let incoming = env.home.root().join("incoming");
    std::fs::create_dir_all(&incoming).context("creating the incoming briefs folder")?;
    let file = incoming.join(format!("{}-{slug}.md", lead.project()));
    std::fs::write(&file, text).with_context(|| format!("writing {}", file.display()))?;
    let request = SpawnRequest {
        request: TaskRequest {
            project: lead.project().clone(),
            slug,
            kind: REVIEW_KIND.to_owned(),
            model: Some(env.config.reviews.lead_model),
            effort: None,
            in_main: false,
            keep: false,
        },
        brief: &file,
        depends_on: None,
        permission_mode: PermissionMode::Auto,
        allow_ask: &[],
    };
    let result = spawn_shaped(store, env, &request, TaskShape::Part { parent: lead.id });
    std::fs::remove_file(&file).ok();
    result
}

/// The reviewer lead's brief: the review, the level's rules, the target, and the nodes at the level; `review_next`
/// hands out their prompts.
///
/// # Errors
///
/// Never fails today; a flow whose overview cannot be written would.
pub fn brief(
    loaded: &LoadedFlow,
    review: &ReviewRow,
    target: &ReviewTarget,
    lead: &str,
) -> Result<String> {
    let flow = &loaded.flow;
    let level = review.level;
    let mut text = String::new();
    let _ = writeln!(
        text,
        "# Review {} of {lead}: level {level}, flow `{}`\n",
        review.id,
        flow.name()
    );
    let _ = writeln!(
        text,
        "The user asked `{lead}` for this review. You are its reviewer lead: your rules say how you work, and this \
         brief is everything you know about the change.\n"
    );
    let _ = writeln!(text, "- Flow: `{}`, {}.", flow.name(), flow.description());
    let _ = writeln!(text, "- Level {level}: {}", level_rules(flow, level));
    let _ = writeln!(
        text,
        "- Commits: base `{}`, head `{}`. Your folder is a read-only detached worktree at the head: `{}`.",
        target.base, target.head, target.worktree
    );
    if !review.focus.is_empty() {
        let _ = writeln!(text, "- The user's focus: {}", review.focus);
    }
    let _ = writeln!(
        text,
        "- The implementing lead: `{lead}` (your `SendMessage` address for it)."
    );
    let _ = writeln!(
        text,
        "\n## The nodes\n\nheadwork hands them out with `review_next`, each with its model and exact prompt, as their \
         inputs finish:"
    );
    for node in flow.nodes_at(level) {
        let inputs: Vec<String> = flow
            .inputs(node, level)
            .iter()
            .map(|input| input.id.to_string())
            .collect();
        let model = node
            .model_at(level)
            .map_or_else(|| "?".to_owned(), |model| model.to_string());
        let _ = write!(text, "\n- `{}`, model {model}", node.id);
        if !inputs.is_empty() {
            let _ = write!(text, ", after {}", inputs.join(", "));
        }
        if node.each == Some(Each::Finding) {
            let _ = write!(text, ", once per finding of its inputs");
        }
    }
    let _ = writeln!(text, "\n\nStart with `review_next`.");
    Ok(text)
}

fn level_rules(flow: &Flow, level: Level) -> String {
    let spec = flow.level(level);
    let dispute = match spec.dispute {
        DisputeRule::Reason => "a dispute is accepted when it gives a reason",
        DisputeRule::Evidence => {
            "a dispute is accepted only with evidence (a test, a code reference, a doc line)"
        }
    };
    let argue = if spec.argue_back {
        "; argue back once before you let a dispute go further"
    } else {
        ""
    };
    let unsettled = match spec.unsettled {
        Unsettled::Accept => "a dispute you do not accept is accepted anyway",
        Unsettled::Ask => "a dispute you do not accept goes to the user",
    };
    let rounds = if spec.rounds > 1 {
        format!(
            "{} full rounds: after the fixes, run every node again on the new commit",
            spec.rounds
        )
    } else {
        "one round; the fixes are re-checked".to_owned()
    };
    format!("{dispute}{argue}; {unsettled}; {rounds}.")
}
