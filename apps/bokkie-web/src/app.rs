//! The application: sign-in, sign-out and the signed-in user's Bot list.
//!
//! State changes only in `apply` and `perform`. Commands go out through an
//! [`Api`], and their replies come back as [`BokkieIntent::Reply`], so tests
//! drive the same transitions with a [`Recorder`](crate::api::Recorder).

use bokkie_protocol::api::{Bot, BotStatus, LoginRequest, User};
use serde::Serialize;
use stobie_ui_egui::{
    Action, ActionEmphasis, ActionTarget, Application, ApplicationIdentity, Availability,
    ContentTextSpec, Frame, IntentSender, NativeTextControlKind, Observation, PresentationContext,
    PresentationScope, SemanticUiId, TextInteraction, TextOverflow, TextRole, UiNode, UiRole,
    application_identity, observe_visible_only,
};

use crate::api::{Api, ApiReply, ApiRequest};
use crate::store::{Snapshot, Store};

/// The application's name, as the page and the host report it.
pub const APPLICATION_NAME: &str = "Bokkie";

/// The username field's stable ID; `stobie set-text` addresses it.
pub const USERNAME_ID: &str = "bokkie.username";
/// The password field's stable ID.
pub const PASSWORD_ID: &str = "bokkie.password";

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
    #[action(label = "Refresh", description = "Load the list of Bots again")]
    Refresh,
}

/// Data changes from the UI and replies from the transport.
pub enum BokkieIntent {
    EditUsername(String),
    EditPassword(String),
    Reply(ApiReply),
}

// The password must never reach a log line.
impl std::fmt::Debug for BokkieIntent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EditUsername(username) => f.debug_tuple("EditUsername").field(username).finish(),
            Self::EditPassword(_) => f.write_str("EditPassword(<redacted>)"),
            Self::Reply(reply) => f.debug_tuple("Reply").field(reply).finish(),
        }
    }
}

/// Makes the transport once the host hands over its intent sender.
pub type ApiFactory = Box<dyn FnOnce(IntentSender<BokkieIntent>) -> Box<dyn Api>>;

/// Where the user is.
#[derive(Debug)]
enum Session {
    /// Asking the gateway whether the browser's cookie is still a session.
    Checking,
    SignedOut(SignInForm),
    SignedIn(SignedIn),
}

#[derive(Debug, Default)]
struct SignInForm {
    username: String,
    password: String,
    /// A sign-in is under way.
    busy: bool,
    /// Why the last attempt failed, or why the user is signed out.
    message: Option<String>,
}

#[derive(Debug)]
struct SignedIn {
    user: User,
    bots: BotsState,
    /// A sign-out is under way.
    signing_out: bool,
    /// The last command error, such as a failed sign-out.
    message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BotsState {
    Loading,
    Loaded,
    Failed(String),
}

/// The application state. The UI only reads it.
pub struct App {
    session: Session,
    store: Store,
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
            store: Store::new(),
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
            Session::SignedIn(signed_in) => match &signed_in.bots {
                BotsState::Failed(message) => Some(message),
                _ => signed_in.message.as_deref(),
            },
        }
    }

    /// The Bot list's state, as the `bots` fact names it.
    pub fn bots_state(&self) -> &'static str {
        match &self.session {
            Session::SignedIn(signed_in) => match signed_in.bots {
                BotsState::Loading => "loading",
                BotsState::Loaded => "loaded",
                BotsState::Failed(_) => "failed",
            },
            _ => "",
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    fn send(&self, request: ApiRequest) {
        if let Some(api) = &self.api {
            api.send(request);
        }
    }

    fn signed_out(&mut self, message: Option<String>) {
        let username = match &self.session {
            Session::SignedOut(form) => form.username.clone(),
            Session::SignedIn(signed_in) => signed_in.user.username.clone(),
            Session::Checking => String::new(),
        };
        self.store = Store::new();
        self.session = Session::SignedOut(SignInForm {
            username,
            message,
            ..SignInForm::default()
        });
    }

    fn signed_in(&mut self, user: User) {
        self.store = Store::new();
        self.session = Session::SignedIn(SignedIn {
            user,
            bots: BotsState::Loading,
            signing_out: false,
            message: None,
        });
        self.load_bots();
    }

    fn load_bots(&mut self) {
        if let Session::SignedIn(signed_in) = &mut self.session {
            signed_in.bots = BotsState::Loading;
            // The store holds any event that arrives meanwhile.
            let _ = self.store.request(Snapshot::Bots);
            self.send(ApiRequest::ListBots);
        }
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
            (ApiReply::Bots(result), Session::SignedIn(signed_in))
                if signed_in.bots == BotsState::Loading =>
            {
                match result {
                    Ok(list) => {
                        signed_in.bots = BotsState::Loaded;
                        let _ = self.store.load_bots(list);
                    }
                    Err(error) if error.is_unauthenticated() => self.session_ended(),
                    Err(error) => {
                        signed_in.bots = BotsState::Failed(error.message);
                        self.store.load_failed(Snapshot::Bots);
                    }
                }
            }
            // A reply to a command whose screen has gone, such as a Bot list
            // that arrives after signing out.
            _ => return false,
        }
        true
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
            (BokkieAction::SignOut, Session::SignedIn(signed_in)) if signed_in.signing_out => {
                Some("Signing out")
            }
            (BokkieAction::SignOut, Session::SignedIn(_)) => None,
            (BokkieAction::Refresh, Session::SignedIn(signed_in))
                if signed_in.bots == BotsState::Loading =>
            {
                Some("Loading the Bots")
            }
            (BokkieAction::Refresh, Session::SignedIn(_)) => None,
            (BokkieAction::SignOut | BokkieAction::Refresh, _) => Some("Not signed in"),
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
        egui::CentralPanel::default().show(ui, |ui| {
            let mut p =
                frame.presentation(ui, PresentationScope::new("bokkie"), SemanticUiId::root());
            match &self.session {
                Session::Checking => {
                    p.heading(ui, "title", APPLICATION_NAME);
                    p.status(ui, "status", "Checking your session…", false);
                }
                Session::SignedOut(form) => sign_in_ui(ui, &mut p, frame, form),
                Session::SignedIn(signed_in) => bots_ui(ui, &mut p, frame, signed_in, &self.store),
            }
        });
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
            BokkieIntent::Reply(reply) => self.reply(reply),
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
            (BokkieAction::Refresh, Session::SignedIn(_)) => self.load_bots(),
            _ => return Ok(false),
        }
        Ok(true)
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
        let message: String = self.message().unwrap_or("").chars().take(200).collect();
        observation.fact("message", message);
    }
}

fn line(role: TextRole, max_lines: u8) -> ContentTextSpec {
    ContentTextSpec {
        role,
        overflow: TextOverflow::Wrap,
        max_lines,
        interaction: TextInteraction::Selectable,
    }
}

/// A text field observed as a text input, so agents can fill it with
/// `stobie set-text`. Returns the edited copy and the field's response.
fn text_field(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    id: &'static str,
    label: &str,
    value: &str,
    password: bool,
) -> (String, egui::Response) {
    let mut edited = value.to_owned();
    let caption = ui.label(label);
    let (field, ()) = p.native(ui, NativeTextControlKind::TextEdit, |ui| {
        let edit = egui::TextEdit::singleline(&mut edited)
            .password(password)
            .desired_width(f32::INFINITY);
        // Name the field after its caption for assistive technology.
        (ui.add(edit).labelled_by(caption.id), ())
    });
    let mut node = UiNode::container(
        SemanticUiId::new(id),
        Some(SemanticUiId::root()),
        UiRole::TextInput,
        field.rect.into(),
    );
    node.name = label.into();
    node.focused = field.has_focus();
    p.observe_node(ui, node);
    (edited, field)
}

fn sign_in_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut Frame<'_, BokkieAction, BokkieIntent>,
    form: &SignInForm,
) {
    p.heading(ui, "title", APPLICATION_NAME);
    ui.add_enabled_ui(!form.busy, |ui| {
        let (username, _) = text_field(ui, p, USERNAME_ID, "Username", &form.username, false);
        if username != form.username {
            frame.emit(BokkieIntent::EditUsername(username));
        }
        let (password, field) = text_field(ui, p, PASSWORD_ID, "Password", &form.password, true);
        if password != form.password {
            frame.emit(BokkieIntent::EditPassword(password));
        }
        // Enter in the password field signs in. The host rechecks
        // availability, after this pass's edits apply.
        if field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            frame.trigger(BokkieAction::SignIn);
        }
    });
    frame.action(ui, p, BokkieAction::SignIn, ActionEmphasis::Primary);
    if form.busy {
        p.status(ui, "status", "Signing in…", false);
    } else if let Some(message) = &form.message {
        p.status(ui, "status", message, true);
    }
}

fn status_text(status: BotStatus) -> &'static str {
    match status {
        BotStatus::Idle => "Idle",
        BotStatus::Working => "Working",
        BotStatus::Refused => "Unavailable",
    }
}

fn bots_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut Frame<'_, BokkieAction, BokkieIntent>,
    signed_in: &SignedIn,
    store: &Store,
) {
    // Stobie's headings and content lines take the whole row, so controls
    // go on rows of their own.
    p.heading(ui, "title", APPLICATION_NAME);
    let who = format!("Signed in as {}", signed_in.user.display_name);
    p.content(ui, "user", &who, line(TextRole::Secondary, 1));
    frame.action(ui, p, BokkieAction::SignOut, ActionEmphasis::Normal);
    if signed_in.signing_out {
        p.status(ui, "status", "Signing out…", false);
    } else if let Some(message) = &signed_in.message {
        p.status(ui, "status", message, true);
    }
    ui.separator();
    p.heading(ui, "bots-title", "Bots");
    frame.action(ui, p, BokkieAction::Refresh, ActionEmphasis::Normal);
    match &signed_in.bots {
        BotsState::Loading if !store.bots_loaded() => {
            p.status(ui, "bots-status", "Loading Bots…", false);
            return;
        }
        BotsState::Failed(message) => {
            p.status(ui, "bots-status", message, true);
        }
        _ => {}
    }
    let bots = store.visible_bots();
    if bots.is_empty() && store.bots_loaded() {
        p.content(ui, "no-bots", "No Bots yet", line(TextRole::Secondary, 1));
        return;
    }
    // The list scrolls, so publish only what is visible.
    observe_visible_only(ui, |ui| {
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                let mut rows =
                    frame.presentation(ui, PresentationScope::new("bots"), SemanticUiId::root());
                for bot in bots {
                    bot_row(ui, &mut rows, bot);
                }
            });
    });
}

fn bot_row(ui: &mut egui::Ui, p: &mut PresentationContext, bot: &Bot) {
    let key = bot.id.0;
    p.content(ui, ("name", key), &bot.name, line(TextRole::Body, 1));
    let details = format!("{} · {}", bot.label, status_text(bot.status));
    p.content(ui, ("details", key), &details, line(TextRole::Secondary, 1));
    if let Some(preview) = &bot.last_message {
        p.content(
            ui,
            ("preview", key),
            &preview.text,
            line(TextRole::Caption, 2),
        );
    }
    ui.separator();
}
