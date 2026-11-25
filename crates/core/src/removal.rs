//! When a failed worktree removal may be tried again: pure backoff over the failure count and the time since the last
//! attempt.
//!
//! Only a transient failure (a file held open, a lock) backs off; any other refusal, such as a tree that still
//! holds work, asks the user at once, since trying again cannot change it.

/// Failures after which headwork stops retrying and asks the user to keep or delete the worktree.
pub const ASK_AFTER_FAILURES: u32 = 6;

/// The longest wait between two attempts, in seconds.
pub const MAX_BACKOFF_S: u64 = 60 * 60;

/// What to do about a worktree whose removal may have failed before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovalDecision {
    /// Try the removal now.
    Try,
    /// An earlier failure is still backing off.
    Wait {
        /// Seconds left of the backoff.
        remaining_s: u64,
    },
    /// Too many failures: nothing is retried until the user answers the keep-or-delete ask.
    Ask,
}

/// The wait after `failures` failed attempts: 1, 2, 4, 8 ... minutes, at most [`MAX_BACKOFF_S`]. No failure, no wait.
#[must_use]
pub const fn backoff_s(failures: u32) -> u64 {
    if failures == 0 {
        return 0;
    }
    let exponent = failures - 1;
    if exponent >= 6 {
        return MAX_BACKOFF_S;
    }
    let wait = 60_u64 << exponent;
    if wait > MAX_BACKOFF_S {
        MAX_BACKOFF_S
    } else {
        wait
    }
}

/// Error texts of failures that may pass on their own: Windows file locks, busy devices, git lock files.
const TRANSIENT_MARKERS: [&str; 7] = [
    "permission denied",
    "being used by another process",
    "resource busy",
    "access is denied",
    "index.lock",
    "unable to create",
    "directory not empty",
];

/// Whether a removal's error may pass on its own, so the removal is worth trying again later.
#[must_use]
pub fn is_transient(error: &str) -> bool {
    let error = error.to_lowercase();
    TRANSIENT_MARKERS
        .iter()
        .any(|marker| error.contains(marker))
}

/// Decides whether a removal that failed `failures` times, the last `since_last_s` seconds ago, is tried now. A
/// failure that is not `transient` asks at once.
#[must_use]
pub const fn decide(failures: u32, since_last_s: u64, transient: bool) -> RemovalDecision {
    if failures >= ASK_AFTER_FAILURES || (failures > 0 && !transient) {
        return RemovalDecision::Ask;
    }
    let wait = backoff_s(failures);
    if since_last_s >= wait {
        RemovalDecision::Try
    } else {
        RemovalDecision::Wait {
            remaining_s: wait - since_last_s,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_from_a_minute_up_to_an_hour() {
        let minutes: Vec<u64> = (0..=8).map(|n| backoff_s(n) / 60).collect();
        assert_eq!(
            minutes,
            [0, 1, 2, 4, 8, 16, 32, 60, 60],
            "the waits in minutes"
        );
    }

    #[test]
    fn decisions_follow_the_count_and_the_clock() {
        let table = [
            (0, 0, RemovalDecision::Try),
            (1, 59, RemovalDecision::Wait { remaining_s: 1 }),
            (1, 60, RemovalDecision::Try),
            (2, 60, RemovalDecision::Wait { remaining_s: 60 }),
            (3, 240, RemovalDecision::Try),
            (5, 900, RemovalDecision::Wait { remaining_s: 60 }),
            (5, 960, RemovalDecision::Try),
            (6, 100_000, RemovalDecision::Ask),
            (9, 0, RemovalDecision::Ask),
        ];
        for (failures, since, expected) in table {
            assert_eq!(
                decide(failures, since, true),
                expected,
                "{failures} failures, {since}s ago"
            );
        }
    }

    #[test]
    fn a_lasting_refusal_asks_at_once() {
        assert_eq!(
            decide(1, 0, false),
            RemovalDecision::Ask,
            "a dirty tree asks on the first failure"
        );
        assert_eq!(
            decide(0, 0, false),
            RemovalDecision::Try,
            "no failure yet, so try"
        );
    }

    #[test]
    fn errors_are_sorted_into_transient_and_lasting() {
        let table = [
            ("error: failed to delete 'x': Permission denied", true),
            (
                "The process cannot access the file because it is being used by another process.",
                true,
            ),
            (
                "fatal: Unable to create 'S:/r/.git/worktrees/x/index.lock': File exists.",
                true,
            ),
            ("blocked: x has uncommitted changes", false),
            (
                "fatal: 'x' contains modified or untracked files, use --force to delete it",
                false,
            ),
        ];
        for (error, expected) in table {
            assert_eq!(is_transient(error), expected, "{error}");
        }
    }
}
