//! Workspace assignments share managed definitions; host events are observations.
use crate::{HandoffBrief, ProjectDestination};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTaskDefinition {
    pub project: ProjectDestination,
    pub brief: HandoffBrief,
    pub criteria: Vec<WorkspaceCriterion>,
    pub permitted_actions: Vec<String>,
    pub decision_rules: String,
    pub limits: WorkspaceLimits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_retained_work: Option<WorkspaceEvidenceReview>,
}

/// An explicit acceptance revision inspects one already ceased delivery. It
/// cannot launch workspace implementation or change the original report.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceEvidenceReview {
    pub source: WorkspaceEvidenceSource,
    pub summary: String,
    pub criteria: Vec<WorkspaceCriterionResult>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceEvidenceSource {
    pub execution_id: String,
    pub result_digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCriterion {
    pub id: String,
    pub description: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceLimits {
    pub max_seconds: u32,
    pub max_turns: u32,
    pub max_tokens: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePolicy {
    pub host_name: String,
    pub project_id: String,
    pub permitted_actions: Vec<String>,
    pub limits: WorkspaceLimits,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceDispatch {
    pub execution_id: String,
    pub task_id: String,
    pub obligation_id: String,
    pub definition_revision: i64,
    pub profile_revision: String,
    pub admitted_at: i64,
    pub deadline_at: i64,
    pub assignment: WorkspaceTaskDefinition,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceHostEvent {
    pub execution_id: String,
    pub sequence: i64,
    pub event: WorkspaceEvent,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceEvent {
    Started {
        runtime_id: String,
        instruction_sources: Vec<String>,
    },
    Progress {
        summary: String,
    },
    Question {
        question: WorkspaceQuestion,
    },
    Attention {
        reason: String,
    },
    Stopped {
        cessation: WorkspaceCessation,
        result: Option<WorkspaceResult>,
        verification: Option<WorkspaceVerification>,
        reason: String,
    },
    RecoveredResult {
        result: WorkspaceResult,
        provenance: WorkspaceRecoveryProvenance,
        verification: Option<WorkspaceVerification>,
    },
}

/// A trusted host reconstructs delivery evidence after the original boundary
/// stopped. Digests bind retained sources without inventing a runtime receipt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRecoveryProvenance {
    pub origin: String,
    pub algorithm: String,
    pub recovered_at: i64,
    pub dispatch_digest: String,
    pub admission_digest: String,
    pub result_digest: String,
    pub cessation: WorkspaceCessation,
    pub sources: Vec<WorkspaceRecoverySource>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRecoverySource {
    pub kind: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceQuestion {
    pub id: String,
    pub kind: String,
    pub prompt: String,
    pub options: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCessation {
    pub boundary_id: String,
    pub kind: String,
    pub evidence: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCriterionResult {
    pub id: String,
    pub satisfied: bool,
    pub evidence: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceDelivery {
    pub repository: String,
    pub pull_request: String,
    pub reviewed_head: String,
    pub merge_revision: String,
    pub tree: String,
    pub checks: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceResult {
    pub summary: String,
    pub criteria: Vec<WorkspaceCriterionResult>,
    pub deliveries: Vec<WorkspaceDelivery>,
    pub limitations: Vec<String>,
}

/// The authenticated host supplies observations acquired outside the agent turn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceVerification {
    pub passed: bool,
    pub evidence: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceAnswer {
    pub question_id: String,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceControl {
    pub execution_id: String,
    pub cancel: bool,
    pub answers: Vec<WorkspaceAnswer>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceExchangeRequest {
    pub events: Vec<WorkspaceHostEvent>,
    pub heartbeats: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceAcknowledgement {
    pub execution_id: String,
    pub sequence: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceExchangeResponse {
    pub acknowledgements: Vec<WorkspaceAcknowledgement>,
    pub dispatches: Vec<WorkspaceDispatch>,
    pub controls: Vec<WorkspaceControl>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceRun {
    pub execution_id: String,
    pub status: String,
    pub progress: String,
    pub question: Option<WorkspaceQuestion>,
    pub result: Option<WorkspaceResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<WorkspaceRecoveryProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<WorkspaceVerification>,
    pub cessation_verified: bool,
    pub cancellation_requested: bool,
    pub last_event_sequence: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTaskEditRequest {
    pub command_id: String,
    pub conversation_id: String,
    pub configuration_revision: i64,
    pub definition: crate::ManagedTaskDefinition,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRunActionRequest {
    pub command_id: String,
    pub conversation_id: String,
    pub execution_id: String,
    pub expected_event_sequence: i64,
    pub answer: Option<WorkspaceAnswer>,
    pub cancel: bool,
}
