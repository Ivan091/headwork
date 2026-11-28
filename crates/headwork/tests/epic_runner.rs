//! The epic runner (`docs/backlog.md`, section 13) against a temporary home, the fake host and an in-process store: a
//! pump starts an epic's ready children up to `[epics] max_parallel`, a later pump starts what their end freed and never
//! starts a child twice, a lost child brings one re-plan explorer, and too many re-plans block the epic for a decision.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;
use std::process::Command;

use headwork::home::Home;
use headwork::ops::Env;
use headwork::ops::mcp::{Identity, Server};
use headwork::ops::spawn::{Outcome, Pumped, pump};
use headwork_adapters::git::FakeGit;
use headwork_adapters::host::fake::FakeHost;
use headwork_core::backlog::{
    BacklogActor, BacklogDraft, BacklogOwner, BacklogStatus, BlockReason, ItemKind, Target,
};
use headwork_core::lifecycle::{MergePolicy, MergeSource, SessionFacts, Trigger};
use headwork_core::{Actor, Config, EXPLORE_KIND, Role, TaskState};
use headwork_store::{BacklogMove, ItemPlan, ItemShape, Store, Task, TaskId};

const EXE: &str = "S:/bin/headwork.exe";

struct World {
    _dir: tempfile::TempDir,
    home: Home,
    config: Config,
    store: Store,
    host: FakeHost,
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "git {args:?}");
}

fn world() -> World {
    world_with("max_parallel = 2\nmax_replans = 1\n", "")
}

fn world_with(epics: &str, extra: &str) -> World {
    let dir = tempfile::tempdir().unwrap();
    let dir_text = dir.path().display().to_string().replace('\\', "/");
    let main = dir.path().join("demo");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "master"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let main_text = main.display().to_string().replace('\\', "/");
    let text = format!(
        "[machine]\nhost = \"wmux\"\n\n[hooks]\nbrief_writable = [\"{dir_text}/notes\"]\n\n\
         [epics]\n{epics}\n[project.demo]\nmain = \"{main_text}\"\n\
         worktree_new = \"builtin\"\nworktree_remove = \"builtin\"\nworktree_path = \"{{main}}-{{slug}}\"\n\
         deliver = \"pr\"\nmerge = \"deliver\"\nallowed_repos = [\"acme/demo\"]\n\n{extra}"
    );
    let config = Config::parse(&text, "test").unwrap();
    let home = Home::at(Some(dir.path().join("home")))
        .unwrap()
        .with_claude_json(dir.path().join(".claude.json"))
        .with_claude_dir(dir.path().join(".claude"));
    let mut store = home.open_store().unwrap();
    store.sync_projects(&config).unwrap();
    World {
        _dir: dir,
        home,
        config,
        store,
        host: FakeHost::new(),
    }
}

fn draft(text: &str) -> BacklogDraft {
    BacklogDraft {
        project: "demo".to_owned(),
        section: None,
        text: text.to_owned(),
        notes: Vec::new(),
        status: BacklogStatus::Backlog,
        owner: BacklogOwner::Agent,
        added: None,
        not_before: None,
        due: None,
        done_on: None,
        done_note: None,
    }
}

impl World {
    fn pump(&mut self) -> Vec<Pumped> {
        let env = Env {
            home: &self.home,
            config: &self.config,
            host: &self.host,
            exe: Path::new(EXE),
            actor: Actor::Tick,
        };
        pump(&mut self.store, &env).unwrap()
    }

    fn add(&mut self, text: &str, shape: &ItemShape) -> i64 {
        self.store
            .add_backlog_item_shaped(
                &draft(text),
                Actor::Orchestrator,
                BacklogActor::Orchestrator,
                None,
                shape,
            )
            .unwrap()
            .id
    }

    fn child(&mut self, epic: i64, text: &str, after: &[i64]) -> i64 {
        self.add(
            text,
            &ItemShape {
                parent: Some(epic),
                after: after.to_vec(),
                plan: ItemPlan {
                    brief: Some(format!("Goal: {text}\n\nDone when tests pass.")),
                    task_kind: Some("code".to_owned()),
                    ..ItemPlan::default()
                },
                ..ItemShape::default()
            },
        )
    }

    fn status(&self, id: i64) -> BacklogStatus {
        self.store.backlog_item(id).unwrap().draft.status
    }

    fn task_of(&self, id: i64) -> Option<Task> {
        let linked = self.store.backlog_item(id).unwrap().task?;
        Some(self.store.task(TaskId(linked.id)).unwrap())
    }

    fn merge(&mut self, task: &Task) {
        for trigger in [
            Trigger::SessionStarted(SessionFacts {
                session_matches: true,
                launcher_alive: true,
            }),
            Trigger::ReviewOpened { repo_allowed: true },
            Trigger::Merged {
                source: MergeSource::GitHost,
                policy: MergePolicy::Deliver,
            },
        ] {
            self.store
                .transition(task.id, &trigger, Actor::Tick)
                .unwrap();
        }
    }

    fn drop_child_task(&mut self, id: i64) {
        let task = self.task_of(id).unwrap();
        self.store.drop_task(task.id, Actor::Orchestrator).unwrap();
        assert_eq!(
            self.status(id).block().map(|block| block.reason),
            Some(BlockReason::TaskLost),
            "#{id}"
        );
    }

    fn worker_tool(
        &mut self,
        task: &Task,
        name: &str,
        arguments: &serde_json::Value,
    ) -> (bool, String) {
        let session = self.store.session_of(task.id).unwrap().unwrap().id;
        let env = Env {
            home: &self.home,
            config: &self.config,
            host: &self.host,
            exe: Path::new(EXE),
            actor: Actor::Worker,
        };
        let identity = Identity {
            session: Some(session),
            role: Some(Role::Worker),
        };
        let message = serde_json::json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        let prs = FakeGit::new();
        let reply = Server::new(&mut self.store, env, &prs, identity)
            .handle(&message)
            .unwrap();
        let result = reply["result"].clone();
        let text = result["content"][0]["text"].as_str().unwrap().to_owned();
        (result["isError"].as_bool().unwrap(), text)
    }

    fn epic(&mut self, text: &str) -> i64 {
        let epic = self.add(
            text,
            &ItemShape {
                kind: Some(ItemKind::Epic),
                ..ItemShape::default()
            },
        );
        let start = BacklogMove::new(Target::InProgress, BacklogActor::User, Actor::User, true);
        self.store.move_backlog_item(epic, &start).unwrap();
        epic
    }

    fn children(&self, epic: i64) -> Vec<headwork_core::backlog::BacklogItem> {
        self.store
            .backlog_items(&headwork_store::BacklogFilter {
                epic: Some(epic),
                ..headwork_store::BacklogFilter::default()
            })
            .unwrap()
    }

    fn tasks(&self) -> usize {
        TaskState::ALL
            .iter()
            .map(|state| self.store.tasks_in(*state).unwrap().len())
            .sum()
    }
}

fn started(pumped: &[Pumped]) -> Vec<String> {
    pumped
        .iter()
        .filter(|line| matches!(line.outcome, Outcome::Started { .. }))
        .map(|line| line.name.clone())
        .collect()
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one epic walked from its first start to its churn block"
)]
fn the_runner_starts_children_in_order_re_plans_a_lost_one_and_blocks_on_churn() {
    let mut rig = world();
    let epic = rig.add(
        "Runner epic",
        &ItemShape {
            kind: Some(ItemKind::Epic),
            ..ItemShape::default()
        },
    );
    let a = rig.child(epic, "A piece", &[]);
    let b = rig.child(epic, "B piece", &[a]);
    let c = rig.child(epic, "C piece", &[]);
    let d = rig.child(epic, "D piece", &[]);
    assert!(
        started(&rig.pump()).is_empty(),
        "nothing runs while the epic is in the backlog"
    );
    let start = BacklogMove::new(Target::InProgress, BacklogActor::User, Actor::User, true);
    rig.store.move_backlog_item(epic, &start).unwrap();

    let first = rig.pump();
    assert_eq!(
        started(&first),
        vec![
            format!("demo-e{epic}-{a}-a-piece"),
            format!("demo-e{epic}-{c}-c-piece")
        ],
        "{first:?}"
    );
    for id in [a, c] {
        assert_eq!(rig.status(id), BacklogStatus::InProgress);
        let task = rig.task_of(id).unwrap();
        assert_eq!(task.kind, "code");
        assert_eq!(task.epic, None, "a plain child is no explorer");
        let brief = std::fs::read_to_string(&task.brief_path).unwrap();
        assert!(brief.starts_with("Goal: "), "{brief}");
    }
    assert_eq!(
        rig.status(b).block().map(|block| block.reason),
        Some(BlockReason::Depends)
    );
    assert_eq!(
        rig.status(d),
        BacklogStatus::Backlog,
        "max_parallel 2 holds D"
    );

    let tasks = rig.tasks();
    assert!(
        started(&rig.pump()).is_empty(),
        "a second pump starts nothing twice"
    );
    assert_eq!(rig.tasks(), tasks);

    let task_a = rig.task_of(a).unwrap();
    rig.merge(&task_a);
    assert_eq!(rig.status(a), BacklogStatus::Done);
    assert_eq!(
        rig.status(b),
        BacklogStatus::Backlog,
        "B's dependency is done"
    );
    let after_merge = rig.pump();
    assert_eq!(
        started(&after_merge),
        vec![format!("demo-e{epic}-{b}-b-piece")],
        "B takes the freed slot before D"
    );
    assert_eq!(rig.status(d), BacklogStatus::Backlog);

    rig.drop_child_task(c);
    let replanned = rig.pump();
    let names = started(&replanned);
    assert_eq!(names.len(), 1, "one re-plan explorer: {replanned:?}");
    let children = rig
        .store
        .backlog_items(&headwork_store::BacklogFilter {
            epic: Some(epic),
            ..headwork_store::BacklogFilter::default()
        })
        .unwrap();
    let replan = children.last().unwrap();
    assert_eq!(replan.draft.text, format!("Re-plan: #{c} task_lost"));
    assert_eq!(
        rig.store
            .backlog_plan(replan.id)
            .unwrap()
            .task_kind
            .as_deref(),
        Some(EXPLORE_KIND)
    );
    let explorer = rig.task_of(replan.id).unwrap();
    assert_eq!(names[0], explorer.name.to_string());
    assert_eq!(explorer.kind, EXPLORE_KIND);
    assert_eq!(explorer.epic, Some(epic), "spawned as an epic explorer");
    let brief = std::fs::read_to_string(&explorer.brief_path).unwrap();
    assert!(
        brief.contains("Writes: ") && brief.contains("/epics/"),
        "{brief}"
    );
    assert!(brief.contains("task_lost"), "{brief}");
    assert_eq!(
        rig.status(d),
        BacklogStatus::Backlog,
        "the live explorer holds D"
    );

    let tasks = rig.tasks();
    assert!(
        started(&rig.pump()).is_empty(),
        "no second re-plan for the same block"
    );
    assert_eq!(rig.tasks(), tasks);

    rig.drop_child_task(replan.id);
    let cancel = BacklogMove::new(Target::Cancelled, BacklogActor::User, Actor::User, true);
    rig.store.move_backlog_item(replan.id, &cancel).unwrap();
    rig.drop_child_task(b);
    let tasks = rig.tasks();
    let churned = rig.pump();
    assert!(started(&churned).is_empty(), "{churned:?}");
    assert_eq!(rig.tasks(), tasks);
    let block = rig.status(epic).block().cloned().unwrap();
    assert_eq!(block.reason, BlockReason::Decision);
    assert_eq!(block.note.as_deref(), Some("epic-churn: 1 re-plans"));
    let open_explore = rig
        .store
        .backlog_items(&headwork_store::BacklogFilter {
            epic: Some(epic),
            ..headwork_store::BacklogFilter::default()
        })
        .unwrap()
        .into_iter()
        .filter(|item| item.draft.status.is_open() && item.draft.text.starts_with("Re-plan"))
        .count();
    assert_eq!(open_explore, 0, "the churn block adds no re-plan");
    assert_eq!(rig.task_of(d), None);
    assert_eq!(
        rig.store.task(explorer.id).unwrap().state,
        TaskState::Dropped
    );
}

#[test]
fn a_plan_complete_re_plan_explorer_finishes_the_epic_though_its_own_child_is_open() {
    let mut rig = world();
    let epic = rig.epic("Finished epic");
    let only = rig.child(epic, "Only piece", &[]);
    let done = BacklogMove::new(Target::Done, BacklogActor::User, Actor::User, true);
    rig.store.move_backlog_item(only, &done).unwrap();
    let pumped = rig.pump();
    assert_eq!(started(&pumped).len(), 1, "{pumped:?}");
    let replan = rig.children(epic).last().unwrap().clone();
    assert_eq!(replan.draft.text, "Re-plan: plan complete?");
    let explorer = rig.task_of(replan.id).unwrap();
    assert_eq!(explorer.epic, Some(epic));
    let (is_error, text) = rig.worker_tool(
        &explorer,
        "plan_finish",
        &serde_json::json!({ "note": "all done" }),
    );
    assert!(!is_error, "{text}");
    assert_eq!(rig.status(epic), BacklogStatus::Done);
    assert_eq!(
        rig.status(replan.id),
        BacklogStatus::InProgress,
        "its own child stays with its task until the task ends"
    );
}
