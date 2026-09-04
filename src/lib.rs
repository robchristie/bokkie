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
pub mod events;
pub mod execution_lane;
pub mod gardener;
pub mod gardener_runner;
pub mod git_workspace;
pub mod http;
pub mod http_security;
pub mod migrations;
pub mod operator;
pub mod pagination;
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
    ApprovalDecision, Attempt, AttemptOutcome, AuditEvent, Claim, Completion, FailureDisposition,
    MAX_APPROVAL_ACTOR_CHARS, MAX_APPROVAL_NOTE_CHARS, MAX_AUDIT_DETAILS_BYTES,
    MAX_AUDIT_EVENT_TYPE_CHARS, MAX_COMPLETION_ERROR_CHARS, MAX_COMPLETION_EVIDENCE_CHARS,
    MAX_OBLIGATION_DESCRIPTION_CHARS, MAX_OBLIGATION_ID_CHARS, MAX_RECURRENCE_EXPRESSION_CHARS,
    MAX_RECURRENCE_TIMEZONE_CHARS, NewObligation, Obligation, ObligationState, RetryPolicy,
};
pub use events::{
    ChangeRecord, EventEnvelope, EventProvenance, EventSource, MAX_CHANGE_PAGE_SIZE, Page,
};
pub use execution_lane::ExecutionLane;
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
pub use pagination::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, ReadPage, page_limit};
pub use recurrence::Recurrence;
pub use runner::{FakeOutcome, FakeRunner, RunResult, Runner, run_one};
pub use runtime_trust::{
    ChildEnvironment, ExecutableIdentity, ExecutableRole, GardenerExecutableIdentities,
    GitHubCredential, ProcessPolicy, RuntimeTrustError,
};
pub use store::{ManualClock, Store, StoreError, SystemClock, UnixClock};
