//! Address-book destinations and hand-off snapshots are separate from execution tasks.
use crate::ServiceIdentity;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectRegistration {
    pub name: String,
    pub host: String,
    pub workspace: String,
    pub codex_project_id: Option<String>,
    pub codex_host_id: Option<String>,
    pub context: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProjectDestination {
    pub id: String,
    pub revision: i64,
    pub registration: ProjectRegistration,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSaveRequest {
    pub command_id: String,
    pub project_id: String,
    pub expected_revision: i64,
    pub registration: ProjectRegistration,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProjectList {
    pub service: ServiceIdentity,
    pub items: Vec<ProjectDestination>,
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffBrief {
    pub outcome: String,
    pub context: String,
    pub constraints: String,
    pub acceptance: String,
    pub references: Vec<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HandoffDraft {
    pub id: String,
    pub conversation_id: String,
    pub source_request_id: String,
    pub project_query: String,
    pub brief: HandoffBrief,
    pub candidates: Vec<ProjectDestination>,
    pub saved_revision: i64,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffSaveRequest {
    pub command_id: String,
    pub draft_id: String,
    pub expected_revision: i64,
    pub project_id: String,
    pub project_revision: i64,
    pub brief: HandoffBrief,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HandoffSnapshot {
    pub id: String,
    pub revision: i64,
    pub conversation_id: String,
    pub source_request_id: String,
    pub project: ProjectDestination,
    pub brief: HandoffBrief,
    pub created_at: i64,
    pub return_path: String,
    pub complete_brief: String,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffActivityKind {
    CopyRequested,
    CopySucceeded,
    CopyFailed,
    ManualOpeningViewed,
    OpeningProblemReported,
    ResultNote,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffActivityRequest {
    pub command_id: String,
    pub handoff_id: String,
    pub revision: i64,
    pub kind: HandoffActivityKind,
    pub note: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HandoffActivity {
    pub kind: HandoffActivityKind,
    pub note: String,
    pub provenance: String,
    pub created_at: i64,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HandoffView {
    pub service: ServiceIdentity,
    pub snapshot: HandoffSnapshot,
    pub activities: Vec<HandoffActivity>,
    pub previous_revision: Option<i64>,
    pub latest_revision: i64,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HandoffList {
    pub service: ServiceIdentity,
    pub items: Vec<HandoffSnapshot>,
    pub next_after: Option<String>,
}
