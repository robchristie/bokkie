//! The admin section: every account and the outstanding invitations, one
//! account at a time with its permissions, oversight, limits, password and
//! allowance, the member's Bots an admin may see, and, while oversight is
//! on, a member's conversation, read-only.
//!
//! What it shows are snapshots over HTTP: an admin's changes to another
//! account reach that account's own event log, not the admin's, so the
//! section loads again after each change and on Refresh.

mod view;

use std::collections::BTreeMap;

use bokkie_protocol::api::{
    AdminUser, Allowance, CreateBotRequest, CreateInvitationRequest, ErrorCode, InvitationSummary,
    Limits, MemberBotList, Message, Permissions, ResetPasswordRequest, Role,
    UpdateBotProfileRequest, UpdateUserRequest,
};
use bokkie_protocol::{BotId, MessageId, UserId};

use crate::api::{ApiReply, ApiRequest, Invitation};
use crate::app::{App, BokkieAction, NewBot, ProfileEditor, Session, SignedIn};

pub(crate) use view::present;

/// The daily turns field's stable ID.
pub const DAILY_TURNS_ID: &str = "bokkie.daily-turns";
/// The weekly tokens field's stable ID.
pub const WEEKLY_TOKENS_ID: &str = "bokkie.weekly-tokens";
/// The reset password field's stable ID. Stobie offers no `set-text` on a
/// password field.
pub const NEW_PASSWORD_ID: &str = "bokkie.new-password";
/// The invitation's display name field's stable ID.
pub const INVITE_NAME_ID: &str = "bokkie.invite-name";
/// The name field of a Bot made for a member.
pub const MEMBER_BOT_NAME_ID: &str = "bokkie.member-bot-name";

/// The shortest password the gateway accepts.
const MIN_PASSWORD_CHARS: usize = 10;

/// The admin section, while an admin has it open.
#[derive(Debug, Default)]
pub(crate) struct Admin {
    /// Every account, once loaded.
    pub(crate) accounts: Option<Vec<AdminUser>>,
    pub(crate) accounts_loading: bool,
    pub(crate) accounts_error: Option<String>,
    /// The outstanding invitations, once loaded.
    pub(crate) invitations: Option<Vec<InvitationSummary>>,
    pub(crate) invitations_loading: bool,
    pub(crate) invitations_error: Option<String>,
    /// The invitation being withdrawn.
    pub(crate) withdrawing: Option<i64>,
    pub(crate) invite: InviteForm,
    /// The account open, if any.
    pub(crate) account: Option<AccountScreen>,
}

/// A new invitation: its role, permissions (both on at first) and
/// suggested name.
#[derive(Debug, Default)]
pub(crate) struct InviteForm {
    pub(crate) admin: bool,
    pub(crate) permissions: Permissions,
    pub(crate) display_name: String,
    pub(crate) busy: bool,
    pub(crate) error: Option<String>,
    /// The invitation just made. Its code is shown here, once, and goes when
    /// the admin hides it or leaves the section.
    pub(crate) created: Option<Invitation>,
}

/// One account, open in the admin section.
#[derive(Debug)]
pub(crate) struct AccountScreen {
    pub(crate) id: UserId,
    /// A change to the account is under way.
    pub(crate) saving: bool,
    /// Why the last change failed.
    pub(crate) error: Option<String>,
    pub(crate) daily_turns: String,
    pub(crate) weekly_tokens: String,
    pub(crate) new_password: String,
    pub(crate) resetting: bool,
    /// What the last reset did, and whether that is a problem.
    pub(crate) reset_note: Option<(String, bool)>,
    pub(crate) allowance: Option<Allowance>,
    pub(crate) allowance_error: Option<String>,
    /// The member's Bots an admin may see; never loaded for an admin.
    pub(crate) bots: Option<MemberBotList>,
    pub(crate) bots_loading: bool,
    pub(crate) bots_error: Option<String>,
    /// A Bot being made for the member.
    pub(crate) new_bot: NewBot,
    /// An admin-made Bot's profile being edited.
    pub(crate) editing: Option<(BotId, ProfileEditor)>,
    /// A conversation being read under oversight.
    pub(crate) reading: Option<Reading>,
}

impl AccountScreen {
    fn new(account: &AdminUser) -> Self {
        let mut screen = Self {
            id: account.user.id,
            saving: false,
            error: None,
            daily_turns: String::new(),
            weekly_tokens: String::new(),
            new_password: String::new(),
            resetting: false,
            reset_note: None,
            allowance: None,
            allowance_error: None,
            bots: None,
            bots_loading: false,
            bots_error: None,
            new_bot: NewBot::default(),
            editing: None,
            reading: None,
        };
        screen.show_limits(account.limits);
        screen
    }

    /// Oversight is off: drop the conversation being read and everything
    /// the Bot list showed under oversight, keeping only what an admin may
    /// see without it, until the list is read again.
    fn end_oversight(&mut self) {
        self.reading = None;
        if let Some(list) = &mut self.bots {
            list.oversight = false;
            list.bots.retain(|bot| bot.made_by_admin);
            for bot in &mut list.bots {
                bot.overseen = None;
            }
        }
    }

    fn show_limits(&mut self, limits: Limits) {
        self.daily_turns = limits
            .daily_turns
            .map(|n| n.to_string())
            .unwrap_or_default();
        self.weekly_tokens = limits
            .weekly_tokens
            .map(|n| n.to_string())
            .unwrap_or_default();
    }

    /// The limits as entered: a whole number, or empty for no limit.
    pub(crate) fn limits(&self) -> Result<Limits, &'static str> {
        let daily_turns = number(&self.daily_turns)
            .ok_or("Turns per day must be a whole number, or empty for no limit")?;
        let weekly_tokens = number(&self.weekly_tokens)
            .ok_or("Tokens per week must be a whole number, or empty for no limit")?;
        Ok(Limits {
            daily_turns,
            weekly_tokens,
        })
    }
}

/// "" as no limit, "20" as 20; anything else is not a number.
fn number<T: std::str::FromStr>(text: &str) -> Option<Option<T>> {
    let text = text.trim();
    if text.is_empty() {
        return Some(None);
    }
    if !text.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().map(Some)
}

/// A member's conversation, read-only, under oversight.
#[derive(Debug)]
pub(crate) struct Reading {
    pub(crate) bot: BotId,
    /// The messages loaded, oldest first.
    pub(crate) messages: BTreeMap<MessageId, Message>,
    /// The cursor for earlier messages, `None` at the start.
    pub(crate) older: Option<MessageId>,
    /// Whether the newest page has loaded.
    pub(crate) loaded: bool,
    /// The page being loaded: `Some(None)` the newest, `Some(Some(id))` the
    /// one before `id`.
    pub(crate) loading: Option<Option<MessageId>>,
    pub(crate) error: Option<String>,
}

impl SignedIn {
    /// The account the admin section has open, as last loaded.
    pub(crate) fn admin_account(&self) -> Option<(&AdminUser, &AccountScreen)> {
        let admin = self.admin.as_ref()?;
        let screen = admin.account.as_ref()?;
        let account = admin
            .accounts
            .iter()
            .flatten()
            .find(|account| account.user.id == screen.id)?;
        Some((account, screen))
    }
}

impl App {
    fn admin_mut(&mut self) -> Option<&mut Admin> {
        match &mut self.session {
            Session::SignedIn(signed_in) => signed_in.admin.as_mut(),
            _ => None,
        }
    }

    fn account_screen_mut(&mut self, id: UserId) -> Option<&mut AccountScreen> {
        self.admin_mut()?
            .account
            .as_mut()
            .filter(|screen| screen.id == id)
    }

    fn load_accounts(&mut self) {
        if let Some(admin) = self.admin_mut() {
            admin.accounts_loading = true;
        }
        self.send(ApiRequest::ListAccounts);
    }

    fn load_invitations(&mut self) {
        if let Some(admin) = self.admin_mut() {
            admin.invitations_loading = true;
        }
        self.send(ApiRequest::ListInvitations);
    }

    fn load_member_bots(&mut self, id: UserId) {
        if let Some(screen) = self.account_screen_mut(id) {
            screen.bots_loading = true;
        }
        self.send(ApiRequest::MemberBots(id));
    }

    /// Load the open account's allowance, and a member's Bots.
    fn load_account(&mut self, id: UserId, member: bool) {
        self.send(ApiRequest::AccountUsage(id));
        if member {
            self.load_member_bots(id);
        }
    }

    /// Apply a reply that belongs to the admin section, or hand it back.
    pub(crate) fn admin_reply(&mut self, reply: ApiReply) -> Result<bool, Box<ApiReply>> {
        let Session::SignedIn(signed_in) = &mut self.session else {
            return Err(Box::new(reply));
        };
        // Accepting an invitation happens signed out; the rest needs the
        // section open.
        let Some(admin) = signed_in.admin.as_mut() else {
            return match reply {
                ApiReply::Accounts(_)
                | ApiReply::AccountUpdated(..)
                | ApiReply::PasswordReset(..)
                | ApiReply::AccountUsage(..)
                | ApiReply::Invitations(_)
                | ApiReply::Invited(_)
                | ApiReply::InvitationWithdrawn(..)
                | ApiReply::MemberBots(..)
                | ApiReply::MemberBotCreated(..)
                | ApiReply::MemberBotUpdated(..)
                | ApiReply::MemberMessages(..) => Ok(false),
                reply => Err(Box::new(reply)),
            };
        };
        let ended = |error: &crate::wire::CommandError| error.is_unauthenticated();
        match reply {
            ApiReply::Accounts(result) => {
                admin.accounts_loading = false;
                match result {
                    Ok(list) => {
                        admin.accounts = Some(list.users);
                        admin.accounts_error = None;
                    }
                    Err(error) if ended(&error) => return Ok(self.session_ended_now()),
                    Err(error) => admin.accounts_error = Some(error.message),
                }
            }
            ApiReply::Invitations(result) => {
                admin.invitations_loading = false;
                match result {
                    Ok(list) => {
                        admin.invitations = Some(list.invitations);
                        admin.invitations_error = None;
                    }
                    Err(error) if ended(&error) => return Ok(self.session_ended_now()),
                    Err(error) => admin.invitations_error = Some(error.message),
                }
            }
            ApiReply::Invited(result) if admin.invite.busy => {
                let form = &mut admin.invite;
                form.busy = false;
                match result {
                    Ok(invitation) => {
                        form.created = Some(invitation);
                        form.display_name.clear();
                        self.load_invitations();
                    }
                    Err(error) if ended(&error) => return Ok(self.session_ended_now()),
                    Err(error) => form.error = Some(error.message),
                }
            }
            ApiReply::InvitationWithdrawn(id, result) if admin.withdrawing == Some(id) => {
                admin.withdrawing = None;
                match result {
                    Ok(()) => {}
                    Err(error) if ended(&error) => return Ok(self.session_ended_now()),
                    Err(error) => admin.invitations_error = Some(error.message),
                }
                self.load_invitations();
            }
            ApiReply::AccountUpdated(id, result) => {
                let Some(screen) = admin.account.as_mut().filter(|s| s.id == id && s.saving) else {
                    return Ok(false);
                };
                screen.saving = false;
                let updated = match result {
                    Ok(updated) => updated,
                    Err(error) if ended(&error) => return Ok(self.session_ended_now()),
                    Err(error) => {
                        screen.error = Some(error.message);
                        return Ok(true);
                    }
                };
                screen.error = None;
                screen.show_limits(updated.limits);
                let was = admin
                    .accounts
                    .iter_mut()
                    .flatten()
                    .find(|account| account.user.id == id);
                // What the admin may see of the member's Bots follows
                // oversight.
                let oversight_changed = was.as_ref().map(|was| was.user.oversight)
                    != Some(updated.user.oversight)
                    || screen.bots.as_ref().map(|list| list.oversight)
                        != Some(updated.user.oversight);
                if !updated.user.oversight {
                    // Access ends at once: nothing read under it stays.
                    screen.end_oversight();
                }
                let member = updated.user.role == Role::Member;
                match was {
                    Some(was) => *was = updated,
                    None => admin.accounts.get_or_insert_default().push(updated),
                }
                self.send(ApiRequest::AccountUsage(id));
                if member && oversight_changed {
                    self.load_member_bots(id);
                }
            }
            ApiReply::PasswordReset(id, result) => {
                let Some(screen) = admin.account.as_mut().filter(|s| s.id == id && s.resetting)
                else {
                    return Ok(false);
                };
                screen.resetting = false;
                screen.reset_note = Some(match result {
                    Ok(()) => (
                        "Password reset. Every session of the account has ended".into(),
                        false,
                    ),
                    Err(error) if ended(&error) => return Ok(self.session_ended_now()),
                    Err(error) => (error.message, true),
                });
            }
            ApiReply::AccountUsage(id, result) => {
                let Some(screen) = admin.account.as_mut().filter(|s| s.id == id) else {
                    return Ok(false);
                };
                match result {
                    Ok(allowance) => {
                        screen.allowance = Some(allowance);
                        screen.allowance_error = None;
                    }
                    Err(error) if ended(&error) => return Ok(self.session_ended_now()),
                    Err(error) => screen.allowance_error = Some(error.message),
                }
            }
            ApiReply::MemberBots(id, result) => {
                let Some(screen) = admin.account.as_mut().filter(|s| s.id == id) else {
                    return Ok(false);
                };
                screen.bots_loading = false;
                match result {
                    Ok(list) => {
                        if !list.oversight {
                            screen.reading = None;
                        }
                        screen.bots = Some(list);
                        screen.bots_error = None;
                    }
                    Err(error) if ended(&error) => return Ok(self.session_ended_now()),
                    Err(error) => screen.bots_error = Some(error.message),
                }
            }
            ApiReply::MemberBotCreated(id, result) => {
                let Some(screen) = admin
                    .account
                    .as_mut()
                    .filter(|s| s.id == id && s.new_bot.busy)
                else {
                    return Ok(false);
                };
                screen.new_bot.busy = false;
                match result {
                    Ok(_) => {
                        screen.new_bot.name.clear();
                        self.load_member_bots(id);
                    }
                    Err(error) if ended(&error) => return Ok(self.session_ended_now()),
                    Err(error) => screen.new_bot.error = Some(error.message),
                }
            }
            ApiReply::MemberBotUpdated(id, bot, result) => {
                let Some(screen) = admin.account.as_mut().filter(|s| s.id == id) else {
                    return Ok(false);
                };
                let Some((_, editor)) = screen
                    .editing
                    .as_mut()
                    .filter(|(editing, editor)| *editing == bot && editor.busy)
                else {
                    return Ok(false);
                };
                editor.busy = false;
                match result {
                    Ok(_) => {
                        screen.editing = None;
                        self.load_member_bots(id);
                    }
                    Err(error) if ended(&error) => return Ok(self.session_ended_now()),
                    Err(error) => editor.error = Some(error.message),
                }
            }
            ApiReply::MemberMessages(id, bot, before, result) => {
                let Some(screen) = admin.account.as_mut().filter(|s| s.id == id) else {
                    return Ok(false);
                };
                let Some(reading) = screen
                    .reading
                    .as_mut()
                    .filter(|r| r.bot == bot && r.loading == Some(before))
                else {
                    return Ok(false);
                };
                reading.loading = None;
                match result {
                    Ok(page) => {
                        reading.error = None;
                        if before.is_none() {
                            // The newest page starts the copy afresh.
                            reading.messages.clear();
                        }
                        reading.older = page.older;
                        reading.loaded = true;
                        reading.messages.extend(
                            page.messages
                                .into_iter()
                                .filter(|message| message.bot_id == bot)
                                .map(|message| (message.id, message)),
                        );
                    }
                    Err(error) if ended(&error) => return Ok(self.session_ended_now()),
                    // Oversight went off, here or elsewhere: what was read
                    // under it goes at once, and the Bots shown are read
                    // again for what may still be seen.
                    Err(error) if error.code == ErrorCode::Forbidden => {
                        screen.end_oversight();
                        screen.error = Some(error.message);
                        self.load_accounts();
                        self.load_member_bots(id);
                    }
                    Err(error) => reading.error = Some(error.message),
                }
            }
            // A reply for something no longer under way.
            ApiReply::Invited(_) | ApiReply::InvitationWithdrawn(..) => return Ok(false),
            reply => return Err(Box::new(reply)),
        }
        Ok(true)
    }

    /// Whether an admin action is available, or `None` for any other
    /// action.
    pub(crate) fn admin_availability(&self, action: BokkieAction) -> Option<Option<&'static str>> {
        let signed_in = match &self.session {
            Session::SignedIn(signed_in) => Some(signed_in),
            _ => None,
        };
        let admin = signed_in.and_then(|s| s.admin.as_ref());
        let account = signed_in.and_then(|s| s.admin_account());
        let me = signed_in.map(|s| s.user.id);
        Some(match action {
            BokkieAction::Admin => match signed_in {
                Some(s) if s.user.role != Role::Admin => Some("Only an admin manages accounts"),
                Some(s) if s.open.is_some() || s.household.is_some() || s.admin.is_some() => {
                    Some("Return to the list of Bots first")
                }
                _ => None,
            },
            BokkieAction::OpenAccount => match admin {
                None => Some("Open the accounts first"),
                Some(admin) if admin.account.is_some() => Some("Return to the accounts first"),
                Some(admin) if admin.accounts.is_none() => Some("Loading the accounts"),
                Some(_) => None,
            },
            BokkieAction::InviteAdmin
            | BokkieAction::InviteMayCreateBots
            | BokkieAction::InviteMayEditProfiles => match admin {
                None => Some("Open the accounts first"),
                Some(admin) if admin.account.is_some() => Some("Return to the accounts first"),
                Some(admin) if admin.invite.busy => Some("Creating the invitation"),
                // An admin always has both permissions.
                Some(admin) if admin.invite.admin && action != BokkieAction::InviteAdmin => {
                    Some("An admin always has both permissions")
                }
                Some(_) => None,
            },
            BokkieAction::CreateInvitation => match admin {
                None => Some("Open the accounts first"),
                Some(admin) if admin.account.is_some() => Some("Return to the accounts first"),
                Some(admin) if admin.invite.busy => Some("Creating the invitation"),
                Some(_) => None,
            },
            BokkieAction::HideInvitationCode => match admin {
                Some(admin) if admin.invite.created.is_some() => None,
                _ => Some("No code is shown"),
            },
            BokkieAction::WithdrawInvitation => match admin {
                None => Some("Open the accounts first"),
                Some(admin) if admin.withdrawing.is_some() => Some("Withdrawing an invitation"),
                Some(admin) if admin.invitations.is_none() => Some("Loading the invitations"),
                Some(_) => None,
            },
            BokkieAction::MayCreateBots
            | BokkieAction::MayEditProfiles
            | BokkieAction::Oversight => match account {
                None => Some("Open an account first"),
                Some((account, _)) if account.user.role == Role::Admin => Some(
                    "An admin always has both permissions, and oversight never applies to an admin",
                ),
                Some((_, screen)) if screen.saving => Some("Saving the account"),
                Some(_) => None,
            },
            BokkieAction::Disabled => match account {
                None => Some("Open an account first"),
                Some((account, _)) if Some(account.user.id) == me => {
                    Some("You cannot disable your own account")
                }
                Some((_, screen)) if screen.saving => Some("Saving the account"),
                Some(_) => None,
            },
            BokkieAction::SaveLimits => match account {
                None => Some("Open an account first"),
                Some((_, screen)) if screen.saving => Some("Saving the account"),
                Some((_, screen)) => screen.limits().err(),
            },
            BokkieAction::ResetPassword => match account {
                None => Some("Open an account first"),
                Some((account, _))
                    if account.user.role == Role::Admin && Some(account.user.id) != me =>
                {
                    Some("No admin can reset another admin's password")
                }
                Some((_, screen)) if screen.resetting => Some("Resetting the password"),
                Some((_, screen)) if screen.new_password.chars().count() < MIN_PASSWORD_CHARS => {
                    Some("Enter a new password of at least 10 characters")
                }
                Some(_) => None,
            },
            BokkieAction::MakeMemberBot => match account {
                None => Some("Open an account first"),
                Some((account, _)) if account.user.role == Role::Admin => {
                    Some("An admin's Bots are their own")
                }
                Some((_, screen)) if screen.new_bot.busy => Some("Making the Bot"),
                Some((_, screen)) if screen.new_bot.name.trim().is_empty() => Some("Enter a name"),
                Some(_) => None,
            },
            BokkieAction::EditMemberBot => match account {
                None => Some("Open an account first"),
                Some((_, screen)) if screen.editing.is_some() => Some("Already editing a Bot"),
                Some((_, screen)) if screen.bots.is_none() => Some("Loading the Bots"),
                Some(_) => None,
            },
            BokkieAction::SaveMemberBot => match account.and_then(|(_, s)| s.editing.as_ref()) {
                None => Some("No Bot is being edited"),
                Some((_, editor)) if editor.busy => Some("Saving the Bot"),
                Some((_, editor)) if editor.name.trim().is_empty() => Some("Enter a name"),
                Some(_) => None,
            },
            BokkieAction::CancelMemberBot => match account.and_then(|(_, s)| s.editing.as_ref()) {
                None => Some("No Bot is being edited"),
                Some((_, editor)) if editor.busy => Some("Saving the Bot"),
                Some(_) => None,
            },
            BokkieAction::ReadConversation => match account {
                None => Some("Open an account first"),
                Some((_, screen)) if screen.reading.is_some() => Some("Already reading"),
                Some((_, screen)) if !screen.bots.as_ref().is_some_and(|list| list.oversight) => {
                    Some("Only while oversight of this member is on")
                }
                Some(_) => None,
            },
            BokkieAction::LoadEarlier => match account.and_then(|(_, s)| s.reading.as_ref()) {
                None => Some("No conversation is being read"),
                Some(reading) if reading.loading.is_some() => Some("Loading"),
                Some(reading) if reading.older.is_none() => Some("No earlier messages"),
                Some(_) => None,
            },
            _ => return None,
        })
    }

    /// Perform an admin action without an argument, or `None` for any other
    /// action.
    pub(crate) fn admin_perform(&mut self, action: BokkieAction) -> Option<Result<bool, String>> {
        let Session::SignedIn(signed_in) = &mut self.session else {
            return None;
        };
        if action == BokkieAction::Admin {
            signed_in.admin = Some(Admin::default());
            self.load_accounts();
            self.load_invitations();
            return Some(Ok(true));
        }
        let admin = signed_in.admin.as_mut()?;
        let screen = admin.account.as_mut();
        let account = screen.as_ref().and_then(|screen| {
            admin
                .accounts
                .iter()
                .flatten()
                .find(|account| account.user.id == screen.id)
                .cloned()
        });
        match action {
            BokkieAction::InviteAdmin => admin.invite.admin = !admin.invite.admin,
            BokkieAction::InviteMayCreateBots => {
                let permissions = &mut admin.invite.permissions;
                permissions.may_create_bots = !permissions.may_create_bots;
            }
            BokkieAction::InviteMayEditProfiles => {
                let permissions = &mut admin.invite.permissions;
                permissions.may_edit_bot_profiles = !permissions.may_edit_bot_profiles;
            }
            BokkieAction::CreateInvitation => {
                let form = &mut admin.invite;
                form.busy = true;
                form.error = None;
                form.created = None;
                let name = form.display_name.trim();
                let request = CreateInvitationRequest {
                    role: if form.admin {
                        Role::Admin
                    } else {
                        Role::Member
                    },
                    // An admin always has both.
                    permissions: if form.admin {
                        Permissions::default()
                    } else {
                        form.permissions
                    },
                    display_name: (!name.is_empty()).then(|| name.to_owned()),
                    expires_in_hours: None,
                };
                self.send(ApiRequest::CreateInvitation(request));
            }
            BokkieAction::HideInvitationCode => admin.invite.created = None,
            BokkieAction::MayCreateBots
            | BokkieAction::MayEditProfiles
            | BokkieAction::Oversight
            | BokkieAction::Disabled
            | BokkieAction::SaveLimits => {
                let (Some(screen), Some(account)) = (screen, account) else {
                    return Some(Ok(false));
                };
                let user = &account.user;
                let mut permissions = user.permissions;
                let changes = match action {
                    BokkieAction::MayCreateBots => {
                        permissions.may_create_bots = !permissions.may_create_bots;
                        UpdateUserRequest {
                            permissions: Some(permissions),
                            ..Default::default()
                        }
                    }
                    BokkieAction::MayEditProfiles => {
                        permissions.may_edit_bot_profiles = !permissions.may_edit_bot_profiles;
                        UpdateUserRequest {
                            permissions: Some(permissions),
                            ..Default::default()
                        }
                    }
                    BokkieAction::Oversight => UpdateUserRequest {
                        oversight: Some(!user.oversight),
                        ..Default::default()
                    },
                    BokkieAction::Disabled => UpdateUserRequest {
                        disabled: Some(!user.disabled),
                        ..Default::default()
                    },
                    _ => match screen.limits() {
                        Ok(limits) => UpdateUserRequest {
                            limits: Some(limits),
                            ..Default::default()
                        },
                        Err(reason) => return Some(Err(reason.into())),
                    },
                };
                screen.saving = true;
                screen.error = None;
                let id = screen.id;
                self.send(ApiRequest::UpdateAccount(id, changes));
            }
            BokkieAction::ResetPassword => {
                let Some(screen) = screen else {
                    return Some(Ok(false));
                };
                screen.resetting = true;
                screen.reset_note = None;
                // The password leaves the form as soon as it is sent.
                let reset = ResetPasswordRequest {
                    new_password: std::mem::take(&mut screen.new_password),
                };
                let id = screen.id;
                self.send(ApiRequest::ResetPassword(id, reset));
            }
            BokkieAction::MakeMemberBot => {
                let Some(screen) = screen else {
                    return Some(Ok(false));
                };
                screen.new_bot.busy = true;
                screen.new_bot.error = None;
                let request = CreateBotRequest {
                    name: screen.new_bot.name.trim().to_owned(),
                    label: String::new(),
                    description: String::new(),
                    avatar: None,
                };
                let id = screen.id;
                self.send(ApiRequest::CreateMemberBot(id, request));
            }
            BokkieAction::SaveMemberBot => {
                let Some(screen) = screen else {
                    return Some(Ok(false));
                };
                let Some((bot, editor)) = screen.editing.as_mut() else {
                    return Some(Ok(false));
                };
                let held = screen
                    .bots
                    .iter()
                    .flat_map(|list| &list.bots)
                    .find(|member_bot| member_bot.id == *bot);
                let changed = |edited: &str, held: Option<&str>| {
                    (Some(edited) != held).then(|| edited.to_owned())
                };
                let changes = UpdateBotProfileRequest {
                    name: changed(editor.name.trim(), held.map(|b| b.name.as_str())),
                    label: changed(editor.label.trim(), held.map(|b| b.label.as_str())),
                    description: changed(&editor.description, held.map(|b| b.description.as_str())),
                    ..Default::default()
                };
                if changes == UpdateBotProfileRequest::default() {
                    screen.editing = None;
                    return Some(Ok(true));
                }
                editor.busy = true;
                editor.error = None;
                let (id, bot) = (screen.id, *bot);
                self.send(ApiRequest::UpdateMemberBot(id, bot, changes));
            }
            BokkieAction::CancelMemberBot => {
                if let Some(screen) = screen {
                    screen.editing = None;
                }
            }
            BokkieAction::LoadEarlier => {
                let Some(screen) = screen else {
                    return Some(Ok(false));
                };
                let Some(reading) = screen.reading.as_mut() else {
                    return Some(Ok(false));
                };
                let Some(before) = reading.older else {
                    return Some(Ok(false));
                };
                reading.loading = Some(Some(before));
                reading.error = None;
                let (id, bot) = (screen.id, reading.bot);
                self.send(ApiRequest::MemberMessages(id, bot, Some(before)));
            }
            _ => return None,
        }
        Some(Ok(true))
    }

    /// Whether the admin section lists the row an admin action with an ID
    /// names: an account, an invitation, or a member's Bot that an admin
    /// made (to edit) or that oversight lets an admin read.
    pub(crate) fn admin_lists(&self, action: BokkieAction, id: i64) -> bool {
        let Session::SignedIn(signed_in) = &self.session else {
            return false;
        };
        let Some(admin) = &signed_in.admin else {
            return false;
        };
        let member_bots = || {
            admin
                .account
                .iter()
                .flat_map(|screen| &screen.bots)
                .flat_map(|list| list.bots.iter().map(move |bot| (list, bot)))
                .filter(move |(_, bot)| bot.id.0 == id)
        };
        match action {
            BokkieAction::OpenAccount => admin
                .accounts
                .iter()
                .flatten()
                .any(|account| account.user.id.0 == id),
            BokkieAction::WithdrawInvitation => admin
                .invitations
                .iter()
                .flatten()
                .any(|invitation| invitation.id == id),
            BokkieAction::EditMemberBot => member_bots().any(|(_, bot)| bot.made_by_admin),
            BokkieAction::ReadConversation => member_bots().any(|(list, _)| list.oversight),
            _ => false,
        }
    }

    /// Perform an admin action that names an account, an invitation or a
    /// Bot, or `None` for any other action.
    pub(crate) fn admin_perform_with(
        &mut self,
        action: BokkieAction,
        id: i64,
    ) -> Option<Result<bool, String>> {
        let Session::SignedIn(signed_in) = &mut self.session else {
            return None;
        };
        let admin = signed_in.admin.as_mut()?;
        match action {
            BokkieAction::OpenAccount => {
                let Some(account) = admin
                    .accounts
                    .iter()
                    .flatten()
                    .find(|account| account.user.id.0 == id)
                else {
                    return Some(Err(format!("There is no account {id}")));
                };
                let member = account.user.role == Role::Member;
                admin.account = Some(AccountScreen::new(account));
                self.load_account(UserId(id), member);
            }
            BokkieAction::WithdrawInvitation => {
                if !admin.invitations.iter().flatten().any(|i| i.id == id) {
                    return Some(Err(format!("There is no invitation {id}")));
                }
                admin.withdrawing = Some(id);
                admin.invitations_error = None;
                self.send(ApiRequest::WithdrawInvitation(id));
            }
            BokkieAction::EditMemberBot | BokkieAction::ReadConversation => {
                let screen = admin.account.as_mut()?;
                let Some(bot) = screen
                    .bots
                    .iter()
                    .flat_map(|list| &list.bots)
                    .find(|bot| bot.id.0 == id)
                else {
                    return Some(Err(format!("There is no Bot {id} to see")));
                };
                if action == BokkieAction::EditMemberBot {
                    if !bot.made_by_admin {
                        return Some(Err("Only Bots an admin made can be edited here".into()));
                    }
                    screen.editing = Some((
                        bot.id,
                        ProfileEditor {
                            name: bot.name.clone(),
                            label: bot.label.clone(),
                            description: bot.description.clone(),
                            ..ProfileEditor::default()
                        },
                    ));
                } else {
                    screen.reading = Some(Reading {
                        bot: bot.id,
                        messages: BTreeMap::new(),
                        older: None,
                        loaded: false,
                        loading: Some(None),
                        error: None,
                    });
                    let user = screen.id;
                    self.send(ApiRequest::MemberMessages(user, BotId(id), None));
                }
            }
            _ => return None,
        }
        Some(Ok(true))
    }

    /// The admin section's Refresh: what the screen shows, loaded again.
    pub(crate) fn admin_refresh(&mut self) -> bool {
        let Some(admin) = self.admin_mut() else {
            return false;
        };
        match admin.account.as_mut() {
            None => {
                self.load_accounts();
                self.load_invitations();
            }
            Some(screen) => {
                let id = screen.id;
                if let Some(reading) = screen.reading.as_mut() {
                    reading.loading = Some(None);
                    reading.error = None;
                    let bot = reading.bot;
                    self.send(ApiRequest::MemberMessages(id, bot, None));
                    return true;
                }
                let member = self
                    .admin_mut()
                    .and_then(|admin| admin.accounts.as_ref())
                    .and_then(|accounts| accounts.iter().find(|a| a.user.id == id))
                    .is_some_and(|account| account.user.role == Role::Member);
                self.load_accounts();
                self.load_account(id, member);
            }
        }
        true
    }

    /// The admin section's Back: from a conversation to the account, from
    /// the account to the accounts, and from there out. Returns whether it
    /// handled it.
    pub(crate) fn admin_back(&mut self) -> bool {
        let Session::SignedIn(signed_in) = &mut self.session else {
            return false;
        };
        let Some(admin) = signed_in.admin.as_mut() else {
            return false;
        };
        match admin.account.as_mut() {
            Some(screen) if screen.reading.is_some() => screen.reading = None,
            Some(_) => {
                admin.account = None;
                // What changed there shows on the list.
                self.load_accounts();
            }
            None => signed_in.admin = None,
        }
        true
    }

    /// The screen the admin section shows, if open.
    pub(crate) fn admin_screen(&self) -> Option<&'static str> {
        let Session::SignedIn(signed_in) = &self.session else {
            return None;
        };
        let admin = signed_in.admin.as_ref()?;
        Some(match &admin.account {
            None => "admin",
            Some(screen) if screen.reading.is_some() => "overseen",
            Some(_) => "account",
        })
    }

    /// The facts the admin section publishes. Never an invitation code or a
    /// password.
    pub(crate) fn observe_admin(&self, observation: &mut stobie_ui_egui::Observation<'_>) {
        let signed_in = match &self.session {
            Session::SignedIn(signed_in) => Some(signed_in),
            _ => None,
        };
        let admin = signed_in.and_then(|s| s.admin.as_ref());
        let account = signed_in.and_then(|s| s.admin_account());
        let screen = admin.and_then(|a| a.account.as_ref());
        observation.fact(
            "accounts",
            admin
                .and_then(|a| a.accounts.as_ref())
                .map_or(0, |accounts| accounts.len()),
        );
        observation.fact(
            "invitations",
            admin
                .and_then(|a| a.invitations.as_ref())
                .map_or(0, |invitations| invitations.len()),
        );
        observation.fact("inviting", admin.is_some_and(|a| a.invite.busy));
        observation.fact(
            "invitation_shown",
            admin.is_some_and(|a| a.invite.created.is_some()),
        );
        observation.fact(
            "invite_error",
            crate::app::bounded(admin.and_then(|a| a.invite.error.as_deref()).unwrap_or("")),
        );
        observation.fact("account", screen.map_or(0, |s| s.id.0));
        let user = account.map(|(account, _)| &account.user);
        observation.fact(
            "account_role",
            match user.map(|u| u.role) {
                None => "",
                Some(Role::Admin) => "admin",
                Some(Role::Member) => "member",
            },
        );
        observation.fact(
            "account_may_create_bots",
            user.is_some_and(|u| u.permissions.may_create_bots),
        );
        observation.fact(
            "account_may_edit_profiles",
            user.is_some_and(|u| u.permissions.may_edit_bot_profiles),
        );
        observation.fact("account_oversight", user.is_some_and(|u| u.oversight));
        observation.fact("account_disabled", user.is_some_and(|u| u.disabled));
        observation.fact(
            "account_daily_turns",
            account
                .and_then(|(a, _)| a.limits.daily_turns)
                .map_or(-1, i64::from),
        );
        observation.fact("account_saving", screen.is_some_and(|s| s.saving));
        observation.fact(
            "account_error",
            crate::app::bounded(screen.and_then(|s| s.error.as_deref()).unwrap_or("")),
        );
        observation.fact(
            "reset",
            match screen {
                Some(s) if s.resetting => "resetting",
                Some(AccountScreen {
                    reset_note: Some((_, false)),
                    ..
                }) => "done",
                Some(AccountScreen {
                    reset_note: Some((_, true)),
                    ..
                }) => "refused",
                _ => "",
            },
        );
        observation.fact(
            "member_bots",
            screen
                .and_then(|s| s.bots.as_ref())
                .map_or(0, |list| list.bots.len()),
        );
        let reading = screen.and_then(|s| s.reading.as_ref());
        observation.fact("reading", reading.map_or(0, |r| r.bot.0));
        observation.fact("reading_count", reading.map_or(0, |r| r.messages.len()));
    }
}

impl App {
    /// End the session from inside a reply handler, reporting a change.
    fn session_ended_now(&mut self) -> bool {
        self.session_ended();
        true
    }
}

/// Apply an edit to an admin form field. Returns whether a form took it.
pub(crate) fn edit(signed_in: &mut SignedIn, field: Field, text: String) -> bool {
    let Some(admin) = signed_in.admin.as_mut() else {
        return false;
    };
    match (field, admin.account.as_mut()) {
        (Field::InviteName, None) if !admin.invite.busy => admin.invite.display_name = text,
        (Field::DailyTurns, Some(screen)) if !screen.saving => screen.daily_turns = text,
        (Field::WeeklyTokens, Some(screen)) if !screen.saving => screen.weekly_tokens = text,
        (Field::NewPassword, Some(screen)) if !screen.resetting => screen.new_password = text,
        (Field::MemberBotName, Some(screen)) if !screen.new_bot.busy => screen.new_bot.name = text,
        _ => return false,
    }
    true
}

/// An admin form's text field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Field {
    InviteName,
    DailyTurns,
    WeeklyTokens,
    NewPassword,
    MemberBotName,
}

/// The profile editor of the admin-made Bot being edited, if any.
pub(crate) fn editor(signed_in: &mut SignedIn) -> Option<&mut ProfileEditor> {
    signed_in
        .admin
        .as_mut()?
        .account
        .as_mut()?
        .editing
        .as_mut()
        .map(|(_, editor)| editor)
}
