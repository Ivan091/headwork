//! The kanban board of the dashboard (`docs/dashboard.md`, section 6a): backlog items and the tasks that work on them in,
//! columns, groups and lanes out.
//!
//! Columns are the item statuses. The tasks' statuses nest inside the In progress column as groups, in the order the
//! user should look at them. [`build`] is pure: facts in, board out.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::TaskState;
use crate::TaskStatus;
use crate::backlog::{BacklogDate, BacklogItem, BacklogStatus, ItemKind, ItemStatus};

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
    /// The project the task belongs to.
    pub project: String,
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

/// One dependency of a card: the item it waits on and where that item stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DepLabel {
    /// The item's id.
    pub id: i64,
    /// Its status; an item that is gone is left out.
    pub status: ItemStatus,
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
    /// The items it waits on, `after #12, #14`, each with its status.
    pub deps: Vec<DepLabel>,
}

/// How many children of an epic stand in one status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StatusCount {
    /// The status.
    pub status: ItemStatus,
    /// The children in it.
    pub count: usize,
}

/// The header of an epic's lane: the epic and what its children add up to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EpicHead {
    /// The epic item.
    pub item: BacklogItem,
    /// Its children by status, the statuses that hold none left out.
    pub counts: Vec<StatusCount>,
    /// The tokens its children's tasks spent.
    pub tokens: u64,
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

/// A swimlane: a project, or an epic holding its children.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Lane {
    /// `project:<name>`, or `epic:<id>`.
    pub key: String,
    /// What the lane is called.
    pub title: String,
    /// For an epic's lane, its header.
    pub epic: Option<EpicHead>,
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

/// The lane key of an epic.
#[must_use]
pub fn epic_lane_key(id: i64) -> String {
    format!("epic:{id}")
}

/// Builds the board from every item, the top-level tasks on the board, the ids waiting on the user and the tokens
/// each task spent (`spend`, by task id).
///
/// An epic is a lane, not a card: its children sit in its lane, in the column of their own status.
#[must_use]
pub fn build(
    items: &[BacklogItem],
    tasks: &[TaskCard],
    waiting: &[i64],
    spend: &BTreeMap<i64, u64>,
    today: &BacklogDate,
    options: &Options,
) -> Kanban {
    let by_id: BTreeMap<i64, &TaskCard> = tasks.iter().map(|task| (task.id, task)).collect();
    let linked: BTreeSet<i64> = items
        .iter()
        .filter_map(|item| item.task.as_ref().map(|task| task.id))
        .collect();
    let epics: BTreeMap<i64, &BacklogItem> = items
        .iter()
        .filter(|item| item.kind == ItemKind::Epic)
        .map(|item| (item.id, item))
        .collect();
    let epic_of = |item: &BacklogItem| item.parent.filter(|parent| epics.contains_key(parent));
    let project_ok = |project: &str| {
        options
            .project
            .as_ref()
            .is_none_or(|wanted| wanted == project)
    };
    let shown = |item: &BacklogItem| {
        project_ok(&item.draft.project)
            || epic_of(item)
                .and_then(|parent| epics.get(&parent))
                .is_some_and(|epic| project_ok(&epic.draft.project))
    };
    let mut cards: Vec<Card> = items
        .iter()
        .filter(|item| item.kind == ItemKind::Item && shown(item))
        .map(|item| card(item, &by_id, waiting, today, items, epic_of(item)))
        .collect();
    cards.sort_by_key(|card| card.held);
    let mut lanes: BTreeMap<(String, i64), Lane> = BTreeMap::new();
    for card in &cards {
        if !card.lane.starts_with("project:") {
            continue;
        }
        let project = card.item.draft.project.clone();
        lanes.entry((project.clone(), 0)).or_insert_with(|| Lane {
            key: lane_key(&project),
            title: project,
            epic: None,
        });
    }
    for epic in epics.values() {
        let children: Vec<&BacklogItem> = items
            .iter()
            .filter(|item| item.parent == Some(epic.id) && item.kind == ItemKind::Item)
            .collect();
        if !project_ok(&epic.draft.project) && !children.iter().any(|child| shown(child)) {
            continue;
        }
        lanes.insert(
            (epic.draft.project.clone(), epic.id),
            Lane {
                key: epic_lane_key(epic.id),
                title: format!("#{} {}", epic.id, epic.draft.text),
                epic: Some(EpicHead {
                    item: (*epic).clone(),
                    counts: counts(&children),
                    tokens: children
                        .iter()
                        .filter_map(|child| child.task.as_ref().map(|task| task.id))
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .map(|task| spend.get(&task).copied().unwrap_or(0))
                        .sum(),
                }),
            },
        );
    }
    let lanes: Vec<Lane> = lanes.into_values().collect();
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
                .is_none_or(|project| *project == task.project)
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

fn counts(children: &[&BacklogItem]) -> Vec<StatusCount> {
    [
        ItemStatus::Backlog,
        ItemStatus::InProgress,
        ItemStatus::Blocked,
        ItemStatus::Done,
        ItemStatus::Cancelled,
    ]
    .into_iter()
    .map(|status| StatusCount {
        status,
        count: children
            .iter()
            .filter(|child| child.draft.status.kind() == status)
            .count(),
    })
    .filter(|entry| entry.count > 0)
    .collect()
}

fn card(
    item: &BacklogItem,
    tasks: &BTreeMap<i64, &TaskCard>,
    waiting: &[i64],
    today: &BacklogDate,
    all: &[BacklogItem],
    epic: Option<i64>,
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
        lane: epic.map_or_else(|| lane_key(&item.draft.project), epic_lane_key),
        deps: item
            .deps
            .iter()
            .filter_map(|id| all.iter().find(|other| other.id == *id))
            .map(|other| DepLabel {
                id: other.id,
                status: other.draft.status.kind(),
            })
            .collect(),
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
            project: "headwork".to_owned(),
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
                &BTreeMap::new(),
                &date("2026-11-28"),
                &Options::default(),
            );
            assert_eq!(group(&board, key), vec![1], "{status}");
            let drift = board.columns[1]
                .groups
                .iter()
                .flat_map(|group| &group.cards)
                .any(|card| card.drift);
            assert_eq!(drift, key == NO_TASK, "drift mark for {status}");
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
        let board = build(
            &items,
            &[],
            &[],
            &BTreeMap::new(),
            &date("2026-11-28"),
            &Options::default(),
        );
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
    fn the_project_filter_keeps_unlinked_tasks_of_prefix_overlapping_projects_apart() {
        let mut site = task(8, TaskStatus::Queued);
        site.name = "me-sim-site-foo".to_owned();
        site.project = "me-sim-site".to_owned();
        let mut sim = task(9, TaskStatus::Queued);
        sim.name = "me-sim-bar".to_owned();
        sim.project = "me-sim".to_owned();
        let board = build(
            &[],
            &[site, sim],
            &[],
            &date("2026-11-28"),
            &Options {
                project: Some("me-sim".to_owned()),
                ..Options::default()
            },
        );
        assert_eq!(
            board
                .unlinked
                .iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec![9]
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
            &BTreeMap::new(),
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
        let board = build(
            &items,
            &[],
            &[],
            &BTreeMap::new(),
            &date("2026-11-28"),
            &Options::default(),
        );
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
        let board = build(
            &items,
            &[],
            &[],
            &BTreeMap::new(),
            &date("2026-11-28"),
            &Options::default(),
        );
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
        let all = build(
            &items,
            &[],
            &[2],
            &BTreeMap::new(),
            &date("2026-11-28"),
            &Options::default(),
        );
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
            &BTreeMap::new(),
            &date("2026-11-28"),
            &Options {
                project: Some("me-sim".to_owned()),
                ..Options::default()
            },
        );
        assert_eq!(one.lanes.len(), 1, "one lane");
        assert_eq!(one.columns[0].count, 1, "one card");
    }

    fn epic(id: i64, project: &str) -> BacklogItem {
        let mut epic = item(id, project, BacklogStatus::Backlog, None);
        epic.kind = ItemKind::Epic;
        epic
    }

    fn child(
        id: i64,
        project: &str,
        parent: i64,
        status: BacklogStatus,
        task: Option<i64>,
    ) -> BacklogItem {
        let mut child = item(
            id,
            project,
            status,
            task.map(|task| (task, TaskState::Working)),
        );
        child.parent = Some(parent);
        child
    }

    #[test]
    fn an_epic_is_a_lane_with_its_children_counted_and_their_tokens_summed() {
        let items = [
            epic(1, "headwork"),
            child(2, "headwork", 1, BacklogStatus::Backlog, None),
            child(3, "me-sim", 1, BacklogStatus::InProgress, Some(7)),
            child(4, "headwork", 1, BacklogStatus::Done, Some(8)),
            item(5, "headwork", BacklogStatus::Backlog, None),
        ];
        let spend = BTreeMap::from([(7, 300), (8, 1200), (9, 5)]);
        let board = build(
            &items,
            &[task(7, TaskStatus::Working)],
            &[],
            &spend,
            &date("2026-11-28"),
            &Options::default(),
        );
        let keys: Vec<&str> = board.lanes.iter().map(|lane| lane.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["project:headwork", "epic:1"],
            "project lane, then its epic"
        );
        let head = board.lanes[1].epic.as_ref().expect("epic head");
        assert_eq!(head.tokens, 1500, "summed tokens");
        let mut shared = items.to_vec();
        shared.push(child(6, "headwork", 1, BacklogStatus::Done, Some(8)));
        let again = build(
            &shared,
            &[],
            &[],
            &spend,
            &date("2026-11-28"),
            &Options::default(),
        );
        assert_eq!(
            again.lanes[1].epic.as_ref().expect("head").tokens,
            1500,
            "a task shared by two children counts once"
        );
        let counts: Vec<(ItemStatus, usize)> = head
            .counts
            .iter()
            .map(|entry| (entry.status, entry.count))
            .collect();
        assert_eq!(
            counts,
            vec![
                (ItemStatus::Backlog, 1),
                (ItemStatus::InProgress, 1),
                (ItemStatus::Done, 1)
            ],
            "counts by status"
        );
        let lane_of = |id: i64| {
            board
                .columns
                .iter()
                .flat_map(|column| {
                    column
                        .cards
                        .iter()
                        .chain(column.groups.iter().flat_map(|group| group.cards.iter()))
                })
                .find(|card| card.item.id == id)
                .map(|card| card.lane.clone())
        };
        assert_eq!(
            lane_of(3).as_deref(),
            Some("epic:1"),
            "child outside the epic's project"
        );
        assert_eq!(
            lane_of(5).as_deref(),
            Some("project:headwork"),
            "loose item"
        );
        assert_eq!(lane_of(1), None, "the epic is no card");
    }

    #[test]
    fn a_project_filter_keeps_the_epic_of_a_child_it_shows() {
        let items = [
            epic(1, "headwork"),
            child(3, "me-sim", 1, BacklogStatus::Backlog, None),
        ];
        let options = Options {
            project: Some("me-sim".to_owned()),
            ..Options::default()
        };
        let board = build(
            &items,
            &[],
            &[],
            &BTreeMap::new(),
            &date("2026-11-28"),
            &options,
        );
        assert_eq!(board.lanes.len(), 1, "one lane");
        assert_eq!(board.lanes[0].key, "epic:1", "the epic's lane");
        assert_eq!(board.columns[0].count, 1, "the child shows");
    }

    #[test]
    fn a_card_carries_its_dependencies_with_their_status() {
        let mut waiting = item(1, "headwork", BacklogStatus::Backlog, None);
        waiting.deps = vec![2, 3, 99];
        let items = [
            waiting,
            item(2, "headwork", BacklogStatus::Done, None),
            item(3, "headwork", BacklogStatus::InProgress, None),
        ];
        let board = build(
            &items,
            &[],
            &[],
            &BTreeMap::new(),
            &date("2026-11-28"),
            &Options::default(),
        );
        let labels: Vec<(i64, ItemStatus)> = board.columns[0].cards[0]
            .deps
            .iter()
            .map(|dep| (dep.id, dep.status))
            .collect();
        assert_eq!(
            labels,
            vec![(2, ItemStatus::Done), (3, ItemStatus::InProgress)],
            "gone item left out"
        );
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
                let board = build(&items, &[], &[], &BTreeMap::new(), &date("2026-11-28"), &options);
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
