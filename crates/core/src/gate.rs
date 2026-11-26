//! CPU and GPU gates: machine-wide slots that heavy commands hold while they run (`docs/decisions/2026-11-26-gates-in-headwork.md`).
//!
//! A slot is a folder `<dir>/<class>/slot-N` holding an `owner` file of `key=value` lines. This module is the pure
//! part: the classes, the owner file's format, and the decisions a gate takes from what it read (which slot to try,
//! whether a waiter may take a free slot, when a holder gives its slot back to legacy holders, the wait message). The
//! folders themselves are `headwork_adapters::gate`.

use std::fmt::{self, Write as _};
use std::num::NonZeroU32;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::UnknownWord;
use crate::vocabulary::text_enum;

text_enum!(
    /// What a gated command needs the machine for.
    GateClass, "gate class" {
        /// Cargo build, test, clippy and bench, `check.sh`, renders, scans and `git gc`.
        Cpu => "cpu",
        /// Training.
        Gpu => "gpu",
    }
);

impl GateClass {
    /// The integer a job row keeps in its `heavy` column: 0 for no gate, 1 for `cpu`, 2 for `gpu`.
    #[must_use]
    pub const fn job_code(gate: Option<Self>) -> i64 {
        match gate {
            None => 0,
            Some(Self::Cpu) => 1,
            Some(Self::Gpu) => 2,
        }
    }

    /// The gate a job row's `heavy` column says: 0 and unknown numbers none, 1 `cpu`, 2 `gpu`.
    #[must_use]
    pub const fn from_job_code(code: i64) -> Option<Self> {
        match code {
            1 => Some(Self::Cpu),
            2 => Some(Self::Gpu),
            _ => None,
        }
    }
}

/// Seconds after which a slot folder whose owner file was never written counts as stale.
pub const UNOWNED_STALE_S: u64 = 10;

/// Seconds after which a `reap.lock` folder counts as stale and may be removed.
pub const REAP_LOCK_STALE_S: u64 = 30;

/// Seconds a native owner's process may seem to have started after the owner's `epoch` and still be the owner: start
/// times and `epoch` are whole seconds read from two clocks.
pub const PID_REUSE_SLACK_S: u64 = 5;

/// Whether the process now running under an owner's pid took the pid over.
///
/// It did when it started more than [`PID_REUSE_SLACK_S`] seconds after the owner's `epoch`, since the owner's
/// process was already running then. An owner without `epoch`, or a start time that is unknown or reads 0, never
/// counts as taken over.
#[must_use]
pub const fn pid_taken_over(epoch: Option<u64>, started: Option<u64>) -> bool {
    match (epoch, started) {
        (Some(epoch), Some(started)) => {
            started != 0 && started > epoch.saturating_add(PID_REUSE_SLACK_S)
        }
        _ => false,
    }
}

/// The owner file's name inside a slot folder.
pub const OWNER_FILE: &str = "owner";

/// The prefix of a slot folder's name, followed by its number from 1.
pub const SLOT_PREFIX: &str = "slot-";

/// The prefix of a waiter's file name, followed by its pid.
pub const WAIT_PREFIX: &str = "wait-";

/// The lock folder held while a stale slot or waiter file is taken away.
pub const REAP_LOCK: &str = "reap.lock";

/// The default slot count of a class.
#[must_use]
pub const fn default_count(class: GateClass) -> NonZeroU32 {
    match class {
        GateClass::Cpu => NonZeroU32::MIN.saturating_add(2),
        GateClass::Gpu => NonZeroU32::MIN.saturating_add(1),
    }
}

/// Who holds a slot or waits for one: the `owner` file of a slot folder, or a `wait-<pid>` file.
///
/// Lines are `key=value`; unknown keys are ignored, so older and newer writers share the folders. An owner written by
/// a Git Bash script (`heavy.sh`) carries no `native=1`: its `pid` is an MSYS pid, not the system's.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Owner {
    /// The holder's process id.
    pub pid: u32,
    /// Whether `pid` is the OS-native process id (`native=1`); a Git Bash script writes an MSYS pid without it.
    pub native: bool,
    /// The class, when the writer recorded it.
    pub class: Option<GateClass>,
    /// The task's name, else its id, else empty.
    pub task: String,
    /// The task's id (`HEADWORK_TASK`), when the holder runs inside a task.
    pub task_id: Option<i64>,
    /// The git top level of the folder the command runs in, else that folder.
    pub worktree: String,
    /// The command line.
    pub command: String,
    /// When it started, local time `%Y-%m-%d %H:%M:%S`, for people.
    pub started: String,
    /// When it started, Unix seconds; `None` from writers that do not record it.
    pub epoch: Option<u64>,
}

/// Why an owner file does not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OwnerError {
    /// No `pid=` line.
    #[error("no pid")]
    NoPid,
    /// The `pid=` line is not a number.
    #[error("pid `{0}` is not a number")]
    BadPid(String),
}

impl Owner {
    /// Reads an owner file. The first line of each key wins; unknown keys, lines without `=` and values that do not
    /// parse (other than `pid`) are skipped.
    ///
    /// # Errors
    ///
    /// Returns [`OwnerError`] when there is no `pid` or it is not a number.
    pub fn parse(text: &str) -> Result<Self, OwnerError> {
        let mut owner = Self::default();
        let mut pid = None;
        let mut seen = std::collections::HashSet::new();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim_end_matches('\r');
            if !seen.insert(key) {
                continue;
            }
            match key {
                "pid" => pid = Some(parse_pid(value)?),
                "native" => owner.native = value.trim() == "1",
                "class" => owner.class = value.trim().parse().ok(),
                "task" => value.clone_into(&mut owner.task),
                "task_id" => owner.task_id = value.trim().parse().ok(),
                "worktree" => value.clone_into(&mut owner.worktree),
                "command" => value.clone_into(&mut owner.command),
                "started" => value.clone_into(&mut owner.started),
                "epoch" => owner.epoch = value.trim().parse().ok(),
                _ => {}
            }
        }
        owner.pid = pid.ok_or(OwnerError::NoPid)?;
        Ok(owner)
    }

    /// Writes the owner file: one `key=value` line each, line breaks inside a value turned into spaces.
    #[must_use]
    pub fn render(&self) -> String {
        let mut text = format!("pid={}\n", self.pid);
        if self.native {
            text.push_str("native=1\n");
        }
        let mut line = |key: &str, value: &str| {
            let _written = writeln!(text, "{key}={}", one_line(value));
        };
        if let Some(class) = self.class {
            line("class", class.as_str());
        }
        line("task", &self.task);
        if let Some(id) = self.task_id {
            line("task_id", &id.to_string());
        }
        line("worktree", &self.worktree);
        line("command", &self.command);
        line("started", &self.started);
        if let Some(epoch) = self.epoch {
            line("epoch", &epoch.to_string());
        }
        text
    }

    /// Who this is for people: the task when known, else the last part of the worktree.
    #[must_use]
    pub fn who(&self) -> &str {
        if self.task.is_empty() {
            last_segment(&self.worktree)
        } else {
            &self.task
        }
    }

    /// Whether this one came before `other`: earlier `epoch` (an unknown one counts as the earliest), then the
    /// smaller pid. Every reader orders a class's waiters the same way.
    #[must_use]
    pub fn is_before(&self, other: &Self) -> bool {
        (self.epoch.unwrap_or(0), self.pid) < (other.epoch.unwrap_or(0), other.pid)
    }
}

fn parse_pid(value: &str) -> Result<u32, OwnerError> {
    value
        .trim()
        .parse()
        .map_err(|_| OwnerError::BadPid(value.to_owned()))
}

fn one_line(value: &str) -> String {
    value.replace(['\r', '\n'], " ")
}

fn last_segment(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
}

/// The slot number in a folder name `slot-N`, N from 1.
#[must_use]
pub fn slot_number(name: &str) -> Option<u32> {
    name.strip_prefix(SLOT_PREFIX)?
        .parse()
        .ok()
        .filter(|&n| n > 0)
}

/// The pid in a waiter's file name `wait-<pid>`.
#[must_use]
pub fn wait_pid(name: &str) -> Option<u32> {
    name.strip_prefix(WAIT_PREFIX)?.parse().ok()
}

/// The first slot number from 1 to `count` that is not taken.
#[must_use]
pub fn first_free_slot(count: NonZeroU32, taken: &[u32]) -> Option<u32> {
    (1..=count.get()).find(|n| !taken.contains(n))
}

/// Free slots of a class: its count less its live holders (legacy holders included for `cpu`).
#[must_use]
pub const fn free_slots(count: NonZeroU32, live_holders: u32) -> u32 {
    count.get().saturating_sub(live_holders)
}

/// Whether a waiter may take a free slot: first come first served, so only when fewer live waiters came before it
/// than there are free slots.
#[must_use]
pub const fn may_take(free: u32, older_waiters: u32) -> bool {
    older_waiters < free
}

/// Whether a holder that just took `cpu/slot-N` gives it back to the live legacy holders.
///
/// It does when the holders that came before it in its class and the live legacy holders already fill the count.
/// Legacy scripts never look in `cpu/`, so they may have started past a full class; the newest holder yields to them.
#[must_use]
pub const fn must_give_back(count: NonZeroU32, older_holders: u32, legacy_live: u32) -> bool {
    older_holders.saturating_add(legacy_live) >= count.get()
}

/// Why a gated command runs without taking a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bypass {
    /// `CI` is set.
    Ci,
    /// `HEADWORK_GATE_OFF=1`.
    Off,
    /// The process tree already holds this class (`HEADWORK_GATE_HELD`, or `HEADWORK_HEAVY_HELD=1` for `cpu`).
    Nested,
}

/// What the environment says about gating.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GateEnv {
    /// `CI` is set (to anything).
    pub ci: bool,
    /// `HEADWORK_GATE_OFF` is `1`.
    pub off: bool,
    /// `HEADWORK_GATE_HELD`: a comma list of classes the process tree holds.
    pub held: String,
    /// `HEADWORK_HEAVY_HELD` is `1` (the old `heavy.sh`), which counts as `cpu`.
    pub heavy_held: bool,
}

impl GateEnv {
    /// Whether the process tree already holds `class`.
    #[must_use]
    pub fn holds(&self, class: GateClass) -> bool {
        self.held
            .split(',')
            .any(|word| word.trim() == class.as_str())
            || (class == GateClass::Cpu && self.heavy_held)
    }

    /// Why a command of `class` runs directly, or `None` when it takes a slot.
    #[must_use]
    pub fn bypass(&self, class: GateClass) -> Option<Bypass> {
        if self.ci {
            Some(Bypass::Ci)
        } else if self.off {
            Some(Bypass::Off)
        } else if self.holds(class) {
            Some(Bypass::Nested)
        } else {
            None
        }
    }

    /// `HEADWORK_GATE_HELD` for the command a gate of `class` runs: the held list with `class` added once.
    #[must_use]
    pub fn held_with(&self, class: GateClass) -> String {
        let mut classes: Vec<&str> = self
            .held
            .split(',')
            .map(str::trim)
            .filter(|word| !word.is_empty())
            .collect();
        if !classes.contains(&class.as_str()) {
            classes.push(class.as_str());
        }
        classes.join(",")
    }
}

/// A holder as a wait message or a status line shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolderLine {
    /// The task, else the worktree's last part ([`Owner::who`]).
    pub who: String,
    /// The command, short ([`short_command`]).
    pub command: String,
    /// Seconds it has held the slot.
    pub held_s: u64,
}

impl HolderLine {
    /// The line for an owner that has held its slot `held_s` seconds.
    #[must_use]
    pub fn of(owner: &Owner, held_s: u64) -> Self {
        Self {
            who: owner.who().to_owned(),
            command: short_command(&owner.command),
            held_s,
        }
    }
}

/// `me-sim-x cargo test 4m`, without the parts that are empty.
impl fmt::Display for HolderLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts = [self.who.as_str(), self.command.as_str()];
        for part in parts.iter().filter(|part| !part.is_empty()) {
            write!(f, "{part} ")?;
        }
        f.write_str(&held_for(self.held_s))
    }
}

/// The line a waiting gate prints: `waiting for cpu (3/3: me-sim-x cargo test 4m, headwork-y check.sh 1m)`, with
/// `; N ahead` added when live waiters came before this one.
#[must_use]
pub fn wait_message(
    class: GateClass,
    count: NonZeroU32,
    holders: &[HolderLine],
    ahead: u32,
) -> String {
    let mut text = format!("waiting for {class} ({}/{count}", holders.len());
    for (index, holder) in holders.iter().enumerate() {
        text.push_str(if index == 0 { ": " } else { ", " });
        text.push_str(&holder.to_string());
    }
    if ahead > 0 {
        let _written = write!(text, "; {ahead} ahead");
    }
    text.push(')');
    text
}

/// Whether something that has lasted `age_s` seconds has lasted `minutes` minutes; a limit of 0 never is.
#[must_use]
pub fn lasted_minutes(age_s: u64, minutes: u32) -> bool {
    minutes > 0 && age_s >= u64::from(minutes) * 60
}

/// One class as the board line shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassLine {
    /// The class.
    pub class: GateClass,
    /// Its slot count.
    pub count: NonZeroU32,
    /// The live holders.
    pub holders: Vec<HolderLine>,
    /// The live waiters.
    pub waiting: u32,
}

/// `cpu 2/3 (me-sim-x cargo test 4m, headwork-y check.sh 1m) · 1 waiting · gpu 0/2`, after `Gates: ` on the board.
#[must_use]
pub fn gates_line(classes: &[ClassLine]) -> String {
    let mut parts = Vec::new();
    for line in classes {
        let mut part = format!("{} {}/{}", line.class, line.holders.len(), line.count);
        if !line.holders.is_empty() {
            let holders: Vec<String> = line.holders.iter().map(ToString::to_string).collect();
            let _written = write!(part, " ({})", holders.join(", "));
        }
        parts.push(part);
        if line.waiting > 0 {
            parts.push(format!("{} waiting", line.waiting));
        }
    }
    parts.join(" \u{b7} ")
}

/// A duration for people: `45s`, `4m`, `2h05m`.
#[must_use]
pub fn held_for(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        _ => format!("{}h{:02}m", seconds / 3600, seconds % 3600 / 60),
    }
}

/// A command shortened for a wait message: `cargo test`, `check.sh`.
///
/// It is the program's file name, without a leading `bash`, `sh`, `env`, their options or `NAME=value`, and the next
/// word when it is a subcommand (not an option or a path).
#[must_use]
pub fn short_command(command: &str) -> String {
    let mut words = command.split_whitespace().skip_while(|word| {
        matches!(*word, "bash" | "sh" | "env" | "bash.exe" | "sh.exe")
            || word.contains('=')
            || word.starts_with('-')
    });
    let Some(program) = words.next() else {
        return String::new();
    };
    let mut short = last_segment(program).to_owned();
    if let Some(next) = words.next()
        && !next.starts_with('-')
        && !next.contains(['/', '\\', '='])
    {
        short.push(' ');
        short.push_str(next);
    }
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(n: u32) -> NonZeroU32 {
        NonZeroU32::new(n).unwrap_or(NonZeroU32::MIN)
    }

    fn owner(pid: u32, epoch: Option<u64>) -> Owner {
        Owner {
            pid,
            native: true,
            class: Some(GateClass::Cpu),
            task: "me-sim-x".to_owned(),
            task_id: Some(7),
            worktree: "S:/Code/me-sim/me-sim-x".to_owned(),
            command: "cargo test --workspace".to_owned(),
            started: "2026-11-26 10:00:00".to_owned(),
            epoch,
        }
    }

    #[test]
    fn a_pid_is_taken_over_only_by_a_process_that_started_well_after_the_epoch() {
        let cases = [
            (Some(100), Some(90), false),
            (Some(100), Some(100), false),
            (Some(100), Some(105), false),
            (Some(100), Some(106), true),
            (Some(100), Some(0), false),
            (Some(100), None, false),
            (None, Some(1_000), false),
        ];
        for (epoch, started, taken) in cases {
            assert_eq!(
                pid_taken_over(epoch, started),
                taken,
                "{epoch:?} {started:?}"
            );
        }
    }

    #[test]
    fn minutes_are_counted_from_a_limit_and_zero_never_passes() {
        assert!(!lasted_minutes(45 * 60 - 1, 45), "just under");
        assert!(lasted_minutes(45 * 60, 45), "at the limit");
        assert!(!lasted_minutes(u64::MAX, 0), "0 is off");
    }

    #[test]
    fn the_board_line_names_holders_and_waiters() {
        let holder = |who: &str, command: &str, held_s| HolderLine {
            who: who.to_owned(),
            command: command.to_owned(),
            held_s,
        };
        let classes = [
            ClassLine {
                class: GateClass::Cpu,
                count: count(3),
                holders: vec![
                    holder("me-sim-x", "cargo test", 240),
                    holder("headwork-y", "check.sh", 60),
                ],
                waiting: 1,
            },
            ClassLine {
                class: GateClass::Gpu,
                count: count(2),
                holders: Vec::new(),
                waiting: 0,
            },
        ];
        assert_eq!(
            gates_line(&classes),
            "cpu 2/3 (me-sim-x cargo test 4m, headwork-y check.sh 1m) \u{b7} 1 waiting \u{b7} gpu 0/2"
        );
        assert_eq!(gates_line(&[]), "", "no classes");
    }

    #[test]
    fn job_codes_round_trip() {
        for gate in [None, Some(GateClass::Cpu), Some(GateClass::Gpu)] {
            assert_eq!(
                GateClass::from_job_code(GateClass::job_code(gate)),
                gate,
                "{gate:?}"
            );
        }
        assert_eq!(GateClass::from_job_code(9), None, "unknown");
    }

    #[test]
    fn classes_parse_and_display() {
        for (word, class) in [("cpu", GateClass::Cpu), ("gpu", GateClass::Gpu)] {
            assert_eq!(word.parse::<GateClass>(), Ok(class), "{word}");
            assert_eq!(class.to_string(), word, "{word}");
        }
        assert!("disk".parse::<GateClass>().is_err(), "unknown class");
        assert_eq!(default_count(GateClass::Cpu).get(), 3, "cpu default");
        assert_eq!(default_count(GateClass::Gpu).get(), 2, "gpu default");
    }

    #[test]
    fn an_owner_round_trips() {
        let written = owner(42, Some(1_764_000_000));
        let text = written.render();
        assert!(text.starts_with("pid=42\nnative=1\nclass=cpu\n"), "{text}");
        assert_eq!(Owner::parse(&text), Ok(written), "round trip");
    }

    #[test]
    fn a_line_break_in_a_value_becomes_a_space() {
        let mut written = owner(1, None);
        "a\nb\r\nc".clone_into(&mut written.command);
        let read = Owner::parse(&written.render()).unwrap_or_default();
        assert_eq!(read.command, "a b  c", "one line");
    }

    #[test]
    fn owner_files_parse() {
        type Read<'a> = Result<(u32, bool, &'a str, Option<u64>), OwnerError>;
        let cases: &[(&str, Read<'_>)] = &[
            (
                "pid=4711\nworktree=S:/Code/me-sim/me-sim\ncommand=render x\nstarted=2026-11-26 09:00:00\n",
                Ok((4711, false, "render x", None)),
            ),
            (
                "pid=5\r\nnative=1\r\ncommand=c\r\nepoch=9\r\n",
                Ok((5, true, "c", Some(9))),
            ),
            (
                "pid=5\npid=6\ncommand=first\ncommand=second\n",
                Ok((5, false, "first", None)),
            ),
            (
                "junk\nfuture=key\npid=8\nepoch=soon\n",
                Ok((8, false, "", None)),
            ),
            ("command=x\n", Err(OwnerError::NoPid)),
            ("pid=abc\n", Err(OwnerError::BadPid("abc".to_owned()))),
            ("", Err(OwnerError::NoPid)),
        ];
        for (text, expected) in cases {
            let parsed = Owner::parse(text).map(|o| (o.pid, o.native, o.command.clone(), o.epoch));
            let expected = expected
                .clone()
                .map(|(pid, native, command, epoch)| (pid, native, command.to_owned(), epoch));
            assert_eq!(parsed, expected, "{text:?}");
        }
    }

    #[test]
    fn who_is_the_task_else_the_worktree() {
        let mut legacy = owner(1, None);
        legacy.task.clear();
        "S:/Code/headwork/headwork-y/".clone_into(&mut legacy.worktree);
        assert_eq!(legacy.who(), "headwork-y", "worktree");
        assert_eq!(owner(1, None).who(), "me-sim-x", "task");
        legacy.worktree.clear();
        assert_eq!(legacy.who(), "", "nothing");
    }

    #[test]
    fn order_is_epoch_then_pid() {
        let cases = [
            (owner(9, Some(1)), owner(1, Some(2)), true),
            (owner(1, Some(2)), owner(9, Some(1)), false),
            (owner(1, Some(5)), owner(2, Some(5)), true),
            (owner(2, Some(5)), owner(1, Some(5)), false),
            (owner(5, None), owner(1, Some(1)), true),
            (owner(5, None), owner(1, None), false),
            (owner(5, None), owner(6, Some(1)), true),
            (owner(3, Some(3)), owner(3, Some(3)), false),
        ];
        for (a, b, before) in cases {
            assert_eq!(
                a.is_before(&b),
                before,
                "{:?} before {:?}",
                (a.epoch, a.pid),
                (b.epoch, b.pid)
            );
        }
    }

    #[test]
    fn names_parse() {
        let slots = [
            ("slot-1", Some(1)),
            ("slot-12", Some(12)),
            ("slot-0", None),
            ("slot-x", None),
            ("wait-3", None),
        ];
        for (name, number) in slots {
            assert_eq!(slot_number(name), number, "{name}");
        }
        let waits = [
            ("wait-77", Some(77)),
            ("wait-", None),
            ("slot-1", None),
            ("wait-a", None),
        ];
        for (name, pid) in waits {
            assert_eq!(wait_pid(name), pid, "{name}");
        }
    }

    #[test]
    fn the_first_free_slot_is_tried() {
        let cases: &[(u32, &[u32], Option<u32>)] = &[
            (3, &[], Some(1)),
            (3, &[1], Some(2)),
            (3, &[2], Some(1)),
            (3, &[1, 2, 3], None),
            (3, &[1, 3, 7], Some(2)),
            (1, &[1], None),
        ];
        for (n, taken, free) in cases {
            assert_eq!(first_free_slot(count(*n), taken), *free, "{n} {taken:?}");
        }
    }

    #[test]
    fn waiters_go_first_come_first_served() {
        let cases = [
            (3, 0, 0, true),
            (3, 2, 0, true),
            (3, 3, 0, false),
            (3, 4, 0, false),
            (3, 2, 1, false),
            (3, 1, 1, true),
            (3, 0, 2, true),
            (3, 0, 3, false),
        ];
        for (n, holders, older, takes) in cases {
            let free = free_slots(count(n), holders);
            assert_eq!(
                may_take(free, older),
                takes,
                "count {n}, {holders} holders, {older} older waiters"
            );
        }
    }

    #[test]
    fn the_newest_holder_gives_way_to_legacy_holders() {
        let cases = [
            (3, 0, 0, false),
            (3, 2, 0, false),
            (3, 2, 1, true),
            (3, 0, 3, true),
            (3, 1, 1, false),
            (1, 0, 1, true),
        ];
        for (n, older, legacy, gives) in cases {
            assert_eq!(
                must_give_back(count(n), older, legacy),
                gives,
                "count {n}, {older} older, {legacy} legacy"
            );
        }
    }

    #[test]
    fn the_environment_decides_bypass_and_nesting() {
        let env = |ci, off, held: &str, heavy| GateEnv {
            ci,
            off,
            held: held.to_owned(),
            heavy_held: heavy,
        };
        let cases = [
            (env(false, false, "", false), GateClass::Cpu, None),
            (
                env(true, false, "", false),
                GateClass::Cpu,
                Some(Bypass::Ci),
            ),
            (
                env(false, true, "", false),
                GateClass::Gpu,
                Some(Bypass::Off),
            ),
            (
                env(false, false, "cpu", false),
                GateClass::Cpu,
                Some(Bypass::Nested),
            ),
            (env(false, false, "cpu", false), GateClass::Gpu, None),
            (
                env(false, false, "gpu, cpu", false),
                GateClass::Cpu,
                Some(Bypass::Nested),
            ),
            (env(false, false, "cpux", false), GateClass::Cpu, None),
            (
                env(false, false, "", true),
                GateClass::Cpu,
                Some(Bypass::Nested),
            ),
            (env(false, false, "", true), GateClass::Gpu, None),
        ];
        for (env, class, bypass) in cases {
            assert_eq!(env.bypass(class), bypass, "{env:?} {class}");
        }
    }

    #[test]
    fn the_held_list_grows_once() {
        let cases = [
            ("", "cpu"),
            ("gpu", "gpu,cpu"),
            ("cpu", "cpu"),
            (" gpu , ", "gpu,cpu"),
            ("cpu,gpu", "cpu,gpu"),
        ];
        for (held, with) in cases {
            let env = GateEnv {
                held: held.to_owned(),
                ..GateEnv::default()
            };
            assert_eq!(env.held_with(GateClass::Cpu), with, "{held:?}");
        }
    }

    #[test]
    fn durations_read_short() {
        let cases = [
            (0, "0s"),
            (59, "59s"),
            (60, "1m"),
            (299, "4m"),
            (3599, "59m"),
            (3600, "1h00m"),
            (7500, "2h05m"),
        ];
        for (seconds, text) in cases {
            assert_eq!(held_for(seconds), text, "{seconds}");
        }
    }

    #[test]
    fn commands_shorten() {
        let cases = [
            ("cargo test --workspace", "cargo test"),
            ("bash scripts/check.sh --quick", "check.sh"),
            ("check.sh --quick in headwork-y", "check.sh"),
            ("C:\\tools\\me-sim.exe render scene", "me-sim.exe render"),
            ("env FOO=1 cargo build", "cargo build"),
            ("python train.py", "python train.py"),
            ("bash -c cargo test --locked", "cargo test"),
            ("bash scripts/x.sh sub/dir", "x.sh"),
            ("", ""),
            ("bash", ""),
        ];
        for (command, short) in cases {
            assert_eq!(short_command(command), short, "{command}");
        }
    }

    #[test]
    fn the_wait_message_names_each_holder() {
        let holders = [
            HolderLine::of(&owner(1, None), 240),
            HolderLine {
                who: "headwork-y".to_owned(),
                command: "check.sh".to_owned(),
                held_s: 61,
            },
            HolderLine {
                who: String::new(),
                command: String::new(),
                held_s: 5,
            },
        ];
        assert_eq!(
            wait_message(GateClass::Cpu, count(3), &holders, 0),
            "waiting for cpu (3/3: me-sim-x cargo test 4m, headwork-y check.sh 1m, 5s)",
            "full"
        );
        assert_eq!(
            wait_message(GateClass::Gpu, count(2), &[], 2),
            "waiting for gpu (0/2; 2 ahead)",
            "ahead"
        );
    }
}
