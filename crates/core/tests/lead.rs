//! Task shapes and the queue order of leads and parts, as tables plus property tests.

use std::cmp::Ordering;

use headwork_core::{QueueEntry, QueueKey, ShapeError, TaskShape, queue_order};
use proptest::prelude::*;

/// `parent_id`, `lead`, and the shape they make.
type Columns = (Option<u8>, bool, Result<TaskShape<u8>, ShapeError>);

#[test]
fn every_column_pair_makes_its_shape_or_is_refused() {
    let table: [Columns; 4] = [
        (None, false, Ok(TaskShape::Plain)),
        (None, true, Ok(TaskShape::Lead)),
        (Some(7), false, Ok(TaskShape::Part { parent: 7 })),
        (Some(7), true, Err(ShapeError::PartIsLead)),
    ];
    for (parent, lead, want) in table {
        assert_eq!(
            TaskShape::from_columns(parent, lead),
            want,
            "parent {parent:?}, lead {lead}"
        );
    }
}

#[test]
fn a_shape_gives_back_its_columns() {
    assert_eq!(TaskShape::<u8>::Plain.parent(), None, "plain parent");
    assert!(!TaskShape::<u8>::Plain.is_lead(), "plain lead");
    assert!(TaskShape::<u8>::Lead.is_lead(), "lead");
    assert!(!TaskShape::<u8>::Lead.is_part(), "a lead is no part");
    let part = TaskShape::Part { parent: 3_u8 };
    assert_eq!(part.parent(), Some(3), "part parent");
    assert!(part.is_part() && !part.is_lead(), "a part is no lead");
}

fn key(id: u32, rank: Option<i64>, at: &str) -> QueueKey<u32> {
    QueueKey {
        rank,
        queued_at: at.to_owned(),
        id,
    }
}

fn plain(id: u32, rank: Option<i64>, at: &str) -> QueueEntry<u32> {
    QueueEntry {
        id,
        key: key(id, rank, at),
        lead: None,
    }
}

fn part(id: u32, at: &str, lead: QueueKey<u32>) -> QueueEntry<u32> {
    QueueEntry {
        id,
        key: key(id, None, at),
        lead: Some(lead),
    }
}

#[test]
fn tasks_without_a_lead_keep_the_old_order() {
    let order = queue_order(vec![
        plain(1, None, "t1"),
        plain(2, Some(2), "t5"),
        plain(3, None, "t0"),
        plain(4, Some(1), "t9"),
        plain(5, None, "t0"),
    ]);
    assert_eq!(
        order,
        vec![4, 2, 3, 5, 1],
        "ranked by rank, then oldest first"
    );
}

#[test]
fn a_waiting_part_sits_right_behind_its_lead_ahead_of_later_tasks() {
    let lead = key(10, None, "t1");
    let order = queue_order(vec![
        plain(2, None, "t2"),
        plain(3, None, "t3"),
        part(12, "t4", lead.clone()),
        plain(1, None, "t0"),
        part(11, "t3", lead),
    ]);
    assert_eq!(
        order,
        vec![1, 11, 12, 2, 3],
        "the lead queued at t1, so its parts go before tasks queued at t2 and t3, oldest part first"
    );
}

#[test]
fn a_part_follows_its_ranked_lead_and_its_queued_lead() {
    let ranked = key(10, Some(1), "t9");
    let order = queue_order(vec![
        plain(2, Some(2), "t0"),
        part(11, "t9", ranked),
        plain(3, None, "t0"),
    ]);
    assert_eq!(order, vec![11, 2, 3], "behind rank 1, ahead of rank 2");
    let lead = key(10, None, "t1");
    let order = queue_order(vec![
        part(11, "t2", lead),
        plain(10, None, "t1"),
        plain(30, None, "t1"),
        plain(3, None, "t1"),
    ]);
    assert_eq!(
        order,
        vec![3, 10, 11, 30],
        "a queued lead goes first, its part right after it, before a task of the same age with a larger id"
    );
}

fn compare(a: &QueueKey<u32>, b: &QueueKey<u32>) -> Ordering {
    (a.rank.is_none(), a.rank, &a.queued_at, a.id).cmp(&(
        b.rank.is_none(),
        b.rank,
        &b.queued_at,
        b.id,
    ))
}

fn queue_key(id: u32) -> impl Strategy<Value = QueueKey<u32>> {
    (proptest::option::of(1..4_i64), 0..6_u8)
        .prop_map(move |(rank, at)| key(id, rank, &format!("t{at}")))
}

/// Up to 8 plain tasks with ids 0..8 and up to 8 parts with ids 100.., each part under one of 3 leads with ids 50..53
/// that are not queued themselves.
fn queue() -> impl Strategy<Value = Vec<QueueEntry<u32>>> {
    let plains = proptest::collection::vec(queue_key(0), 0..8).prop_map(|keys| {
        keys.into_iter()
            .zip(0_u32..)
            .map(|(key, id)| QueueEntry {
                id,
                key: QueueKey { id, ..key },
                lead: None,
            })
            .collect::<Vec<_>>()
    });
    let leads = (queue_key(50), queue_key(51), queue_key(52));
    let parts = proptest::collection::vec((0..3_usize, 0..6_u8), 0..8);
    (plains, leads, parts).prop_map(|(mut entries, (a, b, c), parts)| {
        let leads: [QueueKey<u32>; 3] = (a, b, c).into();
        for ((which, at), id) in parts.into_iter().zip(100_u32..) {
            entries.push(QueueEntry {
                id,
                key: key(id, None, &format!("t{at}")),
                lead: leads.get(which).cloned(),
            });
        }
        entries
    })
}

proptest! {
    #[test]
    fn the_order_is_a_permutation(entries in queue()) {
        let mut want: Vec<u32> = entries.iter().map(|entry| entry.id).collect();
        let mut got = queue_order(entries);
        want.sort_unstable();
        got.sort_unstable();
        prop_assert_eq!(got, want);
    }

    #[test]
    fn plain_tasks_keep_their_order_among_themselves(entries in queue()) {
        let mut plains: Vec<QueueKey<u32>> =
            entries.iter().filter(|entry| entry.lead.is_none()).map(|entry| entry.key.clone()).collect();
        plains.sort_by(compare);
        let want: Vec<u32> = plains.iter().map(|key| key.id).collect();
        let got: Vec<u32> = queue_order(entries).into_iter().filter(|id| *id < 100).collect();
        prop_assert_eq!(got, want);
    }

    #[test]
    fn a_part_sits_where_its_lead_would(entries in queue()) {
        let order = queue_order(entries.clone());
        let at = |id: u32| order.iter().position(|other| *other == id);
        for part in entries.iter().filter(|entry| entry.lead.is_some()) {
            let lead = part.lead.as_ref().map(|lead| (lead.id, lead.clone()));
            let Some((lead_id, lead)) = lead else { continue };
            for other in &entries {
                let (Some(part_at), Some(other_at)) = (at(part.id), at(other.id)) else { continue };
                match &other.lead {
                    None if compare(&other.key, &lead) == Ordering::Greater => {
                        prop_assert!(part_at < other_at, "part {} before later task {}", part.id, other.id);
                    }
                    None => prop_assert!(part_at > other_at, "part {} after earlier task {}", part.id, other.id),
                    Some(sibling) if sibling.id == lead_id => {
                        let earlier = compare(&other.key, &part.key) == Ordering::Less;
                        prop_assert_eq!(other_at < part_at, earlier, "siblings {} and {} oldest first", other.id, part.id);
                    }
                    Some(_) => {}
                }
            }
        }
    }
}
