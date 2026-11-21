//! Tick against the real wmux. Ignored by default: run with `cargo test -p headwork --test live -- --ignored`.
//!
//! The test works in a workspace named `headwork-test` and closes it afterwards. wmux shows one notification for the
//! pane no task owns. The lead test drives the real cmux instead, on macOS, in a `headwork-test-lead` workspace it
//! opens without focus and closes again.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;

use headwork::home::Home;
use headwork::ops::Env;
use headwork::ops::close::close_surface_or_workspace;
use headwork::ops::spawn::{SpawnRequest, pump, spawn};
use headwork::ops::tick::{Status, TickEnv, tick};
use headwork_adapters::disk::FakeDisks;
use headwork_adapters::git::FakeGit;
use headwork_adapters::host::cmux::{Cli as CmuxCli, Cmux, Runner as _};
use headwork_adapters::host::wmux::{Cli, Runner, Wmux};
use headwork_adapters::host::{SurfaceId, Target, TerminalHost, WorkspaceId};
use headwork_core::watch::Limits;
use headwork_core::{
    Actor, Config, FindingId, Model, PermissionMode, PlaceKind, ProjectName, Slug, TaskRequest,
    TaskShape,
};
use headwork_store::NewTask;

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
            task.shape,
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

/// The titles of the tabs of a cmux workspace, in tab order.
fn cmux_tab_titles(workspace: &WorkspaceId) -> Vec<String> {
    let tree: serde_json::Value = serde_json::from_str(
        &CmuxCli::locate(None)
            .run(&["tree", "--json", "--workspace", workspace.as_str()])
            .unwrap(),
    )
    .unwrap();
    tree["windows"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|window| window["workspaces"].as_array().cloned().unwrap_or_default())
        .filter(|ws| ws["id"].as_str() == Some(workspace.as_str()))
        .flat_map(|ws| ws["panes"].as_array().cloned().unwrap_or_default())
        .flat_map(|pane| pane["surfaces"].as_array().cloned().unwrap_or_default())
        .filter_map(|surface| surface["title"].as_str().map(str::to_owned))
        .collect()
}

/// A lead and one part against the real cmux: the lead opens full-screen in a workspace of its own titled with its
/// name, though `[layout] mode` is `panes`; the part opens as a second tab of it, titled with the part only; the part's
/// release closes only its tab, and the lead's closes the workspace.
#[test]
#[ignore = "drives the real cmux"]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario against the real host, set up and checked in order"
)]
fn a_lead_gets_a_workspace_and_its_part_a_tab_in_the_real_cmux() {
    let host = Cmux::new(CmuxCli::locate(None));
    host.ping().unwrap();
    assert!(
        host.workspaces()
            .unwrap()
            .iter()
            .all(|w| w.title != "headwork-test-lead"),
        "a workspace named headwork-test-lead is already open"
    );
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("demo");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "master"]);
    std::fs::write(main.join("launch"), "sleep 300\n").unwrap();
    git(&main, &["add", "launch"]);
    git(&main, &["commit", "-q", "-m", "init"]);
    let text = format!(
        "[machine]\nhost = \"cmux\"\n\n[leads]\nenabled = true\n\n[project.headwork-test]\nmain = \"{main}\"\n\
         worktree_new = \"builtin\"\nworktree_remove = \"builtin\"\nworktree_path = \"{{main}}-{{slug}}\"\n\
         deliver = \"pr\"\nmerge = \"deliver\"\n",
        main = main.display()
    );
    let config = Config::parse(&text, "live").unwrap();
    let home = Home::at(Some(dir.path().join("home")))
        .unwrap()
        .with_claude_json(dir.path().join(".claude.json"));
    let mut store = home.open_store().unwrap();
    store.sync_projects(&config).unwrap();
    let brief = dir.path().join("brief.md");
    std::fs::write(&brief, "Sleep.\n").unwrap();
    let env = Env {
        home: &home,
        config: &config,
        host: &host,
        exe: Path::new("/bin/sh"),
        actor: Actor::User,
    };
    let request = |slug: &str| TaskRequest {
        project: ProjectName::new("headwork-test").unwrap(),
        slug: Slug::new(slug).unwrap(),
        kind: "task".to_owned(),
        model: Some(Model::Haiku),
        effort: None,
        in_main: false,
        keep: false,
    };
    let (lead, _) = spawn(
        &mut store,
        &env,
        &SpawnRequest {
            request: request("lead"),
            brief: &brief,
            depends_on: None,
            permission_mode: PermissionMode::Auto,
            allow_ask: &[],
        },
    )
    .unwrap();
    let part = store
        .create_task(
            &config,
            &NewTask {
                request: request("lead-review"),
                brief_path: brief.display().to_string(),
                depends_on: None,
                permission_mode: PermissionMode::Auto,
                shape: TaskShape::Part { parent: lead.id },
            },
            Actor::Worker,
        )
        .unwrap();
    let pumped = pump(&mut store, &env).unwrap();
    let lead_row = store.session_of(lead.id).unwrap().unwrap();
    let part_row = store.session_of(part.id).unwrap().unwrap();
    let workspace = WorkspaceId(lead_row.host_workspace.clone().unwrap());
    let titled = host
        .workspaces()
        .unwrap()
        .into_iter()
        .find(|w| w.id == workspace)
        .map(|w| w.title);
    let cells = host.layout(&workspace).unwrap().cells();
    let tab_titles = cmux_tab_titles(&workspace);
    let release = |shape, row: &headwork_store::SessionRow| {
        close_surface_or_workspace(
            &env,
            shape,
            Some(&workspace),
            &SurfaceId(row.host_surface.clone().unwrap()),
        )
        .unwrap();
    };
    release(part.shape, &part_row);
    let open_after_part = host.workspaces().unwrap().iter().any(|w| w.id == workspace);
    let left = if open_after_part {
        host.surfaces(&workspace).unwrap()
    } else {
        Vec::new()
    };
    release(lead.shape, &lead_row);
    let start = std::time::Instant::now();
    let open = || host.workspaces().unwrap().iter().any(|w| w.id == workspace);
    while open() && start.elapsed() < std::time::Duration::from_secs(5) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let open_after_lead = open();
    if open_after_lead {
        drop(host.close_workspace(&workspace));
    }
    assert!(
        pumped
            .iter()
            .all(|p| matches!(p.outcome, headwork::ops::spawn::Outcome::Started { .. })),
        "the part started: {pumped:?}"
    );
    assert_eq!(
        titled.as_deref(),
        Some("headwork-test-lead"),
        "titled with the lead's name"
    );
    assert_eq!(
        part_row.host_workspace, lead_row.host_workspace,
        "the part opens in its lead's workspace"
    );
    assert_eq!(
        lead_row.host_place,
        Some(PlaceKind::Pane),
        "the lead full-screen"
    );
    assert_eq!(part_row.host_place, Some(PlaceKind::Tab), "the part a tab");
    assert_eq!(cells.len(), 1, "no split: {cells:?}");
    assert_eq!(
        cells.first().map(|cell| cell.surfaces.len()),
        Some(2),
        "the lead replaced the shell, the part is the second tab: {cells:?}"
    );
    assert_eq!(
        tab_titles,
        ["headwork-test-lead", "review"],
        "the lead's tab carries the task name, the part's only the part"
    );
    assert!(open_after_part, "the part's release leaves the workspace");
    assert_eq!(
        left,
        [SurfaceId(lead_row.host_surface.unwrap())],
        "only the part's tab closed"
    );
    assert!(!open_after_lead, "the lead's release closes the workspace");
}
