//! The cmux adapter: parsers against saved cmux output, and the command lines it sends, through a recording runner.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests fail loudly on a bad fixture"
)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::Path;

use headwork_adapters::host::cmux::{
    Cmux, Runner, parse_initial_command, parse_programs, parse_states, parse_tree,
    parse_workspaces, quote, shell_line, shell_words,
};
use headwork_adapters::host::{
    AgentId, Choice, HostError, Key, PaneId, PaneTree, SurfaceId, Target, TerminalHost, WorkspaceId,
};

const WORKSPACE: &str = "926F5E33-D23F-459B-8FE7-FC1EFB7F5FF9";
const TOP: &str = "2A620EEB-7BCC-4224-AE37-9CC58B65A47D";
const BOTTOM: &str = "C127F88D-4A0F-4914-AF10-944F9540E12A";
const SHELL: &str = "95868C00-DC5A-489D-9684-8A0BC78C66B2";
const WORKER: &str = "7800EA2C-9108-4295-BC77-AECA4724B2C8";
const LOWER: &str = "07F2921B-3BC2-44F9-B0A1-3FA2B32E7045";

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/cmux")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

fn workspace() -> WorkspaceId {
    WorkspaceId(WORKSPACE.to_owned())
}

#[derive(Debug, Default)]
struct Recorder {
    answers: RefCell<VecDeque<Result<String, String>>>,
    calls: RefCell<Vec<String>>,
}

impl Recorder {
    fn answering(answers: &[&str]) -> Self {
        Self {
            answers: RefCell::new(answers.iter().map(|a| Ok((*a).to_owned())).collect()),
            calls: RefCell::default(),
        }
    }

    fn then_failing(self, message: &str) -> Self {
        self.answers.borrow_mut().push_back(Err(message.to_owned()));
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl Runner for &Recorder {
    fn run(&self, args: &[&str]) -> Result<String, HostError> {
        self.calls.borrow_mut().push(args.join(" "));
        match self.answers.borrow_mut().pop_front() {
            Some(Err(message)) => Err(HostError::Failed {
                command: args.join(" "),
                message,
            }),
            Some(Ok(text)) => Ok(text),
            None => Ok(String::new()),
        }
    }
}

fn host(recorder: &Recorder) -> Cmux<&Recorder> {
    Cmux::new(recorder).with_search_path(Some("/opt/homebrew/bin:/usr/bin".to_owned()))
}

#[test]
fn the_tree_parses_into_workspaces_and_a_layout() {
    let text = fixture("tree.json");
    let workspaces = parse_workspaces(&text).unwrap();
    assert_eq!(workspaces.len(), 1, "one workspace");
    assert_eq!(workspaces.first().unwrap().title, "hw-probe", "title");
    let tree = parse_tree(&text, &workspace()).unwrap().unwrap();
    let PaneTree::Split { down, ratio, parts } = &tree else {
        panic!("a split: {tree:?}");
    };
    assert!(*down, "cmux's vertical split stacks top and bottom");
    assert!((ratio - 0.5).abs() < f64::EPSILON, "ratio");
    assert_eq!(parts.len(), 2, "two panes");
    let cells = tree.cells();
    assert_eq!(
        cells.first().unwrap().pane,
        PaneId(TOP.to_owned()),
        "top first"
    );
    assert_eq!(cells.first().unwrap().surfaces.len(), 5, "five tabs on top");
    assert_eq!(
        cells.get(1).unwrap().surfaces,
        [SurfaceId(LOWER.to_owned())],
        "one tab below"
    );
    assert!(
        parse_tree(&text, &WorkspaceId("other".to_owned()))
            .unwrap()
            .is_none(),
        "another workspace"
    );
}

#[test]
fn only_programs_headwork_started_are_listed() {
    let programs = parse_programs(&fixture("surface-list.json"), &workspace()).unwrap();
    assert_eq!(
        programs.len(),
        1,
        "the shell and the plain terminals are not programs"
    );
    let program = programs.first().unwrap();
    assert_eq!(program.label, "demo/sand", "label");
    assert_eq!(
        program.agent,
        AgentId(WORKER.to_owned()),
        "agent is the surface"
    );
    assert!(program.running, "a listed surface runs");
    assert_eq!(program.workspace, Some(workspace()), "workspace");
    assert!(
        program.command.starts_with("/bin/sh -c "),
        "{}",
        program.command
    );
}

#[test]
fn states_come_from_the_active_session_of_each_surface() {
    let states = parse_states(&fixture("sessions-list.json")).unwrap();
    let words: Vec<(&str, &str)> = states
        .iter()
        .map(|state| (state.surface.as_str(), state.state.as_str()))
        .collect();
    assert_eq!(
        words,
        [
            (WORKER, "working"),
            (LOWER, "blocked"),
            ("DEADBEEF-0000-4000-8000-000000000000", "idle")
        ],
        "inactive sessions are skipped"
    );
    assert_eq!(
        states.first().unwrap().session_id.as_deref(),
        Some("0d439313-b93d-46bd-9765-f9f66414d535"),
        "session id"
    );
}

#[test]
fn surface_states_keep_only_open_surfaces() {
    let recorder = Recorder::answering(&[&fixture("sessions-list.json"), &fixture("tree.json")]);
    let states = host(&recorder).surface_states().unwrap();
    assert_eq!(states.len(), 2, "the closed surface's session is dropped");
    assert_eq!(
        recorder.calls(),
        [
            "sessions list --json --agent claude",
            r#"rpc system.tree {"all":true}"#
        ],
        "calls"
    );
}

#[test]
fn words_quote_and_split_back() {
    let argv: Vec<String> = [
        "/Users/me/bin/headwork",
        "launch",
        "--task",
        "7",
        "a b",
        "it's",
        "",
        "$HOME",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let line = shell_line(&argv);
    assert_eq!(
        line, r#"/Users/me/bin/headwork launch --task 7 'a b' 'it'\''s' '' '$HOME'"#,
        "quoting"
    );
    assert_eq!(shell_words(&line), argv, "round trip");
    assert_eq!(
        shell_words(r#"a "b \"c\" \$d" e\ f"#),
        ["a", r#"b "c" $d"#, "e f"],
        "double quotes and backslashes"
    );
    assert_eq!(quote("demo/sand"), "demo/sand", "safe words stay bare");
}

#[test]
fn ping_wants_pong() {
    let up = Recorder::answering(&["{\n  \"pong\" : true\n}\n"]);
    assert!(host(&up).ping().is_ok(), "pong");
    assert_eq!(up.calls(), ["rpc system.ping {}"], "call");
    let down = Recorder::default().then_failing("Error: socket not found");
    assert!(
        matches!(host(&down).ping(), Err(HostError::NotRunning)),
        "no socket"
    );
}

#[test]
fn ensure_workspace_creates_it_once() {
    let created = r#"{"surface_id": "S-1", "workspace_id": "W-1"}"#;
    let recorder = Recorder::answering(&[&fixture("tree.json"), created]);
    let (id, fresh) = host(&recorder)
        .ensure_workspace("headwork-test", Path::new("/tmp/proj"))
        .unwrap();
    assert_eq!((id.as_str(), fresh), ("W-1", true), "created");
    assert_eq!(
        recorder.calls().get(1).unwrap(),
        r#"rpc workspace.create {"cwd":"/tmp/proj","focus":false,"title":"headwork-test"}"#,
        "create"
    );
    let again = Recorder::answering(&[&fixture("tree.json")]);
    let (id, fresh) = host(&again)
        .ensure_workspace("hw-probe", Path::new("/tmp"))
        .unwrap();
    assert_eq!((id.as_str(), fresh), (WORKSPACE, false), "found");
    assert_eq!(again.calls().len(), 1, "no create");
}

#[test]
fn spawn_writes_the_label_and_path_into_the_initial_command() {
    let created = r#"{"surface_id": "S-2", "pane_id": "P-1", "workspace_id": "926F5E33-D23F-459B-8FE7-FC1EFB7F5FF9"}"#;
    let recorder = Recorder::answering(&[created]);
    let argv = vec![
        "/Users/me/.local/bin/headwork".to_owned(),
        "launch".to_owned(),
        "--task".to_owned(),
        "3".to_owned(),
    ];
    let pane = host(&recorder)
        .spawn(
            &workspace(),
            Path::new("/tmp/wt"),
            "demo/sand",
            &argv,
            &Target::Tab,
        )
        .unwrap();
    assert_eq!(pane.surface, SurfaceId("S-2".to_owned()), "surface");
    assert_eq!(pane.label, "demo/sand", "label");
    assert_eq!(
        pane.command, "/Users/me/.local/bin/headwork launch --task 3",
        "command"
    );
    let calls = recorder.calls();
    let call = calls.first().unwrap();
    let params: serde_json::Value =
        serde_json::from_str(call.strip_prefix("rpc surface.create ").unwrap()).unwrap();
    let initial = params["initial_command"].as_str().unwrap();
    assert_eq!(
        initial,
        r#"/usr/bin/env HEADWORK_HOST_LABEL=demo/sand "PATH=${PATH%%:*}:"/opt/homebrew/bin:/usr/bin /Users/me/.local/bin/headwork launch --task 3"#,
        "the surface's own shims stay first on PATH"
    );
    assert_eq!(
        parse_initial_command(initial).unwrap(),
        ("demo/sand".to_owned(), argv),
        "read back"
    );
    assert_eq!(params["working_directory"], "/tmp/wt", "cwd");
    assert_eq!(params["focus"], false, "focus stays with the user");
    assert!(params.get("pane_id").is_none(), "the host picks the pane");
}

#[test]
fn spawn_into_a_pane_replaces_its_active_tab() {
    let created = r#"{"surface_id": "S-3", "pane_id": "C127F88D-4A0F-4914-AF10-944F9540E12A"}"#;
    let recorder = Recorder::answering(&[&fixture("tree.json"), created, "{}"]);
    let target = Target::Pane {
        pane: PaneId(BOTTOM.to_owned()),
        replace_tab: true,
    };
    host(&recorder)
        .spawn(
            &workspace(),
            Path::new("/tmp"),
            "demo/mist",
            &["claude".to_owned()],
            &target,
        )
        .unwrap();
    let calls = recorder.calls();
    assert!(
        calls
            .get(1)
            .unwrap()
            .contains(&format!(r#""pane_id":"{BOTTOM}""#)),
        "into the pane"
    );
    assert_eq!(
        calls.get(2).unwrap(),
        &format!(r#"rpc surface.close {{"surface_id":"{LOWER}"}}"#),
        "the pane's shell closes after the program opened"
    );
}

#[test]
fn split_cuts_the_first_pane_and_split_pane_any_pane() {
    let split = r#"{"surface_id": "S-9", "pane_id": "P-9"}"#;
    let recorder = Recorder::answering(&[&fixture("tree.json"), split]);
    let (pane, surface) = host(&recorder)
        .split(&SurfaceId(LOWER.to_owned()), true)
        .unwrap();
    assert_eq!(
        (pane.as_str(), surface.as_str()),
        ("P-9", "S-9"),
        "new pane"
    );
    assert_eq!(
        recorder.calls().get(1).unwrap(),
        &format!(
            r#"rpc surface.split {{"direction":"down","focus":false,"surface_id":"{SHELL}"}}"#
        ),
        "the top pane's active tab is split"
    );
    let beside = Recorder::answering(&[&fixture("tree.json"), split]);
    host(&beside)
        .split_pane(&workspace(), &PaneId(BOTTOM.to_owned()))
        .unwrap();
    assert_eq!(
        beside.calls().get(1).unwrap(),
        &format!(
            r#"rpc surface.split {{"direction":"right","focus":false,"surface_id":"{LOWER}"}}"#
        ),
        "side by side"
    );
}

#[test]
fn close_pane_closes_its_surfaces_and_kill_tolerates_a_closed_one() {
    let recorder = Recorder::answering(&[&fixture("tree.json"), "{}"]);
    host(&recorder)
        .close_pane(&PaneId(BOTTOM.to_owned()))
        .unwrap();
    assert_eq!(
        recorder.calls().get(1).unwrap(),
        &format!(r#"rpc surface.close {{"surface_id":"{LOWER}"}}"#),
        "close"
    );
    let gone = Recorder::default().then_failing("Error: not_found: Surface not found");
    assert!(
        host(&gone).kill_agent(&AgentId("S-0".to_owned())).is_ok(),
        "already gone"
    );
}

#[test]
fn keys_and_text() {
    let recorder = Recorder::default();
    let host = host(&recorder);
    let surface = SurfaceId("S".to_owned());
    host.send_text(&surface, "/exit").unwrap();
    host.send_key(&surface, Key::Enter).unwrap();
    host.send_key(&surface, Key::Escape).unwrap();
    host.send_key(&surface, Key::Char('1')).unwrap();
    host.send_key(&surface, Key::Ctrl('C')).unwrap();
    host.rename_surface(&surface, "demo-sand").unwrap();
    host.read_screen(&surface, 40).unwrap_err();
    assert_eq!(
        recorder.calls(),
        [
            r#"rpc surface.send_text {"surface_id":"S","text":"/exit"}"#,
            r#"rpc surface.send_key {"key":"enter","surface_id":"S"}"#,
            r#"rpc surface.send_key {"key":"escape","surface_id":"S"}"#,
            r#"rpc surface.send_text {"surface_id":"S","text":"1"}"#,
            r#"rpc surface.send_text {"surface_id":"S","text":"\u0003"}"#,
            r#"rpc tab.action {"action":"rename","surface_id":"S","title":"demo-sand"}"#,
            r#"rpc surface.read_text {"lines":40,"scrollback":true,"surface_id":"S"}"#,
        ],
        "calls"
    );
}

#[test]
fn a_blocked_state_is_a_sidebar_status_and_a_notification() {
    let recorder = Recorder::answering(&[&fixture("tree.json"), "OK", "{}"]);
    let choices = [
        Choice {
            id: "yes".to_owned(),
            label: "Yes".to_owned(),
            key: "1".to_owned(),
        },
        Choice {
            id: "no".to_owned(),
            label: "No".to_owned(),
            key: "2".to_owned(),
        },
    ];
    host(&recorder)
        .show_blocked(&SurfaceId(LOWER.to_owned()), "Trust the folder?", &choices)
        .unwrap();
    let calls = recorder.calls();
    assert_eq!(
        calls.get(1).unwrap(),
        &format!(
            "set-status headwork-{LOWER} Trust the folder? (1 Yes · 2 No) --workspace {WORKSPACE} --icon bell.fill \
             --color #FF9500 --priority 100"
        ),
        "status"
    );
    assert!(
        calls
            .get(2)
            .unwrap()
            .starts_with("rpc notification.create_for_surface "),
        "notification"
    );
    let clear = Recorder::answering(&[&fixture("tree.json"), "OK"]);
    host(&clear)
        .clear_blocked(&SurfaceId(LOWER.to_owned()))
        .unwrap();
    assert_eq!(
        clear.calls().get(1).unwrap(),
        &format!("clear-status headwork-{LOWER} --workspace {WORKSPACE}"),
        "cleared"
    );
    let closed = Recorder::answering(&[&fixture("tree.json")]);
    host(&closed)
        .clear_blocked(&SurfaceId("gone".to_owned()))
        .unwrap();
    assert_eq!(
        closed.calls().len(),
        1,
        "nothing to clear on a closed surface"
    );
}

#[test]
fn panes_list_every_workspace() {
    let recorder = Recorder::answering(&[&fixture("tree.json"), &fixture("surface-list.json")]);
    let programs = host(&recorder).panes().unwrap();
    assert_eq!(programs.len(), 1, "one program");
    assert_eq!(
        recorder.calls().get(1).unwrap(),
        &format!(r#"rpc surface.list {{"workspace_id":"{WORKSPACE}"}}"#),
        "listed"
    );
}
