//! Wasm-safe wire contract for Bokkie's authoritative operator projection.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Version of the HTTP contract consumed by the bundled operator UI.
pub const API_CONTRACT_VERSION: u32 = 1;
/// Exact SQLite migration version understood by this build of the UI.
pub const SUPPORTED_SCHEMA_VERSION: i64 = 9;
/// Stable package identity; the per-process session ID distinguishes restarts.
pub const BOKKIE_BUILD_ID: &str = concat!("bokkie/", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceIdentity {
    pub build: String,
    pub api_contract_version: u32,
    pub schema_version: i64,
    pub process_id: u32,
    pub session_id: String,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionBootstrap {
    pub service: ServiceIdentity,
    pub mutation_token: String,
}

impl std::fmt::Debug for SessionBootstrap {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionBootstrap")
            .field("service", &self.service)
            .field("mutation_token", &"[REDACTED]")
            .finish()
    }
}

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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorFailureDisposition {
    RetrySafe,
    NeedsReconciliation,
    HumanDecision,
    Terminal,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalSubject {
    Generic,
    GardenerProposal {
        repository: String,
        /// Stable goal identity retained across source-bound generations.
        fingerprint: String,
        #[serde(default)]
        instance_id: String,
        #[serde(default)]
        generation: u32,
        #[serde(default)]
        source_commit: String,
        #[serde(default)]
        source_observation_id: i64,
        #[serde(default)]
        source_inspection_id: String,
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
/// obligation. Exact gardener decisions bind the stable goal fingerprint and
/// every field identifying its immutable source-bound proposal instance;
/// ordinary lifecycle actions leave all gardener fields unset.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActionPrecondition {
    pub obligation_id: String,
    pub occurrence: u32,
    pub state_revision: i64,
    pub gardener_fingerprint: Option<String>,
    #[serde(default)]
    pub gardener_proposal_instance_id: Option<String>,
    #[serde(default)]
    pub gardener_source_commit: Option<String>,
    #[serde(default)]
    pub gardener_source_observation_id: Option<i64>,
    #[serde(default)]
    pub gardener_source_inspection_id: Option<String>,
    #[serde(default)]
    pub gardener_generation: Option<u32>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_disposition: Option<OperatorFailureDisposition>,
    pub created_at: i64,
    pub updated_at: i64,
    pub exception: Option<ExceptionReason>,
    pub liveness: Option<DurableLiveness>,
    pub capabilities: OperatorCapabilities,
}

/// A single affected-obligation projection tied to both the serving process
/// and the exact durable global event-envelope revision used for the read.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperatorObligationProjection {
    pub service: ServiceIdentity,
    pub watermark: i64,
    pub obligation: OperatorObligation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperatorSnapshot {
    pub captured_at: i64,
    /// HTTP handlers populate this process identity; Store-only projections omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<ServiceIdentity>,
    /// Opaque continuation identity, bound to this projection and watermark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Exact durable global event-envelope sequence for this snapshot walk.
    #[serde(default)]
    pub watermark: i64,
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
    GardenerProposalInstance,
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
    /// HTTP handlers populate this process identity; Store-only projections omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<ServiceIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub watermark: i64,
    pub items: Vec<TopicItem>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionEventProvenance {
    LegacyNonCausal,
    LiveAppend,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectionEventSource {
    AuditEvent { sequence: i64 },
    GardenerEvent { sequence: i64 },
    GardenerRunEvent { sequence: i64 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectionChange {
    pub revision: i64,
    pub provenance: ProjectionEventProvenance,
    pub source: ProjectionEventSource,
    pub event_type: String,
    pub occurred_at: i64,
    pub obligation_id: Option<String>,
    pub occurrence: Option<u32>,
    pub repository: Option<String>,
    pub inspection_id: Option<String>,
    pub proposal_fingerprint: Option<String>,
    pub proposal_instance_id: Option<String>,
    pub run_id: Option<String>,
}

/// Incremental invalidations use durable envelope revisions, independently of
/// process identity and mutation state revisions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectionChangePage {
    pub service: ServiceIdentity,
    pub requested_after: i64,
    pub requested_through: Option<i64>,
    pub next_after: Option<i64>,
    pub watermark: i64,
    pub changes: Vec<ProjectionChange>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_topic_without_service_identity_remains_deserialisable() {
        let topic: ObligationTopic = serde_json::from_value(serde_json::json!({
            "captured_at": 100,
            "obligation_id": "obligation-1",
            "items": []
        }))
        .unwrap();

        assert!(topic.service.is_none());
        assert_eq!(topic.watermark, 0);
    }

    #[test]
    fn legacy_gardener_wire_shapes_remain_deserialisable() {
        let subject: ApprovalSubject = serde_json::from_value(serde_json::json!({
            "kind": "gardener_proposal",
            "repository": "robchristie/bokkie",
            "fingerprint": "stable-goal",
            "prompt": "Implement the reviewed goal",
            "obligation_id": "implementation",
            "occurrence": 1
        }))
        .unwrap();
        assert!(matches!(
            subject,
            ApprovalSubject::GardenerProposal {
                instance_id,
                generation: 0,
                source_commit,
                source_observation_id: 0,
                source_inspection_id,
                ..
            } if instance_id.is_empty()
                && source_commit.is_empty()
                && source_inspection_id.is_empty()
        ));

        let precondition: ActionPrecondition = serde_json::from_value(serde_json::json!({
            "obligation_id": "implementation",
            "occurrence": 1,
            "state_revision": 7,
            "gardener_fingerprint": "stable-goal"
        }))
        .unwrap();
        assert!(precondition.gardener_proposal_instance_id.is_none());
        assert!(precondition.gardener_source_commit.is_none());
        assert!(precondition.gardener_source_observation_id.is_none());
        assert!(precondition.gardener_source_inspection_id.is_none());
        assert!(precondition.gardener_generation.is_none());
    }
}
