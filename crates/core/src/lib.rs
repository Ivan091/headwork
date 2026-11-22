//! The pure core of headwork: the lifecycle state machine, its guards, the hook rules and the config model.
//!
//! Nothing here performs I/O; the store and the binary feed it data and act on its decisions.

pub mod admission;
pub mod clean;
pub mod config;
pub mod guard;
pub mod layout;
pub mod lead;
pub mod lifecycle;
pub mod names;
pub mod platform;
pub mod process;
pub mod reap;
pub mod review;
pub mod slots;
pub mod update;
pub mod usage;
pub mod user_go;
pub mod vocabulary;
pub mod watch;

pub use admission::{AdmissionRefusal, Admitted, TaskRequest, admit};
pub use config::{
    Config, ConfigError, LayoutConfig, LayoutMode, LeadsConfig, ProjectConfig, ProjectOverlay,
    SILENT_KIND, UpdateConfig,
};
pub use guard::{Decision, Verdict};
pub use lead::{
    PartSlots, QueueEntry, QueueKey, ShapeError, StatusFacts, TaskShape, TaskStatus, queue_order,
    status,
};
pub use lifecycle::{Refusal, Step, TaskSnapshot, Trigger, apply};
pub use names::{
    LEGACY_ORCHESTRATOR_ADDRESS, NameError, ORCHESTRATOR_ADDRESS, ProjectName, Slug, TaskName,
    is_orchestrator_address, orchestrator_address,
};
pub use platform::Platform;
pub use vocabulary::{
    Actor, AskKind, AskState, Effort, FindingId, FindingVerdict, JobState, MachineMode, Model,
    NodeState, PermissionMode, PlaceKind, RecapKind, Resolution, ReviewState, Role, RuleId,
    SessionState, Severity, TaskState, UnknownWord, UsageGroup, UsageRole,
};
