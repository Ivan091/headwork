//! The done check of a project (`[project.<name>] done_check`, `docs/video-flow.md`): a command that must accept the
//! scenes a task names before its `done` passes.

use std::process::Command;
use std::time::Duration;

use anyhow::{Result, bail};
use headwork_adapters::timed;
use headwork_core::video_flow::{DoneVerdict, done_verdict};
use headwork_store::Task;
use serde_json::Value;

use super::Env;

const TIMEOUT: Duration = Duration::from_secs(60);

/// What `done` does with the check.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// No check applies to this task, or every scene is acknowledged.
    Pass,
    /// The check could not tell: `done` goes on and the report says why.
    Unsure(String),
}

/// The scenes a `done` call names.
fn scenes(args: &Value) -> Vec<String> {
    args.get("scenes")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Runs the project's done check for `task` when one is configured for its kind.
///
/// # Errors
///
/// Refuses (`done-check-scenes`) a task the check applies to that names no scene, and (`done-check-waiting`) one whose
/// scenes wait; the message lists them, one per line, for the lead's `Close` ask.
pub fn run(env: &Env<'_>, task: &Task, args: &Value) -> Result<Outcome> {
    let Ok(project) = env.project(task) else {
        return Ok(Outcome::Pass);
    };
    let Some(check) = &project.done_check else {
        return Ok(Outcome::Pass);
    };
    if !check.kinds.contains(&task.kind) {
        return Ok(Outcome::Pass);
    }
    let scenes = scenes(args);
    if scenes.is_empty() {
        bail!(
            "done-check-scenes: this {} task must name its scenes: call `done` with `scenes` (the scene ids it made)",
            task.kind
        );
    }
    let Some((program, leading)) = check.command.split_first() else {
        return Ok(Outcome::Unsure(
            "the done check has an empty command".to_owned(),
        ));
    };
    let mut command = Command::new(program);
    command.args(leading).args(&scenes);
    let verdict = match timed::run(&mut command, TIMEOUT) {
        Ok(ran) => done_verdict(ran.status.code(), &ran.stdout, &ran.stderr),
        Err(error) => DoneVerdict::Unknown(error.to_string()),
    };
    match verdict {
        DoneVerdict::Acknowledged => Ok(Outcome::Pass),
        DoneVerdict::Unknown(why) => Ok(Outcome::Unsure(why)),
        DoneVerdict::Waiting(lines) => bail!(
            "done-check-waiting: these scenes have not passed the human gate yet; put the lines in your `Close` ask and \
             wait:\n{}",
            lines.join("\n")
        ),
    }
}
