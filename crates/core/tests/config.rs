//! Config parsing and validation against the fixtures in `tests/fixtures`.

#![expect(clippy::unwrap_used, reason = "tests fail loudly on a bad fixture")]

use std::path::Path;

use headwork_core::config::{
    ConfigError, Delivery, HostKind, ParkPolicy, PushMode, RETIRED_PROJECT_KEYS, TaskVars,
    TemplateProblem, WorktreeCommand, check_template, expand_home,
};
use headwork_core::lifecycle::MergePolicy;
use headwork_core::{Config, LayoutMode, Model, ProjectName, ProjectOverlay, Severity};

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
fn each_host_names_the_variable_that_tells_a_program_its_surface() {
    for (host, var) in [
        (HostKind::Wmux, "WMUX_SURFACE_ID"),
        (HostKind::Cmux, "CMUX_SURFACE_ID"),
    ] {
        assert_eq!(host.surface_var(), var, "{host:?}");
    }
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
fn heavy_keys_still_load_and_are_named_as_retired() {
    let text = format!(
        "{}\n[project.extra]\nmain = \"S:/x\"\nheavy_slots = 2\nheavy_gate = \"scripts/heavy.sh\"\n",
        fixture("config.toml").replace("[project.me-sim]", "[project.me-sim]\nheavy_slots = 1")
    );
    assert!(
        Config::retired_keys(&fixture("config.toml")).is_empty(),
        "the fixture is current"
    );
    let retired = Config::retired_keys(&text);
    assert!(
        retired.contains(&"project.me-sim.heavy_slots".to_owned()),
        "{retired:?}"
    );
    assert!(
        retired.contains(&"project.extra.heavy_slots".to_owned())
            && retired.contains(&"project.extra.heavy_gate".to_owned()),
        "{retired:?}"
    );
    assert!(
        Config::retired_keys("not [toml").is_empty(),
        "unparsable text"
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
fn user_only_is_the_union_of_every_project() {
    assert_eq!(
        machine().all_user_only(),
        [
            "me-studio publish",
            "scripts/publish-now.sh",
            "scripts/publish.sh",
            "scripts/release.sh",
            "scripts/reupload.sh",
            "scripts/studio-publish.sh",
        ],
        "sorted, each once"
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
    let config = Config::parse(
        "[machine]
host = \"wmux\"

[layout]
mode = \"workspaces\"
",
        "layout",
    )
    .unwrap();
    assert_eq!(config.layout.mode, LayoutMode::Workspaces, "workspaces");
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
        "me-sim-cars",
        "default with an effort"
    );
    assert_eq!(
        default.layout.title_for("headwork-m7", "sonnet", None),
        "headwork-m7",
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

#[test]
fn enforce_rules_name_known_rules_and_enforce_only_those() {
    use headwork_core::RuleId;
    let listed = "[machine]\nhost = \"wmux\"\n[hooks]\nenforce_rules = [\"stop-unfinished\", \"child-fable\"]\n";
    let config = Config::parse(listed, "t").unwrap();
    assert!(config.hooks.enforces(RuleId::StopUnfinished), "listed");
    assert!(config.hooks.enforces(RuleId::WorkerFable), "old spelling");
    assert!(
        !config.hooks.enforces(RuleId::UserOnly),
        "unlisted stays shadow"
    );
    let all = Config::parse("[machine]\nhost = \"wmux\"\n[hooks]\nenforce = true\n", "t").unwrap();
    assert!(
        all.hooks.enforces(RuleId::UserOnly),
        "enforce covers every rule"
    );
    assert!(
        !machine().hooks.enforces(RuleId::UserOnly),
        "default is shadow"
    );
    assert!(machine().hooks.enforce_rules.is_empty(), "default is empty");
    let unknown = Config::parse(
        "[machine]\nhost = \"wmux\"\n[hooks]\nenforce_rules = [\"no-such-rule\"]\n",
        "t",
    );
    assert!(unknown.is_err(), "unknown id");
}

#[test]
fn extra_writable_paths_name_their_task_kinds() {
    let config = machine();
    let me_sim = config.project(&project("me-sim")).unwrap();
    let paths: Vec<&str> = me_sim
        .extra_writable
        .iter()
        .map(|grant| grant.path.as_str())
        .collect();
    assert_eq!(
        paths,
        [
            "S:/Code/notes/handoff-car-training.md",
            "S:/Code/notes/night-results"
        ],
        "in order"
    );
    assert!(
        me_sim
            .extra_writable
            .iter()
            .all(|grant| grant.applies_to("car-training")),
        "car-training"
    );
    assert!(
        !me_sim
            .extra_writable
            .iter()
            .any(|grant| grant.applies_to("task")),
        "not other kinds"
    );
    let studio = config.project(&project("me-studio")).unwrap();
    assert!(studio.extra_writable.is_empty(), "none by default");
    assert!(
        config.hooks.brief_writable.is_empty(),
        "no brief roots by default"
    );
    let every = fixture("config.toml").replace(", kinds = [\"car-training\"] }", " }");
    let every = Config::parse(&every, "every").unwrap();
    let every = every.project(&project("me-sim")).unwrap();
    assert_eq!(every.extra_writable.len(), 2, "both entries");
    assert!(
        every
            .extra_writable
            .iter()
            .all(|grant| grant.applies_to("task")),
        "no kinds: every task"
    );
    let unknown = fixture("config.toml").replace("kinds = [\"car-training\"]", "kind = \"x\"");
    assert!(
        Config::parse(&unknown, "unknown").is_err(),
        "unknown keys refused"
    );
}

#[test]
fn leads_are_off_by_default_with_two_parts_titled_by_the_part() {
    let config = Config::parse("[machine]\nhost = \"cmux\"\n", "minimal").unwrap();
    assert!(
        !config.leads.enabled,
        "off until placement and part_spawn exist"
    );
    assert_eq!(config.leads.max_parts, None, "no limit without the key");
    assert_eq!(config.leads.part_title, "{part}", "the part's own slug");
    let config = Config::parse(
        "[machine]\nhost = \"cmux\"\n\n[leads]\nenabled = true\nmax_parts = 3\npart_title = \"{part} {model}\"\n",
        "leads",
    )
    .unwrap();
    assert!(config.leads.enabled, "enabled");
    assert_eq!(
        config.leads.max_parts.map(std::num::NonZeroU32::get),
        Some(3),
        "three parts"
    );
    let zero = Config::parse(
        "[machine]
host = \"cmux\"

[leads]
max_parts = 0
",
        "zero",
    )
    .unwrap();
    assert_eq!(zero.leads.max_parts, None, "0 means no limit");
    assert_eq!(config.leads.part_title, "{part} {model}", "title");
}

#[test]
fn leads_refuse_unknown_keys_zero_parts_and_bad_titles() {
    for bad in [
        "max_parts = -1",
        "max_parts = \"two\"",
        "parts = 2",
        "enabled = \"yes\"",
    ] {
        let text = format!("[machine]\nhost = \"cmux\"\n\n[leads]\n{bad}\n");
        let error = Config::parse(&text, "leads").unwrap_err();
        assert!(matches!(error, ConfigError::Parse { .. }), "{bad}: {error}");
    }
    let cases = [
        ("{slug}", TemplateProblem::Unknown("slug".to_owned())),
        ("{part", TemplateProblem::Unclosed),
    ];
    for (title, problem) in cases {
        let text = format!("[machine]\nhost = \"cmux\"\n\n[leads]\npart_title = \"{title}\"\n");
        let error = Config::parse(&text, "leads").unwrap_err();
        assert!(
            matches!(&error, ConfigError::PartTitle(found) if *found == problem),
            "{title}: {error}"
        );
    }
    for title in ["{part}", "{name}", "{model} {effort}", "plain"] {
        let text = format!("[machine]\nhost = \"cmux\"\n\n[leads]\npart_title = \"{title}\"\n");
        assert!(Config::parse(&text, "leads").is_ok(), "{title} is fine");
    }
}

#[test]
fn update_is_off_by_default_for_headwork_every_fifteen_minutes() {
    let config = Config::parse("[machine]\nhost = \"cmux\"\n", "minimal").unwrap();
    assert!(!config.update.enabled, "off until the user turns it on");
    assert_eq!(config.update.project, project("headwork"), "project");
    assert_eq!(config.update.poll_min, 15, "poll");
    assert_eq!(config.update_repo(), None, "no headwork project here");
}

fn with_headwork(update: &str, repos: &str) -> String {
    format!(
        "[machine]\nhost = \"cmux\"\n\n[update]\n{update}\n\n[project.headwork]\nmain = \"~/headwork\"\n\
         worktree_new = \"builtin\"\nworktree_remove = \"builtin\"\nworktree_path = \"{{main}}-{{slug}}\"\n\
         deliver = \"pr\"\nmerge = \"human\"\nallowed_repos = [{repos}]\n"
    )
}

#[test]
fn update_names_a_configured_project_with_a_repository_when_on() {
    let config = Config::parse(
        &with_headwork("enabled = true\npoll_min = 0", "\"Ivan091/headwork\""),
        "on",
    )
    .unwrap();
    assert!(config.update.enabled, "on");
    assert_eq!(config.update.poll_min, 0, "tick trigger off");
    assert_eq!(
        config
            .update_repo()
            .map(headwork_core::config::Repo::as_str),
        Some("Ivan091/headwork"),
        "the project's repository"
    );
    let error = Config::parse(
        &with_headwork("enabled = true\nproject = \"elsewhere\"", "\"a/b\""),
        "missing",
    )
    .unwrap_err();
    assert!(
        matches!(&error, ConfigError::UpdateProject(name) if name.as_str() == "elsewhere"),
        "{error}"
    );
    let error = Config::parse(&with_headwork("enabled = true", ""), "no repo").unwrap_err();
    assert!(matches!(error, ConfigError::UpdateRepo(_)), "{error}");
    assert!(
        Config::parse(&with_headwork("project = \"elsewhere\"", ""), "off").is_ok(),
        "an update that is off is not checked"
    );
}

#[test]
fn update_refuses_unknown_keys_and_bad_values() {
    for bad in [
        "every = 5",
        "enabled = \"yes\"",
        "poll_min = -1",
        "project = \"Bad Name\"",
    ] {
        let error = Config::parse(&with_headwork(bad, "\"a/b\""), "update").unwrap_err();
        assert!(matches!(error, ConfigError::Parse { .. }), "{bad}: {error}");
    }
}

#[test]
fn usage_prices_parse_and_bad_ones_are_refused() {
    let config = Config::parse("[machine]\nhost = \"wmux\"\n", "minimal").unwrap();
    assert_eq!(config.usage.keep_days, 90, "default retention");
    assert_eq!(config.usage.nudge_context_k, 200, "default nudge");
    let off = Config::parse(
        "[machine]
host = \"wmux\"

[usage]
nudge_context_k = 0
",
        "off",
    )
    .unwrap();
    assert_eq!(off.usage.nudge_context_k, 0, "nudge off");
    assert!(config.usage.prices.is_empty(), "no prices by default");
    let text = "[machine]\nhost = \"wmux\"\n\n[usage]\nkeep_days = 30\n\n[usage.prices.\"claude-opus\"]\ninput = 15.0\noutput = 75\n";
    let config = Config::parse(text, "prices").unwrap();
    assert_eq!(config.usage.keep_days, 30, "retention");
    let opus = config.usage.price_for("claude-opus-5-5").unwrap();
    assert_eq!(opus.input.micros(), 15_000_000, "input");
    assert_eq!(opus.output.micros(), 75_000_000, "an integer price");
    assert_eq!(opus.cache_read.micros(), 0, "unset is zero");
    let negative = text.replace("input = 15.0", "input = -1.0");
    assert!(
        Config::parse(&negative, "negative").is_err(),
        "a negative price"
    );
    let unknown = text.replace("input = 15.0", "inptu = 15.0");
    assert!(Config::parse(&unknown, "typo").is_err(), "an unknown key");
}

#[test]
fn a_kind_takes_its_own_default_model() {
    let text = "[machine]\nhost = \"cmux\"\n[models]\ndefault = \"sonnet\"\n[models.by_kind]\ncar-training = \"haiku\"\n";
    let config = Config::parse(text, "kinds").unwrap();
    assert_eq!(
        config.models.for_kind("car-training"),
        Model::Haiku,
        "listed kind"
    );
    assert_eq!(
        config.models.for_kind("task"),
        Model::Sonnet,
        "unlisted kind"
    );
}

#[test]
fn every_retired_key_is_still_a_project_field() {
    let base = fixture("config.toml");
    for key in RETIRED_PROJECT_KEYS {
        let accepted = ["1", "\"x\""].iter().any(|value| {
            let text = base.replace(
                "[project.me-sim]",
                &format!(
                    "[project.me-sim]
{key} = {value}"
                ),
            );
            Config::parse(&text, "test").is_ok()
                && Config::retired_keys(&text).contains(&format!("project.me-sim.{key}"))
        });
        assert!(
            accepted,
            "{key} is listed as retired but the parser no longer accepts it"
        );
    }
}

#[test]
fn digest_defaults_parse_and_a_bad_hour_is_refused() {
    let base = "[machine]\nhost = \"wmux\"\n";
    let config = Config::parse(base, "minimal").unwrap();
    assert_eq!(config.digest.finding_hours, 3, "default finding hours");
    assert!(config.digest.push, "pushed by default");
    assert_eq!(config.digest.push_hour, 9, "default hour");
    let set = format!("{base}\n[digest]\nfinding_hours = 6\npush = false\npush_hour = 18\n");
    let config = Config::parse(&set, "set").unwrap();
    assert_eq!(
        (
            config.digest.finding_hours,
            config.digest.push,
            config.digest.push_hour
        ),
        (6, false, 18),
        "set values"
    );
    let bad = format!("{base}\n[digest]\npush_hour = 24\n");
    assert!(
        matches!(
            Config::parse(&bad, "bad").unwrap_err(),
            ConfigError::DigestHour(24)
        ),
        "an hour past 23"
    );
}

#[test]
fn review_policy_defaults_parse_and_bad_values_are_refused() {
    let base = "[machine]
host = \"wmux\"
";
    let config = Config::parse(base, "minimal").unwrap();
    assert_eq!(
        (
            config.reviews.floor,
            config.reviews.max_round_findings,
            config.reviews.max_task_findings
        ),
        (Severity::Doubt, 20, 40),
        "defaults"
    );
    let set = format!(
        "{base}
[reviews]
floor = \"nit\"
max_round_findings = 0
max_task_findings = 5
"
    );
    let config = Config::parse(&set, "set").unwrap();
    assert_eq!(
        (
            config.reviews.floor,
            config.reviews.max_round_findings,
            config.reviews.max_task_findings
        ),
        (Severity::Nit, 0, 5),
        "set values"
    );
    for bad in [
        "floor = \"minor\"",
        "floor = 1",
        "max_round_findings = -1",
        "max_task_findings = \"many\"",
    ] {
        let text = format!(
            "{base}
[reviews]
{bad}
"
        );
        let error = Config::parse(&text, "bad").unwrap_err();
        assert!(matches!(error, ConfigError::Parse { .. }), "{bad}: {error}");
    }
}

#[test]
fn the_todo_section_loads_as_backlog_and_is_named_as_deprecated() {
    let cases = [
        (
            "[todo]\nfile = \"S:/x/backlog.md\"\n",
            vec![("todo.file", "backlog.file")],
        ),
        (
            "[todo]\nfile = \"S:/x/backlog.md\"\nleave_pointer = true\n",
            vec![
                ("todo.file", "backlog.file"),
                ("todo.leave_pointer", "backlog.leave_pointer"),
            ],
        ),
        ("[backlog]\nfile = \"S:/x/backlog.md\"\n", Vec::new()),
    ];
    for (text, expected) in cases {
        let config = Config::parse(
            &format!(
                "[machine]
host = \"wmux\"
{text}"
            ),
            "todo",
        )
        .unwrap();
        assert_eq!(
            config.backlog.file.as_deref(),
            Some("S:/x/backlog.md"),
            "{text}"
        );
        let named: Vec<(String, String)> = expected
            .into_iter()
            .map(|(old, new)| (old.to_owned(), new.to_owned()))
            .collect();
        assert_eq!(Config::deprecated_keys(text), named, "{text}");
    }
    let config = Config::parse(
        "[machine]
host = \"wmux\"
[backlog]\nleave_pointer = true\n",
        "pointer",
    )
    .unwrap();
    assert!(config.backlog.leave_pointer);
    assert!(
        !Config::parse(
            "[machine]
host = \"wmux\"
",
            "empty"
        )
        .unwrap()
        .backlog
        .leave_pointer
    );
}

#[test]
fn todo_beside_backlog_folds_into_backlog_and_only_different_values_are_refused() {
    let base = "[machine]\nhost = \"wmux\"\n";
    let cases = [
        (
            "[todo]\nleave_pointer = true\n[backlog]\nfile = \"S:/x/backlog.md\"\n",
            Some((Some("S:/x/backlog.md"), true)),
        ),
        (
            "[todo]\nfile = \"S:/x/backlog.md\"\n[backlog]\nfile = \"S:/x/backlog.md\"\n",
            Some((Some("S:/x/backlog.md"), false)),
        ),
        (
            "[todo]\nfile = \"S:/x/todo.md\"\n[backlog]\nfile = \"S:/x/backlog.md\"\n",
            None,
        ),
    ];
    for (text, expected) in cases {
        let parsed = Config::parse(&format!("{base}{text}"), "both");
        if let Some((file, pointer)) = expected {
            let config = parsed.unwrap();
            assert_eq!(config.backlog.file.as_deref(), file, "{text}");
            assert_eq!(config.backlog.leave_pointer, pointer, "{text}");
        } else {
            let error = parsed.unwrap_err();
            assert!(
                matches!(&error, ConfigError::TodoConflict { key, .. } if key == "file"),
                "{text}: {error}"
            );
        }
    }
}
