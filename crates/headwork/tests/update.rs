//! `headwork update` against a temporary home, fake releases, fake builds and a fake clock: no network.
//!
//! A fake "program" is a text file whose first line is what its `--version` prints; the fake builds read it, and a
//! file holding `smoke-fail` fails its doctor.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::Result;
use headwork::home::Home;
use headwork::ops::install::program_name;
use headwork::ops::update::{
    Builds, Clock, Forward, PENDING_FILE, STATE_FILE, Started, ThisBuild, UpdateEnv, UpdateLock,
    UpdateOptions, UpdateReport, UpdateState, after, doctor_summary, expected_hash,
    forward_checkout, update,
};
use headwork_adapters::disk::FakeDisks;
use headwork_adapters::release::{Asset, FakeReleases, ReleaseError, ReleaseInfo, SUMS_NAME};
use headwork_core::update::{FailureKind, Version, parse_utc};
use headwork_core::{Actor, Config};
use headwork_store::{EventFilter, MIGRATIONS, Store};
use sha2::{Digest, Sha256};

const ASSET: &str = "headwork-test-asset";
const GIB: u64 = 1024 * 1024 * 1024;

/// A release and the files its assets point at, as `(url, bytes)`.
type Published = (ReleaseInfo, Vec<(String, Vec<u8>)>);

const fn schema() -> usize {
    MIGRATIONS.len()
}

fn line(version: &str, commit: &str) -> String {
    format!("headwork {version} ({commit}, schema {})", schema())
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::new();
    for byte in &Sha256::digest(bytes) {
        let _infallible: std::fmt::Result =
            std::fmt::Write::write_fmt(&mut text, format_args!("{byte:02x}"));
    }
    text
}

/// A release whose program prints `program` and whose `SHA256SUMS` says `sums_for` hashes to it (the program itself
/// when `None`).
fn release_files(
    tag: &str,
    published_at: &str,
    program: &str,
    sums_for: Option<&str>,
) -> Published {
    let asset_url = format!("https://example.test/{tag}/{ASSET}");
    let sums_url = format!("https://example.test/{tag}/{SUMS_NAME}");
    let hashed = sums_for.unwrap_or(program);
    let sums = format!(
        "{}  {ASSET}\n{}  other-file\n",
        hex(hashed.as_bytes()),
        hex(b"x")
    );
    let release = ReleaseInfo {
        tag: tag.to_owned(),
        published_at: published_at.to_owned(),
        assets: vec![
            Asset {
                name: ASSET.to_owned(),
                url: asset_url.clone(),
            },
            Asset {
                name: SUMS_NAME.to_owned(),
                url: sums_url.clone(),
            },
        ],
    };
    let files = vec![
        (asset_url, program.as_bytes().to_vec()),
        (sums_url, sums.into_bytes()),
    ];
    (release, files)
}

fn publish(releases: &FakeReleases, tag: &str, published_at: &str, program: &str) {
    let (release, files) = release_files(tag, published_at, program, None);
    releases.publish(release, files);
}

#[derive(Debug, Default)]
struct FakeBuilds {
    started: RefCell<Vec<(PathBuf, Vec<String>)>>,
    doctors: RefCell<Vec<(PathBuf, bool, bool)>>,
}

impl Builds for FakeBuilds {
    fn version(&self, program: &Path) -> Result<String> {
        let text = std::fs::read_to_string(program)?;
        Ok(text.lines().next().unwrap_or_default().to_owned())
    }

    fn doctor(&self, program: &Path, home: &Path) -> Result<Vec<(String, String)>> {
        self.doctors.borrow_mut().push((
            program.to_owned(),
            home.join("config.toml").is_file(),
            home.join("state.db").is_file(),
        ));
        let text = std::fs::read_to_string(program)?;
        let store = if text.contains("smoke-fail") {
            "fail"
        } else {
            "ok"
        };
        Ok(vec![
            ("gh".to_owned(), "fail".to_owned()),
            ("store".to_owned(), store.to_owned()),
        ])
    }

    fn start(&self, program: &Path, args: &[String]) -> Result<Started> {
        self.started
            .borrow_mut()
            .push((program.to_owned(), args.to_vec()));
        Ok(Started {
            pid: 4242,
            note: None,
        })
    }
}

/// A clock that moves only when slept on, and publishes one release at its first sleep.
#[derive(Debug)]
struct FakeClock<'a> {
    now: Cell<u64>,
    releases: &'a FakeReleases,
    on_sleep: RefCell<Option<Published>>,
    sleeps: Cell<u32>,
}

impl<'a> FakeClock<'a> {
    fn new(releases: &'a FakeReleases) -> Self {
        Self {
            now: Cell::new(parse_utc("2026-11-22T12:00:00Z").unwrap()),
            releases,
            on_sleep: RefCell::new(None),
            sleeps: Cell::new(0),
        }
    }

    fn advance(&self, minutes: u64) {
        self.now.set(self.now.get() + minutes * 60_000);
    }
}

impl Clock for FakeClock<'_> {
    fn now_ms(&self) -> u64 {
        self.now.get()
    }

    fn sleep(&self, duration: Duration) {
        self.sleeps.set(self.sleeps.get() + 1);
        self.now
            .set(self.now.get() + u64::try_from(duration.as_millis()).unwrap());
        if let Some((release, files)) = self.on_sleep.borrow_mut().take() {
            self.releases.publish(release, files);
        }
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    home: Home,
    bin: PathBuf,
    main: PathBuf,
    config: Config,
    releases: FakeReleases,
    builds: FakeBuilds,
    disks: FakeDisks,
}

fn config_text(enabled: bool, main: &Path) -> String {
    format!(
        "[machine]\nhost = \"wmux\"\n\n[update]\nenabled = {enabled}\n\n[project.headwork]\nmain = \"{}\"\n\
         worktree_new = \"builtin\"\nworktree_remove = \"builtin\"\nworktree_path = \"{{main}}-{{slug}}\"\n\
         deliver = \"pr\"\nmerge = \"human\"\nallowed_repos = [\"Ivan091/headwork\"]\n",
        main.display().to_string().replace('\\', "/")
    )
}

impl Rig {
    fn new(installed: &str) -> Self {
        Self::with(installed, true)
    }

    fn with(installed: &str, enabled: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("home");
        let bin = dir.path().join("bin");
        let main = dir.path().join("main");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&main).unwrap();
        let text = config_text(enabled, &main);
        std::fs::write(root.join("config.toml"), &text).unwrap();
        std::fs::write(bin.join(program_name()), format!("{installed}\n")).unwrap();
        let home = Home::at(Some(root))
            .unwrap()
            .with_claude_json(dir.path().join(".claude.json"));
        let config = Config::parse(&text, "test").unwrap();
        let disks = FakeDisks::new();
        disks.set(home.root(), 100 * GIB);
        Self {
            _dir: dir,
            home,
            bin,
            main,
            config,
            releases: FakeReleases::new(),
            builds: FakeBuilds::default(),
            disks,
        }
    }

    fn env<'a>(&'a self, clock: &'a FakeClock<'a>) -> UpdateEnv<'a> {
        UpdateEnv {
            home: &self.home,
            config: &self.config,
            releases: &self.releases,
            builds: &self.builds,
            disks: &self.disks,
            clock,
            install_dir: &self.bin,
            asset: ASSET,
            actor: Actor::User,
        }
    }

    fn run(&self, clock: &FakeClock<'_>, opts: UpdateOptions) -> UpdateReport {
        update(&self.env(clock), opts).unwrap()
    }

    fn installed(&self) -> String {
        std::fs::read_to_string(self.bin.join(program_name()))
            .unwrap()
            .trim()
            .to_owned()
    }

    fn events(&self, kind: &str) -> Vec<serde_json::Value> {
        let store = Store::open(&self.home.root().join("state.db")).unwrap();
        store
            .events(EventFilter {
                task: None,
                limit: u32::MAX,
            })
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == kind)
            .map(|event| event.data)
            .collect()
    }

    fn state(&self) -> UpdateState {
        UpdateState::load(&self.home)
    }
}

fn plain() -> UpdateOptions {
    UpdateOptions::default()
}

#[test]
fn the_newest_release_installed_is_up_to_date_and_asks_with_its_etag() {
    let rig = Rig::new(&line("0.1.6", "aaa111"));
    let clock = FakeClock::new(&rig.releases);
    publish(
        &rig.releases,
        "v0.1.6",
        "2026-11-22T11:00:00Z",
        &line("0.1.6", "aaa111"),
    );
    let report = rig.run(&clock, plain());
    assert!(
        matches!(&report, UpdateReport::UpToDate { newest: Some(newest), .. } if newest == "v0.1.6"),
        "{report:?}"
    );
    let state = rig.state();
    assert_eq!(
        state.etag.as_deref(),
        Some("\"v0.1.6\""),
        "the etag is kept"
    );
    assert!(state.last_poll.is_some(), "the poll is recorded");
    let again = rig.run(&clock, plain());
    assert!(
        matches!(&again, UpdateReport::UpToDate { newest: Some(newest), .. } if newest == "v0.1.6"),
        "a 304 uses the release kept: {again:?}"
    );
    assert!(rig.releases.downloads().is_empty(), "nothing downloaded");
    assert!(rig.builds.started.borrow().is_empty(), "nothing started");
}

#[test]
fn a_newer_release_is_checked_smoke_tested_installed_and_handed_over() {
    let rig = Rig::new(&line("0.1.5", "aaa111"));
    let clock = FakeClock::new(&rig.releases);
    let new = line("0.1.6", "bbb222");
    publish(&rig.releases, "v0.1.6", "2026-11-22T11:00:00Z", &new);
    let report = rig.run(&clock, plain());
    let UpdateReport::Installed {
        tag,
        from,
        to,
        handed_over,
        ..
    } = &report
    else {
        unreachable!("installed: {report:?}")
    };
    assert_eq!(tag, "v0.1.6", "tag");
    assert_eq!(
        from,
        &format!("0.1.5 (aaa111, schema {})", schema()),
        "from"
    );
    assert_eq!(to, &format!("0.1.6 (bbb222, schema {})", schema()), "to");
    assert_eq!(
        handed_over.as_ref().map(|started| started.pid),
        Some(4242),
        "handed over"
    );
    assert_eq!(rig.installed(), new, "the release is the installed build");
    let started = rig.builds.started.borrow();
    assert_eq!(started.len(), 1, "one hand-over");
    assert_eq!(
        started[0].0,
        rig.bin.join(program_name()),
        "the installed build takes over"
    );
    assert_eq!(
        started[0].1,
        vec![
            "update".to_owned(),
            "--after".to_owned(),
            from.clone(),
            "--dir".to_owned(),
            rig.bin.display().to_string(),
        ],
        "with the old version"
    );
    let doctors = rig.builds.doctors.borrow();
    assert_eq!(doctors.len(), 2, "both builds ran doctor");
    assert!(
        doctors.iter().all(|(_, config, store)| *config && *store),
        "on a scratch home with the config and a copy of the store: {doctors:?}"
    );
    assert_eq!(
        doctors[0].0,
        rig.bin.join(program_name()),
        "the installed build first"
    );
    let release_dir = rig.home.root().join("updates/v0.1.6");
    assert!(release_dir.join(ASSET).is_file(), "the download stays");
    assert!(
        !release_dir.join("smoke").exists(),
        "the scratch home is removed"
    );
    assert_eq!(rig.events("update-started").len(), 1, "started is logged");
    assert!(rig.state().last_failure.is_none(), "no failure");
}

#[test]
fn a_hash_mismatch_latches_the_release_until_a_newer_one() {
    let rig = Rig::new(&line("0.1.5", "aaa111"));
    let clock = FakeClock::new(&rig.releases);
    let (release, files) = release_files(
        "v0.1.6",
        "2026-11-22T11:00:00Z",
        &line("0.1.6", "bbb222"),
        Some("something else"),
    );
    rig.releases.publish(release, files);
    let report = rig.run(&clock, plain());
    assert!(
        matches!(&report, UpdateReport::Failed { step, kind: FailureKind::Deterministic, .. } if step == "hash"),
        "{report:?}"
    );
    assert!(report.failed(), "exits non-zero");
    assert_eq!(
        rig.installed(),
        line("0.1.5", "aaa111"),
        "nothing installed"
    );
    assert_eq!(
        rig.events("update-failed").len(),
        1,
        "the failure is logged"
    );
    let (warn, detail) = doctor_summary(&rig.home, &rig.config);
    assert!(warn, "doctor warns: {detail}");
    assert!(detail.contains("hash"), "{detail}");
    let downloads = rig.releases.downloads().len();
    clock.advance(600);
    let again = rig.run(&clock, plain());
    assert!(
        matches!(&again, UpdateReport::Skipped { reason, .. } if reason == "failed-here"),
        "latched: {again:?}"
    );
    assert_eq!(
        rig.releases.downloads().len(),
        downloads,
        "not downloaded again"
    );
    assert_eq!(rig.events("update-skipped").len(), 1, "the skip is logged");
    publish(
        &rig.releases,
        "v0.1.7",
        "2026-11-22T13:00:00Z",
        &line("0.1.7", "ccc333"),
    );
    let newer = rig.run(&clock, plain());
    assert!(matches!(newer, UpdateReport::Installed { .. }), "{newer:?}");
    assert!(
        !doctor_summary(&rig.home, &rig.config).0,
        "the warning clears"
    );
}

#[test]
fn a_transient_failure_backs_off_before_asking_again() {
    let rig = Rig::new(&line("0.1.5", "aaa111"));
    let clock = FakeClock::new(&rig.releases);
    publish(
        &rig.releases,
        "v0.1.6",
        "2026-11-22T11:00:00Z",
        &line("0.1.6", "bbb222"),
    );
    rig.releases.fail_next(ReleaseError::Transfer {
        what: "the newest release".to_owned(),
        detail: "timed out".to_owned(),
    });
    let report = rig.run(&clock, plain());
    assert!(
        matches!(&report, UpdateReport::Failed { kind: FailureKind::Transient, attempts: 1, step, .. } if step == "ask"),
        "{report:?}"
    );
    assert!(
        rig.events("update-failed").is_empty(),
        "a first transient failure is quiet"
    );
    assert!(
        !doctor_summary(&rig.home, &rig.config).0,
        "and no doctor warning"
    );
    let requests = rig.releases.requests();
    clock.advance(2);
    let waiting = rig.run(&clock, plain());
    assert!(
        matches!(waiting, UpdateReport::Waiting { .. }),
        "{waiting:?}"
    );
    assert_eq!(
        rig.releases.requests(),
        requests,
        "GitHub is not asked while backing off"
    );
    clock.advance(3);
    let retried = rig.run(&clock, plain());
    assert!(
        matches!(retried, UpdateReport::Installed { .. }),
        "{retried:?}"
    );
}

#[test]
fn transient_failures_are_reported_from_the_third() {
    let rig = Rig::new(&line("0.1.5", "aaa111"));
    let clock = FakeClock::new(&rig.releases);
    for attempt in 1..=3 {
        rig.releases.fail_next(ReleaseError::RateLimited {
            what: "the newest release".to_owned(),
            status: 403,
        });
        let report = rig.run(&clock, plain());
        assert!(
            matches!(report, UpdateReport::Failed { attempts, .. } if attempts == attempt),
            "{report:?}"
        );
        clock.advance(61);
    }
    assert_eq!(
        rig.events("update-failed").len(),
        1,
        "only the third is logged"
    );
    assert!(
        doctor_summary(&rig.home, &rig.config).0,
        "doctor warns from the third"
    );
}

#[test]
fn a_held_lock_leaves_a_pending_mark() {
    let rig = Rig::new(&line("0.1.5", "aaa111"));
    let clock = FakeClock::new(&rig.releases);
    publish(
        &rig.releases,
        "v0.1.6",
        "2026-11-22T11:00:00Z",
        &line("0.1.6", "bbb222"),
    );
    let held = UpdateLock::try_take(&rig.home).unwrap().unwrap();
    let report = rig.run(&clock, plain());
    assert_eq!(report, UpdateReport::Pending, "another update runs");
    assert!(
        rig.home.root().join(PENDING_FILE).is_file(),
        "it is asked to run again"
    );
    assert_eq!(rig.releases.requests(), 0, "GitHub is not asked");
    drop(held);
    let report = rig.run(&clock, plain());
    assert!(
        matches!(report, UpdateReport::Installed { .. }),
        "{report:?}"
    );
    assert!(
        !rig.home.root().join(PENDING_FILE).exists(),
        "the mark is taken up"
    );
}

#[test]
fn a_dev_build_gives_way_only_to_a_release_published_after_it() {
    let rig = Rig::new(&line("0.1.0", "dev"));
    let clock = FakeClock::new(&rig.releases);
    let installed_at = parse_utc("2026-11-22T10:00:00Z").unwrap();
    std::fs::File::options()
        .write(true)
        .open(rig.bin.join(program_name()))
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + Duration::from_millis(installed_at))
        .unwrap();
    publish(
        &rig.releases,
        "v0.1.6",
        "2026-11-22T09:00:00Z",
        &line("0.1.6", "bbb222"),
    );
    let report = rig.run(&clock, plain());
    assert!(
        matches!(&report, UpdateReport::Skipped { reason, .. } if reason == "dev-build"),
        "{report:?}"
    );
    assert_eq!(
        rig.installed(),
        line("0.1.0", "dev"),
        "the test install stays"
    );
    publish(
        &rig.releases,
        "v0.1.7",
        "2026-11-22T11:00:00Z",
        &line("0.1.7", "ccc333"),
    );
    let report = rig.run(&clock, plain());
    assert!(
        matches!(&report, UpdateReport::Installed { tag, .. } if tag == "v0.1.7"),
        "{report:?}"
    );
}

#[test]
fn to_refuses_a_release_that_knows_an_older_schema_or_does_not_exist() {
    let rig = Rig::new(&line("0.1.5", "aaa111"));
    let clock = FakeClock::new(&rig.releases);
    let old = format!("headwork 0.1.4 (abc000, schema {})", schema() - 1);
    publish(&rig.releases, "v0.1.4", "2026-11-20T11:00:00Z", &old);
    let to = |tag: &str| UpdateOptions {
        to: Version::from_tag(tag),
        ..UpdateOptions::default()
    };
    let report = rig.run(&clock, to("v0.1.4"));
    assert!(
        matches!(&report, UpdateReport::Refused { tag, reason } if tag == "v0.1.4" && reason.contains("older")),
        "{report:?}"
    );
    assert!(report.failed(), "exits non-zero");
    assert_eq!(
        rig.installed(),
        line("0.1.5", "aaa111"),
        "nothing installed"
    );
    let missing = rig.run(&clock, to("v0.1.99"));
    assert!(
        matches!(&missing, UpdateReport::Refused { reason, .. } if reason.contains("no release")),
        "{missing:?}"
    );
}

#[test]
fn to_installs_an_older_release_as_a_rollback() {
    let rig = Rig::with(&line("0.1.5", "aaa111"), false);
    let clock = FakeClock::new(&rig.releases);
    publish(
        &rig.releases,
        "v0.1.4",
        "2026-11-20T11:00:00Z",
        &line("0.1.4", "abc000"),
    );
    publish(
        &rig.releases,
        "v0.1.5",
        "2026-11-21T11:00:00Z",
        &line("0.1.5", "aaa111"),
    );
    let report = rig.run(
        &clock,
        UpdateOptions {
            to: Version::from_tag("v0.1.4"),
            ..UpdateOptions::default()
        },
    );
    assert!(
        matches!(&report, UpdateReport::Installed { tag, .. } if tag == "v0.1.4"),
        "even with updates off: {report:?}"
    );
    assert_eq!(rig.installed(), line("0.1.4", "abc000"), "rolled back");
}

#[test]
fn a_failing_smoke_test_installs_nothing() {
    let rig = Rig::new(&line("0.1.5", "aaa111"));
    let clock = FakeClock::new(&rig.releases);
    publish(
        &rig.releases,
        "v0.1.6",
        "2026-11-22T11:00:00Z",
        &format!("{}\nsmoke-fail", line("0.1.6", "bbb222")),
    );
    let report = rig.run(&clock, plain());
    let UpdateReport::Failed { step, error, .. } = &report else {
        unreachable!("failed: {report:?}")
    };
    assert_eq!(step, "smoke", "step");
    assert!(
        error.contains("store"),
        "names the new failure, not gh: {error}"
    );
    assert!(
        !error.contains("gh"),
        "a check both builds fail is not the release's: {error}"
    );
    assert_eq!(
        rig.installed(),
        line("0.1.5", "aaa111"),
        "nothing installed"
    );
}

#[test]
fn a_dry_run_changes_nothing() {
    let rig = Rig::new(&line("0.1.5", "aaa111"));
    let clock = FakeClock::new(&rig.releases);
    publish(
        &rig.releases,
        "v0.1.6",
        "2026-11-22T11:00:00Z",
        &line("0.1.6", "bbb222"),
    );
    let report = rig.run(
        &clock,
        UpdateOptions {
            dry_run: true,
            ..UpdateOptions::default()
        },
    );
    assert!(
        matches!(&report, UpdateReport::WouldInstall { tag, .. } if tag == "v0.1.6"),
        "{report:?}"
    );
    assert_eq!(
        rig.installed(),
        line("0.1.5", "aaa111"),
        "nothing installed"
    );
    for file in [STATE_FILE, "update.lock", "updates"] {
        assert!(
            !rig.home.root().join(file).exists(),
            "{file} is not written"
        );
    }
    assert!(rig.releases.downloads().is_empty(), "nothing downloaded");
    assert!(rig.events("update-started").is_empty(), "nothing logged");
}

#[test]
fn updates_that_are_off_ask_nobody() {
    let rig = Rig::with(&line("0.1.5", "aaa111"), false);
    let clock = FakeClock::new(&rig.releases);
    publish(
        &rig.releases,
        "v0.1.6",
        "2026-11-22T11:00:00Z",
        &line("0.1.6", "bbb222"),
    );
    let report = rig.run(&clock, plain());
    assert!(
        matches!(&report, UpdateReport::Skipped { reason, .. } if reason == "off"),
        "{report:?}"
    );
    assert_eq!(rig.releases.requests(), 0, "GitHub is not asked");
    assert!(
        rig.events("update-skipped").is_empty(),
        "off is not worth an event"
    );
}

#[test]
fn a_full_disk_skips() {
    let rig = Rig::new(&line("0.1.5", "aaa111"));
    rig.disks.set(rig.home.root(), GIB);
    let clock = FakeClock::new(&rig.releases);
    let report = rig.run(&clock, plain());
    assert!(
        matches!(&report, UpdateReport::Skipped { reason, .. } if reason == "disk-stop"),
        "{report:?}"
    );
}

#[test]
fn wait_polls_once_a_minute_until_a_newer_release_appears() {
    let rig = Rig::new(&line("0.1.6", "aaa111"));
    let clock = FakeClock::new(&rig.releases);
    publish(
        &rig.releases,
        "v0.1.6",
        "2026-11-22T11:00:00Z",
        &line("0.1.6", "aaa111"),
    );
    *clock.on_sleep.borrow_mut() = Some(release_files(
        "v0.1.7",
        "2026-11-22T12:00:30Z",
        &line("0.1.7", "ccc333"),
        None,
    ));
    let report = rig.run(
        &clock,
        UpdateOptions {
            wait_min: Some(30),
            ..UpdateOptions::default()
        },
    );
    assert!(
        matches!(&report, UpdateReport::Installed { tag, .. } if tag == "v0.1.7"),
        "{report:?}"
    );
    assert_eq!(clock.sleeps.get(), 1, "one minute's wait");
    let rig = Rig::new(&line("0.1.6", "aaa111"));
    let clock = FakeClock::new(&rig.releases);
    publish(
        &rig.releases,
        "v0.1.6",
        "2026-11-22T11:00:00Z",
        &line("0.1.6", "aaa111"),
    );
    let report = rig.run(
        &clock,
        UpdateOptions {
            wait_min: Some(3),
            ..UpdateOptions::default()
        },
    );
    assert!(
        matches!(report, UpdateReport::UpToDate { .. }),
        "{report:?}"
    );
    assert_eq!(clock.sleeps.get(), 3, "gives up after its minutes");
}

#[test]
fn after_records_the_install_and_runs_again_when_asked_meanwhile() {
    let rig = Rig::new(&line("0.1.6", "bbb222"));
    let clock = FakeClock::new(&rig.releases);
    std::fs::write(rig.home.root().join(PENDING_FILE), "").unwrap();
    let text = format!("0.1.6 (bbb222, schema {})", schema());
    let report = after(
        &rig.env(&clock),
        "0.1.5 (aaa111, schema 14)",
        ThisBuild {
            text: &text,
            release: Version::from_tag("v0.1.6"),
            commit: Some("bbb222"),
        },
    )
    .unwrap();
    assert!(
        matches!(&report.checkout, Forward::NotForwarded { .. }),
        "the main folder is no git checkout: {report:?}"
    );
    assert_eq!(
        report.again.as_ref().map(|again| again.pid),
        Some(4242),
        "runs again"
    );
    assert!(
        !rig.home.root().join(PENDING_FILE).exists(),
        "the mark is taken up"
    );
    let installed = rig.events("update-installed");
    assert_eq!(installed.len(), 1, "installed is logged");
    assert_eq!(installed[0]["tag"], "v0.1.6", "tag");
    assert_eq!(installed[0]["from"], "0.1.5 (aaa111, schema 14)", "from");
    assert_eq!(
        rig.events("checkout-not-forwarded").len(),
        1,
        "the checkout is logged"
    );
    let started = rig.builds.started.borrow();
    assert_eq!(started[0].1[0], "update", "a plain update");
}

#[test]
fn after_starts_no_second_update_when_none_is_needed() {
    let rig = Rig::new(&line("0.1.6", "bbb222"));
    let clock = FakeClock::new(&rig.releases);
    publish(
        &rig.releases,
        "v0.1.6",
        "2026-11-22T11:00:00Z",
        &line("0.1.6", "bbb222"),
    );
    let report = after(
        &rig.env(&clock),
        "0.1.5 (aaa111, schema 14)",
        ThisBuild {
            text: "0.1.6 (bbb222, schema 14)",
            release: Version::from_tag("v0.1.6"),
            commit: None,
        },
    )
    .unwrap();
    assert_eq!(report.again, None, "nothing newer, nothing asked");
    publish(
        &rig.releases,
        "v0.1.7",
        "2026-11-22T13:00:00Z",
        &line("0.1.7", "ccc333"),
    );
    let report = after(
        &rig.env(&clock),
        "0.1.5 (aaa111, schema 14)",
        ThisBuild {
            text: "0.1.6 (bbb222, schema 14)",
            release: Version::from_tag("v0.1.6"),
            commit: None,
        },
    )
    .unwrap();
    assert!(report.again.is_some(), "a newer release appeared meanwhile");
    assert!(rig.main.is_dir(), "the main folder is untouched");
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.test",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn commit_file(dir: &Path, name: &str, text: &str) -> String {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "--quiet", "-m", name]);
    git(dir, &["rev-parse", "HEAD"])
}

#[test]
fn the_main_checkout_is_fast_forwarded_to_the_release_unless_it_has_work() {
    let rig = Rig::new(&line("0.1.6", "bbb222"));
    let origin = rig.main.parent().unwrap().join("origin");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "--quiet", "-b", "master"]);
    commit_file(&origin, "README.md", "one");
    std::fs::remove_dir_all(&rig.main).unwrap();
    git(
        rig.main.parent().unwrap(),
        &["clone", "--quiet", origin.to_str().unwrap(), "main"],
    );
    let released = commit_file(&origin, "plugin/hooks.json", "{}");
    let forward = forward_checkout(&rig.home, &rig.config, Some(&released));
    let Forward::Forwarded {
        to, plugin_changed, ..
    } = &forward
    else {
        unreachable!("forwarded: {forward:?}")
    };
    assert_eq!(to, &released, "to the release");
    assert!(plugin_changed, "plugin files changed");
    assert_eq!(
        git(&rig.main, &["rev-parse", "HEAD"]),
        released,
        "HEAD moved"
    );
    assert_eq!(
        forward_checkout(&rig.home, &rig.config, Some(&released)),
        Forward::AlreadyThere,
        "a second run finds it there"
    );
    let next = commit_file(&origin, "src.txt", "two");
    std::fs::write(rig.main.join("README.md"), "local edit").unwrap();
    let dirty = forward_checkout(&rig.home, &rig.config, Some(&next));
    assert!(
        matches!(&dirty, Forward::NotForwarded { reason } if reason.contains("uncommitted")),
        "{dirty:?}"
    );
    git(&rig.main, &["checkout", "--quiet", "--", "README.md"]);
    git(&rig.main, &["checkout", "--quiet", "-b", "feature"]);
    let branch = forward_checkout(&rig.home, &rig.config, Some(&next));
    assert!(
        matches!(&branch, Forward::NotForwarded { reason } if reason.contains("feature")),
        "{branch:?}"
    );
    git(&rig.main, &["checkout", "--quiet", "master"]);
    commit_file(&rig.main, "local.txt", "mine");
    let ahead = forward_checkout(&rig.home, &rig.config, Some(&next));
    assert!(
        matches!(&ahead, Forward::NotForwarded { reason } if reason.contains("local commits")),
        "{ahead:?}"
    );
    assert!(
        matches!(
            forward_checkout(&rig.home, &rig.config, Some("--upload-pack=x")),
            Forward::NotForwarded { .. }
        ),
        "an option is never passed as a commit"
    );
}

#[test]
fn sums_files_are_read_as_sha256sum_writes_them() {
    let hash = "a".repeat(64);
    let upper = "B".repeat(64);
    let sums = format!("{hash}  one\n{upper} *two\nshort  three\n");
    assert_eq!(expected_hash(&sums, "one"), Some(hash), "text mode");
    assert_eq!(
        expected_hash(&sums, "two"),
        Some("b".repeat(64)),
        "binary mode, lower case"
    );
    assert_eq!(expected_hash(&sums, "three"), None, "not a hash");
    assert_eq!(expected_hash(&sums, "four"), None, "not listed");
}

#[test]
fn the_smoke_test_reads_the_report_doctor_prints() {
    use headwork::ops::doctor::{Check, DoctorReport, Status};
    use headwork::ops::update::doctor_checks;
    let report = DoctorReport {
        checks: vec![
            Check {
                id: "store",
                status: Status::Fail,
                detail: "schema 99".to_owned(),
                fix: None,
            },
            Check {
                id: "gh",
                status: Status::Ok,
                detail: "logged in".to_owned(),
                fix: None,
            },
        ],
    };
    let text = serde_json::to_string_pretty(&report).unwrap();
    assert_eq!(
        doctor_checks(&text).unwrap(),
        vec![
            ("store".to_owned(), "fail".to_owned()),
            ("gh".to_owned(), "ok".to_owned())
        ],
        "ids and statuses"
    );
    assert!(doctor_checks("headwork: no config").is_err(), "no report");
}
