//! Request and response bodies of the HTTP commands under
//! [`API_PREFIX`](crate::API_PREFIX).
//!
//! Every route except sign-in, invitation acceptance and the version needs
//! the session cookie. State-changing requests must also carry an `Origin`
//! header naming the gateway's own origin.

use serde::{Deserialize, Serialize};

use crate::events::Seq;
use crate::{BotId, MessageId, NotificationId, RoutineId, RoutineRunId, Timestamp, UserId};

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
    /// Creates, disables and resets members' accounts, sets their limits
    /// and permissions, switches oversight, and makes and edits Bots for
    /// them. Admins always have both [`Permissions`], and oversight never
    /// applies to them.
    Admin,
    Member,
}

/// What a member may do with their own Bots. Admins always have both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Permissions {
    /// Off: only an admin creates the member's Bots.
    pub may_create_bots: bool,
    /// Off: the name, label, description and avatar of the member's Bots
    /// are locked. The member can still pin and hide them.
    pub may_edit_bot_profiles: bool,
}

impl Default for Permissions {
    /// Both on.
    fn default() -> Self {
        Self {
            may_create_bots: true,
            may_edit_bot_profiles: true,
        }
    }
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
    #[serde(default)]
    pub permissions: Permissions,
    /// Whether admins can read this member's Bots and conversations,
    /// read-only. A client shows its owner a notice the whole time it is
    /// on. Always off for an admin.
    #[serde(default)]
    pub oversight: bool,
    /// When oversight last changed, if it ever has.
    #[serde(default)]
    pub oversight_changed_at: Option<Timestamp>,
    /// The IANA time zone the account's routines are scheduled in, such as
    /// `Australia/Adelaide`. Its owner sets it with
    /// `PUT /api/v1/session/time-zone`.
    #[serde(default = "default_time_zone")]
    pub time_zone: String,
}

/// The time zone of an account that has not chosen one.
pub const DEFAULT_TIME_ZONE: &str = "Australia/Adelaide";

fn default_time_zone() -> String {
    DEFAULT_TIME_ZONE.into()
}

/// `PUT /api/v1/session/time-zone`: the signed-in user's time zone, an IANA
/// name the gateway knows, such as `Europe/London`. The user's routines
/// are rescheduled in it at once. Answers with [`SessionResponse`]; the
/// user's other clients hear of it as
/// [`Event::AccountUpdated`](crate::events::Event::AccountUpdated).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetTimeZoneRequest {
    pub time_zone: String,
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
    /// When an admin reset this account's password, if the owner has not
    /// yet acknowledged it with `DELETE /api/v1/session/password-reset`.
    /// A client tells the owner, with when, at the first sign-in after the
    /// reset, and until they acknowledge it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_reset_by_admin: Option<Timestamp>,
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
    /// The new account's permissions; both on when omitted. An admin's
    /// invitation must leave both on.
    #[serde(default)]
    pub permissions: Permissions,
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
    #[serde(default)]
    pub permissions: Permissions,
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
    /// Turns the user may start in any 24 hours. Steering a running turn
    /// does not count.
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
/// `permissions`, when present, replaces both permissions. An admin's
/// permissions cannot be turned off, nor its oversight on. The account's
/// owner hears of the change as [`Event::AccountUpdated`](crate::events::Event::AccountUpdated).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateUserRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<Limits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Permissions>,
    /// Switches oversight of a member on or off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oversight: Option<bool>,
}

/// `PUT /api/v1/admin/users/{id}/password`. Revokes every session of that
/// user. No admin can reset another admin's password. A member whose
/// password an admin reset is told so at their next sign-in (see
/// [`SessionResponse::password_reset_by_admin`]).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetPasswordRequest {
    pub new_password: String,
}

// ---------------------------------------------------------------------------
// Bots and conversations

/// A capability a Bot can be granted. The gateway runs every tool a grant
/// brings, so no credential enters a Bot's sandbox. A Bot's thread offers
/// the model exactly the tools of its grants.
///
/// A grant is named on the wire as its [`Grant::name`]. A grant may join
/// the catalogue before its tools exist; the gateway refuses to grant it
/// until they do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Grant {
    /// One Markdown notes document of the Bot's own, which it reads,
    /// replaces and appends to.
    #[serde(rename = "notes")]
    Notes,
    /// A record of the items the Bot has shown its owner.
    #[serde(rename = "seen")]
    Seen,
    /// Notifications to the Bot's owner.
    #[serde(rename = "notify")]
    Notify,
    /// Searching the web. The Bot then reads untrusted input.
    #[serde(rename = "web.search")]
    WebSearch,
    /// Fetching web pages and feeds. The Bot then reads untrusted input.
    #[serde(rename = "web.fetch")]
    WebFetch,
}

impl Grant {
    /// Every grant in the catalogue, in order.
    pub const ALL: [Self; 5] = [
        Self::Notes,
        Self::Seen,
        Self::Notify,
        Self::WebSearch,
        Self::WebFetch,
    ];

    /// The grant's name on the wire, such as `web.search`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Notes => "notes",
            Self::Seen => "seen",
            Self::Notify => "notify",
            Self::WebSearch => "web.search",
            Self::WebFetch => "web.fetch",
        }
    }

    /// The grant with this name, if the catalogue has one.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|grant| grant.name() == name)
    }
}

impl std::fmt::Display for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

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
    /// The Bot's grants, in catalogue order. A change reaches the model
    /// from the Bot's next turn, which starts a fresh Codex thread.
    #[serde(default)]
    pub grants: Vec<Grant>,
    /// The newest of the Bot's messages the user has seen, on any of their
    /// devices; `None` before they have seen any.
    #[serde(default)]
    pub read_up_to: Option<MessageId>,
    /// Whether the Bot has a message the user has not seen: see
    /// [`Message::is_unread`].
    #[serde(default)]
    pub unread: bool,
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
    /// How many of the Bots are unread, hidden ones too: the count the
    /// app's badge shows. A client keeps it from each Bot's `unread`.
    #[serde(default)]
    pub unread_bots: u32,
    /// The list reflects every event up to and including this one.
    pub seq: Seq,
}

/// `POST /api/v1/bots`, refused with `forbidden` for a member without
/// [`Permissions::may_create_bots`]. An admin makes a Bot for a member with
/// the same body at `POST /api/v1/admin/users/{id}/bots`, which returns a
/// [`MemberBot`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateBotRequest {
    pub name: String,
    pub label: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
}

/// `PATCH /api/v1/bots/{id}`. A changed description reaches the model from
/// the Bot's next turn. A member without
/// [`Permissions::may_edit_bot_profiles`] is refused with `forbidden` when
/// the request names the name, label, description or avatar; pinning and
/// hiding are always allowed.
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
    /// Replaces the Bot's grants. A member may add only `notes`, `seen`
    /// and `notify` (`forbidden` otherwise); removing any grant is allowed.
    /// A grant whose tools do not exist yet, or a set that breaks the taint
    /// rule, is refused with `invalid_request`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grants: Option<Vec<Grant>>,
}

/// One of a member's Bots, as an admin sees it. With oversight off, an
/// admin sees only the Bots an admin made for the member, and only their
/// profile; with it on, every Bot, and `overseen` too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberBot {
    pub id: BotId,
    pub name: String,
    pub label: String,
    pub description: String,
    pub avatar: Option<String>,
    /// Whether an admin made the Bot for the member.
    pub made_by_admin: bool,
    pub created_at: Timestamp,
    /// The Bot's grants, in catalogue order.
    #[serde(default)]
    pub grants: Vec<Grant>,
    /// Only while oversight is on: the Bot as the member sees it, with its
    /// status, pin, visibility and latest message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overseen: Option<Bot>,
}

/// `GET /api/v1/admin/users/{id}/bots`, for an admin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberBotList {
    /// Whether oversight of the member is on, as this list was read.
    pub oversight: bool,
    pub bots: Vec<MemberBot>,
}

/// `PATCH /api/v1/admin/users/{id}/bots/{bot}`: an admin changes the
/// profile or the grants of a Bot an admin made for the member. The
/// member's own Bots are read-only to admins, even under oversight, and
/// pinning and hiding stay the member's. A request that names no field is
/// refused.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateBotProfileRequest {
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
    /// Replaces the Bot's grants. An admin may grant anything the gateway
    /// offers, within the taint rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grants: Option<Vec<Grant>>,
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

/// What a message is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// Something the user or the Bot said.
    #[default]
    Text,
    /// A note from the gateway in the conversation, such as "Capabilities
    /// changed". Its author is the Bot; the model never wrote it.
    Marker,
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
    #[serde(default)]
    pub kind: MessageKind,
    /// For the marker that begins a routine's run: the routine, the run,
    /// and the prompt the Bot was given, for a client to show folded away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routine: Option<MessageRoutine>,
    /// For the marker of a notification the Bot sent with its `notify`
    /// tool: what it said. The marker's text is the title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification: Option<MessageNotification>,
}

impl Message {
    /// Whether this message makes its Bot unread for a user who has seen
    /// the conversation up to `read_up_to`: the Bot wrote it, a reply or a
    /// marker, after the newest message the user has seen. The user's own
    /// messages never do.
    pub fn is_unread(&self, read_up_to: Option<MessageId>) -> bool {
        self.author == Author::Bot && read_up_to.is_none_or(|read| self.id > read)
    }
}

/// What a routine run's marker in the conversation carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageRoutine {
    pub routine_id: RoutineId,
    pub run_id: RoutineRunId,
    /// The routine's prompt as the run gave it to the Bot.
    pub prompt: String,
}

/// What a notification's marker in the conversation carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageNotification {
    pub kind: NotificationKind,
    /// The body, possibly empty. The title is the marker's text.
    pub body: String,
}

/// `GET /api/v1/bots/{id}/messages?before={message_id}&limit={n}`, newest
/// last. An admin reads a member's conversation, only while oversight of
/// that member is on, at
/// `GET /api/v1/admin/users/{id}/bots/{bot}/messages`, with the same query;
/// there `seq` is the member's log, which the admin cannot follow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessagePage {
    pub messages: Vec<Message>,
    /// Pass as `before` to fetch older messages; `None` at the start.
    pub older: Option<MessageId>,
    /// The page reflects every event up to and including this one.
    pub seq: Seq,
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

// ---------------------------------------------------------------------------
// Routines

/// A day of the week.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    /// Monday to Sunday.
    pub const ALL: [Self; 7] = [
        Self::Mon,
        Self::Tue,
        Self::Wed,
        Self::Thu,
        Self::Fri,
        Self::Sat,
        Self::Sun,
    ];
}

/// A local time of day, `"HH:MM"` on the wire in 24-hour time, such as
/// `"07:00"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimeOfDay {
    hour: u8,
    minute: u8,
}

impl TimeOfDay {
    /// The time, if `hour` is 0 to 23 and `minute` 0 to 59.
    pub fn new(hour: u8, minute: u8) -> Option<Self> {
        (hour < 24 && minute < 60).then_some(Self { hour, minute })
    }

    pub fn hour(self) -> u8 {
        self.hour
    }

    pub fn minute(self) -> u8 {
        self.minute
    }
}

impl std::fmt::Display for TimeOfDay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02}:{:02}", self.hour, self.minute)
    }
}

impl std::str::FromStr for TimeOfDay {
    type Err = String;

    /// Reads `"HH:MM"`, two digits each.
    fn from_str(text: &str) -> Result<Self, String> {
        let invalid = || format!("{text:?} is not a time of day such as \"07:00\"");
        let (hour, minute) = text.split_once(':').ok_or_else(invalid)?;
        let digits = |part: &str| {
            (part.len() == 2 && part.bytes().all(|b| b.is_ascii_digit()))
                .then(|| part.parse::<u8>().ok())
                .flatten()
        };
        match (digits(hour), digits(minute)) {
            (Some(hour), Some(minute)) => Self::new(hour, minute).ok_or_else(invalid),
            _ => Err(invalid()),
        }
    }
}

impl Serialize for TimeOfDay {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for TimeOfDay {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// When a routine runs, in its owner's time zone ([`User::time_zone`]).
/// A local time that does not exist on a day the clocks go forward runs at
/// the first valid instant after it; one that happens twice when they go
/// back runs once, the first time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "repeat", rename_all = "snake_case")]
pub enum Schedule {
    /// Every day at `at`.
    Daily { at: TimeOfDay },
    /// Monday to Friday at `at`.
    Weekdays { at: TimeOfDay },
    /// On the chosen days at `at`. At least one day.
    Days { days: Vec<Weekday>, at: TimeOfDay },
    /// Every `every` hours through the day, at `at` and the local times a
    /// multiple of `every` hours from it. `every` divides a day: 1, 2, 3,
    /// 4, 6, 8 or 12.
    Hours { every: u8, at: TimeOfDay },
}

impl Schedule {
    /// The intervals, in hours, that [`Schedule::Hours`] allows.
    pub const EVERY_HOURS: [u8; 7] = [1, 2, 3, 4, 6, 8, 12];
}

/// When a routine's run notifies its owner.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutinePush {
    /// Every finished run notifies.
    #[default]
    EveryRun,
    /// Only a run in which the Bot calls `notify`.
    NotifyOnly,
}

/// A routine: a prompt one of the user's Bots runs on a schedule, in the
/// Bot's own conversation, with the Bot's grants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Routine {
    pub id: RoutineId,
    pub bot_id: BotId,
    /// A short name, shown on the run's marker, such as "Morning digest".
    pub name: String,
    /// What the Bot is asked each run.
    pub prompt: String,
    pub schedule: Schedule,
    pub push: RoutinePush,
    /// A paused routine does not run on its schedule. Three failed runs in
    /// a row pause it.
    pub paused: bool,
    /// Failed runs in a row since the last one that succeeded or the
    /// routine was resumed. A paused routine with three paused itself.
    pub failures: u32,
    /// When the routine next runs; `None` while paused.
    pub next_run_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// `GET /api/v1/bots/{id}/routines`, or every routine of the user's at
/// `GET /api/v1/routines`, ordered by id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineList {
    pub routines: Vec<Routine>,
    /// The list reflects every event up to and including this one.
    pub seq: Seq,
}

/// `POST /api/v1/bots/{id}/routines`. A user has at most 20 routines, and
/// a Bot's routines have different names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateRoutineRequest {
    pub name: String,
    pub prompt: String,
    pub schedule: Schedule,
    #[serde(default)]
    pub push: RoutinePush,
    /// Created paused, when set.
    #[serde(default)]
    pub paused: bool,
}

/// `PATCH /api/v1/bots/{id}/routines/{routine}`. `None` leaves a field
/// alone. Resuming a paused routine clears its failures and schedules it
/// from now, so the runs it missed while paused do not run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRoutineRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<RoutinePush>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
}

/// What started a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunTrigger {
    /// The routine's schedule.
    Schedule,
    /// "Run now": `POST /api/v1/bots/{id}/routines/{routine}/run`, which
    /// takes the same path as a scheduled run.
    Manual,
}

/// Where a run is. A run waits, then runs, then ends in one of the other
/// states, its outcome; or it ends without running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Queued until the Bot is idle and ChatGPT is signed in.
    Waiting,
    /// The Bot's turn is running.
    Running,
    /// The turn completed.
    Succeeded,
    /// The run could not start, or its turn failed. `reason` says why.
    Failed,
    /// The run was due while it could not run, and too long ago to run
    /// late. `reason` says why.
    Missed,
    /// The run did not happen: the routine's previous run was still going,
    /// or the Bot stayed busy. `reason` says which.
    Skipped,
    /// The turn was stopped: it ran too long, or its owner stopped it.
    Interrupted,
}

impl RunState {
    /// Whether the run has ended.
    pub fn is_finished(self) -> bool {
        !matches!(self, Self::Waiting | Self::Running)
    }
}

/// One run of a routine, kept 90 days.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineRun {
    pub id: RoutineRunId,
    pub routine_id: RoutineId,
    pub bot_id: BotId,
    pub trigger: RunTrigger,
    /// When the schedule said it should run; for a manual run, when it was
    /// asked for.
    pub scheduled_for: Timestamp,
    /// When its turn started.
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub state: RunState,
    /// Why it failed, was missed, skipped or interrupted, or what a
    /// waiting run waits for. Safe to show.
    pub reason: Option<String>,
    /// The messages it added to the Bot's conversation: its marker, then
    /// the Bot's replies.
    pub messages: Vec<MessageId>,
    /// The tokens its turn used, when Codex reported them.
    pub tokens: Option<u64>,
}

/// `GET /api/v1/bots/{id}/routines/{routine}/runs?before={id}&limit={n}`:
/// the routine's runs, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineRunPage {
    pub runs: Vec<RoutineRun>,
    /// Pass as `before` to fetch older runs; `None` at the end.
    pub older: Option<RoutineRunId>,
}

/// `POST /api/v1/bots/{id}/stop`: interrupt the running turn, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopResponse {
    /// Whether a turn was running.
    pub stopped: bool,
}

// ---------------------------------------------------------------------------
// Bots' notes and tool calls

/// `GET /api/v1/bots/{id}/notes`: the Bot's notes, one Markdown document
/// the Bot keeps for itself through its `notes` tools. They are the Bot's
/// own working memory, not instructions. `DELETE` clears them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotNotes {
    pub text: String,
    /// When the notes last changed; `None` if the Bot never wrote any.
    pub updated_at: Option<Timestamp>,
}

/// How a brokered tool call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallOutcome {
    /// The tool ran and answered.
    Ok,
    /// The tool ran and failed, for example on invalid arguments.
    Failed,
    /// The gateway refused the call: the Bot no longer holds the grant, or
    /// it named no tool the gateway has.
    Refused,
    /// The tool did not finish within its deadline.
    TimedOut,
}

/// One entry of a Bot's tool-call log, kept 90 days.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: i64,
    /// The tool's name, such as `notes_append`.
    pub tool: String,
    /// A short, bounded summary of the arguments, such as a size or a URL.
    pub summary: String,
    pub outcome: ToolCallOutcome,
    pub duration_ms: u64,
    pub at: Timestamp,
    /// The routine run whose turn made the call, if a routine's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routine_run_id: Option<RoutineRunId>,
}

/// `GET /api/v1/bots/{id}/tool-calls?before={id}&limit={n}`: the Bot's
/// tool calls, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallPage {
    pub calls: Vec<ToolCall>,
    /// Pass as `before` to fetch older calls; `None` at the end.
    pub older: Option<i64>,
}

// ---------------------------------------------------------------------------
// Unread state and notifications

/// `PUT /api/v1/bots/{id}/read`: the user has seen the Bot's conversation
/// up to `message_id`, one of its messages. A client sends it while the
/// conversation is visible with the app in the foreground. The read marker
/// only moves forward: an older message leaves it where it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkReadRequest {
    pub message_id: MessageId,
}

/// A Bot's read marker, which every device of its user shares: the answer
/// to `PUT /api/v1/bots/{id}/read`, and what
/// [`Event::ReadUpdated`](crate::events::Event::ReadUpdated) carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadState {
    pub bot_id: BotId,
    pub read_up_to: Option<MessageId>,
    /// Whether the Bot still has a message the user has not seen.
    pub unread: bool,
}

/// What a notification is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    /// Something finished or was found.
    Result,
    /// The Bot needs an answer from its owner.
    Question,
}

/// The longest notification title, in Unicode scalar values (Rust
/// `char`s). A title is one line.
pub const NOTIFICATION_TITLE_CHARS: usize = 80;
/// The longest notification body, in Unicode scalar values.
pub const NOTIFICATION_BODY_CHARS: usize = 500;

/// A notification a Bot sent its owner with its `notify` tool, kept 90
/// days. Its marker in the conversation stays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    pub id: NotificationId,
    pub bot_id: BotId,
    /// The notification's marker in the conversation.
    pub message_id: MessageId,
    /// The routine run whose turn sent it, if a routine's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routine_run_id: Option<RoutineRunId>,
    pub kind: NotificationKind,
    pub title: String,
    pub body: String,
    pub created_at: Timestamp,
}

/// `GET /api/v1/notifications?before={id}&limit={n}`: the user's
/// notifications from every Bot, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPage {
    pub notifications: Vec<Notification>,
    /// Pass as `before` to fetch older notifications; `None` at the end.
    pub older: Option<NotificationId>,
}

// ---------------------------------------------------------------------------
// Web Push

/// `GET /api/v1/push/key`: what a device needs to subscribe to Web Push.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushKey {
    /// The gateway's VAPID public key: an uncompressed P-256 point in
    /// unpadded base64url, for `PushManager.subscribe`'s
    /// `applicationServerKey`.
    pub public_key: String,
    /// Whether the gateway sends pushes. Without a configured contact it
    /// serves its key but sends nothing.
    pub sending: bool,
}

/// The longest push endpoint the gateway accepts, in bytes.
pub const PUSH_ENDPOINT_BYTES: usize = 1024;

/// `PUT /api/v1/push/subscription`: the current session's Web Push
/// subscription, as `PushSubscription.toJSON()` gives it (other fields,
/// such as `expirationTime`, are ignored). It replaces the session's
/// earlier subscription; a device sends it again on every launch. An
/// endpoint belongs to one session at a time, so another session's
/// subscription with the same endpoint ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushSubscriptionRequest {
    /// The push service's `https` URL for the device, at most
    /// [`PUSH_ENDPOINT_BYTES`]. The gateway never sends it back.
    pub endpoint: String,
    pub keys: PushSubscriptionKeys,
}

/// A subscription's keys, in base64url (padding allowed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushSubscriptionKeys {
    /// The device's P-256 public key, uncompressed: 65 bytes.
    pub p256dh: String,
    /// The device's authentication secret: 16 bytes.
    pub auth: String,
}

/// `GET` and `PUT /api/v1/push/subscription`: whether the current session
/// has a subscription. The endpoint itself is never sent back: anyone who
/// holds it can address the device. `DELETE` removes the subscription;
/// signing out or revoking the session does too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushSubscriptionState {
    pub subscribed: bool,
}

// ---------------------------------------------------------------------------
// The household's ChatGPT sign-in

/// `GET /api/v1/codex/account`: whether the household's ChatGPT sign-in,
/// which every user's app server shares, is signed in and usable. A member
/// sees only that; an admin also sees the account's email and plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexAccount {
    pub signed_in: bool,
    /// The ChatGPT plan, such as `pro`, when signed in. Admins only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// The account's email, when signed in. Admins only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// Where a device-code sign-in of the household stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginState {
    /// Waiting for someone to enter the code.
    Pending,
    /// Signed in.
    Completed,
    /// The code expired unused. Start a new sign-in for a new code.
    Expired,
    /// The sign-in failed; `error` says why.
    Failed,
    /// An admin cancelled it, or started another.
    Cancelled,
}

/// A device-code sign-in of the household's app server, for admins only:
/// `POST /api/v1/codex/login` starts one, `GET /api/v1/codex/login` reports
/// the latest, and `DELETE /api/v1/codex/login` cancels it while pending.
/// Open the URL, enter the code, then poll until the state is no longer
/// `pending`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCodeLogin {
    pub verification_url: String,
    pub user_code: String,
    pub state: LoginState,
    /// When the code stops working.
    pub expires_at: Timestamp,
    /// Why the sign-in failed, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    /// Turns started in the last 24 hours.
    pub turns_today: u32,
    /// Tokens used in the last seven days.
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
