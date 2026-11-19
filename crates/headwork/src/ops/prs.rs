//! A task's pull requests, as close and the tick both look them up: a task may deliver in several, and a worker may
//! rename its branch, so the stored branch alone does not find them.

use anyhow::Result;
use headwork_adapters::git::{GitHost, PrInfo, PrState};
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
    /// Any open one makes the task's end [`PrEnd::Open`], else any merged one [`PrEnd::Merged`], else any closed one
    /// [`PrEnd::Closed`], else [`PrEnd::None`].
    #[must_use]
    pub fn end(&self) -> PrEnd {
        self.pick().map_or(PrEnd::None, |(end, _)| end)
    }

    /// The pull request that decides [`Self::end`].
    #[must_use]
    pub fn deciding(&self) -> Option<&PrInfo> {
        self.pick().map(|(_, pr)| pr)
    }

    fn pick(&self) -> Option<(PrEnd, &PrInfo)> {
        [
            (PrState::Open, PrEnd::Open),
            (PrState::Merged, PrEnd::Merged),
            (PrState::Closed, PrEnd::Closed),
        ]
        .into_iter()
        .find_map(|(state, end)| {
            self.seen
                .iter()
                .find(|pr| pr.state == state)
                .map(|pr| (end, pr))
        })
    }
}

/// Asks the git host now for the task's pull requests and records each.
///
/// It reads those recorded for the task (reported by the worker or seen before), the one of its worktree's stored
/// branch and the one of the branch checked out in its folder. A task in the main checkout has none.
///
/// # Errors
///
/// Fails when the store or the git host fails.
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
    for url in store.pr_urls_of(task.id)? {
        if let Some(pr) = git.pr_by_url(&url)? {
            seen.push(pr);
        }
    }
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
    for branch in &branches {
        if let Some(pr) = git.pr_for_branch(&dir, branch)?
            && !seen.iter().any(|known| known.url == pr.url)
        {
            seen.push(pr);
        }
    }
    for pr in &seen {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(number: i64, state: PrState) -> PrInfo {
        PrInfo {
            repo: "o/r".to_owned(),
            number,
            url: format!("https://github.com/o/r/pull/{number}"),
            state,
        }
    }

    #[test]
    fn the_end_prefers_open_then_merged_then_closed() {
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
                vec![pr(1, PrState::Merged), pr(2, PrState::Merged)],
                PrEnd::Merged,
                Some(1),
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
}
