//! The admin section's screens, in the navigation's content pane, laid out
//! from Stobie's ordinary controls, each on a row of its own.

use bokkie_protocol::api::{AdminUser, InvitationSummary, MemberBot, Role};
use stobie_ui_egui::{
    ActionEmphasis, BoundAction, CopySpec, NavigationLayout, PresentationContext,
    PresentationScope, SemanticUiId, TextRole, observe_visible_only,
};

use super::{
    AccountScreen, Admin, DAILY_TURNS_ID, INVITE_NAME_ID, MEMBER_BOT_NAME_ID, NEW_PASSWORD_ID,
    Reading, WEEKLY_TOKENS_ID,
};
use crate::app::{
    App, BokkieAction, BokkieIntent, PROFILE_DESCRIPTION_ID, PROFILE_LABEL_ID, PROFILE_NAME_ID,
    ProfileEditor, SignedIn,
};
use crate::ui::{
    BokkieFrame, Shown, line, message_by, status_text, submitted, text_area, text_field,
};

pub(crate) fn present(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    app: &App,
    signed_in: &SignedIn,
) {
    let Some(admin) = &signed_in.admin else {
        return;
    };
    // The stack's header has Back; beside the list, the section keeps its
    // own, to go back a level.
    if app.layout() != NavigationLayout::Stack {
        frame.action(ui, p, BokkieAction::Back, ActionEmphasis::Normal);
    }
    match (&admin.account, signed_in.admin_account()) {
        (Some(screen), Some((account, _))) => match &screen.reading {
            Some(reading) => reading_ui(ui, p, frame, app, account, screen, reading),
            None => account_ui(ui, p, frame, signed_in, account, screen),
        },
        // The account went from the list.
        (Some(_), None) => {
            p.status(ui, "status", "This account is not on the list", true);
        }
        (None, _) => accounts_ui(ui, p, frame, app, admin),
    }
}

/// The title of the admin section's screen: the accounts, an account, or
/// the member's Bot being read.
pub(crate) fn title(signed_in: &SignedIn) -> String {
    let Some(admin) = &signed_in.admin else {
        return String::new();
    };
    match (&admin.account, signed_in.admin_account()) {
        (Some(screen), Some((account, _))) => match &screen.reading {
            Some(reading) => reading_name(screen, reading).to_owned(),
            None => account.user.display_name.clone(),
        },
        (Some(_), None) => "Account".into(),
        (None, _) => "Accounts".into(),
    }
}

/// The name of the member's Bot being read.
fn reading_name<'a>(screen: &'a AccountScreen, reading: &Reading) -> &'a str {
    screen
        .bots
        .iter()
        .flat_map(|list| &list.bots)
        .find(|bot| bot.id == reading.bot)
        .map_or("Bot", |bot| bot.name.as_str())
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Admin => "Admin",
        Role::Member => "Member",
    }
}

/// An account's role and state, as one line.
fn account_details(account: &AdminUser) -> String {
    let user = &account.user;
    let mut details = format!("{} · {}", user.username, role_name(user.role));
    if user.disabled {
        details.push_str(" · Disabled");
    }
    if user.oversight {
        details.push_str(" · Oversight on");
    }
    details
}

fn accounts_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    app: &App,
    admin: &Admin,
) {
    frame.action(ui, p, BokkieAction::Refresh, ActionEmphasis::Normal);
    observe_visible_only(ui, |ui| {
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                let mut rows =
                    frame.presentation(ui, PresentationScope::new("admin"), SemanticUiId::root());
                let p = &mut rows;
                if let Some(error) = &admin.accounts_error {
                    p.status(ui, "accounts-status", error, true);
                } else if admin.accounts.is_none() {
                    p.status(ui, "accounts-status", "Loading accounts…", false);
                }
                for account in admin.accounts.iter().flatten() {
                    let key = account.user.id.0;
                    p.content(
                        ui,
                        ("account", key),
                        &account.user.display_name,
                        line(TextRole::Body, 1),
                    );
                    p.content(
                        ui,
                        ("account-details", key),
                        &account_details(account),
                        line(TextRole::Secondary, 1),
                    );
                    // "Open account Ada" to assistive technology and agents.
                    frame.action(
                        ui,
                        p,
                        BoundAction::new(BokkieAction::OpenAccount, key)
                            .with_subject(&account.user.display_name),
                        ActionEmphasis::Normal,
                    );
                    ui.separator();
                }
                invitations_ui(ui, p, frame, app, admin);
                invite_ui(ui, p, frame, app, admin);
            });
    });
}

/// Whom an invitation is for, naming its Withdraw control: "for Bea", or
/// "for anyone" with its ID, since several can be.
fn invitation_subject(invitation: &InvitationSummary) -> String {
    match invitation.display_name.as_deref() {
        Some(name) => format!("for {name}"),
        None => format!("{} for anyone", invitation.id),
    }
}

fn invitation_text(invitation: &InvitationSummary, app: &App) -> String {
    let who = invitation.display_name.as_deref().unwrap_or("anyone");
    format!(
        "{} invitation for {who}, expires {}",
        role_name(invitation.role),
        crate::usage::resets_in(app.now(), invitation.expires_at)
    )
}

fn invitations_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    app: &App,
    admin: &Admin,
) {
    p.heading(ui, "invitations-title", "Invitations");
    if let Some(error) = &admin.invitations_error {
        p.status(ui, "invitations-status", error, true);
    }
    match &admin.invitations {
        None => {
            p.status(ui, "invitations-loading", "Loading invitations…", false);
        }
        Some(invitations) if invitations.is_empty() => {
            p.content(
                ui,
                "no-invitations",
                "No invitations are waiting",
                line(TextRole::Secondary, 1),
            );
        }
        Some(invitations) => {
            for invitation in invitations {
                let key = invitation.id;
                p.content(
                    ui,
                    ("invitation", key),
                    &invitation_text(invitation, app),
                    line(TextRole::Secondary, 2),
                );
                frame.action(
                    ui,
                    p,
                    BoundAction::new(BokkieAction::WithdrawInvitation, key)
                        .with_subject(invitation_subject(invitation)),
                    ActionEmphasis::Normal,
                );
            }
        }
    }
    if admin.withdrawing.is_some() {
        p.status(ui, "withdraw-status", "Withdrawing…", false);
    }
    ui.separator();
}

fn invite_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    app: &App,
    admin: &Admin,
) {
    let form = &admin.invite;
    p.heading(ui, "invite-title", "Invite someone");
    frame.toggle(ui, p, "invite-admin", BokkieAction::InviteAdmin, form.admin);
    if form.admin {
        p.content(
            ui,
            "invite-admin-note",
            "An admin always has both permissions.",
            line(TextRole::Secondary, 1),
        );
    } else {
        frame.toggle(
            ui,
            p,
            "invite-create",
            BokkieAction::InviteMayCreateBots,
            form.permissions.may_create_bots,
        );
        frame.toggle(
            ui,
            p,
            "invite-edit",
            BokkieAction::InviteMayEditProfiles,
            form.permissions.may_edit_bot_profiles,
        );
    }
    ui.add_enabled_ui(!form.busy, |ui| {
        let (name, field) = text_field(
            ui,
            p,
            INVITE_NAME_ID,
            "Their name (optional)",
            &form.display_name,
            false,
        );
        if name != form.display_name {
            frame.emit(BokkieIntent::EditInviteName(name));
        }
        if submitted(ui, &field) {
            frame.trigger(BokkieAction::CreateInvitation);
        }
    });
    frame.action(
        ui,
        p,
        BokkieAction::CreateInvitation,
        ActionEmphasis::Primary,
    );
    if form.busy {
        p.status(ui, "invite-status", "Creating…", false);
    } else if let Some(error) = &form.error {
        p.status(ui, "invite-status", error, true);
    }
    if let Some(invitation) = &form.created {
        let invitation = &invitation.0;
        p.status(
            ui,
            "code-about",
            "Give this code to the new account's owner. It is shown only now.",
            false,
        );
        // On screen for the person, and copied with Copy; never in content
        // text, a fact or an observation.
        stobie_ui_egui::withhold_content_text(ui, |ui| {
            p.content(
                ui,
                "code",
                &invitation.code,
                line(TextRole::MonospaceTechnical, 2),
            );
        });
        p.copy(
            ui,
            "copy-code",
            &CopySpec::new(
                &invitation.code,
                "Copy invitation code",
                "Copied",
                "Couldn't copy",
            ),
        );
        p.content(
            ui,
            "code-expiry",
            &format!(
                "It expires {}",
                crate::usage::resets_in(app.now(), invitation.expires_at)
            ),
            line(TextRole::Secondary, 1),
        );
        frame.action(
            ui,
            p,
            BokkieAction::HideInvitationCode,
            ActionEmphasis::Normal,
        );
    }
}

fn account_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    signed_in: &SignedIn,
    account: &AdminUser,
    screen: &AccountScreen,
) {
    let user = &account.user;
    let me = user.id == signed_in.user.id;
    p.content(
        ui,
        "details",
        &account_details(account),
        line(TextRole::Secondary, 1),
    );
    frame.action(ui, p, BokkieAction::Refresh, ActionEmphasis::Normal);
    observe_visible_only(ui, |ui| {
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                let mut rows = frame.presentation(
                    ui,
                    PresentationScope::new("account"),
                    SemanticUiId::root(),
                );
                let p = &mut rows;
                if screen.saving {
                    p.status(ui, "saving", "Saving…", false);
                } else if let Some(error) = &screen.error {
                    p.status(ui, "error", error, true);
                }
                if user.role == Role::Member {
                    frame.toggle(
                        ui,
                        p,
                        "may-create",
                        BokkieAction::MayCreateBots,
                        user.permissions.may_create_bots,
                    );
                    frame.toggle(
                        ui,
                        p,
                        "may-edit",
                        BokkieAction::MayEditProfiles,
                        user.permissions.may_edit_bot_profiles,
                    );
                    frame.toggle(ui, p, "oversight", BokkieAction::Oversight, user.oversight);
                    p.content(
                        ui,
                        "oversight-about",
                        "While oversight is on, admins can read all of this member's Bots and conversations, read-only, and the member sees a notice saying so.",
                        line(TextRole::Secondary, 4),
                    );
                } else {
                    p.content(
                        ui,
                        "admin-about",
                        "An admin always has both permissions, and oversight never applies to an admin.",
                        line(TextRole::Secondary, 3),
                    );
                }
                if !me {
                    frame.toggle(ui, p, "disabled", BokkieAction::Disabled, user.disabled);
                }
                ui.separator();
                limits_ui(ui, p, frame, screen);
                ui.separator();
                password_ui(ui, p, frame, user.role == Role::Admin && !me, screen);
                if user.role == Role::Member {
                    ui.separator();
                    member_bots_ui(ui, p, frame, screen);
                }
            });
    });
}

fn limits_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    screen: &AccountScreen,
) {
    p.heading(ui, "limits-title", "Limits");
    if let Some(allowance) = &screen.allowance {
        for (index, text) in crate::usage::allowance_lines(allowance, "Their")
            .iter()
            .enumerate()
        {
            p.content(ui, ("allowance", index), text, line(TextRole::Secondary, 1));
        }
        if allowance.exhausted {
            p.status(ui, "exhausted", "Their allowance is used up for now", true);
        }
    } else if let Some(error) = &screen.allowance_error {
        p.status(ui, "allowance-error", error, true);
    }
    ui.add_enabled_ui(!screen.saving, |ui| {
        let (turns, _) = text_field(
            ui,
            p,
            DAILY_TURNS_ID,
            "Turns per day (empty for no limit)",
            &screen.daily_turns,
            false,
        );
        if turns != screen.daily_turns {
            frame.emit(BokkieIntent::EditDailyTurns(turns));
        }
        let (tokens, field) = text_field(
            ui,
            p,
            WEEKLY_TOKENS_ID,
            "Tokens per week (empty for no limit)",
            &screen.weekly_tokens,
            false,
        );
        if tokens != screen.weekly_tokens {
            frame.emit(BokkieIntent::EditWeeklyTokens(tokens));
        }
        if submitted(ui, &field) {
            frame.trigger(BokkieAction::SaveLimits);
        }
    });
    if let Err(reason) = screen.limits() {
        p.status(ui, "limits-status", reason, true);
    }
    frame.action(ui, p, BokkieAction::SaveLimits, ActionEmphasis::Normal);
}

fn password_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    another_admin: bool,
    screen: &AccountScreen,
) {
    p.heading(ui, "password-title", "Password");
    if another_admin {
        p.status(
            ui,
            "password-refused",
            "No admin can reset another admin's password",
            false,
        );
        return;
    }
    p.content(
        ui,
        "password-about",
        "Resetting the password ends every session of the account. A member is told an admin reset it.",
        line(TextRole::Secondary, 3),
    );
    ui.add_enabled_ui(!screen.resetting, |ui| {
        let (password, field) = text_field(
            ui,
            p,
            NEW_PASSWORD_ID,
            "New password",
            &screen.new_password,
            true,
        );
        if password != screen.new_password {
            frame.emit(BokkieIntent::EditNewPassword(password));
        }
        if submitted(ui, &field) {
            frame.trigger(BokkieAction::ResetPassword);
        }
    });
    frame.action(ui, p, BokkieAction::ResetPassword, ActionEmphasis::Normal);
    if screen.resetting {
        p.status(ui, "reset-status", "Resetting…", false);
    } else if let Some((note, problem)) = &screen.reset_note {
        p.status(ui, "reset-status", note, *problem);
    }
}

fn member_bot_details(bot: &MemberBot) -> String {
    let mut details = String::new();
    if !bot.label.is_empty() {
        details.push_str(&bot.label);
        details.push_str(" · ");
    }
    details.push_str(if bot.made_by_admin {
        "Made by an admin"
    } else {
        "Their own"
    });
    if let Some(overseen) = &bot.overseen {
        details.push_str(" · ");
        details.push_str(status_text(overseen.status));
        if overseen.hidden {
            details.push_str(" · Hidden");
        }
    }
    details
}

fn member_bots_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    screen: &AccountScreen,
) {
    p.heading(ui, "bots-title", "Their Bots");
    if let Some(error) = &screen.bots_error {
        p.status(ui, "bots-status", error, true);
    }
    match &screen.bots {
        None => {
            p.status(ui, "bots-loading", "Loading their Bots…", false);
        }
        Some(list) => {
            if !list.oversight {
                p.content(
                    ui,
                    "bots-about",
                    "With oversight off you see only the Bots an admin made for them, and no conversation.",
                    line(TextRole::Secondary, 3),
                );
            }
            if list.bots.is_empty() {
                p.content(
                    ui,
                    "no-bots",
                    "No Bots to show",
                    line(TextRole::Secondary, 1),
                );
            }
            for bot in &list.bots {
                let key = bot.id.0;
                p.content(ui, ("bot", key), &bot.name, line(TextRole::Body, 1));
                p.content(
                    ui,
                    ("bot-details", key),
                    &member_bot_details(bot),
                    line(TextRole::Secondary, 1),
                );
                if let Some(preview) = bot.overseen.as_ref().and_then(|b| b.last_message.as_ref()) {
                    p.content(
                        ui,
                        ("bot-preview", key),
                        &preview.text,
                        line(TextRole::Caption, 2),
                    );
                }
                match &screen.editing {
                    Some((editing, editor)) if *editing == bot.id => {
                        bot_editor_ui(ui, p, frame, editor);
                    }
                    _ => {
                        if bot.made_by_admin {
                            frame.action(
                                ui,
                                p,
                                BoundAction::new(BokkieAction::EditMemberBot, key)
                                    .with_subject(&bot.name),
                                ActionEmphasis::Normal,
                            );
                        }
                        if list.oversight {
                            frame.action(
                                ui,
                                p,
                                BoundAction::new(BokkieAction::ReadConversation, key)
                                    .with_subject(format!("with {}", bot.name)),
                                ActionEmphasis::Normal,
                            );
                        }
                    }
                }
                ui.separator();
            }
        }
    }
    p.heading(ui, "make-title", "Make a Bot for them");
    let form = &screen.new_bot;
    ui.add_enabled_ui(!form.busy, |ui| {
        let (name, field) = text_field(
            ui,
            p,
            MEMBER_BOT_NAME_ID,
            "New Bot's name",
            &form.name,
            false,
        );
        if name != form.name {
            frame.emit(BokkieIntent::EditMemberBotName(name));
        }
        if submitted(ui, &field) {
            frame.trigger(BokkieAction::MakeMemberBot);
        }
    });
    frame.action(ui, p, BokkieAction::MakeMemberBot, ActionEmphasis::Normal);
    if form.busy {
        p.status(ui, "make-status", "Making…", false);
    } else if let Some(error) = &form.error {
        p.status(ui, "make-status", error, true);
    }
}

fn bot_editor_ui(
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
    frame.action(ui, p, BokkieAction::SaveMemberBot, ActionEmphasis::Primary);
    frame.action(ui, p, BokkieAction::CancelMemberBot, ActionEmphasis::Normal);
    if editor.busy {
        p.status(ui, "edit-status", "Saving…", false);
    } else if let Some(error) = &editor.error {
        p.status(ui, "edit-status", error, true);
    }
}

/// A member's conversation, read-only: no message field, Send, steer or
/// Stop.
fn reading_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    app: &App,
    account: &AdminUser,
    screen: &AccountScreen,
    reading: &Reading,
) {
    let name = reading_name(screen, reading);
    let member = &account.user.display_name;
    p.status(
        ui,
        "read-only",
        &format!(
            "Read-only, under oversight: {member}'s conversation. Nothing can be sent from here"
        ),
        false,
    );
    frame.action(ui, p, BokkieAction::Refresh, ActionEmphasis::Normal);
    if let Some(error) = &reading.error {
        p.status(ui, "reading-status", error, true);
    }
    ui.separator();
    observe_visible_only(ui, |ui| {
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                let mut rows = frame.presentation(
                    ui,
                    PresentationScope::new("overseen"),
                    SemanticUiId::root(),
                );
                let p = &mut rows;
                if reading.older.is_some() {
                    frame.action(ui, p, BokkieAction::LoadEarlier, ActionEmphasis::Normal);
                }
                if reading.loading.is_some() {
                    p.status(ui, "loading", "Loading…", false);
                } else if reading.loaded && reading.messages.is_empty() {
                    p.content(
                        ui,
                        "no-messages",
                        "No messages yet",
                        line(TextRole::Secondary, 1),
                    );
                }
                let local = app.local_time();
                frame.request_repaint_after(
                    "message days",
                    crate::times::until_the_day_changes(&local),
                );
                // Prompts stay folded: reading is read-only.
                let shown = Shown {
                    local: &local,
                    user_name: member,
                    bot_name: name,
                    prompts: None,
                };
                for message in reading.messages.values() {
                    message_by(ui, p, frame, &shown, message);
                }
            });
    });
}
