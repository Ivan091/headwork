//! `status --json` carries the open review of a task.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;

use headwork::ops::status::status;
use headwork_core::review::Level;
use headwork_core::usage::UsageConfig;
use headwork_core::{Actor, Config, PermissionMode, ProjectName, Slug, TaskRequest, TaskShape};
use headwork_store::{NewTask, Store};

#[test]
fn an_open_review_shows_level_flow_and_round_and_none_shows_null() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/config.toml");
    let config = Config::parse(&std::fs::read_to_string(path).unwrap(), "config.toml").unwrap();
    let mut store = Store::open_in_memory().unwrap();
    store.sync_projects(&config).unwrap();
    let request = NewTask {
        request: TaskRequest {
            project: ProjectName::new("me-sim").unwrap(),
            slug: Slug::new("feature").unwrap(),
            kind: "scene".to_owned(),
            model: None,
            effort: None,
            in_main: false,
            keep: false,
        },
        brief_path: "briefs/me-sim-feature.md".to_owned(),
        depends_on: None,
        permission_mode: PermissionMode::Auto,
        shape: TaskShape::Lead,
    };
    let task = store
        .create_task(&config, &request, Actor::Orchestrator)
        .unwrap();
    let prices = UsageConfig::default();
    assert_eq!(
        status(&store, false, &prices).unwrap()[0].review,
        None,
        "no review is open"
    );
    store
        .request_review(
            task.id,
            "code",
            Level::Standard,
            "the park path",
            Actor::Hook,
        )
        .unwrap();
    let lines = status(&store, false, &prices).unwrap();
    let json = serde_json::to_value(&lines[0]).unwrap();
    assert_eq!(
        json["review"],
        serde_json::json!({
            "level": 2,
            "flow": "code",
            "round": 1,
            "state": "requested",
            "focus": "the park path",
            "findings_awaiting_lead": 0
        }),
        "the review field's shape"
    );
}
