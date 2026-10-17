//! Request and response bodies of the HTTP commands under
//! [`API_PREFIX`](crate::API_PREFIX).
//!
//! Every route except sign-in, invitation acceptance and the version needs
//! the session cookie. State-changing requests must also carry an `Origin`
//! header naming the gateway's own origin.

use serde::{Deserialize, Serialize};

use crate::{BotId, MessageId, Timestamp, UserId};

// ---------------------------------------------------------------------------
// Errors

/// The body of every error response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    /// A human-readable explanation. It never contains secrets or another
    /// user's data.
    pub message: String,
}

/// Why a request failed. Each code maps to one HTTP status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// 400: the request was malformed or a field was invalid.
    InvalidRequest,
    /// 401: no valid session, or the sign-in failed.
    Unauthenticated,
    /// 403: the session is valid but may not do this, or the `Origin` or
    /// `Host` check failed.
    Forbidden,
    /// 404: no such resource for this user. Another user's resources are
    /// always reported as not found.
    NotFound,
    /// 409: the request conflicts with the current state, for example a
    /// username that is already taken.
    Conflict,
    /// 429: too many attempts; try again later.
    RateLimited,
    /// 429: the user's own allowance or the shared subscription is used up.
    LimitReached,
    /// 503: the Bot's runtime is not available, or it failed a safety check.
    Unavailable,
    /// 500: an unexpected error. Details are logged, not returned.
    Internal,
}

impl ErrorCode {
    /// The HTTP status code for this error.
    pub fn status(self) -> u16 {
        match self {
            Self::InvalidRequest => 400,
            Self::Unauthenticated => 401,
            Self::Forbidden => 403,
            Self::NotFound => 404,
            Self::Conflict => 409,
            Self::RateLimited | Self::LimitReached => 429,
            Self::Unavailable => 503,
            Self::Internal => 500,
        }
    }
}

// ---------------------------------------------------------------------------
// Version

/// `GET /api/v1/version`, which needs no session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionResponse {
    pub protocol: u32,
    pub gateway: String,
}

// ---------------------------------------------------------------------------
// Accounts and sessions

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Creates, disables and resets accounts and sets their limits.
    Admin,
    Member,
}

/// An account as its owner or an admin sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub disabled: bool,
    pub created_at: Timestamp,
}

/// `POST /api/v1/session`: sign in with a password. Success sets the session
/// cookie and returns [`SessionResponse`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// `GET /api/v1/session` and a successful sign-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionResponse {
    pub user: User,
}

/// One of the signed-in user's sessions, from `GET /api/v1/sessions`.
/// `DELETE /api/v1/sessions/{id}` revokes it; `DELETE /api/v1/session`
/// signs out the current one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: i64,
    pub current: bool,
    pub created_at: Timestamp,
    pub last_seen_at: Timestamp,
    /// The `User-Agent` at sign-in, shortened, to tell devices apart.
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionList {
    pub sessions: Vec<SessionSummary>,
}

/// `PUT /api/v1/session/password`: change the signed-in user's password.
/// Every other session of that user is revoked.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

// ---------------------------------------------------------------------------
// Invitations (signup is invitation-only)

/// `POST /api/v1/admin/invitations`, admin only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateInvitationRequest {
    pub role: Role,
    /// Suggested display name for the new account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// How long the invitation stays valid. The gateway caps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_hours: Option<u32>,
}

/// A newly created invitation. The code is shown once; the gateway stores
/// only its hash.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedInvitation {
    pub id: i64,
    pub code: String,
    pub role: Role,
    pub expires_at: Timestamp,
}

/// An outstanding invitation, from `GET /api/v1/admin/invitations`.
/// `DELETE /api/v1/admin/invitations/{id}` withdraws it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvitationSummary {
    pub id: i64,
    pub role: Role,
    pub display_name: Option<String>,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvitationList {
    pub invitations: Vec<InvitationSummary>,
}

/// `POST /api/v1/invitations/accept`, which needs no session. Success
/// creates the account, signs it in and returns [`SessionResponse`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptInvitationRequest {
    pub code: String,
    pub username: String,
    pub password: String,
    pub display_name: String,
}

// ---------------------------------------------------------------------------
// Admin user management

/// Limits an admin sets for an account. `None` means unlimited.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Turns the user may start per calendar day, in the gateway's time zone.
    pub daily_turns: Option<u32>,
    /// Tokens the user's turns may use per rolling seven days, measured from
    /// `thread/tokenUsage/updated`.
    pub weekly_tokens: Option<u64>,
}

/// An account with its limits, as an admin sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminUser {
    pub user: User,
    pub limits: Limits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminUserList {
    pub users: Vec<AdminUser>,
}

/// `PATCH /api/v1/admin/users/{id}`. Disabling an account revokes its
/// sessions and stops its Bots. `limits`, when present, replaces both limits
/// at once: send the whole object, because an omitted field means unlimited.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateUserRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<Limits>,
}

/// `PUT /api/v1/admin/users/{id}/password`. Revokes every session of that
/// user.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetPasswordRequest {
    pub new_password: String,
}

// ---------------------------------------------------------------------------
// Bots and conversations

/// What a Bot is doing now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotStatus {
    Idle,
    /// A turn is running. New messages steer it.
    Working,
    /// The gateway refused to serve the Bot's thread because a safety check
    /// failed. The Bot accepts no messages until an admin resolves it.
    Refused,
}

/// A Bot with the preview its conversation list entry needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bot {
    pub id: BotId,
    pub name: String,
    /// A short job title, such as "Meal planner".
    pub label: String,
    /// The Bot's durable rules, given to the model as instructions.
    pub description: String,
    /// An emoji or short text shown as the avatar.
    pub avatar: Option<String>,
    pub pinned: bool,
    pub hidden: bool,
    pub status: BotStatus,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub last_message: Option<MessagePreview>,
}

/// The start of a conversation's latest message, for the conversation list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessagePreview {
    pub author: Author,
    pub text: String,
    pub at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotList {
    pub bots: Vec<Bot>,
}

/// `POST /api/v1/bots`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateBotRequest {
    pub name: String,
    pub label: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
}

/// `PATCH /api/v1/bots/{id}`. A changed description reaches the model from
/// the Bot's next turn.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateBotRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `Some(None)` clears the avatar.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub avatar: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
}

/// Who wrote a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    User,
    Bot,
}

/// Where a message is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageState {
    /// A Bot reply that is still arriving.
    Streaming,
    Complete,
    /// Stopped by the user before it finished.
    Interrupted,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub bot_id: BotId,
    pub author: Author,
    pub text: String,
    pub state: MessageState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// `GET /api/v1/bots/{id}/messages?before={message_id}&limit={n}`, newest
/// last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessagePage {
    pub messages: Vec<Message>,
    /// Pass as `before` to fetch older messages; `None` at the start.
    pub older: Option<MessageId>,
}

/// `POST /api/v1/bots/{id}/messages`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendMessageRequest {
    pub text: String,
}

/// How a sent message reached the Bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// The Bot was idle; the message started a turn.
    NewTurn,
    /// The Bot was mid-turn; the message steered it.
    Steered,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendMessageResponse {
    pub message: Message,
    pub delivery: Delivery,
}

/// `POST /api/v1/bots/{id}/stop`: interrupt the running turn, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopResponse {
    /// Whether a turn was running.
    pub stopped: bool,
}

// ---------------------------------------------------------------------------
// The user's Codex account

/// `GET /api/v1/codex/account`: whether the user's own app server is signed
/// in to ChatGPT. Only that user sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexAccount {
    pub signed_in: bool,
    /// The ChatGPT plan, such as `pro`, when signed in.
    pub plan: Option<String>,
}

/// `POST /api/v1/codex/login`: a device-code sign-in. Open the URL, enter
/// the code, then poll [`CodexAccount`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCodeLogin {
    pub verification_url: String,
    pub user_code: String,
}

// ---------------------------------------------------------------------------
// Usage

/// `GET /api/v1/usage`: the shared subscription's rate-limit windows and the
/// user's own allowance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// The subscription's buckets, as the user's app server last reported
    /// them. Empty until the first report.
    pub subscription: Vec<RateLimitBucket>,
    pub allowance: Allowance,
}

/// One of the subscription's rate-limit buckets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimitBucket {
    pub id: String,
    pub name: Option<String>,
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
    /// Set when the subscription reports that this limit is reached.
    pub reached: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimitWindow {
    pub used_percent: i32,
    pub window_minutes: Option<i64>,
    pub resets_at: Option<Timestamp>,
}

/// The user's own allowance, set by an admin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Allowance {
    pub limits: Limits,
    pub turns_today: u32,
    pub tokens_this_week: u64,
    /// Set when a limit is reached; new turns are refused until it resets.
    pub exhausted: bool,
}

// ---------------------------------------------------------------------------
// Secret-bearing types print their secrets redacted, so that a `{:?}` in a
// log line cannot leak a password or an invitation code.

const REDACTED: &str = "<redacted>";

impl std::fmt::Debug for LoginRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginRequest")
            .field("username", &self.username)
            .field("password", &REDACTED)
            .finish()
    }
}

impl std::fmt::Debug for ChangePasswordRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangePasswordRequest")
            .field("current_password", &REDACTED)
            .field("new_password", &REDACTED)
            .finish()
    }
}

impl std::fmt::Debug for AcceptInvitationRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcceptInvitationRequest")
            .field("code", &REDACTED)
            .field("username", &self.username)
            .field("password", &REDACTED)
            .field("display_name", &self.display_name)
            .finish()
    }
}

impl std::fmt::Debug for ResetPasswordRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResetPasswordRequest")
            .field("new_password", &REDACTED)
            .finish()
    }
}

impl std::fmt::Debug for CreatedInvitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreatedInvitation")
            .field("id", &self.id)
            .field("code", &REDACTED)
            .field("role", &self.role)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Serde support for `Option<Option<T>>` fields: absent means "unchanged",
/// `null` means "clear".
mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T: Serialize, S: Serializer>(
        value: &Option<Option<T>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(inner) => inner.serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(deserializer).map(Some)
    }
}
