//! The navigation shell at a phone's, a tablet's and a desktop's width: the
//! Bot list, the content open from it and the open Bot's settings, as panes
//! Stobie's navigation recipe lays out by the page's width.

use stobie_ui_egui::{DomainReference, UiRole};

use super::conversation::{TUTOR, hello};
use super::*;

/// A phone held upright: one pane at a time, with Back.
const PHONE: egui::Vec2 = egui::vec2(390.0, 844.0);
/// A tablet: the list beside the content.
const TABLET: egui::Vec2 = egui::vec2(800.0, 1_000.0);
/// A desktop: the list, the content and the settings side by side.
const DESKTOP: egui::Vec2 = egui::vec2(1_280.0, 1_000.0);

impl Fixture {
    fn resize(&mut self, size: egui::Vec2) {
        self.harness.set_size(size);
        self.harness.run();
    }

    /// Ada, live with her Tutor listed, on a page of `size`.
    fn live_at(&mut self, size: egui::Vec2) {
        self.resize(size);
        self.signed_in_and_live(10);
    }

    /// The panes on screen, as the `panes` fact says and as the regions
    /// Stobie presents name them: each is checked against the other.
    fn assert_panes(&self, fact: &str, names: &[&str]) {
        assert_eq!(self.fact("panes"), fact.into());
        let snapshot = self.host().ui_snapshot();
        let presented: Vec<_> = snapshot
            .by_role(UiRole::Pane)
            .map(|pane| pane.name.as_str())
            .collect();
        assert_eq!(presented, names);
    }

    /// Whether the control `name` has keyboard focus.
    fn focused(&self, name: &str) -> bool {
        self.host()
            .ui_snapshot()
            .by_name(name)
            .any(|node| node.focused)
    }
}

#[test]
fn the_layout_follows_the_page_width() {
    for (size, layout, panes) in [
        (PHONE, "stack", "list"),
        (TABLET, "split", "list,empty"),
        (DESKTOP, "columns", "list,empty"),
    ] {
        let mut fixture = Fixture::new();
        assert_eq!(fixture.fact("layout"), "".into(), "signed out");
        fixture.live_at(size);
        assert_eq!(fixture.fact("layout"), layout.into());
        assert_eq!(fixture.fact("content"), "".into());
        assert_eq!(fixture.fact("detail"), "".into());
        // The empty pane is a prompt, not a region.
        fixture.assert_panes(panes, &[APPLICATION_NAME]);
        assert_eq!(
            fixture.shows("Choose a Bot to open its conversation"),
            size != PHONE
        );
        // The list's own controls, rearranged: the account, the meter, the
        // time zone and the notifications at its bottom.
        for label in [
            "Refresh",
            "Show hidden Bots",
            "Open Tutor",
            "Sign out",
            "Accounts",
            "ChatGPT sign-in",
            "Notifications",
        ] {
            assert!(fixture.shows(label), "{label} at {size:?}");
        }
        fixture.assert_presentable();
    }
}

#[test]
fn choosing_a_bot_opens_its_conversation_in_the_content_pane() {
    for (size, panes, names) in [
        (PHONE, "content", &["Tutor"][..]),
        (TABLET, "list,content", &[APPLICATION_NAME, "Tutor"][..]),
        (DESKTOP, "list,content", &[APPLICATION_NAME, "Tutor"][..]),
    ] {
        let mut fixture = Fixture::new();
        fixture.live_at(size);
        fixture.open_tutor(hello(), 10);
        assert_eq!(fixture.fact("content"), "conversation".into());
        fixture.assert_panes(panes, names);
        // Agents read which Bot is open from the content's pane.
        let snapshot = fixture.host().ui_snapshot();
        let content = snapshot.by_role(UiRole::Pane).last().unwrap();
        assert_eq!(
            content.domain_reference,
            Some(DomainReference::External {
                namespace: "bot".into(),
                id: "1".into(),
            })
        );
        assert!(fixture.shows("Kia ora! What shall we study?"));
        // Back is the stack's: beside the list there is nothing to go back
        // to, and the list's Refresh gives way to the conversation's.
        assert_eq!(fixture.shows("Back"), size == PHONE);
        assert!(!fixture.shows("Refresh"));
        fixture.assert_presentable();
    }
}

#[test]
fn back_on_a_phone_returns_to_the_list_and_to_the_row_that_opened_it() {
    let mut fixture = Fixture::new();
    fixture.live_at(PHONE);
    fixture.open_tutor(hello(), 10);
    // Focus follows the reader to Back, whose level covers the list.
    assert!(fixture.focused("Back"));
    fixture.click("Back");
    assert_eq!(fixture.fact("screen"), "signed_in".into());
    assert_eq!(fixture.fact("bot"), 0_i64.into());
    fixture.assert_panes("list", &[APPLICATION_NAME]);
    assert!(fixture.focused("Open Tutor"));
    assert!(!fixture.enabled(BokkieAction::Back));
}

#[test]
fn beside_the_list_back_empties_the_content_for_an_agent() {
    let mut fixture = Fixture::new();
    fixture.live_at(TABLET);
    fixture.open_tutor(hello(), 10);
    // Back still closes the conversation for an agent, which can invoke it
    // in every layout.
    fixture
        .harness
        .state_mut()
        .app_mut()
        .perform(ActionTarget::application(BokkieAction::Back), &mut ())
        .unwrap();
    fixture.harness.run();
    assert_eq!(fixture.fact("content"), "".into());
    fixture.assert_panes("list,empty", &[APPLICATION_NAME]);
}

#[test]
fn the_settings_show_in_the_detail_pane_in_each_layout() {
    for (size, panes, names, back) in [
        (PHONE, "detail", &["Tutor settings"][..], true),
        (
            TABLET,
            "list,detail",
            &[APPLICATION_NAME, "Tutor settings"][..],
            true,
        ),
        (
            DESKTOP,
            "list,content,detail",
            &[APPLICATION_NAME, "Tutor", "Tutor settings"][..],
            false,
        ),
    ] {
        let mut fixture = Fixture::new();
        fixture.live_at(size);
        fixture.open_tutor(hello(), 10);
        let _ = fixture.sent.take_viewing();
        fixture.click("Settings");
        assert_eq!(
            fixture.sent(),
            [
                ApiRequest::LoadNotes(TUTOR),
                ApiRequest::LoadToolCalls(TUTOR, None),
                ApiRequest::ListRoutines(TUTOR),
            ]
        );
        assert_eq!(fixture.fact("screen"), "bot_settings".into());
        assert_eq!(fixture.fact("detail"), "settings".into());
        fixture.assert_panes(panes, names);
        assert!(fixture.shows("Grants") && fixture.shows("Refresh"));
        assert_eq!(fixture.shows("Back"), back, "{size:?}");
        // The conversation is still on screen beside the settings on a
        // desktop, so it is still the one being viewed and read.
        let viewing = if size == DESKTOP { 1_i64 } else { 0 };
        assert_eq!(fixture.fact("viewing"), viewing.into(), "{size:?}");
        if size != DESKTOP {
            assert_eq!(fixture.sent.take_viewing(), [None]);
        }
        fixture.assert_presentable();
        if back {
            fixture.click("Back");
        } else {
            // The toggle in the conversation's header hides them again.
            fixture.click("Settings");
        }
        assert_eq!(fixture.fact("screen"), "conversation".into());
        assert_eq!(fixture.fact("detail"), "".into());
        assert_eq!(fixture.fact("bot"), 1_i64.into());
    }
}

#[test]
fn the_admin_and_household_screens_open_in_the_content_pane() {
    let mut fixture = Fixture::new();
    fixture.live_at(TABLET);
    fixture.open_tutor(hello(), 10);
    // The content pane holds one thing at a time: the accounts replace the
    // conversation.
    fixture.click("Accounts");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::ListAccounts, ApiRequest::ListInvitations]
    );
    assert_eq!(fixture.fact("screen"), "admin".into());
    assert_eq!(fixture.fact("content"), "admin".into());
    assert_eq!(fixture.fact("bot"), 0_i64.into());
    fixture.assert_panes("list,content", &[APPLICATION_NAME, "Accounts"]);
    // Beside the list, the section keeps its own Back.
    assert!(fixture.shows("Back"));
    assert!(!fixture.enabled(BokkieAction::Admin));
    // The household's sign-in replaces the accounts.
    fixture.click("ChatGPT sign-in");
    assert_eq!(fixture.sent(), [ApiRequest::CodexAccount]);
    assert_eq!(fixture.fact("screen"), "household".into());
    assert_eq!(fixture.fact("content"), "household".into());
    assert_eq!(fixture.fact("accounts"), 0_u64.into());
    fixture.assert_panes(
        "list,content",
        &[APPLICATION_NAME, "ChatGPT for the household"],
    );
    assert!(!fixture.enabled(BokkieAction::Household));
    // And a Bot chosen from the list replaces the sign-in.
    fixture.click("Open Tutor");
    assert_eq!(fixture.sent(), [ApiRequest::LoadMessages(TUTOR)]);
    assert_eq!(fixture.fact("content"), "conversation".into());
    assert_eq!(fixture.fact("chatgpt_login"), "".into());
}

#[test]
fn the_admin_and_household_screens_take_the_phone_with_back() {
    let mut fixture = Fixture::new();
    fixture.live_at(PHONE);
    fixture.click("Accounts");
    let _ = fixture.sent();
    fixture.assert_panes("content", &["Accounts"]);
    // One Back, the header's.
    assert_eq!(fixture.harness.query_all_by_label("Back").count(), 1);
    fixture.assert_presentable();
    fixture.click("Back");
    fixture.assert_panes("list", &[APPLICATION_NAME]);
    fixture.click("ChatGPT sign-in");
    let _ = fixture.sent();
    fixture.assert_panes("content", &["ChatGPT for the household"]);
    fixture.assert_presentable();
    fixture.click("Back");
    assert_eq!(fixture.fact("screen"), "signed_in".into());
    fixture.assert_panes("list", &[APPLICATION_NAME]);
}

#[test]
fn nothing_is_lost_on_a_resize() {
    let mut fixture = Fixture::new();
    fixture.live_at(PHONE);
    fixture.open_tutor(hello(), 10);
    fixture.write("Half a thought");
    fixture.click("Settings");
    let _ = fixture.sent();
    for (size, layout, panes) in [
        (DESKTOP, "columns", "list,content,detail"),
        (TABLET, "split", "list,detail"),
        (PHONE, "stack", "detail"),
        (DESKTOP, "columns", "list,content,detail"),
    ] {
        fixture.resize(size);
        assert_eq!(fixture.fact("layout"), layout.into());
        assert_eq!(fixture.fact("panes"), panes.into());
        assert_eq!(fixture.fact("screen"), "bot_settings".into());
        assert_eq!(fixture.fact("bot"), 1_i64.into());
        assert_eq!(fixture.fact("draft"), "Half a thought".into());
        assert_eq!(fixture.fact("message_count"), 2_u64.into());
        // Nothing is asked of the gateway again.
        assert!(fixture.sent().is_empty(), "{size:?}");
    }
    fixture.click("Settings");
    fixture.resize(PHONE);
    fixture.assert_panes("content", &["Tutor"]);
    assert_eq!(fixture.fact("draft"), "Half a thought".into());
}
