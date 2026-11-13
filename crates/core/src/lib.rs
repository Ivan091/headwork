//! The pure core of headwork: the lifecycle state machine, its guards and the config model.
//!
//! Nothing here performs I/O; the store and the binary feed it data and act on its decisions.

pub mod admission;
pub mod config;
pub mod lifecycle;
pub mod names;
pub mod vocabulary;

pub use admission::{AdmissionRefusal, Admitted, TaskRequest, admit};
pub use config::{Config, ConfigError, ProjectConfig, ProjectOverlay};
pub use lifecycle::{Refusal, Step, TaskSnapshot, Trigger, apply};
pub use names::{NameError, ProjectName, Slug, TaskName};
pub use vocabulary::{
    Actor, AskKind, AskState, MachineMode, Model, Role, SessionState, TaskState, UnknownWord,
};
