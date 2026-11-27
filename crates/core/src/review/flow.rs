//! Flows: named graphs of review nodes, read from TOML and validated (`docs/review-flows.md`, section 5).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use super::Level;
use crate::vocabulary::Model;

const MAX_NAME: usize = 40;

/// Why a flow or one of its names is not valid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FlowError {
    /// The TOML does not parse or does not match the model.
    #[error("{0}")]
    Parse(String),
    /// A flow or node name breaks the naming rule.
    #[error(
        "`{0}` is not a valid name: a lower-case letter, then lower-case letters, digits or `-`, at most 40"
    )]
    BadName(String),
    /// The `levels` table lacks a level or names one that is not 1, 2 or 3.
    #[error("levels must define exactly 1, 2 and 3; {0}")]
    Levels(String),
    /// A level's `rounds` is out of range.
    #[error("level {level}: rounds must be 1, 2 or 3, not {rounds}")]
    Rounds {
        /// The level.
        level: Level,
        /// The value found.
        rounds: u8,
    },
    /// No node runs at level 1.
    #[error("no node runs at level 1 (every flow needs a node with from_level = 1)")]
    NoNodes,
    /// Two nodes share an id.
    #[error("node `{0}` is defined twice")]
    DuplicateNode(String),
    /// A node's `from_level` is not a level.
    #[error("node `{node}`: from_level {value} is not 1, 2 or 3")]
    FromLevel {
        /// The node.
        node: String,
        /// The value found.
        value: u8,
    },
    /// A node's `after` names a node that does not exist, or itself.
    #[error("node `{node}`: after names `{target}`, which is not another node of this flow")]
    UnknownAfter {
        /// The node.
        node: String,
        /// The missing name.
        target: String,
    },
    /// The `after` edges make a cycle.
    #[error("the after edges make a cycle through `{0}`")]
    Cycle(String),
    /// An `each` node has no inputs to fan out over.
    #[error("node `{0}`: each needs after, the nodes whose findings it fans out over")]
    EachWithoutAfter(String),
    /// A per-level `each` map lacks a level the node runs at.
    #[error("node `{node}`: each names no fan-out for level {level}, at which it runs")]
    EachMissing {
        /// The node.
        node: String,
        /// The level.
        level: Level,
    },
    /// An `each` map key is not a level.
    #[error("node `{node}`: each key `{key}` is not 1, 2 or 3")]
    EachKey {
        /// The node.
        node: String,
        /// The key found.
        key: String,
    },
    /// A per-level model map lacks a level the node runs at.
    #[error("node `{node}`: model names no model for level {level}, at which it runs")]
    ModelMissing {
        /// The node.
        node: String,
        /// The level.
        level: Level,
    },
    /// A model map key is not a level.
    #[error("node `{node}`: model key `{key}` is not 1, 2 or 3")]
    ModelKey {
        /// The node.
        node: String,
        /// The key found.
        key: String,
    },
    /// A node's prompt file is missing.
    #[error("node `{node}`: prompt `{prompt}` is missing")]
    PromptMissing {
        /// The node.
        node: String,
        /// The prompt path.
        prompt: String,
    },
    /// A node runs on a model `[models] allowed` does not list.
    #[error("node `{node}`: model `{model}` at level {level} is not in models.allowed")]
    ModelNotAllowed {
        /// The node.
        node: String,
        /// The model.
        model: Model,
        /// The level.
        level: Level,
    },
}

fn valid_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    name.len() <= MAX_NAME
        && bytes.next().is_some_and(|first| first.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

macro_rules! flow_word {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// The name, checked against the naming rule.
            ///
            /// # Errors
            ///
            /// Returns [`FlowError::BadName`] for anything but a lower-case letter followed by lower-case letters,
            /// digits or `-`, at most 40 in all.
            pub fn new(name: &str) -> Result<Self, FlowError> {
                if valid_name(name) {
                    Ok(Self(name.to_owned()))
                } else {
                    Err(FlowError::BadName(name.to_owned()))
                }
            }

            /// The name as text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.pad(&self.0)
            }
        }
    };
}

flow_word!(
    /// A flow's name, such as `code` or `plan`.
    FlowName
);

flow_word!(
    /// A node's id within its flow, such as `correctness`.
    NodeId
);

/// What a flow reviews.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    /// The task branch's diff, base to head.
    Diff,
    /// The paths named in the focus or the brief.
    Files,
}

/// What a dispute needs to be accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DisputeRule {
    /// Any reason.
    Reason,
    /// Evidence: a test, a code reference, a doc line.
    Evidence,
}

/// What happens to a dispute the reviewer lead does not accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Unsettled {
    /// The dispute is accepted anyway.
    Accept,
    /// The dispute goes to the user.
    Ask,
}

/// What a level means in one flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevelSpec {
    /// The pushback text, part of every node's prompt at this level.
    pub pushback: String,
    /// What a dispute needs.
    pub dispute: DisputeRule,
    /// What happens to a dispute the reviewer lead does not accept.
    pub unsettled: Unsettled,
    /// Full rounds: 2 runs the flow again on the fixed commit.
    pub rounds: u8,
    /// The reviewer lead argues back once before it lets a dispute go further.
    #[serde(default)]
    pub argue_back: bool,
}

/// How a node fans out over the findings of its inputs, each group formed from one completed input node's findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Each {
    /// One subagent per finding; each returns a verdict on its finding.
    Finding,
    /// One subagent per input node, checking all that node's findings.
    Node,
    /// One subagent per input node and file, checking that node's findings in that file; findings that name no file
    /// form one group per node.
    File,
}

/// How a node fans out at each level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum EachChoice {
    /// The same at every level.
    One(Each),
    /// One per level.
    PerLevel(BTreeMap<Level, Each>),
}

/// The nodes a node waits for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum After {
    /// These nodes, where they run at the level.
    Nodes(Vec<NodeId>),
    /// Every node declared before this one that runs at the level (`after = ["*"]`).
    Earlier,
}

/// Which model a node runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ModelChoice {
    /// The same at every level.
    One(Model),
    /// One per level.
    PerLevel(BTreeMap<Level, Model>),
}

/// One reviewer of a flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Node {
    /// Its id.
    pub id: NodeId,
    /// Its prompt file, relative to the flows folder.
    pub prompt: String,
    /// The lowest level it runs at.
    pub from_level: Level,
    /// Its model.
    pub model: ModelChoice,
    /// What it waits for.
    pub after: After,
    /// How it fans out, if it does.
    pub each: Option<EachChoice>,
}

impl Node {
    /// Whether it runs at a level.
    #[must_use]
    pub fn runs_at(&self, level: Level) -> bool {
        self.from_level <= level
    }

    /// How it fans out at a level, if it does; validation makes sure a per-level map names every level it runs at.
    #[must_use]
    pub fn each_at(&self, level: Level) -> Option<Each> {
        match self.each.as_ref()? {
            EachChoice::One(each) => Some(*each),
            EachChoice::PerLevel(map) => map.get(&level).copied(),
        }
    }

    /// Whether it fans out over its inputs' findings instead of running once.
    #[must_use]
    pub const fn fans_out(&self) -> bool {
        self.each.is_some()
    }

    /// Its model at a level; validation makes sure every level it runs at has one.
    #[must_use]
    pub fn model_at(&self, level: Level) -> Option<Model> {
        match &self.model {
            ModelChoice::One(model) => Some(*model),
            ModelChoice::PerLevel(models) => models.get(&level).copied(),
        }
    }
}

/// A validated flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Flow {
    name: FlowName,
    description: String,
    target: Target,
    levels: [LevelSpec; 3],
    nodes: Vec<Node>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFlow {
    name: String,
    description: String,
    target: Target,
    levels: BTreeMap<String, LevelSpec>,
    #[serde(rename = "node", default)]
    nodes: Vec<RawNode>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNode {
    id: String,
    prompt: String,
    #[serde(default = "first_level")]
    from_level: u8,
    model: RawModel,
    #[serde(default)]
    after: Vec<String>,
    each: Option<RawEach>,
}

const fn first_level() -> u8 {
    1
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawEach {
    One(Each),
    PerLevel(BTreeMap<String, Each>),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawModel {
    One(Model),
    PerLevel(BTreeMap<String, Model>),
}

impl Flow {
    /// Reads and validates a flow from its TOML text.
    ///
    /// # Errors
    ///
    /// Returns the first [`FlowError`] found: a parse error, a bad name, missing levels, duplicate or unknown nodes,
    /// a cycle, an `each` without inputs, or a model or `each` map that misses a level the node runs at.
    pub fn parse(text: &str) -> Result<Self, FlowError> {
        let raw: RawFlow =
            toml::from_str(text).map_err(|error| FlowError::Parse(error.to_string()))?;
        let name = FlowName::new(&raw.name)?;
        let levels = levels(raw.levels)?;
        let ids = node_ids(&raw.nodes)?;
        let nodes = raw
            .nodes
            .into_iter()
            .map(|node| build_node(node, &ids))
            .collect::<Result<Vec<_>, _>>()?;
        if !nodes.iter().any(|node| node.runs_at(Level::Light)) {
            return Err(FlowError::NoNodes);
        }
        let flow = Self {
            name,
            description: raw.description,
            target: raw.target,
            levels,
            nodes,
        };
        flow.check_acyclic()?;
        Ok(flow)
    }

    /// The flow's name.
    #[must_use]
    pub const fn name(&self) -> &FlowName {
        &self.name
    }

    /// What the flow is for.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// What it reviews.
    #[must_use]
    pub const fn target(&self) -> Target {
        self.target
    }

    /// What a level means here.
    #[must_use]
    pub const fn level(&self, level: Level) -> &LevelSpec {
        match level {
            Level::Light => &self.levels[0],
            Level::Standard => &self.levels[1],
            Level::Strict => &self.levels[2],
        }
    }

    /// Every node, in declaration order.
    #[must_use]
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// A node by id.
    #[must_use]
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|node| node.id.as_str() == id)
    }

    /// The nodes that run at a level, in declaration order.
    pub fn nodes_at(&self, level: Level) -> impl Iterator<Item = &Node> {
        self.nodes.iter().filter(move |node| node.runs_at(level))
    }

    /// The nodes a node waits for at a level: its `after` edges to nodes that run there.
    #[must_use]
    pub fn inputs(&self, node: &Node, level: Level) -> Vec<&Node> {
        match &node.after {
            After::Nodes(ids) => ids
                .iter()
                .filter_map(|id| self.node(id.as_str()))
                .filter(|input| input.runs_at(level))
                .collect(),
            After::Earlier => self
                .nodes
                .iter()
                .take_while(|earlier| earlier.id != node.id)
                .filter(|earlier| earlier.runs_at(level))
                .collect(),
        }
    }

    /// The problems only the surroundings can tell: prompts that are missing and models `[models] allowed` refuses.
    #[must_use]
    pub fn check(&self, prompt_exists: impl Fn(&str) -> bool, allowed: &[Model]) -> Vec<FlowError> {
        let mut problems = Vec::new();
        for node in &self.nodes {
            if !prompt_exists(&node.prompt) {
                problems.push(FlowError::PromptMissing {
                    node: node.id.to_string(),
                    prompt: node.prompt.clone(),
                });
            }
            for level in Level::ALL.into_iter().filter(|level| node.runs_at(*level)) {
                if let Some(model) = node
                    .model_at(level)
                    .filter(|model| !allowed.contains(model))
                {
                    problems.push(FlowError::ModelNotAllowed {
                        node: node.id.to_string(),
                        model,
                        level,
                    });
                }
            }
        }
        problems
    }

    fn check_acyclic(&self) -> Result<(), FlowError> {
        let mut done = BTreeSet::new();
        for node in &self.nodes {
            let mut path = Vec::new();
            self.visit(node, &mut path, &mut done)?;
        }
        Ok(())
    }

    fn visit<'a>(
        &'a self,
        node: &'a Node,
        path: &mut Vec<&'a NodeId>,
        done: &mut BTreeSet<&'a NodeId>,
    ) -> Result<(), FlowError> {
        if done.contains(&node.id) {
            return Ok(());
        }
        if path.contains(&&node.id) {
            return Err(FlowError::Cycle(node.id.to_string()));
        }
        path.push(&node.id);
        for input in self.inputs(node, Level::Strict) {
            self.visit(input, path, done)?;
        }
        path.pop();
        done.insert(&node.id);
        Ok(())
    }
}

fn levels(mut raw: BTreeMap<String, LevelSpec>) -> Result<[LevelSpec; 3], FlowError> {
    let mut take = |level: Level| {
        raw.remove(&level.to_string())
            .ok_or_else(|| FlowError::Levels(format!("level {level} is missing")))
            .and_then(|spec| {
                if (1..=3).contains(&spec.rounds) {
                    Ok(spec)
                } else {
                    Err(FlowError::Rounds {
                        level,
                        rounds: spec.rounds,
                    })
                }
            })
    };
    let specs = [
        take(Level::Light)?,
        take(Level::Standard)?,
        take(Level::Strict)?,
    ];
    if let Some(extra) = raw.keys().next() {
        return Err(FlowError::Levels(format!("`{extra}` is not a level")));
    }
    Ok(specs)
}

fn node_ids(nodes: &[RawNode]) -> Result<BTreeSet<String>, FlowError> {
    let mut ids = BTreeSet::new();
    for node in nodes {
        NodeId::new(&node.id)?;
        if !ids.insert(node.id.clone()) {
            return Err(FlowError::DuplicateNode(node.id.clone()));
        }
    }
    Ok(ids)
}

fn build_node(raw: RawNode, ids: &BTreeSet<String>) -> Result<Node, FlowError> {
    let id = NodeId::new(&raw.id)?;
    let from_level = Level::from_number(raw.from_level).ok_or_else(|| FlowError::FromLevel {
        node: raw.id.clone(),
        value: raw.from_level,
    })?;
    let after = if raw.after.iter().any(|target| target == "*") {
        After::Earlier
    } else {
        let targets = raw
            .after
            .iter()
            .map(|target| {
                if target == &raw.id || !ids.contains(target) {
                    Err(FlowError::UnknownAfter {
                        node: raw.id.clone(),
                        target: target.clone(),
                    })
                } else {
                    NodeId::new(target)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        After::Nodes(targets)
    };
    if raw.each.is_some() && after == After::Nodes(Vec::new()) {
        return Err(FlowError::EachWithoutAfter(raw.id));
    }
    let each = match raw.each {
        None => None,
        Some(RawEach::One(each)) => Some(EachChoice::One(each)),
        Some(RawEach::PerLevel(map)) => Some(EachChoice::PerLevel(per_level(
            map,
            from_level,
            |key| FlowError::EachKey {
                node: raw.id.clone(),
                key,
            },
            |level| FlowError::EachMissing {
                node: raw.id.clone(),
                level,
            },
        )?)),
    };
    let model = match raw.model {
        RawModel::One(model) => ModelChoice::One(model),
        RawModel::PerLevel(map) => ModelChoice::PerLevel(per_level(
            map,
            from_level,
            |key| FlowError::ModelKey {
                node: raw.id.clone(),
                key,
            },
            |level| FlowError::ModelMissing {
                node: raw.id.clone(),
                level,
            },
        )?),
    };
    Ok(Node {
        id,
        prompt: raw.prompt,
        from_level,
        model,
        after,
        each,
    })
}

/// A per-level map keyed by level numbers, refused when a key is not a level or a level from `from_level` on is missing.
fn per_level<T>(
    map: BTreeMap<String, T>,
    from_level: Level,
    bad_key: impl Fn(String) -> FlowError,
    missing: impl Fn(Level) -> FlowError,
) -> Result<BTreeMap<Level, T>, FlowError> {
    let mut values = BTreeMap::new();
    for (key, value) in map {
        let level = key
            .parse::<u8>()
            .ok()
            .and_then(Level::from_number)
            .ok_or_else(|| bad_key(key.clone()))?;
        values.insert(level, value);
    }
    if let Some(level) = Level::ALL
        .into_iter()
        .find(|level| *level >= from_level && !values.contains_key(level))
    {
        return Err(missing(level));
    }
    Ok(values)
}
