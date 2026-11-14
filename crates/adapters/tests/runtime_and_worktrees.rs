//! The claude argv, and worktrees made and removed by git and by a project script.

#![expect(clippy::unwrap_used, reason = "tests fail loudly when git fails")]

use std::path::Path;
use std::process::Command;

use headwork_adapters::runtime::ClaudeLaunch;
use headwork_adapters::worktree::{Worktrees, parse_porcelain, same_path};
use headwork_core::config::{TaskVars, WorktreeCommand};
use headwork_core::{Effort, Model, PermissionMode};

#[test]
fn argv_for_a_new_worker() {
    let launch = ClaudeLaunch {
        session_id: "0f8e",
        resume: false,
        name: "me-sim-sand",
        remote_control: true,
        model: Some(Model::Haiku),
        effort: None,
        permission_mode: PermissionMode::Auto,
        agent: None,
        plugin_dir: None,
        prompt: "Read your brief.",
    };
    assert_eq!(
        launch.argv(),
        [
            "claude",
            "--session-id",
            "0f8e",
            "--remote-control",
            "me-sim-sand",
            "-n",
            "me-sim-sand",
            "--model",
            "haiku",
            "--permission-mode",
            "auto",
            "--",
            "Read your brief."
        ],
        "argv"
    );
}

#[test]
fn effort_becomes_the_effort_flag() {
    let launch = ClaudeLaunch {
        session_id: "0f8e",
        resume: false,
        name: "x",
        remote_control: false,
        model: Some(Model::Sonnet),
        effort: Some(Effort::Xhigh),
        permission_mode: PermissionMode::Auto,
        agent: None,
        plugin_dir: None,
        prompt: "p",
    };
    assert!(
        launch
            .argv()
            .windows(2)
            .any(|pair| pair == ["--effort", "xhigh"]),
        "effort flag"
    );
}

#[test]
fn argv_for_a_resumed_orchestrator_without_remote_control() {
    let launch = ClaudeLaunch {
        session_id: "0f8e",
        resume: true,
        name: "orchestrator",
        remote_control: false,
        model: Some(Model::Opus),
        effort: Some(Effort::High),
        permission_mode: PermissionMode::Default,
        agent: Some("orchestrator"),
        plugin_dir: Some("S:/x/plugin"),
        prompt: "Resumed.",
    };
    let argv = launch.argv();
    assert_eq!(argv.get(1).map(String::as_str), Some("--resume"), "resume");
    assert!(
        !argv.iter().any(|a| a == "--remote-control"),
        "no remote control"
    );
    assert!(
        argv.windows(2)
            .any(|pair| pair == ["--agent", "orchestrator"]),
        "agent"
    );
    assert!(
        argv.windows(2)
            .any(|pair| pair == ["--plugin-dir", "S:/x/plugin"]),
        "plugin"
    );
    assert_eq!(
        argv.iter().rev().nth(1).map(String::as_str),
        Some("--"),
        "the prompt stays last"
    );
}

#[test]
fn argv_without_a_model_leaves_it_to_claude() {
    let launch = ClaudeLaunch {
        session_id: "0f8e",
        resume: false,
        name: "orchestrator",
        remote_control: true,
        model: None,
        effort: None,
        permission_mode: PermissionMode::Default,
        agent: Some("orchestrator"),
        plugin_dir: None,
        prompt: "Hi.",
    };
    assert!(
        !launch.argv().iter().any(|a| a == "--model"),
        "{:?}",
        launch.argv()
    );
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
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

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("main");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "master"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    dir
}

#[test]
fn builtin_worktrees_are_made_listed_and_removed() {
    let dir = repo();
    let main = dir.path().join("main");
    let worktree = dir.path().join("main-sand");
    let (main_text, worktree_text) = (main.display().to_string(), worktree.display().to_string());
    let vars = TaskVars {
        slug: "sand",
        branch: "sand",
        main: &main_text,
        worktree: &worktree_text,
    };
    let worktrees = Worktrees::new("bash", &main);
    worktrees.create(&WorktreeCommand::Builtin, &vars).unwrap();
    assert!(worktree.is_dir(), "made");
    let listed = worktrees.list().unwrap();
    assert!(
        listed
            .iter()
            .any(|(path, branch)| same_path(path, &worktree) && branch.as_deref() == Some("sand")),
        "listed: {listed:?}"
    );
    worktrees.remove(&WorktreeCommand::Builtin, &vars).unwrap();
    assert!(!worktree.exists(), "removed");
}

#[test]
fn a_failing_script_is_reported() {
    let dir = repo();
    let main = dir.path().join("main");
    let vars = TaskVars {
        slug: "sand",
        branch: "sand",
        main: "",
        worktree: "",
    };
    let error = Worktrees::new("bash", &main)
        .create(
            &WorktreeCommand::Command("echo no {slug} >&2; exit 3".to_owned()),
            &vars,
        )
        .unwrap_err();
    assert!(error.to_string().contains("no sand"), "{error}");
}

#[test]
fn porcelain_and_paths() {
    let text = "worktree S:/Code/x\nHEAD abc\nbranch refs/heads/master\n\nworktree S:/Code/x-sand\nHEAD def\ndetached\n";
    let parsed = parse_porcelain(text);
    assert_eq!(parsed.len(), 2, "two");
    assert_eq!(
        parsed.first().unwrap().1.as_deref(),
        Some("master"),
        "branch"
    );
    assert_eq!(parsed.get(1).unwrap().1, None, "detached");
    assert!(
        same_path(Path::new("S:\\Code\\X\\"), Path::new("s:/code/x")),
        "same path"
    );
}
