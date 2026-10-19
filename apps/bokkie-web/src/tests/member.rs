//! What a member's permissions allow, and the notices: oversight, the whole
//! time it is on, and an admin's password reset, until acknowledged. Each
//! follows `account_updated` live.

use bokkie_protocol::api::{Permissions, SendMessageRequest, UpdateBotRequest};
use bokkie_protocol::events::Event;

use super::conversation::{TUTOR, hello, tutor_list};
use super::household::account;
use super::*;

const NOW: i64 = 1_800_000_000_000;
const HOUR: i64 = 3_600_000;

const OVERSIGHT: &str = "Oversight is on: an admin can read all your Bots and conversations. It was switched on 3 h ago";

fn with(permissions: Permissions) -> User {
    User {
        permissions,
        ..member()
    }
}

fn overseen(on: bool) -> User {
    User {
        oversight: on,
        oversight_changed_at: Some(Timestamp(NOW - 3 * HOUR)),
        ..member()
    }
}

const NEITHER: Permissions = Permissions {
    may_create_bots: false,
    may_edit_bot_profiles: false,
};

impl Fixture {
    /// Signed in as Grace, live, at `NOW`.
    fn member_live(&mut self, user: User) {
        self.sent.set_now(Timestamp(NOW));
        self.signed_in_and_live_as(session(user), 10);
    }

    fn account_updated(&mut self, seq: u64, user: User) {
        self.event(seq, Event::AccountUpdated { user });
    }

    fn tutor(&self) -> Bot {
        self.app().store().bot(TUTOR).unwrap().clone()
    }
}

#[test]
fn new_bot_is_hidden_without_the_permission_and_follows_it_live() {
    let mut fixture = Fixture::new();
    fixture.member_live(with(NEITHER));
    assert_eq!(fixture.fact("role"), "member".into());
    assert_eq!(fixture.fact("may_create_bots"), false.into());
    assert!(!fixture.shows("New Bot") && !fixture.shows("Create Bot"));
    assert!(fixture.shows("An admin makes your Bots"));
    assert!(!fixture.enabled(BokkieAction::CreateBot));
    fixture.send(BokkieIntent::EditBotName("Gardener".into()));
    assert!(
        !fixture.enabled(BokkieAction::CreateBot),
        "still not allowed"
    );
    // An admin allows it: the form appears at once.
    fixture.account_updated(
        11,
        with(Permissions {
            may_create_bots: true,
            may_edit_bot_profiles: false,
        }),
    );
    assert_eq!(fixture.fact("may_create_bots"), true.into());
    assert!(fixture.shows("New Bot") && !fixture.shows("An admin makes your Bots"));
    assert!(fixture.enabled(BokkieAction::CreateBot));
    // And takes it back.
    fixture.account_updated(12, with(NEITHER));
    assert!(!fixture.shows("New Bot"));
    assert!(!fixture.enabled(BokkieAction::CreateBot));
    assert!(fixture.sent().is_empty());
}

#[test]
fn profile_editing_is_hidden_without_the_permission_and_pin_and_hide_stay() {
    let mut fixture = Fixture::new();
    fixture.member_live(with(NEITHER));
    fixture.click("Open Tutor");
    let _ = fixture.sent();
    assert_eq!(fixture.fact("may_edit_profiles"), false.into());
    assert!(!fixture.shows("Edit profile"));
    assert!(!fixture.enabled(BokkieAction::EditProfile));
    assert!(fixture.shows("Pin Bot") && fixture.shows("Hide Bot"));
    fixture.click("Pin Bot");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::UpdateBot(
            TUTOR,
            UpdateBotRequest {
                pinned: Some(true),
                ..Default::default()
            }
        )]
    );
    assert_eq!(fixture.fact("updating"), true.into());
    assert!(!fixture.enabled(BokkieAction::Pin), "one change at a time");
    let mut pinned = fixture.tutor();
    pinned.pinned = true;
    fixture.reply(ApiReply::BotUpdated(TUTOR, Ok(pinned.clone())));
    fixture.event(11, Event::BotUpdated { bot: pinned });
    assert_eq!(fixture.fact("bot_pinned"), true.into());
    assert!(fixture.shows("Maths help · Idle · Pinned"));
    // Allowed live, the control appears; an editor open when it is taken
    // back closes.
    let both = with(Permissions::default());
    fixture.account_updated(12, both);
    assert!(fixture.shows("Edit profile"));
    fixture.click("Edit profile");
    assert_eq!(fixture.fact("profile"), "editing".into());
    fixture.account_updated(13, with(NEITHER));
    assert_eq!(fixture.fact("profile"), "".into());
    assert!(!fixture.shows("Edit profile") && !fixture.shows("Save profile"));
    assert!(fixture.shows("Hide Bot"));
    assert!(fixture.sent().is_empty());
}

#[test]
fn a_profile_edit_sends_only_what_changed() {
    let mut fixture = Fixture::new();
    fixture.member_live(member());
    fixture.click("Open Tutor");
    let _ = fixture.sent();
    fixture.click("Edit profile");
    assert!(fixture.shows("Bot name") && fixture.shows("Description"));
    assert!(!fixture.shows("Pin Bot"), "one thing at a time");
    // Nothing changed: it closes without a request.
    fixture.click("Save profile");
    assert!(fixture.sent().is_empty());
    assert_eq!(fixture.fact("profile"), "".into());
    fixture.click("Edit profile");
    fixture.send(BokkieIntent::EditProfileLabel(" Algebra and more ".into()));
    fixture.send(BokkieIntent::EditProfileDescription(
        "Patient, and asks before giving answers.".into(),
    ));
    fixture.click("Save profile");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::UpdateBot(
            TUTOR,
            UpdateBotRequest {
                label: Some("Algebra and more".into()),
                description: Some("Patient, and asks before giving answers.".into()),
                ..Default::default()
            }
        )]
    );
    assert_eq!(fixture.fact("profile"), "saving".into());
    assert!(!fixture.enabled(BokkieAction::SaveProfile));
    // Refused: the edits stay, with why.
    let refused = "Only an admin can change your Bots' profiles";
    fixture.reply(ApiReply::BotUpdated(
        TUTOR,
        Err(error(403, ErrorCode::Forbidden, refused)),
    ));
    assert_eq!(fixture.fact("profile_error"), refused.into());
    assert!(fixture.shows(refused));
    fixture.send(BokkieIntent::EditProfileName("".into()));
    assert!(
        !fixture.enabled(BokkieAction::SaveProfile),
        "a name is needed"
    );
    fixture.send(BokkieIntent::EditProfileName("Maths Tutor".into()));
    fixture.click("Save profile");
    assert!(matches!(
        fixture.sent().as_slice(),
        [ApiRequest::UpdateBot(TUTOR, changes)] if changes.name.as_deref() == Some("Maths Tutor")
    ));
    let mut renamed = fixture.tutor();
    renamed.name = "Maths Tutor".into();
    fixture.reply(ApiReply::BotUpdated(TUTOR, Ok(renamed.clone())));
    assert_eq!(fixture.fact("profile"), "".into());
    // The Bot itself arrives as its event.
    fixture.event(11, Event::BotUpdated { bot: renamed });
    assert!(fixture.shows("Maths Tutor"));
    // Cancelling drops the edits.
    fixture.click("Edit profile");
    fixture.send(BokkieIntent::EditProfileName("Scrap".into()));
    fixture.click("Cancel editing");
    assert_eq!(fixture.fact("profile"), "".into());
    assert!(fixture.sent().is_empty());
}

#[test]
fn a_hidden_bot_leaves_the_list_until_hidden_bots_are_shown() {
    let mut fixture = Fixture::new();
    fixture.member_live(with(NEITHER));
    fixture.click("Open Tutor");
    let _ = fixture.sent();
    fixture.click("Hide Bot");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::UpdateBot(
            TUTOR,
            UpdateBotRequest {
                hidden: Some(true),
                ..Default::default()
            }
        )]
    );
    let refused = "Bokkie's server is not available. Try again shortly";
    fixture.reply(ApiReply::BotUpdated(
        TUTOR,
        Err(error(502, ErrorCode::Unavailable, refused)),
    ));
    assert!(fixture.shows(refused));
    fixture.click("Hide Bot");
    let _ = fixture.sent();
    let mut hidden = fixture.tutor();
    hidden.hidden = true;
    fixture.reply(ApiReply::BotUpdated(TUTOR, Ok(hidden.clone())));
    fixture.event(11, Event::BotUpdated { bot: hidden });
    assert!(!fixture.shows(refused));
    fixture.click("Back");
    assert!(!fixture.shows("Open Tutor"));
    assert_eq!(fixture.fact("bot_count"), 0_u64.into());
    fixture.click("Show hidden Bots");
    assert_eq!(fixture.fact("show_hidden"), true.into());
    assert!(fixture.shows("Open Tutor") && fixture.shows("Maths help · Idle · Hidden"));
    fixture.click("Show hidden Bots");
    assert!(!fixture.shows("Open Tutor"));
}

#[test]
fn the_oversight_notice_shows_the_whole_time_oversight_is_on() {
    let mut fixture = Fixture::new();
    fixture.member_live(overseen(true));
    assert_eq!(fixture.fact("oversight"), true.into());
    assert!(fixture.shows(OVERSIGHT));
    // On every screen.
    fixture.click("Open Tutor");
    assert!(fixture.shows(OVERSIGHT));
    // Switched off live, it goes at once; on again, it comes back.
    fixture.account_updated(11, overseen(false));
    assert_eq!(fixture.fact("oversight"), false.into());
    assert!(!fixture.shows(OVERSIGHT));
    let mut again = overseen(true);
    again.oversight_changed_at = Some(Timestamp(NOW - 5 * 60_000));
    fixture.account_updated(12, again);
    assert!(fixture.shows(
        "Oversight is on: an admin can read all your Bots and conversations. It was switched on 5 min ago"
    ));
    // The time stays true as it passes.
    fixture.later(HOUR);
    assert!(fixture.shows(
        "Oversight is on: an admin can read all your Bots and conversations. It was switched on 1 h ago"
    ));
}

#[test]
fn a_member_signed_in_before_oversight_sees_it_switched_on() {
    let mut fixture = Fixture::new();
    fixture.member_live(member());
    assert!(!fixture.shows(OVERSIGHT));
    fixture.account_updated(11, overseen(true));
    assert!(fixture.shows(OVERSIGHT));
}

#[test]
fn a_session_check_older_than_an_account_event_does_not_undo_it() {
    let mut fixture = Fixture::new();
    fixture.member_live(member());
    // A reset reloads everything, the account included; meanwhile
    // oversight comes on.
    fixture.frame(bokkie_protocol::events::ServerFrame::Reset { after: Seq(10) });
    assert!(fixture.sent().contains(&ApiRequest::CheckSession));
    fixture.account_updated(11, overseen(true));
    // The check was read before the change, and answers now.
    fixture.reply(ApiReply::Session(Ok(session(member()))));
    assert_eq!(fixture.fact("oversight"), true.into());
    assert!(fixture.shows(OVERSIGHT));
    // A check asked for afterwards is current, and applies.
    fixture.frame(bokkie_protocol::events::ServerFrame::Reset { after: Seq(11) });
    assert!(fixture.sent().contains(&ApiRequest::CheckSession));
    fixture.reply(ApiReply::Session(Ok(session(overseen(false)))));
    assert!(!fixture.shows(OVERSIGHT));
}

#[test]
fn an_admin_never_sees_the_oversight_notice() {
    let mut fixture = Fixture::new();
    // Even if the flag arrived for an admin, which the gateway refuses.
    fixture.signed_in_and_live_as(
        session(User {
            oversight: true,
            ..user()
        }),
        10,
    );
    assert_eq!(fixture.fact("oversight"), false.into());
    assert!(!fixture.shows(OVERSIGHT));
}

#[test]
fn a_member_is_told_an_admin_reset_their_password_until_they_acknowledge_it() {
    let notice = "An admin reset your password 2 h ago";
    let mut fixture = Fixture::new();
    fixture.sent.set_now(Timestamp(NOW));
    let reset = SessionResponse {
        user: member(),
        password_reset_by_admin: Some(Timestamp(NOW - 2 * HOUR)),
    };
    fixture.signed_in_and_live_as(reset.clone(), 10);
    assert_eq!(fixture.fact("reset_notice"), true.into());
    assert!(fixture.shows(notice));
    fixture.click("Open Tutor");
    let _ = fixture.sent();
    assert!(fixture.shows(notice), "on every screen until acknowledged");
    fixture.click("Acknowledge");
    assert_eq!(fixture.sent(), [ApiRequest::AcknowledgeReset]);
    assert!(fixture.shows("Acknowledging…"));
    assert!(!fixture.enabled(BokkieAction::AcknowledgeReset));
    // A failure keeps the notice, with why.
    let down = "Bokkie's server is not available. Try again shortly";
    fixture.reply(ApiReply::Acknowledged(Err(error(
        502,
        ErrorCode::Unavailable,
        down,
    ))));
    assert!(fixture.shows(notice));
    assert_eq!(fixture.fact("message"), down.into());
    fixture.click("Acknowledge");
    assert_eq!(fixture.sent(), [ApiRequest::AcknowledgeReset]);
    fixture.reply(ApiReply::Acknowledged(Ok(())));
    assert_eq!(fixture.fact("reset_notice"), false.into());
    assert!(!fixture.shows(notice));
    assert!(!fixture.enabled(BokkieAction::AcknowledgeReset));
    // A session check read before the acknowledgement does not bring it
    // back.
    fixture.frame(bokkie_protocol::events::ServerFrame::Reset { after: Seq(10) });
    assert!(fixture.sent().contains(&ApiRequest::CheckSession));
    fixture.reply(ApiReply::Session(Ok(reset.clone())));
    assert!(!fixture.shows(notice));

    // Acknowledged on another device, a session check clears it here.
    let mut fixture = Fixture::new();
    fixture.sent.set_now(Timestamp(NOW));
    fixture.signed_in_and_live_as(reset.clone(), 10);
    assert!(fixture.shows(notice));
    fixture.frame(bokkie_protocol::events::ServerFrame::Reset { after: Seq(10) });
    assert!(fixture.sent().contains(&ApiRequest::CheckSession));
    fixture.reply(ApiReply::Session(Ok(session(member()))));
    assert!(!fixture.shows(notice));
    assert_eq!(fixture.fact("reset_notice"), false.into());

    // Reloading the page before acknowledging tells them again.
    let mut fixture = Fixture::new();
    fixture.sent.set_now(Timestamp(NOW));
    let _ = fixture.sent();
    fixture.reply(ApiReply::Session(Ok(reset)));
    assert!(fixture.shows(notice));
}

#[test]
fn a_send_refused_while_the_household_is_signed_out_says_so_and_keeps_the_draft() {
    let mut fixture = Fixture::new();
    fixture.member_live(member());
    fixture.reply(ApiReply::Account(Ok(account(true))));
    fixture.open_tutor(hello(), 10);
    fixture.write("What is 7 x 8?");
    fixture.click("Send");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::SendMessage(
            TUTOR,
            SendMessageRequest {
                text: "What is 7 x 8?".into()
            }
        )]
    );
    let refused = "ChatGPT is signed out for the household, so the Bots cannot reply. Ask an admin to sign it in";
    fixture.reply(ApiReply::Sent(
        TUTOR,
        Err(error(503, ErrorCode::Unavailable, refused)),
    ));
    assert_eq!(fixture.fact("send_error"), refused.into());
    assert!(fixture.shows(refused));
    assert_eq!(fixture.fact("draft"), "What is 7 x 8?".into());
    // The household's status is asked again, and the list says it.
    assert_eq!(fixture.sent(), [ApiRequest::CodexAccount]);
    fixture.reply(ApiReply::Account(Ok(account(false))));
    fixture.click("Back");
    assert!(fixture.shows(
        "ChatGPT is signed out for the household, so your Bots cannot reply. Ask an admin to sign it in"
    ));
    // The list's Refresh asks again too.
    fixture.click("Refresh");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::ListBots, ApiRequest::CodexAccount]
    );
    fixture.reply(ApiReply::Bots(Ok(tutor_list(BotStatus::Idle, 10))));
    fixture.reply(ApiReply::Account(Ok(account(true))));
    assert!(fixture.shows("ChatGPT: signed in for the household"));
}

#[test]
fn the_member_view_fits_at_phone_width() {
    let mut fixture = Fixture::new();
    fixture.sent.set_now(Timestamp(NOW));
    fixture.signed_in_and_live_as(
        SessionResponse {
            user: User {
                permissions: NEITHER,
                ..overseen(true)
            },
            password_reset_by_admin: Some(Timestamp(NOW - HOUR)),
        },
        10,
    );
    fixture.reply(ApiReply::Account(Ok(account(false))));
    fixture.assert_presentable();
    fixture.click("Open Tutor");
    fixture.assert_presentable();
    let mut fixture = Fixture::new();
    fixture.member_live(member());
    fixture.click("Open Tutor");
    fixture.click("Edit profile");
    fixture.assert_presentable();
}

#[test]
fn the_member_actions_have_readable_ids() {
    use stobie_ui_egui::ActionKey;
    for (action, id) in [
        (BokkieAction::ShowHidden, "bokkie.show-hidden"),
        (BokkieAction::Pin, "bokkie.pin"),
        (BokkieAction::Hide, "bokkie.hide"),
        (BokkieAction::EditProfile, "bokkie.edit-profile"),
        (BokkieAction::SaveProfile, "bokkie.save-profile"),
        (BokkieAction::CancelProfile, "bokkie.cancel-profile"),
    ] {
        assert_eq!(action.stable_id(), id);
    }
}
