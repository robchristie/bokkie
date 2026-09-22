//! Persistent conversation is a drafting surface; review cards require operator action.
use crate::{
    ManagedCatalogueEntry, ManagedTaskDetail, ManagedTaskPreview, ManagedTaskReceipt,
    ServiceIdentity,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationTurnRequest {
    pub command_id: String,
    pub conversation_id: String,
    pub expected_revision: i64,
    pub text: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationSelectRequest {
    pub command_id: String,
    pub conversation_id: String,
    pub expected_revision: i64,
    pub task_id: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationConfirmRequest {
    pub command_id: String,
    pub conversation_id: String,
    pub proposal_id: String,
    pub session_id: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub role: String,
    pub text: String,
    pub request_id: String,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationAction {
    Activate,
    Pause,
    Resume,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConversationReview {
    pub id: String,
    pub action: ConversationAction,
    pub task_id: String,
    pub configuration_revision: i64,
    pub session_id: String,
    pub preview: Option<ManagedTaskPreview>,
    pub explanation: String,
    pub blockers: Vec<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConversationView {
    pub service: ServiceIdentity,
    pub id: String,
    pub revision: i64,
    pub selected_task_id: Option<String>,
    pub messages: Vec<ConversationMessage>,
    pub candidates: Vec<ManagedCatalogueEntry>,
    pub review: Option<ConversationReview>,
    pub task: Option<ManagedTaskDetail>,
    pub busy: bool,
    pub request_error: Option<String>,
    pub runtime_available: bool,
    pub notes_available: bool,
    pub receipt: Option<ManagedTaskReceipt>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConversationSummary {
    pub id: String,
    pub selected_task_id: Option<String>,
    pub last_text: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConversationList {
    pub service: ServiceIdentity,
    pub items: Vec<ConversationSummary>,
}
