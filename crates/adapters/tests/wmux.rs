//! The wmux adapter: parsers against saved wmux output, and the command lines it sends, through a recording runner.

#![expect(clippy::unwrap_used, reason = "tests fail loudly on a bad fixture")]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::Path;

use headwork_adapters::host::wmux::{
    Runner, Wmux, parse_agents, parse_states, parse_surfaces, parse_tree, parse_workspaces,
};
use headwork_adapters::host::{
    Choice, HostError, Key, PaneId, SurfaceId, Target, TerminalHost, WorkspaceId, command_line,
};

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/wmux")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

#[derive(Debug, Default)]
struct Recorder {
    answers: RefCell<VecDeque<String>>,
    calls: RefCell<Vec<String>>,
}

impl Recorder {
    fn answering(answers: &[&str]) -> Self {
        Self {
            answers: RefCell::new(answers.iter().map(|a| (*a).to_owned()).collect()),
            calls: RefCell::default(),
        }
    }
}

impl Runner for &Recorder {
    fn run_as(&self, caller: Option<&str>, args: &[&str]) -> Result<String, HostError> {
        let line = args.join(" ");
        self.calls
            .borrow_mut()
            .push(caller.map_or_else(|| line.clone(), |c| format!("{c}> {line}")));
        Ok(self.answers.borrow_mut().pop_front().unwrap_or_default())
    }
}

#[test]
fn agent_list_parses() {
    let panes = parse_agents(&fixture("agent-list.json")).unwrap();
    assert_eq!(panes.len(), 2, "two agents");
    let car = panes.get(1).unwrap();
    assert_eq!(car.label, "me-sim/car-training", "label");
    assert!(car.running, "running");
    assert_eq!(car.pid, Some(38760), "pid");
    assert_eq!(
        car.workspace.as_ref().map(WorkspaceId::as_str),
        Some("ws-ee26d8f1-a97f-4fa3-88e8-55b725f4f72b"),
        "workspace"
    );
    assert!(!panes.first().unwrap().running, "exited agent");
}

#[test]
fn workspaces_surfaces_and_states_parse() {
    let workspaces = parse_workspaces(&fixture("list-workspaces.json")).unwrap();
    assert_eq!(
        workspaces
            .iter()
            .map(|w| w.title.as_str())
            .collect::<Vec<_>>(),
        ["Workspace 1", "me-sim"],
        "titles"
    );
    let surfaces = parse_surfaces(&fixture("list-panes.json")).unwrap();
    assert_eq!(surfaces.len(), 2, "both tabs of the pane");
    let states = parse_states(&fixture("agent-state.json")).unwrap();
    assert_eq!(
        states.first().unwrap().session_id.as_deref(),
        Some("d36cc867-1d8e-4167-9049-862e6fbedd50"),
        "session id"
    );
    assert_eq!(
        states.get(1).unwrap().blocked_reason.as_deref(),
        Some("Review m2"),
        "blocked reason"
    );
}

#[test]
fn command_lines_quote_like_the_c_runtime() {
    let argv: Vec<String> = [
        "S:/bin/headwork.exe",
        "launch",
        "--task",
        "7",
        "a b",
        "say \"hi\"",
        "",
        "tail\\",
        "C:\\dir with space\\",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    assert_eq!(
        command_line(&argv),
        r#"S:/bin/headwork.exe launch --task 7 "a b" "say \"hi\"" "" tail\ "C:\dir with space\\""#,
        "quoting"
    );
}

#[test]
fn ping_wants_pong() {
    let up = Recorder::answering(&["pong\n"]);
    assert!(Wmux::new(&up).ping().is_ok(), "pong");
    let down = Recorder::answering(&["nope"]);
    assert!(
        matches!(Wmux::new(&down).ping(), Err(HostError::NotRunning)),
        "not pong"
    );
}

#[test]
fn ensure_workspace_creates_it_once() {
    let empty = r#"{"workspaces": []}"#;
    let made = r#"{"workspaces": [{"id": "ws-9", "title": "headwork-test"}]}"#;
    let recorder = Recorder::answering(&[empty, "", made]);
    let host = Wmux::new(&recorder);
    let (id, fresh) = host
        .ensure_workspace("headwork-test", Path::new("S:/Code"))
        .unwrap();
    assert_eq!((id.as_str(), fresh), ("ws-9", true), "created");
    assert_eq!(
        recorder.calls.borrow().get(1).unwrap(),
        "new-workspace --title headwork-test --cwd S:/Code --panes 1 --layout single",
        "creation command"
    );
    let again = Recorder::answering(&[made]);
    let (_, fresh) = Wmux::new(&again)
        .ensure_workspace("headwork-test", Path::new("S:/Code"))
        .unwrap();
    assert!(!fresh, "found");
}

#[test]
fn spawn_replaces_the_shell_of_a_pane_and_finds_the_new_agent() {
    let before = r#"{"agents": []}"#;
    let after = r#"{"agents": [{"agentId": "agent-1", "surfaceId": "surf-1", "workspaceId": "ws-9",
        "label": "headwork-test/probe", "cmd": "x", "status": "running", "pid": 42, "spawnTime": 5}]}"#;
    let recorder = Recorder::answering(&[before, "", after]);
    let argv = vec![
        "headwork".to_owned(),
        "launch".to_owned(),
        "--task".to_owned(),
        "3".to_owned(),
    ];
    let pane = Wmux::new(&recorder)
        .spawn(
            &WorkspaceId("ws-9".to_owned()),
            Path::new("S:/w"),
            "headwork-test/probe",
            &argv,
            &Target::Pane {
                pane: PaneId("pane-1".to_owned()),
                replace_tab: true,
            },
        )
        .unwrap();
    assert_eq!(pane.surface.as_str(), "surf-1", "surface");
    assert_eq!(pane.pid, Some(42), "pid");
    assert_eq!(
        recorder.calls.borrow().get(1).unwrap(),
        "agent spawn --workspace ws-9 --cwd S:/w --label headwork-test/probe --cmd headwork launch --task 3 \
         --pane pane-1 --replace-tab",
        "spawn command"
    );
    let tab = Recorder::answering(&[before, "", after]);
    Wmux::new(&tab)
        .spawn(
            &WorkspaceId("ws-9".to_owned()),
            Path::new("S:/w"),
            "headwork-test/probe",
            &argv,
            &Target::Tab,
        )
        .unwrap();
    assert_eq!(
        tab.calls.borrow().get(1).unwrap(),
        "agent spawn --workspace ws-9 --cwd S:/w --label headwork-test/probe --cmd headwork launch --task 3",
        "a tab names no pane"
    );
}

#[test]
fn tree_parses_into_panes_with_their_sizes() {
    let tree = parse_tree(&fixture("tree.json")).unwrap();
    let cells = tree.cells();
    let summary: Vec<(&str, usize, f64, f64)> = cells
        .iter()
        .map(|c| (c.pane.as_str(), c.surfaces.len(), c.width, c.height))
        .collect();
    assert_eq!(
        summary,
        [
            ("pane-a", 2, 0.5, 0.5),
            ("pane-c", 1, 0.5, 0.5),
            ("pane-b", 1, 0.5, 1.0),
        ],
        "a left column split top and bottom beside a right pane"
    );
}

#[test]
fn split_runs_as_the_surface_beside_it() {
    let recorder = Recorder::answering(&[r#"{"paneId": "pane-2", "surfaceId": "surf-9"}"#]);
    let host = Wmux::new(&recorder);
    let (pane, shell) = host.split(&SurfaceId("surf-1".to_owned()), true).unwrap();
    assert_eq!(
        (pane.as_str(), shell.as_str()),
        ("pane-2", "surf-9"),
        "new pane"
    );
    host.close_pane(&pane).unwrap();
    assert_eq!(
        *recorder.calls.borrow(),
        ["surf-1> split --down", "close-pane pane-2"],
        "commands"
    );
}

#[test]
fn keys_blocked_states_and_closes_name_the_surface() {
    let recorder = Recorder::default();
    let host = Wmux::new(&recorder);
    let surface = SurfaceId("surf-1".to_owned());
    host.send_key(&surface, Key::Ctrl('c')).unwrap();
    host.send_key(&surface, Key::Enter).unwrap();
    host.show_blocked(
        &surface,
        "Ship it?",
        &[Choice {
            id: "yes".to_owned(),
            label: "1 Ship it".to_owned(),
            key: "1".to_owned(),
        }],
    )
    .unwrap();
    host.clear_blocked(&surface).unwrap();
    host.close_surface(&surface).unwrap();
    assert_eq!(
        *recorder.calls.borrow(),
        [
            "send-key c --surface surf-1 --ctrl",
            "send-key enter --surface surf-1",
            r#"report-agent --blocked Ship it? --choices [{"id":"yes","label":"1 Ship it","key":"1"}] --surface surf-1"#,
            "report-agent --unblocked --surface surf-1",
            "close-surface surf-1",
        ],
        "commands"
    );
}
