//! The kanban board of the dashboard (`docs/dashboard.md`, section 6a): backlog items and the tasks that work on them in,
//! columns, groups and lanes out.
//!
//! Columns are the item statuses. The tasks' statuses nest inside the In progress column as groups, in the order the
//! user should look at them. [`build`] is pure: facts in, board out.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::TaskState;
use crate::TaskStatus;
use crate::backlog::{BacklogDate, BacklogItem, BacklogStatus, ItemStatus};

/// The key of the group of In progress cards whose task is gone or never was.
pub const NO_TASK: &str = "no-task";

/// The review open on a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CardReview {
    /// The level: 1 light, 2 standard, 3 strict.
    pub level: u8,
    /// The round, from 1.
    pub round: u8,
    /// The review's state word.
    pub state: String,
    /// Findings waiting on the implementing lead.
    pub findings: u32,
}

/// What a card shows of a top-level task, its parts rolled in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskCard {
    /// The task's id.
    pub id: i64,
    /// `<project>-<slug>`.
    pub name: String,
    /// The lifecycle state.
    pub state: TaskState,
    /// The rolled-up status.
    pub status: TaskStatus,
    /// The model.
    pub model: String,
    /// The effort, `?` when none is stored.
    pub effort: String,
    /// The level of the review planned for it.
    pub review_plan: Option<u8>,
    /// The review open on it.
    pub review: Option<CardReview>,
    /// The open pull requests of the task and its parts.
    pub prs: Vec<String>,
    /// How many parts it has.
    pub parts: u32,
    /// The parts that wait on the user.
    pub asking_parts: Vec<String>,
    /// The tokens it and its parts spent.
    pub tokens: u64,
    /// Their estimate at list prices, when prices are configured.
    pub estimate: Option<String>,
    /// How many tick findings stand on it.
    pub findings: u32,
    /// The gate class a waiter of it waits a slot of.
    pub waiting_for: Option<String>,
    /// Whether it closes without asking the user.
    pub silent: bool,
    /// The dependency that is gone, for a queued task.
    pub dead_dependency: Option<String>,
    /// When it last changed.
    pub updated_at: String,
}

/// How [`build`] cuts the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Only this project.
    pub project: Option<String>,
    /// How many Done cards to show, the newest first.
    pub done: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            project: None,
            done: crate::backlog::DONE_KEPT,
        }
    }
}

/// One item on the board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Card {
    /// The item, as `/api/backlog` serialises it.
    pub item: BacklogItem,
    /// The lane it sits in.
    pub lane: String,
    /// Whether it waits on the user (the digest's rule).
    pub waiting: bool,
    /// Whether a future `not_before` holds it back.
    pub held: bool,
    /// Whether an in progress item has no live task.
    pub drift: bool,
    /// The task working on it, when it is on the board.
    pub task: Option<TaskCard>,
}

/// A run of In progress cards that share a task status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Group {
    /// `needs-you`, `working`, `in-review`, `merged`, `queued`, `paused` or [`NO_TASK`].
    pub key: &'static str,
    /// Its cards.
    pub cards: Vec<Card>,
}

/// One column: an item status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Column {
    /// The item status.
    pub status: ItemStatus,
    /// How many items it holds, shown or not.
    pub count: usize,
    /// The cards, for every column but In progress.
    pub cards: Vec<Card>,
    /// The groups, for In progress.
    pub groups: Vec<Group>,
    /// How many Done cards the limit left out.
    pub more: usize,
}

/// A swimlane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Lane {
    /// `project:<name>`.
    pub key: String,
    /// What the lane is called.
    pub title: String,
}

/// The whole board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Kanban {
    /// The store's date.
    pub today: String,
    /// The five columns, in order.
    pub columns: Vec<Column>,
    /// The lanes that hold a card, by name.
    pub lanes: Vec<Lane>,
    /// The open items that wait on the user, by the digest's rule.
    pub waiting: Vec<i64>,
    /// Top-level tasks no item links (started before every task got one).
    pub unlinked: Vec<TaskCard>,
}

/// The In progress group a task's status puts its card in; `None` for the statuses an in progress item should not have.
#[must_use]
pub const fn group_of(status: TaskStatus) -> Option<&'static str> {
    match status {
        TaskStatus::NeedsYou => Some("needs-you"),
        TaskStatus::Working => Some("working"),
        TaskStatus::InReview => Some("in-review"),
        TaskStatus::Finished => Some("merged"),
        TaskStatus::Queued => Some("queued"),
        TaskStatus::Parked | TaskStatus::Interrupted => Some("paused"),
        TaskStatus::Failed | TaskStatus::Dropped => None,
    }
}

/// The group keys in the order the board shows them.
pub const GROUPS: [&str; 7] = [
    "needs-you",
    "working",
    "in-review",
    "merged",
    "queued",
    "paused",
    NO_TASK,
];

/// The lane key of a project.
#[must_use]
pub fn lane_key(project: &str) -> String {
    format!("project:{project}")
}

/// Builds the board from every item, the top-level tasks on the board and the ids waiting on the user.
#[must_use]
pub fn build(
    items: &[BacklogItem],
    tasks: &[TaskCard],
    waiting: &[i64],
    today: &BacklogDate,
    options: &Options,
) -> Kanban {
    let by_id: BTreeMap<i64, &TaskCard> = tasks.iter().map(|task| (task.id, task)).collect();
    let linked: BTreeSet<i64> = items
        .iter()
        .filter_map(|item| item.task.as_ref().map(|task| task.id))
        .collect();
    let mut cards: Vec<Card> = items
        .iter()
        .filter(|item| {
            options
                .project
                .as_ref()
                .is_none_or(|project| *project == item.draft.project)
        })
        .map(|item| card(item, &by_id, waiting, today))
        .collect();
    cards.sort_by_key(|card| card.held);
    let lanes: Vec<Lane> = cards
        .iter()
        .map(|card| card.item.draft.project.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|project| Lane {
            key: lane_key(&project),
            title: project,
        })
        .collect();
    let columns = [
        ItemStatus::Backlog,
        ItemStatus::InProgress,
        ItemStatus::Blocked,
        ItemStatus::Done,
        ItemStatus::Cancelled,
    ]
    .into_iter()
    .map(|status| column(status, &cards, options))
    .collect();
    let unlinked = tasks
        .iter()
        .filter(|task| !linked.contains(&task.id))
        .filter(|task| {
            options
                .project
                .as_ref()
                .is_none_or(|project| task.name.starts_with(&format!("{project}-")))
        })
        .cloned()
        .collect();
    Kanban {
        today: today.to_string(),
        columns,
        lanes,
        waiting: waiting.to_vec(),
        unlinked,
    }
}

fn card(
    item: &BacklogItem,
    tasks: &BTreeMap<i64, &TaskCard>,
    waiting: &[i64],
    today: &BacklogDate,
) -> Card {
    let task = item
        .task
        .as_ref()
        .and_then(|linked| tasks.get(&linked.id))
        .map(|task| (*task).clone());
    let in_progress = item.draft.status == BacklogStatus::InProgress;
    let drift = in_progress
        && task
            .as_ref()
            .is_none_or(|task| group_of(task.status).is_none());
    Card {
        lane: lane_key(&item.draft.project),
        waiting: waiting.contains(&item.id),
        held: item
            .draft
            .not_before
            .as_ref()
            .is_some_and(|date| date > today),
        drift,
        task,
        item: item.clone(),
    }
}

fn column(status: ItemStatus, cards: &[Card], options: &Options) -> Column {
    let mut mine: Vec<Card> = cards
        .iter()
        .filter(|card| card.item.draft.status.kind() == status)
        .cloned()
        .collect();
    let count = mine.len();
    let mut more = 0;
    let mut groups = Vec::new();
    match status {
        ItemStatus::InProgress => {
            groups = GROUPS
                .iter()
                .map(|key| Group {
                    key,
                    cards: mine
                        .iter()
                        .filter(|card| group_key(card) == *key)
                        .cloned()
                        .collect(),
                })
                .collect();
            mine.clear();
        }
        ItemStatus::Done => {
            mine.sort_by(|a, b| {
                (&b.item.draft.done_on, b.item.id).cmp(&(&a.item.draft.done_on, a.item.id))
            });
            more = count.saturating_sub(options.done);
            mine.truncate(options.done);
        }
        ItemStatus::Backlog | ItemStatus::Blocked | ItemStatus::Cancelled => {}
    }
    Column {
        status,
        count,
        cards: mine,
        groups,
        more,
    }
}

fn group_key(card: &Card) -> &'static str {
    if card.drift {
        return NO_TASK;
    }
    card.task
        .as_ref()
        .and_then(|task| group_of(task.status))
        .unwrap_or(NO_TASK)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

    use super::*;
    use crate::backlog::{
        BacklogDraft, BacklogOwner, Block, BlockReason, ItemKind, LinkedTask, ReturnTo,
    };

    fn date(text: &str) -> BacklogDate {
        BacklogDate::parse(text).unwrap()
    }

    fn item(
        id: i64,
        project: &str,
        status: BacklogStatus,
        task: Option<(i64, TaskState)>,
    ) -> BacklogItem {
        BacklogItem {
            id,
            position: id,
            task: task.map(|(task, state)| LinkedTask {
                id: task,
                name: format!("{project}-t{task}"),
                state,
            }),
            kind: ItemKind::Item,
            parent: None,
            deps: Vec::new(),
            draft: BacklogDraft {
                project: project.to_owned(),
                section: None,
                text: format!("item {id}"),
                notes: Vec::new(),
                status,
                owner: BacklogOwner::Agent,
                added: None,
                not_before: None,
                due: None,
                done_on: None,
                done_note: None,
            },
        }
    }

    fn blocked() -> BacklogStatus {
        BacklogStatus::Blocked(Block {
            reason: BlockReason::Decision,
            note: None,
            before: ReturnTo::Backlog,
        })
    }

    fn task(id: i64, status: TaskStatus) -> TaskCard {
        TaskCard {
            id,
            name: format!("headwork-t{id}"),
            state: TaskState::Working,
            status,
            model: "sonnet".to_owned(),
            effort: "medium".to_owned(),
            review_plan: None,
            review: None,
            prs: Vec::new(),
            parts: 0,
            asking_parts: Vec::new(),
            tokens: 0,
            estimate: None,
            findings: 0,
            waiting_for: None,
            silent: false,
            dead_dependency: None,
            updated_at: String::new(),
        }
    }

    fn group(board: &Kanban, key: &str) -> Vec<i64> {
        board.columns[1]
            .groups
            .iter()
            .find(|group| group.key == key)
            .expect("group")
            .cards
            .iter()
            .map(|card| card.item.id)
            .collect()
    }

    #[test]
    fn every_task_status_lands_in_its_group() {
        let table = [
            (TaskStatus::NeedsYou, "needs-you"),
            (TaskStatus::Working, "working"),
            (TaskStatus::InReview, "in-review"),
            (TaskStatus::Finished, "merged"),
            (TaskStatus::Queued, "queued"),
            (TaskStatus::Parked, "paused"),
            (TaskStatus::Interrupted, "paused"),
            (TaskStatus::Failed, NO_TASK),
            (TaskStatus::Dropped, NO_TASK),
        ];
        for (status, key) in table {
            let items = [item(
                1,
                "headwork",
                BacklogStatus::InProgress,
                Some((7, TaskState::Working)),
            )];
            let board = build(
                &items,
                &[task(7, status)],
                &[],
                &date("2026-11-28"),
                &Options::default(),
            );
            assert_eq!(group(&board, key), vec![1], "{status}");
        }
    }

    #[test]
    fn an_in_progress_item_without_a_live_task_is_drift() {
        let items = [
            item(1, "headwork", BacklogStatus::InProgress, None),
            item(
                2,
                "headwork",
                BacklogStatus::InProgress,
                Some((9, TaskState::Closed)),
            ),
        ];
        let board = build(&items, &[], &[], &date("2026-11-28"), &Options::default());
        assert_eq!(group(&board, NO_TASK), vec![1, 2], "both under no task");
        assert!(
            board.columns[1].groups[6]
                .cards
                .iter()
                .all(|card| card.drift),
            "drift mark"
        );
    }

    #[test]
    fn a_task_no_item_links_is_unlinked() {
        let items = [item(
            1,
            "headwork",
            BacklogStatus::InProgress,
            Some((7, TaskState::Working)),
        )];
        let board = build(
            &items,
            &[task(7, TaskStatus::Working), task(8, TaskStatus::Queued)],
            &[],
            &date("2026-11-28"),
            &Options::default(),
        );
        assert_eq!(
            board
                .unlinked
                .iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec![8]
        );
    }

    #[test]
    fn done_shows_the_newest_and_counts_the_rest_and_cancelled_is_a_count() {
        let mut items: Vec<BacklogItem> = (1..=25)
            .map(|id| {
                let mut done = item(id, "headwork", BacklogStatus::Done, None);
                done.draft.done_on = Some(date(&format!("2026-11-{:02}", id.min(28))));
                done
            })
            .collect();
        items.push(item(30, "headwork", BacklogStatus::Cancelled, None));
        let board = build(&items, &[], &[], &date("2026-11-28"), &Options::default());
        let done = &board.columns[3];
        assert_eq!(
            (done.count, done.cards.len(), done.more),
            (25, 20, 5),
            "done trimmed"
        );
        assert_eq!(done.cards[0].item.id, 25, "newest first");
        assert_eq!(board.columns[4].count, 1, "cancelled count");
    }

    #[test]
    fn held_items_sort_last_and_blocked_has_its_column() {
        let mut held = item(1, "headwork", BacklogStatus::Backlog, None);
        held.draft.not_before = Some(date("2026-12-01"));
        let items = [
            held,
            item(2, "headwork", BacklogStatus::Backlog, None),
            item(3, "headwork", blocked(), None),
        ];
        let board = build(&items, &[], &[], &date("2026-11-28"), &Options::default());
        let ids: Vec<i64> = board.columns[0]
            .cards
            .iter()
            .map(|card| card.item.id)
            .collect();
        assert_eq!(ids, vec![2, 1], "held last");
        assert!(board.columns[0].cards[1].held, "held flag");
        assert_eq!(board.columns[2].cards.len(), 1, "blocked column");
    }

    #[test]
    fn lanes_are_projects_and_the_filter_narrows_them() {
        let items = [
            item(1, "me-sim", BacklogStatus::Backlog, None),
            item(2, "headwork", BacklogStatus::Backlog, None),
        ];
        let all = build(&items, &[], &[2], &date("2026-11-28"), &Options::default());
        let keys: Vec<&str> = all.lanes.iter().map(|lane| lane.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["project:headwork", "project:me-sim"],
            "sorted lanes"
        );
        assert_eq!(all.waiting, vec![2], "waiting passed on");
        assert!(
            all.columns[0]
                .cards
                .iter()
                .any(|card| card.item.id == 2 && card.waiting),
            "waiting flag"
        );
        let one = build(
            &items,
            &[],
            &[],
            &date("2026-11-28"),
            &Options {
                project: Some("me-sim".to_owned()),
                ..Options::default()
            },
        );
        assert_eq!(one.lanes.len(), 1, "one lane");
        assert_eq!(one.columns[0].count, 1, "one card");
    }

    mod every_item_once {
        use proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn each_item_is_in_exactly_one_column(codes in proptest::collection::vec(0u8..5, 0..40)) {
                let items: Vec<BacklogItem> = codes
                    .iter()
                    .enumerate()
                    .map(|(index, code)| {
                        let id = i64::try_from(index).unwrap() + 1;
                        let status = match code {
                            0 => BacklogStatus::Backlog,
                            1 => BacklogStatus::InProgress,
                            2 => blocked(),
                            3 => BacklogStatus::Done,
                            _ => BacklogStatus::Cancelled,
                        };
                        item(id, "headwork", status, None)
                    })
                    .collect();
                let options = Options { project: None, done: usize::MAX };
                let board = build(&items, &[], &[], &date("2026-11-28"), &options);
                let mut seen: Vec<i64> = Vec::new();
                for column in &board.columns {
                    seen.extend(column.cards.iter().map(|card| card.item.id));
                    for group in &column.groups {
                        seen.extend(group.cards.iter().map(|card| card.item.id));
                    }
                }
                seen.sort_unstable();
                let want: Vec<i64> = items.iter().map(|item| item.id).collect();
                prop_assert_eq!(seen, want);
            }
        }
    }
}
