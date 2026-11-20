//! Process trees: a grandchild dies with the tree when only its parent is killed.
//!
//! The test binary plays every part: the test starts it as a parent, the parent starts it again as a sleeper and
//! writes the sleeper's pid to a file. The two helpers are ignored tests that only act when their variable is set.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly when a process does not start"
)]

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use headwork_adapters::proc::{OwnedChild, Ownership, is_alive, kill_tree};

const ROLE: &str = "HEADWORK_PROC_ROLE";
const PID_FILE: &str = "HEADWORK_PROC_PID_FILE";

fn me() -> PathBuf {
    std::env::current_exe().unwrap()
}

fn helper(name: &str) -> Command {
    let mut command = Command::new(me());
    command.args([
        "--exact",
        name,
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ]);
    command
}

fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    done()
}

#[test]
#[ignore = "a helper started by tree_ends_with_its_parent"]
fn helper_sleeper() {
    if std::env::var_os(ROLE).is_some_and(|role| role == "sleeper") {
        std::thread::sleep(Duration::from_secs(120));
    }
}

#[test]
#[ignore = "a helper started by tree_ends_with_its_parent"]
fn helper_parent() {
    if std::env::var_os(ROLE).is_some_and(|role| role == "parent") {
        let mut sleeper = helper("helper_sleeper")
            .env(ROLE, "sleeper")
            .spawn()
            .expect("sleeper");
        let file = std::env::var_os(PID_FILE).expect("pid file");
        std::fs::write(file, sleeper.id().to_string()).unwrap();
        std::thread::sleep(Duration::from_secs(120));
        sleeper.wait().unwrap();
    }
}

#[test]
fn tree_ends_with_its_parent() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("sleeper.pid");
    let mut command = helper("helper_parent");
    command.env(ROLE, "parent").env(PID_FILE, &pid_file);
    let owned = OwnedChild::spawn(&mut command).unwrap();
    let expected = if cfg!(windows) {
        Ownership::Job
    } else {
        Ownership::ProcessGroup
    };
    assert_eq!(owned.ownership(), &expected, "the tree is owned");
    assert!(
        wait_until(Duration::from_secs(30), || pid_file.exists()
            && std::fs::read_to_string(&pid_file)
                .is_ok_and(|text| !text.is_empty())),
        "the sleeper started"
    );
    let sleeper: u32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(is_alive(sleeper).unwrap(), "the sleeper runs");

    let parent = owned.pid();
    kill_only(parent);
    owned.wait().unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || !is_alive(sleeper).unwrap()),
        "the sleeper died with the tree"
    );
}

#[test]
fn kill_tree_ends_a_process_and_reports_a_missing_one() {
    let mut command = helper("helper_sleeper");
    command.env(ROLE, "sleeper");
    let owned = OwnedChild::spawn(&mut command).unwrap();
    let pid = owned.pid();
    assert!(kill_tree(pid).unwrap(), "killed");
    owned.wait().unwrap();
    assert!(!is_alive(pid).unwrap(), "gone");
    assert!(!kill_tree(pid).unwrap(), "nothing left to kill");
}

#[cfg(windows)]
fn kill_only(pid: u32) {
    let status = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "taskkill");
}

#[cfg(unix)]
fn kill_only(pid: u32) {
    let status = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success(), "kill");
}

#[test]
fn a_detached_process_runs_on_its_own() {
    let mut command = helper("helper_sleeper");
    command.env(ROLE, "sleeper");
    let (pid, _) = headwork_adapters::proc::spawn_detached(&mut command).unwrap();
    assert!(
        wait_until(Duration::from_secs(5), || is_alive(pid).unwrap()),
        "it runs"
    );
    assert!(kill_tree(pid).unwrap(), "killed");
    assert!(
        wait_until(Duration::from_secs(5), || !is_alive(pid).unwrap()),
        "gone"
    );
}

#[test]
fn identify_names_a_running_process_and_nothing_once_it_ended() {
    use headwork_adapters::proc::{Liveness, ProcessIdentity, identify, liveness};
    let mut child = helper("helper_sleeper")
        .env(ROLE, "sleeper")
        .spawn()
        .unwrap();
    let pid = child.id();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let found = identify(pid).expect("the child runs");
    let image = me().file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        found.image.eq_ignore_ascii_case(&image),
        "{found:?} vs {image}"
    );
    assert!(
        found.started.abs_diff(now) <= 5,
        "{found:?} started near {now}"
    );
    let argv = vec!["anything".to_owned()];
    assert_eq!(
        liveness(pid, Some(&found), &argv),
        Liveness::Alive,
        "the same process"
    );
    let earlier = ProcessIdentity {
        started: found.started - 3600,
        ..found.clone()
    };
    assert!(
        matches!(
            liveness(pid, Some(&earlier), &argv),
            Liveness::Reused { .. }
        ),
        "a process that started later holds the pid"
    );
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(identify(pid), None, "gone");
    assert_eq!(liveness(pid, Some(&found), &argv), Liveness::Gone, "gone");
}

#[test]
fn git_bash_ps_links_each_shell_to_its_git_bash_parent() {
    let text = "      PID    PPID    PGID     WINPID   TTY         UID    STIME COMMAND
    50013       1   50013      37324  ?         197609 09:16:04 /usr/bin/bash
S   50014   50013   50013      45556  ?         197609 09:16:04 /usr/bin/bash
    50097   50014   50013      52564  ?         197609 09:16:04 /s/Code/me-sim/runs/bin/cars-rl-gpu
    50200   49999   50200      11111  ?         197609 09:16:04 /usr/bin/tail
";
    let parents = headwork_adapters::proc::parse_git_bash_ps(text);
    assert_eq!(
        parents.get(&37324),
        Some(&None),
        "a parent outside Git Bash"
    );
    assert_eq!(
        parents.get(&45556),
        Some(&Some(37324)),
        "a status letter first"
    );
    assert_eq!(parents.get(&52564), Some(&Some(45556)), "a Windows program");
    assert_eq!(parents.get(&11111), Some(&None), "a parent that ended");
    assert_eq!(parents.len(), 4, "the header is skipped: {parents:?}");
}
