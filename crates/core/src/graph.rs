//! The agent graph (`docs/dashboard.md`, sections 3 to 6): the store's facts in, nodes and edges out.
//!
//! [`build`] draws the graph as it stood at one instant: every row whose start lies at or before it and whose end, if
//! any, lies after it, with each task's state taken from its transitions ([`state_at`]). Times are the store's RFC 3339
//! UTC text, which sorts as it reads. Nothing here reads a file or a database; the store gathers the [`Facts`].

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::names::is_orchestrator_address;
use crate::vocabulary::{Role, TaskState};

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
    pub model: String,
    /// The effort stored at spawn.
    pub effort: Option<String>,
    /// Its lead, for a part.
    pub parent: Option<i64>,
    /// The task it waits for.
    pub depends_on: Option<i64>,
    /// Whether it is a lead.
    pub lead: bool,
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
    pub state: String,
    /// When it started.
    pub started_at: String,
    /// When it ended.
    pub ended_at: Option<String>,
}

/// A subagent a session called (`agents` table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFact {
    /// The agent id.
    pub id: String,
    /// The session that called it.
    pub session: String,
    /// The task of that session.
    pub task: Option<i64>,
    /// Its type, such as `general-purpose`.
    pub agent_type: Option<String>,
    /// The short label its caller gave it.
    pub description: Option<String>,
    /// The model it asked for.
    pub model: Option<String>,
    /// How deep it was nested: 1 for a session's own.
    pub depth: Option<i64>,
    /// Its first response.
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
    pub state: String,
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
    /// `open`, `merged` or `closed`.
    pub state: String,
    /// When the store first learned of it: the first event naming its URL, else its task's start.
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
    /// Its header (topic), else its kind.
    pub header: String,
    /// The question.
    pub question: String,
    /// The ask state now.
    pub state: String,
    /// When it was asked.
    pub created_at: String,
    /// When it was answered, expired or superseded.
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
    pub level: i64,
    /// The review state now.
    pub state: String,
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
    pub state: String,
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
}

/// Which tasks to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Tasks not over (closed, dropped) at the instant.
    Live,
    /// Every task that existed at the instant.
    All,
    /// One task, with its parts and its lead.
    Task(i64),
}

/// What a node stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeKind {
    /// The user.
    User,
    /// Every orchestrator session, as one node.
    Orchestrator,
    /// A task: a box around its sessions, parts and satellites.
    Task,
    /// A lead task's session.
    Lead,
    /// A plain task's session.
    Worker,
    /// A part task's session.
    Part,
    /// A subagent.
    Subagent,
    /// A helper session.
    Helper,
    /// A review.
    Review,
    /// One node run of a review's flow.
    ReviewNode,
    /// A job.
    Job,
    /// A pull request.
    Pr,
    /// An open ask to the user.
    Ask,
    /// A message address headwork does not know.
    External,
}

/// What an edge means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EdgeKind {
    /// The orchestrator made the task.
    Spawned,
    /// A part belongs to its lead's task (also drawn as nesting).
    PartOf,
    /// A task waits for another.
    DependsOn,
    /// A session called a subagent.
    Called,
    /// A session opened a helper or a job.
    Opened,
    /// Session messages, counted.
    Message,
    /// Reports to the orchestrator, counted.
    Report,
    /// Asks to the user, counted; an open one runs through its ask node.
    Asked,
    /// A review looks at a task.
    Reviews,
    /// A review ran a node of its flow.
    Ran,
    /// A task delivers in a pull request.
    Delivered,
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
    /// Its state, drawn as colour.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
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

fn alive(start: &str, end: Option<&str>, at: Option<&str>) -> bool {
    at.is_none_or(|at| start <= at && end.is_none_or(|end| end > at))
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

/// Edges keyed by kind, source and target, so repeats add to one edge's weight.
#[derive(Default)]
struct EdgeSet {
    edges: BTreeMap<(EdgeKind, String, String), Edge>,
}

impl EdgeSet {
    fn add(
        &mut self,
        kind: EdgeKind,
        source: &str,
        target: &str,
        label: Option<String>,
        at: Option<&str>,
    ) {
        let key = (kind, source.to_owned(), target.to_owned());
        let edge = self.edges.entry(key).or_insert_with(|| Edge {
            id: format!("{}:{source}->{target}", kind_name(kind)),
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
}

const fn kind_name(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Spawned => "spawned",
        EdgeKind::PartOf => "part-of",
        EdgeKind::DependsOn => "depends-on",
        EdgeKind::Called => "called",
        EdgeKind::Opened => "opened",
        EdgeKind::Message => "message",
        EdgeKind::Report => "report",
        EdgeKind::Asked => "asked",
        EdgeKind::Reviews => "reviews",
        EdgeKind::Ran => "ran",
        EdgeKind::Delivered => "delivered",
    }
}

/// What a build knows about the sessions it drew, to point edges at them.
struct Places {
    /// Session id to the node that stands for it: its own, or the orchestrator's.
    sessions: BTreeMap<String, String>,
    /// Message address (a Remote Control or task name) to node.
    addresses: BTreeMap<String, String>,
    /// Task to the node of its newest session, or its box when none is drawn.
    task_heads: BTreeMap<i64, String>,
}

impl Places {
    fn of_session(&self, session: &str) -> Option<&String> {
        self.sessions.get(session)
    }

    fn of_address(&self, address: &str) -> Option<String> {
        let bare = address
            .split_once(" [")
            .map_or(address, |(name, _)| name)
            .trim();
        if is_orchestrator_address(bare) {
            return Some(ORCHESTRATOR_NODE.to_owned());
        }
        self.addresses.get(bare).cloned()
    }
}

/// The graph at `at` (now when none) for `scope`.
#[must_use]
pub fn build(facts: &Facts, at: Option<&str>, scope: Scope) -> Graph {
    let mut nodes: BTreeMap<String, Node> = BTreeMap::new();
    let mut edges = EdgeSet::default();
    nodes.insert(
        USER_NODE.to_owned(),
        Node {
            id: USER_NODE.to_owned(),
            kind: NodeKind::User,
            label: "you".to_owned(),
            state: None,
            parent: None,
            start: None,
            end: None,
            details: BTreeMap::new(),
        },
    );
    let tasks = drawn_tasks(facts, at, scope);
    let mut places = Places {
        sessions: BTreeMap::new(),
        addresses: BTreeMap::new(),
        task_heads: BTreeMap::new(),
    };
    orchestrator(facts, at, &mut nodes, &mut places);
    for (task, state) in &tasks {
        task_box(facts, task, *state, &mut nodes, &mut edges, &tasks);
        places
            .addresses
            .insert(task.name.clone(), task_node(task.id));
        places.task_heads.insert(task.id, task_node(task.id));
    }
    for (task, _) in &tasks {
        task_session(facts, task, at, &mut nodes, &mut places);
    }
    satellites(facts, at, &tasks, &mut nodes, &mut edges, &places);
    if let Scope::Task(_) = scope {
        nodes.retain(|_, node| node.kind != NodeKind::Subagent || node.parent.is_some());
    }
    talk(facts, at, &mut nodes, &mut edges, &places);
    let edges: Vec<Edge> = edges
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

fn drawn_tasks<'a>(
    facts: &'a Facts,
    at: Option<&str>,
    scope: Scope,
) -> Vec<(&'a TaskFact, TaskState)> {
    let existing = facts.tasks.iter().filter_map(|task| {
        let state = at.map_or(task.state, |at| {
            state_at(&facts.transitions, task.id, at, task.state)
        });
        let started = at.is_none_or(|at| task.queued_at.as_str() <= at);
        started.then_some((task, state))
    });
    match scope {
        Scope::All => existing.collect(),
        Scope::Live => existing.filter(|(_, state)| !is_over(*state)).collect(),
        Scope::Task(id) => {
            let lead = facts
                .tasks
                .iter()
                .find(|task| task.id == id)
                .and_then(|task| task.parent)
                .unwrap_or(id);
            existing
                .filter(|(task, _)| task.id == lead || task.parent == Some(lead))
                .collect()
        }
    }
}

fn orchestrator(
    facts: &Facts,
    at: Option<&str>,
    nodes: &mut BTreeMap<String, Node>,
    places: &mut Places,
) {
    let sessions: Vec<&SessionFact> = facts
        .sessions
        .iter()
        .filter(|session| session.role == Role::Orchestrator)
        .collect();
    for session in &sessions {
        places
            .sessions
            .insert(session.id.clone(), ORCHESTRATOR_NODE.to_owned());
    }
    let live: Vec<&&SessionFact> = sessions
        .iter()
        .filter(|session| alive(&session.started_at, session.ended_at.as_deref(), at))
        .collect();
    let newest = live
        .iter()
        .map(|session| **session)
        .max_by(|left, right| left.started_at.cmp(&right.started_at))
        .or_else(|| {
            sessions
                .iter()
                .copied()
                .max_by(|left, right| left.started_at.cmp(&right.started_at))
        });
    let label = newest.map_or_else(
        || ORCHESTRATOR_NODE.to_owned(),
        |session| session.rc_name.clone(),
    );
    let state = if live.is_empty() { "ended" } else { "live" };
    let mut info = BTreeMap::new();
    info.insert(
        "sessions".to_owned(),
        sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    );
    nodes.insert(
        ORCHESTRATOR_NODE.to_owned(),
        Node {
            id: ORCHESTRATOR_NODE.to_owned(),
            kind: NodeKind::Orchestrator,
            label,
            state: Some(state.to_owned()),
            parent: None,
            start: sessions
                .iter()
                .map(|session| session.started_at.clone())
                .min(),
            end: None,
            details: info,
        },
    );
}

fn task_box(
    facts: &Facts,
    task: &TaskFact,
    state: TaskState,
    nodes: &mut BTreeMap<String, Node>,
    edges: &mut EdgeSet,
    drawn: &[(&TaskFact, TaskState)],
) {
    let id = task_node(task.id);
    let is_drawn = |other: i64| drawn.iter().any(|(task, _)| task.id == other);
    let parent = task.parent.filter(|lead| is_drawn(*lead)).map(task_node);
    nodes.insert(
        id.clone(),
        Node {
            id: id.clone(),
            kind: NodeKind::Task,
            label: task.name.clone(),
            state: Some(state.as_str().to_owned()),
            parent,
            start: Some(task.queued_at.clone()),
            end: task_end(&facts.transitions, task.id),
            details: details([
                ("project", Some(task.project.clone())),
                ("kind", Some(task.kind.clone())),
                ("model", Some(task.model.clone())),
                ("effort", task.effort.clone()),
                ("last_report", task.last_report.clone()),
            ]),
        },
    );
    match task.parent {
        Some(lead) => edges.add(
            EdgeKind::PartOf,
            &id,
            &task_node(lead),
            None,
            Some(&task.queued_at),
        ),
        None => edges.add(
            EdgeKind::Spawned,
            ORCHESTRATOR_NODE,
            &id,
            None,
            Some(&task.queued_at),
        ),
    }
    if let Some(other) = task.depends_on {
        edges.add(EdgeKind::DependsOn, &id, &task_node(other), None, None);
    }
}

fn task_session(
    facts: &Facts,
    task: &TaskFact,
    at: Option<&str>,
    nodes: &mut BTreeMap<String, Node>,
    places: &mut Places,
) {
    let sessions: Vec<&SessionFact> = facts
        .sessions
        .iter()
        .filter(|session| session.task == Some(task.id) && session.role == Role::Worker)
        .filter(|session| at.is_none_or(|at| session.started_at.as_str() <= at))
        .collect();
    let Some(newest) = sessions
        .iter()
        .max_by(|left, right| left.started_at.cmp(&right.started_at))
    else {
        return;
    };
    let id = session_node(&newest.id);
    for session in &sessions {
        places.sessions.insert(session.id.clone(), id.clone());
        places.addresses.insert(session.rc_name.clone(), id.clone());
    }
    places.task_heads.insert(task.id, id.clone());
    let kind = if task.parent.is_some() {
        NodeKind::Part
    } else if task.lead {
        NodeKind::Lead
    } else {
        NodeKind::Worker
    };
    let state = if alive(&newest.started_at, newest.ended_at.as_deref(), at) {
        if at.is_some() {
            "live".to_owned()
        } else {
            newest.state.clone()
        }
    } else {
        "ended".to_owned()
    };
    let label = task.effort.as_ref().map_or_else(
        || task.model.clone(),
        |effort| format!("{} {effort}", task.model),
    );
    let earlier: Vec<&str> = sessions
        .iter()
        .filter(|session| session.id != newest.id)
        .map(|session| session.id.as_str())
        .collect();
    nodes.insert(
        id.clone(),
        Node {
            id,
            kind,
            label,
            state: Some(state),
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
        },
    );
}

fn satellites(
    facts: &Facts,
    at: Option<&str>,
    tasks: &[(&TaskFact, TaskState)],
    nodes: &mut BTreeMap<String, Node>,
    edges: &mut EdgeSet,
    places: &Places,
) {
    let drawn: BTreeSet<i64> = tasks.iter().map(|(task, _)| task.id).collect();
    subagents(facts, at, &drawn, nodes, edges, places);
    helpers(facts, at, &drawn, nodes, edges, places);
    jobs_and_prs(facts, at, &drawn, nodes, edges, places);
    reviews(facts, at, &drawn, nodes, edges, places);
}

fn subagents(
    facts: &Facts,
    at: Option<&str>,
    drawn: &BTreeSet<i64>,
    nodes: &mut BTreeMap<String, Node>,
    edges: &mut EdgeSet,
    places: &Places,
) {
    for agent in &facts.agents {
        let Some(caller) = places.of_session(&agent.session) else {
            continue;
        };
        let start = agent.first_at.clone().unwrap_or_default();
        if at.is_some_and(|at| start.as_str() > at) {
            continue;
        }
        let id = format!("agent:{}", agent.id);
        let parent = agent
            .task
            .filter(|task| drawn.contains(task))
            .map(task_node);
        let label = agent
            .agent_type
            .clone()
            .unwrap_or_else(|| "subagent".to_owned());
        let state = match (at, agent.last_at.as_deref()) {
            (Some(at), Some(last)) if last <= at => "done",
            (Some(_), _) => "running",
            (None, _) => "seen",
        };
        nodes.insert(
            id.clone(),
            Node {
                id: id.clone(),
                kind: NodeKind::Subagent,
                label,
                state: Some(state.to_owned()),
                parent,
                start: agent.first_at.clone(),
                end: agent.last_at.clone(),
                details: details([
                    ("description", agent.description.clone()),
                    ("model", agent.model.clone()),
                    ("depth", agent.depth.map(|depth| depth.to_string())),
                ]),
            },
        );
        edges.add(
            EdgeKind::Called,
            caller,
            &id,
            None,
            agent.first_at.as_deref(),
        );
    }
}

fn helpers(
    facts: &Facts,
    at: Option<&str>,
    drawn: &BTreeSet<i64>,
    nodes: &mut BTreeMap<String, Node>,
    edges: &mut EdgeSet,
    places: &Places,
) {
    for helper in facts
        .helpers
        .iter()
        .filter(|helper| drawn.contains(&helper.task))
    {
        if at.is_some_and(|at| helper.at.as_str() > at) {
            continue;
        }
        let id = format!("helper:{}", helper.session);
        nodes.insert(
            id.clone(),
            Node {
                id: id.clone(),
                kind: NodeKind::Helper,
                label: "headwork:helper".to_owned(),
                state: None,
                parent: Some(task_node(helper.task)),
                start: Some(helper.at.clone()),
                end: None,
                details: details([("session", Some(helper.session.clone()))]),
            },
        );
        if let Some(head) = places.task_heads.get(&helper.task) {
            edges.add(EdgeKind::Opened, head, &id, None, Some(&helper.at));
        }
    }
}

fn jobs_and_prs(
    facts: &Facts,
    at: Option<&str>,
    drawn: &BTreeSet<i64>,
    nodes: &mut BTreeMap<String, Node>,
    edges: &mut EdgeSet,
    places: &Places,
) {
    for job in &facts.jobs {
        let Some(task) = job.task.filter(|task| drawn.contains(task)) else {
            continue;
        };
        if !alive(&job.started_at, None, at) {
            continue;
        }
        let id = format!("job:{}", job.id);
        let state = match (at, job.ended_at.as_deref()) {
            (Some(at), Some(end)) if end <= at => job.state.clone(),
            (Some(_), _) => "running".to_owned(),
            (None, _) => job.state.clone(),
        };
        nodes.insert(
            id.clone(),
            Node {
                id: id.clone(),
                kind: NodeKind::Job,
                label: job.label.clone(),
                state: Some(state),
                parent: Some(task_node(task)),
                start: Some(job.started_at.clone()),
                end: job.ended_at.clone(),
                details: BTreeMap::new(),
            },
        );
        let opener = job
            .session
            .as_deref()
            .and_then(|session| places.of_session(session))
            .or_else(|| places.task_heads.get(&task));
        if let Some(opener) = opener {
            edges.add(EdgeKind::Opened, opener, &id, None, Some(&job.started_at));
        }
    }
    for pr in facts.prs.iter().filter(|pr| drawn.contains(&pr.task)) {
        if !alive(&pr.first_at, None, at) {
            continue;
        }
        let id = format!("pr:{}", pr.id);
        nodes.insert(
            id.clone(),
            Node {
                id: id.clone(),
                kind: NodeKind::Pr,
                label: format!("#{}", pr.number),
                state: Some(pr.state.clone()),
                parent: Some(task_node(pr.task)),
                start: Some(pr.first_at.clone()),
                end: None,
                details: details([("url", Some(pr.url.clone()))]),
            },
        );
        edges.add(
            EdgeKind::Delivered,
            &task_node(pr.task),
            &id,
            None,
            Some(&pr.first_at),
        );
    }
}

fn reviews(
    facts: &Facts,
    at: Option<&str>,
    drawn: &BTreeSet<i64>,
    nodes: &mut BTreeMap<String, Node>,
    edges: &mut EdgeSet,
    places: &Places,
) {
    for review in facts
        .reviews
        .iter()
        .filter(|review| drawn.contains(&review.task))
    {
        if !alive(&review.requested_at, None, at) {
            continue;
        }
        let id = format!("review:{}", review.id);
        let state = match (at, review.finished_at.as_deref()) {
            (Some(at), Some(end)) if end > at => "running".to_owned(),
            _ => review.state.clone(),
        };
        let parent = review
            .part
            .filter(|part| drawn.contains(part))
            .unwrap_or(review.task);
        nodes.insert(
            id.clone(),
            Node {
                id: id.clone(),
                kind: NodeKind::Review,
                label: format!("{} {}", review.flow, review.level),
                state: Some(state),
                parent: Some(task_node(parent)),
                start: Some(review.requested_at.clone()),
                end: review.finished_at.clone(),
                details: BTreeMap::new(),
            },
        );
        let reviewed = places
            .task_heads
            .get(&review.task)
            .cloned()
            .unwrap_or_else(|| task_node(review.task));
        edges.add(
            EdgeKind::Reviews,
            &id,
            &reviewed,
            None,
            Some(&review.requested_at),
        );
        for run in facts
            .review_nodes
            .iter()
            .filter(|run| run.review == review.id)
        {
            let start = run.started_at.as_deref().unwrap_or(&review.requested_at);
            if !alive(start, None, at) {
                continue;
            }
            let node_id = format!(
                "review-node:{}:{}:{}:{}",
                review.id, run.round, run.node, run.item
            );
            let state = match (at, run.finished_at.as_deref()) {
                (Some(at), Some(end)) if end > at => "running".to_owned(),
                _ => run.state.clone(),
            };
            let label = if run.item == 0 {
                run.node.clone()
            } else {
                format!("{} {}", run.node, run.item)
            };
            nodes.insert(
                node_id.clone(),
                Node {
                    id: node_id.clone(),
                    kind: NodeKind::ReviewNode,
                    label,
                    state: Some(state),
                    parent: Some(task_node(parent)),
                    start: run.started_at.clone(),
                    end: run.finished_at.clone(),
                    details: details([("round", Some(run.round.to_string()))]),
                },
            );
            edges.add(
                EdgeKind::Ran,
                &id,
                &node_id,
                None,
                run.started_at.as_deref(),
            );
        }
    }
}

fn talk(
    facts: &Facts,
    at: Option<&str>,
    nodes: &mut BTreeMap<String, Node>,
    edges: &mut EdgeSet,
    places: &Places,
) {
    let until = |time: &str| at.is_none_or(|at| time <= at);
    for message in facts.messages.iter().filter(|message| until(&message.at)) {
        let Some(from) = places.of_session(&message.from) else {
            continue;
        };
        let target = places.of_address(&message.to).unwrap_or_else(|| {
            let id = format!("external:{}", message.to);
            nodes.entry(id.clone()).or_insert_with(|| Node {
                id: id.clone(),
                kind: NodeKind::External,
                label: message.to.clone(),
                state: None,
                parent: None,
                start: Some(message.at.clone()),
                end: None,
                details: BTreeMap::new(),
            });
            id
        });
        edges.add(
            EdgeKind::Message,
            from,
            &target,
            message.summary.clone(),
            Some(&message.at),
        );
    }
    for report in facts.reports.iter().filter(|report| until(&report.at)) {
        if let Some(from) = places.of_session(&report.session) {
            edges.add(
                EdgeKind::Report,
                from,
                ORCHESTRATOR_NODE,
                Some(report.status.clone()),
                Some(&report.at),
            );
        }
    }
    for ask in facts.asks.iter().filter(|ask| until(&ask.created_at)) {
        let asker = ask
            .session
            .as_deref()
            .and_then(|session| places.of_session(session).cloned())
            .or_else(|| {
                ask.task
                    .and_then(|task| places.task_heads.get(&task).cloned())
            });
        let Some(asker) = asker else {
            continue;
        };
        let open = match (at, ask.closed_at.as_deref()) {
            (Some(at), Some(closed)) => closed > at,
            (Some(_), None) => true,
            (None, _) => matches!(ask.state.as_str(), "pending" | "open"),
        };
        if open {
            let id = format!("ask:{}", ask.id);
            nodes.insert(
                id.clone(),
                Node {
                    id: id.clone(),
                    kind: NodeKind::Ask,
                    label: ask.header.clone(),
                    state: Some("open".to_owned()),
                    parent: None,
                    start: Some(ask.created_at.clone()),
                    end: ask.closed_at.clone(),
                    details: details([("question", Some(ask.question.clone()))]),
                },
            );
            edges.add(
                EdgeKind::Asked,
                &asker,
                &id,
                Some(ask.header.clone()),
                Some(&ask.created_at),
            );
            edges.add(
                EdgeKind::Asked,
                &id,
                USER_NODE,
                Some(ask.header.clone()),
                Some(&ask.created_at),
            );
        } else {
            edges.add(
                EdgeKind::Asked,
                &asker,
                USER_NODE,
                Some(ask.header.clone()),
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
            model: "opus".to_owned(),
            effort: Some("medium".to_owned()),
            parent,
            depends_on: None,
            lead,
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
            state: if ended.is_some() { "ended" } else { "live" }.to_owned(),
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
            header: header.to_owned(),
            question: "Which?".to_owned(),
            state: if closed.is_some() { "answered" } else { "open" }.to_owned(),
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
            level: 3,
            state: "running".to_owned(),
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
                task: Some(1),
                agent_type: Some("Explore".to_owned()),
                description: Some("find the hook".to_owned()),
                model: Some("haiku".to_owned()),
                depth: Some(1),
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
                state: "done".to_owned(),
                started_at: "T05".to_owned(),
                ended_at: Some("T08".to_owned()),
            }],
            prs: vec![PrFact {
                id: 9,
                task: 1,
                number: 41,
                url: "https://github.com/o/r/pull/41".to_owned(),
                state: "open".to_owned(),
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
                state: "done".to_owned(),
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
        }
    }

    fn edge<'g>(graph: &'g Graph, kind: EdgeKind, source: &str, target: &str) -> Option<&'g Edge> {
        graph
            .edges_of(kind)
            .find(|edge| edge.source == source && edge.target == target)
    }

    #[test]
    fn every_node_kind_is_drawn_now() {
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
            ("external:stranger", NodeKind::External),
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
        let graph = build(&facts(), None, Scope::Live);
        let table = [
            (EdgeKind::Spawned, "orchestrator", "task:1"),
            (EdgeKind::PartOf, "task:2", "task:1"),
            (EdgeKind::Called, "session:s1", "agent:a1"),
            (EdgeKind::Opened, "session:s1", "helper:h1"),
            (EdgeKind::Opened, "session:s1", "job:7"),
            (EdgeKind::Message, "session:s2", "session:s1"),
            (EdgeKind::Message, "session:s1", "orchestrator"),
            (EdgeKind::Message, "session:s1", "external:stranger"),
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
            task: None,
            agent_type: None,
            description: None,
            model: None,
            depth: Some(1),
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
            graph.node("task:1").unwrap().state.as_deref(),
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
                graph.node(id).unwrap().state.as_deref(),
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
}
