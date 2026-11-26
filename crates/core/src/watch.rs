//! The tick's decisions (design section 3.4), as pure functions over the facts it gathers: disk levels, runaway
//! output, stuck sessions, spawn timeouts, ask reminders and the merged grace.

use crate::config::WatchConfig;
use crate::vocabulary::{FindingId, TaskState};

const GIB: u64 = 1024 * 1024 * 1024;

/// The thresholds of `[watch]`, in the units the checks use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Free bytes below which the user is warned.
    pub disk_warn: u64,
    /// Free bytes below which the queue pauses.
    pub disk_stop: u64,
    /// Bytes of growth per minute that count as runaway output.
    pub growth: u64,
    /// A single file of this many bytes counts as runaway output.
    pub file: u64,
    /// Seconds without a hook event before a session may be stuck.
    pub stuck_after: u64,
    /// Seconds a `merged` session may sit idle before it gives up its slot; `None` never frees it.
    pub idle_free_merged: Option<u64>,
    /// Seconds an `in_review` session may sit idle before it gives up its slot; `None` never frees it.
    pub idle_free_review: Option<u64>,
    /// Seconds a spawning task may take to report `SessionStart`.
    pub spawn_timeout: u64,
    /// Seconds an ask may wait before each reminder.
    pub ask_remind: u64,
    /// Seconds a merged task lingers before it starts closing.
    pub merged_grace: u64,
    /// Seconds a closing task may keep processes alive before the tick ends them.
    pub closing_grace: u64,
}

/// Seconds a standing finding of this kind stays silent before the user is told again; `None` tells it once.
#[must_use]
pub fn retell_after(watch: &WatchConfig, rule: FindingId) -> Option<u64> {
    let hours = watch
        .retell_after_h_by_rule
        .get(&rule)
        .copied()
        .unwrap_or(watch.retell_after_h);
    (hours > 0).then(|| u64::from(hours) * 3600)
}

/// Whether `live` sessions are more than the `[watch] many_sessions` limit; 0 turns the finding off.
#[must_use]
pub fn many_sessions(live: usize, limit: u32) -> bool {
    limit > 0 && usize::try_from(limit).is_ok_and(|limit| live > limit)
}

/// Whether a finding silent for `quiet_s` seconds is told again after `after`.
#[must_use]
pub const fn retell_due(quiet_s: u64, after: Option<u64>) -> bool {
    match after {
        Some(limit) => quiet_s >= limit,
        None => false,
    }
}

impl Limits {
    /// The limits a config states.
    #[must_use]
    pub fn from_config(watch: &WatchConfig) -> Self {
        let gib = |n: u32| u64::from(n) * GIB;
        let minutes = |n: u32| u64::from(n) * 60;
        let optional = |n: u32| (n > 0).then(|| minutes(n));
        Self {
            disk_warn: gib(watch.disk_warn_gb),
            disk_stop: gib(watch.disk_stop_gb),
            growth: gib(watch.growth_limit_gb_per_min),
            file: gib(watch.file_limit_gb),
            stuck_after: minutes(watch.stuck_after_min),
            idle_free_merged: optional(watch.idle_free_merged_min),
            idle_free_review: optional(watch.idle_free_review_min),
            spawn_timeout: u64::from(watch.spawn_timeout_s),
            ask_remind: minutes(watch.ask_remind_min),
            merged_grace: minutes(watch.merged_grace_min),
            closing_grace: u64::from(watch.closing_grace_s),
        }
    }

    /// Files smaller than this are not sampled between ticks. A file seen for the first time gets no growth verdict
    /// (only the size limit applies to it), so a file that crosses this floor is judged on growth from the next tick.
    #[must_use]
    pub const fn sample_floor(&self) -> u64 {
        self.growth / 4
    }
}

/// How full a volume is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskLevel {
    /// Enough room.
    Ok,
    /// Below the warning level.
    Warn,
    /// Below the stop level.
    Stop,
}

/// The level of a volume with `free` bytes left.
#[must_use]
pub const fn disk_level(free: u64, limits: &Limits) -> DiskLevel {
    if free < limits.disk_stop {
        DiskLevel::Stop
    } else if free < limits.disk_warn {
        DiskLevel::Warn
    } else {
        DiskLevel::Ok
    }
}

/// Why a file counts as runaway output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runaway {
    /// It grows by this many bytes per minute since the last tick.
    Growth(u64),
    /// It is this many bytes large.
    Size(u64),
}

/// The shortest interval growth is spread over: ticks closer together than this are judged as if a minute passed, so a
/// manual tick seconds after the last does not turn a little growth into a huge rate.
pub const MIN_INTERVAL_S: u64 = 60;

/// Whether a file of `size` bytes is runaway.
///
/// `previous` is its size at the last tick, `None` when it was not sampled then, and `elapsed_s` the seconds since
/// that tick. Growth is judged as a rate per minute, and only against a
/// previous sample: a file first seen is judged on its size alone.
#[must_use]
pub fn runaway(
    size: u64,
    previous: Option<u64>,
    elapsed_s: u64,
    limits: &Limits,
) -> Option<Runaway> {
    if size >= limits.file {
        return Some(Runaway::Size(size));
    }
    let grown = size.saturating_sub(previous?);
    let per_minute = grown.saturating_mul(60) / elapsed_s.max(MIN_INTERVAL_S);
    (per_minute >= limits.growth).then_some(Runaway::Growth(per_minute))
}

/// Whether a spawning task has waited for `SessionStart` for too long.
#[must_use]
pub const fn spawn_timed_out(age_s: u64, limits: &Limits) -> bool {
    age_s >= limits.spawn_timeout
}

/// What the stuck check makes of a working session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stuck {
    /// Recently active, or a tool runs: nothing to do.
    No,
    /// Quiet for too long and no earlier look at its screen: look now and compare next time.
    Sample,
    /// Quiet for too long and the screen is unchanged since the last look.
    Yes,
}

/// Decides the stuck check. `idle_s` is the time since the last hook event; `screen_unchanged` is `None` when the
/// screen was not looked at before; `after_s` is how long a quiet session may sit before it counts.
#[must_use]
pub const fn stuck_after(
    idle_s: Option<u64>,
    tool_in_flight: bool,
    screen_unchanged: Option<bool>,
    after_s: u64,
) -> Stuck {
    let quiet = match idle_s {
        Some(idle) => idle >= after_s,
        None => false,
    };
    if !quiet || tool_in_flight {
        return Stuck::No;
    }
    match screen_unchanged {
        Some(true) => Stuck::Yes,
        Some(false) | None => Stuck::Sample,
    }
}

/// Seconds a session in `state` may sit idle at its prompt before it gives up its worker slot; `None` when the state
/// never frees one (or the limit is off).
#[must_use]
pub const fn idle_free_after(state: TaskState, limits: &Limits) -> Option<u64> {
    match state {
        TaskState::InReview => limits.idle_free_review,
        TaskState::Merged => limits.idle_free_merged,
        TaskState::Queued
        | TaskState::Spawning
        | TaskState::Working
        | TaskState::NeedsUser
        | TaskState::Closing
        | TaskState::Parking
        | TaskState::Closed
        | TaskState::Dropped
        | TaskState::Failed
        | TaskState::Interrupted
        | TaskState::Parked => None,
    }
}

/// Whether an ask waiting `age_s` seconds, last notified `since_notified_s` seconds ago (none: never), is due a
/// reminder.
#[must_use]
pub const fn ask_due(age_s: u64, since_notified_s: Option<u64>, limits: &Limits) -> bool {
    if age_s < limits.ask_remind {
        return false;
    }
    match since_notified_s {
        Some(since) => since >= limits.ask_remind,
        None => true,
    }
}

/// Lines of Claude Code's folder trust dialog, in its current and earlier wordings, lower case.
const TRUST_PROMPT_LINES: [&str; 5] = [
    "do you trust this folder",
    "do you trust the files in this folder",
    "yes, i trust this folder",
    "yes, proceed",
    "is this a project you created or one you trust",
];

/// Whether a screen shows Claude Code's folder trust dialog. "Yes, proceed" counts only beside the word "trust".
#[must_use]
pub fn trust_prompt(screen: &str) -> bool {
    let screen = screen.to_lowercase();
    TRUST_PROMPT_LINES
        .iter()
        .any(|line| screen.contains(line) && (*line != "yes, proceed" || screen.contains("trust")))
}

/// How a merged task's session has behaved since the merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergedSession {
    /// The task has no live session.
    Gone,
    /// The session is live.
    Live {
        /// Seconds since its last hook event, none when there was none.
        idle_s: Option<u64>,
        /// A tool call is running.
        tool_in_flight: bool,
    },
}

/// Whether a merged task has lingered long enough to start closing.
///
/// The merge must be older than the grace and its session quiet as long, so a task that delivers in several pull
/// requests and keeps working after a merge is not closed under it.
#[must_use]
pub const fn merged_grace_over(age_s: u64, session: MergedSession, limits: &Limits) -> bool {
    if age_s < limits.merged_grace {
        return false;
    }
    match session {
        MergedSession::Gone => true,
        MergedSession::Live {
            tool_in_flight: true,
            ..
        } => false,
        MergedSession::Live {
            idle_s,
            tool_in_flight: false,
        } => match idle_s {
            Some(idle) => idle >= limits.merged_grace,
            None => true,
        },
    }
}

/// Seconds a tick may hold `tick.lock` before the next run calls it hung.
pub const TICK_HUNG_AFTER_S: u64 = 15 * 60;

/// Who holds `tick.lock` and since when: the first line of `tick.started`, `<pid> <unix seconds>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickStamp {
    /// The process running the tick.
    pub pid: u32,
    /// When it took the lock, in seconds since the Unix epoch.
    pub started_s: u64,
}

impl TickStamp {
    /// The text written to `tick.started`.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "{} {}
",
            self.pid, self.started_s
        )
    }

    /// Reads the text of `tick.started`; `None` when it is not a stamp.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut words = text.split_whitespace();
        let pid = words.next()?.parse().ok()?;
        let started_s = words.next()?.parse().ok()?;
        Some(Self { pid, started_s })
    }

    /// Whether the tick that took the lock has held it for longer than [`TICK_HUNG_AFTER_S`] at `now_s`.
    #[must_use]
    pub const fn hung(&self, now_s: u64) -> bool {
        now_s.saturating_sub(self.started_s) > TICK_HUNG_AFTER_S
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn many_sessions_needs_more_than_the_limit() {
        assert!(many_sessions(21, 20), "over the limit");
        assert!(!many_sessions(20, 20), "at the limit");
        assert!(!many_sessions(500, 0), "0 is off");
    }

    use super::*;

    #[test]
    fn a_tick_is_hung_only_past_the_threshold() {
        let stamp = TickStamp {
            pid: 7,
            started_s: 1000,
        };
        assert!(!stamp.hung(1000 + TICK_HUNG_AFTER_S), "exactly at it");
        assert!(stamp.hung(1001 + TICK_HUNG_AFTER_S), "past it");
        assert!(!stamp.hung(10), "a clock that went back");
    }

    #[test]
    fn a_stamp_round_trips_and_garbage_is_refused() {
        let stamp = TickStamp {
            pid: 42,
            started_s: 1_700_000_000,
        };
        assert_eq!(TickStamp::parse(&stamp.render()), Some(stamp));
        assert_eq!(TickStamp::parse(""), None);
        assert_eq!(TickStamp::parse("12"), None);
        assert_eq!(TickStamp::parse("a b"), None);
    }

    const LIMITS: Limits = Limits {
        disk_warn: 15,
        disk_stop: 5,
        growth: 100,
        file: 1000,
        stuck_after: 60,
        idle_free_merged: Some(120),
        idle_free_review: None,
        spawn_timeout: 120,
        ask_remind: 600,
        merged_grace: 300,
        closing_grace: 60,
    };

    #[test]
    fn the_trust_dialog_is_recognized_in_every_wording() {
        for (screen, shown) in [
            (
                "Do you trust this folder?
> 1. Yes, I trust this folder
  2. No, exit",
                true,
            ),
            (
                " Do you trust the files in this folder?
 S:/Code/site/site",
                true,
            ),
            (
                "Accessing workspace:
Is this a project you created or one you trust?
 1. Yes, proceed",
                true,
            ),
            ("YES, I TRUST THIS FOLDER", true),
            (
                "> 1. Yes, proceed
  2. No",
                false,
            ),
            ("I trust the tests now; running cargo test", false),
            ("", false),
        ] {
            assert_eq!(trust_prompt(screen), shown, "{screen}");
        }
    }

    #[test]
    fn disk_levels_follow_the_two_thresholds() {
        for (free, level) in [
            (4, DiskLevel::Stop),
            (5, DiskLevel::Warn),
            (14, DiskLevel::Warn),
            (15, DiskLevel::Ok),
            (99, DiskLevel::Ok),
        ] {
            assert_eq!(disk_level(free, &LIMITS), level, "{free} bytes free");
        }
    }

    #[test]
    fn the_stuck_threshold_is_the_one_given() {
        for (idle, tool, same, after, expected) in [
            (Some(59), false, Some(true), 60, Stuck::No),
            (Some(60), false, Some(true), 60, Stuck::Yes),
            (Some(60), false, None, 60, Stuck::Sample),
            (Some(100), true, Some(true), 60, Stuck::No),
            (Some(100), false, Some(true), 0, Stuck::Yes),
            (None, false, Some(true), 0, Stuck::No),
        ] {
            assert_eq!(
                stuck_after(idle, tool, same, after),
                expected,
                "{idle:?} {tool} {same:?} {after}"
            );
        }
    }

    #[test]
    fn only_merged_and_in_review_free_a_slot_when_idle() {
        let limits = Limits {
            idle_free_merged: Some(10),
            idle_free_review: Some(20),
            ..LIMITS
        };
        for state in TaskState::ALL {
            let expected = if *state == TaskState::InReview {
                Some(20)
            } else if *state == TaskState::Merged {
                Some(10)
            } else {
                None
            };
            assert_eq!(idle_free_after(*state, &limits), expected, "{state}");
        }
        assert_eq!(
            idle_free_after(TaskState::Merged, &LIMITS),
            Some(120),
            "merged"
        );
        assert_eq!(
            idle_free_after(TaskState::InReview, &LIMITS),
            None,
            "review off"
        );
    }

    #[test]
    fn runaway_is_size_or_growth() {
        for (size, previous, elapsed, expected) in [
            (50, None, 60, None),
            (99, Some(0), 60, None),
            (900, None, 60, None),
            (1000, None, 60, Some(Runaway::Size(1000))),
            (500, Some(350), 60, Some(Runaway::Growth(150))),
            (500, Some(450), 60, None),
            (1000, Some(1000), 60, Some(Runaway::Size(1000))),
            (900, Some(950), 60, None),
            (500, Some(350), 5, Some(Runaway::Growth(150))),
            (500, Some(350), 0, Some(Runaway::Growth(150))),
            (500, Some(200), 120, Some(Runaway::Growth(150))),
            (500, Some(350), 120, None),
            (900, Some(0), 3600, None),
        ] {
            assert_eq!(
                runaway(size, previous, elapsed, &LIMITS),
                expected,
                "size {size}, before {previous:?}, {elapsed} s"
            );
        }
    }

    #[test]
    fn the_spawn_timeout_is_inclusive() {
        assert!(!spawn_timed_out(119, &LIMITS), "just under");
        assert!(spawn_timed_out(120, &LIMITS), "at the limit");
    }

    #[test]
    fn a_session_is_stuck_only_when_quiet_idle_and_unchanged() {
        for (idle, tool, same, expected) in [
            (None, false, None, Stuck::No),
            (Some(59), false, Some(true), Stuck::No),
            (Some(60), true, Some(true), Stuck::No),
            (Some(60), false, None, Stuck::Sample),
            (Some(60), false, Some(false), Stuck::Sample),
            (Some(600), false, Some(true), Stuck::Yes),
        ] {
            assert_eq!(
                stuck_after(idle, tool, same, LIMITS.stuck_after),
                expected,
                "idle {idle:?}, tool {tool}, unchanged {same:?}"
            );
        }
    }

    #[test]
    fn asks_are_reminded_once_per_interval() {
        for (age, since, due) in [
            (599, None, false),
            (600, None, true),
            (900, Some(100), false),
            (1300, Some(600), true),
        ] {
            assert_eq!(
                ask_due(age, since, &LIMITS),
                due,
                "age {age}, notified {since:?} ago"
            );
        }
    }

    #[test]
    fn the_merged_grace_is_inclusive() {
        assert!(
            !merged_grace_over(299, MergedSession::Gone, &LIMITS),
            "just under"
        );
        assert!(
            merged_grace_over(300, MergedSession::Gone, &LIMITS),
            "at the limit"
        );
    }

    #[test]
    fn the_merged_grace_waits_for_a_quiet_session() {
        let live = |idle_s, tool_in_flight| MergedSession::Live {
            idle_s,
            tool_in_flight,
        };
        let cases = [
            (
                300,
                live(Some(300), false),
                true,
                "quiet as long as the grace",
            ),
            (300, live(Some(299), false), false, "worked a moment ago"),
            (
                3_600,
                live(Some(10), false),
                false,
                "a lead still working long after its first merge",
            ),
            (300, live(Some(900), true), false, "a tool call is running"),
            (300, live(None, false), true, "no hook event yet"),
            (
                299,
                live(Some(900), false),
                false,
                "the merge is too recent",
            ),
        ];
        for (age, session, over, why) in cases {
            assert_eq!(merged_grace_over(age, session, &LIMITS), over, "{why}");
        }
    }

    #[test]
    fn a_finding_is_told_again_after_its_kinds_hours() {
        let mut watch = WatchConfig::default();
        assert_eq!(
            retell_after(&watch, FindingId::Stuck),
            Some(6 * 3600),
            "default"
        );
        watch.retell_after_h_by_rule.insert(FindingId::Stuck, 2);
        watch.retell_after_h_by_rule.insert(FindingId::Runaway, 0);
        assert_eq!(
            retell_after(&watch, FindingId::Stuck),
            Some(2 * 3600),
            "per kind"
        );
        assert_eq!(
            retell_after(&watch, FindingId::Runaway),
            None,
            "0 tells once"
        );
        assert_eq!(
            retell_after(&watch, FindingId::IdleSlot),
            Some(6 * 3600),
            "others keep the default"
        );
        assert!(!retell_due(2 * 3600 - 1, Some(2 * 3600)), "not yet");
        assert!(retell_due(2 * 3600, Some(2 * 3600)), "due");
        assert!(!retell_due(u64::MAX, None), "never");
    }

    #[test]
    fn limits_convert_the_config_units() {
        let limits = Limits::from_config(&WatchConfig::default());
        assert_eq!(limits.disk_warn, 15 * GIB, "disk warning");
        assert_eq!(limits.stuck_after, 30 * 60, "stuck after");
        assert_eq!(limits.idle_free_merged, Some(30 * 60), "idle free merged");
        assert_eq!(limits.idle_free_review, Some(60 * 60), "idle free review");
        let off = Limits::from_config(&WatchConfig {
            idle_free_merged_min: 0,
            idle_free_review_min: 0,
            ..WatchConfig::default()
        });
        assert_eq!(off.idle_free_merged, None, "merged off");
        assert_eq!(off.idle_free_review, None, "review off");
        assert_eq!(limits.spawn_timeout, 120, "spawn timeout");
        assert_eq!(limits.ask_remind, 120 * 60, "ask reminder");
        assert_eq!(limits.merged_grace, 10 * 60, "merged grace");
        assert_eq!(limits.sample_floor(), GIB / 4, "sample floor");
    }
}
