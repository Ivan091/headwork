//! The internals of the `headwork` binary: the home folder and the operations behind each command.
//!
//! They live in a library so the operations can be tested in-process against the fake terminal host; nothing outside
//! this package uses it.

pub mod home;
pub mod ops;
pub mod version;
