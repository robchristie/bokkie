//! The two stand-ins of settled decision 8: the New Bot form and the
//! ChatGPT device-code sign-in, against scripted replies. No real sign-in is
//! attempted.

use bokkie_protocol::api::{CodexAccount, CreateBotRequest, DeviceCodeLogin};
use bokkie_protocol::events::Event;

use super::*;
use crate::api::DeviceCode;
use crate::app::LOGIN_POLL;

const NOW: i64 = 1_800_000_000_000;
const CODE: &str = "WXYZ-1234";
const URL: &str = "https://auth.example/codex/device";

fn account(signed_in: bool) -> CodexAccount {
    CodexAccount {
        signed_in,
        plan: signed_in.then(|| "plus".into()),
    }
}

fn code() -> DeviceCode {
    DeviceCode(DeviceCodeLogin {
        verification_url: URL.into(),
        user_code: CODE.into(),
    })
}

impl Fixture {
    /// Move the clock on and let the host run a pass, as a scheduled
    /// repaint would.
    fn later(&mut self, millis: i64) {
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
}

#[test]
fn the_new_bot_form_creates_a_named_bot_which_arrives_as_an_event() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    assert!(fixture.shows("New Bot"));
    assert!(!fixture.enabled(BokkieAction::CreateBot), "no name yet");
    fixture.send(BokkieIntent::EditBotName("  Gardener ".into()));
    assert!(fixture.enabled(BokkieAction::CreateBot));
    fixture.click("Create Bot");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::CreateBot(CreateBotRequest {
            name: "Gardener".into(),
            label: String::new(),
            description: String::new(),
            avatar: None,
        })]
    );
    assert_eq!(fixture.fact("creating"), true.into());
    assert!(fixture.shows("Creating…"));
    assert!(!fixture.enabled(BokkieAction::CreateBot));
    let created = bot(2, "Gardener", "", BotStatus::Idle, None);
    fixture.reply(ApiReply::Created(Ok(created.clone())));
    assert_eq!(fixture.fact("creating"), false.into());
    assert_eq!(fixture.fact("new_bot_name"), "".into());
    // The list takes the Bot from its event.
    assert!(!fixture.shows("Open Gardener"));
    fixture.event(11, Event::BotCreated { bot: created });
    assert!(fixture.shows("Open Gardener"));
    assert_eq!(fixture.fact("bot_count"), 2_u64.into());
    // A Bot with no label shows its status alone.
    assert!(fixture.shows("Idle"));
}

#[test]
fn a_refused_name_stays_to_be_corrected() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.send(BokkieIntent::EditBotName("Tutor".into()));
    fixture.click("Create Bot");
    let _ = fixture.sent();
    let taken = "You already have a Bot with that name";
    fixture.reply(ApiReply::Created(Err(error(
        409,
        ErrorCode::Conflict,
        taken,
    ))));
    assert_eq!(fixture.fact("create_error"), taken.into());
    assert!(fixture.shows(taken));
    assert_eq!(fixture.fact("new_bot_name"), "Tutor".into());
    assert!(fixture.enabled(BokkieAction::CreateBot));
}

#[test]
fn typing_a_name_and_pressing_enter_creates_the_bot() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture
        .harness
        .get_by_role_and_label(egui::accesskit::Role::TextInput, "Name")
        .click();
    fixture.harness.run();
    fixture
        .harness
        .get_by_role_and_label(egui::accesskit::Role::TextInput, "Name")
        .type_text("Gardener");
    fixture.harness.run();
    fixture.harness.key_press(egui::Key::Enter);
    fixture.harness.run();
    assert!(matches!(
        fixture.sent().as_slice(),
        [ApiRequest::CreateBot(request)] if request.name == "Gardener"
    ));
}

#[test]
fn a_signed_in_account_needs_no_sign_in() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    assert_eq!(fixture.fact("chatgpt"), "checking".into());
    assert!(fixture.shows("ChatGPT: checking…"));
    fixture.reply(ApiReply::Account(Ok(account(true))));
    assert_eq!(fixture.fact("chatgpt"), "signed_in".into());
    assert!(fixture.shows("ChatGPT: signed in (plus)"));
    assert!(!fixture.shows("Sign in to ChatGPT"));
    assert!(!fixture.enabled(BokkieAction::ChatGptSignIn));
}

#[test]
fn a_device_code_sign_in_shows_the_code_and_polls_until_signed_in() {
    let mut fixture = Fixture::new();
    fixture.sent.set_now(Timestamp(NOW));
    fixture.signed_in_and_live(10);
    fixture.reply(ApiReply::Account(Ok(account(false))));
    assert_eq!(fixture.fact("chatgpt"), "signed_out".into());
    assert!(fixture.shows("ChatGPT is not signed in, so your Bots cannot reply"));
    fixture.click("Sign in to ChatGPT");
    assert_eq!(fixture.fact("screen"), "chatgpt".into());
    assert_eq!(fixture.fact("chatgpt_login"), "ready".into());
    // The account is checked again on the way in.
    assert_eq!(fixture.sent(), [ApiRequest::CodexAccount]);
    assert!(
        !fixture.enabled(BokkieAction::StartChatGpt),
        "still checking"
    );
    fixture.reply(ApiReply::Account(Ok(account(false))));
    assert!(fixture.enabled(BokkieAction::StartChatGpt));
    fixture.click("Start");
    assert_eq!(fixture.sent(), [ApiRequest::CodexLogin]);
    assert_eq!(fixture.fact("chatgpt_login"), "starting".into());
    assert!(fixture.shows("Asking for a code…"));
    fixture.reply(ApiReply::Login(Ok(code())));
    assert_eq!(fixture.fact("chatgpt_login"), "waiting".into());
    assert!(fixture.shows(URL) && fixture.shows(CODE));
    assert!(fixture.shows("Waiting for you to finish signing in…"));
    fixture.code_kept_off_the_record();
    // No poll before the interval has passed.
    fixture.later(1_000);
    assert!(fixture.sent().is_empty());
    let poll = LOGIN_POLL.as_millis() as i64;
    fixture.later(poll);
    assert_eq!(fixture.sent(), [ApiRequest::CodexAccount]);
    // One poll at a time.
    fixture.later(poll);
    assert!(fixture.sent().is_empty());
    fixture.reply(ApiReply::Account(Ok(account(false))));
    assert_eq!(fixture.fact("chatgpt_login"), "waiting".into());
    // A poll that fails says so and polling goes on.
    fixture.later(poll);
    assert_eq!(fixture.sent(), [ApiRequest::CodexAccount]);
    let down = "The Bots' runtime is unavailable";
    fixture.reply(ApiReply::Account(Err(error(
        503,
        ErrorCode::Unavailable,
        down,
    ))));
    assert!(fixture.shows(down) && fixture.shows(CODE));
    fixture.later(poll);
    assert_eq!(fixture.sent(), [ApiRequest::CodexAccount]);
    fixture.reply(ApiReply::Account(Ok(account(true))));
    assert_eq!(fixture.fact("chatgpt_login"), "done".into());
    assert_eq!(fixture.fact("chatgpt"), "signed_in".into());
    assert!(fixture.shows("Signed in to ChatGPT (plus). Your Bots can reply"));
    assert!(!fixture.shows(CODE), "the code is gone");
    fixture.later(poll);
    assert!(fixture.sent().is_empty(), "polling stopped");
    fixture.click("Back");
    assert_eq!(fixture.fact("screen"), "signed_in".into());
    assert!(fixture.shows("ChatGPT: signed in (plus)"));
}

#[test]
fn leaving_the_sign_in_drops_the_code_and_stops_polling() {
    let mut fixture = Fixture::new();
    fixture.sent.set_now(Timestamp(NOW));
    fixture.signed_in_and_live(10);
    fixture.reply(ApiReply::Account(Ok(account(false))));
    fixture.click("Sign in to ChatGPT");
    fixture.reply(ApiReply::Account(Ok(account(false))));
    fixture.click("Start");
    fixture.reply(ApiReply::Login(Ok(code())));
    let _ = fixture.sent();
    fixture.click("Back");
    assert_eq!(fixture.fact("screen"), "signed_in".into());
    assert_eq!(fixture.fact("chatgpt_login"), "".into());
    assert!(!fixture.shows(CODE));
    fixture.later(LOGIN_POLL.as_millis() as i64 * 3);
    assert!(fixture.sent().is_empty());
    // A code that arrives after leaving is not shown.
    fixture.reply(ApiReply::Login(Ok(code())));
    assert!(!fixture.shows(CODE));
}

#[test]
fn a_failed_start_says_why_and_can_be_tried_again() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.reply(ApiReply::Account(Ok(account(false))));
    fixture.click("Sign in to ChatGPT");
    fixture.reply(ApiReply::Account(Ok(account(false))));
    fixture.click("Start");
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
    fixture.click("Start");
    assert_eq!(fixture.sent(), [ApiRequest::CodexLogin]);
}

#[test]
fn signing_out_drops_a_code_on_screen() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.reply(ApiReply::Account(Ok(account(false))));
    fixture.click("Sign in to ChatGPT");
    fixture.reply(ApiReply::Account(Ok(account(false))));
    fixture.click("Start");
    fixture.reply(ApiReply::Login(Ok(code())));
    fixture.socket(crate::connection::Input::Closed {
        code: Some(crate::connection::CLOSE_SIGNED_OUT),
    });
    assert_eq!(fixture.fact("screen"), "signed_out".into());
    assert!(!fixture.shows(CODE));
}

#[test]
fn the_stand_ins_fit_at_phone_width() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.reply(ApiReply::Account(Ok(account(false))));
    fixture.send(BokkieIntent::EditBotName("Gardener".into()));
    fixture.assert_presentable();
    fixture.click("Sign in to ChatGPT");
    fixture.reply(ApiReply::Account(Ok(account(false))));
    fixture.assert_presentable();
    fixture.click("Start");
    fixture.reply(ApiReply::Login(Ok(code())));
    fixture.assert_presentable();
}

#[test]
fn the_stand_ins_actions_have_readable_ids() {
    use stobie_ui_egui::ActionKey;
    assert_eq!(BokkieAction::CreateBot.stable_id(), "bokkie.create-bot");
    assert_eq!(
        BokkieAction::ChatGptSignIn.stable_id(),
        "bokkie.chatgpt-sign-in"
    );
    assert_eq!(
        BokkieAction::StartChatGpt.stable_id(),
        "bokkie.start-chatgpt"
    );
}

impl Fixture {
    /// Show a device code, as after Start.
    fn waiting_for_code(&mut self) {
        self.reply(ApiReply::Account(Ok(account(false))));
        self.click("Sign in to ChatGPT");
        self.reply(ApiReply::Account(Ok(account(false))));
        self.click("Start");
        self.reply(ApiReply::Login(Ok(code())));
        let _ = self.sent();
        assert_eq!(self.fact("chatgpt_login"), "waiting".into());
    }
}

#[test]
fn the_verification_link_opens_in_a_new_tab_so_the_code_stays_on_screen() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.waiting_for_code();
    assert!(fixture.shows(
        "Keep this page open: the code is shown only here. If it has expired, start again."
    ));
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
fn an_expired_code_can_be_replaced() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.waiting_for_code();
    assert!(fixture.enabled(BokkieAction::StartChatGpt));
    fixture.click("Start");
    assert_eq!(fixture.sent(), [ApiRequest::CodexLogin]);
    assert!(!fixture.shows(CODE), "the old code goes at once");
    fixture.reply(ApiReply::Login(Ok(DeviceCode(DeviceCodeLogin {
        verification_url: URL.into(),
        user_code: "ABCD-5678".into(),
    }))));
    assert!(fixture.shows("ABCD-5678"));
    assert!(!fixture.shows(CODE));
}

#[test]
fn reopening_the_sign_in_after_a_failure_checks_the_account_before_start() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.reply(ApiReply::Account(Ok(account(false))));
    fixture.click("Sign in to ChatGPT");
    fixture.reply(ApiReply::Account(Ok(account(false))));
    fixture.click("Start");
    fixture.reply(ApiReply::Login(Err(error(
        503,
        ErrorCode::Unavailable,
        "The Bots' runtime is unavailable",
    ))));
    assert!(fixture.enabled(BokkieAction::StartChatGpt));
    // Leaving and coming back checks the account again.
    fixture.click("Back");
    fixture.click("Sign in to ChatGPT");
    assert!(
        !fixture.enabled(BokkieAction::StartChatGpt),
        "still checking"
    );
}
