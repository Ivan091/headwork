//! Dependencies between backlog items and the shape of epics (`docs/backlog.md`, section 11).
//!
//! Pure: the store gathers the facts (the edges, the statuses, the kinds), asks these guards and writes the rows.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::UnknownWord;
use crate::backlog::ItemStatus;
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

/// The guard for adding "`item` depends on `dep`": not itself, the item in the backlog or blocked, the dependency not
/// cancelled, not a cycle. A dependency already done is allowed (it gates nothing).
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
    if dep.status == ItemStatus::Cancelled {
        return Err(DepRefusal::DependencyCancelled(dep.id));
    }
    if let Some(cycle) = would_cycle(edges, item.id, dep.id) {
        return Err(DepRefusal::Cycle(cycle));
    }
    Ok(())
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

    const fn node(id: i64, status: ItemStatus) -> DepNode {
        DepNode { id, status }
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
