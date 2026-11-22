//! The exact prompt a node's fresh subagent gets (`docs/review-flows.md`, section 6): the node's own prompt, the level's
//! pushback, the target, the focus, and for an `each` node its one finding. Nothing else goes in, so reviewers never
//! anchor on each other or on the reviewer lead.

use std::fmt::Write as _;

use serde::Serialize;

use super::Level;
use super::flow::{Each, LevelSpec, Node, Target};

/// What a review looks at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewTarget {
    /// What the flow reviews.
    pub kind: Target,
    /// The commit the diff starts at.
    pub base: String,
    /// The commit under review.
    pub head: String,
    /// The read-only worktree checked out at `head`.
    pub worktree: String,
    /// The files the diff changes.
    pub changed: Vec<String>,
}

/// The finding an `each` node checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FindingText {
    /// The finding's number.
    pub id: i64,
    /// The node that reported it.
    pub node: String,
    /// The file and line, as `path:line`, when it names them.
    pub place: Option<String>,
    /// How much it weighs.
    pub severity: String,
    /// What is wrong.
    pub summary: String,
    /// The failing scenario.
    pub scenario: String,
}

/// What a plain node returns.
pub const FINDINGS_SHAPE: &str = "Answer with a JSON array and nothing else, one object per finding: \
     [{\"file\": \"src/x.rs\", \"line\": 12, \"severity\": \"important\", \"summary\": \"what is wrong\", \
     \"scenario\": \"the concrete inputs or state and the wrong result\"}]. `severity` is `important` (a defect), `nit` \
     (a minor issue) or `doubt` (about the approach or the scope); `file` and `line` may be null when the finding names \
     no place. Answer [] when you find nothing.";

/// What an `each` node returns.
pub const VERDICT_SHAPE: &str = "Answer with a JSON object and nothing else: {\"verdict\": \"confirmed\", \"evidence\": \
     \"the lines you read and what they show\"}. `verdict` is `confirmed`, `plausible` or `rejected`.";

/// The prompt for one node instance at a level; `finding` is the item of an `each` node.
#[must_use]
pub fn compose(
    node: &Node,
    node_prompt: &str,
    level: Level,
    spec: &LevelSpec,
    target: &ReviewTarget,
    focus: &str,
    finding: Option<&FindingText>,
) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "{}\n", node_prompt.trim_end());
    let _ = writeln!(
        text,
        "## How hard to push (level {level})\n\n{}\n",
        spec.pushback.trim()
    );
    let _ = writeln!(text, "## What you review\n");
    match target.kind {
        Target::Diff => {
            let _ = writeln!(
                text,
                "The change is the diff `{base}..{head}`. Read it with `git -C {tree} diff {base} {head}` and the code \
                 around it in the worktree `{tree}`, checked out at `{head}`.",
                base = target.base,
                head = target.head,
                tree = target.worktree,
            );
        }
        Target::Files => {
            let _ = writeln!(
                text,
                "Review the files the focus names, in the worktree `{}`, checked out at `{}`.",
                target.worktree, target.head
            );
        }
    }
    if !target.changed.is_empty() {
        let _ = writeln!(text, "\nChanged files:\n");
        for file in &target.changed {
            let _ = writeln!(text, "- {file}");
        }
    }
    if !focus.trim().is_empty() {
        let _ = writeln!(text, "\nThe user asked to look most at: {}", focus.trim());
    }
    let _ = writeln!(
        text,
        "\nYou only read: never edit, commit, check out, build or run tests; use Read, Grep, Glob and read-only git \
         commands in the worktree."
    );
    match (node.each, finding) {
        (Some(Each::Finding), Some(finding)) => {
            let _ = writeln!(text, "\n## The finding to check (number {})\n", finding.id);
            let _ = writeln!(text, "- Reported by: {}", finding.node);
            if let Some(place) = &finding.place {
                let _ = writeln!(text, "- Place: {place}");
            }
            let _ = writeln!(text, "- Severity: {}", finding.severity);
            let _ = writeln!(text, "- Summary: {}", finding.summary);
            let _ = writeln!(text, "- Scenario: {}", finding.scenario);
            let _ = writeln!(text, "\n## Your answer\n\n{VERDICT_SHAPE}");
        }
        _ => {
            let _ = writeln!(text, "\n## Your answer\n\n{FINDINGS_SHAPE}");
        }
    }
    text
}
