//! The agent graph (`docs/dashboard.md`, sections 3 to 6): the store's facts in, nodes and edges out.
//!
//! [`build`] draws the graph as it stood at one instant. A task (one node holding its lead's, part's or worker's
//! sessions) and the orchestrator are drawn while they exist at the instant; the satellites of a drawn task (helpers,
//! jobs, pull requests, reviews and their nodes) are drawn from their start on, ended ones in their end state, and so
//! are subagents, except that the live view of now keeps only those still running under a live session. Past states
//! come from the store's logs: task `transition`, `pr-state`, `job-state`, `job-started`, `job-forgotten` and
//! `review-<trigger>` events
//! ([`Change`]), and a review node's start and finish times. Times are the store's RFC 3339 UTC text, which sorts as it
//! reads. Nothing here reads a file or a database; the store gathers the [`Facts`].

mod build;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub use build::build;

use crate::backlog::ItemStatus;
use crate::review::Level;
use crate::vocabulary::{
    Actor, AskState, Effort, FindingVerdict, JobState, Model, NodeState, PullState, Resolution,
    ReviewState, Role, SessionState, Severity, TaskState, UnknownWord, text_enum,
};

/// The id of the one node that stands for the user.
pub(crate) const USER_NODE: &str = "user";
/// The id of the one node that stands for every orchestrator session.
pub(crate) const ORCHESTRATOR_NODE: &str = "orchestrator";

/// A task as the graph needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskFact {
    /// The store's id.
    pub id: i64,
    /// `<project>-<slug>`; a closed task's name may be taken again by a later task.
    pub name: String,
    /// The project.
    pub project: String,
    /// The task kind.
    pub kind: String,
    /// The state now.
    pub state: TaskState,
    /// The model it runs on.
    pub model: Model,
    /// The effort stored at spawn.
    pub effort: Option<Effort>,
    /// Its lead, for a part.
    pub parent: Option<i64>,
    /// The task it waits for.
    pub depends_on: Option<i64>,
    /// Whether it is a lead.
    pub lead: bool,
    /// Whether it closes without asking the user (`silent`) or asks first (`confirm`).
    pub close: crate::CloseMode,
    /// The actor of its `task-created` event: `user` for a CLI `headwork spawn` (whoever typed it), `orchestrator`
    /// for the orchestrator's MCP `spawn`, `worker` for a lead's `part_spawn`.
    pub created_by: Actor,
    /// When it was queued: its start.
    pub queued_at: String,
    /// Its last report.
    pub last_report: Option<String>,
}

/// A Claude session headwork started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFact {
    /// The Claude session id.
    pub id: String,
    /// Its task, none for the orchestrator.
    pub task: Option<i64>,
    /// The role.
    pub role: Role,
    /// Its Remote Control name: the address other sessions write to.
    pub rc_name: String,
    /// The process state now.
    pub state: SessionState,
    /// When it first started: its first event or its row's start, whichever is earlier (a resume restarts the row).
    pub started_at: String,
    /// When it ended, if it is not running now.
    pub ended_at: Option<String>,
}

/// A subagent a session called (`agents` table); its task is its calling session's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFact {
    /// The agent id.
    pub id: String,
    /// The session that called it.
    pub session: String,
    /// Its type, such as `general-purpose`.
    pub agent_type: Option<String>,
    /// The short label its caller gave it.
    pub description: Option<String>,
    /// The model it asked for, as its meta file names it.
    pub model: Option<String>,
    /// How deep it was nested: 1 for a session's own.
    pub depth: Option<i64>,
    /// The subagent that started it, for a nested one.
    pub parent: Option<String>,
    /// Whether its meta file says it finished; none when it does not say.
    pub finished: Option<bool>,
    /// Its first response; none when its responses were summed into days before the table existed.
    pub first_at: Option<String>,
    /// Its last response.
    pub last_at: Option<String>,
}

/// A helper `claude` a worker opened with `helper_tab`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperFact {
    /// The helper's Claude session id.
    pub session: String,
    /// The task it works for.
    pub task: i64,
    /// When it was opened.
    pub at: String,
}

/// A registered long-running job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobFact {
    /// The store's id.
    pub id: i64,
    /// Its task.
    pub task: Option<i64>,
    /// The session that started it.
    pub session: Option<String>,
    /// Its label.
    pub label: String,
    /// The job state now.
    pub state: JobState,
    /// When it started.
    pub started_at: String,
    /// When it ended.
    pub ended_at: Option<String>,
}

/// A pull request a task delivers in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrFact {
    /// The store's id.
    pub id: i64,
    /// Its task.
    pub task: i64,
    /// The number.
    pub number: i64,
    /// The URL.
    pub url: String,
    /// The pull request state now.
    pub state: PullState,
    /// When the store first learned of it: the first event of its task naming its URL, else its task's start.
    pub first_at: String,
}

/// An ask to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskFact {
    /// The store's id.
    pub id: i64,
    /// Its task.
    pub task: Option<i64>,
    /// The session that asked; none for an ask headwork raised itself (the tick's trust, close or worktree asks).
    pub session: Option<String>,
    /// Its header (topic), if it has one.
    pub header: Option<String>,
    /// The question.
    pub question: String,
    /// The ask state now.
    pub state: AskState,
    /// When it was asked.
    pub created_at: String,
    /// When it was answered or superseded.
    pub closed_at: Option<String>,
}

/// A review the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewFact {
    /// The store's id.
    pub id: i64,
    /// The lead under review.
    pub task: i64,
    /// The reviewer part.
    pub part: Option<i64>,
    /// The flow.
    pub flow: String,
    /// The level.
    pub level: Level,
    /// The review state now.
    pub state: ReviewState,
    /// When it was asked for.
    pub requested_at: String,
    /// When it finished.
    pub finished_at: Option<String>,
}

/// One node run of a review's flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewNodeFact {
    /// The review.
    pub review: i64,
    /// The round.
    pub round: i64,
    /// The flow node's name.
    pub node: String,
    /// The item of an `each` node.
    pub item: i64,
    /// The node state now.
    pub state: NodeState,
    /// When it started; a node never started is drawn from its review's start.
    pub started_at: Option<String>,
    /// When it finished.
    pub finished_at: Option<String>,
}

/// A session message (`SendMessage`), from the `tool-used` event its sender's hook logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageFact {
    /// The sending session.
    pub from: String,
    /// The address as the sender's tool input held it.
    pub to: String,
    /// The sender's short summary, never the body.
    pub summary: Option<String>,
    /// When it was sent.
    pub at: String,
}

/// A session message as its receiver's `prompt` hook logged it: who it came from, by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedFact {
    /// The receiving session.
    pub session: String,
    /// The sender's name (`from-name`).
    pub from: String,
    /// When it arrived.
    pub at: String,
}

/// A `report` a worker made to the orchestrator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportFact {
    /// The reporting session.
    pub session: String,
    /// The status reported.
    pub status: String,
    /// When.
    pub at: String,
}

/// A finding the tick raised about a task (`findings` table), such as a stuck session or a stray tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingFact {
    /// The task it names.
    pub task: i64,
    /// The tick's rule.
    pub rule: String,
    /// What it found.
    pub detail: String,
    /// When it was first seen.
    pub first_at: String,
    /// When it was resolved.
    pub resolved_at: Option<String>,
}

/// A finding a review node raised (`review_findings` table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewFindingFact {
    /// The review.
    pub review: i64,
    /// The round.
    pub round: i64,
    /// The flow node that raised it.
    pub node: String,
    /// How bad.
    pub severity: Severity,
    /// What the verify stage said.
    pub verdict: Option<FindingVerdict>,
    /// Where it stands between the leads.
    pub resolution: Option<Resolution>,
}

/// A backlog item as the graph needs it (`backlog_items`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemFact {
    /// The store's id.
    pub id: i64,
    /// The project.
    pub project: String,
    /// The item's text.
    pub text: String,
    /// Where it stands now; item history is not logged, so a replay shows this.
    pub status: ItemStatus,
    /// Whether it is an epic.
    pub epic: bool,
    /// The epic it belongs to.
    pub parent: Option<i64>,
    /// The task linked to it.
    pub task: Option<i64>,
    /// When it was added.
    pub created_at: String,
}

/// An item waiting for another (`backlog_deps`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepFact {
    /// The item that waits.
    pub item: i64,
    /// The item it waits for.
    pub depends_on: i64,
    /// When the dependency was added.
    pub created_at: String,
}

/// Tokens spent (every input kind plus output), as the usage reader stored them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Spend {
    /// Each task's own tokens.
    pub tasks: BTreeMap<i64, u64>,
    /// Each session's own tokens, its subagents' left out.
    pub sessions: BTreeMap<String, u64>,
    /// Each subagent's tokens.
    pub agents: BTreeMap<String, u64>,
}

/// A logged state change of a task, pull request, job or review, keyed by its row id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change<S> {
    /// The row: a task, pull request, job or review id.
    pub key: i64,
    /// The state before, when the log says it.
    pub from: Option<S>,
    /// The state after.
    pub to: S,
    /// When.
    pub at: String,
}

/// Everything the graph is drawn from, gathered by the store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// Every task.
    pub tasks: Vec<TaskFact>,
    /// Every session.
    pub sessions: Vec<SessionFact>,
    /// Every subagent.
    pub agents: Vec<AgentFact>,
    /// Every helper session.
    pub helpers: Vec<HelperFact>,
    /// Every job.
    pub jobs: Vec<JobFact>,
    /// Every pull request.
    pub prs: Vec<PrFact>,
    /// Every ask.
    pub asks: Vec<AskFact>,
    /// Every review.
    pub reviews: Vec<ReviewFact>,
    /// Every review node run.
    pub review_nodes: Vec<ReviewNodeFact>,
    /// Every session message its sender logged, oldest first.
    pub messages: Vec<MessageFact>,
    /// Every session message its receiver logged, oldest first.
    pub received: Vec<ReceivedFact>,
    /// Every report, oldest first.
    pub reports: Vec<ReportFact>,
    /// Every task transition, oldest first.
    pub transitions: Vec<Change<TaskState>>,
    /// Every logged pull request state change, oldest first.
    pub pr_changes: Vec<Change<PullState>>,
    /// Every logged job state change, oldest first.
    pub job_changes: Vec<Change<JobState>>,
    /// Every logged review state change, oldest first.
    pub review_changes: Vec<Change<ReviewState>>,
    /// Every tick finding about a task.
    pub findings: Vec<FindingFact>,
    /// Every review finding.
    pub review_findings: Vec<ReviewFindingFact>,
    /// Every backlog item.
    pub items: Vec<ItemFact>,
    /// Every dependency between backlog items.
    pub item_deps: Vec<DepFact>,
    /// The tokens spent so far.
    pub spend: Spend,
    /// Events left out because they hold a word this build does not know, each named with why.
    pub skipped: Vec<String>,
}

/// Which tasks to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Tasks not over (closed, dropped) at the instant, and the lead of every part drawn; subagents only while they
    /// run.
    Live,
    /// Every task that existed at the instant.
    All,
    /// One task, with its parts and its lead; the orchestrator's own subagents and asks are left out.
    Task(i64),
}

text_enum!(
    /// What a node stands for.
    NodeKind, "node kind" {
        /// The user.
        User => "user",
        /// Every orchestrator session, as one node.
        Orchestrator => "orchestrator",
        /// A lead task, with the sessions it went through.
        Lead => "lead",
        /// A plain task, with the sessions it went through.
        Worker => "worker",
        /// A part task, with the sessions it went through; a `part-of` edge joins it to its lead.
        Part => "part",
        /// A subagent.
        Subagent => "subagent",
        /// A helper session.
        Helper => "helper",
        /// A review.
        Review => "review",
        /// One node run of a review's flow.
        ReviewNode => "review-node",
        /// A job.
        Job => "job",
        /// A pull request.
        Pr => "pr",
        /// An open ask to the user.
        Ask => "ask",
        /// A backlog item.
        Item => "item",
        /// A backlog epic.
        Epic => "epic",
        /// A message address headwork does not know.
        External => "external",
    }
);

text_enum!(
    /// What an edge means.
    EdgeKind, "edge kind" {
        /// Who made a top-level task: the orchestrator, or the user node for a CLI spawn.
        Spawned => "spawned",
        /// From a part to its lead.
        PartOf => "part-of",
        /// A task waits for another.
        DependsOn => "depends-on",
        /// A session or a parent subagent called a subagent.
        Called => "called",
        /// A session opened a helper or a job.
        Opened => "opened",
        /// Session messages, counted.
        Message => "message",
        /// Reports to the orchestrator, counted.
        Report => "report",
        /// Asks to the user, counted; an open one runs through its ask node.
        Asked => "asked",
        /// A review looks at a task.
        Reviews => "reviews",
        /// A review ran a node of its flow.
        Ran => "ran",
        /// A task delivers in a pull request.
        Delivered => "delivered",
        /// A backlog item waits for another.
        Depends => "depends",
        /// A task works on a backlog item.
        Tracks => "tracks",
    }
);

text_enum!(
    /// What a subagent is doing, as far as its transcript and meta file tell.
    AgentState, "subagent state" {
        /// Now, for one whose meta file does not say whether it finished.
        Seen => "seen",
        /// Between its first and last response, or now while its meta says it has not finished.
        Running => "running",
        /// After its last response, or now when its meta says it finished.
        Done => "done",
    }
);

/// A node's state, from the vocabulary of what the node stands for ([`Shape::state`]); written as its word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    /// A task.
    Task(TaskState),
    /// The orchestrator (`live` while one of its sessions is).
    Session(SessionState),
    /// A subagent.
    Agent(AgentState),
    /// A job.
    Job(JobState),
    /// A pull request.
    Pull(PullState),
    /// A review.
    Review(ReviewState),
    /// A review's flow node.
    ReviewNode(NodeState),
    /// An open ask.
    Ask(AskState),
    /// A backlog item or epic.
    Item(ItemStatus),
}

impl Status {
    /// The word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Task(state) => state.as_str(),
            Self::Session(state) => state.as_str(),
            Self::Agent(state) => state.as_str(),
            Self::Job(state) => state.as_str(),
            Self::Pull(state) => state.as_str(),
            Self::Review(state) => state.as_str(),
            Self::ReviewNode(state) => state.as_str(),
            Self::Ask(state) => state.as_str(),
            Self::Item(state) => state.as_str(),
        }
    }
}

impl Serialize for Status {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// What a node stands for, with the state its kind has: the type fixes which state goes with which kind. It is written
/// as the node's `kind` and, where there is one, its `state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shape {
    /// The user.
    User,
    /// Every orchestrator session: `live` while one of them is.
    Orchestrator(SessionState),
    /// A lead task.
    Lead(TaskState),
    /// A plain task.
    Worker(TaskState),
    /// A part task.
    Part(TaskState),
    /// A subagent.
    Subagent(AgentState),
    /// A helper session.
    Helper,
    /// A review.
    Review(ReviewState),
    /// One node run of a review's flow.
    ReviewNode(NodeState),
    /// A job.
    Job(JobState),
    /// A pull request.
    Pr(PullState),
    /// An open ask.
    Ask(AskState),
    /// A backlog item.
    Item(ItemStatus),
    /// A backlog epic.
    Epic(ItemStatus),
    /// A message address headwork does not know.
    External,
}

impl Shape {
    /// What the node stands for.
    #[must_use]
    pub const fn kind(self) -> NodeKind {
        match self {
            Self::User => NodeKind::User,
            Self::Orchestrator(_) => NodeKind::Orchestrator,
            Self::Lead(_) => NodeKind::Lead,
            Self::Worker(_) => NodeKind::Worker,
            Self::Part(_) => NodeKind::Part,
            Self::Subagent(_) => NodeKind::Subagent,
            Self::Helper => NodeKind::Helper,
            Self::Review(_) => NodeKind::Review,
            Self::ReviewNode(_) => NodeKind::ReviewNode,
            Self::Job(_) => NodeKind::Job,
            Self::Pr(_) => NodeKind::Pr,
            Self::Ask(_) => NodeKind::Ask,
            Self::Item(_) => NodeKind::Item,
            Self::Epic(_) => NodeKind::Epic,
            Self::External => NodeKind::External,
        }
    }

    /// Its state, drawn as colour; none for the user, a helper and an external address.
    #[must_use]
    pub const fn state(self) -> Option<Status> {
        match self {
            Self::User | Self::Helper | Self::External => None,
            Self::Orchestrator(state) => Some(Status::Session(state)),
            Self::Lead(state) | Self::Worker(state) | Self::Part(state) => {
                Some(Status::Task(state))
            }
            Self::Subagent(state) => Some(Status::Agent(state)),
            Self::Review(state) => Some(Status::Review(state)),
            Self::ReviewNode(state) => Some(Status::ReviewNode(state)),
            Self::Job(state) => Some(Status::Job(state)),
            Self::Pr(state) => Some(Status::Pull(state)),
            Self::Ask(state) => Some(Status::Ask(state)),
            Self::Item(state) | Self::Epic(state) => Some(Status::Item(state)),
        }
    }
}

impl Serialize for Shape {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let state = self.state();
        let mut map = serializer.serialize_map(Some(if state.is_some() { 2 } else { 1 }))?;
        map.serialize_entry("kind", &self.kind())?;
        if let Some(state) = state {
            map.serialize_entry("state", &state)?;
        }
        map.end()
    }
}

/// One node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Node {
    /// Unique across the graph, such as `task:12` or `agent:<id>`; never agent-written text.
    pub id: String,
    /// What it stands for and its state. A task has its task state; the orchestrator is `live` or `ended`.
    #[serde(flatten)]
    pub shape: Shape,
    /// The text drawn on it.
    pub label: String,
    /// When it appeared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    /// When it went away.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    /// More to show on click.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, String>,
    /// Tokens spent so far, in the view of now, for a task (its own), a session, the orchestrator and a subagent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    /// Counts drawn on the node: `silent` (1) on a task that closes without asking, open tick `findings` on a
    /// task, open `asks` on the user, review `findings` (and
    /// `open` ones) on a review and its flow nodes.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub badges: BTreeMap<String, u32>,
}

/// One edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Edge {
    /// Unique across the graph.
    pub id: String,
    /// What it means.
    pub kind: EdgeKind,
    /// From.
    pub source: String,
    /// To.
    pub target: String,
    /// How many times it happened, for counted kinds; 1 otherwise.
    pub weight: u32,
    /// The text drawn on it, such as the last report's status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// When it last happened.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

/// The graph at one instant.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Graph {
    /// The instant it shows; none for now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// Every node, in a stable order.
    pub nodes: Vec<Node>,
    /// Every edge, in a stable order.
    pub edges: Vec<Edge>,
    /// Events left out of the facts, each named with why.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
}

impl Node {
    /// What it stands for.
    #[must_use]
    pub const fn kind(&self) -> NodeKind {
        self.shape.kind()
    }

    /// Its state, if its kind has one.
    #[must_use]
    pub const fn state(&self) -> Option<Status> {
        self.shape.state()
    }
}

impl Graph {
    /// The node with this id.
    #[must_use]
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// The edges of this kind.
    pub fn edges_of(&self, kind: EdgeKind) -> impl Iterator<Item = &Edge> {
        self.edges.iter().filter(move |edge| edge.kind == kind)
    }
}

/// The id of a task's node.
#[must_use]
pub fn task_node(task: i64) -> String {
    format!("task:{task}")
}

/// The task named `name` that a view of `at` (now when none) means: the newest one queued by then; now, a live one
/// before a closed one. None when no task of that name had been queued by then.
#[must_use]
pub fn task_named<'f>(facts: &'f Facts, name: &str, at: Option<&str>) -> Option<&'f TaskFact> {
    facts
        .tasks
        .iter()
        .filter(|task| task.name == name && at.is_none_or(|at| task.queued_at.as_str() <= at))
        .max_by_key(|task| (at.is_none() && task.state != TaskState::Closed, task.id))
}

/// The state of row `key` at `at` from its logged changes: the newest at or before it, else the state before the
/// first one after it (`first` when the log does not say), else `now`.
pub(crate) fn change_at<S: Copy>(changes: &[&Change<S>], at: &str, first: S, now: S) -> S {
    let before = changes
        .iter()
        .filter(|change| change.at.as_str() <= at)
        .max_by(|left, right| left.at.cmp(&right.at));
    let after = changes
        .iter()
        .filter(|change| change.at.as_str() > at)
        .min_by(|left, right| left.at.cmp(&right.at));
    before.map_or_else(
        || after.map_or(now, |change| change.from.unwrap_or(first)),
        |change| change.to,
    )
}
