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
    Author, BotStatus, CreateBotRequest, Delivery, LoginRequest, Message, MessageState,
    SendMessageRequest, User,
};
use bokkie_protocol::{BotId, MessageId, Timestamp};
use serde::Serialize;
use stobie_ui_egui::{
    Action, ActionArgument, ActionKey, ActionTarget, Application, ApplicationIdentity,
    Availability, Frame, IntentSender, Observation, PassContext, application_identity,
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
/// The New Bot name field's stable ID.
pub const BOT_NAME_ID: &str = "bokkie.bot-name";

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
        label = "Load older",
        description = "Load the conversation's earlier messages, a page at a time"
    )]
    LoadOlder,
    #[action(
        label = "Send",
        description = "Send the message to the Bot; while it replies, the message steers the reply"
    )]
    Send,
    #[action(
        label = "Create Bot",
        description = "Create a Bot with the name entered"
    )]
    CreateBot,
    #[action(
        label = "Sign in to ChatGPT",
        description = "Open the ChatGPT sign-in for your own Bots",
        id = "bokkie.chatgpt-sign-in"
    )]
    ChatGptSignIn,
    #[action(
        label = "Start",
        description = "Start a ChatGPT sign-in with a device code, or get a new code",
        id = "bokkie.start-chatgpt"
    )]
    StartChatGpt,
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
    EditBotName(String),
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
            Self::EditBotName(name) => f.debug_tuple("EditBotName").field(name).finish(),
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
    SignedIn(Box<SignedIn>),
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
    pub(crate) new_bot: NewBot,
    /// Whether the user's own app server is signed in to ChatGPT.
    pub(crate) account: Account,
    /// The ChatGPT sign-in screen, when shown.
    pub(crate) login: Option<Login>,
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

/// The user's ChatGPT account, as `GET /codex/account` last said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Account {
    Checking,
    SignedIn { plan: Option<String> },
    SignedOut,
    Failed(String),
}

/// The ChatGPT device-code sign-in screen.
#[derive(Debug)]
pub(crate) enum Login {
    /// Explained, not started.
    Ready,
    /// `POST /codex/login` is under way.
    Starting,
    /// The code is on screen and the account is polled until signed in.
    /// The code lives here only, and goes when the screen does.
    Waiting {
        code: crate::api::DeviceCode,
        next_poll: Timestamp,
        polling: bool,
        /// Why the last poll failed, while polling goes on.
        error: Option<String>,
    },
    Done,
    Failed(String),
}

/// How often the account is checked while a device code is on screen.
pub const LOGIN_POLL: std::time::Duration = std::time::Duration::from_secs(5);

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
    /// Pages of older messages added since the application started.
    older_pages: u64,
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
            usage_in_flight: false,
            usage_again: false,
            older_pages: 0,
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
            Session::SignedIn(signed_in) if signed_in.login.is_some() => "chatgpt",
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

    pub(crate) fn login(&self) -> Option<&Login> {
        match &self.session {
            Session::SignedIn(signed_in) => signed_in.login.as_ref(),
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
        self.usage_in_flight = false;
        self.usage_again = false;
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
        self.session = Session::SignedIn(Box::new(SignedIn {
            user,
            bots_error: None,
            signing_out: false,
            message: None,
            link: Link::Stopped,
            open: None,
            new_bot: NewBot::default(),
            account: Account::Checking,
            login: None,
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
    fn load(&mut self, snapshot: Snapshot) {
        let request = match snapshot {
            Snapshot::Bots => ApiRequest::ListBots,
            Snapshot::Messages(bot) => ApiRequest::LoadMessages(bot),
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
            (ApiReply::Account(result), Session::SignedIn(_)) => {
                let now = self.now();
                let Session::SignedIn(signed_in) = &mut self.session else {
                    return false;
                };
                let account = match result {
                    Ok(account) if account.signed_in => Account::SignedIn { plan: account.plan },
                    Ok(_) => Account::SignedOut,
                    Err(error) if error.is_unauthenticated() => {
                        self.session_ended();
                        return true;
                    }
                    Err(error) => Account::Failed(error.message),
                };
                if let Some(Login::Waiting {
                    next_poll,
                    polling,
                    error,
                    ..
                }) = &mut signed_in.login
                {
                    *polling = false;
                    *next_poll = Timestamp(now.0 + LOGIN_POLL.as_millis() as i64);
                    *error = match &account {
                        Account::Failed(message) => Some(message.clone()),
                        _ => None,
                    };
                }
                if matches!(account, Account::SignedIn { .. }) && signed_in.login.is_some() {
                    // The code has served its purpose: it goes.
                    signed_in.login = Some(Login::Done);
                }
                // A failed poll does not unsay what the screen knows.
                if !(matches!(account, Account::Failed(_))
                    && matches!(signed_in.login, Some(Login::Waiting { .. })))
                {
                    signed_in.account = account;
                }
            }
            (ApiReply::Login(result), Session::SignedIn(signed_in))
                if matches!(signed_in.login, Some(Login::Starting)) =>
            {
                let now = self.now();
                let Session::SignedIn(signed_in) = &mut self.session else {
                    return false;
                };
                signed_in.login = Some(match result {
                    Ok(code) => Login::Waiting {
                        code,
                        next_poll: Timestamp(now.0 + LOGIN_POLL.as_millis() as i64),
                        polling: false,
                        error: None,
                    },
                    Err(error) if error.is_unauthenticated() => {
                        self.session_ended();
                        return true;
                    }
                    Err(error) => Login::Failed(error.message),
                });
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
            (BokkieAction::OpenBot, _) if self.login().is_some() => {
                Some("Leave the ChatGPT sign-in first")
            }
            (BokkieAction::OpenBot, _) if !self.store.bots_loaded() => Some("Loading the Bots"),
            (BokkieAction::OpenBot, _) => None,
            (BokkieAction::Back, _) if self.open().is_none() && self.login().is_none() => {
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
                if signed_in.new_bot.busy {
                    Some("Creating the Bot")
                } else if signed_in.new_bot.name.trim().is_empty() {
                    Some("Enter a name")
                } else {
                    None
                }
            }),
            (BokkieAction::ChatGptSignIn, _) => signed_in.and_then(|signed_in| {
                if signed_in.open.is_some() || signed_in.login.is_some() {
                    Some("Return to the list of Bots first")
                } else if matches!(signed_in.account, Account::SignedIn { .. }) {
                    Some("Already signed in to ChatGPT")
                } else {
                    None
                }
            }),
            (BokkieAction::StartChatGpt, _) => {
                signed_in.and_then(|signed_in| match (&signed_in.login, &signed_in.account) {
                    (None, _) => Some("Open the ChatGPT sign-in first"),
                    (_, Account::SignedIn { .. }) => Some("Already signed in to ChatGPT"),
                    (Some(Login::Ready | Login::Failed(_)), Account::Checking) => {
                        Some("Checking your ChatGPT sign-in")
                    }
                    // While a code is shown, starting again replaces it, as
                    // when it has expired.
                    (Some(Login::Ready | Login::Failed(_) | Login::Waiting { .. }), _) => None,
                    (Some(Login::Starting), _) => Some("Starting"),
                    (Some(Login::Done), _) => Some("Already signed in to ChatGPT"),
                })
            }
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

    /// Poll the ChatGPT account while a device code is on screen.
    fn prepare(&mut self, pass: &mut PassContext<'_>, _: &mut ()) {
        let now = self.now();
        let Session::SignedIn(signed_in) = &mut self.session else {
            return;
        };
        let Some(Login::Waiting {
            next_poll, polling, ..
        }) = &mut signed_in.login
        else {
            return;
        };
        if *polling {
            return;
        }
        if now >= *next_poll {
            *polling = true;
            self.send(ApiRequest::CodexAccount);
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
                // Leaving the ChatGPT sign-in drops its code with it.
                if signed_in.login.take().is_some() {
                    return Ok(true);
                }
                let Some(open) = signed_in.open.take() else {
                    return Ok(false);
                };
                self.store.close_conversation(open.bot);
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
            (BokkieAction::ChatGptSignIn, Session::SignedIn(signed_in)) => {
                signed_in.login = Some(Login::Ready);
                // Ask again: it may have changed since sign-in.
                signed_in.account = Account::Checking;
                self.send(ApiRequest::CodexAccount);
            }
            (BokkieAction::StartChatGpt, Session::SignedIn(signed_in)) => {
                signed_in.login = Some(Login::Starting);
                self.send(ApiRequest::CodexLogin);
            }
            (BokkieAction::Send, Session::SignedIn(signed_in)) => {
                let Some(open) = &mut signed_in.open else {
                    return Ok(false);
                };
                open.sending = true;
                open.delivery = None;
                open.send_error = None;
                open.stop_note = None;
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
            "chatgpt",
            match signed_in.map(|s| &s.account) {
                None => "",
                Some(Account::Checking) => "checking",
                Some(Account::SignedIn { .. }) => "signed_in",
                Some(Account::SignedOut) => "signed_out",
                Some(Account::Failed(_)) => "failed",
            },
        );
        // Never the code itself.
        observation.fact(
            "chatgpt_login",
            match signed_in.and_then(|s| s.login.as_ref()) {
                None => "",
                Some(Login::Ready) => "ready",
                Some(Login::Starting) => "starting",
                Some(Login::Waiting { .. }) => "waiting",
                Some(Login::Done) => "done",
                Some(Login::Failed(_)) => "failed",
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
