//! The application: sign-in and sign-out, the signed-in user's Bot list, and
//! one Bot's conversation at a time, kept current by the event socket. An
//! admin also has the household's ChatGPT sign-in; a member sees only
//! whether it is signed in, what their permissions allow, and the notices
//! that an admin can read their conversations or reset their password.
//!
//! State changes only in `apply` and `perform`. Commands go out through an
//! [`Api`], and their replies come back as [`BokkieIntent::Reply`]; the event
//! socket's updates come back as [`BokkieIntent::Socket`]. Tests drive the
//! same transitions with a [`Recorder`](crate::api::Recorder) and a
//! [`Driver`](crate::socket::Driver).

use std::collections::{BTreeMap, BTreeSet};

use bokkie_protocol::api::{
    Author, Bot, BotStatus, CodexAccount, CreateBotRequest, Delivery, ErrorCode, LoginRequest,
    LoginState, Message, MessageKind, MessageState, Role, SendMessageRequest, SessionResponse,
    UpdateBotRequest, User,
};
use bokkie_protocol::events::Event;
use bokkie_protocol::{BotId, MessageId, Timestamp};
use serde::Serialize;
use stobie_ui_egui::local_time::{Clock, LocalTime, TimeLocale, TimeZone};
use stobie_ui_egui::web_push::PushEvent;
use stobie_ui_egui::{
    Action, ActionArgument, ActionKey, ActionTarget, Application, ApplicationIdentity,
    Availability, Frame, IntentSender, NavigationLayout, Observation, PassContext, UiPreferences,
    application_identity,
};

use crate::admin::Field;
use crate::api::{Api, ApiReply, ApiRequest, Generation};
use crate::socket::{Link, SocketUpdate};
use crate::store::{Snapshot, Store};
use crate::ui;

/// The application's name, as the page and the host report it.
pub const APPLICATION_NAME: &str = "Bokkie";

/// The username field's stable ID; `stobie set-text` addresses it.
pub const USERNAME_ID: &str = "bokkie.username";
/// The password field's stable ID.
pub const PASSWORD_ID: &str = "bokkie.password";
/// The New Bot name field's stable ID.
pub const BOT_NAME_ID: &str = "bokkie.bot-name";
/// The invitation code field's stable ID.
pub const INVITATION_CODE_ID: &str = "bokkie.invitation-code";
/// The new account's name field's stable ID.
pub const DISPLAY_NAME_ID: &str = "bokkie.display-name";
/// The profile editor's name field's stable ID.
pub const PROFILE_NAME_ID: &str = "bokkie.profile-name";
/// The profile editor's label field's stable ID.
pub const PROFILE_LABEL_ID: &str = "bokkie.profile-label";
/// The profile editor's description field's stable ID.
pub const PROFILE_DESCRIPTION_ID: &str = "bokkie.profile-description";

/// The largest ID an action accepts, such as a Bot's or a message's: the
/// largest integer a JSON number carries exactly.
const MAX_ID: f64 = 9_007_199_254_740_991.0;

/// How many earlier pages a message asked for, such as a routine run's
/// marker, may load before the client stops looking: 20 pages of 50, or
/// 1,000 messages back. Load older goes on from there.
pub(crate) const GOTO_PAGES: u32 = 20;

/// Every user action, declared once. Buttons, shortcuts and agents share
/// them; IDs are `bokkie.<kebab-case variant>`.
#[derive(Action, Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[action(prefix = "bokkie")]
pub enum BokkieAction {
    #[action(
        label = "Sign in",
        description = "Sign in with the username and password"
    )]
    SignIn,
    #[action(label = "Sign out", description = "Sign this browser out of Bokkie")]
    SignOut,
    #[action(
        label = "Refresh",
        description = "Load the open conversation, the Bot's settings or the admin screen shown again"
    )]
    Refresh,
    #[action(
        label = "Open",
        description = "Open the conversation of the Bot with this ID",
        argument = "number",
        minimum = 1,
        maximum = 9007199254740991,
        integer = true
    )]
    OpenBot,
    #[action(
        label = "Open",
        description = "Open the conversation of the pinned Bot with this ID, from the strip of pinned Bots above the list",
        argument = "number",
        minimum = 1,
        maximum = 9007199254740991,
        integer = true
    )]
    OpenPinned,
    #[action(
        label = "Refresh Bots",
        description = "Load the list of Bots and the household's ChatGPT sign-in again"
    )]
    RefreshBots,
    #[action(label = "Back", description = "Return to the previous level")]
    Back,
    #[action(
        label = "Load older",
        description = "Load the conversation's earlier messages, a page at a time"
    )]
    LoadOlder,
    #[action(
        label = "Show prompt",
        description = "Show the prompt a routine gave the Bot, below the marker of its run with this message ID, or fold it away again",
        argument = "number",
        minimum = 1,
        maximum = 9007199254740991,
        integer = true
    )]
    ShowPrompt,
    #[action(
        label = "Send",
        description = "Send the message to the Bot; while it replies, the message steers the reply",
        argument = "text"
    )]
    Send,
    #[action(
        label = "Create Bot",
        description = "Create a Bot with the name entered"
    )]
    CreateBot,
    #[action(
        label = "Show hidden Bots",
        description = "Show the Bots you have hidden in the list too, or hide them again"
    )]
    ShowHidden,
    #[action(
        label = "Pin Bot",
        description = "Pin the open Bot to the top of the list, or unpin it",
        id = "bokkie.pin"
    )]
    Pin,
    #[action(
        label = "Hide Bot",
        description = "Hide the open Bot from the list, or show it again",
        id = "bokkie.hide"
    )]
    Hide,
    #[action(
        label = "Edit profile",
        description = "Change the open Bot's name, label and description"
    )]
    EditProfile,
    #[action(label = "Save profile", description = "Save the Bot's profile")]
    SaveProfile,
    #[action(
        label = "Cancel editing",
        description = "Close the profile without saving it"
    )]
    CancelProfile,
    #[action(
        label = "Acknowledge",
        description = "Dismiss the notice that an admin reset your password",
        id = "bokkie.acknowledge-reset"
    )]
    AcknowledgeReset,
    #[action(
        label = "ChatGPT sign-in",
        description = "Open the household's ChatGPT sign-in, which every account's Bots use",
        id = "bokkie.household"
    )]
    Household,
    #[action(
        label = "Get a code",
        description = "Start signing the household in to ChatGPT with a device code, or get a new code",
        id = "bokkie.start-chatgpt"
    )]
    StartChatGpt,
    #[action(
        label = "Cancel sign-in",
        description = "Cancel the household's pending ChatGPT sign-in",
        id = "bokkie.cancel-chatgpt"
    )]
    CancelChatGpt,
    #[action(
        label = "Sign out of ChatGPT",
        description = "Ask to sign the household out of ChatGPT, which signs out every account's Bots",
        id = "bokkie.sign-out-chatgpt"
    )]
    SignOutChatGpt,
    #[action(
        label = "Sign out everyone",
        description = "Sign the household out of ChatGPT: no account's Bots can reply until an admin signs in again",
        id = "bokkie.confirm-sign-out-chatgpt"
    )]
    ConfirmSignOutChatGpt,
    #[action(
        label = "Keep signed in",
        description = "Leave the household signed in to ChatGPT",
        id = "bokkie.keep-chatgpt"
    )]
    KeepChatGpt,
    #[action(
        label = "I have an invitation",
        description = "Join Bokkie with an invitation code instead of signing in",
        id = "bokkie.join"
    )]
    Join,
    #[action(
        label = "Join Bokkie",
        description = "Create your account from the invitation code, and sign in",
        id = "bokkie.accept-invitation"
    )]
    AcceptInvitation,
    #[action(
        label = "Accounts",
        description = "Manage the household's accounts and invitations",
        id = "bokkie.admin"
    )]
    Admin,
    #[action(
        label = "Open account",
        description = "Open the account with this ID",
        argument = "number",
        minimum = 1,
        maximum = 9007199254740991,
        integer = true
    )]
    OpenAccount,
    #[action(
        label = "New account is an admin",
        description = "Invite an admin rather than a member",
        id = "bokkie.invite-admin"
    )]
    InviteAdmin,
    #[action(
        label = "New account may create Bots",
        description = "Let the invited member create their own Bots",
        id = "bokkie.invite-may-create-bots"
    )]
    InviteMayCreateBots,
    #[action(
        label = "New account may edit Bot profiles",
        description = "Let the invited member change their Bots' name, label, description and avatar",
        id = "bokkie.invite-may-edit-profiles"
    )]
    InviteMayEditProfiles,
    #[action(
        label = "Create invitation",
        description = "Create an invitation code, shown once"
    )]
    CreateInvitation,
    #[action(
        label = "Hide the code",
        description = "Take the invitation code off the screen",
        id = "bokkie.hide-invitation-code"
    )]
    HideInvitationCode,
    #[action(
        label = "Withdraw invitation",
        description = "Withdraw the invitation with this ID",
        argument = "number",
        minimum = 1,
        maximum = 9007199254740991,
        integer = true
    )]
    WithdrawInvitation,
    #[action(
        label = "May create Bots",
        description = "Let the member create their own Bots, or only an admin",
        id = "bokkie.may-create-bots"
    )]
    MayCreateBots,
    #[action(
        label = "May edit Bot profiles",
        description = "Let the member change their Bots' name, label, description and avatar",
        id = "bokkie.may-edit-profiles"
    )]
    MayEditProfiles,
    #[action(
        label = "Oversight",
        description = "Let admins read all of the member's Bots and conversations, read-only; the member is shown a notice the whole time",
        id = "bokkie.oversight"
    )]
    Oversight,
    #[action(
        label = "Account disabled",
        description = "Disable the account, which ends its sessions and stops its Bots, or enable it again",
        id = "bokkie.disabled"
    )]
    Disabled,
    #[action(
        label = "Save limits",
        description = "Save the account's turn and token limits"
    )]
    SaveLimits,
    #[action(
        label = "Reset password",
        description = "Set the account's password, which ends its sessions"
    )]
    ResetPassword,
    #[action(label = "Make Bot", description = "Make a Bot for the member")]
    MakeMemberBot,
    #[action(
        label = "Edit Bot",
        description = "Edit the profile of the admin-made Bot with this ID",
        id = "bokkie.edit-member-bot",
        argument = "number",
        minimum = 1,
        maximum = 9007199254740991,
        integer = true
    )]
    EditMemberBot,
    #[action(
        label = "Save Bot",
        description = "Save the Bot's profile",
        id = "bokkie.save-member-bot"
    )]
    SaveMemberBot,
    #[action(
        label = "Cancel Bot edit",
        description = "Close the Bot's profile without saving it",
        id = "bokkie.cancel-member-bot"
    )]
    CancelMemberBot,
    #[action(
        label = "Read conversation",
        description = "Read the conversation of the member's Bot with this ID, under oversight",
        argument = "number",
        minimum = 1,
        maximum = 9007199254740991,
        integer = true
    )]
    ReadConversation,
    #[action(
        label = "Load earlier",
        description = "Load the earlier messages of the conversation being read"
    )]
    LoadEarlier,
    #[action(label = "Stop", description = "Stop the Bot's reply in progress")]
    Stop,
    #[action(
        label = "Settings",
        description = "Show or hide the open Bot's panel: its routines, recent runs, grants, notes and activity"
    )]
    BotSettings,
    #[action(
        label = "Grant notes",
        description = "Let the Bot keep notes that it reads at the start of each turn, or take the grant away"
    )]
    GrantNotes,
    #[action(
        label = "Grant seen",
        description = "Let the Bot remember what it has already shown you, or take the grant away"
    )]
    GrantSeen,
    #[action(
        label = "Grant notify",
        description = "Let the Bot send you notifications, or take the grant away"
    )]
    GrantNotify,
    #[action(
        label = "Grant web.fetch",
        description = "Let the Bot read web pages, or take the grant away; only an admin can grant it"
    )]
    GrantWebFetch,
    #[action(
        label = "Grant web.search",
        description = "Let the Bot search the web, or take the grant away; only an admin can grant it"
    )]
    GrantWebSearch,
    #[action(
        label = "Clear notes",
        description = "Clear the Bot's notes, after confirming"
    )]
    ClearNotes,
    #[action(
        label = "Clear for good",
        description = "Confirm clearing the Bot's notes, which cannot be undone"
    )]
    ConfirmClearNotes,
    #[action(label = "Keep notes", description = "Keep the Bot's notes after all")]
    KeepNotes,
    #[action(
        label = "Earlier tool calls",
        description = "Load the Bot's earlier tool calls, a page at a time"
    )]
    OlderToolCalls,
    #[action(
        label = "New routine",
        description = "Start a routine: a prompt the Bot runs on a schedule"
    )]
    NewRoutine,
    #[action(
        label = "Edit routine",
        description = "Change the routine with this ID",
        id = "bokkie.edit-routine",
        argument = "number",
        minimum = 1,
        maximum = 9007199254740991,
        integer = true
    )]
    EditRoutine,
    #[action(label = "Every day", description = "Run the routine every day")]
    RepeatDaily,
    #[action(label = "Weekdays", description = "Run the routine Monday to Friday")]
    RepeatWeekdays,
    #[action(
        label = "On chosen days",
        description = "Run the routine on the days of the week chosen"
    )]
    RepeatDays,
    #[action(
        label = "Every few hours",
        description = "Run the routine every few hours through the day, at the time entered and the times a multiple of the hours from it"
    )]
    RepeatHours,
    #[action(label = "Monday", description = "Run the routine on Mondays, or not")]
    RoutineMon,
    #[action(label = "Tuesday", description = "Run the routine on Tuesdays, or not")]
    RoutineTue,
    #[action(
        label = "Wednesday",
        description = "Run the routine on Wednesdays, or not"
    )]
    RoutineWed,
    #[action(
        label = "Thursday",
        description = "Run the routine on Thursdays, or not"
    )]
    RoutineThu,
    #[action(label = "Friday", description = "Run the routine on Fridays, or not")]
    RoutineFri,
    #[action(
        label = "Saturday",
        description = "Run the routine on Saturdays, or not"
    )]
    RoutineSat,
    #[action(label = "Sunday", description = "Run the routine on Sundays, or not")]
    RoutineSun,
    #[action(
        label = "Notify every run",
        description = "Send a notification after every run, or only when the Bot asks to notify you"
    )]
    RoutinePush,
    #[action(label = "Save routine", description = "Save the routine")]
    SaveRoutine,
    #[action(
        label = "Close without saving",
        description = "Close the routine without saving it",
        id = "bokkie.cancel-routine"
    )]
    CancelRoutine,
    #[action(
        label = "Pause",
        description = "Pause the routine with this ID, so it runs only when run now",
        id = "bokkie.pause-routine",
        argument = "number",
        minimum = 1,
        maximum = 9007199254740991,
        integer = true
    )]
    PauseRoutine,
    #[action(
        label = "Resume",
        description = "Resume the paused routine with this ID, so it runs on its schedule again",
        id = "bokkie.resume-routine",
        argument = "number",
        minimum = 1,
        maximum = 9007199254740991,
        integer = true
    )]
    ResumeRoutine,
    #[action(
        label = "Run now",
        description = "Run the routine with this ID now, even while it is paused",
        id = "bokkie.run-routine",
        argument = "number",
        minimum = 1,
        maximum = 9007199254740991,
        integer = true
    )]
    RunRoutine,
    #[action(
        label = "Delete",
        description = "Delete the routine with this ID and its history, after confirming",
        id = "bokkie.delete-routine",
        argument = "number",
        minimum = 1,
        maximum = 9007199254740991,
        integer = true
    )]
    DeleteRoutine,
    #[action(
        label = "Delete for good",
        description = "Confirm deleting the routine, which cannot be undone",
        id = "bokkie.confirm-delete-routine"
    )]
    ConfirmDeleteRoutine,
    #[action(
        label = "Keep routine",
        description = "Keep the routine after all",
        id = "bokkie.keep-routine"
    )]
    KeepRoutine,
    #[action(
        label = "History",
        description = "Show the run history of the routine with this ID, or hide it",
        id = "bokkie.routine-history",
        argument = "number",
        minimum = 1,
        maximum = 9007199254740991,
        integer = true
    )]
    RoutineHistory,
    #[action(
        label = "Earlier runs",
        description = "Load the routine's earlier runs, a page at a time"
    )]
    OlderRuns,
    #[action(
        label = "Show",
        description = "Show the run with this ID in the conversation, from its marker",
        id = "bokkie.show-run",
        argument = "number",
        minimum = 1,
        maximum = 9007199254740991,
        integer = true
    )]
    ShowRun,
    // The recent runs' Show is an action of its own, since a run can show
    // in its routine's history at the same time, and one ID must name one
    // control.
    #[action(
        label = "Show",
        description = "Show the recent run with this ID in the conversation, from its marker",
        id = "bokkie.show-recent-run",
        argument = "number",
        minimum = 1,
        maximum = 9007199254740991,
        integer = true
    )]
    ShowRecentRun,
    #[action(
        label = "Notifications",
        description = "Notify this device when a Bot has something for you, or stop notifying it"
    )]
    Notifications,
    #[action(
        label = "Save time zone",
        description = "Save the time zone entered, which routines run in and times show in"
    )]
    SaveTimeZone,
    #[action(
        label = "Reload page",
        description = "Reload the page to fetch the client that matches Bokkie's server"
    )]
    ReloadPage,
}

/// Data changes from the UI, and replies and updates from the transport.
pub enum BokkieIntent {
    EditUsername(String),
    EditPassword(String),
    EditDraft(String),
    EditBotName(String),
    EditProfileName(String),
    EditProfileLabel(String),
    EditProfileDescription(String),
    EditInvitationCode(String),
    EditDisplayName(String),
    EditInviteName(String),
    EditDailyTurns(String),
    EditWeeklyTokens(String),
    EditNewPassword(String),
    EditMemberBotName(String),
    EditRoutineName(String),
    EditRoutinePrompt(String),
    EditRoutineAt(String),
    EditRoutineEvery(String),
    EditTimeZone(String),
    /// A command's reply, with the generation it was sent in.
    Reply {
        generation: Generation,
        reply: ApiReply,
    },
    /// An update from the event socket opened for `generation`.
    Socket {
        generation: Generation,
        update: SocketUpdate,
    },
    /// The page is visible again, after the viewer may have changed their
    /// time zone or locale.
    Foreground,
    /// The page is hidden: nothing on it is being read.
    Background,
    /// A result from Web Push.
    Push(PushEvent),
}

// The password and a push subscription must never reach a log line.
impl std::fmt::Debug for BokkieIntent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EditUsername(username) => f.debug_tuple("EditUsername").field(username).finish(),
            Self::EditPassword(_) => f.write_str("EditPassword(<redacted>)"),
            Self::EditDraft(draft) => f.debug_tuple("EditDraft").field(draft).finish(),
            Self::EditBotName(name) => f.debug_tuple("EditBotName").field(name).finish(),
            Self::EditProfileName(name) => f.debug_tuple("EditProfileName").field(name).finish(),
            Self::EditProfileLabel(label) => {
                f.debug_tuple("EditProfileLabel").field(label).finish()
            }
            Self::EditProfileDescription(description) => f
                .debug_tuple("EditProfileDescription")
                .field(description)
                .finish(),
            Self::EditInvitationCode(_) => f.write_str("EditInvitationCode(<redacted>)"),
            Self::EditDisplayName(name) => f.debug_tuple("EditDisplayName").field(name).finish(),
            Self::EditInviteName(name) => f.debug_tuple("EditInviteName").field(name).finish(),
            Self::EditDailyTurns(text) => f.debug_tuple("EditDailyTurns").field(text).finish(),
            Self::EditWeeklyTokens(text) => f.debug_tuple("EditWeeklyTokens").field(text).finish(),
            Self::EditNewPassword(_) => f.write_str("EditNewPassword(<redacted>)"),
            Self::EditMemberBotName(name) => {
                f.debug_tuple("EditMemberBotName").field(name).finish()
            }
            Self::EditRoutineName(text) => f.debug_tuple("EditRoutineName").field(text).finish(),
            Self::EditRoutinePrompt(text) => {
                f.debug_tuple("EditRoutinePrompt").field(text).finish()
            }
            Self::EditRoutineAt(text) => f.debug_tuple("EditRoutineAt").field(text).finish(),
            Self::EditRoutineEvery(text) => f.debug_tuple("EditRoutineEvery").field(text).finish(),
            Self::EditTimeZone(text) => f.debug_tuple("EditTimeZone").field(text).finish(),
            Self::Reply { generation, reply } => f
                .debug_struct("Reply")
                .field("generation", generation)
                .field("reply", reply)
                .finish(),
            Self::Socket { generation, update } => f
                .debug_struct("Socket")
                .field("generation", generation)
                .field("update", update)
                .finish(),
            Self::Foreground => f.write_str("Foreground"),
            Self::Background => f.write_str("Background"),
            // Never the endpoint or the keys.
            Self::Push(event) => write!(f, "Push({})", crate::notifications::describe(event)),
        }
    }
}

/// Makes the transport once the host hands over its intent sender.
pub type ApiFactory = Box<dyn FnOnce(IntentSender<BokkieIntent>) -> Box<dyn Api>>;

/// Where the user is.
#[derive(Debug)]
pub(crate) enum Session {
    /// Asking the gateway whether the browser's cookie is still a session.
    Checking,
    SignedOut(SignInForm),
    SignedIn(Box<SignedIn>),
}

#[derive(Debug, Default)]
pub(crate) struct SignInForm {
    /// Joining with an invitation rather than signing in.
    pub(crate) joining: bool,
    /// The invitation code, while joining.
    pub(crate) code: String,
    /// The new account's name, while joining.
    pub(crate) display_name: String,
    pub(crate) username: String,
    pub(crate) password: String,
    /// A sign-in is under way.
    pub(crate) busy: bool,
    /// Why the last attempt failed, or why the user is signed out.
    pub(crate) message: Option<String>,
}

#[derive(Debug)]
pub(crate) struct SignedIn {
    /// The account as the gateway last said, by sign-in, a session check or
    /// `account_updated`.
    pub(crate) user: User,
    /// When an admin reset this account's password, until the user
    /// acknowledges it.
    pub(crate) reset_notice: Option<Timestamp>,
    /// The acknowledgement is under way.
    pub(crate) acknowledging: bool,
    /// The reset this session acknowledged, which a session check that
    /// predates the acknowledgement must not bring back.
    acknowledged: Option<Timestamp>,
    /// Why the Bot list last failed to load, until it loads.
    pub(crate) bots_error: Option<String>,
    /// A sign-out is under way.
    pub(crate) signing_out: bool,
    /// The last command error, such as a failed sign-out.
    pub(crate) message: Option<String>,
    /// The event socket's state.
    pub(crate) link: Link,
    /// The conversation shown, if any; otherwise the Bot list is.
    pub(crate) open: Option<Open>,
    /// The unsent drafts of the Bots not open now, kept until their
    /// conversation opens again or the user signs out.
    pub(crate) drafts: BTreeMap<BotId, String>,
    pub(crate) new_bot: NewBot,
    /// Whether the list shows hidden Bots too.
    pub(crate) show_hidden: bool,
    /// Whether the household is signed in to ChatGPT.
    pub(crate) account: Account,
    /// The household's ChatGPT sign-in screen, when an admin has it open.
    pub(crate) household: Option<Household>,
    /// The admin section, when an admin has it open.
    pub(crate) admin: Option<crate::admin::Admin>,
    /// The user's own time zone, which times are shown in, or `None` when
    /// it is not one the platform knows.
    pub(crate) zone: Option<TimeZone>,
    /// The time zone being entered, until it is saved.
    pub(crate) zone_draft: Option<String>,
    /// A time zone change is under way.
    pub(crate) zone_saving: bool,
    /// Why the last time zone change failed.
    pub(crate) zone_error: Option<String>,
    /// This sign-in's push subscription and read markers.
    pub(crate) push: crate::notifications::Signed,
}

impl SignedIn {
    /// Close the open conversation, keeping its unsent draft for when it
    /// opens again. A draft being sent is not kept: it is on its way.
    pub(crate) fn take_open(&mut self) -> Option<Open> {
        let open = self.open.take()?;
        if !open.sending && !open.draft.is_empty() {
            self.drafts.insert(open.bot, open.draft.clone());
        }
        Some(open)
    }

    pub(crate) fn is_admin(&self) -> bool {
        self.user.role == Role::Admin
    }

    /// Whether the user may create Bots. Admins always may.
    pub(crate) fn may_create_bots(&self) -> bool {
        self.is_admin() || self.user.permissions.may_create_bots
    }

    /// Whether the user may change their Bots' profiles. Admins always may.
    pub(crate) fn may_edit_profiles(&self) -> bool {
        self.is_admin() || self.user.permissions.may_edit_bot_profiles
    }

    /// Since when an admin can read this member's Bots and conversations,
    /// if oversight is on. It never applies to an admin.
    pub(crate) fn overseen(&self) -> Option<Option<Timestamp>> {
        (self.user.role == Role::Member && self.user.oversight)
            .then_some(self.user.oversight_changed_at)
    }

    /// Take the account as it now is, and drop what it no longer allows.
    pub(crate) fn account_changed(&mut self, user: User) {
        if user.time_zone != self.user.time_zone {
            self.zone = TimeZone::named(&user.time_zone).ok();
        }
        self.user = user;
        if !self.may_edit_profiles()
            && let Some(open) = &mut self.open
        {
            open.editor = None;
        }
        if !self.is_admin() {
            self.household = None;
            self.admin = None;
        }
    }
}

/// The New Bot form: a name only.
#[derive(Debug, Default)]
pub(crate) struct NewBot {
    pub(crate) name: String,
    /// A create is under way.
    pub(crate) busy: bool,
    /// Why the last create failed.
    pub(crate) error: Option<String>,
}

/// The household's ChatGPT sign-in, as `GET /codex/account` last said. A
/// member is told only whether it is signed in; an admin also as whom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Account {
    Checking,
    SignedIn {
        email: Option<String>,
        plan: Option<String>,
    },
    SignedOut,
    Failed(String),
}

/// The household's ChatGPT sign-in screen, for admins.
#[derive(Debug)]
pub(crate) struct Household {
    pub(crate) login: Login,
    pub(crate) sign_out: SignOut,
}

/// A device-code sign-in of the household, as this screen knows it.
#[derive(Debug)]
pub(crate) enum Login {
    /// None started from this screen.
    Idle,
    /// `POST /codex/login` is under way.
    Starting,
    /// The code is on screen and its state is polled until it ends. The
    /// code lives here only, and goes when the screen does.
    Waiting {
        code: crate::api::DeviceCode,
        next_poll: Timestamp,
        polling: bool,
        /// Why the last poll failed, while polling goes on.
        error: Option<String>,
    },
    /// `DELETE /codex/login` is under way.
    Cancelling { code: crate::api::DeviceCode },
    /// The sign-in ended: completed, expired, failed or cancelled.
    Ended {
        state: LoginState,
        error: Option<String>,
    },
    /// Starting or following the sign-in failed.
    Failed(String),
}

/// Signing the household out, which signs out every account's Bots.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SignOut {
    Idle,
    /// The warning is on screen, awaiting confirmation.
    Confirming,
    /// `POST /codex/logout` is under way.
    Busy,
    Failed(String),
}

/// How often a pending sign-in's state is checked while its code is on
/// screen.
pub const LOGIN_POLL: std::time::Duration = std::time::Duration::from_secs(5);

/// The open Bot's profile, being edited.
#[derive(Debug, Default)]
pub(crate) struct ProfileEditor {
    pub(crate) name: String,
    pub(crate) label: String,
    pub(crate) description: String,
    /// A save is under way.
    pub(crate) busy: bool,
    /// Why the last save failed.
    pub(crate) error: Option<String>,
}

/// One Bot's conversation, open on screen.
#[derive(Debug)]
pub(crate) struct Open {
    pub(crate) bot: BotId,
    /// Why the conversation last failed to load, until it loads.
    pub(crate) error: Option<String>,
    pub(crate) draft: String,
    /// A send is under way.
    pub(crate) sending: bool,
    /// How the last message sent reached the Bot.
    pub(crate) delivery: Option<Delivery>,
    /// Why the last send failed.
    pub(crate) send_error: Option<String>,
    /// A stop is under way.
    pub(crate) stopping: bool,
    /// What the last stop found, when it is worth saying.
    pub(crate) stop_note: Option<String>,
    /// Why the last page of older messages failed to load.
    pub(crate) older_error: Option<String>,
    /// The message that was oldest before the last page of older messages
    /// arrived, which the view keeps in place, with the number of that page
    /// among all pages loaded, so the view moves once for each.
    pub(crate) anchor: Option<(u64, MessageId)>,
    /// The profile editor, when open.
    pub(crate) editor: Option<ProfileEditor>,
    /// A pin, hide or grant change is under way.
    pub(crate) updating: bool,
    /// The grant to turn on (`true`) or off once the Bot's current grants
    /// arrive.
    pub(crate) grant: Option<(bokkie_protocol::api::Grant, bool)>,
    /// Why the last pin, hide or grant change failed.
    pub(crate) update_error: Option<String>,
    /// The routine runs whose prompt is shown, by their marker's ID; every
    /// other run's prompt is folded away.
    pub(crate) prompts: BTreeSet<MessageId>,
    /// The Bot's settings, when open in place of the conversation.
    pub(crate) settings: Option<crate::settings::Settings>,
    /// A message to bring into view, such as a routine run's marker, with
    /// the number of that request among all made, so the view moves once
    /// for each. Earlier pages load until the conversation holds it.
    pub(crate) goto: Option<(u64, MessageId)>,
    /// Why the last message asked for could not be shown.
    pub(crate) goto_note: Option<String>,
    /// The earlier pages loaded to find the message asked for.
    pub(crate) goto_pages: u32,
}

impl Open {
    fn new(bot: BotId) -> Self {
        Self {
            bot,
            error: None,
            draft: String::new(),
            sending: false,
            delivery: None,
            send_error: None,
            stopping: false,
            stop_note: None,
            older_error: None,
            anchor: None,
            editor: None,
            updating: false,
            grant: None,
            update_error: None,
            prompts: BTreeSet::new(),
            settings: None,
            goto: None,
            goto_note: None,
            goto_pages: 0,
        }
    }
}

/// The application state. The UI only reads it.
pub struct App {
    pub(crate) session: Session,
    /// Moves on at every sign-in and sign-out; replies and socket updates
    /// from before are dropped.
    generation: Generation,
    pub(crate) store: Store,
    /// Snapshot loads sent and not yet answered. At most one of each is in
    /// flight.
    in_flight: BTreeSet<Snapshot>,
    /// Snapshots to load again as soon as their load in flight answers,
    /// because something asked for them meanwhile.
    again: BTreeSet<Snapshot>,
    /// A usage meter request is in flight.
    usage_in_flight: bool,
    /// Load the meter again once the request in flight answers.
    usage_again: bool,
    /// An `account_updated` event arrived since the session was last
    /// checked over HTTP, so the account that check returns may be older.
    account_event_seen: bool,
    /// Pages of older messages added since the application started.
    older_pages: u64,
    /// Messages asked to be brought into view since the application
    /// started.
    gotos: u64,
    make_api: Option<ApiFactory>,
    pub(crate) api: Option<Box<dyn Api>>,
    /// The device's Web Push and badge, and whether the page is hidden.
    pub(crate) device: crate::notifications::Device,
    /// The viewer's zone and locale, as the transport last read them.
    viewer: Option<LocalTime>,
    /// The navigation's layout at the width of the last pass.
    pub(crate) layout: NavigationLayout,
    /// The appearance asked of Stobie (see `appearance`).
    pub(crate) preferences: UiPreferences,
    /// The theme the last pass drew in, light or dark as the system is.
    pub(crate) theme: egui::Theme,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("screen", &self.screen())
            .finish_non_exhaustive()
    }
}

impl App {
    /// An application that will reach the gateway through the transport
    /// `make_api` builds when the host starts.
    pub fn new(make_api: ApiFactory) -> Self {
        Self {
            session: Session::Checking,
            generation: Generation::default(),
            store: Store::new(),
            in_flight: BTreeSet::new(),
            again: BTreeSet::new(),
            usage_in_flight: false,
            usage_again: false,
            account_event_seen: false,
            older_pages: 0,
            gotos: 0,
            make_api: Some(make_api),
            api: None,
            device: crate::notifications::Device::default(),
            viewer: None,
            layout: NavigationLayout::Stack,
            preferences: crate::appearance::preferences(),
            theme: egui::Theme::Dark,
        }
    }

    /// The screen shown, as the `screen` fact names it.
    pub fn screen(&self) -> &'static str {
        match &self.session {
            Session::Checking => "checking",
            Session::SignedOut(form) if form.busy => "signing_in",
            Session::SignedOut(_) => "signed_out",
            Session::SignedIn(signed_in) if signed_in.signing_out => "signing_out",
            Session::SignedIn(signed_in) if signed_in.household.is_some() => "household",
            Session::SignedIn(_) if self.admin_screen().is_some() => {
                self.admin_screen().unwrap_or_default()
            }
            Session::SignedIn(signed_in)
                if signed_in
                    .open
                    .as_ref()
                    .is_some_and(|o| o.settings.is_some()) =>
            {
                "bot_settings"
            }
            Session::SignedIn(signed_in) if signed_in.open.is_some() => "conversation",
            Session::SignedIn(_) => "signed_in",
        }
    }

    /// The signed-in user, if any.
    pub fn user(&self) -> Option<&User> {
        match &self.session {
            Session::SignedIn(signed_in) => Some(&signed_in.user),
            _ => None,
        }
    }

    /// The message the current screen shows, if any.
    pub fn message(&self) -> Option<&str> {
        match &self.session {
            Session::Checking => None,
            Session::SignedOut(form) => form.message.as_deref(),
            Session::SignedIn(signed_in) => signed_in
                .message
                .as_deref()
                .or(signed_in.bots_error.as_deref()),
        }
    }

    /// The Bot list's state, as the `bots` fact names it.
    pub fn bots_state(&self) -> &'static str {
        match &self.session {
            Session::SignedIn(_) if self.in_flight.contains(&Snapshot::Bots) => "loading",
            Session::SignedIn(signed_in) if signed_in.bots_error.is_some() => "failed",
            Session::SignedIn(_) if self.store.bots_loaded() => "loaded",
            _ => "",
        }
    }

    /// The open conversation's state, as the `conversation` fact names it.
    pub fn conversation_state(&self) -> &'static str {
        let Some(open) = self.open() else {
            return "";
        };
        let snapshot = Snapshot::Messages(open.bot);
        if self.store.conversation(open.bot).is_some() {
            "loaded"
        } else if self.in_flight.contains(&snapshot) {
            "loading"
        } else if open.error.is_some() {
            "failed"
        } else {
            "loading"
        }
    }

    /// The Bot whose conversation is open, if any.
    pub fn open_bot(&self) -> Option<BotId> {
        self.open().map(|open| open.bot)
    }

    /// The event socket's state.
    pub fn link(&self) -> Link {
        match &self.session {
            Session::SignedIn(signed_in) => signed_in.link,
            _ => Link::Stopped,
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The generation the next command is sent in.
    pub fn generation(&self) -> Generation {
        self.generation
    }

    pub(crate) fn household(&self) -> Option<&Household> {
        match &self.session {
            Session::SignedIn(signed_in) => signed_in.household.as_ref(),
            _ => None,
        }
    }

    pub(crate) fn open(&self) -> Option<&Open> {
        match &self.session {
            Session::SignedIn(signed_in) => signed_in.open.as_ref(),
            _ => None,
        }
    }

    fn open_mut(&mut self) -> Option<&mut Open> {
        match &mut self.session {
            Session::SignedIn(signed_in) => signed_in.open.as_mut(),
            _ => None,
        }
    }

    /// The open conversation's latest Bot reply, if it holds one. The
    /// gateway's markers are not replies.
    pub(crate) fn latest_reply(&self) -> Option<&Message> {
        let bot = self.open_bot()?;
        self.store
            .conversation(bot)?
            .messages()
            .rev()
            .find(|message| message.author == Author::Bot && message.kind == MessageKind::Text)
    }

    pub(crate) fn send(&self, request: ApiRequest) {
        if let Some(api) = &self.api {
            api.send(self.generation, request);
        }
    }

    fn next_generation(&mut self) {
        self.generation = Generation(self.generation.0 + 1);
        self.store = Store::new();
        self.in_flight.clear();
        self.again.clear();
        self.usage_in_flight = false;
        self.usage_again = false;
        self.account_event_seen = false;
    }

    fn signed_out(&mut self, message: Option<String>) {
        self.leave(message, true);
    }

    /// Show the sign-in form. `ended` says the gateway has no session for
    /// this browser, so whatever push subscription the device holds is no
    /// session's.
    fn leave(&mut self, message: Option<String>, ended: bool) {
        if ended {
            self.forget_subscription();
        }
        let username = match &self.session {
            Session::SignedOut(form) => form.username.clone(),
            Session::SignedIn(signed_in) => signed_in.user.username.clone(),
            Session::Checking => String::new(),
        };
        if matches!(self.session, Session::SignedIn(_))
            && let Some(api) = &self.api
        {
            api.disconnect_events();
        }
        self.next_generation();
        self.session = Session::SignedOut(SignInForm {
            username,
            message,
            ..SignInForm::default()
        });
    }

    fn signed_in(&mut self, session: SessionResponse) {
        // Only a launch with a live session keeps the device's
        // subscription; a new sign-in switches notifications on itself.
        if !matches!(self.session, Session::Checking) {
            self.forget_subscription();
        }
        self.next_generation();
        self.session = Session::SignedIn(Box::new(SignedIn {
            zone: TimeZone::named(&session.user.time_zone).ok(),
            user: session.user,
            reset_notice: session.password_reset_by_admin,
            acknowledging: false,
            acknowledged: None,
            bots_error: None,
            signing_out: false,
            message: None,
            link: Link::Stopped,
            open: None,
            drafts: BTreeMap::new(),
            new_bot: NewBot::default(),
            show_hidden: false,
            account: Account::Checking,
            household: None,
            admin: None,
            zone_draft: None,
            zone_saving: false,
            zone_error: None,
            push: crate::notifications::Signed::default(),
        }));
        // The list shows at once. Once the socket is live it is loaded again,
        // so that no event can fall between the list and the socket.
        self.load(Snapshot::Bots);
        self.load_usage();
        self.send(ApiRequest::CodexAccount);
        if let Some(api) = &self.api {
            api.connect_events(self.generation);
        }
    }

    /// Load the usage meter. As with snapshots, one request is in flight at
    /// a time, and one asked for meanwhile follows it.
    fn load_usage(&mut self) {
        if self.usage_in_flight {
            self.usage_again = true;
            return;
        }
        self.usage_in_flight = true;
        self.store.request_usage();
        self.send(ApiRequest::LoadUsage);
    }

    /// Load a snapshot over HTTP. The store holds any event that arrives
    /// meanwhile. A snapshot already loading is loaded again once that load
    /// answers, because it may predate whatever asked for it now.
    ///
    /// Older messages load one page at a time, from where the conversation
    /// starts when the page is requested.
    pub(crate) fn load(&mut self, snapshot: Snapshot) {
        let request = match snapshot {
            Snapshot::Bots => ApiRequest::ListBots,
            Snapshot::Messages(bot) => ApiRequest::LoadMessages(bot),
            Snapshot::Routines(bot) => ApiRequest::ListRoutines(bot),
            Snapshot::Older(_) if self.in_flight.contains(&snapshot) => return,
            Snapshot::Older(bot) => match self.store.request_older(bot) {
                Some(before) => ApiRequest::LoadOlder(bot, before),
                None => return,
            },
        };
        let _ = self.store.request(snapshot);
        if !self.in_flight.insert(snapshot) {
            self.again.insert(snapshot);
            return;
        }
        self.send(request);
    }

    pub(crate) fn load_all(&mut self, snapshots: impl IntoIterator<Item = Snapshot>) {
        for snapshot in snapshots {
            self.load(snapshot);
        }
    }

    /// Reload everything shown, as after `reset` or the first time live.
    fn reload_everything(&mut self) {
        let mut all = self.store.reload_all();
        // A conversation still on its first load is not in the store yet.
        if let Some(bot) = self.open_bot() {
            all.insert(Snapshot::Messages(bot));
        }
        self.load_all(all);
        // The settings' notes, tool calls and run history announce no
        // events of their own that the store keeps.
        if let Some(bot) = self.open_bot() {
            self.load_settings_extras(bot);
        }
        // The meter and the account may have missed events too, and the
        // household's sign-in announces no events at all.
        self.load_usage();
        self.check_session();
        self.send(ApiRequest::CodexAccount);
        self.retry_push();
    }

    /// Ask for the session's account again. An `account_updated` event that
    /// arrives before the answer is at least as new, so it wins.
    fn check_session(&mut self) {
        self.account_event_seen = false;
        self.send(ApiRequest::CheckSession);
    }

    /// Whether a load of `snapshot` is in flight.
    pub(crate) fn loading(&self, snapshot: Snapshot) -> bool {
        self.in_flight.contains(&snapshot)
    }

    /// A load answered. Returns whether to load it again at once.
    pub(crate) fn answered(&mut self, snapshot: Snapshot) -> bool {
        self.in_flight.remove(&snapshot);
        self.again.remove(&snapshot)
    }

    /// A signed-in command answered 401: the session is gone.
    pub(crate) fn session_ended(&mut self) {
        self.signed_out(Some("Your session has ended. Sign in again".into()));
    }

    fn reply(&mut self, reply: ApiReply) -> bool {
        let reply = match self.admin_reply(reply) {
            Ok(changed) => return changed,
            Err(reply) => *reply,
        };
        let reply = match self.settings_reply(reply) {
            Ok(changed) => return changed,
            Err(reply) => *reply,
        };
        let reply = match self.push_reply(reply) {
            Ok(changed) => return changed,
            Err(reply) => *reply,
        };
        match (reply, &mut self.session) {
            (ApiReply::Joined(result), Session::SignedOut(form)) if form.busy && form.joining => {
                match result {
                    Ok(session) => self.signed_in(session),
                    // The code and names stay, to correct.
                    Err(error) => {
                        form.busy = false;
                        form.message = Some(error.message);
                    }
                }
            }
            (ApiReply::Session(Ok(session)), Session::Checking) => self.signed_in(session),
            // A session check while signed in: the account as it now is,
            // unless an event has said something newer meanwhile, and
            // whether an admin's reset is still to be acknowledged.
            (ApiReply::Session(Ok(session)), Session::SignedIn(signed_in)) => {
                if session.user.id != signed_in.user.id {
                    // Another account signed in on this browser.
                    self.session_ended();
                    return true;
                }
                if !self.account_event_seen {
                    signed_in.account_changed(session.user);
                }
                match session.password_reset_by_admin {
                    _ if signed_in.acknowledging => {}
                    // Acknowledged here after this check was read.
                    Some(at) if Some(at) == signed_in.acknowledged => {}
                    // None too: acknowledged here or on another device.
                    reset => signed_in.reset_notice = reset,
                }
            }
            (ApiReply::Session(Err(error)), Session::Checking) => {
                // 401 is the ordinary signed-out answer; anything else is
                // worth saying.
                let ended = error.is_unauthenticated();
                let message = (!ended).then_some(error.message);
                self.leave(message, ended);
            }
            // The socket asked whether the session still holds.
            (ApiReply::Session(Err(error)), Session::SignedIn(_)) if error.is_unauthenticated() => {
                self.session_ended()
            }
            (ApiReply::SignedIn(result), Session::SignedOut(form)) if form.busy => match result {
                Ok(session) => self.signed_in(session),
                Err(error) => {
                    form.busy = false;
                    form.message = Some(error.message);
                }
            },
            (ApiReply::SignedOut(result), Session::SignedIn(signed_in))
                if signed_in.signing_out =>
            {
                match result {
                    // Already signed out counts as success.
                    Ok(()) => self.signed_out(None),
                    Err(error) if error.is_unauthenticated() => self.signed_out(None),
                    Err(error) => {
                        signed_in.signing_out = false;
                        signed_in.message = Some(error.message);
                    }
                }
            }
            (ApiReply::Bots(result), Session::SignedIn(_))
                if self.in_flight.contains(&Snapshot::Bots) =>
            {
                let again = self.answered(Snapshot::Bots);
                match result {
                    Ok(list) => {
                        if let Session::SignedIn(signed_in) = &mut self.session {
                            signed_in.bots_error = None;
                        }
                        let applied = self.store.load_bots(list);
                        self.load_all(applied.reload);
                    }
                    Err(error) if error.is_unauthenticated() => {
                        self.session_ended();
                        return true;
                    }
                    Err(_) if again => {}
                    Err(error) => {
                        if let Session::SignedIn(signed_in) = &mut self.session {
                            signed_in.bots_error = Some(error.message);
                        }
                        self.store.load_failed(Snapshot::Bots);
                    }
                }
                if again {
                    self.load(Snapshot::Bots);
                }
            }
            (ApiReply::Messages(bot, result), Session::SignedIn(_))
                if self.in_flight.contains(&Snapshot::Messages(bot)) =>
            {
                let snapshot = Snapshot::Messages(bot);
                let again = self.answered(snapshot);
                if self.open_bot() != Some(bot) {
                    // Closed meanwhile; the store has already let it go.
                    return false;
                }
                match result {
                    Ok(page) => {
                        if let Some(open) = self.open_mut() {
                            open.error = None;
                            // The older pages it failed among are gone too.
                            open.older_error = None;
                        }
                        let applied = self.store.load_messages(bot, page);
                        self.load_all(applied.reload);
                        self.seek_goto();
                    }
                    Err(error) if error.is_unauthenticated() => {
                        self.session_ended();
                        return true;
                    }
                    Err(_) if again => {}
                    Err(error) => {
                        if let Some(open) = self.open_mut() {
                            open.error = Some(error.message);
                        }
                        self.store.load_failed(snapshot);
                    }
                }
                if again {
                    self.load(snapshot);
                }
            }
            (ApiReply::Older(bot, before, result), Session::SignedIn(_))
                if self.in_flight.contains(&Snapshot::Older(bot)) =>
            {
                let snapshot = Snapshot::Older(bot);
                self.in_flight.remove(&snapshot);
                if self.open_bot() != Some(bot) {
                    // Closed meanwhile; the store has already let it go.
                    return false;
                }
                match result {
                    Ok(page) => {
                        let applied = self.store.load_older(bot, before, page);
                        if applied.changed {
                            self.older_pages += 1;
                        }
                        let pages = self.older_pages;
                        if let Some(open) = self.open_mut() {
                            open.older_error = None;
                            if applied.changed {
                                open.anchor = Some((pages, before));
                            }
                        }
                        self.load_all(applied.reload);
                        self.seek_goto();
                    }
                    Err(error) if error.is_unauthenticated() => {
                        self.session_ended();
                        return true;
                    }
                    Err(error) => {
                        if let Some(open) = self.open_mut() {
                            open.older_error = Some(error.message);
                        }
                        self.store.load_failed(snapshot);
                    }
                }
            }
            (ApiReply::Sent(bot, result), Session::SignedIn(_)) => {
                let Some(open) = self
                    .open_mut()
                    .filter(|open| open.bot == bot && open.sending)
                else {
                    return false;
                };
                open.sending = false;
                match result {
                    Ok(sent) => {
                        open.draft.clear();
                        open.delivery = Some(sent.delivery);
                        // A delivered message moves on from the Bot's last
                        // error.
                        let _ = self.store.clear_error(bot);
                    }
                    Err(error) if error.is_unauthenticated() => self.session_ended(),
                    // The draft stays, to send again. Refused for want of
                    // ChatGPT, the household's status may be news too.
                    Err(error) => {
                        let unavailable = error.code == ErrorCode::Unavailable;
                        open.send_error = Some(error.message);
                        if unavailable {
                            self.send(ApiRequest::CodexAccount);
                        }
                    }
                }
            }
            (ApiReply::Stopped(bot, result), Session::SignedIn(_)) => {
                let Some(open) = self
                    .open_mut()
                    .filter(|open| open.bot == bot && open.stopping)
                else {
                    return false;
                };
                open.stopping = false;
                match result {
                    // The interrupted reply arrives on the socket.
                    Ok(stop) if stop.stopped => {}
                    Ok(_) => open.stop_note = Some("The reply had already finished".into()),
                    Err(error) if error.is_unauthenticated() => self.session_ended(),
                    Err(error) => open.stop_note = Some(error.message),
                }
            }
            (ApiReply::Created(result), Session::SignedIn(signed_in)) if signed_in.new_bot.busy => {
                let form = &mut signed_in.new_bot;
                form.busy = false;
                match result {
                    // The Bot itself arrives as `bot_created`.
                    Ok(_) => form.name.clear(),
                    Err(error) if error.is_unauthenticated() => self.session_ended(),
                    // The name stays, to correct.
                    Err(error) => form.error = Some(error.message),
                }
            }
            (ApiReply::BotUpdated(bot, result), Session::SignedIn(signed_in)) => {
                let Some(open) = signed_in.open.as_mut().filter(|open| open.bot == bot) else {
                    return false;
                };
                // The Bot itself arrives as `bot_updated`.
                if let Some(editor) = open.editor.as_mut().filter(|editor| editor.busy) {
                    editor.busy = false;
                    match result {
                        Ok(_) => open.editor = None,
                        Err(error) if error.is_unauthenticated() => self.session_ended(),
                        // The edits stay, to correct.
                        Err(error) => editor.error = Some(error.message),
                    }
                } else if open.updating {
                    open.updating = false;
                    match result {
                        Ok(_) => {}
                        Err(error) if error.is_unauthenticated() => self.session_ended(),
                        Err(error) => open.update_error = Some(error.message),
                    }
                } else {
                    return false;
                }
            }
            (ApiReply::Acknowledged(result), Session::SignedIn(signed_in))
                if signed_in.acknowledging =>
            {
                signed_in.acknowledging = false;
                match result {
                    Ok(()) => signed_in.acknowledged = signed_in.reset_notice.take(),
                    Err(error) if error.is_unauthenticated() => self.session_ended(),
                    // The notice stays, to acknowledge again.
                    Err(error) => signed_in.message = Some(error.message),
                }
            }
            (ApiReply::Account(result), Session::SignedIn(signed_in)) => {
                let admin = signed_in.is_admin();
                signed_in.account = match result {
                    Ok(CodexAccount {
                        signed_in: true,
                        email,
                        plan,
                    }) => Account::SignedIn {
                        // A member is never told as whom, whatever arrives.
                        email: email.filter(|_| admin),
                        plan: plan.filter(|_| admin),
                    },
                    Ok(_) => Account::SignedOut,
                    Err(error) if error.is_unauthenticated() => {
                        self.session_ended();
                        return true;
                    }
                    Err(error) => Account::Failed(error.message),
                };
            }
            (ApiReply::Login(result), Session::SignedIn(signed_in))
                if signed_in
                    .household
                    .as_ref()
                    .is_some_and(|household| matches!(household.login, Login::Starting)) =>
            {
                let next_poll = self.next_poll();
                let login = match result {
                    Ok(code) => following(code, next_poll),
                    Err(error) if error.is_unauthenticated() => {
                        self.session_ended();
                        return true;
                    }
                    Err(error) => Login::Failed(error.message),
                };
                self.login_moved(login);
            }
            (ApiReply::LoginStatus(result), Session::SignedIn(signed_in))
                if signed_in.household.as_ref().is_some_and(|household| {
                    matches!(household.login, Login::Waiting { polling: true, .. })
                }) =>
            {
                let next_poll = self.next_poll();
                let Session::SignedIn(signed_in) = &mut self.session else {
                    return false;
                };
                let Some(household) = signed_in.household.as_mut() else {
                    return false;
                };
                let login = match result {
                    Ok(code) => following(code, next_poll),
                    Err(error) if error.is_unauthenticated() => {
                        self.session_ended();
                        return true;
                    }
                    // The gateway restarted and lost the sign-in, along with
                    // Codex's own pending login.
                    Err(error) if error.code == ErrorCode::NotFound => Login::Failed(
                        "Bokkie's server no longer knows this sign-in. Get a new code".into(),
                    ),
                    // A failed poll does not unsay the code; polling goes on.
                    Err(failure) => {
                        if let Login::Waiting {
                            next_poll: next,
                            polling,
                            error,
                            ..
                        } = &mut household.login
                        {
                            *polling = false;
                            *next = next_poll;
                            *error = Some(failure.message);
                        }
                        return true;
                    }
                };
                self.login_moved(login);
            }
            (ApiReply::LoginCancelled(result), Session::SignedIn(signed_in))
                if signed_in.household.as_ref().is_some_and(|household| {
                    matches!(household.login, Login::Cancelling { .. })
                }) =>
            {
                let next_poll = self.next_poll();
                let Session::SignedIn(signed_in) = &mut self.session else {
                    return false;
                };
                let Some(household) = signed_in.household.as_mut() else {
                    return false;
                };
                let login = match result {
                    Ok(code) => following(code, next_poll),
                    Err(error) if error.is_unauthenticated() => {
                        self.session_ended();
                        return true;
                    }
                    Err(error) if error.code == ErrorCode::NotFound => Login::Ended {
                        state: LoginState::Cancelled,
                        error: None,
                    },
                    // Not cancelled: the code stays, with why.
                    Err(failure) => {
                        let Login::Cancelling { code } =
                            std::mem::replace(&mut household.login, Login::Idle)
                        else {
                            return false;
                        };
                        Login::Waiting {
                            code,
                            next_poll,
                            polling: false,
                            error: Some(failure.message),
                        }
                    }
                };
                self.login_moved(login);
            }
            (ApiReply::HouseholdSignedOut(result), Session::SignedIn(signed_in))
                if signed_in
                    .household
                    .as_ref()
                    .is_some_and(|household| household.sign_out == SignOut::Busy) =>
            {
                let Some(household) = signed_in.household.as_mut() else {
                    return false;
                };
                match result {
                    Ok(()) => {
                        household.sign_out = SignOut::Idle;
                        household.login = Login::Idle;
                        signed_in.account = Account::SignedOut;
                        self.send(ApiRequest::CodexAccount);
                    }
                    Err(error) if error.is_unauthenticated() => self.session_ended(),
                    Err(error) => household.sign_out = SignOut::Failed(error.message),
                }
            }
            (ApiReply::Usage(result), Session::SignedIn(_)) if self.usage_in_flight => {
                self.usage_in_flight = false;
                let changed = match result {
                    Ok(usage) => self.store.load_usage(usage),
                    Err(error) if error.is_unauthenticated() => {
                        self.session_ended();
                        return true;
                    }
                    // The meter stays as it was; events or the next reload
                    // bring it up to date.
                    Err(_) => false,
                };
                if std::mem::take(&mut self.usage_again) {
                    self.load_usage();
                }
                return changed;
            }
            // A reply to a command whose screen has gone, such as a Bot list
            // that arrives after signing out.
            _ => return false,
        }
        true
    }

    /// Change one field of an admin form.
    fn edit_admin(&mut self, field: Field, text: String) -> bool {
        match &mut self.session {
            Session::SignedIn(signed_in) => crate::admin::edit(signed_in, field, text),
            _ => false,
        }
    }

    /// Change one field of the profile being edited, unless it is saving:
    /// an admin-made Bot's in the admin section, or the open Bot's.
    fn edit_profile(&mut self, field: fn(&mut ProfileEditor) -> &mut String, text: String) -> bool {
        if let Session::SignedIn(signed_in) = &mut self.session
            && let Some(editor) = crate::admin::editor(signed_in)
        {
            if editor.busy {
                return false;
            }
            *field(editor) = text;
            return true;
        }
        match self.open_mut().and_then(|open| open.editor.as_mut()) {
            Some(editor) if !editor.busy => {
                *field(editor) = text;
                true
            }
            _ => false,
        }
    }

    /// The time the next poll of a pending sign-in is due.
    fn edit_routine(
        &mut self,
        field: fn(&mut crate::settings::RoutineForm) -> &mut String,
        text: String,
    ) -> bool {
        match self
            .open_mut()
            .and_then(|open| open.settings.as_mut())
            .and_then(|settings| settings.form.as_mut())
        {
            Some(form) if !form.busy => {
                *field(form) = text;
                true
            }
            _ => false,
        }
    }

    /// Bring a message of the open conversation into view.
    pub(crate) fn go_to(&mut self, message: MessageId) {
        self.gotos += 1;
        let gotos = self.gotos;
        if let Some(open) = self.open_mut() {
            open.goto = Some((gotos, message));
            open.goto_note = None;
            open.goto_pages = 0;
        }
        self.seek_goto();
    }

    /// Load earlier pages until the conversation holds the message asked
    /// for, or say it cannot be found.
    fn seek_goto(&mut self) {
        let Some((bot, (_, message))) = self.open().and_then(|open| Some((open.bot, open.goto?)))
        else {
            return;
        };
        let Some(conversation) = self.store.conversation(bot) else {
            // The conversation's first page is still loading.
            return;
        };
        if conversation.message(message).is_some() {
            return;
        }
        let far = self
            .open()
            .is_some_and(|open| open.goto_pages >= GOTO_PAGES);
        if conversation.older().is_some() && !far {
            // After a failed page, Load older tries again and carries on.
            if self.open().is_some_and(|open| open.older_error.is_none())
                && !self.in_flight.contains(&Snapshot::Older(bot))
            {
                if let Some(open) = self.open_mut() {
                    open.goto_pages += 1;
                }
                self.load(Snapshot::Older(bot));
            }
            return;
        }
        if let Some(open) = self.open_mut() {
            open.goto = None;
            open.goto_note = Some(if far {
                "That run is more than 1,000 messages back. Load older messages to find it".into()
            } else {
                "That run's messages are no longer in the conversation".into()
            });
        }
    }

    fn next_poll(&self) -> Timestamp {
        Timestamp(self.now().0 + LOGIN_POLL.as_millis() as i64)
    }

    /// The household's sign-in moved on. Once it completes, the household
    /// is signed in, and the account says as whom.
    fn login_moved(&mut self, login: Login) {
        let Session::SignedIn(signed_in) = &mut self.session else {
            return;
        };
        let completed = matches!(
            login,
            Login::Ended {
                state: LoginState::Completed,
                ..
            }
        );
        if let Some(household) = &mut signed_in.household {
            household.login = login;
        }
        if completed {
            signed_in.account = Account::Checking;
            self.send(ApiRequest::CodexAccount);
        }
    }

    fn socket(&mut self, update: SocketUpdate) -> bool {
        let Session::SignedIn(signed_in) = &mut self.session else {
            return false;
        };
        match update {
            SocketUpdate::Event(envelope) => {
                let mut changed = false;
                if let Event::AccountUpdated { user } = &envelope.event
                    && user.id == signed_in.user.id
                {
                    self.account_event_seen = true;
                    changed = signed_in.user != *user;
                    signed_in.account_changed(user.clone());
                }
                let applied = self.store.apply(&envelope);
                self.load_all(applied.reload);
                match &envelope.event {
                    Event::RoutineRunUpdated { run } => changed |= self.run_updated(run),
                    Event::BotStatusChanged {
                        bot_id,
                        status: BotStatus::Idle,
                    } => changed |= self.turn_ended(*bot_id),
                    Event::RoutineDeleted {
                        bot_id, routine_id, ..
                    } => changed |= self.routine_gone(*bot_id, *routine_id),
                    _ => {}
                }
                changed || applied.changed
            }
            SocketUpdate::Reload => {
                self.reload_everything();
                true
            }
            SocketUpdate::CheckSession => {
                self.check_session();
                false
            }
            SocketUpdate::SignedOut => {
                self.session_ended();
                true
            }
            SocketUpdate::Link(link) => {
                signed_in.link = link;
                if link == Link::Live {
                    // Retry what failed to load, now that the server answers.
                    let mut failed = Vec::new();
                    if signed_in.bots_error.is_some() {
                        failed.push(Snapshot::Bots);
                    }
                    if let Some(open) = &signed_in.open
                        && open.error.is_some()
                    {
                        failed.push(Snapshot::Messages(open.bot));
                    }
                    if let Some(open) = &signed_in.open
                        && open
                            .settings
                            .as_ref()
                            .is_some_and(|s| s.routines_error.is_some())
                    {
                        failed.push(Snapshot::Routines(open.bot));
                    }
                    self.load_all(failed);
                }
                true
            }
        }
    }

    pub(crate) fn open_conversation(&mut self, bot: BotId) -> Result<bool, String> {
        let Session::SignedIn(signed_in) = &mut self.session else {
            return Ok(false);
        };
        if self.store.bot(bot).is_none() {
            return Err(format!("There is no Bot {bot}"));
        }
        // The content pane holds one conversation, or another screen.
        signed_in.household = None;
        signed_in.admin = None;
        // Choosing the open Bot again, from the list beside it, keeps its
        // conversation as it stands and brings it into view: outside the
        // columns the settings give way to it.
        if signed_in.open.as_ref().is_some_and(|open| open.bot == bot) {
            let shown = self.layout == NavigationLayout::Columns || self.close_settings();
            return Ok(shown);
        }
        if let Some(open) = signed_in.take_open() {
            self.store.close_conversation(open.bot);
            self.store.close_routines(open.bot);
        }
        let mut open = Open::new(bot);
        open.draft = signed_in.drafts.remove(&bot).unwrap_or_default();
        signed_in.open = Some(open);
        signed_in.push.opened();
        self.load(Snapshot::Messages(bot));
        Ok(true)
    }

    /// The `older` fact, and whether the Load older control shows: `more`
    /// while earlier messages remain to load, `loading` while a page loads,
    /// `failed` after a page failed, and empty at the conversation's start.
    pub(crate) fn older_state(&self) -> &'static str {
        let Some(open) = self.open() else {
            return "";
        };
        if self.in_flight.contains(&Snapshot::Older(open.bot)) {
            "loading"
        } else if self
            .store
            .conversation(open.bot)
            .is_none_or(|conversation| conversation.older().is_none())
        {
            ""
        } else if open.older_error.is_some() {
            "failed"
        } else {
            "more"
        }
    }

    /// Send `text` to the open Bot, as the composer or an agent gives it.
    /// The draft is cleared once the gateway accepts it, and kept if it
    /// refuses it.
    fn send_text(&mut self, text: &str) -> Result<bool, String> {
        self.can_send(Some(text))?;
        let Session::SignedIn(signed_in) = &mut self.session else {
            return Ok(false);
        };
        let Some(open) = &mut signed_in.open else {
            return Ok(false);
        };
        open.sending = true;
        open.delivery = None;
        open.send_error = None;
        open.stop_note = None;
        let bot = open.bot;
        let text = text.trim().to_owned();
        self.send(ApiRequest::SendMessage(bot, SendMessageRequest { text }));
        Ok(true)
    }

    fn can_stop(&self) -> Result<(), &'static str> {
        let Some(open) = self.open() else {
            return Err("No conversation is open");
        };
        if open.stopping {
            return Err("Stopping");
        }
        match self.store.bot(open.bot).map(|bot| bot.status) {
            Some(BotStatus::Working) => Ok(()),
            _ => Err("No reply is running"),
        }
    }

    /// Whether the Stop control is shown: while a reply runs or a stop is
    /// under way.
    pub(crate) fn shows_stop(&self) -> bool {
        self.open().is_some_and(|open| open.stopping) || self.can_stop().is_ok()
    }

    /// The limit-reached banner, when the subscription or the user's
    /// allowance is used up.
    pub fn limit_banner(&self) -> Option<String> {
        crate::usage::limit_banner(self.store.usage()?, self.now())
    }

    /// The time now, from the transport.
    pub(crate) fn now(&self) -> Timestamp {
        self.api.as_ref().map_or(Timestamp(0), |api| api.now())
    }

    /// How times are shown now: in the user's own zone, or the viewer's
    /// until it is known, in the viewer's locale, with the transport's
    /// clock.
    pub(crate) fn local_time(&self) -> LocalTime {
        let viewer = self.viewer.clone().unwrap_or_else(|| {
            LocalTime::new(TimeZone::utc(), TimeLocale::ENGLISH_US, Clock::System)
        });
        let local = match &self.session {
            Session::SignedIn(signed_in) => match &signed_in.zone {
                Some(zone) => viewer.with_zone(zone.clone()),
                None => viewer,
            },
            _ => viewer,
        };
        local.with_clock(Clock::Fixed(crate::times::instant(self.now())))
    }

    /// The open Bot, if the user may change it now: not while a change to it
    /// is under way.
    pub(crate) fn can_update(&self) -> Result<&Bot, &'static str> {
        let Some(open) = self.open() else {
            return Err("No conversation is open");
        };
        if open.updating || open.editor.as_ref().is_some_and(|editor| editor.busy) {
            return Err("Saving the Bot");
        }
        self.store.bot(open.bot).ok_or("Loading the Bot")
    }

    /// Whether `text` can be sent to the open Bot now; without a text, its
    /// draft.
    fn can_send(&self, text: Option<&str>) -> Result<(), &'static str> {
        let Some(open) = self.open() else {
            return Err("No conversation is open");
        };
        if open.sending {
            return Err("Sending");
        }
        if text.unwrap_or(&open.draft).trim().is_empty() {
            return Err("Write a message");
        }
        match self.store.bot(open.bot).map(|bot| bot.status) {
            Some(BotStatus::Refused) => Err("This Bot accepts no messages"),
            _ => Ok(()),
        }
    }
}

impl Application for App {
    type Action = BokkieAction;
    type Intent = BokkieIntent;
    type UiState = ();

    fn identity(&self) -> ApplicationIdentity {
        application_identity!(APPLICATION_NAME)
    }

    fn preferences(&self) -> UiPreferences {
        self.preferences
    }

    fn availability(&self, target: ActionTarget<BokkieAction>) -> Availability {
        if let Some(reason) = self.admin_availability(target.action) {
            return reason.map_or(Availability::Enabled, |reason| Availability::Disabled {
                reason: reason.into(),
            });
        }
        if let Some(reason) = self.settings_availability(target.action) {
            return reason.map_or(Availability::Enabled, |reason| Availability::Disabled {
                reason,
            });
        }
        if target.action == BokkieAction::Notifications {
            return self
                .notifications_availability()
                .map_or(Availability::Enabled, |reason| Availability::Disabled {
                    reason,
                });
        }
        let signed_in = match &self.session {
            Session::SignedIn(signed_in) => Some(signed_in),
            _ => None,
        };
        let reason = match (target.action, &self.session) {
            (BokkieAction::SignIn, Session::SignedOut(form)) => {
                if form.joining {
                    Some("Join with your invitation, or sign in instead")
                } else if form.busy {
                    Some("Signing in")
                } else if form.username.trim().is_empty() {
                    Some("Enter your username")
                } else if form.password.is_empty() {
                    Some("Enter your password")
                } else {
                    None
                }
            }
            (BokkieAction::SignIn, _) => Some("Already signed in"),
            (BokkieAction::Join, Session::SignedOut(form)) if form.busy => Some("Signing in"),
            (BokkieAction::Join, Session::SignedOut(_)) => None,
            (BokkieAction::Join, _) => Some("Already signed in"),
            (BokkieAction::AcceptInvitation, Session::SignedOut(form)) => {
                if !form.joining {
                    Some("Choose I have an invitation first")
                } else if form.busy {
                    Some("Joining")
                } else if form.code.trim().is_empty() {
                    Some("Enter the invitation code")
                } else if form.username.trim().is_empty() {
                    Some("Choose a username")
                } else if form.display_name.trim().is_empty() {
                    Some("Enter your name")
                } else if form.password.chars().count() < 10 {
                    Some("Choose a password of at least 10 characters")
                } else {
                    None
                }
            }
            (BokkieAction::AcceptInvitation, _) => Some("Already signed in"),
            (BokkieAction::ReloadPage, _) => {
                (self.link() != Link::Outdated).then_some("This page matches Bokkie's server")
            }
            (_, _) if signed_in.is_none() => Some("Not signed in"),
            (BokkieAction::SignOut, _) if signed_in.is_some_and(|s| s.signing_out) => {
                Some("Signing out")
            }
            (BokkieAction::SignOut, _) => None,
            (BokkieAction::Refresh, _)
                if self
                    .open_bot()
                    .is_some_and(|bot| self.in_flight.contains(&Snapshot::Messages(bot))) =>
            {
                Some("Loading the conversation")
            }
            // The list has Refresh Bots of its own.
            (BokkieAction::Refresh, _)
                if self.open().is_none() && self.admin_screen().is_none() =>
            {
                Some("No conversation is open")
            }
            (BokkieAction::Refresh, _) => None,
            (BokkieAction::RefreshBots, _) if self.in_flight.contains(&Snapshot::Bots) => {
                Some("Loading the Bots")
            }
            (BokkieAction::RefreshBots, _) => None,
            (BokkieAction::OpenBot | BokkieAction::OpenPinned, _) if !self.store.bots_loaded() => {
                Some("Loading the Bots")
            }
            (BokkieAction::OpenBot | BokkieAction::OpenPinned, _) => None,
            (BokkieAction::ShowPrompt, _) if self.open().is_none() => {
                Some("No conversation is open")
            }
            (BokkieAction::ShowPrompt, _) => None,
            (BokkieAction::Back, _)
                if self.open().is_none()
                    && self.household().is_none()
                    && self.admin_screen().is_none() =>
            {
                Some("Already on the list of Bots")
            }
            (BokkieAction::Back, _) => None,
            (BokkieAction::LoadOlder, _) => match (self.open_bot(), self.older_state()) {
                (None, _) => Some("No conversation is open"),
                (_, "loading") => Some("Loading older messages"),
                (Some(bot), "") if self.store.conversation(bot).is_none() => {
                    Some("Loading the conversation")
                }
                (_, "") => Some("No older messages"),
                _ => None,
            },
            (BokkieAction::CreateBot, _) => signed_in.and_then(|signed_in| {
                if !signed_in.may_create_bots() {
                    Some("Only an admin can make your Bots")
                } else if signed_in.new_bot.busy {
                    Some("Creating the Bot")
                } else if signed_in.new_bot.name.trim().is_empty() {
                    Some("Enter a name")
                } else {
                    None
                }
            }),
            (BokkieAction::ShowHidden, _) => None,
            (BokkieAction::Pin | BokkieAction::Hide, _) => self.can_update().err(),
            (BokkieAction::EditProfile, _) => match (signed_in, self.can_update()) {
                (Some(signed_in), _) if !signed_in.may_edit_profiles() => {
                    Some("Only an admin can change your Bots' profiles")
                }
                (_, Err(reason)) => Some(reason),
                _ if self.open().is_some_and(|open| open.editor.is_some()) => {
                    Some("Already editing")
                }
                _ => None,
            },
            (BokkieAction::SaveProfile, _) => match self.open().and_then(|o| o.editor.as_ref()) {
                None => Some("No profile is being edited"),
                Some(editor) if editor.busy => Some("Saving the profile"),
                Some(editor) if editor.name.trim().is_empty() => Some("Enter a name"),
                Some(_) if signed_in.is_some_and(|s| !s.may_edit_profiles()) => {
                    Some("Only an admin can change your Bots' profiles")
                }
                Some(_) => None,
            },
            (BokkieAction::CancelProfile, _) => match self.open().and_then(|o| o.editor.as_ref()) {
                None => Some("No profile is being edited"),
                Some(editor) if editor.busy => Some("Saving the profile"),
                Some(_) => None,
            },
            (BokkieAction::AcknowledgeReset, _) => signed_in.and_then(|signed_in| {
                if signed_in.reset_notice.is_none() {
                    Some("Nothing to acknowledge")
                } else if signed_in.acknowledging {
                    Some("Acknowledging")
                } else {
                    None
                }
            }),
            (BokkieAction::Household, _) => signed_in.and_then(|signed_in| {
                if !signed_in.is_admin() {
                    Some("Only an admin signs the household in to ChatGPT or out")
                } else if signed_in.household.is_some() {
                    Some("The ChatGPT sign-in is open")
                } else {
                    None
                }
            }),
            (
                BokkieAction::StartChatGpt
                | BokkieAction::CancelChatGpt
                | BokkieAction::SignOutChatGpt
                | BokkieAction::ConfirmSignOutChatGpt
                | BokkieAction::KeepChatGpt,
                _,
            ) => match signed_in.and_then(|s| s.household.as_ref().map(|h| (s, h))) {
                None => Some("Open the ChatGPT sign-in first"),
                Some((signed_in, household)) => {
                    household_availability(target.action, &signed_in.account, household)
                }
            },
            (BokkieAction::Send, _) => self.can_send(None).err(),
            (BokkieAction::Stop, _) => self.can_stop().err(),
            // The admin section's actions answered above.
            _ => None,
        };
        reason.map_or(Availability::Enabled, |reason| Availability::Disabled {
            reason: reason.into(),
        })
    }

    /// A row's action is available while its row is listed: a Bot in the
    /// list, an account, an invitation, a member's Bot or a routine run's
    /// marker. A click or an invocation for anything else is refused.
    fn availability_with(
        &self,
        target: ActionTarget<BokkieAction>,
        argument: &ActionArgument,
    ) -> Availability {
        // The composer asks about the text it would send.
        if let (BokkieAction::Send, ActionArgument::Text(text)) = (target.action, argument) {
            return match self.can_send(Some(text)) {
                Ok(()) => Availability::Enabled,
                Err(reason) => Availability::Disabled {
                    reason: reason.into(),
                },
            };
        }
        let id = match argument {
            ActionArgument::Number(id) if id.fract() == 0.0 && (1.0..=MAX_ID).contains(id) => {
                Some(*id as i64)
            }
            _ => None,
        };
        let listed = match (target.action, id) {
            (BokkieAction::OpenBot, Some(id)) => self.store.bot(BotId(id)).is_some(),
            (BokkieAction::OpenPinned, Some(id)) => {
                self.strip().iter().any(|bot| bot.id == BotId(id))
            }
            (BokkieAction::ShowPrompt, Some(id)) => self.open().is_some_and(|open| {
                self.store
                    .conversation(open.bot)
                    .and_then(|conversation| conversation.message(MessageId(id)))
                    .is_some_and(|message| message.routine.is_some())
            }),
            (
                action @ (BokkieAction::OpenAccount
                | BokkieAction::WithdrawInvitation
                | BokkieAction::EditMemberBot
                | BokkieAction::ReadConversation),
                Some(id),
            ) => self.admin_lists(action, id),
            (action, Some(id)) => self.settings_lists(action, id).unwrap_or(true),
            // An argument the action's contract refuses is refused there.
            _ => true,
        };
        if !listed {
            return Availability::Hidden;
        }
        match (self.availability(target), id) {
            (Availability::Enabled, Some(id)) => self
                .routine_availability(target.action, id)
                .map_or(Availability::Enabled, |reason| Availability::Disabled {
                    reason,
                }),
            (availability, _) => availability,
        }
    }

    fn started(&mut self, intents: IntentSender<BokkieIntent>) {
        if let Some(make_api) = self.make_api.take() {
            self.api = Some(make_api(intents));
        }
        self.viewer = self.api.as_ref().map(|api| api.local_time());
        self.send(ApiRequest::CheckSession);
    }

    /// Bring the device and the gateway in line with the state (see
    /// `notifications`), and poll the household's sign-in while its device
    /// code is on screen.
    fn prepare(&mut self, pass: &mut PassContext<'_>, _: &mut ()) {
        // The navigation lays its panes out across the whole page.
        self.layout = NavigationLayout::for_width(pass.available_rect().width());
        self.theme = pass.context().theme();
        self.sync();
        let now = self.now();
        let Session::SignedIn(signed_in) = &mut self.session else {
            return;
        };
        let Some(Household {
            login: Login::Waiting {
                next_poll, polling, ..
            },
            ..
        }) = &mut signed_in.household
        else {
            return;
        };
        if *polling {
            return;
        }
        if now >= *next_poll {
            *polling = true;
            self.send(ApiRequest::CodexLoginStatus);
        } else {
            let wait = u64::try_from(next_poll.0 - now.0).unwrap_or(0);
            pass.request_repaint_after(
                "ChatGPT sign-in poll",
                std::time::Duration::from_millis(wait),
            );
        }
    }

    fn ui(&self, ui: &mut egui::Ui, _: &mut (), frame: &mut Frame<'_, BokkieAction, BokkieIntent>) {
        ui::present(self, ui, frame);
    }

    fn apply(&mut self, intent: BokkieIntent, _: &mut ()) -> Result<bool, String> {
        Ok(match intent {
            BokkieIntent::Foreground => {
                let viewer = self.api.as_ref().map(|api| api.local_time());
                let changed = viewer != self.viewer;
                self.viewer = viewer;
                self.set_hidden(false) || changed
            }
            BokkieIntent::Background => self.set_hidden(true),
            BokkieIntent::Push(event) => self.push_event(event),
            BokkieIntent::EditUsername(username) => match &mut self.session {
                Session::SignedOut(form) if !form.busy => {
                    form.username = username;
                    true
                }
                _ => false,
            },
            BokkieIntent::EditPassword(password) => match &mut self.session {
                Session::SignedOut(form) if !form.busy => {
                    form.password = password;
                    true
                }
                _ => false,
            },
            BokkieIntent::EditDraft(draft) => match self.open_mut() {
                Some(open) if !open.sending => {
                    open.draft = draft;
                    true
                }
                _ => false,
            },
            BokkieIntent::EditBotName(name) => match &mut self.session {
                Session::SignedIn(signed_in) if !signed_in.new_bot.busy => {
                    signed_in.new_bot.name = name;
                    true
                }
                _ => false,
            },
            BokkieIntent::EditInvitationCode(text) => match &mut self.session {
                Session::SignedOut(form) if !form.busy => {
                    form.code = text;
                    true
                }
                _ => false,
            },
            BokkieIntent::EditDisplayName(text) => match &mut self.session {
                Session::SignedOut(form) if !form.busy => {
                    form.display_name = text;
                    true
                }
                _ => false,
            },
            BokkieIntent::EditInviteName(text) => self.edit_admin(Field::InviteName, text),
            BokkieIntent::EditDailyTurns(text) => self.edit_admin(Field::DailyTurns, text),
            BokkieIntent::EditWeeklyTokens(text) => self.edit_admin(Field::WeeklyTokens, text),
            BokkieIntent::EditNewPassword(text) => self.edit_admin(Field::NewPassword, text),
            BokkieIntent::EditMemberBotName(text) => self.edit_admin(Field::MemberBotName, text),
            BokkieIntent::EditProfileName(text) => self.edit_profile(|e| &mut e.name, text),
            BokkieIntent::EditProfileLabel(text) => self.edit_profile(|e| &mut e.label, text),
            BokkieIntent::EditProfileDescription(text) => {
                self.edit_profile(|e| &mut e.description, text)
            }
            BokkieIntent::EditRoutineName(text) => self.edit_routine(|f| &mut f.name, text),
            BokkieIntent::EditRoutinePrompt(text) => self.edit_routine(|f| &mut f.prompt, text),
            BokkieIntent::EditRoutineAt(text) => self.edit_routine(|f| &mut f.at, text),
            BokkieIntent::EditRoutineEvery(text) => self.edit_routine(|f| &mut f.every, text),
            BokkieIntent::EditTimeZone(text) => match &mut self.session {
                Session::SignedIn(signed_in) if !signed_in.zone_saving => {
                    signed_in.zone_draft = Some(text);
                    true
                }
                _ => false,
            },
            BokkieIntent::Reply { generation, reply } => {
                generation == self.generation && self.reply(reply)
            }
            BokkieIntent::Socket { generation, update } => {
                generation == self.generation && self.socket(update)
            }
        })
    }

    fn perform(&mut self, target: ActionTarget<BokkieAction>, _: &mut ()) -> Result<bool, String> {
        if let Some(done) = self.admin_perform(target.action) {
            return done;
        }
        if let Some(done) = self.settings_perform(target.action) {
            return Ok(done);
        }
        if target.action == BokkieAction::Notifications {
            // Inside the tap, where Safari lets a page subscribe.
            return Ok(self.toggle_notifications());
        }
        if let Some(bot) = self.settings().and(self.open_bot()) {
            match target.action {
                BokkieAction::Refresh => {
                    self.load_settings(bot);
                    return Ok(true);
                }
                BokkieAction::Back => return Ok(self.close_settings()),
                _ => {}
            }
        }
        if self.admin_screen().is_some() {
            match target.action {
                BokkieAction::Refresh => return Ok(self.admin_refresh()),
                BokkieAction::Back => return Ok(self.admin_back()),
                _ => {}
            }
        }
        match (target.action, &mut self.session) {
            (BokkieAction::Join, Session::SignedOut(form)) => {
                form.joining = !form.joining;
                form.message = None;
            }
            (BokkieAction::AcceptInvitation, Session::SignedOut(form)) => {
                form.busy = true;
                form.message = None;
                // The password leaves the form as soon as it is sent.
                let accept = bokkie_protocol::api::AcceptInvitationRequest {
                    code: form.code.trim().to_owned(),
                    username: form.username.trim().to_owned(),
                    password: std::mem::take(&mut form.password),
                    display_name: form.display_name.trim().to_owned(),
                };
                self.send(ApiRequest::AcceptInvitation(accept));
            }
            (BokkieAction::SignIn, Session::SignedOut(form)) => {
                form.busy = true;
                form.message = None;
                // The password leaves the form as soon as it is sent.
                let login = LoginRequest {
                    username: form.username.trim().to_owned(),
                    password: std::mem::take(&mut form.password),
                };
                self.send(ApiRequest::SignIn(login));
            }
            (BokkieAction::SignOut, Session::SignedIn(signed_in)) => {
                signed_in.signing_out = true;
                signed_in.message = None;
                self.send(ApiRequest::SignOut);
            }
            // The open conversation; the settings and the admin section
            // took theirs above.
            (BokkieAction::Refresh, Session::SignedIn(signed_in)) => match &signed_in.open {
                Some(open) => {
                    let bot = open.bot;
                    self.load(Snapshot::Messages(bot));
                }
                None => return Ok(false),
            },
            // Wherever the user is, the Bots and the household's sign-in,
            // which announces no events of its own.
            (BokkieAction::RefreshBots, Session::SignedIn(_)) => {
                self.load(Snapshot::Bots);
                self.send(ApiRequest::CodexAccount);
            }
            (BokkieAction::Back, Session::SignedIn(signed_in)) => {
                // Leaving the ChatGPT sign-in drops its code with it.
                if signed_in.household.take().is_some() {
                    return Ok(true);
                }
                let Some(open) = signed_in.take_open() else {
                    return Ok(false);
                };
                self.store.close_conversation(open.bot);
                self.store.close_routines(open.bot);
            }
            (BokkieAction::LoadOlder, Session::SignedIn(signed_in)) => {
                let Some(open) = &mut signed_in.open else {
                    return Ok(false);
                };
                open.older_error = None;
                let bot = open.bot;
                self.load(Snapshot::Older(bot));
            }
            (BokkieAction::CreateBot, Session::SignedIn(signed_in)) => {
                let form = &mut signed_in.new_bot;
                form.busy = true;
                form.error = None;
                let request = CreateBotRequest {
                    name: form.name.trim().to_owned(),
                    label: String::new(),
                    description: String::new(),
                    avatar: None,
                };
                self.send(ApiRequest::CreateBot(request));
            }
            (BokkieAction::ShowHidden, Session::SignedIn(signed_in)) => {
                signed_in.show_hidden = !signed_in.show_hidden;
            }
            (BokkieAction::Pin | BokkieAction::Hide, Session::SignedIn(signed_in)) => {
                let Some(open) = &mut signed_in.open else {
                    return Ok(false);
                };
                let Some(bot) = self.store.bot(open.bot) else {
                    return Ok(false);
                };
                let changes = if target.action == BokkieAction::Pin {
                    UpdateBotRequest {
                        pinned: Some(!bot.pinned),
                        ..Default::default()
                    }
                } else {
                    UpdateBotRequest {
                        hidden: Some(!bot.hidden),
                        ..Default::default()
                    }
                };
                open.updating = true;
                open.update_error = None;
                let bot = open.bot;
                self.send(ApiRequest::UpdateBot(bot, changes));
            }
            (BokkieAction::EditProfile, Session::SignedIn(signed_in)) => {
                let Some(open) = &mut signed_in.open else {
                    return Ok(false);
                };
                let Some(bot) = self.store.bot(open.bot) else {
                    return Ok(false);
                };
                open.editor = Some(ProfileEditor {
                    name: bot.name.clone(),
                    label: bot.label.clone(),
                    description: bot.description.clone(),
                    ..ProfileEditor::default()
                });
            }
            (BokkieAction::SaveProfile, Session::SignedIn(signed_in)) => {
                let Some(open) = &mut signed_in.open else {
                    return Ok(false);
                };
                let (Some(editor), Some(bot)) = (&mut open.editor, self.store.bot(open.bot)) else {
                    return Ok(false);
                };
                // Only what changed, so an unchanged field is never sent.
                let changed =
                    |edited: &str, held: &str| (edited != held).then(|| edited.to_owned());
                let name = editor.name.trim();
                let changes = UpdateBotRequest {
                    name: changed(name, &bot.name),
                    label: changed(editor.label.trim(), &bot.label),
                    description: changed(&editor.description, &bot.description),
                    ..Default::default()
                };
                if changes == UpdateBotRequest::default() {
                    open.editor = None;
                    return Ok(true);
                }
                editor.busy = true;
                editor.error = None;
                let bot = open.bot;
                self.send(ApiRequest::UpdateBot(bot, changes));
            }
            (BokkieAction::CancelProfile, Session::SignedIn(signed_in)) => {
                if let Some(open) = &mut signed_in.open {
                    open.editor = None;
                }
            }
            (BokkieAction::AcknowledgeReset, Session::SignedIn(signed_in)) => {
                signed_in.acknowledging = true;
                signed_in.message = None;
                self.send(ApiRequest::AcknowledgeReset);
            }
            (BokkieAction::Household, Session::SignedIn(_)) => {
                // The content pane holds one thing at a time.
                self.close_content();
                let Session::SignedIn(signed_in) = &mut self.session else {
                    return Ok(false);
                };
                signed_in.household = Some(Household {
                    login: Login::Idle,
                    sign_out: SignOut::Idle,
                });
                // Ask again: it may have changed since sign-in.
                signed_in.account = Account::Checking;
                self.send(ApiRequest::CodexAccount);
            }
            (BokkieAction::StartChatGpt, Session::SignedIn(signed_in)) => {
                if let Some(household) = &mut signed_in.household {
                    household.login = Login::Starting;
                    household.sign_out = SignOut::Idle;
                }
                self.send(ApiRequest::CodexLogin);
            }
            (BokkieAction::CancelChatGpt, Session::SignedIn(signed_in)) => {
                let Some(household) = &mut signed_in.household else {
                    return Ok(false);
                };
                let Login::Waiting { code, .. } =
                    std::mem::replace(&mut household.login, Login::Idle)
                else {
                    return Ok(false);
                };
                household.login = Login::Cancelling { code };
                self.send(ApiRequest::CancelCodexLogin);
            }
            (BokkieAction::SignOutChatGpt, Session::SignedIn(signed_in)) => {
                if let Some(household) = &mut signed_in.household {
                    household.sign_out = SignOut::Confirming;
                }
            }
            (BokkieAction::KeepChatGpt, Session::SignedIn(signed_in)) => {
                if let Some(household) = &mut signed_in.household {
                    household.sign_out = SignOut::Idle;
                }
            }
            (BokkieAction::ConfirmSignOutChatGpt, Session::SignedIn(signed_in)) => {
                if let Some(household) = &mut signed_in.household {
                    household.sign_out = SignOut::Busy;
                }
                self.send(ApiRequest::CodexLogout);
            }
            (BokkieAction::Stop, Session::SignedIn(signed_in)) => {
                let Some(open) = &mut signed_in.open else {
                    return Ok(false);
                };
                open.stopping = true;
                open.stop_note = None;
                let bot = open.bot;
                self.send(ApiRequest::StopBot(bot));
            }
            (BokkieAction::ReloadPage, _) => {
                if let Some(api) = &self.api {
                    api.reload_page();
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn perform_with(
        &mut self,
        target: ActionTarget<BokkieAction>,
        argument: ActionArgument,
        state: &mut (),
    ) -> Result<bool, String> {
        match (target.action, argument) {
            (
                action @ (BokkieAction::OpenAccount
                | BokkieAction::WithdrawInvitation
                | BokkieAction::EditMemberBot
                | BokkieAction::ReadConversation),
                ActionArgument::Number(id),
            ) if id.fract() == 0.0 && (1.0..=MAX_ID).contains(&id) => self
                .admin_perform_with(action, id as i64)
                .unwrap_or(Ok(false)),
            (
                BokkieAction::OpenAccount
                | BokkieAction::WithdrawInvitation
                | BokkieAction::EditMemberBot
                | BokkieAction::ReadConversation,
                _,
            ) => Err(format!(
                "{} needs an ID",
                target.action.specification().label
            )),
            (action, ActionArgument::Number(id))
                if id.fract() == 0.0
                    && (1.0..=MAX_ID).contains(&id)
                    && self.settings_lists(action, id as i64).is_some() =>
            {
                self.settings_perform_with(action, id as i64)
                    .unwrap_or(Ok(false))
            }
            (BokkieAction::OpenBot | BokkieAction::OpenPinned, ActionArgument::Number(id))
                if id.fract() == 0.0 && (1.0..=MAX_ID).contains(&id) =>
            {
                // Exact: the contract admits only integers in range.
                self.open_conversation(BotId(id as i64))
            }
            (BokkieAction::OpenBot | BokkieAction::OpenPinned, _) => {
                Err("Open needs a Bot ID".into())
            }
            (BokkieAction::ShowPrompt, ActionArgument::Number(id))
                if id.fract() == 0.0 && (1.0..=MAX_ID).contains(&id) =>
            {
                let id = MessageId(id as i64);
                let Some(open) = self.open_mut() else {
                    return Err("No conversation is open".into());
                };
                if !open.prompts.remove(&id) {
                    open.prompts.insert(id);
                }
                Ok(true)
            }
            (BokkieAction::ShowPrompt, _) => Err("Show prompt needs a message ID".into()),
            (BokkieAction::Send, ActionArgument::Text(text)) => self.send_text(&text),
            (BokkieAction::Send, _) => Err("Send needs the message's text".into()),
            (_, ActionArgument::None) => self.perform(target, state),
            (action, _) => Err(format!(
                "{} takes no argument",
                action.specification().label
            )),
        }
    }

    fn report_error(&mut self, message: String) {
        match &mut self.session {
            Session::SignedOut(form) => form.message = Some(message),
            Session::SignedIn(signed_in) => signed_in.message = Some(message),
            Session::Checking => self.signed_out(Some(message)),
        }
    }

    fn meaning(&self, target: ActionTarget<BokkieAction>) -> String {
        match (target.action, &self.session) {
            // Never the password.
            (BokkieAction::SignIn, Session::SignedOut(form)) => format!("user:{}", form.username),
            (BokkieAction::Send, _) => match self.open() {
                Some(open) => format!("send:{}:{}", open.bot, open.draft),
                None => self.screen().into(),
            },
            _ => self.screen().into(),
        }
    }

    /// Bounded facts that agents read with `stobie observe`. The password is
    /// never one of them.
    fn observe(&self, observation: &mut Observation<'_>) {
        observation.fact("screen", self.screen());
        self.observe_navigation(observation);
        self.observe_appearance(observation);
        observation.fact(
            "joining",
            matches!(&self.session, Session::SignedOut(form) if form.joining),
        );
        self.observe_admin(observation);
        self.observe_device(observation);
        observation.fact(
            "user",
            self.user().map_or("", |user| user.username.as_str()),
        );
        observation.fact("bots", self.bots_state());
        observation.fact("bot_count", self.store.visible_bots().len());
        let strip: Vec<String> = self
            .strip()
            .iter()
            .map(|bot| bot.id.0.to_string())
            .collect();
        observation.fact("strip", strip.join(","));
        observation.fact("message", bounded(self.message().unwrap_or("")));
        observation.fact("connection", self.link().name());
        observation.fact("bot", self.open_bot().map_or(0, |bot| bot.0));
        observation.fact("conversation", self.conversation_state());
        let open = self.open();
        let bot = open.and_then(|open| self.store.bot(open.bot));
        observation.fact("bot_status", bot.map_or("", |bot| status_name(bot.status)));
        observation.fact(
            "bot_error",
            bounded(
                open.and_then(|open| self.store.error(open.bot))
                    .unwrap_or(""),
            ),
        );
        let conversation = open.and_then(|open| self.store.conversation(open.bot));
        observation.fact(
            "message_count",
            conversation.map_or(0, |conversation| conversation.messages().count()),
        );
        observation.fact("older", self.older_state());
        let reply = self.latest_reply();
        observation.fact("reply", bounded(reply.map_or("", |reply| &reply.text)));
        observation.fact(
            "reply_chars",
            reply.map_or(0, |reply| reply.text.chars().count()),
        );
        observation.fact(
            "reply_state",
            reply.map_or("", |reply| state_name(reply.state)),
        );
        observation.fact("draft", bounded(open.map_or("", |open| &open.draft)));
        observation.fact("sending", open.is_some_and(|open| open.sending));
        observation.fact(
            "delivery",
            match open.and_then(|open| open.delivery) {
                Some(Delivery::NewTurn) => "new_turn",
                Some(Delivery::Steered) => "steered",
                None => "",
            },
        );
        observation.fact("stopping", open.is_some_and(|open| open.stopping));
        observation.fact(
            "stop_note",
            bounded(
                open.and_then(|open| open.stop_note.as_deref())
                    .unwrap_or(""),
            ),
        );
        let signed_in = match &self.session {
            Session::SignedIn(signed_in) => Some(signed_in),
            _ => None,
        };
        observation.fact(
            "new_bot_name",
            bounded(signed_in.map_or("", |s| s.new_bot.name.as_str())),
        );
        observation.fact("creating", signed_in.is_some_and(|s| s.new_bot.busy));
        observation.fact(
            "create_error",
            bounded(
                signed_in
                    .and_then(|s| s.new_bot.error.as_deref())
                    .unwrap_or(""),
            ),
        );
        observation.fact(
            "role",
            match signed_in.map(|s| s.user.role) {
                None => "",
                Some(Role::Admin) => "admin",
                Some(Role::Member) => "member",
            },
        );
        observation.fact(
            "may_create_bots",
            signed_in.is_some_and(|s| s.may_create_bots()),
        );
        observation.fact(
            "may_edit_profiles",
            signed_in.is_some_and(|s| s.may_edit_profiles()),
        );
        observation.fact(
            "oversight",
            signed_in.is_some_and(|s| s.overseen().is_some()),
        );
        observation.fact(
            "reset_notice",
            signed_in.is_some_and(|s| s.reset_notice.is_some()),
        );
        observation.fact("show_hidden", signed_in.is_some_and(|s| s.show_hidden));
        observation.fact("bot_pinned", bot.is_some_and(|bot| bot.pinned));
        observation.fact("bot_hidden", bot.is_some_and(|bot| bot.hidden));
        observation.fact("updating", open.is_some_and(|open| open.updating));
        let editor = open.and_then(|open| open.editor.as_ref());
        observation.fact(
            "profile",
            match editor {
                None => "",
                Some(editor) if editor.busy => "saving",
                Some(_) => "editing",
            },
        );
        observation.fact(
            "profile_error",
            bounded(
                editor
                    .and_then(|editor| editor.error.as_deref())
                    .or(open.and_then(|open| open.update_error.as_deref()))
                    .unwrap_or(""),
            ),
        );
        observation.fact(
            "chatgpt",
            match signed_in.map(|s| &s.account) {
                None => "",
                Some(Account::Checking) => "checking",
                Some(Account::SignedIn { .. }) => "signed_in",
                Some(Account::SignedOut) => "signed_out",
                Some(Account::Failed(_)) => "failed",
            },
        );
        let household = signed_in.and_then(|s| s.household.as_ref());
        // Never the code itself.
        observation.fact(
            "chatgpt_login",
            match household.map(|h| &h.login) {
                None => "",
                Some(Login::Idle) => "idle",
                Some(Login::Starting) => "starting",
                Some(Login::Waiting { .. }) => "waiting",
                Some(Login::Cancelling { .. }) => "cancelling",
                Some(Login::Ended { state, .. }) => match state {
                    LoginState::Pending => "waiting",
                    LoginState::Completed => "completed",
                    LoginState::Expired => "expired",
                    LoginState::Failed => "failed",
                    LoginState::Cancelled => "cancelled",
                },
                Some(Login::Failed(_)) => "failed",
            },
        );
        observation.fact(
            "chatgpt_sign_out",
            match household.map(|h| &h.sign_out) {
                None | Some(SignOut::Idle) => "",
                Some(SignOut::Confirming) => "confirming",
                Some(SignOut::Busy) => "signing_out",
                Some(SignOut::Failed(_)) => "failed",
            },
        );
        let usage = self.store.usage();
        observation.fact("usage", if usage.is_some() { "loaded" } else { "" });
        observation.fact(
            "subscription_used",
            usage
                .into_iter()
                .flat_map(|usage| &usage.subscription)
                .flat_map(|bucket| [&bucket.primary, &bucket.secondary])
                .flatten()
                .map(|window| i64::from(window.used_percent))
                .max()
                .unwrap_or(0),
        );
        observation.fact(
            "turns_today",
            usage.map_or(0, |usage| usage.allowance.turns_today),
        );
        observation.fact("limit", bounded(&self.limit_banner().unwrap_or_default()));
        observation.fact(
            "send_error",
            bounded(
                open.and_then(|open| open.send_error.as_deref())
                    .unwrap_or(""),
            ),
        );
        let marker = self.open_bot().and_then(|bot| {
            self.store
                .conversation(bot)?
                .messages()
                .rev()
                .find(|message| message.kind == MessageKind::Marker)
        });
        observation.fact("marker", bounded(marker.map_or("", |m| &m.text)));
        observation.fact(
            "showing",
            open.and_then(|open| open.goto).map_or(0, |(_, id)| id.0),
        );
        observation.fact(
            "showing_error",
            bounded(open.and_then(|o| o.goto_note.as_deref()).unwrap_or("")),
        );
        self.observe_settings(observation);
    }
}

/// At most 200 characters of a text, for a fact.
pub(crate) fn bounded(text: &str) -> String {
    text.chars().take(200).collect()
}

fn status_name(status: BotStatus) -> &'static str {
    match status {
        BotStatus::Idle => "idle",
        BotStatus::Working => "working",
        BotStatus::Refused => "refused",
    }
}

fn state_name(state: MessageState) -> &'static str {
    match state {
        MessageState::Streaming => "streaming",
        MessageState::Complete => "complete",
        MessageState::Interrupted => "interrupted",
        MessageState::Failed => "failed",
    }
}

/// What a sign-in's state from the gateway leaves on screen: its code while
/// pending, otherwise how it ended.
fn following(code: crate::api::DeviceCode, next_poll: Timestamp) -> Login {
    match code.0.state {
        LoginState::Pending => Login::Waiting {
            code,
            next_poll,
            polling: false,
            error: None,
        },
        state => Login::Ended {
            state,
            error: code.0.error,
        },
    }
}

/// Why a control of the household's ChatGPT sign-in is unavailable, if it
/// is. Signing in and signing out never overlap.
fn household_availability(
    action: BokkieAction,
    account: &Account,
    household: &Household,
) -> Option<&'static str> {
    let waiting = matches!(
        household.login,
        Login::Starting | Login::Waiting { .. } | Login::Cancelling { .. }
    );
    match action {
        BokkieAction::StartChatGpt => match (&household.login, account) {
            (Login::Starting, _) => Some("Asking for a code"),
            (Login::Cancelling { .. }, _) => Some("Cancelling the sign-in"),
            // While a code is shown, a new one replaces it, as when it has
            // expired.
            (Login::Waiting { .. }, _) => None,
            (_, Account::Checking) => Some("Checking the household's sign-in"),
            (_, Account::SignedIn { .. }) => Some("The household is signed in to ChatGPT"),
            _ if household.sign_out == SignOut::Busy => Some("Signing out"),
            _ => None,
        },
        BokkieAction::CancelChatGpt => match household.login {
            Login::Waiting { .. } => None,
            Login::Cancelling { .. } => Some("Cancelling the sign-in"),
            _ => Some("No sign-in is waiting"),
        },
        BokkieAction::SignOutChatGpt => match (account, &household.sign_out) {
            (Account::SignedIn { .. }, SignOut::Idle | SignOut::Failed(_)) if !waiting => None,
            (Account::SignedIn { .. }, SignOut::Confirming) => Some("Confirm or keep signed in"),
            (Account::SignedIn { .. }, SignOut::Busy) => Some("Signing out"),
            (Account::SignedIn { .. }, _) => Some("A sign-in is under way"),
            _ => Some("The household is not signed in to ChatGPT"),
        },
        BokkieAction::ConfirmSignOutChatGpt | BokkieAction::KeepChatGpt => {
            match household.sign_out {
                SignOut::Confirming => None,
                SignOut::Busy => Some("Signing out"),
                _ => Some("Sign out of ChatGPT first"),
            }
        }
        _ => None,
    }
}
