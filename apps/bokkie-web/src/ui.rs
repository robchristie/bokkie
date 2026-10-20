//! The application's presentation: plain stand-ins (settled decision 3), one
//! screen at a time, each control on a row of its own.

use std::collections::BTreeSet;

use bokkie_protocol::MessageId;
use bokkie_protocol::api::{
    Author, Bot, BotStatus, Delivery, LoginState, Message, MessageKind, MessageState,
    NotificationKind,
};
use stobie_ui_egui::local_time::LocalTime;
use stobie_ui_egui::{
    ActionEmphasis, BoundAction, ContentTextSpec, Frame, LinkSpec, MarkdownSpec,
    NativeTextControlKind, PresentationContext, PresentationScope, ReadingTextSpec, SemanticUiId,
    TextInteraction, TextOverflow, TextRole, UiNode, UiRole, observe_visible_only,
};

use crate::app::{
    APPLICATION_NAME, Account, App, BOT_NAME_ID, BokkieAction, BokkieIntent, DRAFT_ID, Household,
    Login, NewBot, Open, PASSWORD_ID, PROFILE_DESCRIPTION_ID, PROFILE_LABEL_ID, PROFILE_NAME_ID,
    ProfileEditor, Session, SignInForm, SignOut, SignedIn, USERNAME_ID,
};
use crate::app::{DISPLAY_NAME_ID, INVITATION_CODE_ID};
use crate::socket::Link;

pub(crate) type BokkieFrame<'a, 'b> = Frame<'a, BokkieAction, BokkieIntent>;

/// Room kept below a conversation's messages for the message field, its
/// buttons and a status line.
const COMPOSER_HEIGHT: f32 = 230.0;

/// How often a meter with reset times is shown again, so they stay true.
pub(crate) const METER_REFRESH: std::time::Duration = std::time::Duration::from_secs(60);

/// What assistive technology says of every link in a Bot's reply.
const OPENS_A_NEW_TAB: &str = "Opens in a new tab";

pub(crate) fn present(app: &App, ui: &mut egui::Ui, frame: &mut BokkieFrame<'_, '_>) {
    egui::CentralPanel::default().show(ui, |ui| {
        let mut p = frame.presentation(ui, PresentationScope::new("bokkie"), SemanticUiId::root());
        match &app.session {
            Session::Checking => {
                p.heading(ui, "title", APPLICATION_NAME);
                p.status(ui, "status", "Checking your session…", false);
            }
            Session::SignedOut(form) => sign_in_ui(ui, &mut p, frame, form),
            Session::SignedIn(signed_in) if signed_in.admin.is_some() => {
                crate::admin::present(ui, &mut p, frame, app, signed_in);
            }
            Session::SignedIn(signed_in) => match (&signed_in.household, &signed_in.open) {
                (Some(household), _) => household_ui(ui, &mut p, frame, app, signed_in, household),
                (None, Some(open)) => conversation_ui(ui, &mut p, frame, app, signed_in, open),
                (None, None) => bots_ui(ui, &mut p, frame, app, signed_in),
            },
        }
    });
}

pub(crate) fn line(role: TextRole, max_lines: u8) -> ContentTextSpec {
    ContentTextSpec {
        role,
        overflow: TextOverflow::Wrap,
        max_lines,
        interaction: TextInteraction::Selectable,
    }
}

/// A text field observed as a text input, so agents can fill it with
/// `stobie set-text`. Returns the edited copy and the field's response.
pub(crate) fn text_field(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    id: &'static str,
    label: &str,
    value: &str,
    password: bool,
) -> (String, egui::Response) {
    field(ui, p, id, label, value, password, false)
}

/// A text field of several lines, such as a Bot's description.
pub(crate) fn text_area(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    id: &'static str,
    label: &str,
    value: &str,
) -> (String, egui::Response) {
    field(ui, p, id, label, value, false, true)
}

fn field(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    id: &'static str,
    label: &str,
    value: &str,
    password: bool,
    multiline: bool,
) -> (String, egui::Response) {
    let mut edited = value.to_owned();
    let caption = ui.label(label);
    let (field, ()) = p.native(ui, NativeTextControlKind::TextEdit, |ui| {
        let edit = if multiline {
            egui::TextEdit::multiline(&mut edited).desired_rows(4)
        } else {
            egui::TextEdit::singleline(&mut edited)
        }
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

/// Enter pressed in a field that just lost focus to it.
pub(crate) fn submitted(ui: &egui::Ui, field: &egui::Response) -> bool {
    field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))
}

fn sign_in_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    form: &SignInForm,
) {
    p.heading(ui, "title", APPLICATION_NAME);
    let submit = if form.joining {
        BokkieAction::AcceptInvitation
    } else {
        BokkieAction::SignIn
    };
    ui.add_enabled_ui(!form.busy, |ui| {
        if form.joining {
            p.content(
                ui,
                "join-about",
                "Enter the invitation code an admin gave you, and choose your username, name and password.",
                line(TextRole::Secondary, 3),
            );
            let (code, _) = text_field(
                ui,
                p,
                INVITATION_CODE_ID,
                "Invitation code",
                &form.code,
                false,
            );
            if code != form.code {
                frame.emit(BokkieIntent::EditInvitationCode(code));
            }
        }
        let (username, _) = text_field(ui, p, USERNAME_ID, "Username", &form.username, false);
        if username != form.username {
            frame.emit(BokkieIntent::EditUsername(username));
        }
        if form.joining {
            let (name, _) = text_field(
                ui,
                p,
                DISPLAY_NAME_ID,
                "Your name",
                &form.display_name,
                false,
            );
            if name != form.display_name {
                frame.emit(BokkieIntent::EditDisplayName(name));
            }
        }
        let (password, field) = text_field(ui, p, PASSWORD_ID, "Password", &form.password, true);
        if password != form.password {
            frame.emit(BokkieIntent::EditPassword(password));
        }
        // Enter in the password field signs in, or joins. The host
        // rechecks availability, after this pass's edits apply.
        if submitted(ui, &field) {
            frame.trigger(submit);
        }
    });
    frame.action(ui, p, submit, ActionEmphasis::Primary);
    frame.toggle(ui, p, "join", BokkieAction::Join, form.joining);
    if form.busy {
        let text = if form.joining {
            "Joining…"
        } else {
            "Signing in…"
        };
        p.status(ui, "status", text, false);
    } else if let Some(message) = &form.message {
        p.status(ui, "status", message, true);
    }
}

/// A Bot's label and status, or its status alone for a Bot with no label,
/// and whether it is pinned or hidden.
fn details(bot: &Bot) -> String {
    let mut details = match bot.label.as_str() {
        "" => status_text(bot.status).to_owned(),
        label => format!("{label} · {}", status_text(bot.status)),
    };
    if bot.pinned {
        details.push_str(" · Pinned");
    }
    if bot.hidden {
        details.push_str(" · Hidden");
    }
    details
}

pub(crate) fn status_text(status: BotStatus) -> &'static str {
    match status {
        BotStatus::Idle => "Idle",
        BotStatus::Working => "Working",
        BotStatus::Refused => "Unavailable",
    }
}

/// The event socket's state, when it is worth saying.
fn link_status(ui: &mut egui::Ui, p: &mut PresentationContext, link: Link) {
    let text = match link {
        Link::Live | Link::Stopped | Link::SignedOut => return,
        Link::Connecting => "Connecting…",
        Link::Reconnecting => "Reconnecting…",
        Link::Outdated => "Bokkie has been updated. Reload the page to continue",
    };
    p.status(ui, "link", text, link == Link::Outdated);
}

fn bots_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    app: &App,
    signed_in: &SignedIn,
) {
    let store = app.store();
    // Stobie's headings and content lines take the whole row, so controls
    // go on rows of their own.
    p.heading(ui, "title", APPLICATION_NAME);
    let who = format!("Signed in as {}", signed_in.user.display_name);
    p.content(ui, "user", &who, line(TextRole::Secondary, 1));
    frame.action(ui, p, BokkieAction::SignOut, ActionEmphasis::Normal);
    if signed_in.is_admin() {
        frame.action(ui, p, BokkieAction::Admin, ActionEmphasis::Normal);
    }
    if signed_in.signing_out {
        p.status(ui, "status", "Signing out…", false);
    } else if let Some(message) = &signed_in.message {
        p.status(ui, "status", message, true);
    }
    notices_ui(ui, p, frame, app, signed_in);
    link_status(ui, p, signed_in.link);
    if signed_in.link == Link::Outdated {
        frame.action(ui, p, BokkieAction::ReloadPage, ActionEmphasis::Primary);
    }
    limit_ui(ui, p, frame, app);
    account_ui(ui, p, frame, signed_in);
    meter_ui(ui, p, frame, app);
    ui.separator();
    p.heading(ui, "bots-title", "Bots");
    frame.action(ui, p, BokkieAction::Refresh, ActionEmphasis::Normal);
    frame.toggle(
        ui,
        p,
        "show-hidden",
        BokkieAction::ShowHidden,
        signed_in.show_hidden,
    );
    if let Some(message) = &signed_in.bots_error {
        p.status(ui, "bots-status", message, true);
    } else if !store.bots_loaded() {
        p.status(ui, "bots-status", "Loading Bots…", false);
        return;
    }
    let bots = if signed_in.show_hidden {
        store.listed_bots()
    } else {
        store.visible_bots()
    };
    // The list scrolls, so publish only what is visible.
    observe_visible_only(ui, |ui| {
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                let mut rows =
                    frame.presentation(ui, PresentationScope::new("bots"), SemanticUiId::root());
                if bots.is_empty() && store.bots_loaded() {
                    rows.content(ui, "no-bots", "No Bots yet", line(TextRole::Secondary, 1));
                }
                for bot in bots {
                    bot_row(ui, &mut rows, frame, bot);
                }
                if signed_in.may_create_bots() {
                    new_bot_ui(ui, &mut rows, frame, &signed_in.new_bot);
                } else {
                    rows.content(
                        ui,
                        "no-new-bot",
                        "An admin makes your Bots",
                        line(TextRole::Secondary, 1),
                    );
                }
            });
    });
}

/// The notices a signed-in user sees on every screen: that an admin can
/// read their Bots and conversations, the whole time oversight is on, and
/// that an admin reset their password, until they acknowledge it.
fn notices_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    app: &App,
    signed_in: &SignedIn,
) {
    let local = app.local_time();
    let mut timed = false;
    if let Some(since) = signed_in.overseen() {
        let text = match since {
            Some(at) => {
                timed = true;
                format!(
                    "Oversight is on: an admin can read all your Bots and conversations. It has been on since {}",
                    crate::times::day_at(&local, at)
                )
            }
            None => "Oversight is on: an admin can read all your Bots and conversations".into(),
        };
        p.status(ui, "oversight", &text, true);
    }
    if let Some(at) = signed_in.reset_notice {
        timed = true;
        let text = format!(
            "An admin reset your password {}",
            crate::times::on_day_at(&local, at)
        );
        p.status(ui, "reset-notice", &text, true);
        frame.action(
            ui,
            p,
            BokkieAction::AcknowledgeReset,
            ActionEmphasis::Normal,
        );
        if signed_in.acknowledging {
            p.status(ui, "reset-status", "Acknowledging…", false);
        }
    }
    if timed {
        // "today at 9:41 AM" becomes "yesterday at 9:41 AM" at midnight.
        frame.request_repaint_after("notice days", crate::times::until_the_day_changes(&local));
    }
}

/// The New Bot form, after the list: a name only (settled decision 8).
fn new_bot_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    form: &NewBot,
) {
    p.heading(ui, "new-bot-title", "New Bot");
    ui.add_enabled_ui(!form.busy, |ui| {
        let (name, field) = text_field(ui, p, BOT_NAME_ID, "Name", &form.name, false);
        if name != form.name {
            frame.emit(BokkieIntent::EditBotName(name));
        }
        if submitted(ui, &field) {
            frame.trigger(BokkieAction::CreateBot);
        }
    });
    frame.action(ui, p, BokkieAction::CreateBot, ActionEmphasis::Normal);
    if form.busy {
        p.status(ui, "new-bot-status", "Creating…", false);
    } else if let Some(error) = &form.error {
        p.status(ui, "new-bot-status", error, true);
    }
}

/// Whether the household is signed in to ChatGPT. A member sees only
/// that; an admin also sees as whom, and the way to the sign-in.
fn account_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    signed_in: &SignedIn,
) {
    let admin = signed_in.user.role == bokkie_protocol::api::Role::Admin;
    match &signed_in.account {
        Account::Checking => {
            p.content(
                ui,
                "chatgpt",
                "ChatGPT: checking…",
                line(TextRole::Secondary, 1),
            );
        }
        Account::SignedIn { email, plan } => {
            let text = if admin {
                account_text(email.as_deref(), plan.as_deref())
            } else {
                "ChatGPT: signed in for the household".into()
            };
            p.content(ui, "chatgpt", &text, line(TextRole::Secondary, 2));
        }
        Account::SignedOut if admin => {
            p.status(
                ui,
                "chatgpt",
                "ChatGPT is signed out for the household, so no one's Bots can reply. Sign it in",
                true,
            );
        }
        Account::SignedOut => {
            p.status(
                ui,
                "chatgpt",
                "ChatGPT is signed out for the household, so your Bots cannot reply. Ask an admin to sign it in",
                true,
            );
        }
        Account::Failed(message) => {
            p.status(ui, "chatgpt", message, true);
        }
    }
    if admin {
        frame.action(ui, p, BokkieAction::Household, ActionEmphasis::Normal);
    }
}

/// "ChatGPT: signed in as ada@example.com (plus)", as an admin sees it.
fn account_text(email: Option<&str>, plan: Option<&str>) -> String {
    match (email, plan) {
        (Some(email), Some(plan)) => format!("ChatGPT: signed in as {email} ({plan})"),
        (Some(email), None) => format!("ChatGPT: signed in as {email}"),
        (None, Some(plan)) => format!("ChatGPT: signed in ({plan})"),
        (None, None) => "ChatGPT: signed in".into(),
    }
}

/// The household's ChatGPT sign-in, for admins: sign in by device code,
/// with the code's state and expiry, cancel, and sign out with its warning.
/// The code is shown here and kept nowhere else.
fn household_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    app: &App,
    signed_in: &SignedIn,
    household: &Household,
) {
    frame.action(ui, p, BokkieAction::Back, ActionEmphasis::Normal);
    p.heading(ui, "title", "ChatGPT for the household");
    p.content(
        ui,
        "about",
        "Every account's Bots reply through this one ChatGPT sign-in. Only admins sign it in or out.",
        line(TextRole::Secondary, 4),
    );
    notices_ui(ui, p, frame, app, signed_in);
    match &signed_in.account {
        Account::Checking => p.status(ui, "account", "Checking the sign-in…", false),
        Account::SignedIn { email, plan } => p.status(
            ui,
            "account",
            &account_text(email.as_deref(), plan.as_deref()),
            false,
        ),
        Account::SignedOut => p.status(
            ui,
            "account",
            "Signed out: no one's Bots can reply until an admin signs in",
            true,
        ),
        Account::Failed(message) => p.status(ui, "account", message, true),
    };
    match &household.login {
        Login::Idle => {}
        Login::Starting => {
            p.status(ui, "status", "Asking for a code…", false);
        }
        Login::Waiting { code, .. } | Login::Cancelling { code } => {
            let (cancelling, error) = match &household.login {
                Login::Waiting { error, .. } => (false, error.as_ref()),
                _ => (true, None),
            };
            p.content(
                ui,
                "step-1",
                "1. Open this page, on any device:",
                line(TextRole::Body, 1),
            );
            // In a new tab, as links open: this page holds the code and must
            // stay open.
            let url = &code.0.verification_url;
            p.link(
                ui,
                "link",
                LinkSpec {
                    description: Some(OPENS_A_NEW_TAB),
                    ..LinkSpec::new(url, url)
                },
            );
            p.content(ui, "step-2", "2. Enter this code:", line(TextRole::Body, 1));
            p.heading(ui, "code", &code.0.user_code);
            let now = app.now();
            let expiry = if now >= code.0.expires_at {
                "The code is about to expire".to_owned()
            } else {
                format!(
                    "The code expires {}",
                    crate::usage::resets_in(now, code.0.expires_at)
                )
            };
            p.content(ui, "expiry", &expiry, line(TextRole::Secondary, 1));
            if cancelling {
                p.status(ui, "status", "Cancelling…", false);
            } else {
                p.status(ui, "status", "Waiting for you to finish signing in…", false);
            }
            if let Some(error) = error {
                p.status(ui, "poll-error", error, true);
            }
            p.content(
                ui,
                "keep-open",
                "Keep this page open: the code is shown only here.",
                line(TextRole::Secondary, 2),
            );
            frame.action(ui, p, BokkieAction::CancelChatGpt, ActionEmphasis::Normal);
            frame.request_repaint_after("code expiry", METER_REFRESH);
        }
        Login::Ended { state, error } => {
            let (text, problem) = match state {
                LoginState::Completed => (
                    "Signed in to ChatGPT. Every account's Bots can reply".to_owned(),
                    false,
                ),
                LoginState::Expired => (
                    "The code expired before the sign-in finished. Get a new code to try again"
                        .to_owned(),
                    true,
                ),
                LoginState::Cancelled => ("The sign-in was cancelled".to_owned(), false),
                LoginState::Failed | LoginState::Pending => (
                    match error {
                        Some(error) => format!("The sign-in failed: {error}"),
                        None => "The sign-in failed".to_owned(),
                    },
                    true,
                ),
            };
            p.status(ui, "status", &text, problem);
        }
        Login::Failed(message) => {
            p.status(ui, "status", message, true);
        }
    }
    let waiting = matches!(
        household.login,
        Login::Starting | Login::Waiting { .. } | Login::Cancelling { .. }
    );
    let signed_out = matches!(signed_in.account, Account::SignedOut | Account::Failed(_));
    if waiting || signed_out {
        let emphasis = if waiting {
            ActionEmphasis::Normal
        } else {
            ActionEmphasis::Primary
        };
        frame.action(ui, p, BokkieAction::StartChatGpt, emphasis);
    }
    if matches!(signed_in.account, Account::SignedIn { .. }) && !waiting {
        sign_out_ui(ui, p, frame, &household.sign_out);
    }
}

/// Signing the household out, which signs out every account's Bots, so it
/// asks first.
fn sign_out_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    sign_out: &SignOut,
) {
    ui.separator();
    match sign_out {
        SignOut::Idle | SignOut::Failed(_) => {
            p.content(
                ui,
                "sign-out-about",
                "Signing out signs out every account's Bots at once, until an admin signs in again.",
                line(TextRole::Secondary, 3),
            );
            frame.action(ui, p, BokkieAction::SignOutChatGpt, ActionEmphasis::Normal);
            if let SignOut::Failed(message) = sign_out {
                p.status(ui, "sign-out-status", message, true);
            }
        }
        SignOut::Confirming => {
            p.status(
                ui,
                "sign-out-warning",
                "Sign out everyone? No account's Bots can reply until an admin signs in again.",
                true,
            );
            frame.action(
                ui,
                p,
                BokkieAction::ConfirmSignOutChatGpt,
                ActionEmphasis::Primary,
            );
            frame.action(ui, p, BokkieAction::KeepChatGpt, ActionEmphasis::Normal);
        }
        SignOut::Busy => {
            p.status(ui, "sign-out-status", "Signing out…", false);
        }
    }
}

fn bot_row(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    bot: &Bot,
) {
    let key = bot.id.0;
    p.content(ui, ("name", key), &bot.name, line(TextRole::Body, 1));
    p.content(
        ui,
        ("details", key),
        &details(bot),
        line(TextRole::Secondary, 1),
    );
    if let Some(preview) = &bot.last_message {
        p.content(
            ui,
            ("preview", key),
            &preview.text,
            line(TextRole::Caption, 2),
        );
    }
    // "Open Tutor" to assistive technology and agents.
    frame.action(
        ui,
        p,
        BoundAction::new(BokkieAction::OpenBot, key).with_subject(&bot.name),
        ActionEmphasis::Normal,
    );
    ui.separator();
}

fn conversation_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    app: &App,
    signed_in: &SignedIn,
    open: &Open,
) {
    let store = app.store();
    frame.action(ui, p, BokkieAction::Back, ActionEmphasis::Normal);
    let bot = store.bot(open.bot);
    let name = bot.map_or("Bot", |bot| bot.name.as_str());
    p.heading(ui, "title", name);
    if let Some(bot) = bot {
        p.content(ui, "details", &details(bot), line(TextRole::Secondary, 1));
    }
    notices_ui(ui, p, frame, app, signed_in);
    if let Some(bot) = bot {
        bot_controls_ui(ui, p, frame, signed_in, open, bot);
    }
    link_status(ui, p, signed_in.link);
    if signed_in.link == Link::Outdated {
        frame.action(ui, p, BokkieAction::ReloadPage, ActionEmphasis::Primary);
    }
    limit_ui(ui, p, frame, app);
    // A conversation that failed to load can be loaded again here.
    if open.error.is_some() && store.conversation(open.bot).is_none() {
        frame.action(ui, p, BokkieAction::Refresh, ActionEmphasis::Normal);
    }
    if let Some(error) = store.error(open.bot) {
        p.status(ui, "bot-error", error, true);
    }
    ui.separator();
    messages_ui(ui, frame, app, open, name);
    ui.separator();
    composer_ui(ui, p, frame, app, open);
}

/// Pin and hide, which every user can, and the profile, which needs the
/// permission: its control is not shown without it.
fn bot_controls_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    signed_in: &SignedIn,
    open: &Open,
    bot: &Bot,
) {
    if let Some(editor) = &open.editor {
        profile_ui(ui, p, frame, editor);
        return;
    }
    // Short controls share a row, to leave the conversation its room.
    ui.horizontal(|ui| {
        frame.toggle(ui, p, "pin", BokkieAction::Pin, bot.pinned);
        frame.toggle(ui, p, "hide", BokkieAction::Hide, bot.hidden);
        if signed_in.may_edit_profiles() {
            frame.action(ui, p, BokkieAction::EditProfile, ActionEmphasis::Normal);
        }
    });
    if open.updating {
        p.status(ui, "update-status", "Saving…", false);
    } else if let Some(error) = &open.update_error {
        p.status(ui, "update-status", error, true);
    }
}

/// The Bot's profile, being edited: its name, label and description.
fn profile_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    editor: &ProfileEditor,
) {
    ui.add_enabled_ui(!editor.busy, |ui| {
        let (name, _) = text_field(ui, p, PROFILE_NAME_ID, "Bot name", &editor.name, false);
        if name != editor.name {
            frame.emit(BokkieIntent::EditProfileName(name));
        }
        let (label, _) = text_field(ui, p, PROFILE_LABEL_ID, "Label", &editor.label, false);
        if label != editor.label {
            frame.emit(BokkieIntent::EditProfileLabel(label));
        }
        let (description, _) = text_area(
            ui,
            p,
            PROFILE_DESCRIPTION_ID,
            "Description",
            &editor.description,
        );
        if description != editor.description {
            frame.emit(BokkieIntent::EditProfileDescription(description));
        }
    });
    frame.action(ui, p, BokkieAction::SaveProfile, ActionEmphasis::Primary);
    frame.action(ui, p, BokkieAction::CancelProfile, ActionEmphasis::Normal);
    if editor.busy {
        p.status(ui, "profile-status", "Saving…", false);
    } else if let Some(error) = &editor.error {
        p.status(ui, "profile-status", error, true);
    }
}

/// The limit-reached banner: a plain error line.
fn limit_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    app: &App,
) {
    if let Some(banner) = app.limit_banner() {
        p.status(ui, "limit", &banner, true);
        refresh_reset_times(frame, app);
    }
}

/// While reset times are on screen, show them again each minute so they
/// stay true.
fn refresh_reset_times(frame: &mut BokkieFrame<'_, '_>, app: &App) {
    let resets = app.store().usage().is_some_and(|usage| {
        usage
            .subscription
            .iter()
            .flat_map(|bucket| [&bucket.primary, &bucket.secondary])
            .flatten()
            .any(|window| window.resets_at.is_some())
    });
    if resets {
        frame.request_repaint_after("usage reset times", METER_REFRESH);
    }
}

/// The usage meter: the subscription's windows and the user's allowance.
fn meter_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    app: &App,
) {
    let Some(usage) = app.store().usage() else {
        return;
    };
    ui.separator();
    p.heading(ui, "usage-title", "Usage");
    for (index, text) in crate::usage::meter_lines(usage, app.now())
        .iter()
        .enumerate()
    {
        p.content(ui, ("usage", index), text, line(TextRole::Secondary, 2));
    }
    refresh_reset_times(frame, app);
}

fn messages_ui(
    ui: &mut egui::Ui,
    frame: &mut BokkieFrame<'_, '_>,
    app: &App,
    open: &Open,
    bot_name: &str,
) {
    let store = app.store();
    let height = (ui.available_height() - COMPOSER_HEIGHT).max(80.0);
    observe_visible_only(ui, |ui| {
        egui::ScrollArea::vertical()
            .max_height(height)
            .min_scrolled_height(height)
            .stick_to_bottom(true)
            .auto_shrink(false)
            .show(ui, |ui| {
                let mut p = frame.presentation(
                    ui,
                    PresentationScope::new("messages"),
                    SemanticUiId::root(),
                );
                let Some(conversation) = store.conversation(open.bot) else {
                    match &open.error {
                        Some(error) => p.status(ui, "messages-status", error, true),
                        None => p.status(ui, "messages-status", "Loading the conversation…", false),
                    };
                    return;
                };
                if conversation.messages().next().is_none() {
                    p.content(
                        ui,
                        "no-messages",
                        "No messages yet. Say hello",
                        line(TextRole::Secondary, 1),
                    );
                }
                let older = app.older_state();
                if !older.is_empty() {
                    frame.action(ui, &mut p, BokkieAction::LoadOlder, ActionEmphasis::Normal);
                }
                if older == "loading" {
                    p.status(ui, "older-status", "Loading older messages…", false);
                } else if let Some(error) = &open.older_error {
                    p.status(ui, "older-status", error, true);
                }
                // Once a page of older messages arrives above it, the message
                // that was oldest moves back to the top of the view, once.
                let anchored = egui::Id::new("bokkie.older-anchor");
                let scroll_to = open
                    .anchor
                    .filter(|(page, _)| ui.data(|data| data.get_temp(anchored)) != Some(*page));
                let local = app.local_time();
                // Today's messages show their time alone until midnight.
                frame.request_repaint_after(
                    "message days",
                    crate::times::until_the_day_changes(&local),
                );
                let shown = Shown {
                    local: &local,
                    user_name: "You",
                    bot_name,
                    prompts: Some(&open.prompts),
                };
                for message in conversation.messages() {
                    let byline = message_by(ui, &mut p, frame, &shown, message);
                    if let Some((page, anchor)) = scroll_to
                        && anchor == message.id
                    {
                        byline.scroll_to_me(Some(egui::Align::TOP));
                        ui.data_mut(|data| data.insert_temp(anchored, page));
                    }
                }
            });
    });
}

/// How messages are shown to whoever reads them.
pub(crate) struct Shown<'a> {
    pub(crate) local: &'a LocalTime,
    /// The user's messages are by "You", or by the member an admin reads.
    pub(crate) user_name: &'a str,
    pub(crate) bot_name: &'a str,
    /// The routine runs whose prompt is unfolded, or `None` where prompts
    /// stay folded, as for an admin reading under oversight.
    pub(crate) prompts: Option<&'a BTreeSet<MessageId>>,
}

/// One message: the user's as reading text, the Bot's reply as Markdown,
/// and a marker as a compact line of its own. Returns its first line's
/// response.
pub(crate) fn message_by(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    shown: &Shown<'_>,
    message: &Message,
) -> egui::Response {
    let first = match message.kind {
        MessageKind::Marker => marker_ui(ui, p, frame, shown, message),
        MessageKind::Text => text_message_ui(ui, p, shown, message),
    };
    ui.add_space(6.0);
    first
}

fn text_message_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    shown: &Shown<'_>,
    message: &Message,
) -> egui::Response {
    let key = message.id.0;
    let author = match message.author {
        Author::User => shown.user_name,
        Author::Bot => shown.bot_name,
    };
    let time = crate::times::message_time(shown.local, message.created_at);
    let byline = match message.state {
        MessageState::Streaming => format!("{author} · {time} · replying…"),
        MessageState::Complete => format!("{author} · {time}"),
        MessageState::Interrupted => format!("{author} · {time} · stopped"),
        MessageState::Failed => format!("{author} · {time} · failed"),
    };
    let byline = p.content(ui, ("author", key), &byline, line(TextRole::Caption, 1));
    if message.text.is_empty() && message.state == MessageState::Streaming {
        p.reading_text(ui, ("text", key), "…", ReadingTextSpec::default());
    } else if message.author == Author::Bot {
        // A reply that is still arriving is safe to show as it stands.
        p.markdown(
            ui,
            ("text", key),
            &message.text,
            MarkdownSpec {
                link_description: Some(OPENS_A_NEW_TAB),
                ..MarkdownSpec::default()
            },
        );
    } else {
        p.reading_text(ui, ("text", key), &message.text, ReadingTextSpec::default());
    }
    byline
}

/// A marker: a capability change, a routine's run with its prompt folded
/// away, or a notification with its kind and body. None is the Bot
/// speaking, so none has a byline.
fn marker_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    shown: &Shown<'_>,
    message: &Message,
) -> egui::Response {
    let key = message.id.0;
    let time = crate::times::message_time(shown.local, message.created_at);
    let title = match (&message.routine, &message.notification) {
        // The gateway's text already says when the run was due, in the
        // user's zone, so a second time would repeat it.
        (Some(_), _) => format!("Routine run: {}", message.text),
        (None, Some(notification)) => {
            let kind = match notification.kind {
                NotificationKind::Result => "Result",
                NotificationKind::Question => "Question",
            };
            format!("{kind}: {} · {time}", message.text)
        }
        (None, None) => format!("{} · {time}", message.text),
    };
    let first = p.content(ui, ("marker", key), &title, line(TextRole::Caption, 3));
    if let Some(notification) = &message.notification
        && !notification.body.is_empty()
    {
        p.reading_text(
            ui,
            ("body", key),
            &notification.body,
            ReadingTextSpec::new(TextRole::Secondary),
        );
    }
    if let (Some(routine), Some(prompts)) = (&message.routine, shown.prompts) {
        let unfolded = prompts.contains(&message.id);
        frame.toggle(
            ui,
            p,
            "prompt",
            BoundAction::new(BokkieAction::ShowPrompt, key).with_subject(&message.text),
            unfolded,
        );
        if unfolded {
            p.reading_text(
                ui,
                ("prompt", key),
                &routine.prompt,
                ReadingTextSpec::new(TextRole::Secondary),
            );
        }
    }
    first
}

fn composer_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    app: &App,
    open: &Open,
) {
    // Only while a reply runs, on a row of its own.
    if app.shows_stop() {
        frame.action(ui, p, BokkieAction::Stop, ActionEmphasis::Normal);
    }
    if open.stopping {
        p.status(ui, "stop-status", "Stopping…", false);
    } else if let Some(note) = &open.stop_note {
        p.status(ui, "stop-status", note, false);
    }
    ui.add_enabled_ui(!open.sending, |ui| {
        let (draft, field) = text_field(ui, p, DRAFT_ID, "Message", &open.draft, false);
        if draft != open.draft {
            frame.emit(BokkieIntent::EditDraft(draft));
        }
        if submitted(ui, &field) {
            frame.trigger(BokkieAction::Send);
        }
    });
    frame.action(ui, p, BokkieAction::Send, ActionEmphasis::Primary);
    if open.sending {
        p.status(ui, "send-status", "Sending…", false);
    } else if let Some(error) = open
        .send_error
        .as_ref()
        // The banner already says it.
        .filter(|error| app.limit_banner().as_ref() != Some(*error))
    {
        p.status(ui, "send-status", error, true);
    } else if open.delivery == Some(Delivery::Steered) {
        p.status(
            ui,
            "send-status",
            "Sent while the Bot was replying: it steers the reply",
            false,
        );
    }
}
