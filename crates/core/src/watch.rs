//! The tick's decisions (design section 3.4), as pure functions over the facts it gathers: disk levels, runaway
//! output, stuck sessions, spawn timeouts, ask reminders and the merged grace.

use crate::config::WatchConfig;

const GIB: u64 = 1024 * 1024 * 1024;

/// The thresholds of `[watch]`, in the units the checks use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Free bytes below which the user is warned.
    pub disk_warn: u64,
    /// Free bytes below which the queue pauses.
    pub disk_stop: u64,
    /// Bytes of growth between two ticks that count as runaway output.
    pub growth: u64,
    /// A single file of this many bytes counts as runaway output.
    pub file: u64,
    /// Seconds without a hook event before a session may be stuck.
    pub stuck_after: u64,
    /// Seconds a spawning task may take to report `SessionStart`.
    pub spawn_timeout: u64,
    /// Seconds an ask may wait before each reminder.
    pub ask_remind: u64,
    /// Seconds a merged task lingers before it starts closing.
    pub merged_grace: u64,
    /// Seconds a closing task may keep processes alive before the tick ends them.
    pub closing_grace: u64,
}

impl Limits {
    /// The limits a config states.
    #[must_use]
    pub fn from_config(watch: &WatchConfig) -> Self {
        let gib = |n: u32| u64::from(n) * GIB;
        let minutes = |n: u32| u64::from(n) * 60;
        Self {
            disk_warn: gib(watch.disk_warn_gb),
            disk_stop: gib(watch.disk_stop_gb),
            growth: gib(watch.growth_limit_gb_per_min),
            file: gib(watch.file_limit_gb),
            stuck_after: minutes(watch.stuck_after_min),
            spawn_timeout: u64::from(watch.spawn_timeout_s),
            ask_remind: minutes(watch.ask_remind_min),
            merged_grace: minutes(watch.merged_grace_min),
            closing_grace: u64::from(watch.closing_grace_s),
        }
    }

    /// Files smaller than this are not sampled between ticks: a file that was below it last time and grew past the
    /// growth limit since is measured against zero, which is never less than its true growth.
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
    /// It grew by this many bytes since the last tick.
    Growth(u64),
    /// It is this many bytes large.
    Size(u64),
}

/// Whether a file of `size` bytes, `previous` bytes at the last tick (none when it was not sampled), is runaway.
#[must_use]
pub fn runaway(size: u64, previous: Option<u64>, limits: &Limits) -> Option<Runaway> {
    if size >= limits.file {
        return Some(Runaway::Size(size));
    }
    let grown = size.saturating_sub(previous.unwrap_or(0));
    (grown >= limits.growth).then_some(Runaway::Growth(grown))
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
/// screen was not looked at before.
#[must_use]
pub const fn stuck(
    idle_s: Option<u64>,
    tool_in_flight: bool,
    screen_unchanged: Option<bool>,
    limits: &Limits,
) -> Stuck {
    let quiet = match idle_s {
        Some(idle) => idle >= limits.stuck_after,
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

/// Whether a merged task has lingered long enough to start closing.
#[must_use]
pub const fn merged_grace_over(age_s: u64, limits: &Limits) -> bool {
    age_s >= limits.merged_grace
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        disk_warn: 15,
        disk_stop: 5,
        growth: 100,
        file: 1000,
        stuck_after: 60,
        spawn_timeout: 120,
        ask_remind: 600,
        merged_grace: 300,
        closing_grace: 60,
    };

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
    fn runaway_is_size_or_growth() {
        for (size, previous, expected) in [
            (50, None, None),
            (99, Some(0), None),
            (100, None, Some(Runaway::Growth(100))),
            (500, Some(350), Some(Runaway::Growth(150))),
            (500, Some(450), None),
            (1000, Some(1000), Some(Runaway::Size(1000))),
            (900, Some(950), None),
        ] {
            assert_eq!(
                runaway(size, previous, &LIMITS),
                expected,
                "size {size}, before {previous:?}"
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
                stuck(idle, tool, same, &LIMITS),
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
        assert!(!merged_grace_over(299, &LIMITS), "just under");
        assert!(merged_grace_over(300, &LIMITS), "at the limit");
    }

    #[test]
    fn limits_convert_the_config_units() {
        let limits = Limits::from_config(&WatchConfig::default());
        assert_eq!(limits.disk_warn, 15 * GIB, "disk warning");
        assert_eq!(limits.stuck_after, 30 * 60, "stuck after");
        assert_eq!(limits.spawn_timeout, 120, "spawn timeout");
        assert_eq!(limits.ask_remind, 120 * 60, "ask reminder");
        assert_eq!(limits.merged_grace, 10 * 60, "merged grace");
        assert_eq!(limits.sample_floor(), GIB / 4, "sample floor");
    }
}
