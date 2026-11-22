//! The effort level a session really runs with, for tasks that name none.
//!
//! Claude Code resolves it per model from the settings layers, the highest layer that names one winning: the project's
//! local `.claude/settings.local.json`, then the shared `.claude/settings.json`, then the user's `settings.json`. In
//! a layer, `modelSettings.<model id>.effortLevel` beats the top-level `effortLevel`, and the user layer's top-level
//! `effortLevel` does not reach the 5.5 models. With no setting a 5.5 model runs at [`BUILT_IN`]. The choice is
//! recorded in `docs/decisions.md`.

use std::path::Path;

use headwork_core::{Effort, Model};
use serde_json::Value;

use headwork_core::Config;

use crate::home::Home;

/// What the current Claude models run with when no settings layer names an effort.
pub const BUILT_IN: Effort = Effort::Medium;

/// The model id `modelSettings` is keyed by for a model alias.
#[must_use]
pub const fn model_id(model: Model) -> &'static str {
    match model {
        Model::Opus => "claude-opus-5-5",
        Model::Sonnet => "claude-sonnet-5-5",
        Model::Haiku => "claude-haiku-5-5",
        Model::Fable => "claude-fable-5-2",
    }
}

/// The effort the settings name for a model, else the built-in one. `project` holds the shared then the local project
/// layer, the later one winning; `user` is the user's layer.
#[must_use]
pub fn from_layers(user: Option<&Value>, project: &[Value], model: Model) -> Effort {
    let id = model_id(model);
    let word = |value: &Value| {
        value
            .get("effortLevel")
            .and_then(Value::as_str)
            .and_then(|word| word.parse::<Effort>().ok())
    };
    let for_model = |layer: &Value| {
        layer
            .get("modelSettings")
            .and_then(|models| models.get(id))
            .and_then(word)
    };
    project
        .iter()
        .rev()
        .find_map(|layer| for_model(layer).or_else(|| word(layer)))
        .or_else(|| user.and_then(for_model))
        .unwrap_or(BUILT_IN)
}

fn read(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// The effort a session of `model` runs with in `dir`, from the settings files there and in the user's Claude folder.
#[must_use]
pub fn resolve(home: &Home, model: Model, dir: &Path) -> Effort {
    let user = read(&home.claude_dir().join("settings.json"));
    let project: Vec<Value> = ["settings.json", "settings.local.json"]
        .iter()
        .filter_map(|name| read(&dir.join(".claude").join(name)))
        .collect();
    from_layers(user.as_ref(), &project, model)
}

/// The effort a task runs with: the one it stores, else the one the settings give its model in its project.
#[must_use]
pub fn of_task(home: &Home, config: &Config, task: &headwork_store::Task) -> Effort {
    task.effort.unwrap_or_else(|| {
        let dir = config
            .project(task.project())
            .map(|project| home.expand(&project.main))
            .unwrap_or_default();
        resolve(home, task.model, &dir)
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn nothing_set_gives_the_built_in_level() {
        assert_eq!(
            from_layers(None, &[], Model::Sonnet),
            Effort::Medium,
            "built in"
        );
    }

    #[test]
    fn the_user_layer_names_models_only_by_id() {
        let user = json!({
            "effortLevel": "low",
            "modelSettings": { "claude-opus-5-5": { "effortLevel": "xhigh" } }
        });
        assert_eq!(
            from_layers(Some(&user), &[], Model::Opus),
            Effort::Xhigh,
            "opus by id"
        );
        assert_eq!(
            from_layers(Some(&user), &[], Model::Sonnet),
            Effort::Medium,
            "the user's top level does not reach the 5.5 models"
        );
    }

    #[test]
    fn project_layers_beat_the_user_and_local_beats_shared() {
        let user = json!({ "modelSettings": { "claude-opus-5-5": { "effortLevel": "low" } } });
        let shared = json!({ "effortLevel": "high" });
        let local = json!({ "modelSettings": { "claude-opus-5-5": { "effortLevel": "max" } } });
        assert_eq!(
            from_layers(Some(&user), std::slice::from_ref(&shared), Model::Opus),
            Effort::High,
            "shared beats user"
        );
        assert_eq!(
            from_layers(Some(&user), &[shared, local], Model::Opus),
            Effort::Max,
            "local beats shared"
        );
    }
}
