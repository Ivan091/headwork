//! Everything headwork does to the outside world: process trees, the terminal host, the Claude runtime and worktrees.
//!
//! Each concern sits behind a small type or trait, so the binary's lifecycle code can be tested with
//! [`host::fake::FakeHost`] and temporary folders.

pub mod disk;
pub mod gate;
pub mod git;
pub mod host;
pub mod notes;
pub mod proc;
pub mod release;
pub mod runtime;
pub mod shell;
pub mod ssh;
pub mod timed;
pub mod trust;
pub mod worktree;
