//! A task's pull requests, as close and the tick both look them up: a task may deliver in several, and a worker may
//! rename its branch, so the stored branch alone does not find them.

use anyhow::Result;
use headwork_adapters::git::{GitHost, PrInfo, PrState};
use headwork_core::TaskState;
use headwork_core::guard::DEFAULT_BRANCHES;
use headwork_core::lifecycle::PrEnd;
use headwork_store::{PrSeen, Store, Task};

use super::hook::{Git, GitProbe};
use super::{Env, slashed};

/// Where a task's pull requests stand together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrLookup {
    /// Every pull request found, oldest recorded first.
    pub seen: Vec<PrInfo>,
}

impl PrLookup {
    /// Any open one makes the task's end [`PrEnd::Open`]; else the newest one decides, [`PrEnd::Merged`] or
    /// [`PrEnd::Closed`], so a later pull request closed without a merge does not read as merged; else [`PrEnd::None`].
    #[must_use]
    pub fn end(&self) -> PrEnd {
        self.pick().map_or(PrEnd::None, |(end, _)| end)
    }

    /// The pull request that decides [`Self::end`]: the newest open one, else the newest.
    #[must_use]
    pub fn deciding(&self) -> Option<&PrInfo> {
        self.pick().map(|(_, pr)| pr)
    }

    fn pick(&self) -> Option<(PrEnd, &PrInfo)> {
        let open = self.seen.iter().rev().find(|pr| pr.state == PrState::Open);
        open.or_else(|| self.seen.last()).map(|pr| {
            let end = match pr.state {
                PrState::Open => PrEnd::Open,
                PrState::Merged => PrEnd::Merged,
                PrState::Closed => PrEnd::Closed,
            };
            (end, pr)
        })
    }
}

/// Whether the host says the pull request was opened before the task was queued at `queued_at`.
///
/// Such a pull request belongs to an earlier task whose branch had the same name. Compared to the second; a time
/// missing on either side is never before.
#[must_use]
pub fn opened_before(pr: &PrInfo, queued_at: &str) -> bool {
    let second = |time: &str| time.get(..19).map(str::to_owned);
    match (pr.created_at.as_deref().and_then(second), second(queued_at)) {
        (Some(opened), Some(queued)) => opened < queued,
        _ => false,
    }
}

/// Asks the git host now for the task's pull requests and records each.
///
/// It reads those recorded for the task (reported by the worker or seen before), the one of its worktree's stored
/// branch and the one of the branch checked out in its folder. A recorded pull request already merged is not asked
/// again: a merge is final. A branch's pull request opened before the task was queued is an earlier task's and is left
/// out. A closed task is asked only about its recorded pull requests not merged yet (a queued task may wait on them),
/// never about its branches, whose names a later task may reuse. A task in the main checkout has none.
///
/// # Errors
///
/// Fails when the store or the git host fails, including a recorded URL the host cannot resolve.
pub fn lookup(
    store: &mut Store,
    env: &Env<'_>,
    git: &dyn GitHost,
    task: &Task,
) -> Result<PrLookup> {
    if task.in_main {
        return Ok(PrLookup { seen: Vec::new() });
    }
    let mut seen: Vec<PrInfo> = Vec::new();
    let mut fresh: Vec<PrInfo> = Vec::new();
    for recorded in store.recorded_prs(task.id)? {
        if recorded.state == PrState::Merged.as_str() {
            seen.push(PrInfo {
                repo: recorded.repo,
                number: recorded.number,
                url: recorded.url,
                state: PrState::Merged,
                created_at: None,
            });
        } else if let Some(pr) = git.pr_by_url(&recorded.url)? {
            seen.push(pr.clone());
            fresh.push(pr);
        }
    }
    if task.state != TaskState::Closed {
        for pr in branch_prs(store, env, git, task)? {
            if !seen.iter().any(|known| known.url == pr.url) {
                seen.push(pr.clone());
                fresh.push(pr);
            }
        }
    }
    for pr in &fresh {
        store.record_pr(
            task.id,
            &PrSeen {
                repo: &pr.repo,
                number: pr.number,
                url: &pr.url,
                state: pr.state.as_str(),
            },
            env.actor,
        )?;
    }
    Ok(PrLookup { seen })
}

/// The pull requests of the task's stored branch and of the branch checked out in its folder, those opened before the
/// task left out.
fn branch_prs(store: &Store, env: &Env<'_>, git: &dyn GitHost, task: &Task) -> Result<Vec<PrInfo>> {
    let dir = env.task_dir(task)?;
    let dir = if dir.is_dir() {
        dir
    } else {
        env.main_path(task)?
    };
    let mut branches = vec![
        store
            .worktree_of(task.id)?
            .map_or_else(|| task.slug().to_string(), |w| w.branch),
    ];
    if let Some(current) = Git.branch(&slashed(&dir))
        && !branches.contains(&current)
        && !DEFAULT_BRANCHES.contains(&current.as_str())
    {
        branches.push(current);
    }
    let mut found: Vec<PrInfo> = Vec::new();
    for branch in &branches {
        if let Some(pr) = git.pr_for_branch(&dir, branch)?
            && !opened_before(&pr, &task.queued_at)
            && !found.iter().any(|known| known.url == pr.url)
        {
            found.push(pr);
        }
    }
    Ok(found)
}

/// Asks the git host again for the pull requests of queued tasks' ending dependencies not counted merged yet.
///
/// A dependency closed straight from `in_review` then counts once its pull requests merged. A lookup that fails is
/// logged and the dependency keeps waiting.
///
/// # Errors
///
/// Fails when the store fails.
pub fn refresh_dependencies(store: &mut Store, env: &Env<'_>, git: &dyn GitHost) -> Result<()> {
    for id in store.unsettled_dependencies()? {
        let task = store.task(id)?;
        if let Err(error) = lookup(store, env, git, &task) {
            store.append_event(
                env.actor,
                Some(id),
                None,
                "pr-lookup-error",
                &serde_json::json!({ "error": format!("{error:#}") }),
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(number: i64, state: PrState) -> PrInfo {
        PrInfo {
            repo: "o/r".to_owned(),
            number,
            url: format!("https://github.com/o/r/pull/{number}"),
            state,
            created_at: None,
        }
    }

    #[test]
    fn the_end_prefers_an_open_one_then_the_newest() {
        let cases = [
            (vec![], PrEnd::None, None),
            (vec![pr(1, PrState::Closed)], PrEnd::Closed, Some(1)),
            (
                vec![pr(1, PrState::Closed), pr(2, PrState::Merged)],
                PrEnd::Merged,
                Some(2),
            ),
            (
                vec![pr(1, PrState::Merged), pr(2, PrState::Open)],
                PrEnd::Open,
                Some(2),
            ),
            (
                vec![pr(1, PrState::Open), pr(2, PrState::Merged)],
                PrEnd::Open,
                Some(1),
            ),
            (
                vec![pr(1, PrState::Merged), pr(2, PrState::Merged)],
                PrEnd::Merged,
                Some(2),
            ),
            (
                vec![pr(1, PrState::Merged), pr(2, PrState::Closed)],
                PrEnd::Closed,
                Some(2),
            ),
        ];
        for (seen, end, number) in cases {
            let lookup = PrLookup { seen };
            assert_eq!(lookup.end(), end, "{:?}", lookup.seen);
            assert_eq!(
                lookup.deciding().map(|pr| pr.number),
                number,
                "{:?}",
                lookup.seen
            );
        }
    }

    #[test]
    fn a_pull_request_opened_before_the_task_was_queued_is_an_earlier_tasks() {
        let queued = "2026-11-22T08:11:05.502Z";
        let opened = |at: Option<&str>| PrInfo {
            created_at: at.map(str::to_owned),
            ..pr(1, PrState::Merged)
        };
        let cases = [
            (Some("2026-11-21T10:00:00Z"), true),
            (Some("2026-11-22T08:11:04Z"), true),
            (Some("2026-11-22T08:11:05Z"), false),
            (Some("2026-11-22T09:00:00Z"), false),
            (None, false),
            (Some("bad"), false),
        ];
        for (at, before) in cases {
            assert_eq!(opened_before(&opened(at), queued), before, "{at:?}");
        }
        assert!(
            !opened_before(&opened(Some("2026-11-21T10:00:00Z")), "t"),
            "a queue time that is not one"
        );
    }
}
