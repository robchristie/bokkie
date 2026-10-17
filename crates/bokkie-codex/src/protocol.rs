//! Typed parameters, results, notifications and server requests.
//!
//! Outgoing types omit unset optional fields. Incoming types accept every
//! message the schema allows: a field is required here only when the schema
//! requires it, and unknown variants decode to an `Unknown` case rather than
//! failing. Incoming types also implement `Serialize` so that tests can prove
//! every field name exists in the schema.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::rpc::RequestId;

// ---------------------------------------------------------------------------
// Shared enums

/// When Codex asks for approval before running a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AskForApproval {
    Untrusted,
    OnRequest,
    Never,
}

/// The legacy sandbox modes. Bokkie prefers named permission profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

// ---------------------------------------------------------------------------
// initialize

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub client_info: ClientInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<InitializeCapabilities>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeCapabilities {
    /// Required for experimental methods and fields such as `dynamicTools`.
    pub experimental_api: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opt_out_notification_methods: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub codex_home: String,
    pub platform_family: String,
    pub platform_os: String,
    pub user_agent: String,
}

// ---------------------------------------------------------------------------
// model/list

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_hidden: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListResponse {
    pub data: Vec<Model>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub model: String,
    pub display_name: String,
    pub description: String,
    pub hidden: bool,
    pub is_default: bool,
}

// ---------------------------------------------------------------------------
// account/*

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetAccountParams {
    /// Forces a token refresh before answering.
    pub refresh_token: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetAccountResponse {
    pub account: Option<Account>,
    pub requires_openai_auth: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Account {
    ApiKey,
    #[serde(rename_all = "camelCase")]
    Chatgpt {
        email: Option<String>,
        plan_type: String,
    },
    #[serde(other)]
    Unknown,
}

/// Bokkie signs in only with the ChatGPT device-code flow.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LoginAccountParams {
    ChatgptDeviceCode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LoginAccountResponse {
    #[serde(rename_all = "camelCase")]
    ChatgptDeviceCode {
        login_id: String,
        user_code: String,
        verification_url: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelLoginAccountParams {
    pub login_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelLoginAccountResponse {
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetAccountRateLimitsResponse {
    pub rate_limits: RateLimitSnapshot,
    pub rate_limits_by_limit_id: Option<BTreeMap<String, RateLimitSnapshot>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitSnapshot {
    pub limit_id: Option<String>,
    pub limit_name: Option<String>,
    pub plan_type: Option<String>,
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
    pub rate_limit_reached_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitWindow {
    pub used_percent: i32,
    pub window_duration_mins: Option<i64>,
    pub resets_at: Option<i64>,
}

// ---------------------------------------------------------------------------
// config/read and permissionProfile/list

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigReadParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub include_layers: bool,
}

/// The effective configuration. Bokkie inspects it as JSON because only a few
/// keys matter and the full `Config` type changes often.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigReadResponse {
    pub config: Value,
    pub origins: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionProfileListParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionProfileListResponse {
    pub data: Vec<PermissionProfileSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionProfileSummary {
    pub id: String,
    pub allowed: bool,
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// thread/start and thread/resume

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<AskForApproval>,
    /// A named permission profile. Codex rejects it together with `sandbox`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub developer_instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ephemeral: Option<bool>,
    /// Experimental: needs `experimentalApi` at initialize.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_tools: Option<Vec<DynamicToolSpec>>,
    /// Per-thread configuration overrides, as dotted keys.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Map<String, Value>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadResumeParams {
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<AskForApproval>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub developer_instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Map<String, Value>>,
}

/// The result of `thread/start` and `thread/resume`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSessionResponse {
    pub thread: Thread,
    pub model: String,
    pub model_provider: String,
    pub cwd: String,
    pub approval_policy: Value,
    /// The effective `SandboxPolicy`, kept as JSON for startup checks.
    pub sandbox: Value,
    pub active_permission_profile: Option<ActivePermissionProfile>,
}

pub type ThreadStartResponse = ThreadSessionResponse;
pub type ThreadResumeResponse = ThreadSessionResponse;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivePermissionProfile {
    pub id: String,
    pub extends: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    pub session_id: String,
    pub preview: String,
    pub ephemeral: bool,
    pub model_provider: String,
    pub cwd: String,
    pub cli_version: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub path: Option<String>,
    pub turns: Vec<Turn>,
}

// ---------------------------------------------------------------------------
// turn/start and turn/interrupt

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartParams {
    pub thread_id: String,
    pub input: Vec<UserInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<AskForApproval>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl TurnStartParams {
    /// A turn with a single text message and no overrides.
    pub fn text(thread_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            thread_id: thread_id.into(),
            input: vec![UserInput::Text { text: text.into() }],
            cwd: None,
            approval_policy: None,
            permissions: None,
            model: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UserInput {
    Text { text: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartResponse {
    pub turn: Turn,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptParams {
    pub thread_id: String,
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnInterruptResponse {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: String,
    pub items: Vec<ThreadItem>,
    pub status: TurnStatus,
    pub error: Option<TurnError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    Completed,
    Interrupted,
    Failed,
    InProgress,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnError {
    pub message: String,
    pub additional_details: Option<String>,
}

/// The thread items Bokkie reads. Other item types decode to `Unknown`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ThreadItem {
    #[serde(rename_all = "camelCase")]
    UserMessage { id: String },
    #[serde(rename_all = "camelCase")]
    AgentMessage { id: String, text: String },
    #[serde(rename_all = "camelCase")]
    CommandExecution {
        id: String,
        command: String,
        cwd: String,
        status: String,
        aggregated_output: Option<String>,
        exit_code: Option<i32>,
    },
    #[serde(rename_all = "camelCase")]
    DynamicToolCall {
        id: String,
        tool: String,
        namespace: Option<String>,
        arguments: Value,
        status: String,
        success: Option<bool>,
    },
    #[serde(rename_all = "camelCase")]
    McpToolCall {
        id: String,
        server: String,
        tool: String,
        arguments: Value,
        status: String,
    },
    #[serde(other)]
    Unknown,
}

// ---------------------------------------------------------------------------
// command/exec

/// Runs one command in the app server's sandbox without a thread or turn.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecParams {
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// A named permission profile. Codex rejects it together with
    /// `sandboxPolicy`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<i64>,
    /// Required for `command/exec/terminate`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecResponse {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

// ---------------------------------------------------------------------------
// Dynamic tools (experimental)

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DynamicToolSpec {
    #[serde(rename_all = "camelCase")]
    Function {
        name: String,
        description: String,
        input_schema: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicToolCallParams {
    pub thread_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub tool: String,
    pub namespace: Option<String>,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicToolCallResponse {
    pub success: bool,
    pub content_items: Vec<DynamicToolCallOutputContentItem>,
}

impl DynamicToolCallResponse {
    pub fn text(success: bool, text: impl Into<String>) -> Self {
        Self {
            success,
            content_items: vec![DynamicToolCallOutputContentItem::InputText { text: text.into() }],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DynamicToolCallOutputContentItem {
    InputText { text: String },
}

// ---------------------------------------------------------------------------
// Approvals

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecutionRequestApprovalParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeRequestApprovalParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub reason: Option<String>,
}

/// The decisions Bokkie gives. Amendment decisions are not offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalDecision {
    Accept,
    AcceptForSession,
    Decline,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApprovalResponse {
    pub decision: ApprovalDecision,
}

// ---------------------------------------------------------------------------
// Notifications

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnNotification {
    pub thread_id: String,
    pub turn: Turn,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item: ThreadItem,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMessageDeltaNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLoginCompletedNotification {
    pub login_id: Option<String>,
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountUpdatedNotification {
    pub auth_mode: Option<String>,
    pub plan_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountRateLimitsUpdatedNotification {
    pub rate_limits: RateLimitSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub error: TurnError,
    pub will_retry: bool,
}

/// A server notification. Methods Bokkie does not read keep their raw
/// parameters.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerNotification {
    TurnStarted(TurnNotification),
    TurnCompleted(TurnNotification),
    ItemStarted(ItemNotification),
    ItemCompleted(ItemNotification),
    AgentMessageDelta(AgentMessageDeltaNotification),
    AccountLoginCompleted(AccountLoginCompletedNotification),
    AccountUpdated(AccountUpdatedNotification),
    AccountRateLimitsUpdated(AccountRateLimitsUpdatedNotification),
    Error(ErrorNotification),
    Unknown {
        method: String,
        params: Option<Value>,
    },
}

/// Notification methods with a typed decode, and their schema definitions.
pub const TYPED_NOTIFICATIONS: &[(&str, &str)] = &[
    ("turn/started", "v2/TurnStartedNotification"),
    ("turn/completed", "v2/TurnCompletedNotification"),
    ("item/started", "v2/ItemStartedNotification"),
    ("item/completed", "v2/ItemCompletedNotification"),
    (
        "item/agentMessage/delta",
        "v2/AgentMessageDeltaNotification",
    ),
    (
        "account/login/completed",
        "v2/AccountLoginCompletedNotification",
    ),
    ("account/updated", "v2/AccountUpdatedNotification"),
    (
        "account/rateLimits/updated",
        "v2/AccountRateLimitsUpdatedNotification",
    ),
    ("error", "v2/ErrorNotification"),
];

impl ServerNotification {
    pub fn decode(method: String, params: Option<Value>) -> Result<Self, serde_json::Error> {
        fn typed<T: serde::de::DeserializeOwned>(
            params: Option<Value>,
        ) -> Result<T, serde_json::Error> {
            serde_json::from_value(params.unwrap_or(Value::Null))
        }
        Ok(match method.as_str() {
            "turn/started" => Self::TurnStarted(typed(params)?),
            "turn/completed" => Self::TurnCompleted(typed(params)?),
            "item/started" => Self::ItemStarted(typed(params)?),
            "item/completed" => Self::ItemCompleted(typed(params)?),
            "item/agentMessage/delta" => Self::AgentMessageDelta(typed(params)?),
            "account/login/completed" => Self::AccountLoginCompleted(typed(params)?),
            "account/updated" => Self::AccountUpdated(typed(params)?),
            "account/rateLimits/updated" => Self::AccountRateLimitsUpdated(typed(params)?),
            "error" => Self::Error(typed(params)?),
            _ => Self::Unknown { method, params },
        })
    }
}

/// A request from the server that the client must answer.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerRequest {
    pub id: RequestId,
    pub kind: ServerRequestKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServerRequestKind {
    DynamicToolCall(DynamicToolCallParams),
    CommandExecutionApproval(CommandExecutionRequestApprovalParams),
    FileChangeApproval(FileChangeRequestApprovalParams),
    Unknown {
        method: String,
        params: Option<Value>,
    },
}

/// Server request methods with a typed decode, the schema definitions of
/// their parameters, and the definitions Bokkie's answers must satisfy.
pub const TYPED_SERVER_REQUESTS: &[(&str, &str, &str)] = &[
    (
        "item/tool/call",
        "DynamicToolCallParams",
        "DynamicToolCallResponse",
    ),
    (
        "item/commandExecution/requestApproval",
        "CommandExecutionRequestApprovalParams",
        "CommandExecutionRequestApprovalResponse",
    ),
    (
        "item/fileChange/requestApproval",
        "FileChangeRequestApprovalParams",
        "FileChangeRequestApprovalResponse",
    ),
];

impl ServerRequestKind {
    pub fn decode(method: String, params: Option<Value>) -> Result<Self, serde_json::Error> {
        let value = || params.clone().unwrap_or(Value::Null);
        Ok(match method.as_str() {
            "item/tool/call" => Self::DynamicToolCall(serde_json::from_value(value())?),
            "item/commandExecution/requestApproval" => {
                Self::CommandExecutionApproval(serde_json::from_value(value())?)
            }
            "item/fileChange/requestApproval" => {
                Self::FileChangeApproval(serde_json::from_value(value())?)
            }
            _ => Self::Unknown { method, params },
        })
    }
}
