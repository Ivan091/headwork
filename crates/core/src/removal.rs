//! When a failed worktree removal may be tried again: pure backoff over the failure count and the time since the last
//! attempt.

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

/// Decides whether a removal that failed `failures` times, the last `since_last_s` seconds ago, is tried now.
#[must_use]
pub const fn decide(failures: u32, since_last_s: u64) -> RemovalDecision {
    if failures >= ASK_AFTER_FAILURES {
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
                decide(failures, since),
                expected,
                "{failures} failures, {since}s ago"
            );
        }
    }
}
