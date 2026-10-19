//! The New Bot form, a stand-in of settled decision 8, against scripted
//! replies.

use bokkie_protocol::api::CreateBotRequest;
use bokkie_protocol::events::Event;

use super::*;

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
