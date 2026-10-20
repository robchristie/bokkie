//! The Bot list: each Bot's row with its avatar, name, preview, time and
//! unread dot; the strip of pinned Bots above the list on a phone; and the
//! account and this device at the bottom of the list.

use bokkie_protocol::api::MessageState;
use bokkie_protocol::events::Event;
use stobie_ui_egui::{ActionArgument, Availability, UiRole};

use super::conversation::{TUTOR, hello};
use super::navigation::{DESKTOP, PHONE, TABLET};
use super::*;

/// 10:50 PM on Sunday 12 April 2026 in Adelaide, Ada's zone (1:20 PM UTC).
const NOW: i64 = 1_776_000_000_000;
const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;

/// What each row's description ends with: what the row does.
const OPENS: &str = "Open the conversation of the Bot with this ID";

fn said(bot: &mut Bot, author: Author, text: &str, at: i64) {
    bot.last_message = Some(MessagePreview {
        author,
        text: text.into(),
        at: Timestamp(at),
    });
}

/// Signed in on a page of `size` at [`NOW`], with `bots` listed.
fn listing(size: egui::Vec2, bots: Vec<Bot>) -> Fixture {
    let mut fixture = Fixture::new();
    fixture.resize(size);
    fixture.sent.set_now(Timestamp(NOW));
    let _ = fixture.sent();
    fixture.sign_in();
    fixture.reply(ApiReply::Bots(Ok(BotList {
        unread_bots: bots.iter().filter(|bot| bot.unread).count() as u32,
        bots,
        seq: Seq(1),
    })));
    fixture
}

impl Fixture {
    /// The control in the strip of pinned Bots that opens `name`, if the
    /// strip presents one.
    fn pinned(&self, name: &str) -> Option<stobie_ui_egui::UiNode> {
        let open = stobie_ui_egui::ActionKey::stable_id(BokkieAction::OpenPinned);
        self.host()
            .ui_snapshot()
            .nodes
            .iter()
            .find(|node| node.name == name && node.actions.iter().any(|action| action.0 == open))
            .cloned()
    }

    fn open_pinned(&mut self, name: &str) {
        let action = stobie_ui_egui::ActionKey::specification(BokkieAction::OpenPinned).description;
        self.harness
            .query_all_by_label(name)
            .find(|node| {
                egui_kittest::kittest::NodeT::accesskit_node(node)
                    .description()
                    .is_some_and(|description| description.ends_with(action))
            })
            .unwrap_or_else(|| panic!("no pinned Bot {name:?}"))
            .click();
        self.harness.run();
    }
}

#[test]
fn each_row_gives_the_name_preview_time_and_unread_state() {
    let mut planner = bot(1, "Meal Planner", "Plans dinners", BotStatus::Idle, None);
    // The preview is one line, however the message was written.
    said(&mut planner, Author::Bot, "Tacos\non   Friday?", NOW - HOUR);
    planner.unread = true;
    let mut tutor = bot(2, "Tutor", "Maths help", BotStatus::Working, None);
    said(&mut tutor, Author::User, "What is 7 x 8?", NOW - DAY);
    let mut chef = bot(3, "Chef", "Cooks", BotStatus::Idle, None);
    said(&mut chef, Author::Bot, "Done", NOW - 3 * DAY);
    let mut gardener = bot(4, "Gardener", "Plans the garden", BotStatus::Idle, None);
    said(
        &mut gardener,
        Author::Bot,
        "Water the beans",
        NOW - 10 * DAY,
    );
    let quiet = bot(5, "Quiet", "Reads the news", BotStatus::Refused, None);
    let fixture = listing(PHONE, vec![planner, tutor, chef, gardener, quiet]);
    // Each row is named by its Bot; its description leads with the unread
    // words, which paint the unread dot, then the preview, then the time:
    // the time of day today, otherwise the day.
    assert_eq!(
        fixture.row_says("Meal Planner"),
        format!("Unread; Tacos on Friday?; 9:50 PM; {OPENS}")
    );
    assert_eq!(
        fixture.row_says("Tutor"),
        format!("Working · You: What is 7 x 8?; Yesterday; {OPENS}")
    );
    assert_eq!(fixture.row_says("Chef"), format!("Done; Thursday; {OPENS}"));
    assert_eq!(
        fixture.row_says("Gardener"),
        format!("Water the beans; 2 Apr; {OPENS}")
    );
    // Before its first message, a Bot's label stands in, with no time.
    assert_eq!(
        fixture.row_says("Quiet"),
        format!("Unavailable · Reads the news; {OPENS}")
    );
    // The row is the control: a button named by the Bot, as the label
    // assistive technology reads; the unread state is no longer text.
    assert!(fixture.shows("Meal Planner"));
    assert!(!fixture.shows("Unread · Plans dinners · Idle"));
    assert!(!fixture.shows("Open Meal Planner"));
    assert!(fixture.row("Meal Planner").unwrap().argument == Some(serde_json::json!(1)));
    fixture.assert_presentable();
}

#[test]
fn the_open_bots_row_is_the_current_one() {
    let mut fixture = Fixture::new();
    fixture.resize(TABLET);
    fixture.signed_in_and_live(10);
    assert!(!fixture.row("Tutor").unwrap().selected);
    fixture.open_tutor(hello(), 10);
    // Beside the conversation, the list marks the Bot on show.
    assert!(fixture.row("Tutor").unwrap().selected);
}

#[test]
fn a_row_follows_its_bot_live() {
    let mut fixture = Fixture::new();
    fixture.sent.set_now(Timestamp(NOW));
    fixture.signed_in_and_live(10);
    let mut message =
        super::conversation::message(5, Author::Bot, "All done", MessageState::Complete);
    message.updated_at = Timestamp(NOW - 2 * HOUR);
    fixture.event(11, Event::MessageCreated { message });
    assert_eq!(
        fixture.row_says("Tutor"),
        format!("Unread; All done; 8:50 PM; {OPENS}")
    );
}

#[test]
fn the_strip_holds_only_pinned_bots_and_only_on_a_phone() {
    let mut tutor = bot(1, "Tutor", "Maths help", BotStatus::Idle, Some("Hi"));
    tutor.pinned = true;
    let planner = bot(2, "Meal Planner", "Plans dinners", BotStatus::Idle, None);
    let mut gardener = bot(3, "Gardener", "Plans the garden", BotStatus::Idle, None);
    gardener.pinned = true;
    gardener.hidden = true;
    let mut fixture = listing(PHONE, vec![tutor, planner, gardener]);
    assert_eq!(fixture.fact("strip"), "1".into());
    assert!(fixture.pinned("Tutor").is_some());
    assert!(fixture.pinned("Meal Planner").is_none());
    // A hidden pinned Bot joins the strip with the hidden Bots.
    assert!(fixture.pinned("Gardener").is_none());
    fixture.click("Show hidden Bots");
    assert_eq!(fixture.fact("strip"), "1,3".into());
    assert!(fixture.pinned("Gardener").is_some());
    fixture.click("Show hidden Bots");
    // The strip's own action refuses a Bot that is not in it.
    let strip = ActionTarget::application(BokkieAction::OpenPinned);
    let availability = |fixture: &Fixture, id: f64| {
        fixture
            .app()
            .availability_with(strip, &ActionArgument::Number(id))
    };
    assert_eq!(availability(&fixture, 1.0), Availability::Enabled);
    assert_eq!(availability(&fixture, 2.0), Availability::Hidden);
    assert_eq!(availability(&fixture, 3.0), Availability::Hidden);
    fixture.assert_presentable();

    // Its name opens the Bot, as its row does.
    fixture.open_pinned("Tutor");
    assert_eq!(fixture.fact("bot"), 1_i64.into());
    assert_eq!(fixture.sent(), [ApiRequest::LoadMessages(TUTOR)]);
    // With the conversation over the list, there is no strip.
    assert_eq!(fixture.fact("strip"), "".into());
    assert!(fixture.pinned("Tutor").is_none());
    fixture.click("Back");
    assert_eq!(fixture.fact("strip"), "1".into());

    // Wider, the list has room for every Bot, pinned ones first.
    for size in [TABLET, DESKTOP] {
        fixture.resize(size);
        assert_eq!(fixture.fact("strip"), "".into(), "{size:?}");
        assert!(fixture.pinned("Tutor").is_none(), "{size:?}");
        assert_eq!(availability(&fixture, 1.0), Availability::Hidden);
        assert!(fixture.lists("Tutor"));
    }
}

#[test]
fn without_pinned_bots_there_is_no_strip() {
    let fixture = listing(
        PHONE,
        vec![bot(1, "Tutor", "Maths help", BotStatus::Idle, None)],
    );
    assert_eq!(fixture.fact("strip"), "".into());
    assert!(fixture.pinned("Tutor").is_none());
}

#[test]
fn refresh_bots_stays_beside_a_conversation_and_the_accounts() {
    let mut fixture = Fixture::new();
    fixture.resize(TABLET);
    fixture.signed_in_and_live(10);
    fixture.open_tutor(hello(), 10);
    fixture.click("Refresh Bots");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::ListBots, ApiRequest::CodexAccount]
    );
    assert!(!fixture.enabled(BokkieAction::RefreshBots), "loading");
    fixture.reply(ApiReply::Bots(Ok(super::conversation::tutor_list(
        BotStatus::Idle,
        10,
    ))));
    // The conversation stays open.
    assert_eq!(fixture.fact("bot"), 1_i64.into());
    fixture.click("Accounts");
    let _ = fixture.sent();
    fixture.click("Refresh Bots");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::ListBots, ApiRequest::CodexAccount]
    );
    assert_eq!(fixture.fact("content"), "admin".into());
}

#[test]
fn the_account_and_this_device_sit_at_the_bottom_of_the_list() {
    let fixture = listing(
        PHONE,
        vec![bot(1, "Tutor", "Maths help", BotStatus::Idle, None)],
    );
    for label in ["Signed in as Ada", "Refresh Bots", "Sign out", "Accounts"] {
        assert!(fixture.shows(label), "{label}");
    }
    let snapshot = fixture.host().ui_snapshot();
    // The notifications control is Stobie's switch, off until switched on.
    let switch = snapshot
        .by_name("Notifications on this device")
        .find(|node| node.role == UiRole::Switch)
        .expect("the notifications switch");
    assert!(switch.actions.iter().any(
        |action| action.0 == stobie_ui_egui::ActionKey::stable_id(BokkieAction::Notifications)
    ));
    // Below a short list, the block sits at the foot of the pane.
    let sign_out = snapshot.by_name("Sign out").next().unwrap();
    assert!(sign_out.rect.max_y > PHONE.y * 0.8, "{:?}", sign_out.rect);
    let row = fixture.row("Tutor").unwrap();
    assert!(row.rect.max_y < PHONE.y * 0.4, "{:?}", row.rect);
}
