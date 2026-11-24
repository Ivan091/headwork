//! Drawing the graph: which tasks a view holds, where every session and address points, and one routine per node kind.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    AgentFact, AgentState, AskFact, Change, Edge, EdgeKind, Facts, Graph, Node, ORCHESTRATOR_NODE,
    Scope, SessionFact, Shape, TaskFact, USER_NODE, change_at, task_node,
};
use crate::names::{bare_address, is_orchestrator_address};
use crate::vocabulary::{
    Actor, AskState, FindingVerdict, JobState, NodeState, PullState, ReviewState, Role,
    SessionState, TaskState,
};

/// The longest label an ask without a header takes from its question, in characters.
pub(super) const ASK_LABEL_CHARS: usize = 40;

fn agent_node(agent: &str) -> String {
    format!("agent:{agent}")
}

fn helper_node(session: &str) -> String {
    format!("helper:{session}")
}

/// The id of the node of a message address headwork does not know: `external:` and a hash of its name (without a
/// `[ref]`), so the address, written by an agent, is only ever a label and never part of an id.
pub(super) fn external_node(address: &str) -> String {
    let hash = bare_address(address)
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    format!("external:{hash:016x}")
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

fn details<const N: usize>(pairs: [(&str, Option<String>); N]) -> BTreeMap<String, String> {
    pairs
        .into_iter()
        .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value)))
        .collect()
}

/// The changes of each row, oldest first.
fn by_key<S>(changes: &[Change<S>]) -> BTreeMap<i64, Vec<&Change<S>>> {
    let mut grouped: BTreeMap<i64, Vec<&Change<S>>> = BTreeMap::new();
    for change in changes {
        grouped.entry(change.key).or_default().push(change);
    }
    grouped
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
        Self::touch(edge, label, at);
    }

    /// Gives an edge already drawn a newer label, without counting it again.
    fn label(
        &mut self,
        kind: EdgeKind,
        source: &str,
        target: &str,
        label: Option<String>,
        at: Option<&str>,
    ) {
        if let Some(edge) = self
            .edges
            .get_mut(&(kind, source.to_owned(), target.to_owned()))
        {
            Self::touch(edge, label, at);
        }
    }

    fn touch(edge: &mut Edge, label: Option<String>, at: Option<&str>) {
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

    fn finish(self, at: Option<&str>, skipped: &[String]) -> Graph {
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
            skipped: skipped.to_vec(),
        }
    }
}

/// One build: the facts and their indexes, the instant, the scope, the tasks drawn and where sessions and addresses
/// point.
struct Draw<'f> {
    facts: &'f Facts,
    at: Option<&'f str>,
    scope: Scope,
    drawn: BTreeMap<i64, TaskState>,
    /// Each task's newest session started by the instant.
    newest: BTreeMap<i64, &'f SessionFact>,
    /// The task of every session and helper session.
    session_tasks: BTreeMap<&'f str, i64>,
    /// Every session by id.
    by_id: BTreeMap<&'f str, &'f SessionFact>,
    /// Every session by its Remote Control name, oldest first: a closed task's name may be taken again.
    by_name: BTreeMap<&'f str, Vec<&'f SessionFact>>,
    /// Each task's worker sessions, oldest first.
    task_sessions: BTreeMap<i64, Vec<&'f SessionFact>>,
    agents: BTreeMap<&'f str, &'f AgentFact>,
    sessions: BTreeMap<&'f str, Place>,
    addresses: BTreeMap<&'f str, Place>,
    transitions: BTreeMap<i64, Vec<&'f Change<TaskState>>>,
    pr_changes: BTreeMap<i64, Vec<&'f Change<PullState>>>,
    job_changes: BTreeMap<i64, Vec<&'f Change<JobState>>>,
    review_changes: BTreeMap<i64, Vec<&'f Change<ReviewState>>>,
}

impl<'f> Draw<'f> {
    fn new(facts: &'f Facts, at: Option<&'f str>, scope: Scope) -> Self {
        let transitions = by_key(&facts.transitions);
        let mut newest: BTreeMap<i64, &'f SessionFact> = BTreeMap::new();
        for session in &facts.sessions {
            let Some(task) = session.task.filter(|_| session.role == Role::Worker) else {
                continue;
            };
            if at.is_some_and(|at| session.started_at.as_str() > at) {
                continue;
            }
            if newest
                .get(&task)
                .is_none_or(|seen| seen.started_at <= session.started_at)
            {
                newest.insert(task, session);
            }
        }
        let mut session_tasks: BTreeMap<&'f str, i64> = facts
            .sessions
            .iter()
            .filter_map(|session| session.task.map(|task| (session.id.as_str(), task)))
            .collect();
        session_tasks.extend(
            facts
                .helpers
                .iter()
                .map(|helper| (helper.session.as_str(), helper.task)),
        );
        let mut by_name: BTreeMap<&'f str, Vec<&'f SessionFact>> = BTreeMap::new();
        let mut task_sessions: BTreeMap<i64, Vec<&'f SessionFact>> = BTreeMap::new();
        for session in &facts.sessions {
            by_name
                .entry(session.rc_name.as_str())
                .or_default()
                .push(session);
            if let Some(task) = session.task.filter(|_| session.role == Role::Worker) {
                task_sessions.entry(task).or_default().push(session);
            }
        }
        let mut draw = Self {
            facts,
            at,
            scope,
            drawn: drawn_tasks(facts, &transitions, at, scope),
            newest,
            session_tasks,
            by_id: facts
                .sessions
                .iter()
                .map(|session| (session.id.as_str(), session))
                .collect(),
            by_name,
            task_sessions,
            agents: facts
                .agents
                .iter()
                .map(|agent| (agent.id.as_str(), agent))
                .collect(),
            sessions: BTreeMap::new(),
            addresses: BTreeMap::new(),
            transitions,
            pr_changes: by_key(&facts.pr_changes),
            job_changes: by_key(&facts.job_changes),
            review_changes: by_key(&facts.review_changes),
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

    /// Points `name` at `place`, unless it already points at a drawn node and `place` is hidden: a name a closed task
    /// once had and a later task took points at the one this view draws.
    fn claim(&mut self, name: &'f str, place: Place) {
        if place == Place::Hidden && matches!(self.addresses.get(name), Some(Place::Drawn(_))) {
            return;
        }
        self.addresses.insert(name, place);
    }

    /// Where every session, helper, task name, orchestrator name and subagent id points.
    fn place(&mut self) {
        let facts = self.facts;
        for task in &facts.tasks {
            let place = if self.is_drawn(task.id) {
                Place::Drawn(task_node(task.id))
            } else {
                Place::Hidden
            };
            self.claim(&task.name, place);
        }
        for session in &facts.sessions {
            let place = if session.role == Role::Orchestrator {
                Place::Drawn(ORCHESTRATOR_NODE.to_owned())
            } else {
                session
                    .task
                    .filter(|task| self.is_drawn(*task))
                    .map_or(Place::Hidden, |task| Place::Drawn(task_node(task)))
            };
            self.sessions.insert(&session.id, place.clone());
            self.claim(&session.rc_name, place);
        }
        for helper in &facts.helpers {
            let place = if self.is_drawn(helper.task) && self.until(&helper.at) {
                Place::Drawn(helper_node(&helper.session))
            } else {
                Place::Hidden
            };
            self.sessions.insert(&helper.session, place);
        }
        for agent in &facts.agents {
            if !self.addresses.contains_key(agent.id.as_str()) {
                self.addresses
                    .insert(&agent.id, Place::Drawn(agent_node(&agent.id)));
            }
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

    /// Where a message address pointed when it was used at `when`: the session that held the name then (running
    /// then, else the newest started by then), else whatever the name points at in this view.
    fn of_address_at(&self, address: &str, when: &str) -> Option<Place> {
        let bare = bare_address(address);
        if is_orchestrator_address(bare) {
            return Some(Place::Drawn(ORCHESTRATOR_NODE.to_owned()));
        }
        let holders = self.by_name.get(bare).map_or(&[][..], Vec::as_slice);
        let started: Vec<&&SessionFact> = holders
            .iter()
            .filter(|session| session.started_at.as_str() <= when)
            .collect();
        let holder = started
            .iter()
            .rev()
            .find(|session| session.ended_at.as_deref().is_none_or(|end| end > when))
            .or_else(|| started.last());
        holder.map_or_else(
            || self.of_address(address),
            |session| {
                Some(
                    self.sessions
                        .get(session.id.as_str())
                        .cloned()
                        .unwrap_or(Place::Hidden),
                )
            },
        )
    }

    fn is_session_name(&self, address: &str) -> bool {
        let bare = bare_address(address);
        is_orchestrator_address(bare) || self.by_name.contains_key(bare)
    }
}

/// The graph at `at` (now when none) for `scope`.
#[must_use]
pub fn build(facts: &Facts, at: Option<&str>, scope: Scope) -> Graph {
    let draw = Draw::new(facts, at, scope);
    let mut canvas = Canvas::default();
    canvas.put(Node {
        id: USER_NODE.to_owned(),
        shape: Shape::User,
        label: "you".to_owned(),
        start: None,
        end: None,
        details: BTreeMap::new(),
        tokens: None,
        badges: BTreeMap::new(),
    });
    orchestrator(&draw, &mut canvas);
    for task in &facts.tasks {
        if let Some(state) = draw.drawn.get(&task.id) {
            task_node_of(&draw, task, *state, &mut canvas);
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
    spend_and_badges(&draw, &mut canvas);
    canvas.finish(at, &facts.skipped)
}

fn drawn_tasks(
    facts: &Facts,
    transitions: &BTreeMap<i64, Vec<&Change<TaskState>>>,
    at: Option<&str>,
    scope: Scope,
) -> BTreeMap<i64, TaskState> {
    let existing: BTreeMap<i64, TaskState> = facts
        .tasks
        .iter()
        .filter(|task| at.is_none_or(|at| task.queued_at.as_str() <= at))
        .map(|task| {
            let state = at.map_or(task.state, |at| {
                let changes = transitions.get(&task.id).map_or(&[][..], Vec::as_slice);
                change_at(changes, at, task.state, task.state)
            });
            (task.id, state)
        })
        .collect();
    let parents: BTreeMap<i64, i64> = facts
        .tasks
        .iter()
        .filter_map(|task| task.parent.map(|parent| (task.id, parent)))
        .collect();
    match scope {
        Scope::All => existing,
        Scope::Live => {
            let live: BTreeSet<i64> = existing
                .iter()
                .filter(|(_, state)| !is_over(**state))
                .map(|(id, _)| *id)
                .collect();
            let leads: BTreeSet<i64> = live
                .iter()
                .filter_map(|id| parents.get(id).copied())
                .collect();
            existing
                .into_iter()
                .filter(|(id, _)| live.contains(id) || leads.contains(id))
                .collect()
        }
        Scope::Task(id) => {
            let lead = parents.get(&id).copied().unwrap_or(id);
            let in_family = |task: i64| parents.get(&task).copied().unwrap_or(task) == lead;
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
        .filter(|session| session.role == Role::Orchestrator && draw.until(&session.started_at))
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
        shape: Shape::Orchestrator(if live.is_empty() {
            SessionState::Ended
        } else {
            SessionState::Live
        }),
        label: newest.map_or_else(
            || ORCHESTRATOR_NODE.to_owned(),
            |session| session.rc_name.clone(),
        ),
        start: sessions
            .iter()
            .map(|session| session.started_at.clone())
            .min(),
        end: None,
        details: details([("sessions", (!listed.is_empty()).then_some(listed))]),
        tokens: None,
        badges: BTreeMap::new(),
    });
}

fn task_node_of(draw: &Draw<'_>, task: &TaskFact, state: TaskState, canvas: &mut Canvas) {
    let id = task_node(task.id);
    let end = draw.transitions.get(&task.id).and_then(|changes| {
        changes
            .iter()
            .filter(|change| is_over(change.to))
            .map(|change| change.at.clone())
            .max()
    });
    let shape = if task.parent.is_some() {
        Shape::Part(state)
    } else if task.lead {
        Shape::Lead(state)
    } else {
        Shape::Worker(state)
    };
    let newest = draw.newest.get(&task.id);
    let earlier: Vec<&str> = draw
        .task_sessions
        .get(&task.id)
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter(|session| newest.is_none_or(|newest| session.id != newest.id))
        .filter(|session| draw.until(&session.started_at))
        .map(|session| session.id.as_str())
        .collect();
    canvas.put(Node {
        id: id.clone(),
        shape,
        label: task.name.clone(),
        start: Some(task.queued_at.clone()),
        end,
        details: details([
            ("project", Some(task.project.clone())),
            ("kind", Some(task.kind.clone())),
            ("model", Some(task.model.as_str().to_owned())),
            (
                "effort",
                task.effort.map(|effort| effort.as_str().to_owned()),
            ),
            ("last_report", task.last_report.clone()),
            ("session", newest.map(|session| session.id.clone())),
            ("name", newest.map(|session| session.rc_name.clone())),
            (
                "earlier_sessions",
                (!earlier.is_empty()).then(|| earlier.join(", ")),
            ),
        ]),
        tokens: None,
        badges: BTreeMap::new(),
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

/// Where a subagent is drawn and in what state, or none when this view leaves it out: its caller is not drawn, it is
/// the orchestrator's in a single-task view, it had not responded yet at the instant (or has no times at all), or,
/// in the live view, it finished or its caller ended.
fn agent_shown<'d>(draw: &'d Draw<'_>, agent: &AgentFact) -> Option<(&'d String, AgentState)> {
    let caller = draw.of_session(&agent.session)?;
    let home = draw.task_box(draw.session_tasks.get(agent.session.as_str()).copied());
    if matches!(draw.scope, Scope::Task(_)) && home.is_none() {
        return None;
    }
    let state = match (draw.at, agent.first_at.as_deref(), agent.last_at.as_deref()) {
        (None, _, _) => match agent.finished {
            Some(true) => AgentState::Done,
            Some(false) => AgentState::Running,
            None => AgentState::Seen,
        },
        (Some(_), None, _) => return None,
        (Some(at), Some(first), _) if first > at => return None,
        (Some(at), Some(_), Some(last)) if last <= at => AgentState::Done,
        (Some(_), Some(_), _) => AgentState::Running,
    };
    if draw.at.is_none() && draw.scope == Scope::Live {
        let caller_ended = draw
            .by_id
            .get(agent.session.as_str())
            .is_some_and(|session| session.ended_at.is_some());
        if state == AgentState::Done || caller_ended {
            return None;
        }
    }
    Some((caller, state))
}

fn subagents(draw: &Draw<'_>, canvas: &mut Canvas) {
    for agent in &draw.facts.agents {
        let Some((caller, state)) = agent_shown(draw, agent) else {
            continue;
        };
        let id = agent_node(&agent.id);
        canvas.put(Node {
            id: id.clone(),
            shape: Shape::Subagent(state),
            label: agent
                .agent_type
                .clone()
                .unwrap_or_else(|| "subagent".to_owned()),
            start: agent.first_at.clone(),
            end: agent.last_at.clone(),
            details: details([
                ("description", agent.description.clone()),
                ("model", agent.model.clone()),
                ("depth", agent.depth.map(|depth| depth.to_string())),
            ]),
            tokens: None,
            badges: BTreeMap::new(),
        });
        let starter = agent
            .parent
            .as_deref()
            .and_then(|parent| draw.agents.get(parent).copied())
            .filter(|parent| parent.session == agent.session && agent_shown(draw, parent).is_some())
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
        let Some(id) = draw.of_session(&helper.session).cloned() else {
            continue;
        };
        canvas.put(Node {
            id: id.clone(),
            shape: Shape::Helper,
            label: "headwork:helper".to_owned(),
            start: Some(helper.at.clone()),
            end: None,
            details: details([("session", Some(helper.session.clone()))]),
            tokens: None,
            badges: BTreeMap::new(),
        });
        canvas.link(
            EdgeKind::Opened,
            &task_node(helper.task),
            &id,
            None,
            Some(&helper.at),
        );
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
            let changes = draw.job_changes.get(&job.id).map_or(&[][..], Vec::as_slice);
            change_at(changes, at, JobState::Running, unlogged)
        });
        let id = format!("job:{}", job.id);
        canvas.put(Node {
            id: id.clone(),
            shape: Shape::Job(state),
            label: job.label.clone(),
            start: Some(job.started_at.clone()),
            end: job.ended_at.clone(),
            details: BTreeMap::new(),
            tokens: None,
            badges: BTreeMap::new(),
        });
        let opener = job
            .session
            .as_deref()
            .and_then(|session| draw.of_session(session))
            .cloned()
            .unwrap_or_else(|| task_node(task));
        canvas.link(EdgeKind::Opened, &opener, &id, None, Some(&job.started_at));
    }
}

fn prs(draw: &Draw<'_>, canvas: &mut Canvas) {
    for pr in &draw.facts.prs {
        if !draw.is_drawn(pr.task) || !draw.until(&pr.first_at) {
            continue;
        }
        let state = draw.at.map_or(pr.state, |at| {
            let changes = draw.pr_changes.get(&pr.id).map_or(&[][..], Vec::as_slice);
            change_at(changes, at, PullState::Open, pr.state)
        });
        let id = format!("pr:{}", pr.id);
        canvas.put(Node {
            id: id.clone(),
            shape: Shape::Pr(state),
            label: format!("#{}", pr.number),
            start: Some(pr.first_at.clone()),
            end: None,
            details: details([("url", Some(pr.url.clone()))]),
            tokens: None,
            badges: BTreeMap::new(),
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
            let changes = draw
                .review_changes
                .get(&review.id)
                .map_or(&[][..], Vec::as_slice);
            change_at(changes, at, ReviewState::Requested, review.state)
        });
        canvas.put(Node {
            id: id.clone(),
            shape: Shape::Review(state),
            label: format!("{} {}", review.flow, review.level.number()),
            start: Some(review.requested_at.clone()),
            end: review.finished_at.clone(),
            details: BTreeMap::new(),
            tokens: None,
            badges: BTreeMap::new(),
        });
        let reviewed = task_node(review.task);
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
                shape: Shape::ReviewNode(state),
                label: if run.item == 0 {
                    run.node.clone()
                } else {
                    format!("{} {}", run.node, run.item)
                },
                start: run.started_at.clone(),
                end: run.finished_at.clone(),
                details: details([("round", Some(run.round.to_string()))]),
                tokens: None,
                badges: BTreeMap::new(),
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

/// Message edges. A message to a session headwork runs is counted from its receiver's `prompt` event, which every
/// managed session logs (also for senders outside headwork and for history before senders logged `to`); the
/// sender's `tool-used` event gives that edge its summary. A message to anything else (a subagent, an unknown name) is
/// counted from the sender's event.
fn messages(draw: &Draw<'_>, canvas: &mut Canvas) {
    for received in draw
        .facts
        .received
        .iter()
        .filter(|received| draw.until(&received.at))
    {
        let Some(to) = draw.of_session(&received.session) else {
            continue;
        };
        let from = match draw.of_address_at(&received.from, &received.at) {
            Some(Place::Drawn(id)) => id,
            Some(Place::Hidden) => continue,
            None => external(canvas, &received.from, &received.at),
        };
        canvas.link(EdgeKind::Message, &from, to, None, Some(&received.at));
    }
    for message in draw
        .facts
        .messages
        .iter()
        .filter(|message| draw.until(&message.at))
    {
        let Some(from) = draw.of_session(&message.from) else {
            continue;
        };
        let managed = draw.is_session_name(&message.to);
        let target = match draw.of_address_at(&message.to, &message.at) {
            Some(Place::Drawn(id)) => id,
            Some(Place::Hidden) => continue,
            None => external(canvas, &message.to, &message.at),
        };
        if managed {
            canvas.label(
                EdgeKind::Message,
                from,
                &target,
                message.summary.clone(),
                Some(&message.at),
            );
        } else {
            canvas.link(
                EdgeKind::Message,
                from,
                &target,
                message.summary.clone(),
                Some(&message.at),
            );
        }
    }
}

/// The `external` node for an address, put on the canvas the first time.
fn external(canvas: &mut Canvas, address: &str, at: &str) -> String {
    let id = external_node(address);
    if !canvas.nodes.contains_key(&id) {
        canvas.put(Node {
            id: id.clone(),
            shape: Shape::External,
            label: bare_address(address).to_owned(),
            start: Some(at.to_owned()),
            end: None,
            details: BTreeMap::new(),
            tokens: None,
            badges: BTreeMap::new(),
        });
    }
    id
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

fn asks(draw: &Draw<'_>, canvas: &mut Canvas) {
    for ask in draw
        .facts
        .asks
        .iter()
        .filter(|ask| draw.until(&ask.created_at))
    {
        let asker = ask.session.as_deref().map_or_else(
            || draw.task_box(ask.task),
            |session| draw.of_session(session).cloned(),
        );
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
            let state = if ask.state.is_waiting() {
                ask.state
            } else {
                AskState::Open
            };
            canvas.put(Node {
                id: id.clone(),
                shape: Shape::Ask(state),
                label: label.clone(),
                start: Some(ask.created_at.clone()),
                end: ask.closed_at.clone(),
                details: details([("question", Some(ask.question.clone()))]),
                tokens: None,
                badges: BTreeMap::new(),
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

fn count(number: usize) -> u32 {
    u32::try_from(number).unwrap_or(u32::MAX)
}

/// Tokens (in the view of now only: summed days cannot be cut at an instant) and the counts drawn on nodes.
fn spend_and_badges(draw: &Draw<'_>, canvas: &mut Canvas) {
    let spend = &draw.facts.spend;
    let open_asks = canvas
        .nodes
        .values()
        .filter(|node| node.kind() == super::NodeKind::Ask)
        .count();
    for node in canvas.nodes.values_mut() {
        let id = node.id.clone();
        if let Some(task) = id
            .strip_prefix("task:")
            .and_then(|task| task.parse::<i64>().ok())
        {
            let open = draw
                .facts
                .findings
                .iter()
                .filter(|finding| finding.task == task)
                .filter(|finding| {
                    draw.at.map_or_else(
                        || finding.resolved_at.is_none(),
                        |at| {
                            finding.first_at.as_str() <= at
                                && finding.resolved_at.as_deref().is_none_or(|end| end > at)
                        },
                    )
                })
                .count();
            if draw
                .facts
                .tasks
                .iter()
                .any(|fact| fact.id == task && fact.close == crate::CloseMode::Silent)
            {
                node.badges.insert("silent".to_owned(), 1);
            }
            if open > 0 {
                node.badges.insert("findings".to_owned(), count(open));
            }
            if draw.at.is_none() {
                node.tokens = spend.tasks.get(&task).copied();
            }
        } else if id == ORCHESTRATOR_NODE {
            if draw.at.is_none() {
                let total: u64 = draw
                    .facts
                    .sessions
                    .iter()
                    .filter(|session| session.role == Role::Orchestrator)
                    .filter_map(|session| spend.sessions.get(&session.id))
                    .sum();
                node.tokens = (total > 0).then_some(total);
            }
        } else if let Some(agent) = id.strip_prefix("agent:") {
            if draw.at.is_none() {
                node.tokens = spend.agents.get(agent).copied();
            }
        } else if id == USER_NODE {
            if open_asks > 0 {
                node.badges.insert("asks".to_owned(), count(open_asks));
            }
        } else if let Some(review) = id
            .strip_prefix("review:")
            .and_then(|review| review.parse::<i64>().ok())
        {
            let found: Vec<&super::ReviewFindingFact> = draw
                .facts
                .review_findings
                .iter()
                .filter(|finding| finding.review == review)
                .collect();
            finding_badges(node, &found);
        } else if let Some(rest) = id.strip_prefix("review-node:") {
            let mut parts = rest.splitn(4, ':');
            let (review, round, name) = (parts.next(), parts.next(), parts.next());
            let found: Vec<&super::ReviewFindingFact> = draw
                .facts
                .review_findings
                .iter()
                .filter(|finding| {
                    review == Some(finding.review.to_string().as_str())
                        && round == Some(finding.round.to_string().as_str())
                        && name == Some(finding.node.as_str())
                })
                .collect();
            finding_badges(node, &found);
        }
    }
}

/// A review's or flow node's finding counts: all, those still open between the leads (a finding the verify stage
/// rejected never reached them), and by severity in details.
fn finding_badges(node: &mut Node, found: &[&super::ReviewFindingFact]) {
    if found.is_empty() {
        return;
    }
    node.badges
        .insert("findings".to_owned(), count(found.len()));
    let open = found
        .iter()
        .filter(|finding| finding.verdict != Some(FindingVerdict::Rejected))
        .filter(|finding| {
            finding
                .resolution
                .is_none_or(|resolution| !resolution.is_settled())
        })
        .count();
    node.badges.insert("open".to_owned(), count(open));
    let mut by_severity: BTreeMap<&str, usize> = BTreeMap::new();
    for finding in found {
        *by_severity.entry(finding.severity.as_str()).or_default() += 1;
    }
    let summary = by_severity
        .iter()
        .map(|(severity, number)| format!("{number} {severity}"))
        .collect::<Vec<_>>()
        .join(", ");
    node.details.insert("findings".to_owned(), summary);
}
