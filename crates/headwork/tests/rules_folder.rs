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
