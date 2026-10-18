//! The application: sign-in and sign-out, the signed-in user's Bot list, and
//! one Bot's conversation at a time, kept current by the event socket.
//!
//! State changes only in `apply` and `perform`. Commands go out through an
//! [`Api`], and their replies come back as [`BokkieIntent::Reply`]; the event
//! socket's updates come back as [`BokkieIntent::Socket`]. Tests drive the
//! same transitions with a [`Recorder`](crate::api::Recorder) and a
//! [`Driver`](crate::socket::Driver).

use std::collections::BTreeSet;

use bokkie_protocol::api::{
    Author, BotStatus, Delivery, LoginRequest, Message, MessageState, SendMessageRequest, User,
};
use bokkie_protocol::{BotId, Timestamp};
use serde::Serialize;
use stobie_ui_egui::{
    Action, ActionArgument, ActionKey, ActionTarget, Application, ApplicationIdentity,
    Availability, Frame, IntentSender, Observation, application_identity,
};

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
/// The message field's stable ID.
pub const DRAFT_ID: &str = "bokkie.draft";

/// The largest Bot ID the Open action accepts: the largest integer a JSON
/// number carries exactly.
const MAX_BOT_ID: f64 = 9_007_199_254_740_991.0;

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
        description = "Load the list of Bots, or the open conversation, again"
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
    #[action(label = "Back", description = "Return to the list of Bots")]
    Back,
    #[action(
        label = "Send",
        description = "Send the message to the Bot; while it replies, the message steers the reply"
    )]
    Send,
    #[action(label = "Stop", description = "Stop the Bot's reply in progress")]
    Stop,
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
}

// The password must never reach a log line.
impl std::fmt::Debug for BokkieIntent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EditUsername(username) => f.debug_tuple("EditUsername").field(username).finish(),
            Self::EditPassword(_) => f.write_str("EditPassword(<redacted>)"),
            Self::EditDraft(draft) => f.debug_tuple("EditDraft").field(draft).finish(),
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
    SignedIn(SignedIn),
}

#[derive(Debug, Default)]
pub(crate) struct SignInForm {
    pub(crate) username: String,
    pub(crate) password: String,
    /// A sign-in is under way.
    pub(crate) busy: bool,
    /// Why the last attempt failed, or why the user is signed out.
    pub(crate) message: Option<String>,
}

#[derive(Debug)]
pub(crate) struct SignedIn {
    pub(crate) user: User,
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
    make_api: Option<ApiFactory>,
    api: Option<Box<dyn Api>>,
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
            make_api: Some(make_api),
            api: None,
        }
    }

    /// The screen shown, as the `screen` fact names it.
    pub fn screen(&self) -> &'static str {
        match &self.session {
            Session::Checking => "checking",
            Session::SignedOut(form) if form.busy => "signing_in",
            Session::SignedOut(_) => "signed_out",
            Session::SignedIn(signed_in) if signed_in.signing_out => "signing_out",
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

    /// The open conversation's latest Bot reply, if it holds one.
    pub(crate) fn latest_reply(&self) -> Option<&Message> {
        let bot = self.open_bot()?;
        self.store
            .conversation(bot)?
            .messages()
            .rev()
            .find(|message| message.author == Author::Bot)
    }

    fn send(&self, request: ApiRequest) {
        if let Some(api) = &self.api {
            api.send(self.generation, request);
        }
    }

    fn next_generation(&mut self) {
        self.generation = Generation(self.generation.0 + 1);
        self.store = Store::new();
        self.in_flight.clear();
        self.again.clear();
    }

    fn signed_out(&mut self, message: Option<String>) {
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

    fn signed_in(&mut self, user: User) {
        self.next_generation();
        self.session = Session::SignedIn(SignedIn {
            user,
            bots_error: None,
            signing_out: false,
            message: None,
            link: Link::Stopped,
            open: None,
        });
        // The list shows at once. Once the socket is live it is loaded again,
        // so that no event can fall between the list and the socket.
        self.load(Snapshot::Bots);
        self.load_usage();
        if let Some(api) = &self.api {
            api.connect_events(self.generation);
        }
    }

    fn load_usage(&mut self) {
        self.store.request_usage();
        self.send(ApiRequest::LoadUsage);
    }

    /// Load a snapshot over HTTP. The store holds any event that arrives
    /// meanwhile. A snapshot already loading is loaded again once that load
    /// answers, because it may predate whatever asked for it now.
    fn load(&mut self, snapshot: Snapshot) {
        let _ = self.store.request(snapshot);
        if !self.in_flight.insert(snapshot) {
            self.again.insert(snapshot);
            return;
        }
        self.send(match snapshot {
            Snapshot::Bots => ApiRequest::ListBots,
            Snapshot::Messages(bot) => ApiRequest::LoadMessages(bot),
        });
    }

    fn load_all(&mut self, snapshots: impl IntoIterator<Item = Snapshot>) {
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
        // The meter may have missed events too.
        self.load_usage();
    }

    /// A load answered. Returns whether to load it again at once.
    fn answered(&mut self, snapshot: Snapshot) -> bool {
        self.in_flight.remove(&snapshot);
        self.again.remove(&snapshot)
    }

    /// A signed-in command answered 401: the session is gone.
    fn session_ended(&mut self) {
        self.signed_out(Some("Your session has ended. Sign in again".into()));
    }

    fn reply(&mut self, reply: ApiReply) -> bool {
        match (reply, &mut self.session) {
            (ApiReply::Session(Ok(session)), Session::Checking) => self.signed_in(session.user),
            (ApiReply::Session(Err(error)), Session::Checking) => {
                // 401 is the ordinary signed-out answer; anything else is
                // worth saying.
                let message = (!error.is_unauthenticated()).then_some(error.message);
                self.signed_out(message);
            }
            // The socket asked whether the session still holds.
            (ApiReply::Session(Err(error)), Session::SignedIn(_)) if error.is_unauthenticated() => {
                self.session_ended()
            }
            (ApiReply::SignedIn(result), Session::SignedOut(form)) if form.busy => match result {
                Ok(session) => self.signed_in(session.user),
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
                        }
                        let applied = self.store.load_messages(bot, page);
                        self.load_all(applied.reload);
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
                    // The draft stays, to send again.
                    Err(error) => open.send_error = Some(error.message),
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
            (ApiReply::Usage(result), Session::SignedIn(_)) => match result {
                Ok(usage) => return self.store.load_usage(usage),
                Err(error) if error.is_unauthenticated() => self.session_ended(),
                // The meter stays as it was; events or the next reload
                // bring it up to date.
                Err(_) => return false,
            },
            // A reply to a command whose screen has gone, such as a Bot list
            // that arrives after signing out.
            _ => return false,
        }
        true
    }

    fn socket(&mut self, update: SocketUpdate) -> bool {
        let Session::SignedIn(signed_in) = &mut self.session else {
            return false;
        };
        match update {
            SocketUpdate::Event(envelope) => {
                let applied = self.store.apply(&envelope);
                self.load_all(applied.reload);
                applied.changed
            }
            SocketUpdate::Reload => {
                self.reload_everything();
                true
            }
            SocketUpdate::CheckSession => {
                self.send(ApiRequest::CheckSession);
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
                    self.load_all(failed);
                }
                true
            }
        }
    }

    fn open_conversation(&mut self, bot: BotId) -> Result<bool, String> {
        let Session::SignedIn(signed_in) = &mut self.session else {
            return Ok(false);
        };
        if self.store.bot(bot).is_none() {
            return Err(format!("There is no Bot {bot}"));
        }
        if let Some(open) = signed_in.open.take() {
            self.store.close_conversation(open.bot);
        }
        signed_in.open = Some(Open::new(bot));
        self.load(Snapshot::Messages(bot));
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

    fn can_send(&self) -> Result<(), &'static str> {
        let Some(open) = self.open() else {
            return Err("No conversation is open");
        };
        if open.sending {
            return Err("Sending");
        }
        if open.draft.trim().is_empty() {
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

    fn availability(&self, target: ActionTarget<BokkieAction>) -> Availability {
        let signed_in = match &self.session {
            Session::SignedIn(signed_in) => Some(signed_in),
            _ => None,
        };
        let reason = match (target.action, &self.session) {
            (BokkieAction::SignIn, Session::SignedOut(form)) => {
                if form.busy {
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
            (BokkieAction::Refresh, _)
                if self.open().is_none() && self.in_flight.contains(&Snapshot::Bots) =>
            {
                Some("Loading the Bots")
            }
            (BokkieAction::Refresh, _) => None,
            (BokkieAction::OpenBot, _) if !self.store.bots_loaded() => Some("Loading the Bots"),
            (BokkieAction::OpenBot, _) => None,
            (BokkieAction::Back, _) if self.open().is_none() => Some("No conversation is open"),
            (BokkieAction::Back, _) => None,
            (BokkieAction::Send, _) => self.can_send().err(),
            (BokkieAction::Stop, _) => self.can_stop().err(),
        };
        reason.map_or(Availability::Enabled, |reason| Availability::Disabled {
            reason: reason.into(),
        })
    }

    fn started(&mut self, intents: IntentSender<BokkieIntent>) {
        if let Some(make_api) = self.make_api.take() {
            self.api = Some(make_api(intents));
        }
        self.send(ApiRequest::CheckSession);
    }

    fn ui(&self, ui: &mut egui::Ui, _: &mut (), frame: &mut Frame<'_, BokkieAction, BokkieIntent>) {
        ui::present(self, ui, frame);
    }

    fn apply(&mut self, intent: BokkieIntent, _: &mut ()) -> Result<bool, String> {
        Ok(match intent {
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
            BokkieIntent::Reply { generation, reply } => {
                generation == self.generation && self.reply(reply)
            }
            BokkieIntent::Socket { generation, update } => {
                generation == self.generation && self.socket(update)
            }
        })
    }

    fn perform(&mut self, target: ActionTarget<BokkieAction>, _: &mut ()) -> Result<bool, String> {
        match (target.action, &mut self.session) {
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
            // On the list, the list; in a conversation, that conversation.
            (BokkieAction::Refresh, Session::SignedIn(signed_in)) => match &signed_in.open {
                Some(open) => {
                    let bot = open.bot;
                    self.load(Snapshot::Messages(bot));
                }
                None => self.load(Snapshot::Bots),
            },
            (BokkieAction::Back, Session::SignedIn(signed_in)) => {
                let Some(open) = signed_in.open.take() else {
                    return Ok(false);
                };
                self.store.close_conversation(open.bot);
            }
            (BokkieAction::Send, Session::SignedIn(signed_in)) => {
                let Some(open) = &mut signed_in.open else {
                    return Ok(false);
                };
                open.sending = true;
                open.delivery = None;
                open.send_error = None;
                let bot = open.bot;
                let text = open.draft.trim().to_owned();
                self.send(ApiRequest::SendMessage(bot, SendMessageRequest { text }));
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
            (BokkieAction::OpenBot, ActionArgument::Number(id))
                if id.fract() == 0.0 && (1.0..=MAX_BOT_ID).contains(&id) =>
            {
                // Exact: the contract admits only integers in range.
                self.open_conversation(BotId(id as i64))
            }
            (BokkieAction::OpenBot, _) => Err("Open needs a Bot ID".into()),
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
        observation.fact(
            "user",
            self.user().map_or("", |user| user.username.as_str()),
        );
        observation.fact("bots", self.bots_state());
        observation.fact("bot_count", self.store.visible_bots().len());
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
    }
}

/// At most 200 characters of a text, for a fact.
fn bounded(text: &str) -> String {
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
