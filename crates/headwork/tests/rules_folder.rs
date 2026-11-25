//! The rules folder the hook's pointer names exists and matches the rules.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use headwork::home::Home;
use headwork::ops::rules::RuleFiles;
use headwork_core::Config;
use headwork_store::AddedRule;

fn config() -> Config {
    Config::parse(include_str!("../../../tests/fixtures/config.toml"), "test").unwrap()
}

#[test]
fn a_missing_folder_is_written_and_then_clean() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home::at(Some(dir.path().join("home"))).unwrap();
    let mut config = config();
    config.machine.rules_dir = Some(dir.path().join("rules").to_string_lossy().into_owned());
    let added = [AddedRule {
        id: 1,
        scope: "global".to_owned(),
        body: "be brief".to_owned(),
        version: 1,
    }];
    let files = RuleFiles::load(&home, &config, &added, "orchestrator").unwrap();
    assert!(!files.folder_problems().is_empty(), "folder is missing");
    files.sync_folder().unwrap();
    assert!(files.folder_problems().is_empty(), "folder is complete");
    for name in ["orchestrator.md", "worker.md", "added.md"] {
        assert!(
            dir.path().join("rules").join(name).is_file(),
            "{name} exists"
        );
    }
    let text = std::fs::read_to_string(dir.path().join("rules/added.md")).unwrap();
    assert!(text.contains("be brief"), "added rule listed");
}

fn rule(id: i64, body: &str) -> AddedRule {
    AddedRule {
        id,
        scope: "global".to_owned(),
        body: body.to_owned(),
        version: id,
    }
}

fn setup() -> (tempfile::TempDir, Home, Config) {
    let dir = tempfile::tempdir().unwrap();
    let home = Home::at(Some(dir.path().join("home"))).unwrap();
    let mut config = config();
    config.machine.rules_dir = Some(dir.path().join("rules").to_string_lossy().into_owned());
    (dir, home, config)
}

#[test]
fn every_role_file_is_written_and_no_added_file_without_added_rules() {
    let (dir, home, config) = setup();
    let files = RuleFiles::load(&home, &config, &[], "orchestrator").unwrap();
    files.sync_folder().unwrap();
    for name in ["orchestrator.md", "worker.md", "lead.md", "reviewer.md"] {
        assert!(
            dir.path().join("rules").join(name).is_file(),
            "{name} exists"
        );
    }
    assert!(!dir.path().join("rules/added.md").exists(), "no added.md");
}

#[test]
fn a_new_rule_refreshes_added_md_and_an_edited_role_file_stays() {
    let (dir, home, config) = setup();
    let first = [rule(1, "be brief")];
    RuleFiles::load(&home, &config, &first, "orchestrator")
        .unwrap()
        .sync_folder()
        .unwrap();
    let worker = dir.path().join("rules/worker.md");
    std::fs::write(&worker, "my own worker rules").unwrap();
    let second = [rule(1, "be brief"), rule(2, "be kind")];
    let files = RuleFiles::load(&home, &config, &second, "orchestrator").unwrap();
    assert_eq!(
        files.folder_problems(),
        ["added.md"],
        "only added.md is stale"
    );
    files.sync_folder().unwrap();
    let added = std::fs::read_to_string(dir.path().join("rules/added.md")).unwrap();
    assert!(added.contains("be kind"), "added.md refreshed");
    assert_eq!(
        std::fs::read_to_string(&worker).unwrap(),
        "my own worker rules"
    );
}

#[test]
fn the_pointer_names_added_md_only_when_rules_were_added() {
    let (dir, home, config) = setup();
    std::fs::create_dir_all(dir.path().join("rules")).unwrap();
    std::fs::write(dir.path().join("rules/worker.md"), "x".repeat(11_000)).unwrap();
    let exe = std::path::Path::new("headwork");
    let none = RuleFiles::load(&home, &config, &[], "orchestrator").unwrap();
    let text = none.render(headwork_core::Role::Worker, &config, exe, None);
    assert!(
        text.contains("worker.md") && !text.contains("added.md"),
        "{text}"
    );
    let some = RuleFiles::load(&home, &config, &[rule(1, "a")], "orchestrator").unwrap();
    let text = some.render(headwork_core::Role::Worker, &config, exe, None);
    assert!(text.contains("added.md"), "{text}");
}
