//! When a failed worktree removal may be tried again: pure backoff over the failure count and the time since the last
//! attempt.
//!
//! Only a transient failure (a file held open, a lock) backs off, 1, 2, 4, 8 and 16 minutes, and asks the user after
//! the sixth failure; any other refusal, such as a tree that still holds work, asks at once, since trying again
//! cannot change it.

/// Failures after which headwork stops retrying and asks the user to keep or delete the worktree.
pub const ASK_AFTER_FAILURES: u32 = 6;

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
    /// Too many failures, or one that cannot pass: nothing is retried until the user answers the keep-or-delete ask.
    Ask,
}

/// How a failed removal's error is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovalFailure {
    /// It may pass on its own: a file held open, a lock.
    Transient,
    /// Git does not know the folder as a worktree.
    NotAWorktree,
    /// Trying again cannot change it, such as a tree that still holds work.
    Lasting,
}

/// The wait after `failures` failed attempts: 1, 2, 4, 8, 16 minutes. No failure, no wait.
#[must_use]
pub const fn backoff_s(failures: u32) -> u64 {
    if failures == 0 {
        return 0;
    }
    let exponent = failures - 1;
    let last = ASK_AFTER_FAILURES - 1;
    60_u64 << if exponent > last { last } else { exponent }
}

const TRANSIENT_MARKERS: [&str; 15] = [
    "permission denied",
    "odmowa dost",
    "przez inny proces",
    "being used by another process",
    "resource busy",
    "access is denied",
    "index.lock",
    "unable to create",
    "directory not empty",
    "ebusy",
    "eacces",
    "os error 5",
    "os error 16",
    "os error 32",
    "os error 145",
];

const NOT_A_WORKTREE_MARKERS: [&str; 2] = ["is not a worktree", "is not a working tree"];

/// Sorts a removal's error: a not-a-worktree text, then a transient marker, else lasting.
///
/// The markers are English and Polish texts and the OS error codes of Rust's own I/O errors; the Polish ones carry no
/// diacritics because a command's output in a Windows code page reaches us lossily decoded.
#[must_use]
pub fn classify(error: &str) -> RemovalFailure {
    let error = error.to_lowercase();
    if NOT_A_WORKTREE_MARKERS
        .iter()
        .any(|marker| error.contains(marker))
    {
        RemovalFailure::NotAWorktree
    } else if TRANSIENT_MARKERS
        .iter()
        .any(|marker| error.contains(marker))
    {
        RemovalFailure::Transient
    } else {
        RemovalFailure::Lasting
    }
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
    fn backoff_doubles_from_a_minute() {
        let minutes: Vec<u64> = (0..=8).map(|n| backoff_s(n) / 60).collect();
        assert_eq!(
            minutes,
            [0, 1, 2, 4, 8, 16, 32, 32, 32],
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
    fn every_transient_marker_is_transient() {
        for marker in TRANSIENT_MARKERS {
            assert_eq!(
                classify(&format!("fatal: {}", marker.to_uppercase())),
                RemovalFailure::Transient,
                "{marker}"
            );
        }
        for error in [
            "The process cannot access the file because it is being used by another process.",
            "error: failed to delete 'x': Permission denied",
            "rmdir failed: Directory not empty",
            "Access is denied. (os error 5)",
            "fatal: Unable to create 'S:/r/.git/worktrees/x/index.lock': File exists.",
            "Odmowa dostępu.",
            "Odmowa dost\u{fffd}pu.",
            "Proces nie może uzyskać dostępu do pliku, ponieważ jest on używany przez inny proces.",
            "Proces nie mo\u{fffd}e uzyska\u{fffd} dost\u{fffd}pu do pliku, poniewa\u{fffd} jest on u\u{fffd}ywany przez inny proces.",
        ] {
            assert_eq!(classify(error), RemovalFailure::Transient, "{error}");
        }
    }

    #[test]
    fn lasting_errors_are_not_transient() {
        for error in [
            "blocked: x has uncommitted changes",
            "fatal: 'x' contains modified or untracked files, use --force to delete it",
            "uncommitted changes",
            "the remove command left the folder in place",
        ] {
            assert_eq!(classify(error), RemovalFailure::Lasting, "{error}");
        }
    }

    #[test]
    fn a_folder_git_does_not_know_is_not_a_worktree() {
        for error in [
            "fatal: 'x' is not a working tree",
            "fatal: 'x' IS NOT A WORKTREE",
        ] {
            assert_eq!(classify(error), RemovalFailure::NotAWorktree, "{error}");
        }
    }
}
