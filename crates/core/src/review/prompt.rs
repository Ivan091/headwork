//! The exact prompt a node's fresh subagent gets (`docs/review-flows.md`, section 6): the node's own prompt, the level's
//! pushback, the target, the focus, and for an `each` node the findings of its group. Nothing else goes in, so reviewers
//! never anchor on each other or on the reviewer lead.

use std::fmt::Write as _;

use serde::Serialize;

use super::Level;
use super::flow::{LevelSpec, Node, Target};

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
    /// A later round: `base` is the head the previous round reviewed, and the nodes check only the fix commits since.
    pub fixes_only: bool,
}

/// A finding an `each` node checks.
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

/// What an `each` node returns for several findings.
pub const VERDICTS_SHAPE: &str = "Answer with a JSON array and nothing else, one object for every finding above: \
     [{\"id\": 12, \"verdict\": \"confirmed\", \"evidence\": \"the lines you read and what they show\"}]. `id` is the \
     finding's number; `verdict` is `confirmed`, `plausible` or `rejected`. Judge each finding on its own: one being \
     right or wrong says nothing about another.";

/// What an `each` node returns for one finding.
pub const VERDICT_SHAPE: &str = "Answer with a JSON object and nothing else: {\"verdict\": \"confirmed\", \"evidence\": \
     \"the lines you read and what they show\"}. `verdict` is `confirmed`, `plausible` or `rejected`.";

/// The prompt for one node instance at a level; `findings` is the group of an `each` node, empty for a plain node.
#[must_use]
pub fn compose(
    node: &Node,
    node_prompt: &str,
    level: Level,
    spec: &LevelSpec,
    target: &ReviewTarget,
    focus: &str,
    findings: &[FindingText],
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
    if target.fixes_only {
        let _ = writeln!(
            text,
            "\nThis round checks only the fix commits `{base}..{head}` (`git -C {tree} diff {base} {head}`): review the \
             lines they changed and the direct callers of what they changed (find the callers with Grep for the changed \
             functions and types), not the rest of the branch, which the first round already passed.",
            base = target.base,
            head = target.head,
            tree = target.worktree,
        );
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
    match findings {
        [finding] if node.fans_out() => {
            let _ = writeln!(text, "\n## The finding to check (number {})\n", finding.id);
            write_finding(&mut text, finding);
            let _ = writeln!(text, "\n## Your answer\n\n{VERDICT_SHAPE}");
        }
        [_, _, ..] if node.fans_out() => {
            let numbers: Vec<String> = findings
                .iter()
                .map(|finding| finding.id.to_string())
                .collect();
            let _ = writeln!(
                text,
                "\n## The findings to check (numbers {})",
                numbers.join(", ")
            );
            for finding in findings {
                let _ = writeln!(text, "\n### Finding {}\n", finding.id);
                write_finding(&mut text, finding);
            }
            let _ = writeln!(text, "\n## Your answer\n\n{VERDICTS_SHAPE}");
        }
        _ => {
            let _ = writeln!(text, "\n## Your answer\n\n{FINDINGS_SHAPE}");
        }
    }
    text
}

fn write_finding(text: &mut String, finding: &FindingText) {
    let _ = writeln!(text, "- Reported by: {}", finding.node);
    if let Some(place) = &finding.place {
        let _ = writeln!(text, "- Place: {place}");
    }
    let _ = writeln!(text, "- Severity: {}", finding.severity);
    let _ = writeln!(text, "- Summary: {}", finding.summary);
    let _ = writeln!(text, "- Scenario: {}", finding.scenario);
}
