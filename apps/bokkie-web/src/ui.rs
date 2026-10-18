//! The application's presentation: plain stand-ins (settled decision 3), one
//! screen at a time, each control on a row of its own.

use bokkie_protocol::api::{Author, Bot, BotStatus, Delivery, Message, MessageState};
use stobie_ui_egui::{
    ActionArgument, ActionEmphasis, ContentTextSpec, Frame, NativeTextControlKind,
    PresentationContext, PresentationScope, SemanticUiId, TextInteraction, TextOverflow, TextRole,
    UiNode, UiRole, observe_visible_only,
};

use crate::app::{
    APPLICATION_NAME, App, BokkieAction, BokkieIntent, DRAFT_ID, Open, PASSWORD_ID, Session,
    SignInForm, SignedIn, USERNAME_ID,
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
            Session::SignedIn(signed_in) => match &signed_in.open {
                Some(open) => conversation_ui(ui, &mut p, frame, app, signed_in, open),
                None => bots_ui(ui, &mut p, frame, app, signed_in),
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
    limit_ui(ui, p, app);
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
                    bot_row(ui, &mut rows, frame, bot);
                }
            });
    });
}

fn bot_row(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    bot: &Bot,
) {
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
        let details = format!("{} · {}", bot.label, status_text(bot.status));
        p.content(ui, "details", &details, line(TextRole::Secondary, 1));
    }
    link_status(ui, p, signed_in.link);
    if signed_in.link == Link::Outdated {
        frame.action(ui, p, BokkieAction::ReloadPage, ActionEmphasis::Primary);
    }
    limit_ui(ui, p, app);
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
fn limit_ui(ui: &mut egui::Ui, p: &mut PresentationContext, app: &App) {
    if let Some(banner) = app.limit_banner() {
        p.status(ui, "limit", &banner, true);
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
    let resets = usage
        .subscription
        .iter()
        .flat_map(|bucket| [&bucket.primary, &bucket.secondary])
        .flatten()
        .any(|window| window.resets_at.is_some());
    if resets {
        frame.request_repaint_after("usage meter reset times", METER_REFRESH);
    }
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
    } else if let Some(error) = &open.send_error {
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
