//! Versioned, reviewed task definitions shared by local adapters.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManagedTrigger {
    Immediate,
    Once {
        local_datetime: String,
        timezone: String,
    },
    Recurring {
        cron: String,
        timezone: String,
    },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedTaskDefinition {
    pub name: String,
    pub purpose: String,
    pub instructions: String,
    pub context_refs: Vec<String>,
    pub trigger: ManagedTrigger,
    pub capability: String,
    pub profile_revision: String,
    pub effects: Vec<String>,
    pub max_attempts: u32,
    pub max_output_chars: u32,
    pub destination: String,
}
impl ManagedTaskDefinition {
    pub fn reminder(
        name: impl Into<String>,
        instructions: impl Into<String>,
        destination: impl Into<String>,
    ) -> Self {
        let mut definition = Self::local_note(name, instructions);
        definition.capability = "reminder".into();
        definition.profile_revision = "reminder-v1".into();
        definition.effects.push("send_notification".into());
        definition.destination = destination.into();
        definition
    }
    pub fn local_note(name: impl Into<String>, instructions: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            purpose: name.clone(),
            name,
            instructions: instructions.into(),
            context_refs: vec![],
            trigger: ManagedTrigger::Immediate,
            capability: "local_note".into(),
            profile_revision: "local-note-v1".into(),
            effects: vec!["store_local_result".into()],
            max_attempts: 3,
            max_output_chars: 8192,
            destination: "task_results".into(),
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagedDefinitionRevision {
    pub revision: i64,
    pub definition: ManagedTaskDefinition,
    pub created_at: i64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedTaskStatus {
    Draft,
    Active,
    Paused,
    Completed,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagedRun {
    pub obligation_id: String,
    pub definition_revision: i64,
    pub profile_revision: String,
    pub scheduled_at: i64,
    pub admitted_at: Option<i64>,
    pub state: String,
    pub result: Option<String>,
    #[serde(default = "default_run_timezone")]
    pub timezone: String,
    #[serde(default)]
    pub delivery: Option<ManagedDelivery>,
}
fn default_run_timezone() -> String {
    "Australia/Adelaide".into()
}

/// Delivery is independent of the local occurrence's successful result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagedDelivery {
    pub id: String,
    pub status: String,
    pub detail: String,
    pub destination: String,
    pub subject: String,
    pub body: String,
    pub next_retry_at: Option<i64>,
    pub attempts: Vec<ManagedDeliveryAttempt>,
    /// Present only when operator reconciliation is legal at this exact revision.
    pub recovery: Option<crate::ActionPrecondition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<crate::ManagedPushDelivery>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagedDeliveryAttempt {
    pub attempt_number: u32,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub outcome: String,
    pub detail: Option<String>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationRecovery {
    MarkReconciled,
    RetryAcknowledgingDuplicateRisk,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationRecoveryRequest {
    pub precondition: crate::ActionPrecondition,
    pub action: NotificationRecovery,
    pub note: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagedTaskDetail {
    pub id: String,
    pub configuration_revision: i64,
    pub status: ManagedTaskStatus,
    pub active: Option<ManagedDefinitionRevision>,
    pub candidate: Option<ManagedDefinitionRevision>,
    pub next_wake_at: Option<i64>,
    pub runs: Vec<ManagedRun>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagedCapabilityProfile {
    pub capability: String,
    pub revision: String,
    pub available: bool,
    pub effects: Vec<String>,
    pub destination: String,
    pub max_attempts: u32,
    pub max_output_chars: u32,
}
impl ManagedCapabilityProfile {
    pub fn web_push(id: &str, label: &str) -> Self {
        let mut profile = Self::reminder(format!("Bokkie on {label}"));
        profile.revision = format!("reminder-web-push-v1/{id}");
        profile.max_output_chars = 2000;
        profile
    }
    pub fn reminder(destination: impl Into<String>) -> Self {
        Self {
            capability: "reminder".into(),
            revision: "reminder-v1".into(),
            available: true,
            effects: vec!["store_local_result".into(), "send_notification".into()],
            destination: destination.into(),
            max_attempts: 5,
            max_output_chars: 16384,
        }
    }
    pub fn local_note() -> Self {
        Self {
            capability: "local_note".into(),
            revision: "local-note-v1".into(),
            available: true,
            effects: vec!["store_local_result".into()],
            destination: "task_results".into(),
            max_attempts: 5,
            max_output_chars: 16384,
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagedTaskPreview {
    pub task_id: String,
    pub configuration_revision: i64,
    pub candidate_revision: i64,
    pub session_id: String,
    pub profile_revision: String,
    pub profile: Option<ManagedCapabilityProfile>,
    pub definition: ManagedTaskDefinition,
    pub blockers: Vec<String>,
    pub changes: Vec<String>,
    pub occurrences: Vec<i64>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagedTaskReceipt {
    pub command_id: String,
    pub task_id: String,
    pub configuration_revision: i64,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagedCatalogueEntry {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub description: String,
    pub status: String,
    #[serde(default)]
    pub summary: Option<ManagedCatalogueSummary>,
}
/// Timing and results from the authoritative task catalogue, never another list.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagedCatalogueSummary {
    pub next_at: Option<i64>,
    pub timezone: String,
    pub latest_result: Option<String>,
    pub latest_result_at: Option<i64>,
    pub needs_input: bool,
    pub conversation_id: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagedCataloguePage {
    pub items: Vec<ManagedCatalogueEntry>,
    pub next_after: Option<String>,
}
