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
use headwork::ops::close::close_surface_or_workspace;
use headwork::ops::spawn::{SpawnRequest, spawn};
use headwork::ops::tick::{Status, TickEnv, tick};
use headwork_adapters::disk::FakeDisks;
use headwork_adapters::git::FakeGit;
use headwork_adapters::host::wmux::{Cli, Runner, Wmux};
use headwork_adapters::host::{SurfaceId, Target, TerminalHost};
use headwork_core::watch::Limits;
use headwork_core::{
    Actor, Config, FindingId, Model, PermissionMode, PlaceKind, ProjectName, Slug, TaskRequest,
};

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
    let home = Home::at(Some(dir.path().join("home")))
        .unwrap()
        .with_claude_json(dir.path().join(".claude.json"));
    let mut store = home.open_store().unwrap();
    store.sync_projects(&config).unwrap();
    let (workspace, _) = host
        .ensure_workspace("headwork-test-tick", dir.path())
        .unwrap();
    let argv = ["node", "-e", "setTimeout(() => {}, 60000)"].map(str::to_owned);
    let pane = host
        .spawn(
            &workspace,
            dir.path(),
            "demo/live-orphan",
            &argv,
            &Target::Tab,
        )
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

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "git {args:?}");
}

/// Spawns six tasks into a workspace named `headwork-test` with bash as the launcher: each pane runs bash on the
/// `launch` script the repository holds, which sleeps. Four fill the workspace as a grid of four equal quarters, the
/// fifth stacks as a tab, a closing worker's pane goes, and the sixth splits the half its neighbour grew into back into
/// quarters. Then every worker closes, one by one, and the workspace goes with the last.
#[test]
#[ignore = "drives the real wmux"]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario against the real host, set up and checked in order"
)]
fn workers_split_panes_and_take_them_away_in_the_real_host() {
    let host = Wmux::new(Cli::locate(None));
    host.ping().unwrap();
    assert!(
        host.workspaces()
            .unwrap()
            .iter()
            .all(|w| w.title != "headwork-test"),
        "a workspace named headwork-test is already open"
    );
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("demo");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "master"]);
    std::fs::write(main.join("launch"), "sleep 300\n").unwrap();
    git(&main, &["add", "launch"]);
    git(&main, &["commit", "-q", "-m", "init"]);
    let text = format!(
        "[machine]\nhost = \"wmux\"\n\n[project.headwork-test]\nmain = \"{}\"\nworktree_new = \"builtin\"\n\
         worktree_remove = \"builtin\"\nworktree_path = \"{{main}}-{{slug}}\"\ndeliver = \"pr\"\nmerge = \"deliver\"\n",
        main.display().to_string().replace('\\', "/")
    );
    let config = Config::parse(&text, "live").unwrap();
    let home = Home::at(Some(dir.path().join("home")))
        .unwrap()
        .with_claude_json(dir.path().join(".claude.json"));
    let mut store = home.open_store().unwrap();
    store.sync_projects(&config).unwrap();
    let brief = dir.path().join("brief.md");
    std::fs::write(&brief, "Sleep.\n").unwrap();
    std::fs::write(home.root().join("idle"), "sleep 300\n").unwrap();
    let bash = "C:/Progra~1/Git/usr/bin/bash.exe";
    let env = Env {
        home: &home,
        config: &config,
        host: &host,
        exe: Path::new(bash),
        actor: Actor::User,
    };
    let start = |store: &mut headwork_store::Store, slug: &str| {
        let request = SpawnRequest {
            request: TaskRequest {
                project: ProjectName::new("headwork-test").unwrap(),
                slug: Slug::new(slug).unwrap(),
                kind: "task".to_owned(),
                model: Some(Model::Haiku),
                effort: None,
                in_main: false,
                keep: false,
            },
            brief: &brief,
            depends_on: None,
            permission_mode: PermissionMode::Auto,
            allow_ask: &[],
        };
        spawn(store, &env, &request).unwrap().0
    };
    let tasks: Vec<_> = ["a", "b", "c", "d", "e"]
        .iter()
        .map(|slug| start(&mut store, slug))
        .collect();
    let workspace = host
        .workspaces()
        .unwrap()
        .into_iter()
        .find(|w| w.title == "headwork-test")
        .unwrap()
        .id;
    let shape = |host: &Wmux<Cli>| -> Vec<(usize, f64, f64)> {
        host.layout(&workspace)
            .unwrap()
            .cells()
            .into_iter()
            .map(|cell| (cell.surfaces.len(), cell.width, cell.height))
            .collect()
    };
    let split = shape(&host);
    let stacked = tasks
        .last()
        .map(|task| store.session_of(task.id).unwrap().unwrap().host_place);
    let close = |store: &mut headwork_store::Store, task: &headwork_store::Task| {
        let row = store.session_of(task.id).unwrap().unwrap();
        close_surface_or_workspace(
            &env,
            Some(&workspace),
            &SurfaceId(row.host_surface.clone().unwrap()),
        )
        .unwrap();
        store.session_ended(&row.id, Some(0)).unwrap();
        row
    };
    let gone = close(&mut store, tasks.first().unwrap());
    let kept = shape(&host);
    let refill = start(&mut store, "f");
    let refilled = shape(&host);
    let refill_row = store.session_of(refill.id).unwrap().unwrap();
    let tree = Cli::locate(None)
        .run(&["tree", "--workspace", workspace.as_str()])
        .unwrap();
    let mut left = Vec::new();
    for task in [1, 2, 3, 4]
        .iter()
        .filter_map(|i| tasks.get(*i))
        .chain([&refill])
    {
        close(&mut store, task);
        let open = host.workspaces().unwrap().iter().any(|w| w.id == workspace);
        left.push(if open { shape(&host).len() } else { 0 });
    }
    if host.workspaces().unwrap().iter().any(|w| w.id == workspace) {
        host.close_workspace(&workspace).unwrap();
    }
    assert!(
        tree.contains("headwork-test-c"),
        "the pane carries its name, model and effort: {tree}"
    );
    let quarter = |cells: &[(usize, f64, f64)]| {
        cells.len() == 4
            && cells
                .iter()
                .all(|(_, w, h)| (w - 0.5).abs() < 1e-3 && (h - 0.5).abs() < 1e-3)
    };
    assert!(quarter(&split), "a grid of four quarters: {split:?}");
    let mut tabs: Vec<usize> = split.iter().map(|(n, _, _)| *n).collect();
    tabs.sort_unstable();
    assert_eq!(tabs, [1, 1, 1, 2], "the fifth child a tab: {split:?}");
    assert_eq!(stacked, Some(Some(PlaceKind::Tab)), "the fifth is a tab");
    assert_eq!(kept.len(), 3, "the closed worker's pane is gone: {kept:?}");
    assert_eq!(
        refill_row.host_place,
        Some(PlaceKind::Pane),
        "the sixth gets a pane"
    );
    assert_ne!(refill_row.host_pane, gone.host_pane, "a new pane");
    assert!(quarter(&refilled), "the grid again: {refilled:?}");
    assert_eq!(
        left,
        [3, 2, 2, 1, 0],
        "the fourth's pane stays for the fifth, its tab; each other close takes a pane away"
    );
}

type Shape = Vec<(usize, f64, f64)>;

fn open_two(layout: &str) -> Vec<(String, Shape)> {
    let host = Wmux::new(Cli::locate(None));
    host.ping().unwrap();
    assert!(
        host.workspaces()
            .unwrap()
            .iter()
            .all(|w| w.title != "headwork-test"),
        "a workspace named headwork-test is already open"
    );
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("demo");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "master"]);
    std::fs::write(main.join("launch"), "sleep 300\n").unwrap();
    git(&main, &["add", "launch"]);
    git(&main, &["commit", "-q", "-m", "init"]);
    let text = format!(
        "[machine]\nhost = \"wmux\"\n\n[layout]\n{layout}\n\n[project.headwork-test]\nmain = \"{main}\"\n\
         worktree_new = \"builtin\"\nworktree_remove = \"builtin\"\nworktree_path = \"{{main}}-{{slug}}\"\n\
         deliver = \"pr\"\nmerge = \"deliver\"\n",
        main = main.display().to_string().replace('\\', "/"),
        layout = layout
    );
    let config = Config::parse(&text, "live").unwrap();
    let home = Home::at(Some(dir.path().join("home")))
        .unwrap()
        .with_claude_json(dir.path().join(".claude.json"));
    let mut store = home.open_store().unwrap();
    store.sync_projects(&config).unwrap();
    let brief = dir.path().join("brief.md");
    std::fs::write(&brief, "Sleep.\n").unwrap();
    std::fs::write(home.root().join("idle"), "sleep 300\n").unwrap();
    let env = Env {
        home: &home,
        config: &config,
        host: &host,
        exe: Path::new("C:/Progra~1/Git/usr/bin/bash.exe"),
        actor: Actor::User,
    };
    for slug in ["a", "b"] {
        let request = SpawnRequest {
            request: TaskRequest {
                project: ProjectName::new("headwork-test").unwrap(),
                slug: Slug::new(slug).unwrap(),
                kind: "task".to_owned(),
                model: Some(Model::Haiku),
                effort: None,
                in_main: false,
                keep: false,
            },
            brief: &brief,
            depends_on: None,
            permission_mode: PermissionMode::Auto,
            allow_ask: &[],
        };
        spawn(&mut store, &env, &request).unwrap();
    }
    let found: Vec<_> = host
        .workspaces()
        .unwrap()
        .into_iter()
        .filter(|w| w.title.starts_with("headwork-test"))
        .collect();
    let out = found
        .iter()
        .map(|w| {
            let shape = host
                .layout(&w.id)
                .unwrap()
                .cells()
                .into_iter()
                .map(|cell| (cell.surfaces.len(), cell.width, cell.height))
                .collect();
            (w.title.clone(), shape)
        })
        .collect();
    for w in &found {
        host.close_workspace(&w.id).unwrap();
    }
    out
}

#[test]
#[ignore = "drives the real wmux"]
fn two_panes_sit_side_by_side_in_the_real_host() {
    let found = open_two(
        "mode = \"panes\"
panes = 2",
    );
    assert_eq!(found.len(), 1, "one workspace: {found:?}");
    let shape = &found[0].1;
    assert_eq!(shape.len(), 2, "two panes: {shape:?}");
    assert!(
        shape
            .iter()
            .all(|(_, w, h)| (w - 0.5).abs() < 1e-3 && (h - 1.0).abs() < 1e-3),
        "left and right halves: {shape:?}"
    );
}

#[test]
#[ignore = "drives the real wmux"]
fn each_worker_gets_a_workspace_of_its_own_in_the_real_host() {
    let found = open_two("mode = \"workspaces\"");
    let mut titles: Vec<&str> = found.iter().map(|(t, _)| t.as_str()).collect();
    titles.sort_unstable();
    assert_eq!(
        titles,
        ["headwork-test-a", "headwork-test-b"],
        "a workspace per task: {found:?}"
    );
    assert!(
        found
            .iter()
            .all(|(_, shape)| shape.len() == 1 && shape[0].0 == 1),
        "one pane with one surface each: {found:?}"
    );
}
