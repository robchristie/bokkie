//! Codex: the per-user app servers, the household's ChatGPT sign-in they
//! share, and the checks that hold every thread to the capability rule:
//! no Codex built-in tool until Phase 4's sandbox gate passes, and only the
//! Bokkie tools of the grants a thread started with.

pub mod catalog;
pub mod config;
pub mod guard;
pub mod household;
pub mod supervisor;

pub use household::HouseholdSettings;
pub use supervisor::{
    CodexError, HouseholdStatus, Launcher, Owner, Runtime, RuntimeEvent, RuntimeEventKind,
    Supervisor, ToolReply,
};
