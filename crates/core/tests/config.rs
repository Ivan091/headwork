//! Config parsing and validation against the fixtures in `tests/fixtures`.

#![expect(clippy::unwrap_used, reason = "tests fail loudly on a bad fixture")]

use std::path::Path;

use headwork_core::config::{
    ConfigError, Delivery, HostKind, ParkPolicy, PushMode, TaskVars, TemplateProblem,
    WorktreeCommand, check_template, expand_home,
};
use headwork_core::lifecycle::MergePolicy;
use headwork_core::{Config, LayoutMode, Model, ProjectName, ProjectOverlay};

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

fn machine() -> Config {
    Config::parse(&fixture("config.toml"), "config.toml").unwrap()
}

fn project(name: &str) -> ProjectName {
    ProjectName::new(name).unwrap()
}

#[test]
fn fixture_parses_with_all_three_projects() {
    let config = machine();
    assert_eq!(config.machine.host, HostKind::Wmux, "host");
    assert_eq!(config.machine.max_workers.get(), 10, "max_workers");
    let names: Vec<_> = config.projects.keys().map(ProjectName::as_str).collect();
    assert_eq!(
        names,
        ["me-sim", "me-studio", "work-service"],
        "projects in name order"
    );
}

#[test]
fn me_sim_uses_its_own_scripts() {
    let config = machine();
    let me_sim = config.project(&project("me-sim")).unwrap();
    assert_eq!(
        me_sim.deliver,
        Delivery::Command("scripts/ship.sh".to_owned()),
        "deliver"
    );
    assert_eq!(me_sim.merge, MergePolicy::Deliver, "merge");
    assert!(
        me_sim.is_main_only("car-training"),
        "car training runs in main"
    );
    assert!(!me_sim.is_main_only("scene"), "scenes run in worktrees");
    assert_eq!(
        me_sim.jobs["car-training"].park_policy,
        ParkPolicy::Checkpoint,
        "training checkpoints"
    );
    assert_eq!(
        config.allowed_models(me_sim),
        Model::ALL,
        "machine models apply"
    );
}

#[test]
fn work_repository_is_narrower() {
    let config = machine();
    let work = config.project(&project("work-service")).unwrap();
    assert_eq!(
        work.worktree_new,
        WorktreeCommand::Builtin,
        "builtin worktrees"
    );
    assert_eq!(work.deliver, Delivery::Pr, "global PR flow");
    assert_eq!(work.merge, MergePolicy::Human, "human merge");
    assert_eq!(work.push, Some(PushMode::None), "no push");
    assert_eq!(
        work.max_workers.map(std::num::NonZeroU32::get),
        Some(3),
        "project cap"
    );
    assert!(
        !config.allowed_models(work).contains(&Model::Fable),
        "no Fable at work"
    );
}

#[test]
fn defaults_fill_a_minimal_config() {
    let config = Config::parse("[machine]\nhost = \"cmux\"\n", "minimal").unwrap();
    assert_eq!(config.machine.max_workers.get(), 10, "default cap");
    assert_eq!(config.machine.push, PushMode::Claude, "default push");
    assert_eq!(config.models.default, Model::Sonnet, "default model");
    assert_eq!(config.watch.disk_warn_gb, 15, "default disk warning");
    assert!(config.projects.is_empty(), "no projects");
    assert_eq!(config.layout.mode, LayoutMode::Panes, "panes by default");
    assert_eq!(config.layout.panes.get(), 4, "four panes by default");
}

#[test]
fn deprecated_keys_load_and_are_named() {
    let text = fixture("config.toml")
        .replacen("max_workers = 10", "max_children = 7", 1)
        .replacen("max_workers = 3", "max_children = 2", 1);
    let text = format!("{text}\n[hooks]\nroot_writable = [\"~/old\"]\n");
    let config = Config::parse(&text, "old keys").unwrap();
    assert_eq!(config.machine.max_workers.get(), 7, "machine.max_children");
    assert_eq!(
        config.hooks.orchestrator_writable,
        ["~/old"],
        "hooks.root_writable"
    );
    let work = config.project(&project("work-service")).unwrap();
    assert_eq!(
        work.max_workers.map(std::num::NonZeroU32::get),
        Some(2),
        "project max_children"
    );
    let deprecated = Config::deprecated_keys(&text);
    let old: Vec<&str> = deprecated.iter().map(|(old, _)| old.as_str()).collect();
    assert_eq!(
        old,
        [
            "machine.max_children",
            "hooks.root_writable",
            "project.work-service.max_children"
        ],
        "every old key"
    );
    assert!(
        Config::deprecated_keys(&fixture("config.toml")).is_empty(),
        "the fixture is current"
    );
}

#[test]
fn unknown_keys_are_refused() {
    let error = Config::parse("[machine]\nhost = \"wmux\"\nmax_kids = 3\n", "typo").unwrap_err();
    assert!(matches!(error, ConfigError::Parse { .. }), "{error}");
}

#[test]
fn bad_project_names_are_refused() {
    let text = format!(
        "{}\n[project.Me_Sim]\nmain = \"x\"\n",
        "[machine]\nhost = \"wmux\""
    );
    assert!(
        matches!(Config::parse(&text, "name"), Err(ConfigError::Parse { .. })),
        "uppercase project name"
    );
}

#[test]
fn project_may_not_widen_models() {
    let text = fixture("config.toml").replace(
        "allowed = [\"haiku\", \"sonnet\", \"opus\", \"fable\"]",
        "allowed = [\"haiku\", \"sonnet\", \"opus\"]",
    );
    let text = text.replace(
        "models.allowed = [\"haiku\", \"sonnet\", \"opus\"]",
        "models.allowed = [\"fable\"]",
    );
    let error = Config::parse(&text, "widen").unwrap_err();
    assert!(
        matches!(
            error,
            ConfigError::ProjectWidensModels {
                model: Model::Fable,
                ..
            }
        ),
        "project widens to Fable: {error}"
    );
}

#[test]
fn default_model_must_be_allowed() {
    let text =
        "[machine]\nhost = \"wmux\"\n[models]\ndefault = \"fable\"\nallowed = [\"sonnet\"]\n";
    let error = Config::parse(text, "default").unwrap_err();
    assert!(
        matches!(error, ConfigError::DefaultModelNotAllowed(Model::Fable)),
        "{error}"
    );
}

#[test]
fn disk_thresholds_must_be_ordered() {
    let text = "[machine]\nhost = \"wmux\"\n[watch]\ndisk_warn_gb = 5\ndisk_stop_gb = 5\n";
    let error = Config::parse(text, "disk").unwrap_err();
    assert!(
        matches!(error, ConfigError::DiskThresholds { stop: 5, warn: 5 }),
        "{error}"
    );
}

#[test]
fn unknown_placeholders_are_refused() {
    let text = fixture("config.toml").replace("{main}-{slug}", "{main}-{name}");
    let error = Config::parse(&text, "template").unwrap_err();
    assert!(matches!(error, ConfigError::Template { .. }), "{error}");
    assert_eq!(
        check_template("a {slug} b {branch}"),
        Ok(()),
        "known placeholders"
    );
    assert_eq!(
        check_template("{oops"),
        Err(TemplateProblem::Unclosed),
        "unclosed brace"
    );
}

#[test]
fn repositories_must_be_owner_slash_name() {
    let text = fixture("config.toml").replace("\"acme/service\"", "\"service\"");
    let error = Config::parse(&text, "repo").unwrap_err();
    assert!(matches!(error, ConfigError::BadRepo { .. }), "{error}");
}

#[test]
fn overlay_adds_commands() {
    let overlay = ProjectOverlay::parse(&fixture("me-sim.headwork.toml"), "overlay").unwrap();
    let me_sim = machine()
        .project(&project("me-sim"))
        .unwrap()
        .clone()
        .with_overlay(overlay);
    assert!(
        me_sim
            .long_jobs
            .iter()
            .any(|job| job == "scripts/night-render.sh"),
        "long job added"
    );
    assert!(
        me_sim
            .user_only
            .iter()
            .any(|command| command == "scripts/upload-short.sh"),
        "user-only added"
    );
    assert!(
        me_sim
            .user_only
            .iter()
            .any(|command| command == "scripts/publish.sh"),
        "machine entries stay"
    );
}

#[test]
fn overlay_may_not_widen() {
    let error = ProjectOverlay::parse(&fixture("widening.headwork.toml"), "overlay").unwrap_err();
    assert!(
        matches!(&error, ConfigError::OverlayKey { key, .. } if key == "allowed_repos"),
        "{error}"
    );
}

#[test]
fn home_expands_only_a_leading_tilde() {
    let home = Path::new("/home/me");
    assert_eq!(
        expand_home("~/work/service", home),
        home.join("work/service"),
        "tilde"
    );
    assert_eq!(
        expand_home("S:/Code", home),
        Path::new("S:/Code"),
        "absolute path unchanged"
    );
}

#[test]
fn task_templates_expand() {
    let vars = TaskVars {
        slug: "sand",
        branch: "sand",
        main: "S:/Code/me-sim/me-sim",
        worktree: "",
    };
    assert_eq!(
        vars.expand("{main}-{slug}").unwrap(),
        "S:/Code/me-sim/me-sim-sand",
        "path"
    );
    assert_eq!(
        vars.expand("scripts/worktree.sh new {slug}").unwrap(),
        "scripts/worktree.sh new sand",
        "command"
    );
    assert_eq!(
        vars.expand("{out}/x"),
        Err(TemplateProblem::Unknown("out".to_owned())),
        "a task has no out"
    );
    assert_eq!(
        vars.expand("{slug"),
        Err(TemplateProblem::Unclosed),
        "unclosed"
    );
}

#[test]
fn machine_shell_and_host_bin_are_optional() {
    let config = Config::parse(
        "[machine]\nhost = \"wmux\"\nshell = \"C:/Program Files/Git/bin/bash.exe\"\n",
        "shell",
    )
    .unwrap();
    assert_eq!(
        config.machine.shell.as_deref(),
        Some("C:/Program Files/Git/bin/bash.exe"),
        "shell"
    );
    assert_eq!(config.machine.host_bin, None, "host_bin");
}

#[test]
fn layout_takes_a_mode_and_a_pane_count() {
    let config = Config::parse(
        "[machine]\nhost = \"wmux\"\n\n[layout]\nmode = \"tabs\"\npanes = 2\n",
        "layout",
    )
    .unwrap();
    assert_eq!(config.layout.mode, LayoutMode::Tabs, "tabs");
    assert_eq!(config.layout.panes.get(), 2, "two panes");
    for bad in ["mode = \"grid\"", "panes = 0", "columns = 2"] {
        let text = format!("[machine]\nhost = \"wmux\"\n\n[layout]\n{bad}\n");
        assert!(Config::parse(&text, "layout").is_err(), "{bad} is refused");
    }
}

#[test]
fn the_pane_title_is_a_template_of_name_model_and_effort() {
    let default = Config::parse("[machine]\nhost = \"wmux\"\n", "title").unwrap();
    assert_eq!(
        default
            .layout
            .title_for("me-sim-cars", "opus", Some("high")),
        "me-sim-cars - opus high",
        "default with an effort"
    );
    assert_eq!(
        default.layout.title_for("headwork-m7", "sonnet", None),
        "headwork-m7 - sonnet ?",
        "default without an effort"
    );
    let custom = Config::parse(
        "[machine]\nhost = \"wmux\"\n\n[layout]\ntitle = \"[{model}/{effort}] {name}\"\n",
        "title",
    )
    .unwrap();
    assert_eq!(
        custom.layout.title_for("a-b", "haiku", Some("low")),
        "[haiku/low] a-b",
        "custom"
    );
}
