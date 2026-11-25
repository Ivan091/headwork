//! The record of migration notes: settling, replacing, per-repository reads and the events.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use headwork_core::Actor;
use headwork_core::migrate::Settled;
use headwork_store::{EVENT_MIGRATION_APPLIED, EVENT_MIGRATION_SKIPPED, EventFilter, Store};

fn kinds(store: &Store) -> Vec<String> {
    store
        .events(EventFilter {
            task: None,
            limit: 50,
        })
        .unwrap()
        .into_iter()
        .map(|event| event.kind)
        .filter(|kind| kind.starts_with("migration-"))
        .collect()
}

#[test]
fn a_settled_note_is_read_back_for_its_repository_only() {
    let mut store = Store::open_in_memory().unwrap();
    store
        .settle_migration(
            "headwork",
            "a",
            Settled::Applied,
            "out",
            "check passed",
            Actor::Worker,
        )
        .unwrap();
    store
        .settle_migration(
            "headwork",
            "b",
            Settled::Skipped,
            "",
            "not needed",
            Actor::Orchestrator,
        )
        .unwrap();
    store
        .settle_migration(
            "me-sim",
            "a",
            Settled::Skipped,
            "",
            "other repo",
            Actor::Orchestrator,
        )
        .unwrap();
    let headwork = store.settled_migrations("headwork").unwrap();
    assert_eq!(headwork.get("a"), Some(&Settled::Applied), "a applied");
    assert_eq!(headwork.get("b"), Some(&Settled::Skipped), "b skipped");
    assert_eq!(
        store.settled_migrations("me-sim").unwrap().get("a"),
        Some(&Settled::Skipped),
        "same id in another repository"
    );
    assert!(
        store.settled_migrations("none").unwrap().is_empty(),
        "no record"
    );
}

#[test]
fn settling_again_replaces_the_record() {
    let mut store = Store::open_in_memory().unwrap();
    store
        .settle_migration("r", "a", Settled::Skipped, "", "first", Actor::Orchestrator)
        .unwrap();
    store
        .settle_migration("r", "a", Settled::Applied, "done", "second", Actor::Worker)
        .unwrap();
    let rows = store.migration_rows().unwrap();
    assert_eq!(rows.len(), 1, "one row per repository and note");
    let row = rows.first().unwrap();
    assert_eq!(row.state, Settled::Applied, "state");
    assert_eq!(row.output, "done", "output");
    assert_eq!(row.reason, "second", "reason");
}

#[test]
fn each_settlement_is_logged() {
    let mut store = Store::open_in_memory().unwrap();
    store
        .settle_migration("r", "a", Settled::Applied, "", "", Actor::Worker)
        .unwrap();
    store
        .settle_migration("r", "b", Settled::Skipped, "", "no", Actor::Worker)
        .unwrap();
    assert_eq!(
        kinds(&store),
        [EVENT_MIGRATION_APPLIED, EVENT_MIGRATION_SKIPPED],
        "events in order"
    );
}
