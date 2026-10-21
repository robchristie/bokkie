//! The household's ChatGPT sign-in, for admins, against scripted replies:
//! the device code with its expiry, each state a sign-in can end in, cancel,
//! and sign-out with its warning. A member sees only whether the household
//! is signed in. No real sign-in is attempted.

use bokkie_protocol::api::{CodexAccount, DeviceCodeLogin, LoginState};

use super::*;
use crate::api::DeviceCode;
use crate::app::LOGIN_POLL;

const NOW: i64 = 1_800_000_000_000;
const MINUTE: i64 = 60_000;
const CODE: &str = "WXYZ-1234";
const URL: &str = "https://auth.example/codex/device";
const EMAIL: &str = "household@example.test";
const POLL: i64 = LOGIN_POLL.as_millis() as i64;

pub(super) fn account(signed_in: bool) -> CodexAccount {
    CodexAccount {
        signed_in,
        plan: signed_in.then(|| "plus".into()),
        email: signed_in.then(|| EMAIL.into()),
    }
}

fn login(state: LoginState, error: Option<&str>) -> DeviceCode {
    DeviceCode(DeviceCodeLogin {
        verification_url: URL.into(),
        user_code: CODE.into(),
        state,
        expires_at: Timestamp(NOW + 15 * MINUTE),
        error: error.map(Into::into),
    })
}

fn code() -> DeviceCode {
    login(LoginState::Pending, None)
}

impl Fixture {
    /// Move the clock on and let the host run a pass, as a scheduled
    /// repaint would.
    pub(super) fn later(&mut self, millis: i64) {
        let now = self.app().now().0 + millis;
        self.sent.set_now(Timestamp(now));
        self.harness.ctx.request_repaint();
        self.harness.run();
    }

    /// Nothing anywhere outside the screen holds the device code: not the
    /// facts, the semantic snapshot or any `Debug` output.
    fn code_kept_off_the_record(&self) {
        let snapshot = serde_json::to_string(self.host().ui_snapshot()).unwrap();
        assert!(!snapshot.contains(CODE), "{snapshot}");
        let inspection = self.host().inspection().unwrap();
        let reply = inspection.handle(InspectionRequest {
            version: 1,
            request_id: "observe".into(),
            instance: Some(inspection.instance()),
            operation: InspectionOperation::Observe,
        });
        let observed = serde_json::to_string(&reply).unwrap();
        assert!(!observed.contains(CODE), "{observed}");
        assert!(!format!("{:?}", self.app()).contains(CODE));
    }

    /// An admin, live, with the household signed out, on its sign-in screen.
    fn household_signed_out(&mut self) {
        self.sent.set_now(Timestamp(NOW));
        self.signed_in_and_live(10);
        self.reply(ApiReply::Account(Ok(account(false))));
        self.click("ChatGPT sign-in");
        assert_eq!(self.fact("screen"), "household".into());
        assert_eq!(self.fact("chatgpt_login"), "idle".into());
        // The household's status is asked again on the way in.
        assert_eq!(self.sent(), [ApiRequest::CodexAccount]);
        assert!(!self.enabled(BokkieAction::StartChatGpt), "still checking");
        self.reply(ApiReply::Account(Ok(account(false))));
    }

    /// A device code on screen, as after Get a code.
    fn waiting_for_code(&mut self) {
        self.household_signed_out();
        self.click("Get a code");
        assert_eq!(self.sent(), [ApiRequest::CodexLogin]);
        self.reply(ApiReply::Login(Ok(code())));
        assert_eq!(self.fact("chatgpt_login"), "waiting".into());
    }

    /// The poll that is due, answered.
    fn poll(&mut self, reply: Result<DeviceCode, CommandError>) {
        self.later(POLL);
        assert_eq!(self.sent(), [ApiRequest::CodexLoginStatus]);
        self.reply(ApiReply::LoginStatus(reply));
    }
}

#[test]
fn a_member_sees_only_whether_the_household_is_signed_in() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live_as(session(member()), 10);
    // Even if as whom arrived, a member is never shown it.
    fixture.reply(ApiReply::Account(Ok(account(true))));
    assert_eq!(fixture.fact("chatgpt"), "signed_in".into());
    assert!(fixture.shows("ChatGPT: signed in for the household"));
    let page = serde_json::to_string(fixture.host().ui_snapshot()).unwrap();
    assert!(!page.contains(EMAIL) && !page.contains("plus"), "{page}");
    assert!(!fixture.shows(&format!("ChatGPT: signed in as {EMAIL} (plus)")));
    assert!(!format!("{:?}", fixture.app().session).contains(EMAIL));
    // No way to sign it in or out.
    assert!(!fixture.shows("ChatGPT sign-in"));
    assert!(!fixture.enabled(BokkieAction::Household));
    fixture.reply(ApiReply::Account(Ok(account(false))));
    assert_eq!(fixture.fact("chatgpt"), "signed_out".into());
    assert!(fixture.shows(
        "ChatGPT is signed out for the household, so your Bots cannot reply. Ask an admin to sign it in"
    ));
    assert!(!fixture.shows("ChatGPT sign-in"));
    for action in [
        BokkieAction::StartChatGpt,
        BokkieAction::CancelChatGpt,
        BokkieAction::SignOutChatGpt,
        BokkieAction::ConfirmSignOutChatGpt,
    ] {
        assert!(!fixture.enabled(action), "{action:?}");
    }
}

#[test]
fn an_admin_sees_as_whom_the_household_is_signed_in() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    assert_eq!(fixture.fact("chatgpt"), "checking".into());
    assert!(fixture.shows("ChatGPT: checking…"));
    fixture.reply(ApiReply::Account(Ok(account(true))));
    assert_eq!(fixture.fact("chatgpt"), "signed_in".into());
    assert!(fixture.shows(&format!("ChatGPT: signed in as {EMAIL} (plus)")));
    assert!(fixture.enabled(BokkieAction::Household));
    fixture.click("ChatGPT sign-in");
    fixture.reply(ApiReply::Account(Ok(account(true))));
    assert!(!fixture.enabled(BokkieAction::StartChatGpt), "signed in");
    assert!(!fixture.shows("Get a code"));
    assert!(fixture.shows("Sign out of ChatGPT"));
}

#[test]
fn a_device_code_shows_with_its_expiry_and_is_polled_until_the_sign_in_completes() {
    let mut fixture = Fixture::new();
    fixture.household_signed_out();
    assert!(fixture.shows("Signed out: no one's Bots can reply until an admin signs in"));
    assert!(!fixture.shows("Sign out of ChatGPT"), "nothing to sign out");
    fixture.click("Get a code");
    assert_eq!(fixture.sent(), [ApiRequest::CodexLogin]);
    assert_eq!(fixture.fact("chatgpt_login"), "starting".into());
    assert!(fixture.shows("Asking for a code…"));
    assert!(
        !fixture.enabled(BokkieAction::StartChatGpt),
        "one at a time"
    );
    fixture.reply(ApiReply::Login(Ok(code())));
    assert_eq!(fixture.fact("chatgpt_login"), "waiting".into());
    assert!(fixture.shows(URL) && fixture.shows(CODE));
    assert!(fixture.shows("The code expires in 15 min"));
    assert!(fixture.shows("Waiting for you to finish signing in…"));
    fixture.code_kept_off_the_record();
    // No poll before the interval has passed.
    fixture.later(1_000);
    assert!(fixture.sent().is_empty());
    fixture.later(POLL);
    assert_eq!(fixture.sent(), [ApiRequest::CodexLoginStatus]);
    // One poll at a time.
    fixture.later(POLL);
    assert!(fixture.sent().is_empty());
    fixture.reply(ApiReply::LoginStatus(Ok(code())));
    assert_eq!(fixture.fact("chatgpt_login"), "waiting".into());
    // A poll that fails says so, and polling goes on with the code shown.
    let down = "Bokkie's server is not available. Try again shortly";
    fixture.poll(Err(error(502, ErrorCode::Unavailable, down)));
    assert!(fixture.shows(down) && fixture.shows(CODE));
    // The expiry counts down.
    fixture.later(9 * MINUTE);
    assert!(fixture.shows("The code expires in 6 min"));
    assert_eq!(fixture.sent(), [ApiRequest::CodexLoginStatus]);
    fixture.reply(ApiReply::LoginStatus(Ok(code())));
    assert!(!fixture.shows(down), "the poll answered");
    fixture.poll(Ok(login(LoginState::Completed, None)));
    assert_eq!(fixture.fact("chatgpt_login"), "completed".into());
    assert!(fixture.shows("Signed in to ChatGPT. Every account's Bots can reply"));
    assert!(!fixture.shows(CODE), "the code is gone");
    // The household's account is asked again, and says as whom.
    assert_eq!(fixture.sent(), [ApiRequest::CodexAccount]);
    assert_eq!(fixture.fact("chatgpt"), "checking".into());
    fixture.reply(ApiReply::Account(Ok(account(true))));
    assert!(fixture.shows(&format!("ChatGPT: signed in as {EMAIL} (plus)")));
    fixture.later(POLL * 3);
    assert!(fixture.sent().is_empty(), "polling stopped");
    fixture.click("Back");
    assert_eq!(fixture.fact("screen"), "signed_in".into());
}

#[test]
fn an_expired_code_shows_as_expired_and_a_new_one_is_offered() {
    let mut fixture = Fixture::new();
    fixture.waiting_for_code();
    // The code's time runs out on screen.
    fixture.later(15 * MINUTE);
    assert!(fixture.shows("The code is about to expire"));
    assert_eq!(fixture.sent(), [ApiRequest::CodexLoginStatus]);
    fixture.reply(ApiReply::LoginStatus(Ok(login(LoginState::Expired, None))));
    assert_eq!(fixture.fact("chatgpt_login"), "expired".into());
    assert!(
        fixture.shows("The code expired before the sign-in finished. Get a new code to try again")
    );
    assert!(!fixture.shows(CODE), "an expired code is gone");
    fixture.later(POLL * 3);
    assert!(fixture.sent().is_empty(), "polling stopped");
    // A new code is offered, and replaces it.
    assert!(fixture.enabled(BokkieAction::StartChatGpt));
    fixture.click("Get a code");
    assert_eq!(fixture.sent(), [ApiRequest::CodexLogin]);
    fixture.reply(ApiReply::Login(Ok(DeviceCode(DeviceCodeLogin {
        user_code: "ABCD-5678".into(),
        expires_at: Timestamp(fixture.app().now().0 + 15 * MINUTE),
        ..code().0
    }))));
    assert!(fixture.shows("ABCD-5678"));
    assert!(fixture.shows("The code expires in 15 min"));

    // Codex giving up on an unused code says the same.
    let mut fixture = Fixture::new();
    fixture.waiting_for_code();
    fixture.poll(Ok(login(
        LoginState::Expired,
        Some("device auth timed out after 15 minutes"),
    )));
    assert_eq!(fixture.fact("chatgpt_login"), "expired".into());
    assert!(fixture.enabled(BokkieAction::StartChatGpt));
}

#[test]
fn a_code_on_screen_can_be_replaced_by_a_new_one() {
    let mut fixture = Fixture::new();
    fixture.waiting_for_code();
    assert!(fixture.enabled(BokkieAction::StartChatGpt));
    fixture.click("Get a code");
    assert_eq!(fixture.sent(), [ApiRequest::CodexLogin]);
    assert!(!fixture.shows(CODE), "the old code goes at once");
    fixture.reply(ApiReply::Login(Ok(DeviceCode(DeviceCodeLogin {
        user_code: "ABCD-5678".into(),
        ..code().0
    }))));
    assert!(fixture.shows("ABCD-5678"));
    assert!(!fixture.shows(CODE));
}

#[test]
fn a_failed_sign_in_says_why_and_offers_a_new_code() {
    let mut fixture = Fixture::new();
    fixture.waiting_for_code();
    fixture.poll(Ok(login(LoginState::Failed, Some("the code was refused"))));
    assert_eq!(fixture.fact("chatgpt_login"), "failed".into());
    assert!(fixture.shows("The sign-in failed: the code was refused"));
    assert!(!fixture.shows(CODE));
    assert!(fixture.enabled(BokkieAction::StartChatGpt));

    // The household's app server stopping fails it too.
    let mut fixture = Fixture::new();
    fixture.waiting_for_code();
    fixture.poll(Ok(login(
        LoginState::Failed,
        Some("The household's app server stopped"),
    )));
    assert!(fixture.shows("The sign-in failed: The household's app server stopped"));

    // Another admin's new sign-in cancelled this one.
    let mut fixture = Fixture::new();
    fixture.waiting_for_code();
    fixture.poll(Ok(login(LoginState::Cancelled, None)));
    assert_eq!(fixture.fact("chatgpt_login"), "cancelled".into());
    assert!(fixture.shows("The sign-in was cancelled"));
    assert!(fixture.enabled(BokkieAction::StartChatGpt));
}

#[test]
fn a_sign_in_lost_to_a_server_restart_says_so() {
    let mut fixture = Fixture::new();
    fixture.waiting_for_code();
    fixture.poll(Err(error(404, ErrorCode::NotFound, "Not found")));
    assert_eq!(fixture.fact("chatgpt_login"), "failed".into());
    assert!(fixture.shows("Bokkie's server no longer knows this sign-in. Get a new code"));
    assert!(!fixture.shows(CODE));
    assert!(fixture.enabled(BokkieAction::StartChatGpt));
}

#[test]
fn a_pending_sign_in_can_be_cancelled() {
    let mut fixture = Fixture::new();
    fixture.waiting_for_code();
    assert!(fixture.enabled(BokkieAction::CancelChatGpt));
    fixture.click("Cancel sign-in");
    assert_eq!(fixture.sent(), [ApiRequest::CancelCodexLogin]);
    assert_eq!(fixture.fact("chatgpt_login"), "cancelling".into());
    assert!(fixture.shows("Cancelling…"));
    assert!(!fixture.enabled(BokkieAction::CancelChatGpt));
    assert!(!fixture.enabled(BokkieAction::StartChatGpt));
    // No poll while it cancels.
    fixture.later(POLL * 2);
    assert!(fixture.sent().is_empty());
    // It fails first: the code stays, with why.
    let down = "Bokkie's server is not available. Try again shortly";
    fixture.reply(ApiReply::LoginCancelled(Err(error(
        502,
        ErrorCode::Unavailable,
        down,
    ))));
    assert_eq!(fixture.fact("chatgpt_login"), "waiting".into());
    assert!(fixture.shows(down) && fixture.shows(CODE));
    fixture.click("Cancel sign-in");
    assert_eq!(fixture.sent(), [ApiRequest::CancelCodexLogin]);
    fixture.reply(ApiReply::LoginCancelled(Ok(login(
        LoginState::Cancelled,
        None,
    ))));
    assert_eq!(fixture.fact("chatgpt_login"), "cancelled".into());
    assert!(fixture.shows("The sign-in was cancelled"));
    assert!(!fixture.shows(CODE));
    fixture.later(POLL * 2);
    assert!(fixture.sent().is_empty(), "polling stopped");
    assert!(fixture.enabled(BokkieAction::StartChatGpt));

    // A sign-in that completed just before the cancel arrived says so.
    let mut fixture = Fixture::new();
    fixture.waiting_for_code();
    fixture.click("Cancel sign-in");
    let _ = fixture.sent();
    fixture.reply(ApiReply::LoginCancelled(Ok(login(
        LoginState::Completed,
        None,
    ))));
    assert_eq!(fixture.fact("chatgpt_login"), "completed".into());
    assert_eq!(fixture.sent(), [ApiRequest::CodexAccount]);
}

#[test]
fn a_failed_start_says_why_and_can_be_tried_again() {
    let mut fixture = Fixture::new();
    fixture.household_signed_out();
    fixture.click("Get a code");
    let _ = fixture.sent();
    let refused = "Codex did not offer a device-code sign-in";
    fixture.reply(ApiReply::Login(Err(error(
        503,
        ErrorCode::Unavailable,
        refused,
    ))));
    assert_eq!(fixture.fact("chatgpt_login"), "failed".into());
    assert!(fixture.shows(refused));
    assert!(fixture.enabled(BokkieAction::StartChatGpt));
    fixture.click("Get a code");
    assert_eq!(fixture.sent(), [ApiRequest::CodexLogin]);
}

#[test]
fn signing_the_household_out_warns_that_it_signs_out_everyone() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.reply(ApiReply::Account(Ok(account(true))));
    fixture.click("ChatGPT sign-in");
    fixture.reply(ApiReply::Account(Ok(account(true))));
    let _ = fixture.sent();
    assert!(fixture.shows(
        "Signing out signs out every account's Bots at once, until an admin signs in again."
    ));
    assert!(!fixture.enabled(BokkieAction::ConfirmSignOutChatGpt));
    fixture.click("Sign out of ChatGPT");
    assert!(fixture.sent().is_empty(), "nothing yet: it asks first");
    assert_eq!(fixture.fact("chatgpt_sign_out"), "confirming".into());
    // A confirm that says it signs out every account's Bots, with Keep
    // signed in first and focused, and nothing behind it in reach.
    let confirm = fixture
        .dialog(stobie_ui_egui::UiRole::AlertDialog)
        .expect("the confirm is open");
    assert_eq!(confirm.name, "Sign out everyone?");
    assert_eq!(
        confirm.description.as_deref(),
        Some(crate::ui::SIGN_OUT_EVERYONE)
    );
    assert!(fixture.shows(crate::ui::SIGN_OUT_EVERYONE));
    assert_eq!(fixture.focused_label().as_deref(), Some("Keep signed in"));
    for _ in 0..6 {
        fixture.press_key(egui::Key::Tab);
        let label = fixture.focused_label().expect("a control has focus");
        assert!(!fixture.hidden(&label), "{label} is behind the confirm");
    }
    fixture.refused_behind(BokkieAction::RefreshBots, "Sign out everyone?");
    fixture.refused_behind(BokkieAction::Back, "Sign out everyone?");
    assert!(fixture.hidden("Sign out of ChatGPT"));
    assert!(fixture.sent().is_empty());
    // Escape keeps it signed in, as Keep signed in does, and sends nothing.
    fixture.press_key(egui::Key::Escape);
    assert_eq!(fixture.fact("chatgpt_sign_out"), "".into());
    assert_eq!(
        fixture.focused_label().as_deref(),
        Some("Sign out of ChatGPT")
    );
    fixture.click("Sign out of ChatGPT");
    fixture.click("Keep signed in");
    assert!(fixture.sent().is_empty());
    assert_eq!(fixture.fact("chatgpt_sign_out"), "".into());
    assert_eq!(
        fixture.focused_label().as_deref(),
        Some("Sign out of ChatGPT")
    );
    fixture.click("Sign out of ChatGPT");
    fixture.click("Sign out everyone");
    assert_eq!(fixture.sent(), [ApiRequest::CodexLogout]);
    assert_eq!(fixture.fact("chatgpt_sign_out"), "signing_out".into());
    // Chosen, the confirm closes and the screen says it is signing out.
    assert!(
        fixture
            .dialog(stobie_ui_egui::UiRole::AlertDialog)
            .is_none()
    );
    assert!(fixture.shows("Signing out…"));
    assert!(!fixture.enabled(BokkieAction::ConfirmSignOutChatGpt));
    // A failure says why, and it can be tried again.
    let down = "The Bots' runtime is unavailable";
    fixture.reply(ApiReply::HouseholdSignedOut(Err(error(
        503,
        ErrorCode::Unavailable,
        down,
    ))));
    assert_eq!(fixture.fact("chatgpt_sign_out"), "failed".into());
    assert!(fixture.shows(down));
    assert_eq!(fixture.fact("chatgpt"), "signed_in".into());
    fixture.click("Sign out of ChatGPT");
    fixture.click("Sign out everyone");
    assert_eq!(fixture.sent(), [ApiRequest::CodexLogout]);
    fixture.reply(ApiReply::HouseholdSignedOut(Ok(())));
    assert_eq!(fixture.fact("chatgpt"), "signed_out".into());
    assert_eq!(fixture.fact("chatgpt_sign_out"), "".into());
    assert_eq!(fixture.sent(), [ApiRequest::CodexAccount]);
    assert!(fixture.shows("Signed out: no one's Bots can reply until an admin signs in"));
    assert!(fixture.enabled(BokkieAction::StartChatGpt));
}

#[test]
fn leaving_the_sign_in_drops_the_code_and_stops_polling() {
    let mut fixture = Fixture::new();
    fixture.waiting_for_code();
    fixture.click("Back");
    assert_eq!(fixture.fact("screen"), "signed_in".into());
    assert_eq!(fixture.fact("chatgpt_login"), "".into());
    assert!(!fixture.shows(CODE));
    fixture.later(POLL * 3);
    assert!(fixture.sent().is_empty());
    // Answers that arrive after leaving are not shown.
    fixture.reply(ApiReply::Login(Ok(code())));
    fixture.reply(ApiReply::LoginStatus(Ok(code())));
    assert!(!fixture.shows(CODE));
    assert_eq!(fixture.fact("screen"), "signed_in".into());
}

#[test]
fn signing_out_of_bokkie_drops_a_code_on_screen() {
    let mut fixture = Fixture::new();
    fixture.waiting_for_code();
    fixture.socket(crate::connection::Input::Closed {
        code: Some(crate::connection::CLOSE_SIGNED_OUT),
    });
    assert_eq!(fixture.fact("screen"), "signed_out".into());
    assert!(!fixture.shows(CODE));
}

#[test]
fn the_verification_link_opens_in_a_new_tab_so_the_code_stays_on_screen() {
    let mut fixture = Fixture::new();
    fixture.waiting_for_code();
    assert!(fixture.shows("Keep this page open: the code is shown only here."));
    fixture.harness.get_by_label(URL).click();
    fixture.harness.step();
    let opened: Vec<_> = fixture
        .harness
        .output()
        .platform_output
        .commands
        .iter()
        .filter_map(|command| match command {
            egui::OutputCommand::OpenUrl(open) => Some((open.url.clone(), open.new_tab)),
            _ => None,
        })
        .collect();
    assert_eq!(opened, [(URL.to_owned(), true)]);
    fixture.harness.run();
    assert!(fixture.shows(CODE), "the code is still on screen");
}

#[test]
fn the_household_screens_fit_at_phone_width() {
    let mut fixture = Fixture::new();
    fixture.household_signed_out();
    fixture.assert_presentable();
    fixture.click("Get a code");
    let _ = fixture.sent();
    fixture.reply(ApiReply::Login(Ok(code())));
    fixture.assert_presentable();
    fixture.poll(Ok(login(LoginState::Expired, None)));
    fixture.assert_presentable();
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.click("ChatGPT sign-in");
    fixture.reply(ApiReply::Account(Ok(account(true))));
    fixture.click("Sign out of ChatGPT");
    fixture.assert_presentable();
}

#[test]
fn the_household_actions_have_readable_ids() {
    use stobie_ui_egui::ActionKey;
    for (action, id) in [
        (BokkieAction::Household, "bokkie.household"),
        (BokkieAction::StartChatGpt, "bokkie.start-chatgpt"),
        (BokkieAction::CancelChatGpt, "bokkie.cancel-chatgpt"),
        (BokkieAction::SignOutChatGpt, "bokkie.sign-out-chatgpt"),
        (
            BokkieAction::ConfirmSignOutChatGpt,
            "bokkie.confirm-sign-out-chatgpt",
        ),
        (BokkieAction::KeepChatGpt, "bokkie.keep-chatgpt"),
        (BokkieAction::AcknowledgeReset, "bokkie.acknowledge-reset"),
    ] {
        assert_eq!(action.stable_id(), id);
    }
}
