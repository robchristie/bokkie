//! Versioned conversational role configuration; no tool or approval grants.
use crate::ServiceIdentity;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRoleSettings {
    pub model: String,
    pub effort: String,
    pub additional_instructions: String,
    pub timeout_seconds: u64,
    pub max_context_bytes: usize,
    pub max_output_bytes: usize,
    pub max_model_calls: u8,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdviserRoleSettings {
    pub role: AgentRoleSettings,
    pub automatic_consultation: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentProfileRevision {
    pub revision: i64,
    pub contract_version: u8,
    pub main: AgentRoleSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adviser: Option<AdviserRoleSettings>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentModelOption {
    pub model: String,
    pub display_name: String,
    pub supported_efforts: Vec<String>,
    pub default_effort: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentSettingsView {
    pub service: ServiceIdentity,
    pub profile: Option<AgentProfileRevision>,
    pub models: Vec<AgentModelOption>,
    pub ceilings: Option<AgentRoleSettings>,
    pub effective: bool,
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSettingsSaveRequest {
    pub command_id: String,
    pub expected_revision: i64,
    pub main: AgentRoleSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adviser: Option<AdviserRoleSettings>,
}
