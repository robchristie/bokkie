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
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagedCataloguePage {
    pub items: Vec<ManagedCatalogueEntry>,
    pub next_after: Option<String>,
}
