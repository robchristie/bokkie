//! Optional recall data. Memory never grants execution or confirmation authority.
use crate::ServiceIdentity;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Preference,
    TaskOutcome,
    Decision,
    OperationalKnowledge,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryProvenance {
    Explicit,
    Inferred,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySource {
    pub reference: String,
    pub context: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub revision: i64,
    pub kind: MemoryKind,
    pub provenance: MemoryProvenance,
    pub content: Option<String>,
    pub sources: Vec<MemorySource>,
    pub task_id: Option<String>,
    pub corrected: bool,
    pub removed: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryMutation {
    Create {
        kind: MemoryKind,
        provenance: MemoryProvenance,
        content: String,
        sources: Vec<MemorySource>,
        task_id: Option<String>,
    },
    Correct {
        content: String,
    },
    Remove,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryCommandRequest {
    pub command_id: String,
    pub entry_id: Option<String>,
    pub expected_revision: i64,
    pub mutation: MemoryMutation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoryList {
    pub service: ServiceIdentity,
    pub entries: Vec<MemoryEntry>,
    pub next_after: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemorySaved {
    pub service: ServiceIdentity,
    pub entry: MemoryEntry,
}
