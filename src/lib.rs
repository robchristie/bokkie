//! Durable obligation lifecycle kernel.
//!
//! SQLite rows are the authoritative projection. All lifecycle mutations pass
//! through [`Store`], which changes the projection and appends its audit event
//! in the same transaction. Runners receive an already-persisted claim and are
//! deliberately invoked outside those transactions.

pub mod app_server;
pub mod db_executor;
pub mod doctor;
pub mod domain;
pub mod gardener;
pub mod gardener_runner;
pub mod git_workspace;
pub mod http;
pub mod migrations;
pub mod operator;
pub mod process;
pub mod recurrence;
pub mod runner;
pub mod runtime_trust;
pub mod service;
pub mod store;

pub use bokkie_operator_api::*;
pub use db_executor::{DbExecutor, DbExecutorError};
pub use doctor::{
    CommandExternalObserver, DoctorError, DoctorOptions, DoctorReport, NoExternalObserver,
    run_doctor,
};
pub use domain::{
    ApprovalDecision, Attempt, AttemptOutcome, AuditEvent, Claim, Completion, NewObligation,
    Obligation, ObligationState, RetryPolicy,
};
pub use gardener::{
    CANONICAL_DEFAULT_BRANCH, CANONICAL_REPOSITORY, GardenerCandidateQualification, GardenerEvent,
    GardenerImplementationResult, GardenerImplementationRun, GardenerInspection,
    GardenerObligationKind, GardenerPublicationState, GardenerReproducibilityManifest,
    GardenerRunEvent, GardenerRunPhase, GardenerVerificationResult, GardenerVerificationVerdict,
    InspectionResult, NewGardenerImplementationRun, NewGardenerInspection,
    NewRepositoryRegistration, Proposal, ProposalObservation, RepositoryRegistration,
    normalise_goal_prompt, proposal_fingerprint,
};
pub use gardener_runner::{GardenerRunner, GardenerRunnerError, GardenerRuntimeConfig};
pub use migrations::{MigrationManifestEntry, migration_manifest};
pub use recurrence::Recurrence;
pub use runner::{FakeOutcome, FakeRunner, RunResult, Runner, run_one};
pub use runtime_trust::{
    ChildEnvironment, ExecutableIdentity, ExecutableRole, GardenerExecutableIdentities,
    GitHubCredential, ProcessPolicy, RuntimeTrustError,
};
pub use store::{ManualClock, Store, StoreError, SystemClock, UnixClock};
