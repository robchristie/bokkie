//! Runtime-neutral engineering supervision contracts and durable evidence.
use crate::{Claim, Obligation};
use serde::{Deserialize, Serialize};

/// Trusted adapter identity. Deliberately cannot be deserialised from a request.
#[derive(Debug, Clone, Serialize)]
pub enum EngineeringActor {
    Operator { name: String },
    Supervisor { execution_id: String, claim: Claim },
    Worker { execution_id: String, claim: Claim },
    Reconciler { adapter_id: String },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineeringRole {
    Supervisor,
    Worker,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineeringBudget {
    pub max_turns: u32,
    pub max_packages: u32,
    pub max_repairs: u32,
    pub max_recoveries: u32,
    pub max_checkpoints: u32,
    pub max_questions: u32,
    pub max_concurrent_workers: u32,
    pub turn_seconds: i64,
    pub deadline: i64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineeringInstructions {
    pub text: String,
    pub digest: String,
    /// Exact identities of applicable instructions, skills and context artefacts.
    pub context_digests: Vec<String>,
    pub profile_digest: String,
    pub adapter_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineeringCriterion {
    pub id: String,
    pub description: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineeringContract {
    pub intent: String,
    pub criteria: Vec<EngineeringCriterion>,
    pub permitted_scope: Vec<String>,
    pub prohibited_effects: Vec<String>,
    /// Explicit authority decisions, each bound to retained decision evidence.
    pub authority: Vec<EngineeringAuthority>,
    pub supervisor: EngineeringInstructions,
    pub worker: EngineeringInstructions,
    pub budget: EngineeringBudget,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineeringAuthority {
    pub grant: String,
    pub evidence_digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineeringPrecondition {
    pub outcome_id: String,
    pub contract_revision: u64,
    /// Monotonic outcome watermark; includes messages and incoming evidence.
    pub state_revision: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineeringCommandEnvelope {
    pub command_id: String,
    pub expected: Option<EngineeringPrecondition>,
    pub command: EngineeringCommand,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "input",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum EngineeringCommand {
    CreateOutcome {
        contract: EngineeringContract,
    },
    FollowUp {
        text: String,
    },
    ReviseContract {
        contract: EngineeringContract,
    },
    FormaliseContract {
        contract: EngineeringContract,
    },
    CreatePackage(NewEngineeringPackage),
    RecordCheckpoint {
        runtime_identity: String,
        request_identity: Option<String>,
        cursor: String,
        summary: String,
        evidence_digest: String,
    },
    AskQuestion {
        request_key: String,
        kind: EngineeringQuestionKind,
        prompt: String,
        options: Vec<String>,
    },
    ResolveQuestion {
        question_id: String,
        answer: String,
        authority_grants: Vec<String>,
    },
    SubmitResult(EngineeringSubmissionInput),
    AssessResult(EngineeringAssessmentInput),
    CreateRepair {
        assessment_id: String,
        replacement: NewEngineeringPackage,
    },
    YieldSupervisor {
        next_wake_at: i64,
        reason: String,
        processed_message_count: usize,
    },
    RequestCancellation {
        package_id: Option<String>,
    },
    RecordReconciliation(EngineeringReconciliationInput),
    FinishOutcome {
        assessment_ids: Vec<String>,
        review: EngineeringReviewEvidence,
        processed_message_count: usize,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewEngineeringPackage {
    pub parent_id: Option<String>,
    pub dependencies: Vec<String>,
    pub instructions: String,
    pub criteria: Vec<String>,
    pub inputs: Vec<EngineeringArtefact>,
    /// Adapter-issued canonical identity, never inferred from an arbitrary path.
    pub workspace: String,
    pub budget: EngineeringBudget,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EngineeringArtefact {
    Git {
        repository: String,
        commit: String,
        tree: String,
    },
    File {
        store: String,
        path: String,
        bytes: u64,
        sha256: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineeringCriterionEvidence {
    pub criterion_id: String,
    pub artefact: EngineeringArtefact,
    pub command_digest: String,
    pub output_digest: String,
    pub exit_code: i32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineeringSubmissionInput {
    pub artefacts: Vec<EngineeringArtefact>,
    pub evidence: Vec<EngineeringCriterionEvidence>,
    pub limitations: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineeringReviewEvidence {
    pub reviewer_identity: String,
    pub artefacts: Vec<EngineeringArtefact>,
    pub evidence_digest: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineeringVerdict {
    Accept,
    Repair,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineeringAssessmentInput {
    pub submission_id: String,
    pub submission_digest: String,
    pub verdict: EngineeringVerdict,
    pub unmet_criteria: Vec<String>,
    pub review: EngineeringReviewEvidence,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineeringReconciliationInput {
    /// Trusted adapter failure; parks responsibility until new evidence or configuration.
    #[serde(default)]
    pub runtime_failure: Option<String>,
    /// Trusted proof that dispatch did not create an external boundary.
    #[serde(default)]
    pub not_started: bool,
    pub execution_id: String,
    pub runtime_identity: String,
    pub observation: String,
    pub evidence_digest: String,
    /// Only the trusted adapter may attest that its containment boundary was reaped.
    pub reaped_boundary: Option<String>,
    pub recovered_submission: Option<EngineeringSubmissionInput>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineeringQuestionKind {
    Routine,
    /// A fact the supervisor cannot obtain through its authorised tools or context.
    MissingInformation,
    NewAuthority,
}
impl EngineeringQuestionKind {
    pub fn needs_operator(self) -> bool {
        matches!(self, Self::MissingInformation | Self::NewAuthority)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineeringCommandReceipt {
    pub command_id: String,
    pub payload_digest: String,
    pub outcome_id: String,
    pub contract_revision: u64,
    pub state_revision: u64,
    pub record_id: Option<String>,
    pub event_sequence: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineeringClaim {
    pub claim: Claim,
    pub execution_id: String,
    pub outcome_id: String,
    pub package_id: Option<String>,
    pub contract_revision: u64,
    pub state_revision: u64,
    pub instructions: EngineeringInstructions,
    pub workspace: Option<String>,
    pub dispatch_key: String,
    pub remaining_turns: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineeringMessage {
    pub text: String,
    pub actor: String,
    pub at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineeringContractRevision {
    pub revision: u64,
    pub contract: EngineeringContract,
    pub actor: String,
    pub at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineeringPackage {
    pub id: String,
    pub obligation_id: String,
    pub contract_revision: u64,
    pub input: NewEngineeringPackage,
    pub superseded_by: Option<String>,
    pub cancellation_requested: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineeringExecution {
    pub id: String,
    pub obligation_id: String,
    pub package_id: Option<String>,
    pub contract_revision: u64,
    pub role: EngineeringRole,
    pub claim: Claim,
    pub dispatch_key: String,
    pub instructions: EngineeringInstructions,
    pub workspace: Option<String>,
    pub fenced: bool,
    pub cessation_verified: bool,
    #[serde(default)]
    pub recovery_required: bool,
    #[serde(default)]
    pub recovery_charged: bool,
    pub checkpoints: Vec<EngineeringCheckpoint>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineeringCheckpoint {
    pub sequence: u64,
    pub runtime_identity: String,
    pub request_identity: Option<String>,
    pub cursor: String,
    pub summary: String,
    pub evidence_digest: String,
    pub at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineeringQuestion {
    pub id: String,
    pub execution_id: String,
    pub contract_revision: u64,
    pub request_key: String,
    pub kind: EngineeringQuestionKind,
    pub prompt: String,
    pub options: Vec<String>,
    pub resolution: Option<EngineeringResolution>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineeringResolution {
    pub answer: String,
    pub actor: String,
    pub authority_grants: Vec<String>,
    pub contract_revision: u64,
    pub at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineeringSubmission {
    pub id: String,
    pub execution_id: String,
    pub package_id: String,
    pub contract_revision: u64,
    pub digest: String,
    pub input: EngineeringSubmissionInput,
    pub recovered: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineeringAssessment {
    pub id: String,
    pub assessor_execution_id: String,
    pub contract_revision: u64,
    pub input: EngineeringAssessmentInput,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineeringRepair {
    pub assessment_id: String,
    pub replaced_package_id: String,
    pub replacement_package_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineeringReconciliation {
    pub adapter_id: String,
    pub input: EngineeringReconciliationInput,
    pub at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineeringAcceptance {
    pub assessor_execution_id: String,
    pub contract_revision: u64,
    pub assessment_ids: Vec<String>,
    pub review: EngineeringReviewEvidence,
    pub at: i64,
}
/// Materialised projection. Immutable commands, dispatches and audit events retain history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineeringOutcomeSnapshot {
    pub id: String,
    pub root: Obligation,
    pub state_revision: u64,
    pub contract_revision: u64,
    pub contracts: Vec<EngineeringContractRevision>,
    pub messages: Vec<EngineeringMessage>,
    pub processed_message_count: usize,
    pub packages: Vec<EngineeringPackage>,
    pub executions: Vec<EngineeringExecution>,
    pub questions: Vec<EngineeringQuestion>,
    pub submissions: Vec<EngineeringSubmission>,
    pub assessments: Vec<EngineeringAssessment>,
    pub repairs: Vec<EngineeringRepair>,
    pub reconciliations: Vec<EngineeringReconciliation>,
    pub turns_used: u32,
    pub recoveries_used: u32,
    pub cancellation_requested: bool,
    pub acceptance: Option<EngineeringAcceptance>,
}
impl EngineeringOutcomeSnapshot {
    pub fn contract(&self) -> &EngineeringContract {
        &self
            .contracts
            .last()
            .expect("outcome has contract")
            .contract
    }
    pub fn precondition(&self) -> EngineeringPrecondition {
        EngineeringPrecondition {
            outcome_id: self.id.clone(),
            contract_revision: self.contract_revision,
            state_revision: self.state_revision,
        }
    }
}
