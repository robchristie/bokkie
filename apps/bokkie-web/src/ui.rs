//! The application's presentation: plain stand-ins (settled decision 3), one
//! screen at a time, each control on a row of its own.

use bokkie_protocol::api::{Author, Bot, BotStatus, Delivery, Message, MessageState};
use stobie_ui_egui::{
    ActionArgument, ActionEmphasis, ContentTextSpec, Frame, NativeTextControlKind,
    PresentationContext, PresentationScope, SemanticUiId, TextInteraction, TextOverflow, TextRole,
    UiNode, UiRole, observe_visible_only,
};

use crate::app::{
    APPLICATION_NAME, Account, App, BOT_NAME_ID, BokkieAction, BokkieIntent, DRAFT_ID, Login,
    NewBot, Open, PASSWORD_ID, Session, SignInForm, SignedIn, USERNAME_ID,
};
use crate::socket::Link;
use crate::store::Store;

type BokkieFrame<'a, 'b> = Frame<'a, BokkieAction, BokkieIntent>;

/// Room kept below a conversation's messages for the message field, its
/// buttons and a status line.
const COMPOSER_HEIGHT: f32 = 230.0;

/// How often a meter with reset times is shown again, so they stay true.
const METER_REFRESH: std::time::Duration = std::time::Duration::from_secs(60);

pub(crate) fn present(app: &App, ui: &mut egui::Ui, frame: &mut BokkieFrame<'_, '_>) {
    egui::CentralPanel::default().show(ui, |ui| {
        let mut p = frame.presentation(ui, PresentationScope::new("bokkie"), SemanticUiId::root());
        match &app.session {
            Session::Checking => {
                p.heading(ui, "title", APPLICATION_NAME);
                p.status(ui, "status", "Checking your session…", false);
            }
            Session::SignedOut(form) => sign_in_ui(ui, &mut p, frame, form),
            Session::SignedIn(signed_in) => match (&signed_in.login, &signed_in.open) {
                (Some(login), _) => chatgpt_ui(ui, &mut p, frame, signed_in, login),
                (None, Some(open)) => conversation_ui(ui, &mut p, frame, app, signed_in, open),
                (None, None) => bots_ui(ui, &mut p, frame, app, signed_in),
            },
        }
    });
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

/// Enter pressed in a field that just lost focus to it.
fn submitted(ui: &egui::Ui, field: &egui::Response) -> bool {
    field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))
}

fn sign_in_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
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
        if submitted(ui, &field) {
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

/// A Bot's label and status, or its status alone for a Bot with no label.
fn details(bot: &Bot) -> String {
    match bot.label.as_str() {
        "" => status_text(bot.status).to_owned(),
        label => format!("{label} · {}", status_text(bot.status)),
    }
}

fn status_text(status: BotStatus) -> &'static str {
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
    if signed_in.signing_out {
        p.status(ui, "status", "Signing out…", false);
    } else if let Some(message) = &signed_in.message {
        p.status(ui, "status", message, true);
    }
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
    if let Some(message) = &signed_in.bots_error {
        p.status(ui, "bots-status", message, true);
    } else if !store.bots_loaded() {
        p.status(ui, "bots-status", "Loading Bots…", false);
        return;
    }
    let bots = store.visible_bots();
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
                new_bot_ui(ui, &mut rows, frame, &signed_in.new_bot);
            });
    });
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

/// Whether the user's own app server is signed in to ChatGPT, with the way
/// to sign it in when it is not.
fn account_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    signed_in: &SignedIn,
) {
    match &signed_in.account {
        Account::Checking => {
            p.content(
                ui,
                "chatgpt",
                "ChatGPT: checking…",
                line(TextRole::Secondary, 1),
            );
        }
        Account::SignedIn { plan } => {
            let text = match plan {
                Some(plan) => format!("ChatGPT: signed in ({plan})"),
                None => "ChatGPT: signed in".into(),
            };
            p.content(ui, "chatgpt", &text, line(TextRole::Secondary, 1));
        }
        Account::SignedOut => {
            p.status(
                ui,
                "chatgpt",
                "ChatGPT is not signed in, so your Bots cannot reply",
                true,
            );
            frame.action(ui, p, BokkieAction::ChatGptSignIn, ActionEmphasis::Normal);
        }
        Account::Failed(message) => {
            p.status(ui, "chatgpt", message, true);
            frame.action(ui, p, BokkieAction::ChatGptSignIn, ActionEmphasis::Normal);
        }
    }
}

/// The ChatGPT device-code sign-in (settled decision 8). The code is shown
/// here and kept nowhere else.
fn chatgpt_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    signed_in: &SignedIn,
    login: &Login,
) {
    frame.action(ui, p, BokkieAction::Back, ActionEmphasis::Normal);
    p.heading(ui, "title", "Sign in to ChatGPT");
    p.content(
        ui,
        "about",
        "Your Bots reply only once Bokkie's server is signed in to ChatGPT for you. Sign in once, with a code; the server keeps the sign-in for your Bots.",
        line(TextRole::Secondary, 4),
    );
    match login {
        Login::Ready => match &signed_in.account {
            Account::Checking => {
                p.status(ui, "status", "Checking your ChatGPT sign-in…", false);
            }
            Account::SignedIn { .. } => {
                p.status(ui, "status", "You are already signed in to ChatGPT", false);
            }
            Account::SignedOut | Account::Failed(_) => {
                if let Account::Failed(message) = &signed_in.account {
                    p.status(ui, "account-error", message, true);
                }
                frame.action(ui, p, BokkieAction::StartChatGpt, ActionEmphasis::Primary);
            }
        },
        Login::Starting => {
            p.status(ui, "status", "Asking for a code…", false);
        }
        Login::Waiting { code, error, .. } => {
            p.content(
                ui,
                "step-1",
                "1. Open this page, on any device:",
                line(TextRole::Body, 1),
            );
            // In a new tab: this page holds the code and must stay open.
            p.raw(ui, "link", "the device-code page as a link", |ui| {
                ui.add(egui::Hyperlink::new(&code.0.verification_url).open_in_new_tab(true))
            });
            p.content(ui, "step-2", "2. Enter this code:", line(TextRole::Body, 1));
            p.heading(ui, "code", &code.0.user_code);
            p.status(ui, "status", "Waiting for you to finish signing in…", false);
            if let Some(error) = error {
                p.status(ui, "poll-error", error, true);
            }
            p.content(
                ui,
                "keep-open",
                "Keep this page open: the code is shown only here. If it has expired, start again.",
                line(TextRole::Secondary, 3),
            );
            frame.action(ui, p, BokkieAction::StartChatGpt, ActionEmphasis::Normal);
        }
        Login::Done => {
            let text = match &signed_in.account {
                Account::SignedIn { plan: Some(plan) } => {
                    format!("Signed in to ChatGPT ({plan}). Your Bots can reply")
                }
                _ => "Signed in to ChatGPT. Your Bots can reply".into(),
            };
            p.status(ui, "status", &text, false);
        }
        Login::Failed(message) => {
            p.status(ui, "status", message, true);
            frame.action(ui, p, BokkieAction::StartChatGpt, ActionEmphasis::Primary);
        }
    };
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
    // A Stobie action has no per-row target, so each row's control triggers
    // the Open action with its Bot's ID.
    let name = format!("Open {}", bot.name);
    let (open, ()) = p.native(ui, NativeTextControlKind::Button, |ui| {
        (ui.button(name.as_str()), ())
    });
    let mut node = UiNode::container(
        SemanticUiId::new(format!("bokkie.open-bot.{key}")),
        Some(SemanticUiId::root()),
        UiRole::Button,
        open.rect.into(),
    );
    node.name = name;
    node.focused = open.has_focus();
    p.observe_node(ui, node);
    if open.clicked() {
        frame.trigger_with(BokkieAction::OpenBot, ActionArgument::Number(key as f64));
    }
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
    messages_ui(ui, frame, store, open, name);
    ui.separator();
    composer_ui(ui, p, frame, app, open);
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
    store: &Store,
    open: &Open,
    bot_name: &str,
) {
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
                if conversation.older().is_some() {
                    p.content(
                        ui,
                        "older",
                        "Older messages are not shown",
                        line(TextRole::Caption, 1),
                    );
                }
                for message in conversation.messages() {
                    message_ui(ui, &mut p, message, bot_name);
                }
            });
    });
}

fn message_ui(ui: &mut egui::Ui, p: &mut PresentationContext, message: &Message, bot_name: &str) {
    let key = message.id.0;
    let author = match message.author {
        Author::User => "You",
        Author::Bot => bot_name,
    };
    let byline = match message.state {
        MessageState::Streaming => format!("{author} · replying…"),
        MessageState::Complete => author.to_owned(),
        MessageState::Interrupted => format!("{author} · stopped"),
        MessageState::Failed => format!("{author} · failed"),
    };
    p.content(ui, ("author", key), &byline, line(TextRole::Caption, 1));
    let text = if message.text.is_empty() && message.state == MessageState::Streaming {
        "…"
    } else {
        message.text.as_str()
    };
    // A reply can run past the eight lines Stobie's content recipe holds, so
    // its text is a plain wrapped label.
    p.raw(
        ui,
        ("text", key),
        "message text longer than a content recipe's eight lines",
        |ui| ui.add(egui::Label::new(text).wrap()),
    );
    ui.add_space(6.0);
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
