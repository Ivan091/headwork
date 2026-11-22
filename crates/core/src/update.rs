//! Whether `headwork update` installs a release, and which (`docs/autoupdate.md`, "Decisions in `core`").
//!
//! The runner (`headwork/src/ops/update.rs`) gathers the facts: the config, the installed build, the newest release
//! GitHub names, the last failure kept in `update.json`, the machine mode and free disk. [`decide`] turns them into
//! one [`UpdateStep`], which the runner carries out.
//!
//! Failures are of two kinds. A deterministic one (hash mismatch, smoke test, install check) latches on its release: it
//! is not tried again here until a newer one appears. A transient one (network, rate limit, timeout) never latches:
//! the next try waits 5, 15, then 60 minutes.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::vocabulary::MachineMode;
use crate::watch::DiskLevel;

/// A release version, `<major>.<minor>.<patch>`, compared by number; releases are `0.1.<run>`, tagged `v0.1.<run>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    /// The version `<major>.<minor>.<patch>`.
    #[must_use]
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Reads `0.1.214`; `None` for anything else.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut fields = text.split('.');
        let mut number = || -> Option<u64> {
            let field = fields.next()?;
            if field.is_empty() || !field.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            field.parse().ok()
        };
        let version = Self::new(number()?, number()?, number()?);
        fields.next().is_none().then_some(version)
    }

    /// Reads a release tag, `v0.1.214`; `None` for anything else.
    #[must_use]
    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::parse(tag.strip_prefix('v')?)
    }

    /// The release tag, `v0.1.214`.
    #[must_use]
    pub fn tag(self) -> String {
        format!("v{self}")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl TryFrom<String> for Version {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::from_tag(&text)
            .or_else(|| Self::parse(&text))
            .ok_or_else(|| format!("`{text}` is not a release tag such as v0.1.7"))
    }
}

impl From<Version> for String {
    fn from(version: Version) -> Self {
        version.tag()
    }
}

/// What `headwork --version` prints after the program's name: `<version> (<commit>, schema <n>)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BuildLine {
    /// The version, `0.1.214`, or the package version `0.1.0` of a build the release workflow did not make.
    pub version: String,
    /// The commit, or `dev`.
    pub commit: String,
    /// The store schema the build knows.
    pub schema: u32,
}

impl BuildLine {
    /// Reads `headwork 0.1.214 (<commit>, schema 14)`, the whole first line `--version` prints; `None` when it is not
    /// that.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let line = text.lines().next()?.trim();
        let rest = line.strip_prefix("headwork ")?;
        let (version, inner) = rest.split_once(" (")?;
        let inner = inner.strip_suffix(')')?;
        let (commit, schema) = inner.split_once(", schema ")?;
        if version.is_empty() || commit.is_empty() || commit.contains(' ') {
            return None;
        }
        Some(Self {
            version: version.to_owned(),
            commit: commit.to_owned(),
            schema: schema.parse().ok()?,
        })
    }

    /// Whether the build is a local one, not a release.
    #[must_use]
    pub fn is_dev(&self) -> bool {
        self.commit == "dev"
    }

    /// The release version of a release build; `None` for a local build.
    #[must_use]
    pub fn release(&self) -> Option<Version> {
        if self.is_dev() {
            None
        } else {
            Version::parse(&self.version)
        }
    }
}

/// The installed build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installed {
    /// A release build.
    Release(Version),
    /// A local build installed by hand (a worker trying a build), with the time its file was written, in milliseconds
    /// since the Unix epoch.
    Dev {
        /// When the file was installed.
        installed_ms: u64,
    },
}

/// A release GitHub names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Release {
    /// Its version.
    pub version: Version,
    /// When it was published, in milliseconds since the Unix epoch.
    pub published_ms: u64,
}

/// How an update failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FailureKind {
    /// The release itself is broken here (hash mismatch, smoke test, install check): latched on the release.
    Deterministic,
    /// The network, GitHub's rate limit, a timeout or the disk: retried after a back-off.
    Transient,
}

impl FailureKind {
    /// The spelling in `update.json` and events.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Deterministic => "deterministic",
            Self::Transient => "transient",
        }
    }
}

/// The last failed update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failure {
    /// The release it tried, when it got as far as knowing one.
    pub release: Option<Version>,
    /// Its kind.
    pub kind: FailureKind,
    /// How many updates in a row failed this way (transient) or on this release (deterministic), from 1.
    pub attempts: u32,
    /// When, in milliseconds since the Unix epoch.
    pub at_ms: u64,
}

/// The minutes a transient failure waits before the next try: 5 after the first, 15 after the second, 60 after any
/// later one.
pub const BACKOFF_MIN: [u64; 3] = [5, 15, 60];

/// How long the next try waits after `attempts` transient failures in a row, in milliseconds.
#[must_use]
pub fn backoff_ms(attempts: u32) -> u64 {
    let index = usize::try_from(attempts.saturating_sub(1))
        .unwrap_or(usize::MAX)
        .min(BACKOFF_MIN.len() - 1);
    BACKOFF_MIN
        .get(index)
        .copied()
        .unwrap_or(60)
        .saturating_mul(60_000)
}

/// The failure to record after `previous`: attempts count on while the kind and, for a deterministic failure, the
/// release stay the same, and start again at 1 otherwise.
#[must_use]
pub fn next_failure(
    previous: Option<&Failure>,
    release: Option<Version>,
    kind: FailureKind,
    at_ms: u64,
) -> Failure {
    let continues = previous.is_some_and(|previous| {
        previous.kind == kind && (kind == FailureKind::Transient || previous.release == release)
    });
    let attempts = previous
        .filter(|_| continues)
        .map_or(1, |previous| previous.attempts.saturating_add(1));
    Failure {
        release,
        kind,
        attempts,
        at_ms,
    }
}

/// Everything [`decide`] looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateFacts {
    /// `[update] enabled`.
    pub enabled: bool,
    /// The release was named by hand (`--to`): it is installed newer or older, even with updates off, during a
    /// back-off, or after it failed here.
    pub requested: bool,
    /// The installed build.
    pub installed: Installed,
    /// The newest release, or the one `--to` names. `None` when GitHub has not been asked yet or names none: then
    /// [`decide`] says only whether anything rules an update out, and [`UpdateStep::UpToDate`] means "go and ask".
    pub release: Option<Release>,
    /// The last failure, from `update.json`; a success clears it.
    pub last_failure: Option<Failure>,
    /// The machine mode.
    pub machine: MachineMode,
    /// How full the home folder's volume is.
    pub disk: DiskLevel,
    /// Now, in milliseconds since the Unix epoch.
    pub now_ms: u64,
}

/// Why no release is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// `[update] enabled` is off.
    Off,
    /// A park or a resume is running.
    Machine(MachineMode),
    /// The home folder's volume is below `[watch] disk_stop_gb`.
    DiskStop,
    /// This release already failed here, deterministically.
    Failed(Version),
    /// A local build installed after the newest release was published stays until a newer release.
    DevBuild,
}

impl SkipReason {
    /// The short stable word in events and JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Machine(_) => "machine-busy",
            Self::DiskStop => "disk-stop",
            Self::Failed(_) => "failed-here",
            Self::DevBuild => "dev-build",
        }
    }

    /// Whether the skip is worth an `update-skipped` event; updates being off is not.
    #[must_use]
    pub const fn worth_logging(self) -> bool {
        !matches!(self, Self::Off)
    }
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => f.write_str("updates are off ([update] enabled = false)"),
            Self::Machine(mode) => write!(f, "the machine is {}", mode.as_str()),
            Self::DiskStop => f.write_str("free disk space is below [watch] disk_stop_gb"),
            Self::Failed(version) => write!(
                f,
                "{} already failed here; it is not tried again until a newer release",
                version.tag()
            ),
            Self::DevBuild => f.write_str(
                "a local build installed after the newest release stays until a newer release is published",
            ),
        }
    }
}

/// What to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateStep {
    /// The installed build is the newest release, or newer.
    UpToDate,
    /// Something rules the update out.
    Skip(SkipReason),
    /// A transient failure backs off until then, in milliseconds since the Unix epoch.
    Wait {
        /// When the next try may run.
        until: u64,
    },
    /// Install this release.
    Install {
        /// The release.
        tag: Version,
    },
}

/// Decides what `headwork update` does with these facts.
#[must_use]
pub fn decide(facts: &UpdateFacts) -> UpdateStep {
    if !facts.enabled && !facts.requested {
        return UpdateStep::Skip(SkipReason::Off);
    }
    if matches!(facts.machine, MachineMode::Parking | MachineMode::Resuming) {
        return UpdateStep::Skip(SkipReason::Machine(facts.machine));
    }
    if facts.disk == DiskLevel::Stop {
        return UpdateStep::Skip(SkipReason::DiskStop);
    }
    if !facts.requested
        && let Some(failure) = facts.last_failure
        && failure.kind == FailureKind::Transient
    {
        let until = failure.at_ms.saturating_add(backoff_ms(failure.attempts));
        if until > facts.now_ms {
            return UpdateStep::Wait { until };
        }
    }
    let Some(release) = facts.release else {
        return UpdateStep::UpToDate;
    };
    if facts.installed == Installed::Release(release.version) {
        return UpdateStep::UpToDate;
    }
    if facts.requested {
        return UpdateStep::Install {
            tag: release.version,
        };
    }
    if let Some(failure) = facts.last_failure
        && failure.kind == FailureKind::Deterministic
        && failure.release == Some(release.version)
    {
        return UpdateStep::Skip(SkipReason::Failed(release.version));
    }
    match facts.installed {
        Installed::Release(version) if release.version > version => UpdateStep::Install {
            tag: release.version,
        },
        Installed::Release(_) => UpdateStep::UpToDate,
        Installed::Dev { installed_ms } if release.published_ms > installed_ms => {
            UpdateStep::Install {
                tag: release.version,
            }
        }
        Installed::Dev { .. } => UpdateStep::Skip(SkipReason::DevBuild),
    }
}

/// Whether a release whose build knows `release_schema` may open a store at `store_schema`: a release that knows an
/// older schema than the store has would be refused by it, so `--to` never installs one.
#[must_use]
pub const fn knows_store(release_schema: u32, store_schema: u32) -> bool {
    release_schema >= store_schema
}

/// Whether the tick starts `headwork update` now: updates are on, the tick trigger is not turned off with
/// `poll_min = 0`, and GitHub was not asked within the last `poll_min` minutes (or never).
#[must_use]
pub fn tick_polls(enabled: bool, poll_min: u32, last_poll_ms: Option<u64>, now_ms: u64) -> bool {
    if !enabled || poll_min == 0 {
        return false;
    }
    last_poll_ms.is_none_or(|last| {
        now_ms.saturating_sub(last) >= u64::from(poll_min).saturating_mul(60_000)
    })
}

/// Reads a UTC time as GitHub writes it, `2026-11-22T10:15:30Z` (fractions and `+hh:mm` offsets too), into
/// milliseconds since the Unix epoch; `None` for anything else or a time before 1970.
#[must_use]
pub fn parse_utc(text: &str) -> Option<u64> {
    let text = text.trim();
    let field = |from: usize, to: usize| -> Option<i64> {
        let part = text.get(from..to)?;
        if part.bytes().all(|b| b.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    let separators = [(4, b'-'), (7, b'-'), (13, b':'), (16, b':')];
    if separators
        .iter()
        .any(|(at, byte)| text.as_bytes().get(*at) != Some(byte))
        || !matches!(text.as_bytes().get(10), Some(b'T' | b't' | b' '))
    {
        return None;
    }
    let (year, month, day) = (field(0, 4)?, field(5, 7)?, field(8, 10)?);
    let (hour, minute, second) = (field(11, 13)?, field(14, 16)?, field(17, 19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut rest = text.get(19..)?;
    let mut millis = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let padded = format!("{:0<3}", fraction.get(..digits.min(3))?);
        millis = padded.parse::<i64>().ok()?;
        rest = fraction.get(digits..)?;
    }
    let offset_s = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes().first() {
                Some(b'+') => 1,
                Some(b'-') => -1,
                _ => return None,
            };
            let (hours, minutes) = rest.get(1..)?.split_once(':')?;
            if hours.len() != 2 || minutes.len() != 2 {
                return None;
            }
            sign * (hours.parse::<i64>().ok()? * 3600 + minutes.parse::<i64>().ok()? * 60)
        }
    };
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_s;
    u64::try_from(seconds * 1000 + millis).ok()
}

/// Writes milliseconds since the Unix epoch as a UTC time, `2026-11-22T10:15:30Z`.
#[must_use]
pub fn format_utc(ms: u64) -> String {
    let seconds = i64::try_from(ms / 1000).unwrap_or(i64::MAX);
    let (days, of_day) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        of_day / 3600,
        of_day % 3600 / 60,
        of_day % 60
    )
}

/// Days since 1970-01-01 of a date in the proleptic Gregorian calendar (Howard Hinnant's `days_from_civil`).
const fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_from_march = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The date of a day counted from 1970-01-01 (Howard Hinnant's `civil_from_days`).
const fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_era + era * 400 + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tick_polls_only_when_on_and_due() {
        let minute = 60_000;
        let cases = [
            (false, 15, None, 0, false),
            (true, 0, None, 0, false),
            (true, 15, None, 0, true),
            (true, 15, Some(0), 14 * minute, false),
            (true, 15, Some(0), 15 * minute, true),
            (true, 15, Some(20 * minute), minute, false),
        ];
        for (enabled, poll_min, last, now, polls) in cases {
            assert_eq!(
                tick_polls(enabled, poll_min, last, now),
                polls,
                "enabled {enabled}, poll_min {poll_min}, last {last:?}, now {now}"
            );
        }
    }

    const MIN: u64 = 60_000;
    const NOW: u64 = 1_000 * MIN;

    fn v(patch: u64) -> Version {
        Version::new(0, 1, patch)
    }

    fn release(patch: u64, published_ms: u64) -> Release {
        Release {
            version: v(patch),
            published_ms,
        }
    }

    fn failure(release: Option<u64>, kind: FailureKind, attempts: u32, ago: u64) -> Failure {
        Failure {
            release: release.map(v),
            kind,
            attempts,
            at_ms: NOW - ago,
        }
    }

    fn base() -> UpdateFacts {
        UpdateFacts {
            enabled: true,
            requested: false,
            installed: Installed::Release(v(5)),
            release: Some(release(6, NOW - 10 * MIN)),
            last_failure: None,
            machine: MachineMode::Normal,
            disk: DiskLevel::Ok,
            now_ms: NOW,
        }
    }

    #[test]
    #[expect(clippy::too_many_lines, reason = "one row per branch of the decision")]
    fn every_branch_of_the_decision() {
        let install = |patch| UpdateStep::Install { tag: v(patch) };
        let cases: Vec<(&str, UpdateFacts, UpdateStep)> = vec![
            ("a newer release installs", base(), install(6)),
            (
                "off skips",
                UpdateFacts {
                    enabled: false,
                    ..base()
                },
                UpdateStep::Skip(SkipReason::Off),
            ),
            (
                "off still installs a release named by hand",
                UpdateFacts {
                    enabled: false,
                    requested: true,
                    ..base()
                },
                install(6),
            ),
            (
                "a park skips",
                UpdateFacts {
                    machine: MachineMode::Parking,
                    ..base()
                },
                UpdateStep::Skip(SkipReason::Machine(MachineMode::Parking)),
            ),
            (
                "a resume skips, even by hand",
                UpdateFacts {
                    machine: MachineMode::Resuming,
                    requested: true,
                    ..base()
                },
                UpdateStep::Skip(SkipReason::Machine(MachineMode::Resuming)),
            ),
            (
                "a parked machine may update",
                UpdateFacts {
                    machine: MachineMode::Parked,
                    ..base()
                },
                install(6),
            ),
            (
                "a full disk skips",
                UpdateFacts {
                    disk: DiskLevel::Stop,
                    ..base()
                },
                UpdateStep::Skip(SkipReason::DiskStop),
            ),
            (
                "a low disk does not",
                UpdateFacts {
                    disk: DiskLevel::Warn,
                    ..base()
                },
                install(6),
            ),
            (
                "the installed release is up to date",
                UpdateFacts {
                    installed: Installed::Release(v(6)),
                    ..base()
                },
                UpdateStep::UpToDate,
            ),
            (
                "an older release is not installed",
                UpdateFacts {
                    installed: Installed::Release(v(9)),
                    ..base()
                },
                UpdateStep::UpToDate,
            ),
            (
                "an older release named by hand is a rollback",
                UpdateFacts {
                    installed: Installed::Release(v(9)),
                    requested: true,
                    ..base()
                },
                install(6),
            ),
            (
                "the installed release named by hand is up to date",
                UpdateFacts {
                    installed: Installed::Release(v(6)),
                    requested: true,
                    ..base()
                },
                UpdateStep::UpToDate,
            ),
            (
                "no release known yet: nothing rules it out",
                UpdateFacts {
                    release: None,
                    ..base()
                },
                UpdateStep::UpToDate,
            ),
            (
                "a release that failed here deterministically is latched",
                UpdateFacts {
                    last_failure: Some(failure(Some(6), FailureKind::Deterministic, 1, 500 * MIN)),
                    ..base()
                },
                UpdateStep::Skip(SkipReason::Failed(v(6))),
            ),
            (
                "a newer release than the latched one installs",
                UpdateFacts {
                    release: Some(release(7, NOW)),
                    last_failure: Some(failure(Some(6), FailureKind::Deterministic, 1, MIN)),
                    ..base()
                },
                install(7),
            ),
            (
                "a latched release named by hand is tried again",
                UpdateFacts {
                    requested: true,
                    last_failure: Some(failure(Some(6), FailureKind::Deterministic, 2, MIN)),
                    ..base()
                },
                install(6),
            ),
            (
                "a first transient failure waits 5 minutes",
                UpdateFacts {
                    last_failure: Some(failure(Some(6), FailureKind::Transient, 1, 2 * MIN)),
                    ..base()
                },
                UpdateStep::Wait {
                    until: NOW + 3 * MIN,
                },
            ),
            (
                "after 5 minutes it tries again",
                UpdateFacts {
                    last_failure: Some(failure(Some(6), FailureKind::Transient, 1, 5 * MIN)),
                    ..base()
                },
                install(6),
            ),
            (
                "a second transient failure waits 15 minutes",
                UpdateFacts {
                    last_failure: Some(failure(None, FailureKind::Transient, 2, 14 * MIN)),
                    ..base()
                },
                UpdateStep::Wait { until: NOW + MIN },
            ),
            (
                "a fifth transient failure waits 60 minutes",
                UpdateFacts {
                    last_failure: Some(failure(None, FailureKind::Transient, 5, 59 * MIN)),
                    ..base()
                },
                UpdateStep::Wait { until: NOW + MIN },
            ),
            (
                "the back-off holds before GitHub is asked",
                UpdateFacts {
                    release: None,
                    last_failure: Some(failure(None, FailureKind::Transient, 3, 0)),
                    ..base()
                },
                UpdateStep::Wait {
                    until: NOW + 60 * MIN,
                },
            ),
            (
                "a release named by hand does not wait",
                UpdateFacts {
                    requested: true,
                    last_failure: Some(failure(None, FailureKind::Transient, 1, 0)),
                    ..base()
                },
                install(6),
            ),
            (
                "a dev build gives way to a release published after it",
                UpdateFacts {
                    installed: Installed::Dev {
                        installed_ms: NOW - 20 * MIN,
                    },
                    ..base()
                },
                install(6),
            ),
            (
                "a dev build installed after the release stays",
                UpdateFacts {
                    installed: Installed::Dev {
                        installed_ms: NOW - 5 * MIN,
                    },
                    ..base()
                },
                UpdateStep::Skip(SkipReason::DevBuild),
            ),
            (
                "a dev build gives way to a release named by hand",
                UpdateFacts {
                    installed: Installed::Dev { installed_ms: NOW },
                    requested: true,
                    ..base()
                },
                install(6),
            ),
        ];
        for (name, facts, step) in cases {
            assert_eq!(decide(&facts), step, "{name}");
        }
    }

    #[test]
    fn only_skips_worth_reading_are_logged() {
        for (reason, logged) in [
            (SkipReason::Off, false),
            (SkipReason::Machine(MachineMode::Parking), true),
            (SkipReason::DiskStop, true),
            (SkipReason::Failed(v(1)), true),
            (SkipReason::DevBuild, true),
        ] {
            assert_eq!(reason.worth_logging(), logged, "{reason}");
        }
    }

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(v(10) > v(9), "10 after 9");
        assert!(Version::new(0, 2, 0) > v(999), "minor before patch");
        for (text, version) in [
            ("v0.1.214", Some(v(214))),
            ("v0.1.0", Some(v(0))),
            ("0.1.7", None),
            ("v0.1", None),
            ("v0.1.7.1", None),
            ("v0.1.x", None),
            ("v0.1.+7", None),
            ("v0..7", None),
        ] {
            assert_eq!(Version::from_tag(text), version, "{text}");
        }
        assert_eq!(Version::parse("0.1.7"), Some(v(7)), "plain version");
        assert_eq!(v(7).tag(), "v0.1.7", "tag");
        assert_eq!(v(7).to_string(), "0.1.7", "display");
    }

    #[test]
    fn version_lines_are_read() {
        let release =
            BuildLine::parse("headwork 0.1.214 (abc123, schema 14)\n").unwrap_or_else(|| {
                BuildLine {
                    version: String::new(),
                    commit: String::new(),
                    schema: 0,
                }
            });
        assert_eq!(release.release(), Some(v(214)), "release");
        assert_eq!(release.schema, 14, "schema");
        assert!(!release.is_dev(), "not dev");
        let dev = BuildLine::parse("headwork 0.1.0 (dev, schema 13)");
        assert_eq!(dev.as_ref().map(BuildLine::is_dev), Some(true), "dev");
        assert_eq!(dev.and_then(|dev| dev.release()), None, "no release");
        for bad in [
            "",
            "headwork",
            "headwork 0.1.0",
            "headwork 0.1.0 (dev)",
            "headwork 0.1.0 (dev, schema x)",
            "other 0.1.0 (dev, schema 1)",
            "headwork  (dev, schema 1)",
        ] {
            assert_eq!(BuildLine::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn back_off_grows_and_stops_at_an_hour() {
        for (attempts, minutes) in [(0, 5), (1, 5), (2, 15), (3, 60), (40, 60)] {
            assert_eq!(backoff_ms(attempts), minutes * MIN, "{attempts}");
        }
    }

    #[test]
    fn failures_count_on_while_they_repeat() {
        let transient = failure(Some(6), FailureKind::Transient, 2, MIN);
        let latched = failure(Some(6), FailureKind::Deterministic, 1, MIN);
        let cases = [
            ("first", None, Some(6), FailureKind::Transient, 1),
            (
                "transient again, any release",
                Some(transient),
                Some(7),
                FailureKind::Transient,
                3,
            ),
            (
                "deterministic after transient",
                Some(transient),
                Some(6),
                FailureKind::Deterministic,
                1,
            ),
            (
                "same release again",
                Some(latched),
                Some(6),
                FailureKind::Deterministic,
                2,
            ),
            (
                "another release",
                Some(latched),
                Some(7),
                FailureKind::Deterministic,
                1,
            ),
            (
                "transient after deterministic",
                Some(latched),
                Some(6),
                FailureKind::Transient,
                1,
            ),
        ];
        for (name, previous, release, kind, attempts) in cases {
            let next = next_failure(previous.as_ref(), release.map(v), kind, NOW);
            assert_eq!(next.attempts, attempts, "{name}");
            assert_eq!(next.at_ms, NOW, "{name}: time");
            assert_eq!(next.kind, kind, "{name}: kind");
        }
    }

    #[test]
    fn a_release_must_know_the_store_schema() {
        assert!(knows_store(14, 14), "same");
        assert!(knows_store(15, 14), "newer");
        assert!(!knows_store(13, 14), "older");
    }

    #[test]
    fn utc_times_round_trip() {
        for (text, ms) in [
            ("1970-01-01T00:00:00Z", Some(0)),
            ("2026-11-22T10:15:30Z", Some(1_795_342_530_000)),
            ("2026-11-22T10:15:30.25Z", Some(1_795_342_530_250)),
            ("2026-11-22T11:15:30+01:00", Some(1_795_342_530_000)),
            ("2000-02-29T00:00:00Z", Some(951_782_400_000)),
            ("1969-12-31T23:59:59Z", None),
            ("2026-13-01T00:00:00Z", None),
            ("2026-11-22 10:15:30", None),
            ("2026-11-22T10:15:30.Z", None),
            ("2026-11-22", None),
            ("", None),
        ] {
            assert_eq!(parse_utc(text), ms, "{text}");
        }
        for ms in [0, 951_782_400_000, 1_795_342_530_000] {
            assert_eq!(parse_utc(&format_utc(ms)), Some(ms), "{ms}");
        }
        assert_eq!(
            format_utc(1_795_342_530_999),
            "2026-11-22T10:15:30Z",
            "seconds"
        );
    }
}
