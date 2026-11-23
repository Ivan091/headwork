//! The agent graph (`docs/dashboard.md`, sections 3 to 6): the store's facts in, nodes and edges out.
//!
//! [`build`] draws the graph as it stood at one instant: every row whose start lies at or before it and whose end, if
//! any, lies after it, with each task's state taken from its transitions ([`state_at`]). Times are the store's RFC 3339
//! UTC text, which sorts as it reads. Nothing here reads a file or a database; the store gathers the [`Facts`].
//!
//! Past states come from the store's logs: task `transition`, `pr-state`, `job-state` and `review-<trigger>` events
//! ([`Change`]), and a review node's start and finish times.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::names::{bare_address, is_orchestrator_address};
use crate::review::Level;
use crate::vocabulary::{
    Actor, AskState, Effort, JobState, Model, NodeState, PullState, ReviewState, Role,
    SessionState, TaskState,
};

/// The id of the one node that stands for the user.
pub const USER_NODE: &str = "user";
/// The id of the one node that stands for every orchestrator session.
pub const ORCHESTRATOR_NODE: &str = "orchestrator";

/// A task as the graph needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskFact {
    /// The store's id.
    pub id: i64,
    /// `<project>-<slug>`.
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
    /// Who created it (the `task-created` event's actor): the user from the CLI, else the orchestrator.
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
    /// When it started.
    pub started_at: String,
    /// When it ended.
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
    /// The session that asked.
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
    /// The address as sent.
    pub to: String,
    /// The sender's short summary, never the body.
    pub summary: Option<String>,
    /// When it was sent.
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

/// A task's state change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionFact {
    /// The task.
    pub task: i64,
    /// The state before.
    pub from: TaskState,
    /// The state after.
    pub to: TaskState,
    /// When.
    pub at: String,
}

/// A logged state change of a pull request, job or review, keyed by its row id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change<S> {
    /// The row: a pull request, job or review id.
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
    /// Every session message, oldest first.
    pub messages: Vec<MessageFact>,
    /// Every report, oldest first.
    pub reports: Vec<ReportFact>,
    /// Every task transition, oldest first.
    pub transitions: Vec<TransitionFact>,
    /// Every logged pull request state change (`pr-state`), oldest first.
    pub pr_changes: Vec<Change<PullState>>,
    /// Every logged job state change (`job-state`), oldest first.
    pub job_changes: Vec<Change<JobState>>,
    /// Every logged review state change (`review-<trigger>`), oldest first.
    pub review_changes: Vec<Change<ReviewState>>,
}

/// Which tasks to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Tasks not over (closed, dropped) at the instant, and the lead of every part drawn.
    Live,
    /// Every task that existed at the instant.
    All,
    /// One task, with its parts and its lead; the orchestrator's own subagents and asks are left out.
    Task(i64),
}

macro_rules! kinds {
    ($(#[$doc:meta])* $name:ident { $($(#[$vdoc:meta])* $variant:ident => $text:literal,)+ }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        pub enum $name {
            $(
                $(#[$vdoc])*
                #[serde(rename = $text)]
                $variant,
            )+
        }

        impl $name {
            /// Every kind, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The name it is written as, in JSON and in text.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)+
                }
            }
        }
    };
}

kinds!(
    /// What a node stands for.
    NodeKind {
        /// The user.
        User => "user",
        /// Every orchestrator session, as one node.
        Orchestrator => "orchestrator",
        /// A task: a box around its sessions, parts and satellites.
        Task => "task",
        /// A lead task's session.
        Lead => "lead",
        /// A plain task's session.
        Worker => "worker",
        /// A part task's session.
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
        /// A message address headwork does not know.
        External => "external",
    }
);

kinds!(
    /// What an edge means.
    EdgeKind {
        /// The orchestrator (or the user, from the CLI) made the task.
        Spawned => "spawned",
        /// A part belongs to its lead's task (also drawn as nesting).
        PartOf => "part-of",
        /// A task waits for another.
        DependsOn => "depends-on",
        /// A session called a subagent.
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
    }
);

/// What a subagent is doing, as far as its transcript tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    /// Now: it has responded; its transcript does not say whether it finished.
    Seen,
    /// At a past instant: between its first and last response.
    Running,
    /// At a past instant: after its last response.
    Done,
}

impl AgentState {
    /// The word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Seen => "seen",
            Self::Running => "running",
            Self::Done => "done",
        }
    }
}

/// A node's state, from the vocabulary of what the node stands for; written as its word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    /// A task.
    Task(TaskState),
    /// A session, or the orchestrator (`live` while one of its sessions is).
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
        }
    }
}

impl Serialize for Status {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// One node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Node {
    /// Unique across the graph, such as `task:12` or `session:<id>`.
    pub id: String,
    /// What it stands for.
    pub kind: NodeKind,
    /// The text drawn on it.
    pub label: String,
    /// Its state, drawn as colour: one word of the node kind's own vocabulary ([`Status`]). At a past instant a
    /// session is `live` or `ended`; now it is its stored state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<Status>,
    /// The box it is drawn in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// When it appeared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    /// When it went away.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    /// More to show on click.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, String>,
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

/// The id of a session's node.
#[must_use]
pub fn session_node(session: &str) -> String {
    format!("session:{session}")
}

fn agent_node(agent: &str) -> String {
    format!("agent:{agent}")
}

/// The id of the node of a message address headwork does not know: `external:` and a hash of the address, so the
/// address, written by an agent, is only ever a label and never part of an id.
#[must_use]
pub fn external_node(address: &str) -> String {
    let hash = address
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    format!("external:{hash:016x}")
}

/// The state of row `key` at `at` from its logged changes: the newest at or before it, else the state before the
/// first one after it (`first` when the log does not say), else `now`.
#[must_use]
pub fn change_at<S: Copy>(changes: &[Change<S>], key: i64, at: &str, first: S, now: S) -> S {
    let mine = changes.iter().filter(|change| change.key == key);
    let before = mine
        .clone()
        .filter(|change| change.at.as_str() <= at)
        .max_by(|left, right| left.at.cmp(&right.at));
    let after = mine
        .filter(|change| change.at.as_str() > at)
        .min_by(|left, right| left.at.cmp(&right.at));
    before.map_or_else(
        || after.map_or(now, |change| change.from.unwrap_or(first)),
        |change| change.to,
    )
}

/// The state of `task` at `at`: the newest transition at or before it, else the state before its first transition
/// after it, else `now`.
#[must_use]
pub fn state_at(transitions: &[TransitionFact], task: i64, at: &str, now: TaskState) -> TaskState {
    let mut before = None;
    let mut after = None;
    for transition in transitions
        .iter()
        .filter(|transition| transition.task == task)
    {
        if transition.at.as_str() <= at {
            if before.is_none_or(|seen: &TransitionFact| seen.at <= transition.at) {
                before = Some(transition);
            }
        } else if after.is_none_or(|seen: &TransitionFact| seen.at > transition.at) {
            after = Some(transition);
        }
    }
    before.map_or_else(
        || after.map_or(now, |transition| transition.from),
        |transition| transition.to,
    )
}

const fn is_over(state: TaskState) -> bool {
    matches!(state, TaskState::Closed | TaskState::Dropped)
}

/// Whether a row that started at `start` and ended at `end` is there at `at`; now (no `at`), while it has no end.
fn alive(start: &str, end: Option<&str>, at: Option<&str>) -> bool {
    at.map_or_else(
        || end.is_none(),
        |at| start <= at && end.is_none_or(|end| end > at),
    )
}

fn task_end(transitions: &[TransitionFact], task: i64) -> Option<String> {
    transitions
        .iter()
        .filter(|transition| transition.task == task && is_over(transition.to))
        .map(|transition| transition.at.clone())
        .max()
}

fn details<const N: usize>(pairs: [(&str, Option<String>); N]) -> BTreeMap<String, String> {
    pairs
        .into_iter()
        .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value)))
        .collect()
}

/// Where an address or a session points.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Place {
    /// At a node drawn in this graph.
    Drawn(String),
    /// At something headwork knows that this graph does not draw (another scope, another instant).
    Hidden,
}

/// The nodes and edges drawn so far; repeated edges add to one edge's weight.
#[derive(Default)]
struct Canvas {
    nodes: BTreeMap<String, Node>,
    edges: BTreeMap<(EdgeKind, String, String), Edge>,
}

impl Canvas {
    fn put(&mut self, node: Node) {
        self.nodes.insert(node.id.clone(), node);
    }

    fn link(
        &mut self,
        kind: EdgeKind,
        source: &str,
        target: &str,
        label: Option<String>,
        at: Option<&str>,
    ) {
        let key = (kind, source.to_owned(), target.to_owned());
        let edge = self.edges.entry(key).or_insert_with(|| Edge {
            id: format!("{}:{source}->{target}", kind.as_str()),
            kind,
            source: source.to_owned(),
            target: target.to_owned(),
            weight: 0,
            label: None,
            at: None,
        });
        edge.weight = edge.weight.saturating_add(1);
        let newer = match (at, edge.at.as_deref()) {
            (Some(at), Some(seen)) => at >= seen,
            _ => true,
        };
        if newer {
            if label.is_some() {
                edge.label = label;
            }
            if let Some(at) = at {
                edge.at = Some(at.to_owned());
            }
        }
    }

    fn finish(self, at: Option<&str>) -> Graph {
        let nodes = self.nodes;
        let edges = self
            .edges
            .into_values()
            .filter(|edge| nodes.contains_key(&edge.source) && nodes.contains_key(&edge.target))
            .collect();
        Graph {
            at: at.map(str::to_owned),
            nodes: nodes.into_values().collect(),
            edges,
        }
    }
}

/// One build: the facts, the instant, the scope, the tasks drawn and where sessions and addresses point.
struct Draw<'f> {
    facts: &'f Facts,
    at: Option<&'f str>,
    scope: Scope,
    drawn: BTreeMap<i64, TaskState>,
    sessions: BTreeMap<&'f str, Place>,
    addresses: BTreeMap<&'f str, Place>,
    heads: BTreeMap<i64, String>,
}

impl<'f> Draw<'f> {
    fn new(facts: &'f Facts, at: Option<&'f str>, scope: Scope) -> Self {
        let mut draw = Self {
            facts,
            at,
            scope,
            drawn: drawn_tasks(facts, at, scope),
            sessions: BTreeMap::new(),
            addresses: BTreeMap::new(),
            heads: BTreeMap::new(),
        };
        draw.place();
        draw
    }

    fn until(&self, time: &str) -> bool {
        self.at.is_none_or(|at| time <= at)
    }

    fn is_drawn(&self, task: i64) -> bool {
        self.drawn.contains_key(&task)
    }

    fn task_box(&self, task: Option<i64>) -> Option<String> {
        task.filter(|task| self.is_drawn(*task)).map(task_node)
    }

    /// Where every session, task name, orchestrator name and subagent id points.
    fn place(&mut self) {
        let facts = self.facts;
        for task in &facts.tasks {
            let place = if self.is_drawn(task.id) {
                Place::Drawn(task_node(task.id))
            } else {
                Place::Hidden
            };
            if let Place::Drawn(id) = &place {
                self.heads.insert(task.id, id.clone());
            }
            self.addresses.insert(&task.name, place);
        }
        for session in &facts.sessions {
            if session.role == Role::Orchestrator {
                self.sessions
                    .insert(&session.id, Place::Drawn(ORCHESTRATOR_NODE.to_owned()));
                self.addresses
                    .insert(&session.rc_name, Place::Drawn(ORCHESTRATOR_NODE.to_owned()));
                continue;
            }
            let head = session
                .task
                .filter(|task| self.is_drawn(*task))
                .and_then(|task| newest_session(facts, task, self.at))
                .map(|newest| session_node(&newest.id));
            let place = head.map_or(Place::Hidden, Place::Drawn);
            if let (Some(task), Place::Drawn(id)) = (session.task, &place) {
                self.heads.insert(task, id.clone());
            }
            self.sessions.insert(&session.id, place.clone());
            self.addresses.insert(&session.rc_name, place);
        }
        for agent in &facts.agents {
            self.addresses
                .insert(&agent.id, Place::Drawn(agent_node(&agent.id)));
        }
    }

    fn of_session(&self, session: &str) -> Option<&String> {
        match self.sessions.get(session) {
            Some(Place::Drawn(id)) => Some(id),
            _ => None,
        }
    }

    fn of_address(&self, address: &str) -> Option<Place> {
        let bare = bare_address(address);
        if is_orchestrator_address(bare) {
            return Some(Place::Drawn(ORCHESTRATOR_NODE.to_owned()));
        }
        self.addresses.get(bare).cloned()
    }

    fn caller_task(&self, session: &str) -> Option<i64> {
        self.facts
            .sessions
            .iter()
            .find(|fact| fact.id == session)
            .and_then(|fact| fact.task)
    }
}

fn newest_session<'f>(facts: &'f Facts, task: i64, at: Option<&str>) -> Option<&'f SessionFact> {
    facts
        .sessions
        .iter()
        .filter(|session| session.task == Some(task) && session.role == Role::Worker)
        .filter(|session| at.is_none_or(|at| session.started_at.as_str() <= at))
        .max_by(|left, right| left.started_at.cmp(&right.started_at))
}

/// The graph at `at` (now when none) for `scope`.
#[must_use]
pub fn build(facts: &Facts, at: Option<&str>, scope: Scope) -> Graph {
    let draw = Draw::new(facts, at, scope);
    let mut canvas = Canvas::default();
    canvas.put(Node {
        id: USER_NODE.to_owned(),
        kind: NodeKind::User,
        label: "you".to_owned(),
        state: None,
        parent: None,
        start: None,
        end: None,
        details: BTreeMap::new(),
    });
    orchestrator(&draw, &mut canvas);
    for task in &facts.tasks {
        if let Some(state) = draw.drawn.get(&task.id) {
            task_box(&draw, task, *state, &mut canvas);
            task_session(&draw, task, &mut canvas);
        }
    }
    subagents(&draw, &mut canvas);
    helpers(&draw, &mut canvas);
    jobs(&draw, &mut canvas);
    prs(&draw, &mut canvas);
    reviews(&draw, &mut canvas);
    messages(&draw, &mut canvas);
    reports(&draw, &mut canvas);
    asks(&draw, &mut canvas);
    canvas.finish(at)
}

fn drawn_tasks(facts: &Facts, at: Option<&str>, scope: Scope) -> BTreeMap<i64, TaskState> {
    let existing: BTreeMap<i64, TaskState> = facts
        .tasks
        .iter()
        .filter(|task| at.is_none_or(|at| task.queued_at.as_str() <= at))
        .map(|task| {
            let state = at.map_or(task.state, |at| {
                state_at(&facts.transitions, task.id, at, task.state)
            });
            (task.id, state)
        })
        .collect();
    let parent = |id: i64| {
        facts
            .tasks
            .iter()
            .find(|task| task.id == id)
            .and_then(|task| task.parent)
    };
    match scope {
        Scope::All => existing,
        Scope::Live => {
            let live: BTreeSet<i64> = existing
                .iter()
                .filter(|(_, state)| !is_over(**state))
                .map(|(id, _)| *id)
                .collect();
            let leads: BTreeSet<i64> = live.iter().filter_map(|id| parent(*id)).collect();
            existing
                .into_iter()
                .filter(|(id, _)| live.contains(id) || leads.contains(id))
                .collect()
        }
        Scope::Task(id) => {
            let lead = parent(id).unwrap_or(id);
            let in_family = |task: i64| parent(task).unwrap_or(task) == lead;
            existing
                .into_iter()
                .filter(|(task, _)| in_family(*task))
                .collect()
        }
    }
}

fn orchestrator(draw: &Draw<'_>, canvas: &mut Canvas) {
    let sessions: Vec<&SessionFact> = draw
        .facts
        .sessions
        .iter()
        .filter(|session| session.role == Role::Orchestrator)
        .collect();
    let live: Vec<&SessionFact> = sessions
        .iter()
        .copied()
        .filter(|session| alive(&session.started_at, session.ended_at.as_deref(), draw.at))
        .collect();
    let newest = live
        .iter()
        .copied()
        .max_by(|left, right| left.started_at.cmp(&right.started_at))
        .or_else(|| {
            sessions
                .iter()
                .copied()
                .max_by(|left, right| left.started_at.cmp(&right.started_at))
        });
    let listed = sessions
        .iter()
        .map(|session| session.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    canvas.put(Node {
        id: ORCHESTRATOR_NODE.to_owned(),
        kind: NodeKind::Orchestrator,
        label: newest.map_or_else(
            || ORCHESTRATOR_NODE.to_owned(),
            |session| session.rc_name.clone(),
        ),
        state: Some(Status::Session(if live.is_empty() {
            SessionState::Ended
        } else {
            SessionState::Live
        })),
        parent: None,
        start: sessions
            .iter()
            .map(|session| session.started_at.clone())
            .min(),
        end: None,
        details: details([("sessions", (!listed.is_empty()).then_some(listed))]),
    });
}

fn task_box(draw: &Draw<'_>, task: &TaskFact, state: TaskState, canvas: &mut Canvas) {
    let id = task_node(task.id);
    canvas.put(Node {
        id: id.clone(),
        kind: NodeKind::Task,
        label: task.name.clone(),
        state: Some(Status::Task(state)),
        parent: draw.task_box(task.parent),
        start: Some(task.queued_at.clone()),
        end: task_end(&draw.facts.transitions, task.id),
        details: details([
            ("project", Some(task.project.clone())),
            ("kind", Some(task.kind.clone())),
            ("model", Some(task.model.as_str().to_owned())),
            (
                "effort",
                task.effort.map(|effort| effort.as_str().to_owned()),
            ),
            ("last_report", task.last_report.clone()),
        ]),
    });
    if let Some(lead) = task.parent {
        canvas.link(
            EdgeKind::PartOf,
            &id,
            &task_node(lead),
            None,
            Some(&task.queued_at),
        );
    } else {
        let maker = if task.created_by == Actor::User {
            USER_NODE
        } else {
            ORCHESTRATOR_NODE
        };
        canvas.link(EdgeKind::Spawned, maker, &id, None, Some(&task.queued_at));
    }
    if let Some(other) = task.depends_on {
        canvas.link(EdgeKind::DependsOn, &id, &task_node(other), None, None);
    }
}

fn task_session(draw: &Draw<'_>, task: &TaskFact, canvas: &mut Canvas) {
    let Some(newest) = newest_session(draw.facts, task.id, draw.at) else {
        return;
    };
    let kind = if task.parent.is_some() {
        NodeKind::Part
    } else if task.lead {
        NodeKind::Lead
    } else {
        NodeKind::Worker
    };
    let state = match draw.at {
        None => newest.state,
        Some(_) if alive(&newest.started_at, newest.ended_at.as_deref(), draw.at) => {
            SessionState::Live
        }
        Some(_) => SessionState::Ended,
    };
    let label = task.effort.map_or_else(
        || task.model.as_str().to_owned(),
        |effort| format!("{} {}", task.model.as_str(), effort.as_str()),
    );
    let earlier: Vec<&str> = draw
        .facts
        .sessions
        .iter()
        .filter(|session| {
            session.task == Some(task.id) && session.role == Role::Worker && session.id != newest.id
        })
        .filter(|session| draw.until(&session.started_at))
        .map(|session| session.id.as_str())
        .collect();
    canvas.put(Node {
        id: session_node(&newest.id),
        kind,
        label,
        state: Some(Status::Session(state)),
        parent: Some(task_node(task.id)),
        start: Some(newest.started_at.clone()),
        end: newest.ended_at.clone(),
        details: details([
            ("session", Some(newest.id.clone())),
            ("name", Some(newest.rc_name.clone())),
            (
                "earlier_sessions",
                (!earlier.is_empty()).then(|| earlier.join(", ")),
            ),
        ]),
    });
}

/// Where a subagent is drawn and in what state, or none when this view leaves it out: its caller is not drawn, it is
/// the orchestrator's in a single-task view, or it had not responded yet at the instant (or has no times at all).
fn agent_shown<'d>(
    draw: &'d Draw<'_>,
    agent: &AgentFact,
) -> Option<(&'d String, Option<String>, AgentState)> {
    let caller = draw.of_session(&agent.session)?;
    let home = draw.task_box(draw.caller_task(&agent.session));
    if matches!(draw.scope, Scope::Task(_)) && home.is_none() {
        return None;
    }
    let state = match (draw.at, agent.first_at.as_deref(), agent.last_at.as_deref()) {
        (None, _, _) => AgentState::Seen,
        (Some(_), None, _) => return None,
        (Some(at), Some(first), _) if first > at => return None,
        (Some(at), Some(_), Some(last)) if last <= at => AgentState::Done,
        (Some(_), Some(_), _) => AgentState::Running,
    };
    Some((caller, home, state))
}

fn subagents(draw: &Draw<'_>, canvas: &mut Canvas) {
    for agent in &draw.facts.agents {
        let Some((caller, home, state)) = agent_shown(draw, agent) else {
            continue;
        };
        let id = agent_node(&agent.id);
        canvas.put(Node {
            id: id.clone(),
            kind: NodeKind::Subagent,
            label: agent
                .agent_type
                .clone()
                .unwrap_or_else(|| "subagent".to_owned()),
            state: Some(Status::Agent(state)),
            parent: home,
            start: agent.first_at.clone(),
            end: agent.last_at.clone(),
            details: details([
                ("description", agent.description.clone()),
                ("model", agent.model.clone()),
                ("depth", agent.depth.map(|depth| depth.to_string())),
            ]),
        });
        let starter = agent
            .parent
            .as_deref()
            .and_then(|parent| draw.facts.agents.iter().find(|other| other.id == parent))
            .filter(|parent| agent_shown(draw, parent).is_some())
            .map_or_else(|| caller.clone(), |parent| agent_node(&parent.id));
        canvas.link(
            EdgeKind::Called,
            &starter,
            &id,
            None,
            agent.first_at.as_deref(),
        );
    }
}

fn helpers(draw: &Draw<'_>, canvas: &mut Canvas) {
    for helper in &draw.facts.helpers {
        if !draw.is_drawn(helper.task) || !draw.until(&helper.at) {
            continue;
        }
        let id = format!("helper:{}", helper.session);
        canvas.put(Node {
            id: id.clone(),
            kind: NodeKind::Helper,
            label: "headwork:helper".to_owned(),
            state: None,
            parent: Some(task_node(helper.task)),
            start: Some(helper.at.clone()),
            end: None,
            details: details([("session", Some(helper.session.clone()))]),
        });
        if let Some(head) = draw.heads.get(&helper.task) {
            canvas.link(EdgeKind::Opened, head, &id, None, Some(&helper.at));
        }
    }
}

fn jobs(draw: &Draw<'_>, canvas: &mut Canvas) {
    for job in &draw.facts.jobs {
        let Some(task) = job.task.filter(|task| draw.is_drawn(*task)) else {
            continue;
        };
        if !draw.until(&job.started_at) {
            continue;
        }
        let state = draw.at.map_or(job.state, |at| {
            let unlogged = if job.ended_at.as_deref().is_some_and(|end| end > at) {
                JobState::Running
            } else {
                job.state
            };
            change_at(
                &draw.facts.job_changes,
                job.id,
                at,
                JobState::Running,
                unlogged,
            )
        });
        let id = format!("job:{}", job.id);
        canvas.put(Node {
            id: id.clone(),
            kind: NodeKind::Job,
            label: job.label.clone(),
            state: Some(Status::Job(state)),
            parent: Some(task_node(task)),
            start: Some(job.started_at.clone()),
            end: job.ended_at.clone(),
            details: BTreeMap::new(),
        });
        let opener = job
            .session
            .as_deref()
            .and_then(|session| draw.of_session(session))
            .or_else(|| draw.heads.get(&task));
        if let Some(opener) = opener {
            canvas.link(EdgeKind::Opened, opener, &id, None, Some(&job.started_at));
        }
    }
}

fn prs(draw: &Draw<'_>, canvas: &mut Canvas) {
    for pr in &draw.facts.prs {
        if !draw.is_drawn(pr.task) || !draw.until(&pr.first_at) {
            continue;
        }
        let id = format!("pr:{}", pr.id);
        canvas.put(Node {
            id: id.clone(),
            kind: NodeKind::Pr,
            label: format!("#{}", pr.number),
            state: Some(Status::Pull(draw.at.map_or(pr.state, |at| {
                change_at(&draw.facts.pr_changes, pr.id, at, PullState::Open, pr.state)
            }))),
            parent: Some(task_node(pr.task)),
            start: Some(pr.first_at.clone()),
            end: None,
            details: details([("url", Some(pr.url.clone()))]),
        });
        canvas.link(
            EdgeKind::Delivered,
            &task_node(pr.task),
            &id,
            None,
            Some(&pr.first_at),
        );
    }
}

fn reviews(draw: &Draw<'_>, canvas: &mut Canvas) {
    for review in &draw.facts.reviews {
        if !draw.is_drawn(review.task) || !draw.until(&review.requested_at) {
            continue;
        }
        let id = format!("review:{}", review.id);
        let state = draw.at.map_or(review.state, |at| {
            change_at(
                &draw.facts.review_changes,
                review.id,
                at,
                ReviewState::Requested,
                review.state,
            )
        });
        let home = task_node(
            review
                .part
                .filter(|part| draw.is_drawn(*part))
                .unwrap_or(review.task),
        );
        canvas.put(Node {
            id: id.clone(),
            kind: NodeKind::Review,
            label: format!("{} {}", review.flow, review.level.number()),
            state: Some(Status::Review(state)),
            parent: Some(home.clone()),
            start: Some(review.requested_at.clone()),
            end: review.finished_at.clone(),
            details: BTreeMap::new(),
        });
        let reviewed = draw
            .heads
            .get(&review.task)
            .cloned()
            .unwrap_or_else(|| task_node(review.task));
        canvas.link(
            EdgeKind::Reviews,
            &id,
            &reviewed,
            None,
            Some(&review.requested_at),
        );
        for run in draw
            .facts
            .review_nodes
            .iter()
            .filter(|run| run.review == review.id)
        {
            if !draw.until(run.started_at.as_deref().unwrap_or(&review.requested_at)) {
                continue;
            }
            let node_id = format!(
                "review-node:{}:{}:{}:{}",
                review.id, run.round, run.node, run.item
            );
            let state = match (draw.at, run.finished_at.as_deref()) {
                (Some(at), Some(end)) if end > at => NodeState::Running,
                _ => run.state,
            };
            canvas.put(Node {
                id: node_id.clone(),
                kind: NodeKind::ReviewNode,
                label: if run.item == 0 {
                    run.node.clone()
                } else {
                    format!("{} {}", run.node, run.item)
                },
                state: Some(Status::ReviewNode(state)),
                parent: Some(home.clone()),
                start: run.started_at.clone(),
                end: run.finished_at.clone(),
                details: details([("round", Some(run.round.to_string()))]),
            });
            canvas.link(
                EdgeKind::Ran,
                &id,
                &node_id,
                None,
                run.started_at.as_deref(),
            );
        }
    }
}

fn messages(draw: &Draw<'_>, canvas: &mut Canvas) {
    for message in draw
        .facts
        .messages
        .iter()
        .filter(|message| draw.until(&message.at))
    {
        let Some(from) = draw.of_session(&message.from) else {
            continue;
        };
        let target = match draw.of_address(&message.to) {
            Some(Place::Drawn(id)) => id,
            Some(Place::Hidden) => continue,
            None => {
                let id = external_node(&message.to);
                if !canvas.nodes.contains_key(&id) {
                    canvas.put(Node {
                        id: id.clone(),
                        kind: NodeKind::External,
                        label: message.to.clone(),
                        state: None,
                        parent: None,
                        start: Some(message.at.clone()),
                        end: None,
                        details: BTreeMap::new(),
                    });
                }
                id
            }
        };
        canvas.link(
            EdgeKind::Message,
            from,
            &target,
            message.summary.clone(),
            Some(&message.at),
        );
    }
}

fn reports(draw: &Draw<'_>, canvas: &mut Canvas) {
    for report in draw
        .facts
        .reports
        .iter()
        .filter(|report| draw.until(&report.at))
    {
        if let Some(from) = draw.of_session(&report.session) {
            canvas.link(
                EdgeKind::Report,
                from,
                ORCHESTRATOR_NODE,
                Some(report.status.clone()),
                Some(&report.at),
            );
        }
    }
}

/// Whether an ask was open at `at` (now when none). An ask that left `pending` and `open` without a closing time is
/// taken as closed when it was made.
fn ask_open(ask: &AskFact, at: Option<&str>) -> bool {
    match at {
        None => ask.state.is_waiting(),
        Some(_) if ask.state.is_waiting() => true,
        Some(at) => ask.closed_at.as_deref().unwrap_or(&ask.created_at) > at,
    }
}

/// The longest label an ask without a header takes from its question, in characters.
const ASK_LABEL_CHARS: usize = 40;

fn asks(draw: &Draw<'_>, canvas: &mut Canvas) {
    for ask in draw
        .facts
        .asks
        .iter()
        .filter(|ask| draw.until(&ask.created_at))
    {
        let asker = ask
            .session
            .as_deref()
            .and_then(|session| draw.of_session(session).cloned())
            .or_else(|| ask.task.and_then(|task| draw.heads.get(&task).cloned()));
        let Some(asker) = asker else {
            continue;
        };
        if matches!(draw.scope, Scope::Task(_)) && asker == ORCHESTRATOR_NODE {
            continue;
        }
        let label = ask
            .header
            .clone()
            .unwrap_or_else(|| ask.question.chars().take(ASK_LABEL_CHARS).collect());
        if ask_open(ask, draw.at) {
            let id = format!("ask:{}", ask.id);
            canvas.put(Node {
                id: id.clone(),
                kind: NodeKind::Ask,
                label: label.clone(),
                state: Some(Status::Ask(AskState::Open)),
                parent: None,
                start: Some(ask.created_at.clone()),
                end: ask.closed_at.clone(),
                details: details([("question", Some(ask.question.clone()))]),
            });
            canvas.link(
                EdgeKind::Asked,
                &asker,
                &id,
                Some(label.clone()),
                Some(&ask.created_at),
            );
            canvas.link(
                EdgeKind::Asked,
                &id,
                USER_NODE,
                Some(label),
                Some(&ask.created_at),
            );
        } else {
            canvas.link(
                EdgeKind::Asked,
                &asker,
                USER_NODE,
                Some(label),
                Some(&ask.created_at),
            );
        }
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests fail loudly on a bad fixture")]
mod tests {
    use super::*;

    fn task(
        id: i64,
        name: &str,
        state: TaskState,
        parent: Option<i64>,
        lead: bool,
        at: &str,
    ) -> TaskFact {
        TaskFact {
            id,
            name: name.to_owned(),
            project: "headwork".to_owned(),
            kind: "feature".to_owned(),
            state,
            model: Model::Opus,
            effort: Some(Effort::Medium),
            parent,
            depends_on: None,
            lead,
            created_by: Actor::Orchestrator,
            queued_at: at.to_owned(),
            last_report: None,
        }
    }

    fn session(
        id: &str,
        task: Option<i64>,
        role: Role,
        name: &str,
        started: &str,
        ended: Option<&str>,
    ) -> SessionFact {
        SessionFact {
            id: id.to_owned(),
            task,
            role,
            rc_name: name.to_owned(),
            state: if ended.is_some() {
                SessionState::Ended
            } else {
                SessionState::Live
            },
            started_at: started.to_owned(),
            ended_at: ended.map(str::to_owned),
        }
    }

    fn transition(task: i64, from: TaskState, to: TaskState, at: &str) -> TransitionFact {
        TransitionFact {
            task,
            from,
            to,
            at: at.to_owned(),
        }
    }

    fn message(from: &str, to: &str, summary: Option<&str>, at: &str) -> MessageFact {
        MessageFact {
            from: from.to_owned(),
            to: to.to_owned(),
            summary: summary.map(str::to_owned),
            at: at.to_owned(),
        }
    }

    fn report(status: &str, at: &str) -> ReportFact {
        ReportFact {
            session: "s1".to_owned(),
            status: status.to_owned(),
            at: at.to_owned(),
        }
    }

    fn ask(
        id: i64,
        task: i64,
        session: &str,
        header: &str,
        created: &str,
        closed: Option<&str>,
    ) -> AskFact {
        AskFact {
            id,
            task: Some(task),
            session: Some(session.to_owned()),
            header: Some(header.to_owned()),
            question: "Which?".to_owned(),
            state: if closed.is_some() {
                AskState::Answered
            } else {
                AskState::Open
            },
            created_at: created.to_owned(),
            closed_at: closed.map(str::to_owned),
        }
    }

    fn review() -> ReviewFact {
        ReviewFact {
            id: 1,
            task: 1,
            part: Some(2),
            flow: "code".to_owned(),
            level: Level::Strict,
            state: ReviewState::Running,
            requested_at: "T03".to_owned(),
            finished_at: None,
        }
    }

    /// A lead (1) with a part (2), a closed plain task (3), the orchestrator, and one of everything else.
    fn facts() -> Facts {
        Facts {
            tasks: vec![
                task(1, "headwork-lead", TaskState::Working, None, true, "T01"),
                task(
                    2,
                    "headwork-lead-review",
                    TaskState::Working,
                    Some(1),
                    false,
                    "T03",
                ),
                task(3, "headwork-old", TaskState::Closed, None, false, "T00"),
            ],
            sessions: vec![
                session("o1", None, Role::Orchestrator, "orchestrator", "T00", None),
                session("s1", Some(1), Role::Worker, "headwork-lead", "T02", None),
                session(
                    "s2",
                    Some(2),
                    Role::Worker,
                    "headwork-lead-review",
                    "T04",
                    None,
                ),
                session(
                    "s3",
                    Some(3),
                    Role::Worker,
                    "headwork-old",
                    "T00",
                    Some("T05"),
                ),
            ],
            agents: vec![AgentFact {
                id: "a1".to_owned(),
                session: "s1".to_owned(),
                agent_type: Some("Explore".to_owned()),
                description: Some("find the hook".to_owned()),
                model: Some("haiku".to_owned()),
                depth: Some(1),
                parent: None,
                first_at: Some("T05".to_owned()),
                last_at: Some("T06".to_owned()),
            }],
            helpers: vec![HelperFact {
                session: "h1".to_owned(),
                task: 1,
                at: "T05".to_owned(),
            }],
            jobs: vec![JobFact {
                id: 7,
                task: Some(1),
                session: Some("s1".to_owned()),
                label: "render".to_owned(),
                state: JobState::Done,
                started_at: "T05".to_owned(),
                ended_at: Some("T08".to_owned()),
            }],
            prs: vec![PrFact {
                id: 9,
                task: 1,
                number: 41,
                url: "https://github.com/o/r/pull/41".to_owned(),
                state: PullState::Open,
                first_at: "T06".to_owned(),
            }],
            asks: vec![
                ask(4, 1, "s1", "Scope", "T02", Some("T03")),
                ask(5, 2, "s2", "Dispute", "T07", None),
            ],
            reviews: vec![review()],
            review_nodes: vec![ReviewNodeFact {
                review: 1,
                round: 1,
                node: "correctness".to_owned(),
                item: 0,
                state: NodeState::Done,
                started_at: Some("T05".to_owned()),
                finished_at: Some("T06".to_owned()),
            }],
            messages: vec![
                message("s2", "headwork-lead", Some("first"), "T05"),
                message("s2", "headwork-lead [ab12]", Some("second"), "T06"),
                message("s1", "root", None, "T06"),
                message("s1", "stranger", None, "T06"),
            ],
            reports: vec![report("progress", "T02"), report("review", "T06")],
            transitions: vec![
                transition(1, TaskState::Queued, TaskState::Working, "T02"),
                transition(3, TaskState::Working, TaskState::Closed, "T05"),
            ],
            pr_changes: Vec::new(),
            job_changes: Vec::new(),
            review_changes: Vec::new(),
        }
    }

    fn edge<'g>(graph: &'g Graph, kind: EdgeKind, source: &str, target: &str) -> Option<&'g Edge> {
        graph
            .edges_of(kind)
            .find(|edge| edge.source == source && edge.target == target)
    }

    #[test]
    fn every_node_kind_is_drawn_now() {
        let stranger = external_node("stranger");
        let graph = build(&facts(), None, Scope::Live);
        let table = [
            ("user", NodeKind::User),
            ("orchestrator", NodeKind::Orchestrator),
            ("task:1", NodeKind::Task),
            ("session:s1", NodeKind::Lead),
            ("task:2", NodeKind::Task),
            ("session:s2", NodeKind::Part),
            ("agent:a1", NodeKind::Subagent),
            ("helper:h1", NodeKind::Helper),
            ("job:7", NodeKind::Job),
            ("pr:9", NodeKind::Pr),
            ("ask:5", NodeKind::Ask),
            ("review:1", NodeKind::Review),
            ("review-node:1:1:correctness:0", NodeKind::ReviewNode),
            (stranger.as_str(), NodeKind::External),
        ];
        for (id, kind) in table {
            let node = graph.node(id);
            assert_eq!(node.map(|node| node.kind), Some(kind), "node {id}");
        }
    }

    #[test]
    fn a_plain_task_has_a_worker_session() {
        let graph = build(&facts(), None, Scope::All);
        assert_eq!(
            graph.node("session:s3").map(|node| node.kind),
            Some(NodeKind::Worker),
            "plain task"
        );
    }

    #[test]
    fn nesting_puts_parts_and_satellites_in_their_boxes() {
        let graph = build(&facts(), None, Scope::Live);
        let table = [
            ("task:1", None),
            ("task:2", Some("task:1")),
            ("session:s1", Some("task:1")),
            ("session:s2", Some("task:2")),
            ("agent:a1", Some("task:1")),
            ("helper:h1", Some("task:1")),
            ("job:7", Some("task:1")),
            ("pr:9", Some("task:1")),
            ("review:1", Some("task:2")),
            ("review-node:1:1:correctness:0", Some("task:2")),
        ];
        for (id, parent) in table {
            assert_eq!(
                graph.node(id).unwrap().parent.as_deref(),
                parent,
                "parent of {id}"
            );
        }
    }

    #[test]
    fn every_edge_kind_is_drawn_now() {
        let stranger = external_node("stranger");
        let graph = build(&facts(), None, Scope::Live);
        let table = [
            (EdgeKind::Spawned, "orchestrator", "task:1"),
            (EdgeKind::PartOf, "task:2", "task:1"),
            (EdgeKind::Called, "session:s1", "agent:a1"),
            (EdgeKind::Opened, "session:s1", "helper:h1"),
            (EdgeKind::Opened, "session:s1", "job:7"),
            (EdgeKind::Message, "session:s2", "session:s1"),
            (EdgeKind::Message, "session:s1", "orchestrator"),
            (EdgeKind::Message, "session:s1", stranger.as_str()),
            (EdgeKind::Report, "session:s1", "orchestrator"),
            (EdgeKind::Asked, "session:s1", "user"),
            (EdgeKind::Asked, "session:s2", "ask:5"),
            (EdgeKind::Asked, "ask:5", "user"),
            (EdgeKind::Reviews, "review:1", "session:s1"),
            (EdgeKind::Ran, "review:1", "review-node:1:1:correctness:0"),
            (EdgeKind::Delivered, "task:1", "pr:9"),
        ];
        for (kind, source, target) in table {
            assert!(
                edge(&graph, kind, source, target).is_some(),
                "{kind:?} {source} -> {target}"
            );
        }
    }

    #[test]
    fn a_task_scope_leaves_out_the_orchestrators_subagents() {
        let mut facts = facts();
        facts.agents.push(AgentFact {
            id: "a2".to_owned(),
            session: "o1".to_owned(),
            agent_type: None,
            description: None,
            model: None,
            depth: Some(1),
            parent: None,
            first_at: Some("T01".to_owned()),
            last_at: None,
        });
        assert!(
            build(&facts, None, Scope::Live).node("agent:a2").is_some(),
            "live draws it"
        );
        let graph = build(&facts, None, Scope::Task(1));
        assert!(
            graph.node("agent:a2").is_none() && graph.node("agent:a1").is_some(),
            "task scope keeps its own"
        );
    }

    #[test]
    fn depends_on_joins_two_drawn_tasks() {
        let mut facts = facts();
        facts.tasks[1].depends_on = Some(1);
        let graph = build(&facts, None, Scope::Live);
        assert!(
            edge(&graph, EdgeKind::DependsOn, "task:2", "task:1").is_some(),
            "dependency drawn"
        );
    }

    #[test]
    fn repeated_talk_adds_weight_and_keeps_the_newest_label() {
        let graph = build(&facts(), None, Scope::Live);
        let sent = edge(&graph, EdgeKind::Message, "session:s2", "session:s1").unwrap();
        assert_eq!(
            (sent.weight, sent.label.as_deref()),
            (2, Some("second")),
            "two messages, the last summary"
        );
        let reported = edge(&graph, EdgeKind::Report, "session:s1", "orchestrator").unwrap();
        assert_eq!(
            (reported.weight, reported.label.as_deref()),
            (2, Some("review")),
            "two reports, the last status"
        );
    }

    #[test]
    fn scopes_choose_the_tasks() {
        let table: [(Scope, &[&str], &[&str]); 3] = [
            (Scope::Live, &["task:1", "task:2"], &["task:3"]),
            (Scope::All, &["task:1", "task:2", "task:3"], &[]),
            (Scope::Task(2), &["task:1", "task:2"], &["task:3"]),
        ];
        for (scope, present, absent) in table {
            let graph = build(&facts(), None, scope);
            for id in present {
                assert!(graph.node(id).is_some(), "{scope:?} draws {id}");
            }
            for id in absent {
                assert!(graph.node(id).is_none(), "{scope:?} leaves out {id}");
            }
        }
    }

    #[test]
    fn edges_never_point_at_nodes_left_out() {
        let graph = build(&facts(), None, Scope::Task(3));
        for edge in &graph.edges {
            assert!(
                graph.node(&edge.source).is_some() && graph.node(&edge.target).is_some(),
                "{edge:?}"
            );
        }
    }

    #[test]
    fn the_graph_at_an_instant_holds_what_existed_then() {
        let table: [(&str, &[&str], &[&str]); 3] = [
            (
                "T01",
                &["task:1", "task:3"],
                &["task:2", "session:s1", "agent:a1"],
            ),
            (
                "T04",
                &["task:1", "task:2", "session:s1", "session:s2", "task:3"],
                &["agent:a1", "pr:9"],
            ),
            ("T07", &["agent:a1", "pr:9", "job:7", "ask:5"], &["task:3"]),
        ];
        for (at, present, absent) in table {
            let graph = build(&facts(), Some(at), Scope::Live);
            for id in present {
                assert!(graph.node(id).is_some(), "at {at} draws {id}");
            }
            for id in absent {
                assert!(graph.node(id).is_none(), "at {at} leaves out {id}");
            }
        }
    }

    #[test]
    fn states_at_an_instant_follow_the_transitions() {
        let graph = build(&facts(), Some("T01"), Scope::All);
        assert_eq!(
            graph.node("task:1").unwrap().state.map(Status::as_str),
            Some("queued"),
            "before its first transition"
        );
        let graph = build(&facts(), Some("T07"), Scope::All);
        let table = [
            ("task:1", "working"),
            ("task:3", "closed"),
            ("agent:a1", "done"),
            ("job:7", "running"),
            ("review-node:1:1:correctness:0", "done"),
        ];
        for (id, state) in table {
            assert_eq!(
                graph.node(id).unwrap().state.map(Status::as_str),
                Some(state),
                "state of {id}"
            );
        }
    }

    #[test]
    fn an_answered_ask_folds_into_an_edge() {
        let graph = build(&facts(), Some("T02"), Scope::Live);
        assert!(graph.node("ask:4").is_some(), "open at T02");
        let graph = build(&facts(), Some("T04"), Scope::Live);
        assert!(graph.node("ask:4").is_none(), "answered by T04");
        assert!(
            edge(&graph, EdgeKind::Asked, "session:s1", "user").is_some(),
            "folded into a line"
        );
    }

    #[test]
    fn the_orchestrator_is_one_node_across_restarts() {
        let mut facts = facts();
        facts.sessions[0].ended_at = Some("T03".to_owned());
        facts.sessions.push(session(
            "o2",
            None,
            Role::Orchestrator,
            "orch-2",
            "T03",
            None,
        ));
        facts
            .messages
            .push(message("o1", "headwork-lead", None, "T02"));
        let graph = build(&facts, None, Scope::Live);
        let orchestrators = graph
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Orchestrator)
            .count();
        assert_eq!(orchestrators, 1, "one node");
        assert_eq!(
            graph.node(ORCHESTRATOR_NODE).unwrap().label,
            "orch-2",
            "the newest live session names it"
        );
        assert!(
            edge(&graph, EdgeKind::Message, "orchestrator", "session:s1").is_some(),
            "old session's talk"
        );
    }

    #[test]
    fn a_resumed_task_shows_its_newest_session() {
        let mut facts = facts();
        facts.sessions[1].ended_at = Some("T04".to_owned());
        facts.sessions.push(session(
            "s1b",
            Some(1),
            Role::Worker,
            "headwork-lead",
            "T04",
            None,
        ));
        let graph = build(&facts, None, Scope::Live);
        assert!(
            graph.node("session:s1").is_none(),
            "the older session folds into the newest"
        );
        let node = graph.node("session:s1b").unwrap();
        assert_eq!(
            node.details.get("earlier_sessions").map(String::as_str),
            Some("s1"),
            "listed in details"
        );
        assert!(
            edge(&graph, EdgeKind::Called, "session:s1b", "agent:a1").is_some(),
            "older session's agents"
        );
    }

    #[test]
    fn state_at_reads_before_after_and_now() {
        let transitions = [
            transition(1, TaskState::Queued, TaskState::Working, "T02"),
            transition(1, TaskState::Working, TaskState::InReview, "T05"),
        ];
        let table = [
            ("T01", TaskState::Queued),
            ("T02", TaskState::Working),
            ("T04", TaskState::Working),
            ("T09", TaskState::InReview),
        ];
        for (at, state) in table {
            assert_eq!(
                state_at(&transitions, 1, at, TaskState::Merged),
                state,
                "at {at}"
            );
        }
        assert_eq!(
            state_at(&transitions, 2, "T01", TaskState::Merged),
            TaskState::Merged,
            "no transitions: now"
        );
    }

    #[test]
    fn the_graph_serialises_with_kebab_case_kinds() {
        let graph = build(&facts(), None, Scope::Live);
        let json = serde_json::to_value(&graph).unwrap();
        let kinds: BTreeSet<&str> = json["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|node| node["kind"].as_str())
            .collect();
        assert!(
            kinds.contains("review-node") && kinds.contains("subagent"),
            "{kinds:?}"
        );
    }

    #[test]
    fn the_orchestrator_is_ended_now_when_every_session_ended() {
        let mut facts = facts();
        facts.sessions[0].ended_at = Some("T05".to_owned());
        let graph = build(&facts, None, Scope::Live);
        assert_eq!(
            graph
                .node(ORCHESTRATOR_NODE)
                .unwrap()
                .state
                .map(Status::as_str),
            Some("ended"),
            "now"
        );
        let graph = build(&facts, Some("T04"), Scope::Live);
        assert_eq!(
            graph
                .node(ORCHESTRATOR_NODE)
                .unwrap()
                .state
                .map(Status::as_str),
            Some("live"),
            "before it ended"
        );
    }

    #[test]
    fn a_message_to_the_orchestrators_own_name_reaches_it() {
        let mut facts = facts();
        facts.sessions[0].rc_name = "boss".to_owned();
        facts.messages.push(message("s1", "boss", None, "T06"));
        let graph = build(&facts, None, Scope::Live);
        assert!(
            edge(&graph, EdgeKind::Message, "session:s1", ORCHESTRATOR_NODE).is_some(),
            "by its name"
        );
        assert!(
            graph.node(&external_node("boss")).is_none(),
            "not a stranger"
        );
    }

    #[test]
    fn a_message_to_something_known_but_not_drawn_is_left_out() {
        let mut facts = facts();
        facts
            .messages
            .push(message("s1", "headwork-old", None, "T06"));
        let graph = build(&facts, None, Scope::Live);
        assert!(
            graph.node(&external_node("headwork-old")).is_none(),
            "a known task is no stranger"
        );
        let graph = build(&facts, None, Scope::All);
        assert!(
            edge(&graph, EdgeKind::Message, "session:s1", "session:s3").is_some(),
            "drawn when its task is"
        );
    }

    #[test]
    fn a_message_to_a_subagent_reaches_it() {
        let mut facts = facts();
        facts.messages.push(message("s1", "a1", None, "T06"));
        let graph = build(&facts, None, Scope::Live);
        assert!(
            edge(&graph, EdgeKind::Message, "session:s1", "agent:a1").is_some(),
            "by its id"
        );
    }

    #[test]
    fn a_task_the_user_spawned_is_spawned_from_the_user() {
        let mut facts = facts();
        facts.tasks[0].created_by = Actor::User;
        let graph = build(&facts, None, Scope::Live);
        assert!(
            edge(&graph, EdgeKind::Spawned, USER_NODE, "task:1").is_some(),
            "from the user"
        );
        assert!(
            edge(&graph, EdgeKind::Spawned, ORCHESTRATOR_NODE, "task:1").is_none(),
            "not the orchestrator"
        );
    }

    #[test]
    fn a_live_part_brings_its_closed_lead() {
        let mut facts = facts();
        facts.tasks[0].state = TaskState::Closed;
        let graph = build(&facts, None, Scope::Live);
        assert!(graph.node("task:1").is_some(), "the lead is drawn");
        assert_eq!(
            graph.node("task:2").unwrap().parent.as_deref(),
            Some("task:1"),
            "in its box"
        );
    }

    #[test]
    fn a_superseded_ask_was_open_until_it_was_superseded() {
        let mut facts = facts();
        facts.asks[0].state = AskState::Superseded;
        facts.asks[0].closed_at = Some("T05".to_owned());
        assert!(
            build(&facts, Some("T04"), Scope::Live)
                .node("ask:4")
                .is_some(),
            "open before"
        );
        assert!(
            build(&facts, Some("T05"), Scope::Live)
                .node("ask:4")
                .is_none(),
            "closed at"
        );
        facts.asks[0].closed_at = None;
        assert!(
            build(&facts, Some("T02"), Scope::Live)
                .node("ask:4")
                .is_none(),
            "no time: closed when made"
        );
    }

    #[test]
    fn an_ask_without_a_header_takes_the_start_of_its_question() {
        let mut facts = facts();
        facts.asks[1].header = None;
        facts.asks[1].question = "q".repeat(60);
        let graph = build(&facts, None, Scope::Live);
        assert_eq!(
            graph.node("ask:5").unwrap().label.chars().count(),
            ASK_LABEL_CHARS,
            "cut"
        );
    }

    #[test]
    fn a_task_scope_leaves_out_the_orchestrators_asks() {
        let mut facts = facts();
        facts.asks.push(AskFact {
            id: 6,
            task: None,
            session: Some("o1".to_owned()),
            header: Some("Spawn".to_owned()),
            question: "Go?".to_owned(),
            state: AskState::Open,
            created_at: "T02".to_owned(),
            closed_at: None,
        });
        assert!(
            build(&facts, None, Scope::Live).node("ask:6").is_some(),
            "live draws it"
        );
        assert!(
            build(&facts, None, Scope::Task(1)).node("ask:6").is_none(),
            "task scope leaves it out"
        );
    }

    #[test]
    fn a_subagent_without_times_shows_only_now() {
        let mut facts = facts();
        facts.agents[0].first_at = None;
        facts.agents[0].last_at = None;
        assert_eq!(
            build(&facts, None, Scope::Live)
                .node("agent:a1")
                .unwrap()
                .state
                .map(Status::as_str),
            Some("seen"),
            "now"
        );
        assert!(
            build(&facts, Some("T09"), Scope::Live)
                .node("agent:a1")
                .is_none(),
            "never in a past view"
        );
    }

    #[test]
    fn session_states_are_the_stored_word_now_and_live_or_ended_then() {
        let mut facts = facts();
        facts.sessions[1].state = SessionState::Starting;
        assert_eq!(
            build(&facts, None, Scope::Live)
                .node("session:s1")
                .unwrap()
                .state
                .map(Status::as_str),
            Some("starting"),
            "now"
        );
        assert_eq!(
            build(&facts, Some("T03"), Scope::Live)
                .node("session:s1")
                .unwrap()
                .state
                .map(Status::as_str),
            Some("live"),
            "then"
        );
    }

    #[test]
    fn kind_names_are_the_json_names() {
        for kind in NodeKind::ALL {
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                kind.as_str(),
                "{kind:?}"
            );
        }
        for kind in EdgeKind::ALL {
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                kind.as_str(),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn past_states_of_pull_requests_jobs_and_reviews_come_from_their_logs() {
        let mut facts = facts();
        facts.prs[0].state = PullState::Merged;
        facts.pr_changes.push(Change {
            key: 9,
            from: Some(PullState::Open),
            to: PullState::Merged,
            at: "T08".to_owned(),
        });
        facts.jobs[0].ended_at = None;
        facts.jobs[0].state = JobState::Failed;
        facts.job_changes.push(Change {
            key: 7,
            from: None,
            to: JobState::Failed,
            at: "T08".to_owned(),
        });
        facts.reviews[0].state = ReviewState::Done;
        facts.review_changes.push(Change {
            key: 1,
            from: Some(ReviewState::Requested),
            to: ReviewState::Running,
            at: "T04".to_owned(),
        });
        facts.review_changes.push(Change {
            key: 1,
            from: Some(ReviewState::Running),
            to: ReviewState::Done,
            at: "T08".to_owned(),
        });
        let table = [
            ("T07", "pr:9", "open"),
            ("T09", "pr:9", "merged"),
            ("T07", "job:7", "running"),
            ("T09", "job:7", "failed"),
            ("T03", "review:1", "requested"),
            ("T05", "review:1", "running"),
            ("T09", "review:1", "done"),
        ];
        for (at, id, state) in table {
            let graph = build(&facts, Some(at), Scope::All);
            assert_eq!(
                graph.node(id).unwrap().state.map(Status::as_str),
                Some(state),
                "{id} at {at}"
            );
        }
        assert_eq!(
            build(&facts, None, Scope::All).node("pr:9").unwrap().state,
            Some(Status::Pull(PullState::Merged)),
            "now"
        );
    }

    #[test]
    fn change_at_reads_before_after_first_and_now() {
        let changes = [
            Change {
                key: 1,
                from: None,
                to: JobState::Stopping,
                at: "T05".to_owned(),
            },
            Change {
                key: 1,
                from: Some(JobState::Stopping),
                to: JobState::Done,
                at: "T07".to_owned(),
            },
        ];
        let table = [
            ("T01", JobState::Running),
            ("T05", JobState::Stopping),
            ("T06", JobState::Stopping),
            ("T09", JobState::Done),
        ];
        for (at, state) in table {
            assert_eq!(
                change_at(&changes, 1, at, JobState::Running, JobState::Parked),
                state,
                "at {at}"
            );
        }
        assert_eq!(
            change_at(&changes, 2, "T01", JobState::Running, JobState::Parked),
            JobState::Parked,
            "no log: now"
        );
    }

    #[test]
    fn a_nested_subagent_is_called_by_its_parent_agent() {
        let mut facts = facts();
        let mut child = facts.agents[0].clone();
        child.id = "a2".to_owned();
        child.depth = Some(2);
        child.parent = Some("a1".to_owned());
        facts.agents.push(child);
        let graph = build(&facts, None, Scope::Live);
        assert!(
            edge(&graph, EdgeKind::Called, "agent:a1", "agent:a2").is_some(),
            "from its parent"
        );
        assert!(
            edge(&graph, EdgeKind::Called, "session:s1", "agent:a2").is_none(),
            "not from the session"
        );
        facts.agents[1].parent = Some("gone".to_owned());
        let graph = build(&facts, None, Scope::Live);
        assert!(
            edge(&graph, EdgeKind::Called, "session:s1", "agent:a2").is_some(),
            "an unknown parent: the session"
        );
    }

    #[test]
    fn an_external_node_id_never_holds_the_address() {
        let mut facts = facts();
        let address = "<img src=x onerror=alert(1)>/../../x";
        facts.messages.push(message("s1", address, None, "T06"));
        let graph = build(&facts, None, Scope::Live);
        let node = graph
            .nodes
            .iter()
            .find(|node| node.label == address)
            .unwrap();
        assert_eq!(node.id, external_node(address), "a stable id");
        assert!(
            node.id.strip_prefix("external:").is_some_and(
                |hash| hash.len() == 16 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            ),
            "only a hash: {}",
            node.id
        );
        assert_ne!(
            external_node("a"),
            external_node("b"),
            "different addresses, different ids"
        );
    }

    #[test]
    fn states_serialise_as_their_words() {
        let json = serde_json::to_value(Status::ReviewNode(NodeState::Running)).unwrap();
        assert_eq!(json, "running", "a word");
        assert_eq!(
            serde_json::to_value(AgentState::Seen).unwrap(),
            AgentState::Seen.as_str(),
            "agent words"
        );
    }
}
