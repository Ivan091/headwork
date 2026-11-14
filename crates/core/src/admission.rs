//! The guard of spawn → `queued`: whether a new task may exist at all.

use crate::config::Config;
use crate::names::{ProjectName, Slug};
use crate::vocabulary::{Effort, MachineMode, Model};

/// A request for a new task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRequest {
    /// The project.
    pub project: ProjectName,
    /// The slug, unique among the project's live tasks.
    pub slug: Slug,
    /// The task kind, such as `scene` or `car-training`.
    pub kind: String,
    /// The model; the machine default when absent.
    pub model: Option<Model>,
    /// The effort; the machine default when absent.
    pub effort: Option<Effort>,
    /// The task runs in the project's main checkout instead of a worktree.
    pub in_main: bool,
    /// A standing role that does not close when idle.
    pub keep: bool,
}

/// A request that passed the guard, with the values it resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Admitted {
    /// The model the task runs.
    pub model: Model,
    /// The effort the task runs; the machine default when absent.
    pub effort: Option<Effort>,
    /// The task needs the user's Fable approval before it may start.
    pub needs_fable_approval: bool,
}

/// Why a task may not be created.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionRefusal {
    /// No such project in the config.
    #[error("project `{0}` is not configured")]
    UnknownProject(ProjectName),
    /// The project is disabled.
    #[error("project `{0}` is disabled")]
    DisabledProject(ProjectName),
    /// A live task already has the name.
    #[error("a live task `{project}-{slug}` already exists")]
    NameTaken {
        /// The project.
        project: ProjectName,
        /// The slug.
        slug: Slug,
    },
    /// A main-only kind asked for a worktree.
    #[error("kind `{0}` is main-only and must run in the main checkout")]
    MainOnlyKind(String),
    /// A worktree kind asked for the main checkout.
    #[error("kind `{0}` is not main-only and must run in a worktree")]
    NotMainOnlyKind(String),
    /// The model is not allowed for the project.
    #[error("model `{0}` is not allowed for this project")]
    ModelNotAllowed(Model),
    /// The machine is not in normal mode.
    #[error("the machine is `{0}`; new tasks wait until it is normal")]
    MachineMode(MachineMode),
}

/// Decides whether a task may be created.
///
/// # Errors
///
/// Returns an [`AdmissionRefusal`] when the project is unknown or disabled, the name is taken, the kind and the
/// checkout disagree, the model is not allowed, or the machine is not in normal mode.
pub fn admit(
    config: &Config,
    request: &TaskRequest,
    name_taken: bool,
    machine: MachineMode,
) -> Result<Admitted, AdmissionRefusal> {
    if machine != MachineMode::Normal {
        return Err(AdmissionRefusal::MachineMode(machine));
    }
    let project = config
        .project(&request.project)
        .ok_or_else(|| AdmissionRefusal::UnknownProject(request.project.clone()))?;
    if !project.enabled {
        return Err(AdmissionRefusal::DisabledProject(request.project.clone()));
    }
    if name_taken {
        return Err(AdmissionRefusal::NameTaken {
            project: request.project.clone(),
            slug: request.slug.clone(),
        });
    }
    match (project.is_main_only(&request.kind), request.in_main) {
        (true, false) => return Err(AdmissionRefusal::MainOnlyKind(request.kind.clone())),
        (false, true) => return Err(AdmissionRefusal::NotMainOnlyKind(request.kind.clone())),
        (true, true) | (false, false) => {}
    }
    let model = request.model.unwrap_or(config.models.default);
    if !config.allowed_models(project).contains(&model) {
        return Err(AdmissionRefusal::ModelNotAllowed(model));
    }
    Ok(Admitted {
        model,
        effort: request.effort,
        needs_fable_approval: model == Model::Fable && config.models.fable_needs_user,
    })
}
