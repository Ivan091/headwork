//! The pure core of headwork: the lifecycle state machine, its guards, the hook rules and the config model.
//!
//! Nothing here performs I/O; the store and the binary feed it data and act on its decisions.

pub mod admission;
pub mod config;
pub mod guard;
pub mod lifecycle;
pub mod names;
pub mod vocabulary;
pub mod watch;

pub use admission::{AdmissionRefusal, Admitted, TaskRequest, admit};
pub use config::{Config, ConfigError, ProjectConfig, ProjectOverlay};
pub use guard::{Decision, Verdict};
pub use lifecycle::{Refusal, Step, TaskSnapshot, Trigger, apply};
pub use names::{NameError, ProjectName, Slug, TaskName};
pub use vocabulary::{
    Actor, AskKind, AskState, FindingId, MachineMode, Model, PermissionMode, Role, RuleId,
    SessionState, TaskState, UnknownWord,
};
