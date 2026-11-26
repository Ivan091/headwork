//! `headwork gate run` and `headwork gate status` against a temporary `HEADWORK_HOME` and `HEADWORK_GATE_DIR`.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const MACHINE: &str = "[machine]\nhost = \"wmux\"\n";

struct Gate {
    home: tempfile::TempDir,
    slots: tempfile::TempDir,
}

impl Gate {
    fn new(config: Option<&str>) -> Self {
        let home = tempfile::tempdir().unwrap();
        if let Some(config) = config {
            std::fs::write(home.path().join("config.toml"), config).unwrap();
        }
        Self {
            home,
            slots: tempfile::tempdir().unwrap(),
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_headwork"));
        command
            .args(args)
            .env("HEADWORK_HOME", self.home.path())
            .env("HEADWORK_GATE_DIR", self.slots.path())
            .env(
                "HEADWORK_CLAUDE_JSON",
                self.home.path().join(".claude.json"),
            )
            .env("HEADWORK_CLAUDE_DIR", self.home.path().join(".claude"));
        for var in [
            "CI",
            "HEADWORK_GATE_OFF",
            "HEADWORK_GATE_HELD",
            "HEADWORK_HEAVY_HELD",
            "HEADWORK_TASK",
            "HEADWORK_ROLE",
        ] {
            command.env_remove(var);
        }
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn spawn(&self, args: &[&str]) -> Child {
        self.command(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    fn cpu(&self) -> PathBuf {
        self.slots.path().join("cpu")
    }
}

fn entries(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir).map_or_else(
        |_| Vec::new(),
        |entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        },
    )
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn status_works_without_a_config() {
    let gate = Gate::new(None);
    let output = gate.run(&["gate", "status"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let out = text(&output.stdout);
    assert!(out.contains("cpu 0/3") && out.contains("gpu 0/2"), "{out}");
    let json = gate.run(&["gate", "status", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["classes"][0]["class"], "cpu", "{value}");
    assert_eq!(value["classes"][1]["count"], 2, "{value}");
}

#[test]
fn run_holds_a_slot_passes_the_exit_code_and_gives_the_slot_back() {
    let gate = Gate::new(None);
    let output = gate.run(&[
        "gate",
        "run",
        "cpu",
        "--",
        "bash",
        "-c",
        "printf '%s|' \"$HEADWORK_GATE_HELD\"; ls \"$HEADWORK_GATE_DIR/cpu\"; exit 7",
    ]);
    assert_eq!(output.status.code(), Some(7), "{}", text(&output.stderr));
    let out = text(&output.stdout);
    assert!(
        out.starts_with("cpu|"),
        "the command sees the held class: {out}"
    );
    assert!(
        out.contains("slot-1"),
        "the slot is held while it runs: {out}"
    );
    assert!(
        entries(&gate.cpu()).is_empty(),
        "slot and wait file are gone: {:?}",
        entries(&gate.cpu())
    );
    assert!(
        !gate.home.path().join("state.db").exists(),
        "the gate never creates the store"
    );
}

#[test]
fn a_command_that_does_not_start_exits_127_and_frees_its_slot() {
    let gate = Gate::new(None);
    let output = gate.run(&["gate", "run", "gpu", "--", "headwork-no-such-program"]);
    assert_eq!(output.status.code(), Some(127), "{}", text(&output.stderr));
    assert!(
        entries(&gate.slots.path().join("gpu")).is_empty(),
        "no slot left"
    );
}

#[test]
fn a_second_holder_waits_for_the_first() {
    let gate = Gate::new(Some(&format!("{MACHINE}[gates]\ncpu = 1\n")));
    let marks = gate.home.path().join("marks");
    let job = |name: &str| {
        format!(
            "echo start-{name} >> '{0}'; sleep 1; echo end-{name} >> '{0}'",
            marks.display().to_string().replace('\\', "/")
        )
    };
    let first = gate.spawn(&["gate", "run", "cpu", "--", "bash", "-c", &job("a")]);
    wait_until("the first holds its slot", || {
        gate.cpu().join("slot-1").join("owner").exists()
    });
    let second = gate.spawn(&["gate", "run", "cpu", "--", "bash", "-c", &job("b")]);
    let first = first.wait_with_output().unwrap();
    let second = second.wait_with_output().unwrap();
    assert!(first.status.success(), "{}", text(&first.stderr));
    assert!(second.status.success(), "{}", text(&second.stderr));
    let waited = text(&second.stderr);
    assert!(
        waited.contains("headwork: waiting for cpu (1/1: "),
        "the second says what it waits for: {waited}"
    );
    assert!(
        waited.contains("echo start-a"),
        "and names the holder's command: {waited}"
    );
    let order = std::fs::read_to_string(&marks).unwrap();
    let lines: Vec<&str> = order.lines().collect();
    assert_eq!(
        lines,
        ["start-a", "end-a", "start-b", "end-b"],
        "one at a time: {order}"
    );
    assert!(
        entries(&gate.cpu()).is_empty(),
        "nothing left: {:?}",
        entries(&gate.cpu())
    );
}

#[test]
fn a_slot_whose_holder_is_gone_is_taken_over() {
    let gate = Gate::new(Some(&format!("{MACHINE}[gates]\ncpu = 1\n")));
    let mut gone = Command::new(env!("CARGO_BIN_EXE_headwork"))
        .arg("--version")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let pid = gone.id();
    gone.wait().unwrap();
    let slot = gate.cpu().join("slot-1");
    std::fs::create_dir_all(&slot).unwrap();
    std::fs::write(
        slot.join("owner"),
        format!("pid={pid}\nnative=1\nclass=cpu\ncommand=dead\n"),
    )
    .unwrap();
    let status = gate.run(&["gate", "status"]);
    assert!(
        text(&status.stdout).contains("stale"),
        "{}",
        text(&status.stdout)
    );
    let output = gate.run(&["gate", "run", "cpu", "--", "bash", "-c", "exit 0"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(
        !text(&output.stderr).contains("waiting"),
        "no wait: {}",
        text(&output.stderr)
    );
    assert!(!slot.exists(), "the stale slot is gone");
}

/// Exits 4 when the command sees `cpu` in `HEADWORK_GATE_HELD`, else 5.
const HOLDS_CPU: &str = "case \",$HEADWORK_GATE_HELD,\" in *,cpu,*) exit 4 ;; esac; exit 5";

#[test]
fn nested_ci_and_off_run_directly() {
    let gate = Gate::new(None);
    let cases: &[(&str, &str)] = &[
        ("HEADWORK_GATE_HELD", "gpu,cpu"),
        ("HEADWORK_HEAVY_HELD", "1"),
        ("CI", "true"),
        ("HEADWORK_GATE_OFF", "1"),
    ];
    for (var, value) in cases {
        let output = gate
            .command(&["gate", "run", "cpu", "--", "bash", "-c", HOLDS_CPU])
            .env(var, value)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(4),
            "{var}: {}",
            text(&output.stderr)
        );
        assert!(!gate.cpu().exists(), "{var}: no slot folder is made");
    }
    let gpu = gate
        .command(&["gate", "run", "gpu", "--", "bash", "-c", "exit 0"])
        .env("HEADWORK_GATE_HELD", "cpu")
        .output()
        .unwrap();
    assert!(gpu.status.success(), "{}", text(&gpu.stderr));
    assert!(
        gate.slots.path().join("gpu").exists(),
        "holding cpu still gates gpu"
    );
}

#[test]
fn a_command_run_without_a_slot_is_not_marked_as_holding_one() {
    let gate = Gate::new(None);
    let file = gate.slots.path().join("a-file");
    std::fs::write(&file, "").unwrap();
    let output = gate
        .command(&["gate", "run", "cpu", "--", "bash", "-c", HOLDS_CPU])
        .env("HEADWORK_GATE_DIR", file.join("below"))
        .output()
        .unwrap();
    let err = text(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(5),
        "nested commands still gate: {err}"
    );
    assert!(err.contains("running without a slot"), "{err}");
}

#[test]
fn events_go_to_an_existing_store() {
    let gate = Gate::new(Some(MACHINE));
    let board = gate.run(&["board"]);
    assert!(board.status.success(), "{}", text(&board.stderr));
    let output = gate.run(&["gate", "run", "cpu", "--", "bash", "-c", "exit 2"]);
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stderr));
    let log = gate.run(&["log", "--json"]);
    let events: serde_json::Value = serde_json::from_slice(&log.stdout).unwrap();
    let events = events.as_array().unwrap();
    let find = |kind: &str| events.iter().find(|event| event["type"] == kind).cloned();
    let acquired = find("gate-acquired").unwrap();
    assert_eq!(acquired["data"]["class"], "cpu", "{acquired}");
    assert!(acquired["data"]["waited_s"].is_u64(), "{acquired}");
    let released = find("gate-released").unwrap();
    assert_eq!(released["data"]["exit"], 2, "{released}");
    assert!(released["data"]["held_s"].is_u64(), "{released}");
    assert!(
        released["data"]["command"]
            .as_str()
            .unwrap()
            .starts_with("bash -c"),
        "{released}"
    );
    assert!(find("gate-wait").is_none(), "it did not wait");
}

#[test]
fn run_needs_a_known_class_and_a_command() {
    let gate = Gate::new(None);
    assert!(
        !gate
            .run(&["gate", "run", "disk", "--", "true"])
            .status
            .success(),
        "unknown class"
    );
    assert!(
        !gate.run(&["gate", "run", "cpu"]).status.success(),
        "no command"
    );
}
