//! Codex: the per-user app servers and the checks that keep threads
//! chat-only until Phase 4's sandbox gate passes.

pub mod catalog;
pub mod config;
pub mod guard;
pub mod supervisor;

pub use supervisor::{CodexError, Launcher, Runtime, RuntimeEvent, RuntimeEventKind, Supervisor};
