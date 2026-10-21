//! The routine sheet, the panel's confirms and the time zone sheet: each
//! opens over what is behind it,
//! takes focus, keeps everything behind it out of reach of the keyboard,
//! an agent and assistive technology, closes by its own controls and
//! Escape, and returns focus to the control that opened it.

use stobie_ui_egui::UiRole;

use super::super::conversation::tutor_list;
use super::super::household::account;
use super::super::limits::usage;
use super::super::navigation::{DESKTOP, PHONE};
use super::*;

impl Fixture {
    /// The routine sheet, open, titled `title`.
    fn sheet(&self, title: &str) -> stobie_ui_egui::UiNode {
        let sheet = self.dialog(UiRole::Dialog).expect("the sheet is open");
        assert_eq!(sheet.name, title);
        sheet
    }

    /// The confirm, open, titled `title` and saying `message`.
    fn confirming(&self, title: &str, message: &str) -> stobie_ui_egui::UiNode {
        let confirm = self
            .dialog(UiRole::AlertDialog)
            .expect("the confirm is open");
        assert_eq!(confirm.name, title);
        assert_eq!(confirm.description.as_deref(), Some(message));
        assert!(self.shows(message));
        assert!(!self.hidden(message));
        confirm
    }

    fn no_dialog(&self) -> bool {
        self.dialog(UiRole::Dialog).is_none() && self.dialog(UiRole::AlertDialog).is_none()
    }

    /// Tab around the dialog: focus never leaves it for what is behind.
    fn tab_stays_inside(&mut self) {
        for _ in 0..12 {
            self.press_key(egui::Key::Tab);
            let label = self.focused_label().expect("a control has focus");
            assert!(!self.hidden(&label), "{label} is behind the dialog");
        }
    }
}

const DELETE: &str = "\"Morning digest\" and its run history are deleted for good. What its \
     runs said stays in the conversation.";
const CLEAR: &str = "Tutor's notes are cleared for good, and it starts its next turn without them.";

#[test]
fn the_routine_sheet_opens_over_the_panel_and_takes_focus() {
    let mut fixture = Fixture::new();
    fixture.settings_loaded(vec![digest()]);
    fixture.reveal("New routine");
    fixture.click("New routine");
    assert_eq!(fixture.fact("routine_form"), "new".into());
    fixture.sheet("New routine");
    assert_eq!(fixture.focused_label().as_deref(), Some("Name"));
    // Nothing behind it is in reach: not by the keyboard, an agent or
    // assistive technology.
    fixture.tab_stays_inside();
    for action in [
        BokkieAction::Refresh,
        BokkieAction::ClearNotes,
        BokkieAction::RefreshBots,
    ] {
        fixture.refused_behind(action, "New routine");
    }
    assert!(fixture.sent().is_empty());
    for behind in ["Clear notes", "Edit routine Morning digest", "Grant notes"] {
        assert!(fixture.hidden(behind), "{behind}");
    }
    assert!(!fixture.hidden("Save routine"));
    assert!(!fixture.hidden("Close without saving"));
}

#[test]
fn escape_and_the_close_control_close_the_sheet_and_focus_returns() {
    let mut fixture = Fixture::new();
    fixture.settings_loaded(vec![digest()]);
    fixture.reveal("New routine");
    fixture.click("New routine");
    fixture.send(BokkieIntent::EditRoutineName("Evening digest".into()));
    fixture.press_key(egui::Key::Escape);
    assert_eq!(fixture.fact("routine_form"), "".into());
    assert!(fixture.no_dialog());
    assert!(!fixture.hidden("Clear notes"));
    assert_eq!(fixture.focused_label().as_deref(), Some("New routine"));
    // Nothing was saved, and the next sheet starts afresh.
    assert!(fixture.sent().is_empty());
    fixture.click("Edit routine Morning digest");
    fixture.sheet("Edit routine");
    fixture.click("Close without saving");
    assert_eq!(fixture.fact("routine_form"), "".into());
    assert!(fixture.no_dialog());
    assert_eq!(
        fixture.focused_label().as_deref(),
        Some("Edit routine Morning digest")
    );
    assert!(fixture.sent().is_empty());
}

#[test]
fn the_sheet_says_what_stops_a_save_and_stays_open_while_it_saves() {
    let mut fixture = Fixture::new();
    fixture.settings_loaded(Vec::new());
    fixture.reveal("New routine");
    fixture.click("New routine");
    // What stops a save is in the sheet, for assistive technology too.
    assert!(fixture.shows("Enter a name"));
    assert!(!fixture.hidden("Enter a name"));
    fixture.send(BokkieIntent::EditRoutineName("Morning digest".into()));
    fixture.send(BokkieIntent::EditRoutinePrompt(
        "Summarise the news.".into(),
    ));
    assert!(!fixture.shows("Enter a name"));
    fixture.reveal("Save routine");
    fixture.click("Save routine");
    assert_eq!(fixture.fact("routine_form"), "saving".into());
    let _ = fixture.sent();
    // While it saves, neither Escape nor the close control closes it.
    assert_eq!(
        fixture.availability_of(BokkieAction::CancelRoutine),
        disabled("Saving the routine")
    );
    fixture.press_key(egui::Key::Escape);
    assert_eq!(fixture.fact("routine_form"), "saving".into());
    fixture.sheet("New routine");
    // The gateway's refusal shows in the sheet, to correct.
    let refused = "This Bot already has a routine called Morning digest";
    fixture.reply(ApiReply::RoutineCreated(
        TUTOR,
        Err(error(409, ErrorCode::Conflict, refused)),
    ));
    fixture.sheet("New routine");
    assert!(fixture.shows(refused));
    assert!(!fixture.hidden(refused));
    fixture.click("Save routine");
    let _ = fixture.sent();
    fixture.reply(ApiReply::RoutineCreated(TUTOR, Ok(digest())));
    assert_eq!(fixture.fact("routine_form"), "".into());
    assert!(fixture.no_dialog());
    assert_eq!(fixture.focused_label().as_deref(), Some("New routine"));
    assert!(fixture.shows("Morning digest"));
}

#[test]
fn deleting_a_routine_asks_in_a_confirm_that_names_it() {
    let mut fixture = Fixture::new();
    fixture.settings_loaded(vec![digest()]);
    fixture.click("Delete Morning digest");
    assert_eq!(fixture.fact("routine_delete"), DIGEST.0.into());
    fixture.confirming("Delete this routine?", DELETE);
    // Keep comes first and takes focus.
    assert_eq!(fixture.focused_label().as_deref(), Some("Keep routine"));
    fixture.tab_stays_inside();
    fixture.refused_behind(BokkieAction::Refresh, "Delete this routine?");
    fixture.refused_behind(BokkieAction::NewRoutine, "Delete this routine?");
    assert!(fixture.hidden("Edit routine Morning digest"));
    // Escape keeps it, as Keep routine does, and focus returns.
    fixture.press_key(egui::Key::Escape);
    assert_eq!(fixture.fact("routine_delete"), 0.into());
    assert!(fixture.no_dialog());
    assert_eq!(
        fixture.focused_label().as_deref(),
        Some("Delete Morning digest")
    );
    fixture.click("Delete Morning digest");
    fixture.click("Keep routine");
    assert_eq!(fixture.fact("routine_delete"), 0.into());
    assert!(fixture.no_dialog());
    assert_eq!(
        fixture.focused_label().as_deref(),
        Some("Delete Morning digest")
    );
    assert!(fixture.sent().is_empty());
    // Confirmed, it closes and the routine's row says it is deleting.
    fixture.click("Delete Morning digest");
    fixture.click("Delete for good");
    assert_eq!(fixture.sent(), [ApiRequest::DeleteRoutine(TUTOR, DIGEST)]);
    assert!(fixture.no_dialog());
    assert!(fixture.shows("Deleting…"));
    assert_eq!(
        fixture.availability_with(BokkieAction::DeleteRoutine, DIGEST.0),
        disabled("Deleting a routine")
    );
    fixture.reply(ApiReply::RoutineDeleted(TUTOR, DIGEST, Ok(())));
    assert_eq!(fixture.fact("routine_count"), 0_u64.into());
}

#[test]
fn clearing_the_notes_asks_in_a_confirm_that_names_the_bot() {
    let mut fixture = Fixture::new();
    fixture.settings_loaded(Vec::new());
    fixture.click("Clear notes");
    fixture.confirming("Clear notes?", CLEAR);
    assert_eq!(fixture.focused_label().as_deref(), Some("Keep notes"));
    fixture.tab_stays_inside();
    fixture.refused_behind(BokkieAction::NewRoutine, "Clear notes?");
    assert!(fixture.hidden("Likes fractions."));
    fixture.press_key(egui::Key::Escape);
    assert_eq!(fixture.fact("notes_clear"), "".into());
    assert!(fixture.no_dialog());
    assert_eq!(fixture.focused_label().as_deref(), Some("Clear notes"));
    assert!(fixture.sent().is_empty());
    fixture.click("Clear notes");
    fixture.click("Clear for good");
    assert_eq!(fixture.sent(), [ApiRequest::ClearNotes(TUTOR)]);
    assert!(fixture.no_dialog());
    assert!(fixture.shows("Clearing the notes…"));
}

#[test]
fn the_sheet_covers_a_phone_and_the_dialogs_are_centred_beside_the_panel() {
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        for size in [PHONE, DESKTOP] {
            let at = format!("{theme:?}, {size:?}");
            let mut fixture = Fixture::panel_at(theme, size, hello());
            let page = fixture.harness.ctx.content_rect();
            let centre = (page.left() + page.right()) / 2.0;
            fixture.reveal("New routine");
            fixture.click("New routine");
            let sheet = fixture.sheet("New routine");
            let width = sheet.rect.max_x - sheet.rect.min_x;
            if size == PHONE {
                // The whole screen, for a form that needs the room.
                assert!(width >= page.width() - 0.5, "{at}: {sheet:?}");
            } else {
                assert!((width - 560.0).abs() < 1.0, "{at}: {sheet:?}");
                let middle = (sheet.rect.min_x + sheet.rect.max_x) / 2.0;
                assert!((middle - centre).abs() < 1.0, "{at}: {sheet:?}");
            }
            fixture.assert_presentable();
            fixture.reveal("Every few hours");
            fixture.click("Every few hours");
            fixture.assert_presentable();
            fixture.press_key(egui::Key::Escape);
            // Tall enough to hold the whole panel, since Stobie presents
            // only what is in view.
            fixture.resize(egui::vec2(size.x, 3_000.0) + egui::vec2(16.0, 16.0));
            for (opener, title) in [
                ("Delete Morning digest", "Delete this routine?"),
                ("Clear notes", "Clear notes?"),
            ] {
                fixture.reveal(opener);
                fixture.click(opener);
                let confirm = fixture
                    .dialog(UiRole::AlertDialog)
                    .unwrap_or_else(|| panic!("{at}: no {title}"));
                assert_eq!(confirm.name, title);
                let middle = (confirm.rect.min_x + confirm.rect.max_x) / 2.0;
                assert!((middle - centre).abs() < 1.0, "{at}: {confirm:?}");
                assert!(
                    confirm.rect.max_x - confirm.rect.min_x <= 400.5,
                    "{at}: {confirm:?}"
                );
                fixture.assert_presentable();
                fixture.press_key(egui::Key::Escape);
                assert!(fixture.no_dialog(), "{at}: {title}");
            }
        }
    }
}

#[test]
fn the_time_zone_sheet_opens_from_the_list_and_closes_without_saving() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.reveal("Change time zone");
    fixture.click("Change time zone");
    fixture.sheet("Your time zone");
    assert_eq!(fixture.focused_label().as_deref(), Some("Time zone"));
    fixture.tab_stays_inside();
    for action in [
        BokkieAction::RefreshBots,
        BokkieAction::SignOut,
        BokkieAction::Notifications,
    ] {
        fixture.refused_behind(action, "Your time zone");
    }
    assert!(fixture.hidden("Change time zone"));
    assert!(fixture.hidden("Refresh Bots"));
    // Escape closes it unsaved, and focus returns.
    fixture.send(BokkieIntent::EditTimeZone("Europe/London".into()));
    fixture.press_key(egui::Key::Escape);
    assert!(fixture.no_dialog());
    assert_eq!(fixture.focused_label().as_deref(), Some("Change time zone"));
    assert!(fixture.sent().is_empty());
    // The next sheet starts again from the account's time zone, and its
    // close control closes it too.
    fixture.click("Change time zone");
    assert_eq!(
        fixture.availability_of(BokkieAction::SaveTimeZone),
        disabled("That is your time zone already")
    );
    fixture.click("Close without saving");
    assert!(fixture.no_dialog());
    assert_eq!(fixture.focused_label().as_deref(), Some("Change time zone"));
    assert!(fixture.sent().is_empty());
}

#[test]
fn the_list_keeps_its_foot_in_view_in_a_desktop_window() {
    // The browser smoke's 1280 by 800 window, with what its list holds: two
    // Bots and a hidden one, the household's sign-in and the meter.
    let mut fixture = Fixture::new();
    fixture.resize(egui::vec2(1_280.0, 800.0) + egui::vec2(16.0, 16.0));
    fixture.signed_in_and_live(10);
    fixture.reply(ApiReply::Account(Ok(account(true))));
    fixture.reply(ApiReply::Usage(Ok(usage(false, false))));
    fixture.click("Refresh Bots");
    let _ = fixture.sent();
    let mut list = tutor_list(BotStatus::Idle, 10);
    list.bots.push(bot(
        2,
        "Planner",
        "The household's meal planner and lists",
        BotStatus::Idle,
        Some("Noted."),
    ));
    list.bots.push(Bot {
        hidden: true,
        ..bot(3, "Archive", "Old notes", BotStatus::Idle, None)
    });
    fixture.reply(ApiReply::Bots(Ok(list)));
    assert!(fixture.shows("Planner"));
    assert!(fixture.shows("Show hidden Bots"));
    let page = fixture.harness.ctx.content_rect();
    for label in ["Sign out", "Accounts"] {
        let rect = fixture.harness.get_by_label(label).rect();
        assert!(
            rect.bottom() <= page.bottom(),
            "{label}: {rect:?} in {page:?}"
        );
    }
}
