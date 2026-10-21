//! The HTTP commands the client sends, independent of how they travel.
//!
//! Each [`ApiRequest`] knows its method, path and body, and how to decode
//! its response into an [`ApiReply`]. The browser transport
//! (`crate::browser`) sends them with `fetch` and runs the event socket;
//! tests record them and reply by hand.

use bokkie_protocol::api::{
    AcceptInvitationRequest, AdminUser, AdminUserList, Allowance, Bot, BotList, BotNotes,
    CodexAccount, CreateBotRequest, CreateInvitationRequest, CreateRoutineRequest,
    CreatedInvitation, DeviceCodeLogin, InvitationList, LoginRequest, MarkReadRequest, MemberBot,
    MemberBotList, MessagePage, PushKey, PushSubscriptionRequest, PushSubscriptionState, ReadState,
    ResetPasswordRequest, Routine, RoutineList, RoutineRun, RoutineRunPage, SendMessageRequest,
    SendMessageResponse, SessionResponse, SetTimeZoneRequest, StopResponse, ToolCallPage,
    UpdateBotProfileRequest, UpdateBotRequest, UpdateRoutineRequest, UpdateUserRequest, Usage,
};
use bokkie_protocol::{API_PREFIX, BotId, MessageId, RoutineId, RoutineRunId, Timestamp, UserId};

/// How many tool calls, or routine runs, a page asks for.
pub const PAGE_SIZE: u32 = 20;

/// How many of each routine's newest runs the Bot panel's recent runs
/// gather.
pub const RECENT_RUNS: u32 = 5;

use crate::wire::{self, CommandError};

/// A command for the gateway.
#[derive(Clone, PartialEq, Eq)]
pub enum ApiRequest {
    /// `GET /api/v1/session`: whether the browser holds a valid session.
    CheckSession,
    /// `POST /api/v1/session`: sign in.
    SignIn(LoginRequest),
    /// `DELETE /api/v1/session`: sign this browser out.
    SignOut,
    /// `GET /api/v1/bots`: the signed-in user's Bots.
    ListBots,
    /// `GET /api/v1/bots/{id}/messages`: the newest page of a conversation.
    LoadMessages(BotId),
    /// `GET /api/v1/bots/{id}/messages?before={message_id}`: the page of a
    /// conversation before that message.
    LoadOlder(BotId, MessageId),
    /// `POST /api/v1/bots/{id}/messages`: send a message, which starts a
    /// turn or steers the running one.
    SendMessage(BotId, SendMessageRequest),
    /// `POST /api/v1/bots/{id}/stop`: interrupt the running reply.
    StopBot(BotId),
    /// `GET /api/v1/usage`: the usage meter.
    LoadUsage,
    /// `POST /api/v1/bots`: create a Bot.
    CreateBot(CreateBotRequest),
    /// `PATCH /api/v1/bots/{id}`: change a Bot's profile, pin or
    /// visibility.
    UpdateBot(BotId, UpdateBotRequest),
    /// `DELETE /api/v1/session/password-reset`: the user has read that an
    /// admin reset their password.
    AcknowledgeReset,
    /// `GET /api/v1/codex/account`: whether the household is signed in to
    /// ChatGPT, and for an admin, as whom.
    CodexAccount,
    /// `POST /api/v1/codex/login`: start a device-code sign-in of the
    /// household (admins only).
    CodexLogin,
    /// `GET /api/v1/codex/login`: where the latest sign-in stands.
    CodexLoginStatus,
    /// `DELETE /api/v1/codex/login`: cancel the pending sign-in.
    CancelCodexLogin,
    /// `POST /api/v1/codex/logout`: sign the household out, which signs out
    /// every account's Bots.
    CodexLogout,
    /// `POST /api/v1/invitations/accept`: create an account from an
    /// invitation code, and sign in as it.
    AcceptInvitation(AcceptInvitationRequest),
    /// `GET /api/v1/admin/users`: every account (admins only, as are the
    /// rest).
    ListAccounts,
    /// `PATCH /api/v1/admin/users/{id}`: change an account.
    UpdateAccount(UserId, UpdateUserRequest),
    /// `PUT /api/v1/admin/users/{id}/password`: reset an account's password.
    ResetPassword(UserId, ResetPasswordRequest),
    /// `GET /api/v1/admin/users/{id}/usage`: an account's allowance.
    AccountUsage(UserId),
    /// `GET /api/v1/admin/invitations`: the outstanding invitations.
    ListInvitations,
    /// `POST /api/v1/admin/invitations`: invite someone.
    CreateInvitation(CreateInvitationRequest),
    /// `DELETE /api/v1/admin/invitations/{id}`: withdraw an invitation.
    WithdrawInvitation(i64),
    /// `GET /api/v1/admin/users/{id}/bots`: the member's Bots an admin may
    /// see.
    MemberBots(UserId),
    /// `POST /api/v1/admin/users/{id}/bots`: make a Bot for the member.
    CreateMemberBot(UserId, CreateBotRequest),
    /// `PATCH /api/v1/admin/users/{id}/bots/{bot}`: change an admin-made
    /// Bot's profile.
    UpdateMemberBot(UserId, BotId, UpdateBotProfileRequest),
    /// `GET /api/v1/admin/users/{id}/bots/{bot}/messages[?before=]`: read a
    /// member's conversation, under oversight.
    MemberMessages(UserId, BotId, Option<MessageId>),
    /// `PUT /api/v1/session/time-zone`: the user's time zone.
    SetTimeZone(SetTimeZoneRequest),
    /// `GET /api/v1/bots/{id}/notes`: the Bot's notes.
    LoadNotes(BotId),
    /// `DELETE /api/v1/bots/{id}/notes`: clear the Bot's notes.
    ClearNotes(BotId),
    /// `GET /api/v1/bots/{id}/tool-calls?limit=[&before=]`: a page of the
    /// Bot's tool calls, newest first.
    LoadToolCalls(BotId, Option<i64>),
    /// `GET /api/v1/bots/{id}/routines`: the Bot's routines.
    ListRoutines(BotId),
    /// `POST /api/v1/bots/{id}/routines`: create a routine.
    CreateRoutine(BotId, CreateRoutineRequest),
    /// `PATCH /api/v1/bots/{id}/routines/{routine}`: change a routine,
    /// pause it or resume it.
    UpdateRoutine(BotId, RoutineId, UpdateRoutineRequest),
    /// `DELETE /api/v1/bots/{id}/routines/{routine}`: delete a routine.
    DeleteRoutine(BotId, RoutineId),
    /// `POST /api/v1/bots/{id}/routines/{routine}/run`: run it now.
    RunRoutine(BotId, RoutineId),
    /// `GET /api/v1/bots/{id}/routines/{routine}/runs?limit=[&before=]`: a
    /// page of the routine's runs, newest first.
    LoadRuns(BotId, RoutineId, Option<RoutineRunId>),
    /// `GET /api/v1/bots/{id}/routines/{routine}/runs?limit=`: the
    /// routine's newest few runs, for the Bot panel's recent runs.
    LoadRecentRuns(BotId, RoutineId),
    /// `GET /api/v1/bots/{id}`: one Bot as it is now.
    LoadBot(BotId),
    /// `PUT /api/v1/bots/{id}/read`: the user has seen the Bot's
    /// conversation up to this message.
    MarkRead(BotId, MarkReadRequest),
    /// `GET /api/v1/push/key`: the key a device subscribes to Web Push with.
    PushKey,
    /// `PUT /api/v1/push/subscription`: this device's subscription, for the
    /// session.
    PutPushSubscription(PushSubscriptionRequest),
    /// `DELETE /api/v1/push/subscription`: end the session's subscription.
    DeletePushSubscription,
}

// The password must never reach a log line, nor a push subscription's
// endpoint or keys, with which anyone could address the device.
impl std::fmt::Debug for ApiRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CheckSession => f.write_str("CheckSession"),
            Self::SignIn(login) => f.debug_tuple("SignIn").field(login).finish(),
            Self::SignOut => f.write_str("SignOut"),
            Self::ListBots => f.write_str("ListBots"),
            Self::LoadMessages(bot) => f.debug_tuple("LoadMessages").field(bot).finish(),
            Self::LoadOlder(bot, before) => {
                f.debug_tuple("LoadOlder").field(bot).field(before).finish()
            }
            Self::SendMessage(bot, message) => f
                .debug_tuple("SendMessage")
                .field(bot)
                .field(message)
                .finish(),
            Self::StopBot(bot) => f.debug_tuple("StopBot").field(bot).finish(),
            Self::LoadUsage => f.write_str("LoadUsage"),
            Self::CreateBot(bot) => f.debug_tuple("CreateBot").field(bot).finish(),
            Self::UpdateBot(bot, changes) => f
                .debug_tuple("UpdateBot")
                .field(bot)
                .field(changes)
                .finish(),
            Self::AcknowledgeReset => f.write_str("AcknowledgeReset"),
            Self::CodexAccount => f.write_str("CodexAccount"),
            Self::CodexLogin => f.write_str("CodexLogin"),
            Self::CodexLoginStatus => f.write_str("CodexLoginStatus"),
            Self::CancelCodexLogin => f.write_str("CancelCodexLogin"),
            Self::CodexLogout => f.write_str("CodexLogout"),
            Self::AcceptInvitation(accept) => {
                f.debug_tuple("AcceptInvitation").field(accept).finish()
            }
            Self::ListAccounts => f.write_str("ListAccounts"),
            Self::UpdateAccount(user, changes) => f
                .debug_tuple("UpdateAccount")
                .field(user)
                .field(changes)
                .finish(),
            Self::ResetPassword(user, reset) => f
                .debug_tuple("ResetPassword")
                .field(user)
                .field(reset)
                .finish(),
            Self::AccountUsage(user) => f.debug_tuple("AccountUsage").field(user).finish(),
            Self::ListInvitations => f.write_str("ListInvitations"),
            Self::CreateInvitation(invitation) => {
                f.debug_tuple("CreateInvitation").field(invitation).finish()
            }
            Self::WithdrawInvitation(id) => f.debug_tuple("WithdrawInvitation").field(id).finish(),
            Self::MemberBots(user) => f.debug_tuple("MemberBots").field(user).finish(),
            Self::CreateMemberBot(user, bot) => f
                .debug_tuple("CreateMemberBot")
                .field(user)
                .field(bot)
                .finish(),
            Self::UpdateMemberBot(user, bot, changes) => f
                .debug_tuple("UpdateMemberBot")
                .field(user)
                .field(bot)
                .field(changes)
                .finish(),
            Self::MemberMessages(user, bot, before) => f
                .debug_tuple("MemberMessages")
                .field(user)
                .field(bot)
                .field(before)
                .finish(),
            Self::SetTimeZone(zone) => f.debug_tuple("SetTimeZone").field(zone).finish(),
            Self::LoadNotes(bot) => f.debug_tuple("LoadNotes").field(bot).finish(),
            Self::ClearNotes(bot) => f.debug_tuple("ClearNotes").field(bot).finish(),
            Self::LoadToolCalls(bot, before) => f
                .debug_tuple("LoadToolCalls")
                .field(bot)
                .field(before)
                .finish(),
            Self::ListRoutines(bot) => f.debug_tuple("ListRoutines").field(bot).finish(),
            Self::CreateRoutine(bot, routine) => f
                .debug_tuple("CreateRoutine")
                .field(bot)
                .field(routine)
                .finish(),
            Self::UpdateRoutine(bot, routine, changes) => f
                .debug_tuple("UpdateRoutine")
                .field(bot)
                .field(routine)
                .field(changes)
                .finish(),
            Self::DeleteRoutine(bot, routine) => f
                .debug_tuple("DeleteRoutine")
                .field(bot)
                .field(routine)
                .finish(),
            Self::RunRoutine(bot, routine) => f
                .debug_tuple("RunRoutine")
                .field(bot)
                .field(routine)
                .finish(),
            Self::LoadRuns(bot, routine, before) => f
                .debug_tuple("LoadRuns")
                .field(bot)
                .field(routine)
                .field(before)
                .finish(),
            Self::LoadRecentRuns(bot, routine) => f
                .debug_tuple("LoadRecentRuns")
                .field(bot)
                .field(routine)
                .finish(),
            Self::LoadBot(bot) => f.debug_tuple("LoadBot").field(bot).finish(),
            Self::MarkRead(bot, read) => f
                .debug_tuple("MarkRead")
                .field(bot)
                .field(&read.message_id)
                .finish(),
            Self::PushKey => f.write_str("PushKey"),
            Self::PutPushSubscription(_) => f.write_str("PutPushSubscription(<redacted>)"),
            Self::DeletePushSubscription => f.write_str("DeletePushSubscription"),
        }
    }
}

/// An HTTP method the client uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

/// The answer to an [`ApiRequest`], decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiReply {
    Session(Result<SessionResponse, CommandError>),
    SignedIn(Result<SessionResponse, CommandError>),
    SignedOut(Result<(), CommandError>),
    Bots(Result<BotList, CommandError>),
    Messages(BotId, Result<MessagePage, CommandError>),
    /// The page before the message named, for that Bot.
    Older(BotId, MessageId, Result<MessagePage, CommandError>),
    Sent(BotId, Result<SendMessageResponse, CommandError>),
    Stopped(BotId, Result<StopResponse, CommandError>),
    Usage(Result<Usage, CommandError>),
    Created(Result<Bot, CommandError>),
    BotUpdated(BotId, Result<Bot, CommandError>),
    Acknowledged(Result<(), CommandError>),
    Account(Result<CodexAccount, CommandError>),
    /// A sign-in just started.
    Login(Result<DeviceCode, CommandError>),
    /// Where the latest sign-in stands.
    LoginStatus(Result<DeviceCode, CommandError>),
    /// The sign-in as cancelling left it.
    LoginCancelled(Result<DeviceCode, CommandError>),
    HouseholdSignedOut(Result<(), CommandError>),
    /// An invitation accepted: the new account, signed in.
    Joined(Result<SessionResponse, CommandError>),
    Accounts(Result<AdminUserList, CommandError>),
    AccountUpdated(UserId, Result<AdminUser, CommandError>),
    PasswordReset(UserId, Result<(), CommandError>),
    AccountUsage(UserId, Result<Allowance, CommandError>),
    Invitations(Result<InvitationList, CommandError>),
    /// A new invitation, with its code, which is shown once.
    Invited(Result<Invitation, CommandError>),
    InvitationWithdrawn(i64, Result<(), CommandError>),
    MemberBots(UserId, Result<MemberBotList, CommandError>),
    MemberBotCreated(UserId, Result<MemberBot, CommandError>),
    MemberBotUpdated(UserId, BotId, Result<MemberBot, CommandError>),
    MemberMessages(
        UserId,
        BotId,
        Option<MessageId>,
        Result<MessagePage, CommandError>,
    ),
    TimeZoneSet(Result<SessionResponse, CommandError>),
    Notes(BotId, Result<BotNotes, CommandError>),
    NotesCleared(BotId, Result<(), CommandError>),
    /// A page of tool calls, requested with that cursor.
    ToolCalls(BotId, Option<i64>, Result<ToolCallPage, CommandError>),
    Routines(BotId, Result<RoutineList, CommandError>),
    RoutineCreated(BotId, Result<Routine, CommandError>),
    RoutineUpdated(BotId, RoutineId, Result<Routine, CommandError>),
    RoutineDeleted(BotId, RoutineId, Result<(), CommandError>),
    RoutineStarted(BotId, RoutineId, Result<RoutineRun, CommandError>),
    /// A page of a routine's runs, requested with that cursor.
    Runs(
        BotId,
        RoutineId,
        Option<RoutineRunId>,
        Result<RoutineRunPage, CommandError>,
    ),
    /// A routine's newest few runs, for the recent runs.
    RecentRuns(BotId, RoutineId, Result<RoutineRunPage, CommandError>),
    /// One Bot, as asked for to change its grants.
    Bot(BotId, Result<Bot, CommandError>),
    /// The read marker after asking to move it to that message.
    MarkedRead(BotId, MessageId, Result<ReadState, CommandError>),
    PushKey(Result<PushKey, CommandError>),
    PushSubscribed(Result<PushSubscriptionState, CommandError>),
    PushUnsubscribed(Result<(), CommandError>),
}

/// A new invitation. Its code is shown on screen once and nowhere else;
/// [`CreatedInvitation`]'s `Debug` leaves it out.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(transparent)]
pub struct Invitation(pub CreatedInvitation);

/// A device-code sign-in as the gateway started it. The code is shown on
/// screen and nowhere else, so its `Debug` leaves it out.
#[derive(Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(transparent)]
pub struct DeviceCode(pub DeviceCodeLogin);

impl std::fmt::Debug for DeviceCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceCode")
            .field("verification_url", &self.0.verification_url)
            .field("user_code", &"<redacted>")
            .field("state", &self.0.state)
            .field("expires_at", &self.0.expires_at)
            .field("error", &self.0.error)
            .finish()
    }
}

impl ApiRequest {
    pub fn method(&self) -> Method {
        match self {
            Self::CheckSession
            | Self::ListBots
            | Self::LoadMessages(_)
            | Self::LoadOlder(..)
            | Self::LoadUsage
            | Self::CodexAccount
            | Self::CodexLoginStatus
            | Self::ListAccounts
            | Self::AccountUsage(_)
            | Self::ListInvitations
            | Self::MemberBots(_)
            | Self::MemberMessages(..)
            | Self::LoadNotes(_)
            | Self::LoadToolCalls(..)
            | Self::ListRoutines(_)
            | Self::LoadRuns(..)
            | Self::LoadRecentRuns(..)
            | Self::LoadBot(_)
            | Self::PushKey => Method::Get,
            Self::SignIn(_)
            | Self::SendMessage(..)
            | Self::StopBot(_)
            | Self::CreateBot(_)
            | Self::CodexLogin
            | Self::CodexLogout
            | Self::AcceptInvitation(_)
            | Self::CreateInvitation(_)
            | Self::CreateMemberBot(..)
            | Self::CreateRoutine(..)
            | Self::RunRoutine(..) => Method::Post,
            Self::ResetPassword(..)
            | Self::SetTimeZone(_)
            | Self::MarkRead(..)
            | Self::PutPushSubscription(_) => Method::Put,
            Self::UpdateBot(..)
            | Self::UpdateAccount(..)
            | Self::UpdateMemberBot(..)
            | Self::UpdateRoutine(..) => Method::Patch,
            Self::SignOut
            | Self::AcknowledgeReset
            | Self::CancelCodexLogin
            | Self::WithdrawInvitation(_)
            | Self::ClearNotes(_)
            | Self::DeleteRoutine(..)
            | Self::DeletePushSubscription => Method::Delete,
        }
    }

    /// The path, relative to the page's origin.
    pub fn path(&self) -> String {
        let route = match self {
            Self::CheckSession | Self::SignIn(_) | Self::SignOut => "/session".into(),
            Self::ListBots | Self::CreateBot(_) => "/bots".into(),
            Self::AcknowledgeReset => "/session/password-reset".into(),
            Self::UpdateBot(bot, _) | Self::LoadBot(bot) => format!("/bots/{bot}"),
            Self::MarkRead(bot, _) => format!("/bots/{bot}/read"),
            Self::PushKey => "/push/key".into(),
            Self::PutPushSubscription(_) | Self::DeletePushSubscription => {
                "/push/subscription".into()
            }
            Self::CodexAccount => "/codex/account".into(),
            Self::CodexLogin | Self::CodexLoginStatus | Self::CancelCodexLogin => {
                "/codex/login".into()
            }
            Self::CodexLogout => "/codex/logout".into(),
            Self::AcceptInvitation(_) => "/invitations/accept".into(),
            Self::ListAccounts => "/admin/users".into(),
            Self::UpdateAccount(user, _) => format!("/admin/users/{user}"),
            Self::ResetPassword(user, _) => format!("/admin/users/{user}/password"),
            Self::AccountUsage(user) => format!("/admin/users/{user}/usage"),
            Self::ListInvitations | Self::CreateInvitation(_) => "/admin/invitations".into(),
            Self::WithdrawInvitation(id) => format!("/admin/invitations/{id}"),
            Self::MemberBots(user) | Self::CreateMemberBot(user, _) => {
                format!("/admin/users/{user}/bots")
            }
            Self::UpdateMemberBot(user, bot, _) => format!("/admin/users/{user}/bots/{bot}"),
            Self::MemberMessages(user, bot, None) => {
                format!("/admin/users/{user}/bots/{bot}/messages")
            }
            Self::MemberMessages(user, bot, Some(before)) => {
                format!("/admin/users/{user}/bots/{bot}/messages?before={before}")
            }
            Self::LoadMessages(bot) | Self::SendMessage(bot, _) => format!("/bots/{bot}/messages"),
            Self::LoadOlder(bot, before) => format!("/bots/{bot}/messages?before={before}"),
            Self::StopBot(bot) => format!("/bots/{bot}/stop"),
            Self::LoadUsage => "/usage".into(),
            Self::SetTimeZone(_) => "/session/time-zone".into(),
            Self::LoadNotes(bot) | Self::ClearNotes(bot) => format!("/bots/{bot}/notes"),
            Self::LoadToolCalls(bot, None) => format!("/bots/{bot}/tool-calls?limit={PAGE_SIZE}"),
            Self::LoadToolCalls(bot, Some(before)) => {
                format!("/bots/{bot}/tool-calls?limit={PAGE_SIZE}&before={before}")
            }
            Self::ListRoutines(bot) | Self::CreateRoutine(bot, _) => {
                format!("/bots/{bot}/routines")
            }
            Self::UpdateRoutine(bot, routine, _) | Self::DeleteRoutine(bot, routine) => {
                format!("/bots/{bot}/routines/{routine}")
            }
            Self::RunRoutine(bot, routine) => format!("/bots/{bot}/routines/{routine}/run"),
            Self::LoadRuns(bot, routine, None) => {
                format!("/bots/{bot}/routines/{routine}/runs?limit={PAGE_SIZE}")
            }
            Self::LoadRuns(bot, routine, Some(before)) => {
                format!("/bots/{bot}/routines/{routine}/runs?limit={PAGE_SIZE}&before={before}")
            }
            Self::LoadRecentRuns(bot, routine) => {
                format!("/bots/{bot}/routines/{routine}/runs?limit={RECENT_RUNS}")
            }
        };
        format!("{API_PREFIX}{route}")
    }

    /// The JSON body, if the command has one.
    pub fn body(&self) -> Option<String> {
        match self {
            Self::SignIn(login) => Some(json(login)),
            Self::SendMessage(_, message) => Some(json(message)),
            Self::CreateBot(bot) => Some(json(bot)),
            Self::UpdateBot(_, changes) => Some(json(changes)),
            Self::AcceptInvitation(accept) => Some(json(accept)),
            Self::UpdateAccount(_, changes) => Some(json(changes)),
            Self::ResetPassword(_, reset) => Some(json(reset)),
            Self::CreateInvitation(invitation) => Some(json(invitation)),
            Self::CreateMemberBot(_, bot) => Some(json(bot)),
            Self::UpdateMemberBot(_, _, changes) => Some(json(changes)),
            Self::SetTimeZone(zone) => Some(json(zone)),
            Self::CreateRoutine(_, routine) => Some(json(routine)),
            Self::UpdateRoutine(_, _, changes) => Some(json(changes)),
            Self::MarkRead(_, read) => Some(json(read)),
            Self::PutPushSubscription(subscription) => Some(json(subscription)),
            _ => None,
        }
    }

    /// Decode the response to this command.
    pub fn decode(&self, status: u16, body: &str) -> ApiReply {
        match self {
            Self::CheckSession => ApiReply::Session(wire::decode_response(status, body)),
            Self::SignIn(_) => ApiReply::SignedIn(wire::decode_response(status, body)),
            Self::SignOut => ApiReply::SignedOut(wire::decode_empty(status, body)),
            Self::ListBots => ApiReply::Bots(wire::decode_response(status, body)),
            Self::LoadMessages(bot) => {
                ApiReply::Messages(*bot, wire::decode_response(status, body))
            }
            Self::LoadOlder(bot, before) => {
                ApiReply::Older(*bot, *before, wire::decode_response(status, body))
            }
            Self::SendMessage(bot, _) => ApiReply::Sent(*bot, wire::decode_response(status, body)),
            Self::StopBot(bot) => ApiReply::Stopped(*bot, wire::decode_response(status, body)),
            Self::LoadUsage => ApiReply::Usage(wire::decode_response(status, body)),
            Self::CreateBot(_) => ApiReply::Created(wire::decode_response(status, body)),
            Self::UpdateBot(bot, _) => {
                ApiReply::BotUpdated(*bot, wire::decode_response(status, body))
            }
            Self::AcknowledgeReset => ApiReply::Acknowledged(wire::decode_empty(status, body)),
            Self::CodexAccount => ApiReply::Account(wire::decode_response(status, body)),
            Self::CodexLogin => ApiReply::Login(wire::decode_response(status, body)),
            Self::CodexLoginStatus => ApiReply::LoginStatus(wire::decode_response(status, body)),
            Self::CancelCodexLogin => ApiReply::LoginCancelled(wire::decode_response(status, body)),
            Self::CodexLogout => ApiReply::HouseholdSignedOut(wire::decode_empty(status, body)),
            Self::AcceptInvitation(_) => ApiReply::Joined(wire::decode_response(status, body)),
            Self::ListAccounts => ApiReply::Accounts(wire::decode_response(status, body)),
            Self::UpdateAccount(user, _) => {
                ApiReply::AccountUpdated(*user, wire::decode_response(status, body))
            }
            Self::ResetPassword(user, _) => {
                ApiReply::PasswordReset(*user, wire::decode_empty(status, body))
            }
            Self::AccountUsage(user) => {
                ApiReply::AccountUsage(*user, wire::decode_response(status, body))
            }
            Self::ListInvitations => ApiReply::Invitations(wire::decode_response(status, body)),
            Self::CreateInvitation(_) => ApiReply::Invited(wire::decode_response(status, body)),
            Self::WithdrawInvitation(id) => {
                ApiReply::InvitationWithdrawn(*id, wire::decode_empty(status, body))
            }
            Self::MemberBots(user) => {
                ApiReply::MemberBots(*user, wire::decode_response(status, body))
            }
            Self::CreateMemberBot(user, _) => {
                ApiReply::MemberBotCreated(*user, wire::decode_response(status, body))
            }
            Self::UpdateMemberBot(user, bot, _) => {
                ApiReply::MemberBotUpdated(*user, *bot, wire::decode_response(status, body))
            }
            Self::MemberMessages(user, bot, before) => {
                ApiReply::MemberMessages(*user, *bot, *before, wire::decode_response(status, body))
            }
            Self::SetTimeZone(_) => ApiReply::TimeZoneSet(wire::decode_response(status, body)),
            Self::LoadNotes(bot) => ApiReply::Notes(*bot, wire::decode_response(status, body)),
            Self::ClearNotes(bot) => ApiReply::NotesCleared(*bot, wire::decode_empty(status, body)),
            Self::LoadToolCalls(bot, before) => {
                ApiReply::ToolCalls(*bot, *before, wire::decode_response(status, body))
            }
            Self::ListRoutines(bot) => {
                ApiReply::Routines(*bot, wire::decode_response(status, body))
            }
            Self::CreateRoutine(bot, _) => {
                ApiReply::RoutineCreated(*bot, wire::decode_response(status, body))
            }
            Self::UpdateRoutine(bot, routine, _) => {
                ApiReply::RoutineUpdated(*bot, *routine, wire::decode_response(status, body))
            }
            Self::DeleteRoutine(bot, routine) => {
                ApiReply::RoutineDeleted(*bot, *routine, wire::decode_empty(status, body))
            }
            Self::RunRoutine(bot, routine) => {
                ApiReply::RoutineStarted(*bot, *routine, wire::decode_response(status, body))
            }
            Self::LoadRuns(bot, routine, before) => {
                ApiReply::Runs(*bot, *routine, *before, wire::decode_response(status, body))
            }
            Self::LoadRecentRuns(bot, routine) => {
                ApiReply::RecentRuns(*bot, *routine, wire::decode_response(status, body))
            }
            Self::LoadBot(bot) => ApiReply::Bot(*bot, wire::decode_response(status, body)),
            Self::MarkRead(bot, read) => {
                ApiReply::MarkedRead(*bot, read.message_id, wire::decode_response(status, body))
            }
            Self::PushKey => ApiReply::PushKey(wire::decode_response(status, body)),
            Self::PutPushSubscription(_) => {
                ApiReply::PushSubscribed(wire::decode_response(status, body))
            }
            Self::DeletePushSubscription => {
                ApiReply::PushUnsubscribed(wire::decode_empty(status, body))
            }
        }
    }

    /// The reply when no response arrived at all.
    pub fn unreachable(&self) -> ApiReply {
        let error = CommandError::unreachable();
        match self {
            Self::CheckSession => ApiReply::Session(Err(error)),
            Self::SignIn(_) => ApiReply::SignedIn(Err(error)),
            Self::SignOut => ApiReply::SignedOut(Err(error)),
            Self::ListBots => ApiReply::Bots(Err(error)),
            Self::LoadMessages(bot) => ApiReply::Messages(*bot, Err(error)),
            Self::LoadOlder(bot, before) => ApiReply::Older(*bot, *before, Err(error)),
            Self::SendMessage(bot, _) => ApiReply::Sent(*bot, Err(error)),
            Self::StopBot(bot) => ApiReply::Stopped(*bot, Err(error)),
            Self::LoadUsage => ApiReply::Usage(Err(error)),
            Self::CreateBot(_) => ApiReply::Created(Err(error)),
            Self::UpdateBot(bot, _) => ApiReply::BotUpdated(*bot, Err(error)),
            Self::AcknowledgeReset => ApiReply::Acknowledged(Err(error)),
            Self::CodexAccount => ApiReply::Account(Err(error)),
            Self::CodexLogin => ApiReply::Login(Err(error)),
            Self::CodexLoginStatus => ApiReply::LoginStatus(Err(error)),
            Self::CancelCodexLogin => ApiReply::LoginCancelled(Err(error)),
            Self::CodexLogout => ApiReply::HouseholdSignedOut(Err(error)),
            Self::AcceptInvitation(_) => ApiReply::Joined(Err(error)),
            Self::ListAccounts => ApiReply::Accounts(Err(error)),
            Self::UpdateAccount(user, _) => ApiReply::AccountUpdated(*user, Err(error)),
            Self::ResetPassword(user, _) => ApiReply::PasswordReset(*user, Err(error)),
            Self::AccountUsage(user) => ApiReply::AccountUsage(*user, Err(error)),
            Self::ListInvitations => ApiReply::Invitations(Err(error)),
            Self::CreateInvitation(_) => ApiReply::Invited(Err(error)),
            Self::WithdrawInvitation(id) => ApiReply::InvitationWithdrawn(*id, Err(error)),
            Self::MemberBots(user) => ApiReply::MemberBots(*user, Err(error)),
            Self::CreateMemberBot(user, _) => ApiReply::MemberBotCreated(*user, Err(error)),
            Self::UpdateMemberBot(user, bot, _) => {
                ApiReply::MemberBotUpdated(*user, *bot, Err(error))
            }
            Self::MemberMessages(user, bot, before) => {
                ApiReply::MemberMessages(*user, *bot, *before, Err(error))
            }
            Self::SetTimeZone(_) => ApiReply::TimeZoneSet(Err(error)),
            Self::LoadNotes(bot) => ApiReply::Notes(*bot, Err(error)),
            Self::ClearNotes(bot) => ApiReply::NotesCleared(*bot, Err(error)),
            Self::LoadToolCalls(bot, before) => ApiReply::ToolCalls(*bot, *before, Err(error)),
            Self::ListRoutines(bot) => ApiReply::Routines(*bot, Err(error)),
            Self::CreateRoutine(bot, _) => ApiReply::RoutineCreated(*bot, Err(error)),
            Self::UpdateRoutine(bot, routine, _) => {
                ApiReply::RoutineUpdated(*bot, *routine, Err(error))
            }
            Self::DeleteRoutine(bot, routine) => {
                ApiReply::RoutineDeleted(*bot, *routine, Err(error))
            }
            Self::RunRoutine(bot, routine) => ApiReply::RoutineStarted(*bot, *routine, Err(error)),
            Self::LoadRuns(bot, routine, before) => {
                ApiReply::Runs(*bot, *routine, *before, Err(error))
            }
            Self::LoadRecentRuns(bot, routine) => ApiReply::RecentRuns(*bot, *routine, Err(error)),
            Self::LoadBot(bot) => ApiReply::Bot(*bot, Err(error)),
            Self::MarkRead(bot, read) => ApiReply::MarkedRead(*bot, read.message_id, Err(error)),
            Self::PushKey => ApiReply::PushKey(Err(error)),
            Self::PutPushSubscription(_) => ApiReply::PushSubscribed(Err(error)),
            Self::DeletePushSubscription => ApiReply::PushUnsubscribed(Err(error)),
        }
    }
}

fn json(body: &impl serde::Serialize) -> String {
    serde_json::to_string(body).expect("request bodies serialise")
}

/// Which sign-in a command belongs to. It changes whenever the user signs in
/// or out, so a reply that arrives after that, possibly with another user's
/// data, is recognised and dropped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Generation(pub u64);

/// Something that carries commands to the gateway and reports each reply
/// back to the application as an intent, tagged with the command's
/// generation. It also runs the event socket
/// ([`crate::socket::Driver`]), whose updates arrive as intents tagged the
/// same way.
pub trait Api {
    fn send(&self, generation: Generation, request: ApiRequest);

    /// Open the event socket for the sign-in `generation`, replacing any
    /// other, with a connection that has never been live.
    fn connect_events(&self, generation: Generation);

    /// Close the event socket for good, as on signing out.
    fn disconnect_events(&self);

    /// Reload the page, to fetch the client that matches the gateway.
    fn reload_page(&self);

    /// The time now, for saying how long until a limit resets.
    fn now(&self) -> Timestamp;

    /// The viewer's time zone and locale, read again each time it is
    /// called. The application shows times in the user's own zone when it
    /// knows it, in this locale.
    fn local_time(&self) -> stobie_ui_egui::local_time::LocalTime;

    /// The Bot whose conversation the application shows, or `None`. The
    /// event socket tells the gateway, as `viewing`, while the page is
    /// visible, and `None` while it is hidden.
    fn set_viewing(&self, bot: Option<BotId>);

    /// Subscribe this device to Web Push with the gateway's key. Call it
    /// directly from the action the user tapped: Safari subscribes only
    /// inside the tap. The result arrives as
    /// [`BokkieIntent::Push`](crate::BokkieIntent::Push).
    fn subscribe_push(&self, key: &str);

    /// Remove this device's Web Push subscription, if it has one.
    fn unsubscribe_push(&self);

    /// Show `count` on the app's icon, or clear it for 0.
    fn set_app_badge(&self, count: u64);
}

/// What the application asked of the event socket, as a [`Recorder`] keeps
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketControl {
    Connect(Generation),
    Disconnect,
    ReloadPage,
}

/// What the application asked of the device's Web Push and badge, as a
/// [`Recorder`] keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceControl {
    Subscribe(String),
    Unsubscribe,
    Badge(u64),
}

/// An [`Api`] that only records what it was asked to send, for tests and for
/// the native build, which has no network transport.
#[derive(Debug, Clone, Default)]
pub struct Recorder {
    sent: std::rc::Rc<std::cell::RefCell<Vec<ApiRequest>>>,
    socket: std::rc::Rc<std::cell::RefCell<Vec<SocketControl>>>,
    device: std::rc::Rc<std::cell::RefCell<Vec<DeviceControl>>>,
    viewing: std::rc::Rc<std::cell::RefCell<Vec<Option<BotId>>>>,
    now: std::rc::Rc<std::cell::Cell<i64>>,
}

impl Recorder {
    /// Every command sent so far, which is then forgotten.
    pub fn take(&self) -> Vec<ApiRequest> {
        std::mem::take(&mut self.sent.borrow_mut())
    }

    /// Set the time [`Api::now`] reports.
    pub fn set_now(&self, now: Timestamp) {
        self.now.set(now.0);
    }

    /// Everything asked of the event socket so far, which is then forgotten.
    pub fn take_socket(&self) -> Vec<SocketControl> {
        std::mem::take(&mut self.socket.borrow_mut())
    }

    /// Everything asked of Web Push and the badge so far, which is then
    /// forgotten.
    pub fn take_device(&self) -> Vec<DeviceControl> {
        std::mem::take(&mut self.device.borrow_mut())
    }

    /// Each Bot the application said it shows, or none, so far, which is
    /// then forgotten.
    pub fn take_viewing(&self) -> Vec<Option<BotId>> {
        std::mem::take(&mut self.viewing.borrow_mut())
    }
}

impl Api for Recorder {
    fn send(&self, _generation: Generation, request: ApiRequest) {
        self.sent.borrow_mut().push(request);
    }

    fn connect_events(&self, generation: Generation) {
        self.socket
            .borrow_mut()
            .push(SocketControl::Connect(generation));
    }

    fn disconnect_events(&self) {
        self.socket.borrow_mut().push(SocketControl::Disconnect);
    }

    fn reload_page(&self) {
        self.socket.borrow_mut().push(SocketControl::ReloadPage);
    }

    fn now(&self) -> Timestamp {
        Timestamp(self.now.get())
    }

    /// UTC in Australian English, so that tests show the user's own zone
    /// wins over the viewer's.
    fn local_time(&self) -> stobie_ui_egui::local_time::LocalTime {
        use stobie_ui_egui::local_time::{Clock, LocalTime, TimeLocale, TimeZone};
        LocalTime::new(
            TimeZone::utc(),
            TimeLocale::for_language_tag("en-AU"),
            Clock::System,
        )
    }

    fn set_viewing(&self, bot: Option<BotId>) {
        self.viewing.borrow_mut().push(bot);
    }

    fn subscribe_push(&self, key: &str) {
        self.device
            .borrow_mut()
            .push(DeviceControl::Subscribe(key.to_owned()));
    }

    fn unsubscribe_push(&self) {
        self.device.borrow_mut().push(DeviceControl::Unsubscribe);
    }

    fn set_app_badge(&self, count: u64) {
        self.device.borrow_mut().push(DeviceControl::Badge(count));
    }
}

#[cfg(test)]
mod tests {
    use bokkie_protocol::api::ErrorCode;

    use super::*;

    #[test]
    fn commands_name_their_routes() {
        let login = ApiRequest::SignIn(LoginRequest {
            username: "ada".into(),
            password: "a long password".into(),
        });
        let routes: Vec<(&str, String)> = [
            ApiRequest::CheckSession,
            login.clone(),
            ApiRequest::SignOut,
            ApiRequest::ListBots,
            ApiRequest::LoadMessages(BotId(3)),
            ApiRequest::LoadOlder(BotId(3), MessageId(41)),
            ApiRequest::SendMessage(
                BotId(3),
                SendMessageRequest {
                    text: "Hello".into(),
                },
            ),
            ApiRequest::StopBot(BotId(3)),
            ApiRequest::LoadUsage,
            ApiRequest::CreateBot(CreateBotRequest {
                name: "Gardener".into(),
                label: String::new(),
                description: String::new(),
                avatar: None,
            }),
            ApiRequest::CodexAccount,
            ApiRequest::CodexLogin,
            ApiRequest::UpdateBot(
                BotId(3),
                UpdateBotRequest {
                    pinned: Some(true),
                    ..Default::default()
                },
            ),
            ApiRequest::AcknowledgeReset,
            ApiRequest::CodexLoginStatus,
            ApiRequest::CancelCodexLogin,
            ApiRequest::CodexLogout,
        ]
        .iter()
        .map(|request| (request.method().as_str(), request.path()))
        .collect();
        assert_eq!(
            routes,
            [
                ("GET", "/api/v1/session".to_owned()),
                ("POST", "/api/v1/session".to_owned()),
                ("DELETE", "/api/v1/session".to_owned()),
                ("GET", "/api/v1/bots".to_owned()),
                ("GET", "/api/v1/bots/3/messages".to_owned()),
                ("GET", "/api/v1/bots/3/messages?before=41".to_owned()),
                ("POST", "/api/v1/bots/3/messages".to_owned()),
                ("POST", "/api/v1/bots/3/stop".to_owned()),
                ("GET", "/api/v1/usage".to_owned()),
                ("POST", "/api/v1/bots".to_owned()),
                ("GET", "/api/v1/codex/account".to_owned()),
                ("POST", "/api/v1/codex/login".to_owned()),
                ("PATCH", "/api/v1/bots/3".to_owned()),
                ("DELETE", "/api/v1/session/password-reset".to_owned()),
                ("GET", "/api/v1/codex/login".to_owned()),
                ("DELETE", "/api/v1/codex/login".to_owned()),
                ("POST", "/api/v1/codex/logout".to_owned()),
            ]
        );
        assert_eq!(
            ApiRequest::UpdateBot(
                BotId(3),
                UpdateBotRequest {
                    hidden: Some(false),
                    ..Default::default()
                }
            )
            .body()
            .as_deref(),
            Some(r#"{"hidden":false}"#)
        );
        assert_eq!(
            ApiRequest::SendMessage(
                BotId(3),
                SendMessageRequest {
                    text: "Hello".into()
                }
            )
            .body()
            .as_deref(),
            Some(r#"{"text":"Hello"}"#)
        );
        assert_eq!(
            login.body().as_deref(),
            Some(r#"{"username":"ada","password":"a long password"}"#)
        );
        assert_eq!(ApiRequest::ListBots.body(), None);
        assert!(!format!("{login:?}").contains("a long password"));
    }

    #[test]
    fn a_new_bot_is_named_only_and_a_device_code_never_reaches_a_log() {
        let create = ApiRequest::CreateBot(CreateBotRequest {
            name: "Gardener".into(),
            label: String::new(),
            description: String::new(),
            avatar: None,
        });
        assert_eq!(
            create.body().as_deref(),
            Some(r#"{"name":"Gardener","label":"","description":""}"#)
        );
        let reply = ApiRequest::CodexLogin.decode(
            200,
            r#"{"verification_url":"https://auth.example/device","user_code":"WXYZ-1234","state":"pending","expires_at":1800000900000}"#,
        );
        let ApiReply::Login(Ok(login)) = &reply else {
            panic!("a device code");
        };
        assert_eq!(login.0.user_code, "WXYZ-1234");
        let logged = format!("{reply:?}");
        assert!(!logged.contains("WXYZ-1234"), "{logged}");
        assert!(logged.contains("https://auth.example/device"));
        // Its status and cancellation carry the code too, and keep it out.
        for request in [ApiRequest::CodexLoginStatus, ApiRequest::CancelCodexLogin] {
            let reply = request.decode(
                200,
                r#"{"verification_url":"https://auth.example/device","user_code":"WXYZ-1234","state":"expired","expires_at":1800000900000}"#,
            );
            let logged = format!("{reply:?}");
            assert!(!logged.contains("WXYZ-1234"), "{logged}");
            assert!(logged.contains("Expired"), "{logged}");
        }
    }

    #[test]
    fn admin_commands_name_their_routes_and_keep_their_secrets() {
        use bokkie_protocol::api::{
            AcceptInvitationRequest, CreateInvitationRequest, Permissions, ResetPasswordRequest,
            Role, UpdateBotProfileRequest, UpdateUserRequest,
        };
        let user = UserId(2);
        let reset = ApiRequest::ResetPassword(
            user,
            ResetPasswordRequest {
                new_password: "a new long password".into(),
            },
        );
        let accept = ApiRequest::AcceptInvitation(AcceptInvitationRequest {
            code: "the-code".into(),
            username: "grace".into(),
            password: "a long password".into(),
            display_name: "Grace".into(),
        });
        let routes: Vec<(&str, String)> = [
            accept.clone(),
            ApiRequest::ListAccounts,
            ApiRequest::UpdateAccount(user, UpdateUserRequest::default()),
            reset.clone(),
            ApiRequest::AccountUsage(user),
            ApiRequest::ListInvitations,
            ApiRequest::CreateInvitation(CreateInvitationRequest {
                role: Role::Member,
                permissions: Permissions::default(),
                display_name: None,
                expires_in_hours: None,
            }),
            ApiRequest::WithdrawInvitation(4),
            ApiRequest::MemberBots(user),
            ApiRequest::CreateMemberBot(
                user,
                CreateBotRequest {
                    name: "Coach".into(),
                    label: String::new(),
                    description: String::new(),
                    avatar: None,
                },
            ),
            ApiRequest::UpdateMemberBot(user, BotId(7), UpdateBotProfileRequest::default()),
            ApiRequest::MemberMessages(user, BotId(7), None),
            ApiRequest::MemberMessages(user, BotId(7), Some(MessageId(11))),
        ]
        .iter()
        .map(|request| (request.method().as_str(), request.path()))
        .collect();
        assert_eq!(
            routes,
            [
                ("POST", "/api/v1/invitations/accept".to_owned()),
                ("GET", "/api/v1/admin/users".to_owned()),
                ("PATCH", "/api/v1/admin/users/2".to_owned()),
                ("PUT", "/api/v1/admin/users/2/password".to_owned()),
                ("GET", "/api/v1/admin/users/2/usage".to_owned()),
                ("GET", "/api/v1/admin/invitations".to_owned()),
                ("POST", "/api/v1/admin/invitations".to_owned()),
                ("DELETE", "/api/v1/admin/invitations/4".to_owned()),
                ("GET", "/api/v1/admin/users/2/bots".to_owned()),
                ("POST", "/api/v1/admin/users/2/bots".to_owned()),
                ("PATCH", "/api/v1/admin/users/2/bots/7".to_owned()),
                ("GET", "/api/v1/admin/users/2/bots/7/messages".to_owned()),
                (
                    "GET",
                    "/api/v1/admin/users/2/bots/7/messages?before=11".to_owned()
                ),
            ]
        );
        assert_eq!(
            reset.body().as_deref(),
            Some(r#"{"new_password":"a new long password"}"#)
        );
        let logged = format!("{reset:?} {accept:?}");
        for secret in ["a new long password", "a long password", "the-code"] {
            assert!(!logged.contains(secret), "{logged}");
        }
        let invited = ApiRequest::CreateInvitation(CreateInvitationRequest {
            role: Role::Member,
            permissions: Permissions::default(),
            display_name: None,
            expires_in_hours: None,
        })
        .decode(
            201,
            r#"{"id":5,"code":"the-code","role":"member","expires_at":1800000900000}"#,
        );
        let ApiReply::Invited(Ok(invitation)) = &invited else {
            panic!("an invitation");
        };
        assert_eq!(invitation.0.code, "the-code");
        assert!(!format!("{invited:?}").contains("the-code"));
    }

    #[test]
    fn settings_and_routine_commands_name_their_routes() {
        use bokkie_protocol::api::{Schedule, TimeOfDay};
        let bot = BotId(3);
        let routine = RoutineId(5);
        let create = ApiRequest::CreateRoutine(
            bot,
            CreateRoutineRequest {
                name: "Morning digest".into(),
                prompt: "Summarise the news.".into(),
                schedule: Schedule::Daily {
                    at: TimeOfDay::new(7, 0).expect("a time"),
                },
                push: Default::default(),
                paused: false,
            },
        );
        let routes: Vec<(&str, String)> = [
            ApiRequest::SetTimeZone(SetTimeZoneRequest {
                time_zone: "Europe/London".into(),
            }),
            ApiRequest::LoadNotes(bot),
            ApiRequest::ClearNotes(bot),
            ApiRequest::LoadToolCalls(bot, None),
            ApiRequest::LoadToolCalls(bot, Some(40)),
            ApiRequest::ListRoutines(bot),
            create.clone(),
            ApiRequest::UpdateRoutine(bot, routine, UpdateRoutineRequest::default()),
            ApiRequest::DeleteRoutine(bot, routine),
            ApiRequest::RunRoutine(bot, routine),
            ApiRequest::LoadRuns(bot, routine, None),
            ApiRequest::LoadRuns(bot, routine, Some(RoutineRunId(9))),
            ApiRequest::LoadRecentRuns(bot, routine),
        ]
        .iter()
        .map(|request| (request.method().as_str(), request.path()))
        .collect();
        assert_eq!(
            routes,
            [
                ("PUT", "/api/v1/session/time-zone".to_owned()),
                ("GET", "/api/v1/bots/3/notes".to_owned()),
                ("DELETE", "/api/v1/bots/3/notes".to_owned()),
                ("GET", "/api/v1/bots/3/tool-calls?limit=20".to_owned()),
                (
                    "GET",
                    "/api/v1/bots/3/tool-calls?limit=20&before=40".to_owned()
                ),
                ("GET", "/api/v1/bots/3/routines".to_owned()),
                ("POST", "/api/v1/bots/3/routines".to_owned()),
                ("PATCH", "/api/v1/bots/3/routines/5".to_owned()),
                ("DELETE", "/api/v1/bots/3/routines/5".to_owned()),
                ("POST", "/api/v1/bots/3/routines/5/run".to_owned()),
                ("GET", "/api/v1/bots/3/routines/5/runs?limit=20".to_owned()),
                (
                    "GET",
                    "/api/v1/bots/3/routines/5/runs?limit=20&before=9".to_owned()
                ),
                ("GET", "/api/v1/bots/3/routines/5/runs?limit=5".to_owned()),
            ]
        );
        assert_eq!(
            create.body().as_deref(),
            Some(
                r#"{"name":"Morning digest","prompt":"Summarise the news.","schedule":{"repeat":"daily","at":"07:00"},"push":"every_run","paused":false}"#
            )
        );
        assert_eq!(
            ApiRequest::UpdateRoutine(
                bot,
                routine,
                UpdateRoutineRequest {
                    paused: Some(true),
                    ..Default::default()
                }
            )
            .body()
            .as_deref(),
            Some(r#"{"paused":true}"#)
        );
        assert_eq!(
            ApiRequest::ClearNotes(bot).decode(204, ""),
            ApiReply::NotesCleared(bot, Ok(()))
        );
    }

    #[test]
    fn replies_decode_per_command() {
        let refused = r#"{"code":"unauthenticated","message":"The username or password is wrong"}"#;
        let ApiReply::SignedIn(Err(error)) = ApiRequest::SignIn(LoginRequest {
            username: "ada".into(),
            password: "wrong".into(),
        })
        .decode(401, refused) else {
            panic!("a refused sign-in");
        };
        assert_eq!(error.message, "The username or password is wrong");
        assert_eq!(
            ApiRequest::SignOut.decode(204, ""),
            ApiReply::SignedOut(Ok(()))
        );
        let ApiReply::Bots(Ok(list)) = ApiRequest::ListBots.decode(200, r#"{"bots":[],"seq":4}"#)
        else {
            panic!("a Bot list");
        };
        assert!(list.bots.is_empty());
        let ApiReply::Session(Err(error)) = ApiRequest::CheckSession.unreachable() else {
            panic!("no response");
        };
        assert_eq!((error.status, error.code), (0, ErrorCode::Unavailable));
    }

    #[test]
    fn read_markers_and_push_name_their_routes_and_keep_the_endpoint_out_of_logs() {
        use bokkie_protocol::api::PushSubscriptionKeys;
        let subscription = ApiRequest::PutPushSubscription(PushSubscriptionRequest {
            endpoint: "https://push.example/device-secret".into(),
            keys: PushSubscriptionKeys {
                p256dh: "the-device-key".into(),
                auth: "the-device-secret".into(),
            },
        });
        let read = ApiRequest::MarkRead(
            BotId(3),
            MarkReadRequest {
                message_id: MessageId(41),
            },
        );
        let routes: Vec<(&str, String)> = [
            ApiRequest::LoadBot(BotId(3)),
            read.clone(),
            ApiRequest::PushKey,
            subscription.clone(),
            ApiRequest::DeletePushSubscription,
        ]
        .iter()
        .map(|request| (request.method().as_str(), request.path()))
        .collect();
        assert_eq!(
            routes,
            [
                ("GET", "/api/v1/bots/3".to_owned()),
                ("PUT", "/api/v1/bots/3/read".to_owned()),
                ("GET", "/api/v1/push/key".to_owned()),
                ("PUT", "/api/v1/push/subscription".to_owned()),
                ("DELETE", "/api/v1/push/subscription".to_owned()),
            ]
        );
        assert_eq!(read.body().as_deref(), Some(r#"{"message_id":41}"#));
        assert_eq!(
            subscription.body().as_deref(),
            Some(
                r#"{"endpoint":"https://push.example/device-secret","keys":{"p256dh":"the-device-key","auth":"the-device-secret"}}"#
            )
        );
        let logged = format!("{subscription:?}");
        for secret in ["device-secret", "the-device-key", "push.example"] {
            assert!(!logged.contains(secret), "{logged}");
        }
        assert_eq!(
            read.decode(200, r#"{"bot_id":3,"read_up_to":41,"unread":false}"#),
            ApiReply::MarkedRead(
                BotId(3),
                MessageId(41),
                Ok(ReadState {
                    bot_id: BotId(3),
                    read_up_to: Some(MessageId(41)),
                    unread: false,
                })
            )
        );
        assert_eq!(
            ApiRequest::PushKey.decode(200, r#"{"public_key":"BKey","sending":false}"#),
            ApiReply::PushKey(Ok(PushKey {
                public_key: "BKey".into(),
                sending: false,
            }))
        );
        assert_eq!(
            subscription.decode(200, r#"{"subscribed":true}"#),
            ApiReply::PushSubscribed(Ok(PushSubscriptionState { subscribed: true }))
        );
        assert_eq!(
            ApiRequest::DeletePushSubscription.decode(204, ""),
            ApiReply::PushUnsubscribed(Ok(()))
        );
        let ApiReply::MarkedRead(_, _, Err(error)) = read.unreachable() else {
            panic!("no response");
        };
        assert_eq!(error.code, ErrorCode::Unavailable);
    }
}
