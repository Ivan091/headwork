//! Dependencies between backlog items and the shape of epics (`docs/backlog.md`, section 11).
//!
//! Pure: the store gathers the facts (the edges, the statuses, the kinds), asks these guards and writes the rows.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::UnknownWord;
use crate::backlog::{BacklogStatus, BlockReason, ItemStatus, ReturnTo, Target};
use crate::vocabulary::text_enum;

text_enum!(
    /// What an item is: work to do, or an epic that groups items.
    ItemKind, "item kind" {
        /// Work a task can do.
        Item => "item",
        /// A group of items, one level deep.
        Epic => "epic",
    }
);

/// What the dependency guards know about one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DepNode {
    /// The item id.
    pub id: i64,
    /// Where it stands.
    pub status: ItemStatus,
    /// Where it returns to when unblocked, for a blocked item.
    pub returns_to: Option<ReturnTo>,
}

/// What the epic guard knows about the parent a new item names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParentFacts {
    /// The parent id.
    pub id: i64,
    /// Whether it is an item or an epic.
    pub kind: ItemKind,
    /// The parent's own parent, if it has one.
    pub parent: Option<i64>,
}

/// Why a dependency or an epic's shape was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DepRefusal {
    /// An item that depends on itself.
    #[error("an item cannot depend on itself")]
    SelfDependency,
    /// A dependency that would close a loop; the path runs from the item back to itself.
    #[error("the dependency would close a cycle: {}", join(.0))]
    Cycle(Vec<i64>),
    /// The item is not in the backlog or blocked, so a dependency cannot gate it.
    #[error("#{id} is {status}: a dependency is added only to an item in the backlog or blocked")]
    ItemStarted {
        /// The item.
        id: i64,
        /// Its status.
        status: ItemStatus,
    },
    /// An item blocked out of progress, which takes no dependency.
    #[error("#{0} is blocked from in progress: an item in progress takes no dependency")]
    BlockedInProgress(i64),
    /// A dependency that was cancelled and will never be done.
    #[error("#{0} is cancelled and will never be done")]
    DependencyCancelled(i64),
    /// A parent that is not an epic.
    #[error("#{0} is not an epic: only an epic has children")]
    ParentNotEpic(i64),
    /// A parent that has a parent of its own.
    #[error("#{0} has a parent itself: epics are one level deep")]
    ParentHasParent(i64),
    /// An epic given a parent.
    #[error("an epic has no parent: epics are one level deep")]
    EpicWithParent,
}

fn join(path: &[i64]) -> String {
    path.iter()
        .map(|id| format!("#{id}"))
        .collect::<Vec<_>>()
        .join(" -> ")
}

/// The cycle that adding "`item` depends on `dep`" would close, or `None`.
///
/// `edges` are `(item, depends_on)` pairs. The path starts at `item`, runs through `dep` along existing dependencies and
/// ends at `item` again, so its first and last ids are the same.
#[must_use]
pub fn would_cycle(edges: &[(i64, i64)], item: i64, dep: i64) -> Option<Vec<i64>> {
    if item == dep {
        return Some(vec![item, item]);
    }
    let mut next: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for &(from, to) in edges {
        next.entry(from).or_default().push(to);
    }
    let mut seen = BTreeSet::from([dep]);
    let mut stack = vec![vec![dep]];
    while let Some(path) = stack.pop() {
        let Some(&last) = path.last() else {
            continue;
        };
        for &to in next.get(&last).into_iter().flatten() {
            if to == item {
                let mut cycle = vec![item];
                cycle.extend(path);
                cycle.push(item);
                return Some(cycle);
            }
            if seen.insert(to) {
                let mut longer = path.clone();
                longer.push(to);
                stack.push(longer);
            }
        }
    }
    None
}

/// The guard for adding "`item` depends on `dep`".
///
/// Refused, in this order: itself, an item not in the backlog or blocked, a blocked item that does not return to the
/// backlog, a cancelled dependency, a cycle. A dependency already done is allowed (it gates nothing).
///
/// # Errors
///
/// Returns the [`DepRefusal`] that names why.
pub fn check_dependency(
    edges: &[(i64, i64)],
    item: DepNode,
    dep: DepNode,
) -> Result<(), DepRefusal> {
    if item.id == dep.id {
        return Err(DepRefusal::SelfDependency);
    }
    if !matches!(item.status, ItemStatus::Backlog | ItemStatus::Blocked) {
        return Err(DepRefusal::ItemStarted {
            id: item.id,
            status: item.status,
        });
    }
    if item.status == ItemStatus::Blocked && item.returns_to != Some(ReturnTo::Backlog) {
        return Err(DepRefusal::BlockedInProgress(item.id));
    }
    if dep.status == ItemStatus::Cancelled {
        return Err(DepRefusal::DependencyCancelled(dep.id));
    }
    if let Some(cycle) = would_cycle(edges, item.id, dep.id) {
        return Err(DepRefusal::Cycle(cycle));
    }
    Ok(())
}

/// The dependencies of an item that still hold it back, split the one way every part of the backlog reads them: a
/// cancelled dependency first, then one that is not done.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WaitingDependencies {
    /// The dependencies neither done nor cancelled, in the order given.
    pub open: Vec<i64>,
    /// The cancelled dependencies, in the order given.
    pub cancelled: Vec<i64>,
}

impl WaitingDependencies {
    /// Splits `(dependency, status)` pairs; done dependencies gate nothing and are dropped.
    #[must_use]
    pub fn of(dependencies: &[(i64, ItemStatus)]) -> Self {
        let mut waiting = Self::default();
        for &(id, status) in dependencies {
            match status {
                ItemStatus::Done => {}
                ItemStatus::Cancelled => waiting.cancelled.push(id),
                ItemStatus::Backlog | ItemStatus::InProgress | ItemStatus::Blocked => {
                    waiting.open.push(id);
                }
            }
        }
        waiting
    }

    /// Whether any dependency holds the item back.
    #[must_use]
    pub const fn any(&self) -> bool {
        !self.open.is_empty() || !self.cancelled.is_empty()
    }

    /// The ids with what holds them back, as `#4 (open), #3 (cancelled)`.
    #[must_use]
    pub fn describe(&self) -> String {
        self.open
            .iter()
            .map(|id| format!("#{id} (open)"))
            .chain(self.cancelled.iter().map(|id| format!("#{id} (cancelled)")))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The move that makes the block of an item match its dependencies, or `None` when it already does.
///
/// Only an item in the backlog, or blocked for `depends` or `dep_cancelled`, is touched; every other status is left alone.
/// A cancelled dependency beats an open one: it blocks `dep_cancelled` with the note `#<dep> cancelled`, else an open one
/// blocks `depends` with the note `#<dep>`; with none waiting, a dependency block is lifted.
#[must_use]
pub fn dependency_block(
    status: &BacklogStatus,
    dependencies: &[(i64, ItemStatus)],
) -> Option<Target> {
    let held = match status {
        BacklogStatus::Backlog => None,
        BacklogStatus::Blocked(block)
            if matches!(
                block.reason,
                BlockReason::Depends | BlockReason::DepCancelled
            ) =>
        {
            Some(block.reason)
        }
        BacklogStatus::Blocked(_)
        | BacklogStatus::InProgress
        | BacklogStatus::Done
        | BacklogStatus::Cancelled => return None,
    };
    let waiting = WaitingDependencies::of(dependencies);
    let wanted = waiting
        .cancelled
        .first()
        .map(|dep| (BlockReason::DepCancelled, format!("#{dep} cancelled")))
        .or_else(|| {
            waiting
                .open
                .first()
                .map(|dep| (BlockReason::Depends, format!("#{dep}")))
        });
    match (wanted, held) {
        (Some((reason, note)), current) if current != Some(reason) => Some(Target::Blocked {
            reason,
            note: Some(note),
        }),
        (None, Some(_)) => Some(Target::Unblock),
        _ => None,
    }
}

/// The guard for the shape of a new item: a parent must be an epic with no parent, and an epic has none.
///
/// # Errors
///
/// Returns the [`DepRefusal`] that names why.
pub const fn check_parent(kind: ItemKind, parent: Option<ParentFacts>) -> Result<(), DepRefusal> {
    let Some(parent) = parent else {
        return Ok(());
    };
    if matches!(kind, ItemKind::Epic) {
        return Err(DepRefusal::EpicWithParent);
    }
    if !matches!(parent.kind, ItemKind::Epic) {
        return Err(DepRefusal::ParentNotEpic(parent.id));
    }
    if parent.parent.is_some() {
        return Err(DepRefusal::ParentHasParent(parent.id));
    }
    Ok(())
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests")]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::backlog::Block;

    const fn node(id: i64, status: ItemStatus) -> DepNode {
        DepNode {
            id,
            status,
            returns_to: Some(ReturnTo::Backlog),
        }
    }

    type Row<'a> = (&'a BacklogStatus, Vec<(i64, ItemStatus)>, Option<Target>);

    fn blocked(reason: BlockReason) -> BacklogStatus {
        BacklogStatus::Blocked(Block {
            reason,
            note: None,
            before: ReturnTo::Backlog,
        })
    }

    fn block_to(reason: BlockReason, note: &str) -> Target {
        Target::Blocked {
            reason,
            note: Some(note.to_owned()),
        }
    }

    #[test]
    fn the_dependency_block_follows_the_dependencies() {
        use ItemStatus as S;
        let depends = blocked(BlockReason::Depends);
        let cancelled = blocked(BlockReason::DepCancelled);
        let backlog = BacklogStatus::Backlog;
        let table: Vec<Row<'_>> = vec![
            (&backlog, vec![], None),
            (&backlog, vec![(1, S::Done)], None),
            (
                &backlog,
                vec![(1, S::Done), (2, S::InProgress)],
                Some(block_to(BlockReason::Depends, "#2")),
            ),
            (
                &backlog,
                vec![(1, S::Blocked)],
                Some(block_to(BlockReason::Depends, "#1")),
            ),
            (
                &backlog,
                vec![(1, S::Backlog), (2, S::Cancelled)],
                Some(block_to(BlockReason::DepCancelled, "#2 cancelled")),
            ),
            (
                &backlog,
                vec![(3, S::Cancelled), (2, S::Cancelled)],
                Some(block_to(BlockReason::DepCancelled, "#3 cancelled")),
            ),
            (&depends, vec![], Some(Target::Unblock)),
            (&depends, vec![(1, S::Done)], Some(Target::Unblock)),
            (&depends, vec![(1, S::Backlog)], None),
            (
                &depends,
                vec![(1, S::Cancelled)],
                Some(block_to(BlockReason::DepCancelled, "#1 cancelled")),
            ),
            (&cancelled, vec![(1, S::Cancelled), (2, S::Backlog)], None),
            (
                &cancelled,
                vec![(2, S::Backlog)],
                Some(block_to(BlockReason::Depends, "#2")),
            ),
            (&cancelled, vec![(1, S::Done)], Some(Target::Unblock)),
        ];
        for (status, dependencies, want) in table {
            assert_eq!(
                dependency_block(status, &dependencies),
                want,
                "{status} {dependencies:?}"
            );
        }
    }

    #[test]
    fn the_dependency_block_leaves_other_statuses_alone() {
        let waiting = [(1, ItemStatus::Cancelled), (2, ItemStatus::Backlog)];
        for status in [
            BacklogStatus::InProgress,
            BacklogStatus::Done,
            BacklogStatus::Cancelled,
            blocked(BlockReason::Decision),
            blocked(BlockReason::Event),
            blocked(BlockReason::TaskLost),
            blocked(BlockReason::Partial),
        ] {
            assert_eq!(dependency_block(&status, &waiting), None, "{status}");
            assert_eq!(dependency_block(&status, &[]), None, "{status}");
        }
    }

    #[test]
    fn waiting_dependencies_split_open_from_cancelled() {
        let waiting = WaitingDependencies::of(&[
            (1, ItemStatus::Done),
            (4, ItemStatus::Blocked),
            (3, ItemStatus::Cancelled),
            (5, ItemStatus::InProgress),
        ]);
        assert_eq!(waiting.open, vec![4, 5]);
        assert_eq!(waiting.cancelled, vec![3]);
        assert!(waiting.any());
        assert_eq!(waiting.describe(), "#4 (open), #5 (open), #3 (cancelled)");
        assert!(!WaitingDependencies::of(&[(1, ItemStatus::Done)]).any());
    }

    #[test]
    fn an_item_blocked_out_of_progress_takes_no_dependency() {
        let blocked = DepNode {
            returns_to: Some(ReturnTo::InProgress),
            ..node(1, ItemStatus::Blocked)
        };
        assert_eq!(
            check_dependency(&[], blocked, node(2, ItemStatus::Backlog)),
            Err(DepRefusal::BlockedInProgress(1))
        );
        let back = DepNode {
            returns_to: Some(ReturnTo::Backlog),
            ..blocked
        };
        assert_eq!(
            check_dependency(&[], back, node(2, ItemStatus::Backlog)),
            Ok(())
        );
    }

    #[test]
    fn a_self_dependency_is_a_cycle_and_refused() {
        assert_eq!(would_cycle(&[], 1, 1), Some(vec![1, 1]));
        assert_eq!(
            check_dependency(
                &[],
                node(1, ItemStatus::Backlog),
                node(1, ItemStatus::Backlog)
            ),
            Err(DepRefusal::SelfDependency)
        );
    }

    #[test]
    fn a_direct_and_a_long_cycle_give_their_path() {
        assert_eq!(would_cycle(&[(2, 1)], 1, 2), Some(vec![1, 2, 1]));
        assert_eq!(
            would_cycle(&[(2, 3), (3, 4), (4, 1)], 1, 2),
            Some(vec![1, 2, 3, 4, 1])
        );
    }

    #[test]
    fn a_diamond_and_a_chain_are_no_cycle() {
        let edges = [(2, 1), (3, 1), (4, 2), (4, 3)];
        assert_eq!(would_cycle(&edges, 5, 4), None);
        assert_eq!(would_cycle(&edges, 3, 2), None);
        assert_eq!(would_cycle(&[(1, 2)], 2, 3), None);
    }

    #[test]
    fn the_refusal_names_the_cycle() {
        let refusal = check_dependency(
            &[(2, 1)],
            node(1, ItemStatus::Backlog),
            node(2, ItemStatus::Backlog),
        )
        .unwrap_err();
        assert_eq!(refusal, DepRefusal::Cycle(vec![1, 2, 1]));
        assert!(refusal.to_string().contains("#1 -> #2 -> #1"));
    }

    #[test]
    fn only_an_item_in_the_backlog_or_blocked_takes_a_dependency() {
        let dep = node(2, ItemStatus::Backlog);
        for (status, ok) in [
            (ItemStatus::Backlog, true),
            (ItemStatus::Blocked, true),
            (ItemStatus::InProgress, false),
            (ItemStatus::Done, false),
            (ItemStatus::Cancelled, false),
        ] {
            let got = check_dependency(&[], node(1, status), dep);
            assert_eq!(got.is_ok(), ok, "{status}");
            if !ok {
                assert_eq!(got, Err(DepRefusal::ItemStarted { id: 1, status }));
            }
        }
    }

    #[test]
    fn a_cancelled_dependency_is_refused_and_any_other_status_is_allowed() {
        let item = node(1, ItemStatus::Backlog);
        assert_eq!(
            check_dependency(&[], item, node(2, ItemStatus::Cancelled)),
            Err(DepRefusal::DependencyCancelled(2))
        );
        for status in [
            ItemStatus::Backlog,
            ItemStatus::InProgress,
            ItemStatus::Blocked,
            ItemStatus::Done,
        ] {
            assert_eq!(check_dependency(&[], item, node(2, status)), Ok(()));
        }
    }

    #[test]
    fn a_parent_must_be_a_top_level_epic() {
        let epic = ParentFacts {
            id: 9,
            kind: ItemKind::Epic,
            parent: None,
        };
        assert_eq!(check_parent(ItemKind::Item, None), Ok(()));
        assert_eq!(check_parent(ItemKind::Epic, None), Ok(()));
        assert_eq!(check_parent(ItemKind::Item, Some(epic)), Ok(()));
        assert_eq!(
            check_parent(ItemKind::Epic, Some(epic)),
            Err(DepRefusal::EpicWithParent)
        );
        assert_eq!(
            check_parent(
                ItemKind::Item,
                Some(ParentFacts {
                    kind: ItemKind::Item,
                    ..epic
                })
            ),
            Err(DepRefusal::ParentNotEpic(9))
        );
        assert_eq!(
            check_parent(
                ItemKind::Item,
                Some(ParentFacts {
                    parent: Some(3),
                    ..epic
                })
            ),
            Err(DepRefusal::ParentHasParent(9))
        );
    }

    proptest! {
        #[test]
        fn random_adds_through_the_guard_never_leave_a_cycle(
            pairs in proptest::collection::vec((1_i64..8, 1_i64..8), 0..60)
        ) {
            let mut edges: Vec<(i64, i64)> = Vec::new();
            for (item, dep) in pairs {
                let ok = check_dependency(
                    &edges,
                    node(item, ItemStatus::Backlog),
                    node(dep, ItemStatus::Backlog),
                )
                .is_ok();
                if ok && !edges.contains(&(item, dep)) {
                    edges.push((item, dep));
                }
                for &(from, to) in &edges {
                    let without: Vec<_> = edges.iter().copied().filter(|e| *e != (from, to)).collect();
                    prop_assert!(would_cycle(&without, from, to).is_none(), "{edges:?}");
                }
            }
        }
    }
}
