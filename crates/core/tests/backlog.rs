//! The backlog reader and renderer against `tests/fixtures/backlog.md`, and a round trip over random items.

#![expect(clippy::unwrap_used, reason = "tests fail loudly on a bad fixture")]

use std::path::Path;

use headwork_core::backlog::{
    BacklogDate, BacklogDraft, BacklogItem, BacklogOwner, BacklogStatus, Block, BlockReason,
    ItemStatus, Parsed, ReturnTo, parse, render,
};
use proptest::prelude::*;

fn fixture() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/backlog.md");
    std::fs::read_to_string(path).unwrap()
}

fn rows(drafts: &[BacklogDraft]) -> Vec<BacklogItem> {
    drafts
        .iter()
        .zip(1..)
        .map(|(draft, id)| BacklogItem {
            id,
            position: id,
            task: None,
            draft: draft.clone(),
        })
        .collect()
}

fn count(parsed: &Parsed, status: ItemStatus) -> usize {
    parsed
        .items
        .iter()
        .filter(|item| item.status.kind() == status)
        .count()
}

#[test]
fn the_fixture_has_exact_counts() {
    let parsed = parse(&fixture());
    assert_eq!(count(&parsed, ItemStatus::Backlog), 9, "backlog");
    assert_eq!(count(&parsed, ItemStatus::Done), 6, "done");
    assert_eq!(
        count(&parsed, ItemStatus::Cancelled),
        4,
        "cancelled, written dropped"
    );
    assert_eq!(parsed.items.len(), 19, "all");
    let owners = parsed
        .items
        .iter()
        .filter(|item| item.owner == BacklogOwner::User)
        .count();
    assert_eq!(owners, 2, "For you items");
}

#[test]
fn the_fixture_reports_the_lines_it_cannot_place() {
    let parsed = parse(&fixture());
    let found: Vec<(usize, &str)> = parsed
        .unparsed
        .iter()
        .map(|line| (line.line_no, line.line.as_str()))
        .collect();
    assert_eq!(
        found,
        [
            (18, "stray words that belong to nothing"),
            (30, "- plain line outside Done and Dropped"),
            (31, "  - note under an unparsed line"),
        ]
    );
}

#[test]
fn the_fixture_fields_come_out_as_written() {
    let parsed = parse(&fixture());
    let by_text = |start: &str| {
        parsed
            .items
            .iter()
            .find(|item| item.text.starts_with(start))
            .unwrap()
    };
    let reels = by_text("Uploads: retry");
    assert_eq!(
        reels.not_before,
        Some(BacklogDate::parse("2026-12-15").unwrap())
    );
    assert_eq!(reels.due, Some(BacklogDate::parse("2026-12-31").unwrap()));
    assert_eq!(reels.added, Some(BacklogDate::parse("2026-11-11").unwrap()));
    assert!(!reels.text.contains('('), "{}", reels.text);
    let platform = by_text("Platform:");
    assert_eq!(
        platform.due,
        Some(BacklogDate::parse("2026-12-01").unwrap())
    );
    assert_eq!(platform.notes.len(), 2, "notes");
    let restore = by_text("me-sim: add the restore");
    assert_eq!(restore.status, BacklogStatus::Done);
    assert_eq!(
        restore.done_note.as_deref(),
        Some("me-sim PR #403; not yet run for real")
    );
    let audit = by_text("Check the fields");
    assert!(
        audit
            .text
            .contains("(audit 2, added 2026-11-17; see notes)"),
        "{}",
        audit.text
    );
    assert_eq!(audit.added, Some(BacklogDate::parse("2026-11-17").unwrap()));
    let open_with_done = by_text("Still open");
    assert_eq!(open_with_done.status, BacklogStatus::Backlog);
    assert!(
        open_with_done.text.contains("(done 2026-11-16"),
        "kept in an open item"
    );
    let cr = by_text("Audit item two");
    assert_eq!(cr.text, "Audit item two with a CR\rinside the line");
    assert_eq!(cr.section.as_deref(), Some("From the audit of 2026-11-15"));
    let first = parsed.items.first().unwrap();
    assert_eq!(first.project, "general", "before any heading");
    let superseded = by_text("decide the training goal");
    assert_eq!(
        superseded.status,
        BacklogStatus::Done,
        "a group that starts with done is done"
    );
    assert_eq!(
        superseded.done_note.as_deref(),
        Some("superseded, user chose another line")
    );
    assert_eq!(superseded.owner, BacklogOwner::User);
    let attribution = by_text("Check attribution");
    assert_eq!(
        attribution.done_on,
        Some(BacklogDate::parse("2026-11-22").unwrap())
    );
}

#[test]
fn the_fixture_round_trips_through_the_renderer() {
    let parsed = parse(&fixture());
    let written = render(&rows(&parsed.items));
    let again = parse(&written);
    assert!(again.unparsed.is_empty(), "{:?}\n{written}", again.unparsed);
    let wanted: Vec<_> = parsed
        .items
        .iter()
        .filter(|item| item.status != BacklogStatus::Cancelled)
        .collect();
    assert_eq!(again.items.len(), wanted.len(), "{written}");
    for want in wanted {
        let got = again
            .items
            .iter()
            .find(|item| item.text == want.text)
            .unwrap();
        assert_eq!(got.status, want.status, "{}", want.text);
        assert_eq!(got.owner, want.owner, "{}", want.text);
        assert_eq!(
            (&got.added, &got.not_before, &got.due, &got.done_on),
            (&want.added, &want.not_before, &want.due, &want.done_on),
            "{}",
            want.text
        );
    }
}

fn word() -> impl Strategy<Value = String> {
    "[A-Za-z][A-Za-z0-9:#,.-]{0,8}"
}

fn day() -> impl Strategy<Value = Option<BacklogDate>> {
    proptest::option::of(
        (2020..2030_u32, 1..13_u32, 1..29_u32).prop_map(|(year, month, day)| {
            BacklogDate::parse(&format!("{year}-{month:02}-{day:02}")).unwrap()
        }),
    )
}

fn draft() -> impl Strategy<Value = BacklogDraft> {
    (
        proptest::collection::vec(word(), 1..6),
        proptest::bool::ANY,
        proptest::bool::ANY,
        (day(), day(), day()),
        proptest::option::of("[a-z ]{1,12}"),
        proptest::collection::vec("[a-z]{1,10}", 0..3),
        proptest::option::of("[a-z]{1,6}"),
        (0..4_u8, proptest::option::of("[a-z][a-z ]{0,10}[a-z]")),
    )
        .prop_map(
            |(
                words,
                user,
                open,
                (added, not_before, due),
                note,
                notes,
                section,
                (which, block_note),
            )| {
                let text = words.join(" ");
                let status = match (open, which) {
                    (false, _) => BacklogStatus::Done,
                    (true, 0) => BacklogStatus::Backlog,
                    (true, 1) => BacklogStatus::InProgress,
                    (true, 2) => BacklogStatus::Blocked(Block {
                        reason: BlockReason::TaskLost,
                        note: block_note,
                        before: ReturnTo::InProgress,
                    }),
                    (true, _) => BacklogStatus::Blocked(Block {
                        reason: BlockReason::Event,
                        note: block_note,
                        before: ReturnTo::Backlog,
                    }),
                };
                let done_on = (!open).then(|| added.clone()).flatten();
                BacklogDraft {
                    project: "proj".to_owned(),
                    section: open.then_some(section).flatten(),
                    text,
                    notes,
                    status,
                    owner: if user {
                        BacklogOwner::User
                    } else {
                        BacklogOwner::Agent
                    },
                    added,
                    not_before: open.then_some(not_before).flatten(),
                    due: open.then_some(due).flatten(),
                    done_on,
                    done_note: (!open)
                        .then(|| note.map(|note| note.trim().to_owned()))
                        .flatten()
                        .filter(|note| !note.is_empty()),
                }
            },
        )
}

proptest! {
    #[test]
    fn render_then_parse_keeps_text_state_owner_and_dates(drafts in proptest::collection::vec(draft(), 1..8)) {
        let drafts: Vec<BacklogDraft> = drafts
            .into_iter()
            .enumerate()
            .map(|(index, mut draft)| {
                draft.text = format!("{} n{index}", draft.text);
                draft
            })
            .collect();
        let written = render(&rows(&drafts));
        let again = parse(&written);
        prop_assert!(again.unparsed.is_empty(), "{:?}\n{}", again.unparsed, written);
        let open: Vec<&BacklogDraft> = drafts.iter().filter(|d| d.status.is_open()).collect();
        let done: Vec<&BacklogDraft> = drafts.iter().filter(|d| d.status == BacklogStatus::Done).collect();
        let mut expected: Vec<&BacklogDraft> = open;
        expected.extend(done);
        prop_assert_eq!(again.items.len(), expected.len(), "{}", written);
        for want in expected {
            let got = again.items.iter().find(|item| item.text == want.text && item.status == want.status);
            prop_assert!(got.is_some(), "{} lost\n{}", want.text, written);
            let got = got.unwrap();
            prop_assert_eq!(got.owner, want.owner);
            prop_assert_eq!((&got.added, &got.not_before, &got.due), (&want.added, &want.not_before, &want.due));
            prop_assert_eq!(&got.done_on, &want.done_on);
        }
    }
}

#[test]
fn a_line_split_off_an_item_is_joined_back_and_reported() {
    let parsed = parse(
        "# Backlog\n\n## me-sim\n\n- [x] repoint the task at C:/x\r\nelease-hourly.vbs (done 2026-11-16: moved)\n",
    );
    assert_eq!(parsed.items.len(), 1, "{:?}", parsed.items);
    let item = &parsed.items[0];
    assert_eq!(
        item.text, "repoint the task at C:/x elease-hourly.vbs",
        "joined text"
    );
    assert_eq!(
        item.done_note.as_deref(),
        Some("moved"),
        "the trailing group on the split line still counts"
    );
    assert_eq!(parsed.unparsed.len(), 1, "the join is reported");
    assert_eq!(
        parsed.unparsed[0].line_no, 6,
        "reported at the line that was joined"
    );
}

type Configured<'a> = &'a [(&'a str, &'a str)];

#[test]
fn a_move_comes_from_the_last_configured_file_only_while_the_target_never_was_one() {
    use headwork_core::backlog::move_source;
    let old = ("S:/c/todo.md", "2026-11-26T10:00:00.000Z");
    let older = ("S:/c/older.md", "2026-11-20T10:00:00.000Z");
    let target = ("S:/c/backlog.md", "2026-11-26T11:00:00.000Z");
    let cases: [(Configured<'_>, Option<&str>); 5] = [
        (&[], None),
        (&[old], Some("S:/c/todo.md")),
        (&[older, old], Some("S:/c/todo.md")),
        (&[old, target], None),
        (&[target], None),
    ];
    for (configured, expected) in cases {
        assert_eq!(
            move_source("S:/c/backlog.md", configured),
            expected,
            "{configured:?}"
        );
    }
}
