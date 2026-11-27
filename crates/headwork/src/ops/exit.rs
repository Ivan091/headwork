//! The I/O side of the exit ladder ([`headwork_core::exit`]), for several sessions at once.
//!
//! Each step is typed into every pane, then their `claude` are waited for together, so a park's time is bounded by
//! the ladder, not by how many sessions it ends.
//!
//! Every end path that may meet a live `claude` (a close, a self-close's detached closer, a park, the tick, a resume
//! closing a restored pane) calls [`ask`] before it ends a process tree or closes a pane; the kill comes only after the
//! last step. Events: `exit-asked` (`attempt`, `step`, `mode`, `grace_s`), `exit-ignored` and `exit-unasked`.

use std::time::Duration;

use anyhow::Result;
use headwork_adapters::host::{HostError, Key, SurfaceId, TerminalHost};
use headwork_core::exit::{EXIT_COMMAND, ExitInput, ExitMode, ExitStep, ladder};
use headwork_store::{SessionRow, Store, TaskId};

use super::close::wait_until;
use super::{Env, session_procs};

/// How long `claude` gets to settle after Escape: an interrupted turn needs a moment to stop, and a key typed right
/// after Escape reads as Alt and that key.
const SETTLE: Duration = Duration::from_secs(1);

/// The pause between the two Ctrl+C: short, since Claude Code exits only on a second one that follows soon.
const BEAT: Duration = Duration::from_millis(300);

/// One session to ask: where to type, and how to tell it left.
pub struct ExitTarget<'a> {
    /// The task it works on, absent for the orchestrator or a session no task owns.
    pub task: Option<TaskId>,
    /// The Claude session id the events are logged under.
    pub session: String,
    /// The pane it runs in.
    pub surface: SurfaceId,
    /// Whether its `claude` is gone.
    pub gone: Box<dyn Fn() -> bool + 'a>,
}

impl std::fmt::Debug for ExitTarget<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExitTarget")
            .field("task", &self.task)
            .field("session", &self.session)
            .field("surface", &self.surface)
            .finish_non_exhaustive()
    }
}

impl ExitTarget<'static> {
    /// A session whose recorded `claude` still runs and whose pane is known; `None` otherwise, since there is no one to
    /// ask or nowhere to type.
    #[must_use]
    pub fn of_session(task: Option<TaskId>, session: &SessionRow) -> Option<Self> {
        let claude = session.claude().filter(session_procs::runs)?;
        let surface = SurfaceId(session.host_surface.clone()?);
        Some(Self {
            task,
            session: session.id.clone(),
            surface,
            gone: Box::new(move || !session_procs::runs(&claude)),
        })
    }
}

/// How one session ended its ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked {
    /// Its `claude` left on its own (or was already gone).
    Left,
    /// It stayed through every step, or asking is off: the caller ends it.
    Stayed,
    /// The host refused the keys: the caller ends it.
    Unasked,
}

impl Asked {
    /// Whether `claude` left on its own.
    #[must_use]
    pub const fn left(self) -> bool {
        matches!(self, Self::Left)
    }
}

/// Climbs `mode`'s ladder for every target at once, one step into every pane whose `claude` still runs.
///
/// After each step it waits up to `[watch] exit_grace_s` for all of them. Returns one outcome per target, in order.
///
/// # Errors
///
/// Fails when the store fails.
pub fn ask(
    store: &Store,
    env: &Env<'_>,
    mode: ExitMode,
    targets: &[ExitTarget<'_>],
) -> Result<Vec<Asked>> {
    let grace = env.config.watch.exit_grace_s;
    let mut outcome: Vec<Asked> = targets
        .iter()
        .map(|target| {
            if (target.gone)() {
                Asked::Left
            } else {
                Asked::Stayed
            }
        })
        .collect();
    for (number, step) in ladder(mode, grace).iter().enumerate() {
        let rung = Rung {
            mode,
            step: *step,
            attempt: number + 1,
            grace,
        };
        if !outcome.contains(&Asked::Stayed) {
            break;
        }
        type_step(store, env, &rung, targets, &mut outcome)?;
        let waiting: Vec<usize> = (0..targets.len())
            .filter(|&index| outcome.get(index) == Some(&Asked::Stayed))
            .collect();
        for &index in &waiting {
            if let Some(target) = targets.get(index) {
                rung.log(store, env, target, "exit-asked", serde_json::json!({}))?;
            }
        }
        wait_until(Duration::from_secs(u64::from(grace)), || {
            let mut all = true;
            for &index in &waiting {
                let (Some(target), Some(slot)) = (targets.get(index), outcome.get_mut(index))
                else {
                    continue;
                };
                if *slot == Asked::Stayed && (target.gone)() {
                    *slot = Asked::Left;
                }
                all &= *slot != Asked::Stayed;
            }
            all
        });
        for &index in &waiting {
            if let (Some(target), Some(Asked::Stayed)) = (targets.get(index), outcome.get(index)) {
                rung.log(store, env, target, "exit-ignored", serde_json::json!({}))?;
            }
        }
    }
    Ok(outcome)
}

/// One step of one ladder, as the events record it.
#[derive(Debug, Clone, Copy)]
struct Rung {
    mode: ExitMode,
    step: ExitStep,
    attempt: usize,
    grace: u32,
}

impl Rung {
    fn log(
        &self,
        store: &Store,
        env: &Env<'_>,
        target: &ExitTarget<'_>,
        kind: &str,
        mut data: serde_json::Value,
    ) -> Result<()> {
        data["grace_s"] = self.grace.into();
        data["attempt"] = self.attempt.into();
        data["step"] = self.step.as_str().into();
        data["mode"] = self.mode.as_str().into();
        store.append_event(env.actor, target.task, Some(&target.session), kind, &data)?;
        Ok(())
    }
}

/// Types one step into every pane still asked, input by input, so the pauses are taken once for all of them. A pane
/// whose host refuses a key is `Unasked` from then on.
fn type_step(
    store: &Store,
    env: &Env<'_>,
    rung: &Rung,
    targets: &[ExitTarget<'_>],
    outcome: &mut [Asked],
) -> Result<()> {
    for input in rung.step.inputs() {
        match input {
            ExitInput::Settle => std::thread::sleep(SETTLE),
            ExitInput::Beat => std::thread::sleep(BEAT),
            ExitInput::Escape
            | ExitInput::ClearLine
            | ExitInput::Command
            | ExitInput::Enter
            | ExitInput::Interrupt => {
                for (target, slot) in targets.iter().zip(outcome.iter_mut()) {
                    if *slot != Asked::Stayed {
                        continue;
                    }
                    if let Err(error) = type_input(env.host, &target.surface, *input) {
                        *slot = Asked::Unasked;
                        let data = serde_json::json!({ "error": error.to_string() });
                        rung.log(store, env, target, "exit-unasked", data)?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// [`ask`] for one target.
///
/// # Errors
///
/// Fails when the store fails.
pub fn ask_one(
    store: &Store,
    env: &Env<'_>,
    mode: ExitMode,
    target: &ExitTarget<'_>,
) -> Result<Asked> {
    Ok(ask(store, env, mode, std::slice::from_ref(target))?
        .first()
        .copied()
        .unwrap_or(Asked::Stayed))
}

/// Runs `mode`'s ladder for a session whose `claude` runs in a known pane, and returns whether it left.
///
/// A session whose recorded `claude` is already gone counts as left; one with no `claude` recorded or no known pane
/// as stayed, for the caller to end.
///
/// # Errors
///
/// Fails when the store fails.
pub fn ask_session(
    store: &Store,
    env: &Env<'_>,
    mode: ExitMode,
    task: Option<TaskId>,
    session: &SessionRow,
) -> Result<bool> {
    let Some(claude) = session.claude() else {
        return Ok(false);
    };
    if !session_procs::runs(&claude) {
        return Ok(true);
    }
    match ExitTarget::of_session(task, session) {
        Some(target) => Ok(ask_one(store, env, mode, &target)?.left()),
        None => Ok(false),
    }
}

fn type_input(
    host: &dyn TerminalHost,
    surface: &SurfaceId,
    input: ExitInput,
) -> Result<(), HostError> {
    match input {
        ExitInput::Escape => host.send_key(surface, Key::Escape),
        ExitInput::ClearLine => host.send_key(surface, Key::Ctrl('u')),
        ExitInput::Command => host.send_text(surface, EXIT_COMMAND),
        ExitInput::Enter => host.send_key(surface, Key::Enter),
        ExitInput::Interrupt => host.send_key(surface, Key::Ctrl('c')),
        ExitInput::Settle | ExitInput::Beat => Ok(()),
    }
}
