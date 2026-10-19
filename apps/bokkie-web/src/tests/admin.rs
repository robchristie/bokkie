//! The admin section, against scripted replies: accounts, invitations with
//! role and permissions, each account's permissions, oversight, disabling,
//! limits, password and allowance, a member's Bots, and the read-only view
//! of a member's conversation. Also joining with an invitation.

use bokkie_protocol::api::{
    AcceptInvitationRequest, AdminUser, AdminUserList, Allowance, CreateBotRequest,
    CreateInvitationRequest, CreatedInvitation, InvitationList, InvitationSummary, Limits,
    MemberBot, MemberBotList, Message, MessageKind, MessagePage, MessageState, Permissions,
    ResetPasswordRequest, UpdateBotProfileRequest, UpdateUserRequest,
};
use bokkie_protocol::{BotId, MessageId};

use super::*;
use crate::api::Invitation;

const NOW: i64 = 1_800_000_000_000;
const DAY: i64 = 86_400_000;
const CODE: &str = "qWv3Zy8sD1eXb0mNa7Kc4Lt2Rj9PfUo6HgIyTsEwQ5x";
const GRACE: UserId = UserId(2);
const ROSA: UserId = UserId(3);

fn admin_row(user: User, limits: Limits) -> AdminUser {
    AdminUser { user, limits }
}

/// Ada (the admin signed in), Grace (a member) and Rosa (another admin).
fn accounts() -> Vec<AdminUser> {
    vec![
        admin_row(user(), Limits::default()),
        admin_row(
            member(),
            Limits {
                daily_turns: Some(20),
                weekly_tokens: None,
            },
        ),
        admin_row(
            User {
                id: ROSA,
                username: "rosa".into(),
                display_name: "Rosa".into(),
                ..user()
            },
            Limits::default(),
        ),
    ]
}

fn grace_with(change: impl FnOnce(&mut User)) -> AdminUser {
    let mut grace = accounts().remove(1);
    change(&mut grace.user);
    grace
}

fn allowance(turns: u32) -> Allowance {
    Allowance {
        limits: Limits {
            daily_turns: Some(20),
            weekly_tokens: None,
        },
        turns_today: turns,
        tokens_this_week: 1_234,
        exhausted: false,
    }
}

fn member_bot(id: i64, name: &str, made_by_admin: bool, overseen: bool) -> MemberBot {
    MemberBot {
        id: BotId(id),
        name: name.into(),
        label: "Helps".into(),
        description: "Be kind.".into(),
        avatar: None,
        made_by_admin,
        created_at: Timestamp(1_000),
        grants: Vec::new(),
        overseen: overseen.then(|| bot(id, name, "Helps", BotStatus::Idle, Some("Last words"))),
    }
}

fn invitation(id: i64, name: Option<&str>) -> InvitationSummary {
    InvitationSummary {
        id,
        role: Role::Member,
        permissions: Permissions::default(),
        display_name: name.map(Into::into),
        created_at: Timestamp(NOW),
        expires_at: Timestamp(NOW + 3 * DAY),
    }
}

fn read_message(id: i64, author: Author, text: &str) -> Message {
    Message {
        id: MessageId(id),
        bot_id: BotId(7),
        author,
        text: text.into(),
        state: MessageState::Complete,
        created_at: Timestamp(3_000 + id),
        updated_at: Timestamp(3_000 + id),
        kind: MessageKind::Text,
    }
}

impl Fixture {
    /// Perform an available action as its control would, wherever the
    /// control has scrolled: a control out of view is not in the
    /// accessibility tree.
    fn act(&mut self, action: BokkieAction) {
        assert!(self.enabled(action), "{action:?}");
        let target = stobie_ui_egui::ActionTarget::application(action);
        self.harness
            .state_mut()
            .app_mut()
            .perform(target, &mut ())
            .unwrap();
        self.harness.run();
    }

    /// The same for an action on one row, such as Read on a Bot's.
    fn act_with(&mut self, action: BokkieAction, id: i64) {
        assert!(self.enabled(action), "{action:?}");
        let target = stobie_ui_egui::ActionTarget::application(action);
        let argument = stobie_ui_egui::ActionArgument::Number(id as f64);
        self.harness
            .state_mut()
            .app_mut()
            .perform_with(target, argument, &mut ())
            .unwrap();
        self.harness.run();
    }

    /// Ada, live, with the accounts open and loaded.
    fn accounts_open(&mut self) {
        self.accounts_open_with(false);
    }

    /// The same, with oversight of Grace on or off.
    fn accounts_open_with(&mut self, oversight: bool) {
        self.sent.set_now(Timestamp(NOW));
        self.signed_in_and_live(10);
        self.click("Accounts");
        assert_eq!(self.fact("screen"), "admin".into());
        assert_eq!(
            self.sent(),
            [ApiRequest::ListAccounts, ApiRequest::ListInvitations]
        );
        let mut users = accounts();
        users[1].user.oversight = oversight;
        self.reply(ApiReply::Accounts(Ok(AdminUserList { users })));
        self.reply(ApiReply::Invitations(Ok(InvitationList {
            invitations: vec![invitation(4, Some("Tom"))],
        })));
    }

    /// Grace's account open, with her allowance and Bots loaded.
    fn grace_open(&mut self, oversight: bool, bots: Vec<MemberBot>) {
        self.accounts_open_with(oversight);
        self.click("Open Grace");
        assert_eq!(self.fact("screen"), "account".into());
        assert_eq!(
            self.sent(),
            [
                ApiRequest::AccountUsage(GRACE),
                ApiRequest::MemberBots(GRACE)
            ]
        );
        self.reply(ApiReply::AccountUsage(GRACE, Ok(allowance(3))));
        self.reply(ApiReply::MemberBots(
            GRACE,
            Ok(MemberBotList { oversight, bots }),
        ));
    }

    /// The account's change, sent and answered with the account as it now is.
    fn saved(&mut self, change: UpdateUserRequest, now: AdminUser) {
        assert_eq!(self.sent()[0], ApiRequest::UpdateAccount(GRACE, change));
        assert_eq!(self.fact("account_saving"), true.into());
        self.reply(ApiReply::AccountUpdated(GRACE, Ok(now)));
        assert_eq!(self.fact("account_saving"), false.into());
    }
}

#[test]
fn only_an_admin_has_the_accounts() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live_as(session(member()), 10);
    assert!(!fixture.shows("Accounts"));
    assert!(!fixture.enabled(BokkieAction::Admin));
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    assert!(fixture.shows("Accounts"));
    assert!(fixture.enabled(BokkieAction::Admin));
}

#[test]
fn the_accounts_list_every_account_and_the_invitations() {
    let mut fixture = Fixture::new();
    fixture.accounts_open();
    assert_eq!(fixture.fact("accounts"), 3_u64.into());
    for text in [
        "Ada",
        "ada · Admin",
        "Grace",
        "grace · Member",
        "Open Grace",
        "Open Rosa",
        "Member invitation for Tom, expires in 3 days",
        "Withdraw invitation 4",
    ] {
        assert!(fixture.shows(text), "{text}");
    }
    // A failed load says why, and Refresh loads both again.
    fixture.click("Refresh");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::ListAccounts, ApiRequest::ListInvitations]
    );
    let down = "Bokkie's server is not available. Try again shortly";
    fixture.reply(ApiReply::Accounts(Err(error(
        502,
        ErrorCode::Unavailable,
        down,
    ))));
    assert!(fixture.shows(down));
    // Back leaves the section.
    fixture.click("Back");
    assert_eq!(fixture.fact("screen"), "signed_in".into());
}

#[test]
fn an_invitation_carries_its_role_and_permissions_and_its_code_shows_once() {
    let mut fixture = Fixture::new();
    fixture.accounts_open();
    // A member who may create Bots but not edit profiles.
    fixture.click("New account may edit Bot profiles");
    fixture.send(BokkieIntent::EditInviteName(" Grace ".into()));
    fixture.click("Create invitation");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::CreateInvitation(CreateInvitationRequest {
            role: Role::Member,
            permissions: Permissions {
                may_create_bots: true,
                may_edit_bot_profiles: false,
            },
            display_name: Some("Grace".into()),
            expires_in_hours: None,
        })]
    );
    assert_eq!(fixture.fact("inviting"), true.into());
    assert!(!fixture.enabled(BokkieAction::CreateInvitation));
    fixture.reply(ApiReply::Invited(Ok(Invitation(CreatedInvitation {
        id: 5,
        code: CODE.into(),
        role: Role::Member,
        expires_at: Timestamp(NOW + 3 * DAY),
    }))));
    assert_eq!(fixture.fact("invitation_shown"), true.into());
    assert!(fixture.shows(CODE));
    assert!(fixture.shows("Give this code to the new account's owner. It is shown only now."));
    assert!(fixture.shows("It expires in 3 days"));
    // The list is loaded again.
    assert_eq!(fixture.sent(), [ApiRequest::ListInvitations]);
    // The code is on screen and nowhere else.
    let snapshot = serde_json::to_string(fixture.host().ui_snapshot()).unwrap();
    assert!(!snapshot.contains(CODE));
    let inspection = fixture.host().inspection().unwrap();
    let observed = serde_json::to_string(&inspection.handle(InspectionRequest {
        version: 1,
        request_id: "observe".into(),
        instance: Some(inspection.instance()),
        operation: InspectionOperation::Observe,
    }))
    .unwrap();
    assert!(!observed.contains(CODE));
    assert!(!format!("{:?}", fixture.app().session).contains(CODE));
    fixture.click("Hide the code");
    assert!(!fixture.shows(CODE));
    assert_eq!(fixture.fact("invitation_shown"), false.into());

    // An admin's invitation always carries both permissions.
    fixture.click("New account is an admin");
    assert!(!fixture.shows("New account may create Bots"));
    assert!(!fixture.enabled(BokkieAction::InviteMayCreateBots));
    fixture.click("Create invitation");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::CreateInvitation(CreateInvitationRequest {
            role: Role::Admin,
            permissions: Permissions::default(),
            display_name: None,
            expires_in_hours: None,
        })]
    );
    let refused = "That invitation could not be made";
    fixture.reply(ApiReply::Invited(Err(error(
        400,
        ErrorCode::InvalidRequest,
        refused,
    ))));
    assert_eq!(fixture.fact("invite_error"), refused.into());
    assert!(fixture.shows(refused));
}

#[test]
fn an_invitation_can_be_withdrawn() {
    let mut fixture = Fixture::new();
    fixture.accounts_open();
    fixture.click("Withdraw invitation 4");
    assert_eq!(fixture.sent(), [ApiRequest::WithdrawInvitation(4)]);
    assert!(fixture.shows("Withdrawing…"));
    assert!(!fixture.enabled(BokkieAction::WithdrawInvitation));
    fixture.reply(ApiReply::InvitationWithdrawn(4, Ok(())));
    assert_eq!(fixture.sent(), [ApiRequest::ListInvitations]);
    fixture.reply(ApiReply::Invitations(Ok(InvitationList {
        invitations: Vec::new(),
    })));
    assert!(fixture.shows("No invitations are waiting"));
}

#[test]
fn a_members_permissions_and_oversight_are_switched_one_at_a_time() {
    let mut fixture = Fixture::new();
    fixture.grace_open(false, Vec::new());
    assert_eq!(fixture.fact("account"), 2_i64.into());
    assert_eq!(fixture.fact("account_role"), "member".into());
    assert!(fixture.shows("Their turns today: 3 of 20"));
    fixture.click("May create Bots");
    // The permissions are sent whole.
    fixture.saved(
        UpdateUserRequest {
            permissions: Some(Permissions {
                may_create_bots: false,
                may_edit_bot_profiles: true,
            }),
            ..Default::default()
        },
        grace_with(|grace| grace.permissions.may_create_bots = false),
    );
    assert_eq!(fixture.fact("account_may_create_bots"), false.into());
    // Its allowance is read again.
    assert_eq!(fixture.sent(), [ApiRequest::AccountUsage(GRACE)]);
    fixture.click("May edit Bot profiles");
    fixture.saved(
        UpdateUserRequest {
            permissions: Some(Permissions {
                may_create_bots: false,
                may_edit_bot_profiles: false,
            }),
            ..Default::default()
        },
        grace_with(|grace| {
            grace.permissions = Permissions {
                may_create_bots: false,
                may_edit_bot_profiles: false,
            }
        }),
    );
    let _ = fixture.sent();
    fixture.click("Oversight");
    fixture.saved(
        UpdateUserRequest {
            oversight: Some(true),
            ..Default::default()
        },
        grace_with(|grace| grace.oversight = true),
    );
    assert_eq!(fixture.fact("account_oversight"), true.into());
    // Oversight changes what the admin may see of her Bots.
    assert_eq!(
        fixture.sent(),
        [
            ApiRequest::AccountUsage(GRACE),
            ApiRequest::MemberBots(GRACE)
        ]
    );
    // A refusal says why, and changes nothing.
    fixture.click("Account disabled");
    let refused = "That account cannot be changed";
    let _ = fixture.sent();
    fixture.reply(ApiReply::AccountUpdated(
        GRACE,
        Err(error(403, ErrorCode::Forbidden, refused)),
    ));
    assert_eq!(fixture.fact("account_error"), refused.into());
    assert!(fixture.shows(refused));
    assert_eq!(fixture.fact("account_disabled"), false.into());
    fixture.click("Account disabled");
    fixture.saved(
        UpdateUserRequest {
            disabled: Some(true),
            ..Default::default()
        },
        grace_with(|grace| grace.disabled = true),
    );
    assert_eq!(fixture.fact("account_disabled"), true.into());
    assert!(fixture.shows("grace · Member · Disabled"));
}

#[test]
fn limits_are_whole_numbers_or_empty_for_none() {
    let mut fixture = Fixture::new();
    fixture.grace_open(false, Vec::new());
    assert_eq!(fixture.fact("account_daily_turns"), 20_i64.into());
    fixture.send(BokkieIntent::EditDailyTurns("five".into()));
    assert!(!fixture.enabled(BokkieAction::SaveLimits));
    assert!(fixture.shows("Turns per day must be a whole number, or empty for no limit"));
    fixture.send(BokkieIntent::EditDailyTurns(" 1 ".into()));
    fixture.send(BokkieIntent::EditWeeklyTokens("50000".into()));
    fixture.click("Save limits");
    fixture.saved(
        UpdateUserRequest {
            limits: Some(Limits {
                daily_turns: Some(1),
                weekly_tokens: Some(50_000),
            }),
            ..Default::default()
        },
        AdminUser {
            limits: Limits {
                daily_turns: Some(1),
                weekly_tokens: Some(50_000),
            },
            ..accounts().remove(1)
        },
    );
    assert_eq!(fixture.fact("account_daily_turns"), 1_i64.into());
    assert_eq!(fixture.sent(), [ApiRequest::AccountUsage(GRACE)]);
    // Empty is no limit.
    fixture.send(BokkieIntent::EditDailyTurns(String::new()));
    fixture.send(BokkieIntent::EditWeeklyTokens(String::new()));
    fixture.click("Save limits");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::UpdateAccount(
            GRACE,
            UpdateUserRequest {
                limits: Some(Limits::default()),
                ..Default::default()
            }
        )]
    );
}

#[test]
fn a_members_password_is_reset_and_another_admins_is_refused() {
    let mut fixture = Fixture::new();
    fixture.grace_open(false, Vec::new());
    assert!(!fixture.enabled(BokkieAction::ResetPassword), "too short");
    fixture.send(BokkieIntent::EditNewPassword("a new long password".into()));
    fixture.click("Reset password");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::ResetPassword(
            GRACE,
            ResetPasswordRequest {
                new_password: "a new long password".into()
            }
        )]
    );
    assert_eq!(fixture.fact("reset"), "resetting".into());
    assert!(!format!("{:?}", fixture.app().session).contains("a new long password"));
    fixture.reply(ApiReply::PasswordReset(GRACE, Ok(())));
    assert_eq!(fixture.fact("reset"), "done".into());
    assert!(fixture.shows("Password reset. Every session of the account has ended"));
    // A refusal from the gateway shows as such.
    fixture.send(BokkieIntent::EditNewPassword(
        "another long password".into(),
    ));
    fixture.click("Reset password");
    let refused = "No admin can reset another admin's password";
    let _ = fixture.sent();
    fixture.reply(ApiReply::PasswordReset(
        GRACE,
        Err(error(403, ErrorCode::Forbidden, refused)),
    ));
    assert_eq!(fixture.fact("reset"), "refused".into());
    assert!(fixture.shows(refused));

    // Another admin's account offers no reset, and says why.
    let mut fixture = Fixture::new();
    fixture.accounts_open();
    fixture.click("Open Rosa");
    // An admin's Bots are their own: only the allowance loads.
    assert_eq!(fixture.sent(), [ApiRequest::AccountUsage(ROSA)]);
    assert_eq!(fixture.fact("account_role"), "admin".into());
    assert!(fixture.shows("No admin can reset another admin's password"));
    assert!(!fixture.shows("New password") && !fixture.shows("Reset password"));
    fixture.send(BokkieIntent::EditNewPassword("a new long password".into()));
    assert!(!fixture.enabled(BokkieAction::ResetPassword));
    // Nor permissions or oversight: an admin always has both.
    assert!(
        fixture.shows(
            "An admin always has both permissions, and oversight never applies to an admin."
        )
    );
    for action in [
        BokkieAction::MayCreateBots,
        BokkieAction::MayEditProfiles,
        BokkieAction::Oversight,
        BokkieAction::MakeMemberBot,
    ] {
        assert!(!fixture.enabled(action), "{action:?}");
    }
    assert!(!fixture.shows("Oversight") && !fixture.shows("Their Bots"));

    // An admin's own account resets, but cannot be disabled.
    let mut fixture = Fixture::new();
    fixture.accounts_open();
    fixture.click("Open Ada");
    assert!(fixture.shows("New password"));
    assert!(!fixture.shows("Account disabled"));
    assert!(!fixture.enabled(BokkieAction::Disabled));
}

#[test]
fn with_oversight_off_an_admin_sees_and_edits_only_the_bots_an_admin_made() {
    let mut fixture = Fixture::new();
    fixture.grace_open(false, vec![member_bot(7, "Coach", true, false)]);
    assert_eq!(fixture.fact("member_bots"), 1_u64.into());
    assert!(fixture.shows(
        "With oversight off you see only the Bots an admin made for them, and no conversation."
    ));
    assert!(fixture.shows("Helps · Made by an admin"));
    assert!(fixture.shows("Edit Coach"));
    assert!(!fixture.shows("Read Coach"));
    assert!(!fixture.enabled(BokkieAction::ReadConversation));
    // Edit sends only what changed.
    fixture.act_with(BokkieAction::EditMemberBot, 7);
    fixture.send(BokkieIntent::EditProfileLabel("Sports coach".into()));
    fixture.act(BokkieAction::SaveMemberBot);
    assert_eq!(
        fixture.sent(),
        [ApiRequest::UpdateMemberBot(
            GRACE,
            BotId(7),
            UpdateBotProfileRequest {
                label: Some("Sports coach".into()),
                ..Default::default()
            }
        )]
    );
    let refused = "That name is taken";
    fixture.reply(ApiReply::MemberBotUpdated(
        GRACE,
        BotId(7),
        Err(error(409, ErrorCode::Conflict, refused)),
    ));
    assert!(fixture.shows(refused));
    fixture.act(BokkieAction::SaveMemberBot);
    let _ = fixture.sent();
    fixture.reply(ApiReply::MemberBotUpdated(
        GRACE,
        BotId(7),
        Ok(member_bot(7, "Coach", true, false)),
    ));
    assert_eq!(fixture.sent(), [ApiRequest::MemberBots(GRACE)]);
    assert!(!fixture.shows("Save Bot"));
    // Make one for her.
    fixture.send(BokkieIntent::EditMemberBotName(" Reader ".into()));
    fixture.act(BokkieAction::MakeMemberBot);
    assert_eq!(
        fixture.sent(),
        [ApiRequest::CreateMemberBot(
            GRACE,
            CreateBotRequest {
                name: "Reader".into(),
                label: String::new(),
                description: String::new(),
                avatar: None,
            }
        )]
    );
    fixture.reply(ApiReply::MemberBotCreated(
        GRACE,
        Ok(member_bot(8, "Reader", true, false)),
    ));
    assert_eq!(fixture.sent(), [ApiRequest::MemberBots(GRACE)]);
}

#[test]
fn with_oversight_on_an_admin_reads_a_conversation_and_can_send_nothing() {
    let mut fixture = Fixture::new();
    fixture.grace_open(
        true,
        vec![
            member_bot(7, "Diary", false, true),
            member_bot(8, "Coach", true, true),
        ],
    );
    assert!(fixture.shows("Helps · Their own · Idle"));
    assert!(fixture.shows("Last words"));
    // Her own Bot cannot be edited; an admin-made one can.
    assert!(!fixture.shows("Edit Diary") && fixture.shows("Edit Coach"));
    fixture.act_with(BokkieAction::ReadConversation, 7);
    assert_eq!(fixture.fact("screen"), "overseen".into());
    assert_eq!(
        fixture.sent(),
        [ApiRequest::MemberMessages(GRACE, BotId(7), None)]
    );
    fixture.reply(ApiReply::MemberMessages(
        GRACE,
        BotId(7),
        None,
        Ok(MessagePage {
            messages: vec![
                read_message(11, Author::User, "Dear diary"),
                read_message(12, Author::Bot, "Noted."),
            ],
            older: Some(MessageId(11)),
            seq: Seq(40),
        }),
    ));
    assert_eq!(fixture.fact("reading"), 7_i64.into());
    assert_eq!(fixture.fact("reading_count"), 2_u64.into());
    assert!(
        fixture.shows(
            "Read-only, under oversight: Grace's conversation. Nothing can be sent from here"
        )
    );
    assert!(fixture.shows("Dear diary") && fixture.shows("Noted."));
    assert!(fixture.shows("Grace") && fixture.shows("Diary"));
    // Nothing to send, steer or stop with.
    for control in ["Message", "Send", "Stop"] {
        assert!(!fixture.shows(control), "{control}");
    }
    assert!(!fixture.enabled(BokkieAction::Send) && !fixture.enabled(BokkieAction::Stop));
    fixture.click("Load earlier");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::MemberMessages(
            GRACE,
            BotId(7),
            Some(MessageId(11))
        )]
    );
    fixture.reply(ApiReply::MemberMessages(
        GRACE,
        BotId(7),
        Some(MessageId(11)),
        Ok(MessagePage {
            messages: vec![read_message(10, Author::Bot, "Hello again.")],
            older: None,
            seq: Seq(40),
        }),
    ));
    assert_eq!(fixture.fact("reading_count"), 3_u64.into());
    assert!(!fixture.shows("Load earlier"));
    // Her own Bot cannot be edited by naming it either.
    let own = fixture.harness.state_mut().app_mut().perform_with(
        stobie_ui_egui::ActionTarget::application(BokkieAction::EditMemberBot),
        stobie_ui_egui::ActionArgument::Number(7.0),
        &mut (),
    );
    assert_eq!(
        own,
        Err("Only Bots an admin made can be edited here".into())
    );
    // Switching oversight off while reading ends the reading at once.
    fixture.act(BokkieAction::Oversight);
    assert_eq!(
        fixture.sent(),
        [ApiRequest::UpdateAccount(
            GRACE,
            UpdateUserRequest {
                oversight: Some(false),
                ..Default::default()
            }
        )]
    );
    fixture.reply(ApiReply::AccountUpdated(
        GRACE,
        Ok(grace_with(|grace| grace.oversight = false)),
    ));
    assert_eq!(fixture.fact("reading"), 0_i64.into());
    assert_eq!(fixture.fact("screen"), "account".into());
    assert!(!fixture.shows("Dear diary"));
    // Before the list is read again, it already shows only what an admin
    // may see without oversight.
    assert!(!fixture.shows("Diary") && !fixture.shows("Last words"));
    assert!(fixture.shows("Coach") && !fixture.shows("Read Coach"));
    assert!(!fixture.enabled(BokkieAction::ReadConversation));
    assert_eq!(
        fixture.sent(),
        [
            ApiRequest::AccountUsage(GRACE),
            ApiRequest::MemberBots(GRACE)
        ]
    );
    fixture.reply(ApiReply::MemberBots(
        GRACE,
        Ok(MemberBotList {
            oversight: false,
            bots: vec![member_bot(8, "Coach", true, false)],
        }),
    ));
    assert!(!fixture.shows("Read Coach") && !fixture.shows("Diary"));
}

#[test]
fn a_read_refused_because_oversight_went_off_elsewhere_closes_the_conversation() {
    let mut fixture = Fixture::new();
    fixture.grace_open(true, vec![member_bot(7, "Diary", false, true)]);
    fixture.act_with(BokkieAction::ReadConversation, 7);
    let _ = fixture.sent();
    fixture.reply(ApiReply::MemberMessages(
        GRACE,
        BotId(7),
        None,
        Ok(MessagePage {
            messages: vec![read_message(11, Author::User, "Dear diary")],
            older: None,
            seq: Seq(40),
        }),
    ));
    assert!(fixture.shows("Dear diary"));
    // Another admin switched oversight off: the next read is refused.
    fixture.click("Refresh");
    let _ = fixture.sent();
    let refused = "Oversight of this account is off";
    fixture.reply(ApiReply::MemberMessages(
        GRACE,
        BotId(7),
        None,
        Err(error(403, ErrorCode::Forbidden, refused)),
    ));
    assert_eq!(fixture.fact("screen"), "account".into());
    assert_eq!(fixture.fact("reading"), 0_i64.into());
    assert!(!fixture.shows("Dear diary"));
    assert!(fixture.shows(refused));
    assert!(!fixture.shows("Last words") && !fixture.enabled(BokkieAction::ReadConversation));
    // The account and what may still be seen of its Bots are read again.
    assert_eq!(
        fixture.sent(),
        [ApiRequest::ListAccounts, ApiRequest::MemberBots(GRACE)]
    );
    fixture.reply(ApiReply::MemberBots(
        GRACE,
        Ok(MemberBotList {
            oversight: false,
            bots: Vec::new(),
        }),
    ));
    assert!(!fixture.shows("Last words") && !fixture.shows("Read Diary"));

    // Another failure keeps the conversation, with why.
    let mut fixture = Fixture::new();
    fixture.grace_open(true, vec![member_bot(7, "Diary", false, true)]);
    fixture.act_with(BokkieAction::ReadConversation, 7);
    let _ = fixture.sent();
    let down = "Bokkie's server is not available. Try again shortly";
    fixture.reply(ApiReply::MemberMessages(
        GRACE,
        BotId(7),
        None,
        Err(error(502, ErrorCode::Unavailable, down)),
    ));
    assert_eq!(fixture.fact("screen"), "overseen".into());
    assert!(fixture.shows(down));
}

#[test]
fn oversight_switched_off_closes_a_conversation_being_read() {
    let mut fixture = Fixture::new();
    fixture.grace_open(true, vec![member_bot(7, "Diary", false, true)]);
    fixture.act_with(BokkieAction::ReadConversation, 7);
    let _ = fixture.sent();
    // The list read again says oversight is off: the reading closes.
    fixture.reply(ApiReply::MemberBots(
        GRACE,
        Ok(MemberBotList {
            oversight: false,
            bots: Vec::new(),
        }),
    ));
    assert_eq!(fixture.fact("screen"), "account".into());
    assert_eq!(fixture.fact("reading"), 0_i64.into());
}

#[test]
fn replies_for_another_account_or_after_leaving_are_dropped() {
    let mut fixture = Fixture::new();
    fixture.grace_open(false, Vec::new());
    fixture.reply(ApiReply::AccountUsage(ROSA, Ok(allowance(9))));
    assert!(fixture.shows("Their turns today: 3 of 20"));
    fixture.click("Back");
    assert_eq!(fixture.fact("screen"), "admin".into());
    assert_eq!(fixture.sent(), [ApiRequest::ListAccounts]);
    fixture.reply(ApiReply::MemberBots(
        GRACE,
        Ok(MemberBotList {
            oversight: true,
            bots: vec![member_bot(7, "Diary", false, true)],
        }),
    ));
    assert_eq!(fixture.fact("member_bots"), 0_u64.into());
    fixture.click("Back");
    assert_eq!(fixture.fact("screen"), "signed_in".into());
    fixture.reply(ApiReply::Accounts(Ok(AdminUserList { users: accounts() })));
    assert_eq!(fixture.fact("accounts"), 0_u64.into());
}

#[test]
fn an_invitation_is_accepted_from_the_sign_in_screen() {
    let mut fixture = Fixture::new();
    let _ = fixture.sent();
    fixture.reply(ApiReply::Session(Err(unauthenticated())));
    assert!(!fixture.enabled(BokkieAction::AcceptInvitation));
    fixture.click("I have an invitation");
    assert_eq!(fixture.fact("joining"), true.into());
    assert!(fixture.shows("Invitation code") && fixture.shows("Your name"));
    assert!(!fixture.enabled(BokkieAction::SignIn));
    fixture.send(BokkieIntent::EditInvitationCode(format!(" {CODE} ")));
    fixture.send(BokkieIntent::EditUsername("grace".into()));
    fixture.send(BokkieIntent::EditDisplayName("Grace".into()));
    fixture.send(BokkieIntent::EditPassword("short".into()));
    assert!(!fixture.enabled(BokkieAction::AcceptInvitation));
    fixture.send(BokkieIntent::EditPassword("a long password".into()));
    fixture.click("Join Bokkie");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::AcceptInvitation(AcceptInvitationRequest {
            code: CODE.into(),
            username: "grace".into(),
            password: "a long password".into(),
            display_name: "Grace".into(),
        })]
    );
    assert!(fixture.shows("Joining…"));
    assert!(!format!("{:?}", BokkieIntent::EditInvitationCode(CODE.into())).contains(CODE));
    let refused = "That invitation is not valid";
    fixture.reply(ApiReply::Joined(Err(error(
        403,
        ErrorCode::Forbidden,
        refused,
    ))));
    assert!(fixture.shows(refused));
    assert_eq!(fixture.fact("screen"), "signed_out".into());
    // The code and names stay to correct; the password goes.
    assert!(!fixture.enabled(BokkieAction::AcceptInvitation));
    fixture.send(BokkieIntent::EditPassword("a long password".into()));
    fixture.click("Join Bokkie");
    let _ = fixture.sent();
    fixture.reply(ApiReply::Joined(Ok(session(member()))));
    assert_eq!(fixture.fact("screen"), "signed_in".into());
    assert_eq!(fixture.fact("user"), "grace".into());
    assert_eq!(
        fixture.sent(),
        [
            ApiRequest::ListBots,
            ApiRequest::LoadUsage,
            ApiRequest::CodexAccount
        ]
    );
    // Signing in instead is one click away.
    let mut fixture = Fixture::new();
    fixture.reply(ApiReply::Session(Err(unauthenticated())));
    fixture.click("I have an invitation");
    fixture.click("I have an invitation");
    assert_eq!(fixture.fact("joining"), false.into());
    assert!(fixture.shows("Sign in") && !fixture.shows("Invitation code"));
}

#[test]
fn the_admin_screens_fit_at_phone_width() {
    let mut fixture = Fixture::new();
    fixture.accounts_open();
    fixture.assert_presentable();
    let mut fixture = Fixture::new();
    fixture.grace_open(
        true,
        vec![
            member_bot(7, "Diary", false, true),
            member_bot(8, "Coach", true, true),
        ],
    );
    fixture.assert_presentable();
    fixture.act_with(BokkieAction::ReadConversation, 7);
    fixture.reply(ApiReply::MemberMessages(
        GRACE,
        BotId(7),
        None,
        Ok(MessagePage {
            messages: vec![read_message(11, Author::User, "Dear diary")],
            older: Some(MessageId(11)),
            seq: Seq(40),
        }),
    ));
    fixture.assert_presentable();
    let mut fixture = Fixture::new();
    fixture.reply(ApiReply::Session(Err(unauthenticated())));
    fixture.click("I have an invitation");
    fixture.assert_presentable();
}

#[test]
fn the_admin_actions_have_readable_ids() {
    use stobie_ui_egui::ActionKey;
    for (action, id) in [
        (BokkieAction::Admin, "bokkie.admin"),
        (BokkieAction::OpenAccount, "bokkie.open-account"),
        (BokkieAction::CreateInvitation, "bokkie.create-invitation"),
        (
            BokkieAction::WithdrawInvitation,
            "bokkie.withdraw-invitation",
        ),
        (BokkieAction::MayCreateBots, "bokkie.may-create-bots"),
        (BokkieAction::MayEditProfiles, "bokkie.may-edit-profiles"),
        (BokkieAction::Oversight, "bokkie.oversight"),
        (BokkieAction::Disabled, "bokkie.disabled"),
        (BokkieAction::SaveLimits, "bokkie.save-limits"),
        (BokkieAction::ResetPassword, "bokkie.reset-password"),
        (BokkieAction::MakeMemberBot, "bokkie.make-member-bot"),
        (BokkieAction::EditMemberBot, "bokkie.edit-member-bot"),
        (BokkieAction::ReadConversation, "bokkie.read-conversation"),
        (BokkieAction::LoadEarlier, "bokkie.load-earlier"),
        (BokkieAction::Join, "bokkie.join"),
        (BokkieAction::AcceptInvitation, "bokkie.accept-invitation"),
    ] {
        assert_eq!(action.stable_id(), id);
    }
}
