//! Tick against the real wmux. Ignored by default: run with `cargo test -p headwork --test live -- --ignored`.
//!
//! The test works in a workspace named `headwork-test` and closes it afterwards. wmux shows one notification for the
//! pane no task owns.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;

use headwork::home::Home;
use headwork::ops::Env;
use headwork::ops::tick::{Status, TickEnv, tick};
use headwork_adapters::disk::FakeDisks;
use headwork_adapters::git::FakeGit;
use headwork_adapters::host::TerminalHost;
use headwork_adapters::host::wmux::{Cli, Wmux};
use headwork_core::watch::Limits;
use headwork_core::{Actor, Config, FindingId};

const GIB: u64 = 1024 * 1024 * 1024;

#[test]
#[ignore = "drives the real wmux"]
fn the_tick_sees_a_pane_no_task_owns_in_the_real_host() {
    let host = Wmux::new(Cli::locate(None));
    host.ping().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("demo");
    std::fs::create_dir(&main).unwrap();
    let text = format!(
        "[machine]\nhost = \"wmux\"\n\n[project.demo]\nmain = \"{}\"\nworktree_new = \"builtin\"\n\
         worktree_remove = \"builtin\"\nworktree_path = \"{{main}}-{{slug}}\"\ndeliver = \"pr\"\nmerge = \"deliver\"\n",
        main.display().to_string().replace('\\', "/")
    );
    let config = Config::parse(&text, "live").unwrap();
    let home = Home::at(Some(dir.path().join("home"))).unwrap();
    let mut store = home.open_store().unwrap();
    store.sync_projects(&config).unwrap();
    let (workspace, _) = host.ensure_workspace("headwork-test", dir.path()).unwrap();
    let argv = ["node", "-e", "setTimeout(() => {}, 60000)"].map(str::to_owned);
    let pane = host
        .spawn(&workspace, dir.path(), "demo/live-orphan", &argv, false)
        .unwrap();
    let disks = FakeDisks::new();
    disks.set(dir.path(), 100 * GIB);
    disks.set(&std::env::home_dir().unwrap(), 100 * GIB);
    let git = FakeGit::new();
    let limits = Limits::from_config(&config.watch);
    let env = Env {
        home: &home,
        config: &config,
        host: &host,
        exe: Path::new("headwork"),
        actor: Actor::Tick,
    };
    let report = tick(
        &mut store,
        &TickEnv {
            env: &env,
            git: &git,
            disks: &disks,
            temp: dir.path(),
            limits,
        },
    );
    host.close_workspace(&workspace).unwrap();
    let report = report.unwrap();
    assert!(report.host_up, "wmux answers");
    assert!(
        report.lines.iter().any(|l| l.rule == FindingId::OrphanPane
            && l.status == Status::Standing
            && l.detail.contains(pane.surface.as_str())),
        "{:?}",
        report.lines
    );
}
