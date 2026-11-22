//! Table tests of the hook rules: the tool guard by role, the ask checks and the stop gate.

use headwork_core::guard::ask::{AskFacts, Precondition, Question, check_ask};
use headwork_core::guard::stop::{MAX_BLOCKS, StopFacts, check_stop};
use headwork_core::guard::{
    DEFAULT_BRANCHES, Decision, GuardPath, Places, Scope, ToolCall, check_tool, repo_from_url,
};
use headwork_core::{Role, RuleId, TaskState};

const WORKTREE: &str = "S:/Code/me-sim/me-sim-sand";
const HOME: &str = "C:/Users/User/.headwork";
const TEMP: &str = "C:/Users/User/AppData/Local/Temp";
const USER: &str = "C:/Users/User";

fn orchestrator() -> Scope {
    Scope {
        role: Some(Role::Orchestrator),
        cwd: "S:/Code".to_owned(),
        writable: vec![GuardPath::new(HOME), GuardPath::new(TEMP)],
        scripts: vec![
            "scripts/ship.sh".to_owned(),
            "scripts/worktree.sh".to_owned(),
        ],
        user_only: vec!["scripts/publish-now.sh".to_owned()],
        default_branches: DEFAULT_BRANCHES.iter().map(|b| (*b).to_owned()).collect(),
        ..Scope::default()
    }
}

fn worker(branch: &str) -> Scope {
    Scope {
        role: Some(Role::Worker),
        cwd: WORKTREE.to_owned(),
        writable: vec![GuardPath::new(WORKTREE), GuardPath::new(TEMP)],
        read_only: vec![GuardPath::new("S:/Code/me-studio")],
        extra_writable: Vec::new(),
        user_only: vec!["scripts/publish-now.sh".to_owned()],
        long_jobs: vec![
            "scripts/rl-run.sh".to_owned(),
            "cargo run --release -p render".to_owned(),
        ],
        own_delivery: true,
        allowed_repos: vec!["Ivan091/me-sim".to_owned()],
        branch: Some(branch.to_owned()),
        default_branches: DEFAULT_BRANCHES.iter().map(|b| (*b).to_owned()).collect(),
        remotes: vec![
            ("origin".to_owned(), "Ivan091/me-sim".to_owned()),
            ("fork".to_owned(), "someone/me-sim".to_owned()),
        ],
        fable_allowed: false,
        scripts: Vec::new(),
        task_name: Some("me-sim-sand".to_owned()),
        places: Places {
            home: Some(USER.to_owned()),
            temp: Some(TEMP.to_owned()),
        },
        allowed_user_only: Vec::new(),
        main_checkout: Some(GuardPath::new(MAIN)),
        may_install: false,
        orchestrator_repos: Vec::new(),
        orchestrator_repo_excludes: Vec::new(),
    }
}

const MAIN: &str = "S:/Code/me-sim/me-sim";

#[test]
fn a_worker_may_fast_forward_its_main_checkout_and_nothing_more() {
    let scope = worker("sand");
    let table: &[(&str, Option<RuleId>)] = &[
        ("git -C S:/Code/me-sim/me-sim pull --ff-only", None),
        (
            "cd S:/Code/me-sim/me-sim && git pull --ff-only origin master",
            None,
        ),
        (
            "git -C S:/Code/me-sim/me-sim pull",
            Some(RuleId::WorkerGitOutside),
        ),
        (
            "git -C S:/Code/me-sim/me-sim merge --ff-only sand",
            Some(RuleId::WorkerGitOutside),
        ),
        (
            "git -C S:/Code/me-sim/me-sim reset --hard",
            Some(RuleId::WorkerGitOutside),
        ),
        (
            "git -C S:/Code/me-studio/me-studio pull --ff-only",
            Some(RuleId::WorkerGitOutside),
        ),
        ("git -C S:/Code/me-sim/me-sim fetch", None),
    ];
    for (command, want) in table {
        assert_eq!(
            rule(&check_tool(&scope, &bash(command))),
            *want,
            "{command}"
        );
    }
}

#[test]
fn only_a_project_that_installs_lets_its_workers_install() {
    let mut scope = worker("sand");
    let install = bash("headwork install");
    assert_eq!(
        rule(&check_tool(&scope, &install)),
        Some(RuleId::WorkerAdmin),
        "not allowed"
    );
    scope.may_install = true;
    assert_eq!(rule(&check_tool(&scope, &install)), None, "allowed");
    assert_eq!(
        rule(&check_tool(
            &scope,
            &bash("headwork spawn me-sim x brief.md")
        )),
        Some(RuleId::WorkerAdmin),
        "install only"
    );
}

#[test]
fn a_project_that_installs_lets_its_workers_run_a_plain_update_only() {
    let table: &[(&str, Option<RuleId>, Option<RuleId>)] = &[
        ("headwork update", Some(RuleId::WorkerAdmin), None),
        ("headwork update --dry-run", Some(RuleId::WorkerAdmin), None),
        ("headwork update --json", Some(RuleId::WorkerAdmin), None),
        ("headwork update --wait 30", Some(RuleId::WorkerAdmin), None),
        (
            "headwork update --wait=30 --json",
            Some(RuleId::WorkerAdmin),
            None,
        ),
        (
            "headwork.exe update --dry-run --wait 5 --json",
            Some(RuleId::WorkerAdmin),
            None,
        ),
        (
            "cargo run -p headwork -- update --dry-run",
            Some(RuleId::WorkerAdmin),
            None,
        ),
        ("headwork update --help", None, None),
        (
            "headwork update --to v0.1.7",
            Some(RuleId::WorkerAdmin),
            Some(RuleId::WorkerAdmin),
        ),
        (
            "headwork update --to=v0.1.7",
            Some(RuleId::WorkerAdmin),
            Some(RuleId::WorkerAdmin),
        ),
        (
            "headwork update --dry-run --to v0.1.7",
            Some(RuleId::WorkerAdmin),
            Some(RuleId::WorkerAdmin),
        ),
        (
            "headwork update --wait",
            Some(RuleId::WorkerAdmin),
            Some(RuleId::WorkerAdmin),
        ),
        (
            "headwork update --wait --to v0.1.7",
            Some(RuleId::WorkerAdmin),
            Some(RuleId::WorkerAdmin),
        ),
        (
            "headwork update --dir /tmp/bin",
            Some(RuleId::WorkerAdmin),
            Some(RuleId::WorkerAdmin),
        ),
        (
            "headwork update --after x",
            Some(RuleId::WorkerAdmin),
            Some(RuleId::WorkerAdmin),
        ),
        (
            "headwork update v0.1.7",
            Some(RuleId::WorkerAdmin),
            Some(RuleId::WorkerAdmin),
        ),
    ];
    let mut scope = worker("sand");
    for (command, without, with) in table {
        scope.may_install = false;
        assert_eq!(
            rule(&check_tool(&scope, &bash(command))),
            *without,
            "{command} without install"
        );
        scope.may_install = true;
        assert_eq!(
            rule(&check_tool(&scope, &bash(command))),
            *with,
            "{command} with install"
        );
    }
}

#[test]
fn the_orchestrator_commits_in_its_notes_repository_but_not_in_projects_inside_it() {
    let mut scope = orchestrator();
    scope.cwd = "S:/Code".to_owned();
    let commit =
        bash("git add briefs/x.md && git commit -q -m \"docs(briefs): add x\" && git push -q");
    assert_eq!(
        rule(&check_tool(&scope, &commit)),
        Some(RuleId::OrchestratorGit),
        "no repos configured"
    );
    scope.orchestrator_repos = vec![GuardPath::new("S:/Code")];
    scope.orchestrator_repo_excludes = vec![GuardPath::new("S:/Code/me-sim")];
    let table: &[(&str, Option<RuleId>)] = &[
        (
            "git add briefs/x.md && git commit -q -m \"docs(briefs): add x\" && git push -q",
            None,
        ),
        ("git -C /s/Code commit -m x", None),
        (
            "cd S:/Code/me-sim/me-sim && git commit -m x",
            Some(RuleId::OrchestratorGit),
        ),
        (
            "git -C S:/Code/me-sim/me-sim-sand push",
            Some(RuleId::OrchestratorGit),
        ),
        (
            "cd S:/Elsewhere && git commit -m x",
            Some(RuleId::OrchestratorGit),
        ),
    ];
    for (command, want) in table {
        assert_eq!(
            rule(&check_tool(&scope, &bash(command))),
            *want,
            "{command}"
        );
    }
}

fn rule(decision: &Decision) -> Option<RuleId> {
    decision.verdict().map(|verdict| verdict.rule)
}

const fn bash(command: &str) -> ToolCall<'_> {
    ToolCall::Bash {
        command,
        background: false,
    }
}

#[test]
fn orchestrator_bash_table() {
    let table: &[(&str, Option<RuleId>)] = &[
        ("git status", None),
        ("git log --oneline -5", None),
        ("gh pr view 12 --json state", None),
        ("gh pr list", None),
        ("headwork board", None),
        ("wmux read-screen --surface s", None),
        ("git commit -m x", Some(RuleId::OrchestratorGit)),
        (
            "git -C S:/Code/me-sim/me-sim push",
            Some(RuleId::OrchestratorGit),
        ),
        (
            "cd me-sim && git merge feature",
            Some(RuleId::OrchestratorGit),
        ),
        ("gh pr merge 12 --squash", Some(RuleId::OrchestratorGit)),
        ("gh pr create --fill", Some(RuleId::OrchestratorGit)),
        ("cargo build --release", Some(RuleId::OrchestratorBuild)),
        (
            "rtk bypass mvnd -B test-compile",
            Some(RuleId::OrchestratorBuild),
        ),
        ("bash -c 'npm install'", Some(RuleId::OrchestratorBuild)),
        (
            "scripts/ship.sh -m 'feat: x'",
            Some(RuleId::OrchestratorDelivery),
        ),
        (
            "bash S:/Code/me-sim/me-sim/scripts/worktree.sh new x",
            Some(RuleId::OrchestratorDelivery),
        ),
        ("./scripts/publish-now.sh", Some(RuleId::UserOnly)),
        ("echo 'git commit' > notes.txt", None),
    ];
    let scope = orchestrator();
    for (command, expected) in table {
        assert_eq!(
            rule(&check_tool(&scope, &bash(command))),
            *expected,
            "{command}"
        );
    }
}

#[test]
fn worker_bash_table() {
    let table: &[(&str, &str, bool, Option<RuleId>)] = &[
        ("git commit -m x", "sand", false, None),
        (
            "git commit -m x",
            "master",
            false,
            Some(RuleId::WorkerDefaultBranch),
        ),
        ("git push", "sand", false, None),
        ("git push -u origin sand", "sand", false, None),
        ("git push", "main", false, Some(RuleId::WorkerDefaultBranch)),
        (
            "git push origin HEAD:master",
            "sand",
            false,
            Some(RuleId::WorkerDefaultBranch),
        ),
        (
            "git push origin sand:refs/heads/main",
            "sand",
            false,
            Some(RuleId::WorkerDefaultBranch),
        ),
        (
            "git push --force origin +main",
            "sand",
            false,
            Some(RuleId::WorkerDefaultBranch),
        ),
        (
            "git push fork sand",
            "sand",
            false,
            Some(RuleId::WorkerPushRepo),
        ),
        (
            "git push git@github.com:other/repo.git sand",
            "sand",
            false,
            Some(RuleId::WorkerPushRepo),
        ),
        ("git push unknown-remote sand", "sand", false, None),
        (
            "scripts/publish-now.sh --all",
            "sand",
            false,
            Some(RuleId::UserOnly),
        ),
        (
            "scripts/rl-run.sh car",
            "sand",
            true,
            Some(RuleId::WorkerLongJob),
        ),
        ("scripts/rl-run.sh car", "sand", false, None),
        (
            "cargo run --release -p render -- sand",
            "sand",
            true,
            Some(RuleId::WorkerLongJob),
        ),
        ("cargo test", "sand", true, None),
        ("scripts/ship.sh -m 'feat: x'", "sand", false, None),
        ("cargo build", "master", false, None),
    ];
    for (command, branch, background, expected) in table {
        let call = ToolCall::Bash {
            command,
            background: *background,
        };
        assert_eq!(
            rule(&check_tool(&worker(branch), &call)),
            *expected,
            "{command} on {branch}"
        );
    }
}

#[test]
fn worker_admin_table() {
    let table: &[(&str, Option<RuleId>)] = &[
        ("headwork board", None),
        ("headwork log me-sim-sand", None),
        ("headwork doctor", None),
        ("headwork jobs", None),
        ("headwork recap me-sim-sand", None),
        ("headwork shadow-report", None),
        ("headwork rule test 'git push'", None),
        ("headwork --version", None),
        ("headwork spawn --help", None),
        ("headwork close me-sim-sand", None),
        ("headwork close me-sim/sand --json", None),
        ("headwork close me-sim-other", Some(RuleId::WorkerAdmin)),
        (
            "headwork spawn me-sim x brief.md",
            Some(RuleId::WorkerAdmin),
        ),
        ("headwork queue", Some(RuleId::WorkerAdmin)),
        ("headwork park", Some(RuleId::WorkerAdmin)),
        ("headwork resume", Some(RuleId::WorkerAdmin)),
        ("headwork import", Some(RuleId::WorkerAdmin)),
        ("headwork tick", Some(RuleId::WorkerAdmin)),
        ("headwork tick --install", Some(RuleId::WorkerAdmin)),
        ("headwork reap", Some(RuleId::WorkerAdmin)),
        ("headwork reap --json", Some(RuleId::WorkerAdmin)),
        ("headwork reap --dry-run", None),
        ("headwork.exe reap --dry-run --json", None),
        ("headwork root", Some(RuleId::WorkerAdmin)),
        ("headwork.exe queue --order a,b", Some(RuleId::WorkerAdmin)),
        (
            "C:/Users/User/.local/bin/headwork.exe park",
            Some(RuleId::WorkerAdmin),
        ),
        (
            "\"C:/Users/User/.local/bin/headwork.exe\" close me-sim-other",
            Some(RuleId::WorkerAdmin),
        ),
        (
            "\"C:/Users/User/.local/bin/headwork.exe\" close me-sim-sand",
            None,
        ),
        (
            "cargo run -p headwork -- spawn me-sim x brief.md",
            Some(RuleId::WorkerAdmin),
        ),
        (
            "cargo run --release --package headwork -- install",
            Some(RuleId::WorkerAdmin),
        ),
        ("cargo run -p headwork -- board", None),
        ("cargo run -p render -- spawn", None),
        ("echo headwork spawn", None),
    ];
    let scope = worker("sand");
    for (command, expected) in table {
        assert_eq!(
            rule(&check_tool(&scope, &bash(command))),
            *expected,
            "{command}"
        );
    }
}

#[test]
fn worker_git_outside_table() {
    let table: &[(&str, Option<RuleId>)] = &[
        (
            "git -C ../other commit -m x",
            Some(RuleId::WorkerGitOutside),
        ),
        ("git -C . commit -m x", None),
        ("git -C src -C .. commit -m x", None),
        (
            "git -C src -C ../.. commit -m x",
            Some(RuleId::WorkerGitOutside),
        ),
        ("cd /elsewhere && git push", Some(RuleId::WorkerGitOutside)),
        (
            "cd S:/Code/me-sim/me-sim; git reset --hard",
            Some(RuleId::WorkerGitOutside),
        ),
        ("cd sub && git commit -m x", None),
        (
            "cd C:/Users/User/AppData/Local/Temp/scratch && git init",
            None,
        ),
        ("git -C /elsewhere status", None),
        ("git -C /elsewhere log --oneline", None),
        ("git -C /elsewhere fetch", None),
        ("git -C /elsewhere branch", None),
        ("git -C /elsewhere branch --list 'f*'", None),
        ("git -C /elsewhere worktree list", None),
        ("git -C /elsewhere stash list", None),
        (
            "git -C /elsewhere branch -D old",
            Some(RuleId::WorkerGitOutside),
        ),
        (
            "git -C /elsewhere worktree remove x",
            Some(RuleId::WorkerGitOutside),
        ),
        (
            "git -C /elsewhere tag -d v1",
            Some(RuleId::WorkerGitOutside),
        ),
        ("git -C /elsewhere stash", Some(RuleId::WorkerGitOutside)),
        ("cd - && git commit -m x", None),
        (
            "cd /elsewhere && cd S:/Code/me-sim/me-sim-sand && git commit -m x",
            None,
        ),
    ];
    let scope = worker("sand");
    for (command, expected) in table {
        assert_eq!(
            rule(&check_tool(&scope, &bash(command))),
            *expected,
            "{command}"
        );
    }
}

#[test]
fn home_and_temp_spellings_resolve() {
    let mut scope = worker("sand");
    scope.cwd = "C:/Users/User/.claude-monitor".to_owned();
    scope.writable = vec![GuardPath::new(&scope.cwd), GuardPath::new(TEMP)];
    let table: &[(&str, Option<RuleId>)] = &[
        ("cd ~/.claude-monitor && git commit -m x", None),
        ("cd $HOME/.claude-monitor && git push -u origin x", None),
        ("git -C ~/.claude-monitor commit -m x", None),
        ("cd /tmp/scratch && git init -q", None),
        (
            "cd ~/.claude && git pull --ff-only",
            Some(RuleId::WorkerGitOutside),
        ),
        ("cd && git pull", Some(RuleId::WorkerGitOutside)),
    ];
    for (command, expected) in table {
        assert_eq!(
            rule(&check_tool(&scope, &bash(command))),
            *expected,
            "{command}"
        );
    }
    let writes: &[(&str, Option<RuleId>)] = &[
        ("~/.claude-monitor/SKILL.md", None),
        ("/tmp/todo.patch", None),
        ("~/.claude/AGENTS.md", Some(RuleId::WorkerEditOutside)),
    ];
    for (path, expected) in writes {
        let call = ToolCall::Write {
            tool: "Write",
            path,
        };
        assert_eq!(rule(&check_tool(&scope, &call)), *expected, "{path}");
    }
}

#[test]
fn user_only_matches_a_program_by_its_base_name() {
    let mut scope = worker("sand");
    scope.user_only = vec![
        "me-studio publish".to_owned(),
        "scripts/studio-publish.sh".to_owned(),
    ];
    let table: &[(&str, Option<RuleId>)] = &[
        (
            "/s/Code/me-studio/me-studio/target/release/me-studio.exe publish --project me-sim embed",
            Some(RuleId::UserOnly),
        ),
        (
            "'S:\\Code\\me-studio\\me-studio\\target\\release\\ME-STUDIO.EXE' publish x",
            Some(RuleId::UserOnly),
        ),
        ("me-studio publish plan", Some(RuleId::UserOnly)),
        (
            "bash S:/Code/me-studio/me-studio/scripts/studio-publish.sh me-sim upload",
            Some(RuleId::UserOnly),
        ),
        (
            "/s/Code/me-studio/me-studio/target/release/me-studio.exe analyze --json",
            None,
        ),
        ("my-me-studio publish", None),
        ("cat me-studio publish", None),
    ];
    for (command, expected) in table {
        assert_eq!(
            rule(&check_tool(&scope, &bash(command))),
            *expected,
            "{command}"
        );
    }
}

#[test]
fn reading_a_script_is_not_running_it() {
    let table: &[(Role, &str, Option<RuleId>)] = &[
        (Role::Orchestrator, "cat scripts/publish-now.sh", None),
        (Role::Orchestrator, "wc -l scripts/publish-now.sh", None),
        (
            Role::Orchestrator,
            "grep -n upload ./scripts/publish-now.sh",
            None,
        ),
        (
            Role::Orchestrator,
            "sed -n 1,60p S:/Code/me-sim/me-sim/scripts/worktree.sh | grep new",
            None,
        ),
        (
            Role::Orchestrator,
            "grep -n branch scripts/worktree.sh",
            None,
        ),
        (
            Role::Orchestrator,
            "cat <<'EOF' > plan.md\ncargo build --release\nEOF",
            None,
        ),
        (Role::Worker, "cat scripts/publish-now.sh", None),
        (
            Role::Worker,
            "rtk grep -c publish scripts/publish-now.sh",
            None,
        ),
        (
            Role::Worker,
            "bash scripts/publish-now.sh --all",
            Some(RuleId::UserOnly),
        ),
    ];
    for (role, command, expected) in table {
        let scope = match role {
            Role::Orchestrator => orchestrator(),
            Role::Worker => worker("sand"),
        };
        assert_eq!(
            rule(&check_tool(&scope, &bash(command))),
            *expected,
            "{command}"
        );
    }
}

#[test]
fn write_table() {
    let table: &[(Role, &str, Option<RuleId>)] = &[
        (
            Role::Orchestrator,
            "C:/Users/User/.headwork/briefs/x.md",
            None,
        ),
        (
            Role::Orchestrator,
            "C:\\Users\\User\\AppData\\Local\\Temp\\claude\\pad\\a.txt",
            None,
        ),
        (
            Role::Orchestrator,
            "S:/Code/me-sim/me-sim/src/lib.rs",
            Some(RuleId::OrchestratorEdit),
        ),
        (Role::Worker, "S:/Code/me-sim/me-sim-sand/src/lib.rs", None),
        (Role::Worker, "src/lib.rs", None),
        (
            Role::Worker,
            "/s/Code/me-sim/me-sim-sand/../me-sim-sand/a.rs",
            None,
        ),
        (
            Role::Worker,
            "S:/Code/me-sim/me-sim/src/lib.rs",
            Some(RuleId::WorkerEditOutside),
        ),
        (
            Role::Worker,
            "../me-sim/src/lib.rs",
            Some(RuleId::WorkerEditOutside),
        ),
        (
            Role::Worker,
            "S:/Code/me-studio/me-studio/a.rs",
            Some(RuleId::WorkerReadOnly),
        ),
    ];
    for (role, path, expected) in table {
        let scope = match role {
            Role::Orchestrator => orchestrator(),
            Role::Worker => worker("sand"),
        };
        let call = ToolCall::Write { tool: "Edit", path };
        assert_eq!(rule(&check_tool(&scope, &call)), *expected, "{role} {path}");
    }
}

#[test]
fn skills_and_agents() {
    let scope = worker("sand");
    let deny = |call: ToolCall<'_>| rule(&check_tool(&scope, &call));
    assert_eq!(
        deny(ToolCall::Skill { name: "ship" }),
        Some(RuleId::WorkerGlobalSkill),
        "ship"
    );
    assert_eq!(
        deny(ToolCall::Skill {
            name: "user:contribute"
        }),
        Some(RuleId::WorkerGlobalSkill),
        "qualified"
    );
    assert_eq!(
        deny(ToolCall::Skill {
            name: "video-analytics"
        }),
        None,
        "other skill"
    );
    let pr_project = Scope {
        own_delivery: false,
        ..worker("sand")
    };
    assert_eq!(
        rule(&check_tool(&pr_project, &ToolCall::Skill { name: "ship" })),
        None,
        "pr project"
    );
    let fable = ToolCall::Agent {
        tool: "Agent",
        model: Some("fable"),
    };
    assert_eq!(deny(fable), Some(RuleId::WorkerFable), "fable");
    let allowed = Scope {
        fable_allowed: true,
        ..worker("sand")
    };
    assert_eq!(rule(&check_tool(&allowed, &fable)), None, "approved fable");
    assert_eq!(
        rule(&check_tool(&orchestrator(), &fable)),
        None,
        "the orchestrator may"
    );
    assert_eq!(
        deny(ToolCall::Agent {
            tool: "Agent",
            model: Some("opus")
        }),
        None,
        "opus"
    );
}

#[test]
fn unmanaged_sessions_are_always_allowed() {
    let scope = Scope::default();
    assert_eq!(
        check_tool(&scope, &bash("git push origin master")),
        Decision::Allow,
        "no role"
    );
}

#[test]
fn repos_from_urls() {
    let table = [
        ("git@github.com:Ivan091/me-sim.git", Some("Ivan091/me-sim")),
        (
            "ssh://git@github.com/Ivan091/me-sim",
            Some("Ivan091/me-sim"),
        ),
        (
            "https://github.com/Ivan091/me-sim.git",
            Some("Ivan091/me-sim"),
        ),
        ("origin", None),
        ("S:/Code/x", None),
    ];
    for (url, expected) in table {
        assert_eq!(repo_from_url(url).as_deref(), expected, "{url}");
    }
}

fn ask(options: &[&str], header: &str, facts: AskFacts) -> Option<RuleId> {
    let questions = [Question {
        header,
        options: options.to_vec(),
    }];
    rule(&check_ask(&questions, facts))
}

#[test]
fn ask_table() {
    let met = AskFacts {
        push: Precondition::Met,
        review_file: Precondition::Met,
    };
    let table: &[(&[&str], &str, AskFacts, Option<RuleId>)] = &[
        (&["Ship it (Recommended)", "Drop it"], "Ship", met, None),
        (&["Ship it"], "Ship", met, Some(RuleId::AskOptionCount)),
        (&["a", "b", "c", "d", "e"], "Ship", met, None),
        (
            &["Ship it (Recommended)", "Needs changes"],
            "Ship",
            met,
            Some(RuleId::AskChangesOption),
        ),
        (
            &["Ship it", "Other"],
            "Ship",
            met,
            Some(RuleId::AskChangesOption),
        ),
        (
            &["Drop it", "Ship it (Recommended)"],
            "Ship",
            met,
            Some(RuleId::AskRecommendedFirst),
        ),
        (
            &["A (Recommended)", "B (Recommended)"],
            "Ship",
            met,
            Some(RuleId::AskRecommendedFirst),
        ),
        (
            &["Ship it (Recommended)", "Drop it"],
            "Ship",
            AskFacts {
                push: Precondition::Missing,
                ..met
            },
            Some(RuleId::AskPush),
        ),
        (
            &["Good (Recommended)", "Drop it"],
            "Review",
            AskFacts {
                review_file: Precondition::Missing,
                ..met
            },
            Some(RuleId::AskReviewFile),
        ),
        (
            &["Good (Recommended)", "Drop it"],
            "Ship",
            AskFacts {
                review_file: Precondition::Missing,
                ..met
            },
            None,
        ),
        (&["A", "B"], "x", AskFacts::default(), None),
    ];
    for (options, header, facts, expected) in table {
        assert_eq!(
            ask(options, header, *facts),
            *expected,
            "{options:?} {header} {facts:?}"
        );
    }
}

fn stop(facts: StopFacts<'_>) -> Option<RuleId> {
    rule(&check_stop(&facts))
}

#[test]
#[expect(clippy::too_many_lines, reason = "one table of every stop case")]
fn stop_table() {
    let base = StopFacts {
        role: Role::Worker,
        task: Some(TaskState::Working),
        blocked_in_row: 0,
        asked: false,
        messaged: false,
        reported: false,
        keep: false,
        running_job: false,
        waiting_background: false,
        waiting_part: false,
        last_message: Some("Done with step 3."),
    };
    let table: &[(StopFacts<'_>, Option<RuleId>, &str)] = &[
        (
            base,
            Some(RuleId::StopUnfinished),
            "silent stop while working",
        ),
        (
            StopFacts {
                asked: true,
                last_message: Some("Answered; are we done?"),
                ..base
            },
            Some(RuleId::StopPlainQuestion),
            "a plain question after an ask in the same turn",
        ),
        (
            StopFacts {
                asked: true,
                ..base
            },
            None,
            "asked",
        ),
        (
            StopFacts {
                messaged: true,
                ..base
            },
            None,
            "reported",
        ),
        (
            StopFacts {
                reported: true,
                ..base
            },
            None,
            "reported done or blocked",
        ),
        (StopFacts { keep: true, ..base }, None, "a keep task"),
        (
            StopFacts {
                task: Some(TaskState::InReview),
                ..base
            },
            None,
            "in review",
        ),
        (
            StopFacts {
                messaged: true,
                last_message: Some("Shall I merge it?"),
                ..base
            },
            Some(RuleId::StopPlainQuestion),
            "a plain question",
        ),
        (
            StopFacts {
                role: Role::Orchestrator,
                task: None,
                last_message: Some("Want me to spawn it?**"),
                ..base
            },
            Some(RuleId::StopPlainQuestion),
            "the orchestrator asks in text",
        ),
        (
            StopFacts {
                role: Role::Orchestrator,
                task: None,
                ..base
            },
            None,
            "the orchestrator ends a turn",
        ),
        (
            StopFacts {
                blocked_in_row: MAX_BLOCKS,
                ..base
            },
            None,
            "the block limit lets it stop",
        ),
    ];
    for (facts, expected, what) in table {
        assert_eq!(stop(*facts), *expected, "{what}");
    }
}

#[test]
fn stop_while_waiting_on_background_work() {
    let waiting = StopFacts {
        role: Role::Worker,
        task: Some(TaskState::Working),
        blocked_in_row: 0,
        asked: false,
        messaged: false,
        reported: false,
        keep: false,
        running_job: false,
        waiting_background: true,
        waiting_part: false,
        last_message: Some("The build runs in the background."),
    };
    assert_eq!(stop(waiting), None, "waiting on background work");
    assert_eq!(
        stop(StopFacts {
            last_message: Some("Waiting for the build, ok?"),
            ..waiting
        }),
        Some(RuleId::StopPlainQuestion),
        "background work does not excuse a plain question"
    );
    assert_eq!(
        stop(StopFacts {
            waiting_background: false,
            waiting_part: false,
            ..waiting
        }),
        Some(RuleId::StopUnfinished),
        "without it the stop is unfinished"
    );
}

#[test]
fn stop_while_a_job_runs() {
    let waiting = StopFacts {
        role: Role::Worker,
        task: Some(TaskState::Working),
        blocked_in_row: 0,
        asked: false,
        messaged: false,
        reported: false,
        keep: false,
        running_job: true,
        waiting_background: false,
        waiting_part: false,
        last_message: Some("Training runs; I will look at the next checkpoint."),
    };
    assert_eq!(stop(waiting), None, "waiting on its own running job");
    assert_eq!(
        stop(StopFacts {
            last_message: Some("Training runs; check back later?"),
            ..waiting
        }),
        Some(RuleId::StopPlainQuestion),
        "a running job does not excuse a plain question"
    );
}

const NOTES: &str = "S:/Code/notes";

fn car_training() -> Scope {
    Scope {
        cwd: MAIN.to_owned(),
        writable: vec![GuardPath::new(MAIN), GuardPath::new(TEMP)],
        branch: Some("master".to_owned()),
        extra_writable: vec![
            GuardPath::new("S:/Code/notes/handoff-car-training.md"),
            GuardPath::new("S:/Code/notes/night-results"),
        ],
        ..worker("master")
    }
}

#[test]
fn a_worker_writes_its_extra_paths_and_nothing_beside_them() {
    let scope = car_training();
    let table: &[(&str, Option<RuleId>)] = &[
        (r"S:\Code\notes\handoff-car-training.md", None),
        ("/s/Code/notes/night-results/2026-11-02.md", None),
        ("S:/Code/notes/night-results/sub/x.md", None),
        (
            "S:/Code/notes/approval-friction-plan.md",
            Some(RuleId::WorkerEditOutside),
        ),
        (
            "S:/Code/notes/handoff-car-training.md.bak",
            Some(RuleId::WorkerEditOutside),
        ),
        ("S:/Code/todo.md", Some(RuleId::WorkerEditOutside)),
        ("S:/Code/briefs/x.md", Some(RuleId::WorkerEditOutside)),
        ("S:/Code/CLAUDE.md", Some(RuleId::WorkerEditOutside)),
    ];
    for (path, want) in table {
        let call = ToolCall::Write { tool: "Edit", path };
        assert_eq!(rule(&check_tool(&scope, &call)), *want, "{path}");
    }
    let without = worker("master");
    let call = ToolCall::Write {
        tool: "Write",
        path: "S:/Code/notes/handoff-car-training.md",
    };
    assert_eq!(
        rule(&check_tool(&without, &call)),
        Some(RuleId::WorkerEditOutside),
        "no grant, no write"
    );
}

#[test]
fn a_worker_commits_its_extra_paths_in_their_notes_repository() {
    let scope = car_training();
    let allowed = [
        "cd S:/Code/notes && git add handoff-car-training.md night-results/2026-11-02.md && git commit -q -m \"docs: v5n\" && git push -q",
        "cd /s/Code && git add notes/handoff-car-training.md notes/night-results/2026-11-02.md && git commit -q -m x && git push -q",
        "cd /s/Code && git add notes/night-results && git commit -qm \"docs: x\" && git push -q origin master",
        "git -C S:/Code add -- notes/night-results/a.md",
        "git -C S:/Code commit -m x -- notes/handoff-car-training.md",
        "git -C S:/Code/notes pull --rebase -q",
        "cd S:/Code/notes/night-results && git add . && git commit -m x",
        "git -C S:/Code rm notes/night-results/old.md",
    ];
    for command in allowed {
        assert_eq!(rule(&check_tool(&scope, &bash(command))), None, "{command}");
    }
    let refused = [
        "cd /s/Code && git add notes",
        "cd /s/Code && git add -A",
        "cd /s/Code && git add .",
        "cd /s/Code && git add todo.md",
        "cd S:/Code/notes && git add -u",
        "git -C S:/Code add :/",
        "git -C S:/Code add --pathspec-from-file=list.txt",
        "git -C S:/Code commit -am x",
        "git -C S:/Code commit -a -m x",
        "git -C S:/Code commit --all -m x",
        "git -C S:/Code commit --amend --no-edit",
        "git -C S:/Code commit -m x todo.md",
        "git -C S:/Code commit -m x -- briefs/a.md",
        "git -C S:/Code push --force",
        "git -C S:/Code push -f origin master",
        "git -C S:/Code push origin +master",
        "git -C S:/Code push origin :old",
        "git -C S:/Code push --delete origin old",
        "git -C S:/Code reset --hard",
        "git -C S:/Code checkout -- todo.md",
        "git -C S:/Code merge origin/master",
        "git -C S:/Code stash",
        "git -C S:/Code/me-studio/me-studio commit -m x",
    ];
    for command in refused {
        assert_eq!(
            rule(&check_tool(&scope, &bash(command))),
            Some(RuleId::WorkerGitOutside),
            "{command}"
        );
    }
}

#[test]
fn granted_notes_git_skips_the_default_branch_and_push_repository_rules() {
    let scope = car_training();
    assert_eq!(
        rule(&check_tool(
            &scope,
            &bash("cd S:/Code/notes && git commit -m x && git push")
        )),
        None,
        "code-home is committed on master by design"
    );
    assert_eq!(
        rule(&check_tool(&scope, &bash("git commit -m x"))),
        Some(RuleId::WorkerDefaultBranch),
        "its own checkout on master stays refused"
    );
    assert_eq!(
        rule(&check_tool(&scope, &bash("git push fork x"))),
        Some(RuleId::WorkerPushRepo),
        "its own checkout keeps the repository rule"
    );
}

#[test]
fn a_refusal_in_a_granted_repository_says_why() {
    let scope = car_training();
    let decision = check_tool(&scope, &bash("cd S:/Code && git add todo.md"));
    let reason = decision
        .verdict()
        .map(|verdict| verdict.reason.clone())
        .unwrap_or_default();
    assert!(
        reason.contains("s:/code/todo.md is not one of the paths"),
        "{reason}"
    );
}

#[test]
fn the_orchestrator_gains_nothing_from_worker_grants() {
    let scope = Scope {
        extra_writable: vec![GuardPath::new(NOTES)],
        ..orchestrator()
    };
    let call = ToolCall::Write {
        tool: "Edit",
        path: "S:/Code/notes/a.md",
    };
    assert_eq!(
        rule(&check_tool(&scope, &call)),
        Some(RuleId::OrchestratorEdit),
        "only orchestrator_writable counts"
    );
}

#[test]
fn a_lead_waiting_on_a_live_part_may_stop() {
    let waiting = StopFacts {
        role: Role::Worker,
        task: Some(TaskState::Working),
        blocked_in_row: 0,
        asked: false,
        messaged: false,
        reported: false,
        keep: false,
        running_job: false,
        waiting_background: false,
        waiting_part: true,
        last_message: Some("The review part runs; I wait for its report."),
    };
    assert_eq!(stop(waiting), None, "a lead waits for its part");
    assert_eq!(
        stop(StopFacts {
            last_message: Some("Shall I wait for the part?"),
            ..waiting
        }),
        Some(RuleId::StopPlainQuestion),
        "a live part does not excuse a plain question"
    );
    assert_eq!(
        stop(StopFacts {
            waiting_part: false,
            ..waiting
        }),
        Some(RuleId::StopUnfinished),
        "without a live part the stop is unfinished"
    );
}
