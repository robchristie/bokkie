//! Codex: the per-user app servers, the household's ChatGPT sign-in they
//! share, and the checks that keep threads chat-only until Phase 4's
//! sandbox gate passes.

pub mod catalog;
pub mod config;
pub mod guard;
pub mod household;
pub mod supervisor;

pub use household::HouseholdSettings;
pub use supervisor::{
    CodexError, HouseholdStatus, Launcher, Owner, Runtime, RuntimeEvent, RuntimeEventKind,
    Supervisor,
};
