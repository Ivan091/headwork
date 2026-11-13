//! Everything headwork does to the outside world: process trees, the terminal host, the Claude runtime and worktrees.
//!
//! Each concern sits behind a small type or trait, so the binary's lifecycle code can be tested with
//! [`host::fake::FakeHost`] and temporary folders.

pub mod host;
pub mod proc;
pub mod runtime;
pub mod worktree;
