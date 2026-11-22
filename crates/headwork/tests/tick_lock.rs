//! The tick lock: one holder at a time, a stamp for the holder, and a hung verdict that never takes the lock.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests fail loudly on an unexpected error"
)]

use headwork::ops::tick_lock::{Acquired, acquire, hung};

#[test]
fn a_second_run_sees_the_first_holder_and_the_lock_frees_on_drop() {
    let dir = tempfile::tempdir().unwrap();
    let Acquired::Held(lock) = acquire(dir.path(), 1000).unwrap() else {
        panic!("the lock was free");
    };
    let Acquired::Busy(Some(stamp)) = acquire(dir.path(), 2000).unwrap() else {
        panic!("the lock should be busy with a stamp");
    };
    assert_eq!(stamp.pid, std::process::id());
    assert_eq!(stamp.started_s, 1000);
    drop(lock);
    assert!(matches!(
        acquire(dir.path(), 3000).unwrap(),
        Acquired::Held(_)
    ));
}

#[test]
fn only_a_lock_held_past_the_threshold_is_hung_and_it_stays_held() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(hung(dir.path(), 10_000).unwrap(), None, "nobody holds it");
    let Acquired::Held(_lock) = acquire(dir.path(), 1000).unwrap() else {
        panic!("the lock was free");
    };
    assert_eq!(hung(dir.path(), 1100).unwrap(), None, "just started");
    let stamp = hung(dir.path(), 1000 + 3600).unwrap().unwrap();
    assert_eq!(stamp.started_s, 1000);
    assert!(
        matches!(acquire(dir.path(), 5000).unwrap(), Acquired::Busy(_)),
        "the lock was not taken from its holder"
    );
}
