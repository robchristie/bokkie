//! Wasm-safe wire contract for Bokkie's authoritative operator projection.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorObligationState {
    Pending,
    AwaitingApproval,
    Running,
    RetryScheduled,
    Attention,
    Completed,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalSubject {
    Generic,
    GardenerProposal {
        repository: String,
        fingerprint: String,
        prompt: String,
        obligation_id: String,
        occurrence: u32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttentionCause {
    Rejected { actor: String, note: Option<String> },
    AttemptsExhausted,
    NonRetryableFailure,
    RecurrenceFailure,
    GardenerVerificationBlocking { summary: String },
    GardenerVerificationInconclusive { summary: String },
    PersistedFailure,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExceptionReason {
    AwaitingApproval {
        subject: ApprovalSubject,
    },
    ExpiredLease {
        token: String,
        generation: u64,
        expires_at: i64,
    },
    Attention {
        cause: AttentionCause,
        error: Option<String>,
        evidence: Option<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DurableLiveness {
    FutureWake {
        wake_at: i64,
    },
    ActiveLease {
        token: String,
        generation: u64,
        expires_at: i64,
    },
    HumanAttention {
        reason: ExceptionReason,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisabledReason {
    StateDoesNotPermit,
    RunningClaimOwnsObligation,
    TerminalObligation,
    GardenerProposalRequiresExactDecision,
    NotGardenerProposal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionConsequence {
    ScheduleCurrentOccurrence,
    MoveToAttention,
    ReopenForRetry,
    CancelObligation,
    ScheduleExactGardenerProposal,
    RejectExactGardenerProposal,
}

/// Immutable backend-issued condition for applying one projected lifecycle action.
///
/// `state_revision` is the latest append-only audit-event sequence for the
/// obligation. Exact gardener decisions also bind the immutable proposal
/// fingerprint; ordinary lifecycle actions leave it unset.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActionPrecondition {
    pub obligation_id: String,
    pub occurrence: u32,
    pub state_revision: i64,
    pub gardener_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActionCapability {
    pub available: bool,
    pub disabled_reason: Option<DisabledReason>,
    pub consequence: ActionConsequence,
    pub precondition: Option<ActionPrecondition>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperatorCapabilities {
    pub approve: ActionCapability,
    pub reject: ActionCapability,
    pub retry: ActionCapability,
    pub cancel: ActionCapability,
    pub approve_gardener_proposal: ActionCapability,
    pub reject_gardener_proposal: ActionCapability,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperatorObligation {
    pub id: String,
    pub description: String,
    pub state: OperatorObligationState,
    pub occurrence: u32,
    pub scheduled_at: i64,
    pub next_wake_at: Option<i64>,
    pub recurrence_cron: Option<String>,
    pub recurrence_timezone: Option<String>,
    pub approval_required: bool,
    pub attempts_made: u32,
    pub max_attempts: u32,
    pub retry_base_seconds: i64,
    pub retry_max_seconds: i64,
    pub last_error: Option<String>,
    pub last_evidence: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub exception: Option<ExceptionReason>,
    pub liveness: Option<DurableLiveness>,
    pub capabilities: OperatorCapabilities,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperatorSnapshot {
    pub captured_at: i64,
    /// Genuine exceptions first, followed by operational state, wake/update and ID.
    pub obligations: Vec<OperatorObligation>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TopicSource {
    AuditEvent,
    ApprovalDecision,
    Attempt,
    GardenerInspection,
    GardenerProposal,
    GardenerObservation,
    GardenerEvent,
    GardenerImplementationRun,
    GardenerRunEvent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TopicItem {
    pub occurred_at: i64,
    pub source: TopicSource,
    /// Monotonic source sequence where one exists, otherwise the immutable row identity.
    pub source_sequence: String,
    pub stable_id: String,
    pub occurrence: Option<u32>,
    pub event_type: String,
    /// Complete durable row, including raw details JSON where the source owns it.
    pub evidence: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObligationTopic {
    pub captured_at: i64,
    pub obligation_id: String,
    pub items: Vec<TopicItem>,
}
