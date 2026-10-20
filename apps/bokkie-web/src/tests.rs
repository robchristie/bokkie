//! The application's state transitions on the Stobie host, with a recording
//! transport: requests are checked as sent and replies injected as intents,
//! as the browser transport delivers them. Controls are clicked through the
//! host's UI, and facts read through Stobie's inspection.

use bokkie_protocol::api::{
    Author, Bot, BotList, BotStatus, ErrorCode, LoginRequest, MessagePreview, Role,
    SessionResponse, User,
};
use bokkie_protocol::events::Seq;
use bokkie_protocol::{BotId, Timestamp, UserId};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use stobie_ui_egui::{
    ActionTarget, AppHost, Application, Inspection, InspectionOperation, InspectionRequest,
    InspectionResult, ObservationValue,
};

use super::*;
use crate::api::{ApiReply, ApiRequest, Generation, Recorder};
use crate::socket::{Driver, Io, Step};
use crate::wire::CommandError;

mod admin;
mod conversation;
mod household;
mod limits;
mod member;
mod reading;
mod settings;
mod stand_ins;

struct Fixture {
    harness: Harness<'static, AppHost<App>>,
    sent: Recorder,
    /// The event socket, driven by hand as the browser transport drives it.
    driver: Driver,
    /// The sign-in the socket was last connected for.
    socket_generation: Generation,
    /// What the driver asked the transport to do, in order.
    io: Vec<Io>,
}

fn app(sent: &Recorder) -> App {
    let sent = sent.clone();
    App::new(Box::new(move |_| Box::new(sent)))
}

impl Fixture {
    fn new() -> Self {
        let sent = Recorder::default();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(
                |ui, host: &mut AppHost<App>| host.present(ui),
                AppHost::new(&egui::Context::default(), app(&Recorder::default())),
            );
        // The host must share the harness's context to be woken through it.
        let context = harness.ctx.clone();
        *harness.state_mut() = AppHost::new(&context, app(&sent));
        harness
            .state_mut()
            .enable_inspection(Inspection::new("bokkie-test"));
        harness.run();
        Self {
            harness,
            sent,
            driver: Driver::new(),
            socket_generation: Generation::default(),
            io: Vec::new(),
        }
    }

    fn host(&self) -> &AppHost<App> {
        self.harness.state()
    }

    fn app(&self) -> &App {
        self.host().app()
    }

    fn send(&mut self, intent: BokkieIntent) {
        self.host().intent_sender().send(intent).unwrap();
        self.harness.run();
    }

    /// A reply in the current generation, as for a command just sent.
    fn reply(&mut self, reply: ApiReply) {
        let generation = self.app().generation();
        self.send(BokkieIntent::Reply { generation, reply });
    }

    fn sent(&self) -> Vec<ApiRequest> {
        self.sent.take()
    }

    fn click(&mut self, label: &str) {
        self.harness.get_by_label(label).click();
        self.harness.run();
    }

    /// Scroll the text or control `label` into view, as a person would.
    /// Stobie presents a control only while some of it is in view.
    fn reveal(&mut self, label: &str) {
        self.harness.get_by_label(label).scroll_to_me();
        self.harness.run();
    }

    /// Whether the page presents this text. Stobie's observation leaves
    /// content text out, so this reads egui's AccessKit tree.
    fn shows(&self, text: &str) -> bool {
        self.harness.query_all_by_label(text).next().is_some()
    }

    /// One fact from the observation Stobie publishes to agents.
    fn fact(&self, key: &str) -> ObservationValue {
        let inspection = self.host().inspection().unwrap();
        let reply = inspection.handle(InspectionRequest {
            version: 1,
            request_id: "observe".into(),
            instance: Some(inspection.instance()),
            operation: InspectionOperation::Observe,
        });
        let Some(InspectionResult::Observe { observation }) = reply.result else {
            panic!("observe failed");
        };
        observation.facts[key].clone()
    }

    fn enabled(&self, action: BokkieAction) -> bool {
        self.app()
            .availability(ActionTarget::application(action))
            .enabled()
    }

    /// Sign in as `ada`, an admin, through the form.
    fn sign_in(&mut self) {
        self.sign_in_with(session(user()));
    }

    /// Sign in through the form, as the gateway's answer says.
    fn sign_in_with(&mut self, session: SessionResponse) {
        self.reply(ApiReply::Session(Err(unauthenticated())));
        let username = session.user.username.clone();
        self.send(BokkieIntent::EditUsername(format!(" {username} ")));
        self.send(BokkieIntent::EditPassword("a long password".into()));
        self.click("Sign in");
        assert_eq!(
            self.sent(),
            [ApiRequest::SignIn(LoginRequest {
                username,
                password: "a long password".into(),
            })]
        );
        self.reply(ApiReply::SignedIn(Ok(session)));
        assert_eq!(
            self.sent(),
            [
                ApiRequest::ListBots,
                ApiRequest::LoadUsage,
                ApiRequest::CodexAccount
            ]
        );
    }
}

fn user() -> User {
    User {
        id: UserId(1),
        username: "ada".into(),
        display_name: "Ada".into(),
        role: Role::Admin,
        disabled: false,
        created_at: Timestamp(1_000),
        permissions: Default::default(),
        oversight: false,
        oversight_changed_at: None,
        time_zone: "Australia/Adelaide".into(),
    }
}

/// Grace, a member with both permissions and oversight off.
fn member() -> User {
    User {
        id: UserId(2),
        username: "grace".into(),
        display_name: "Grace".into(),
        role: Role::Member,
        ..user()
    }
}

fn session(user: User) -> SessionResponse {
    SessionResponse {
        user,
        password_reset_by_admin: None,
    }
}

fn bot(id: i64, name: &str, label: &str, status: BotStatus, preview: Option<&str>) -> Bot {
    Bot {
        read_up_to: None,
        unread: false,
        id: BotId(id),
        name: name.into(),
        label: label.into(),
        description: String::new(),
        avatar: None,
        pinned: false,
        hidden: false,
        status,
        created_at: Timestamp(1_000),
        updated_at: Timestamp(1_000 + id),
        last_message: preview.map(|text| MessagePreview {
            author: Author::Bot,
            text: text.into(),
            at: Timestamp(2_000 + id),
        }),
        grants: Vec::new(),
    }
}

fn error(status: u16, code: ErrorCode, message: &str) -> CommandError {
    CommandError {
        status,
        code,
        message: message.into(),
    }
}

fn unauthenticated() -> CommandError {
    error(401, ErrorCode::Unauthenticated, "Sign in to continue")
}

#[test]
fn startup_checks_the_session_and_shows_the_sign_in_form() {
    let mut fixture = Fixture::new();
    assert_eq!(fixture.sent(), [ApiRequest::CheckSession]);
    assert_eq!(fixture.fact("screen"), "checking".into());
    assert!(fixture.shows("Checking your session…"));
    fixture.reply(ApiReply::Session(Err(unauthenticated())));
    assert_eq!(fixture.fact("screen"), "signed_out".into());
    // An ordinary signed-out answer is not an error to show.
    assert_eq!(fixture.fact("message"), "".into());
    assert!(fixture.shows("Username") && fixture.shows("Password"));
    assert!(
        !fixture.harness.ctx.has_requested_repaint(),
        "idle until input"
    );
}

#[test]
fn a_valid_session_skips_the_form() {
    let mut fixture = Fixture::new();
    let _ = fixture.sent();
    fixture.reply(ApiReply::Session(Ok(session(user()))));
    assert_eq!(fixture.fact("screen"), "signed_in".into());
    assert_eq!(fixture.fact("user"), "ada".into());
    assert_eq!(fixture.fact("bots"), "loading".into());
    assert_eq!(
        fixture.sent(),
        [
            ApiRequest::ListBots,
            ApiRequest::LoadUsage,
            ApiRequest::CodexAccount
        ]
    );
    assert!(fixture.shows("Loading Bots…"));
}

#[test]
fn an_unreachable_server_is_said_on_the_sign_in_form() {
    let mut fixture = Fixture::new();
    fixture.reply(ApiRequest::CheckSession.unreachable());
    assert_eq!(fixture.fact("screen"), "signed_out".into());
    assert_eq!(
        fixture.fact("message"),
        CommandError::unreachable().message.into()
    );
}

#[test]
fn sign_in_needs_both_fields_and_sends_them_once() {
    let mut fixture = Fixture::new();
    let _ = fixture.sent();
    fixture.reply(ApiReply::Session(Err(unauthenticated())));
    assert!(!fixture.enabled(BokkieAction::SignIn));
    fixture.send(BokkieIntent::EditUsername("ada".into()));
    assert!(!fixture.enabled(BokkieAction::SignIn), "no password yet");
    fixture.send(BokkieIntent::EditPassword("a long password".into()));
    assert!(fixture.enabled(BokkieAction::SignIn));
    fixture.click("Sign in");
    assert_eq!(fixture.sent().len(), 1);
    assert_eq!(fixture.fact("screen"), "signing_in".into());
    assert!(fixture.shows("Signing in…"));
    // While it is under way, nothing else is sent and the form is fixed.
    assert!(!fixture.enabled(BokkieAction::SignIn));
    fixture.click("Sign in");
    fixture.send(BokkieIntent::EditUsername("grace".into()));
    assert!(fixture.sent().is_empty());
}

#[test]
fn a_refused_sign_in_shows_the_gateways_message_and_clears_the_password() {
    let mut fixture = Fixture::new();
    let _ = fixture.sent();
    fixture.reply(ApiReply::Session(Err(unauthenticated())));
    fixture.send(BokkieIntent::EditUsername("ada".into()));
    fixture.send(BokkieIntent::EditPassword("the wrong one".into()));
    fixture.click("Sign in");
    let refused = "The username or password is wrong";
    fixture.reply(ApiReply::SignedIn(Err(error(
        401,
        ErrorCode::Unauthenticated,
        refused,
    ))));
    assert_eq!(fixture.fact("screen"), "signed_out".into());
    assert_eq!(fixture.fact("message"), refused.into());
    assert!(fixture.shows(refused));
    assert!(
        !fixture.enabled(BokkieAction::SignIn),
        "the password is gone"
    );
    // The username stays for the next try.
    fixture.send(BokkieIntent::EditPassword("a long password".into()));
    assert!(fixture.enabled(BokkieAction::SignIn));

    let limited = "Too many sign-in attempts. Try again in a few minutes";
    fixture.click("Sign in");
    fixture.reply(ApiReply::SignedIn(Err(error(
        429,
        ErrorCode::RateLimited,
        limited,
    ))));
    assert_eq!(fixture.fact("message"), limited.into());
    assert!(fixture.shows(limited));
}

#[test]
fn the_password_is_never_observed() {
    let mut fixture = Fixture::new();
    fixture.reply(ApiReply::Session(Err(unauthenticated())));
    fixture.send(BokkieIntent::EditPassword("correct horse battery".into()));
    let snapshot = fixture.host().ui_snapshot();
    let json = serde_json::to_string(snapshot).unwrap();
    assert!(!json.contains("correct horse battery"), "{json}");
    assert!(
        !format!(
            "{:?}",
            BokkieIntent::EditPassword("correct horse battery".into())
        )
        .contains("correct horse")
    );
}

#[test]
fn signing_in_loads_and_lists_the_bots() {
    let mut fixture = Fixture::new();
    let _ = fixture.sent();
    fixture.sign_in();
    assert_eq!(fixture.fact("screen"), "signed_in".into());
    assert!(fixture.shows("Signed in as Ada"));
    assert!(!fixture.enabled(BokkieAction::Refresh), "already loading");
    let mut hidden = bot(3, "Hidden", "Secret", BotStatus::Idle, None);
    hidden.hidden = true;
    fixture.reply(ApiReply::Bots(Ok(BotList {
        unread_bots: 0,
        bots: vec![
            bot(
                1,
                "Meal Planner",
                "Plans dinners",
                BotStatus::Idle,
                Some("Tacos on Friday?"),
            ),
            bot(2, "Tutor", "Maths help", BotStatus::Working, None),
            hidden,
        ],
        seq: Seq(7),
    })));
    assert_eq!(fixture.fact("bots"), "loaded".into());
    assert_eq!(fixture.fact("bot_count"), 2_u64.into());
    for text in [
        "Meal Planner",
        "Plans dinners · Idle",
        "Tacos on Friday?",
        "Tutor",
        "Maths help · Working",
    ] {
        assert!(fixture.shows(text), "{text}");
    }
    assert!(!fixture.shows("Hidden"));
    assert!(fixture.enabled(BokkieAction::Refresh));
}

#[test]
fn an_empty_account_says_so() {
    let mut fixture = Fixture::new();
    let _ = fixture.sent();
    fixture.sign_in();
    fixture.reply(ApiReply::Bots(Ok(BotList {
        unread_bots: 0,
        bots: Vec::new(),
        seq: Seq(0),
    })));
    assert!(fixture.shows("No Bots yet"));
}

#[test]
fn a_failed_list_can_be_refreshed() {
    let mut fixture = Fixture::new();
    let _ = fixture.sent();
    fixture.sign_in();
    let down = "Bokkie's server is not available. Try again shortly";
    fixture.reply(ApiReply::Bots(Err(error(
        502,
        ErrorCode::Unavailable,
        down,
    ))));
    assert_eq!(fixture.fact("bots"), "failed".into());
    assert_eq!(fixture.fact("message"), down.into());
    assert!(fixture.shows(down));
    fixture.click("Refresh");
    // The household's sign-in announces no events, so it is asked too.
    assert_eq!(
        fixture.sent(),
        [ApiRequest::ListBots, ApiRequest::CodexAccount]
    );
    assert_eq!(fixture.fact("bots"), "loading".into());
    fixture.reply(ApiReply::Bots(Ok(BotList {
        unread_bots: 0,
        bots: vec![bot(1, "Tutor", "Maths help", BotStatus::Idle, None)],
        seq: Seq(1),
    })));
    assert!(fixture.shows("Tutor"));
    assert_eq!(fixture.fact("message"), "".into());
}

#[test]
fn a_session_that_ends_while_loading_returns_to_the_form() {
    let mut fixture = Fixture::new();
    let _ = fixture.sent();
    fixture.sign_in();
    fixture.reply(ApiReply::Bots(Err(unauthenticated())));
    assert_eq!(fixture.fact("screen"), "signed_out".into());
    assert_eq!(
        fixture.fact("message"),
        "Your session has ended. Sign in again".into()
    );
}

#[test]
fn sign_out_returns_to_the_form_and_ignores_late_replies() {
    let mut fixture = Fixture::new();
    let _ = fixture.sent();
    fixture.sign_in();
    fixture.click("Sign out");
    assert_eq!(fixture.sent(), [ApiRequest::SignOut]);
    assert_eq!(fixture.fact("screen"), "signing_out".into());
    assert!(!fixture.enabled(BokkieAction::SignOut));
    fixture.reply(ApiReply::SignedOut(Ok(())));
    assert_eq!(fixture.fact("screen"), "signed_out".into());
    assert_eq!(fixture.fact("user"), "".into());
    // The Bot list asked for before signing out arrives late: it is dropped.
    fixture.reply(ApiReply::Bots(Ok(BotList {
        unread_bots: 0,
        bots: vec![bot(1, "Tutor", "Maths help", BotStatus::Idle, None)],
        seq: Seq(1),
    })));
    assert_eq!(fixture.fact("screen"), "signed_out".into());
    assert_eq!(fixture.fact("bot_count"), 0_u64.into());
}

#[test]
fn a_failed_sign_out_stays_signed_in_and_says_why() {
    let mut fixture = Fixture::new();
    let _ = fixture.sent();
    fixture.sign_in();
    fixture.click("Sign out");
    let _ = fixture.sent();
    fixture.reply(ApiRequest::SignOut.unreachable());
    assert_eq!(fixture.fact("screen"), "signed_in".into());
    assert!(fixture.shows(&CommandError::unreachable().message));
    assert!(fixture.enabled(BokkieAction::SignOut));
    // A session already gone counts as signed out.
    fixture.click("Sign out");
    fixture.reply(ApiReply::SignedOut(Err(unauthenticated())));
    assert_eq!(fixture.fact("screen"), "signed_out".into());
}

impl Fixture {
    /// Every button is on screen at a usable size, and the audits are clean.
    fn assert_presentable(&self) {
        let snapshot = self.host().ui_snapshot();
        assert!(snapshot.by_name(APPLICATION_NAME).next().is_some());
        assert!(
            snapshot.semantic_audit.is_empty(),
            "{:?}",
            snapshot.semantic_audit
        );
        let buttons: Vec<_> = snapshot.by_role(stobie_ui_egui::UiRole::Button).collect();
        assert!(!buttons.is_empty());
        for button in buttons {
            let width = button.rect.max_x - button.rect.min_x;
            assert!(width >= 40.0 && button.rect.max_x <= 390.0, "{button:?}");
        }
    }
}

#[test]
fn both_screens_present_their_controls_at_phone_width() {
    let mut fixture = Fixture::new();
    let _ = fixture.sent();
    fixture.reply(ApiReply::Session(Err(unauthenticated())));
    fixture.assert_presentable();
    fixture.sign_in();
    fixture.reply(ApiReply::Bots(Ok(BotList {
        unread_bots: 0,
        bots: vec![bot(1, "Tutor", "Maths help", BotStatus::Idle, Some("Hi"))],
        seq: Seq(1),
    })));
    fixture.assert_presentable();
    assert!(fixture.shows("Sign out") && fixture.shows("Refresh"));
}

/// The page links the licences of the fonts the application embeds, and the
/// notices name each font. `scripts/check-web-notices` checks the texts.
#[test]
fn the_page_links_the_font_notices() {
    let page = include_str!("../web/index.html");
    assert!(page.contains(r#"<link rel="license" href="third-party-notices.txt" />"#));
    let notices = include_str!("../web/third-party-notices.txt");
    for font in [
        "Source Sans 3",
        "Hack",
        "Noto Emoji",
        "Ubuntu Light",
        "emoji-icon-font",
    ] {
        assert!(notices.contains(font), "{font}");
    }
    for licence in [
        "SIL OPEN FONT LICENSE Version 1.1",
        "UBUNTU FONT LICENCE Version 1.0",
        "MIT License",
        "BITSTREAM VERA LICENSE",
    ] {
        assert!(notices.contains(licence), "{licence}");
    }
}

#[test]
fn typing_the_password_and_pressing_enter_signs_in() {
    let mut fixture = Fixture::new();
    let _ = fixture.sent();
    fixture.reply(ApiReply::Session(Err(unauthenticated())));
    fixture.send(BokkieIntent::EditUsername("ada".into()));
    let password = fixture
        .harness
        .get_by(|node| node.role() == egui::accesskit::Role::PasswordInput);
    password.click();
    fixture.harness.run();
    let password = fixture
        .harness
        .get_by(|node| node.role() == egui::accesskit::Role::PasswordInput);
    password.type_text("a long password");
    fixture.harness.run();
    fixture.harness.key_press(egui::Key::Enter);
    fixture.harness.run();
    assert_eq!(
        fixture.sent(),
        [ApiRequest::SignIn(LoginRequest {
            username: "ada".into(),
            password: "a long password".into(),
        })]
    );
    assert_eq!(fixture.fact("screen"), "signing_in".into());
}

#[test]
fn a_reply_from_an_earlier_sign_in_is_never_shown_to_the_next_user() {
    let mut fixture = Fixture::new();
    let _ = fixture.sent();
    fixture.sign_in();
    let first: Generation = fixture.app().generation();
    // Ada signs out with her list still on its way, and Grace signs in.
    fixture.click("Sign out");
    fixture.reply(ApiReply::SignedOut(Ok(())));
    fixture.send(BokkieIntent::EditUsername("grace".into()));
    fixture.send(BokkieIntent::EditPassword("another long password".into()));
    fixture.click("Sign in");
    let grace = User {
        id: UserId(2),
        username: "grace".into(),
        display_name: "Grace".into(),
        ..user()
    };
    fixture.reply(ApiReply::SignedIn(Ok(session(grace))));
    let _ = fixture.sent();
    assert_eq!(fixture.fact("bots"), "loading".into());
    // Ada's list arrives now: it is dropped, and Grace's still counts.
    fixture.send(BokkieIntent::Reply {
        generation: first,
        reply: ApiReply::Bots(Ok(BotList {
            unread_bots: 0,
            bots: vec![bot(1, "Ada's Tutor", "Maths help", BotStatus::Idle, None)],
            seq: Seq(4),
        })),
    });
    assert_eq!(fixture.fact("bots"), "loading".into());
    assert!(!fixture.shows("Ada's Tutor"));
    fixture.reply(ApiReply::Bots(Ok(BotList {
        unread_bots: 0,
        bots: vec![bot(
            7,
            "Gardener",
            "Plans the garden",
            BotStatus::Idle,
            None,
        )],
        seq: Seq(9),
    })));
    assert_eq!(fixture.fact("user"), "grace".into());
    assert!(fixture.shows("Gardener") && !fixture.shows("Ada's Tutor"));
}
