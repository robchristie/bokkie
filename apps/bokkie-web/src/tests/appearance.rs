//! The client in both themes, as the system's appearance says, at a phone's,
//! a tablet's and a desktop's width, at the widths where the layout changes,
//! and with large text: each state keeps its controls at their size, and
//! long names, previews and messages wrap or shorten rather than push the
//! page sideways.

use bokkie_protocol::api::{MessagePage, MessageState};
use stobie_ui_egui::AppearancePreference;

use super::conversation::{TUTOR, message};
use super::navigation::{DESKTOP, PHONE, TABLET};
use super::*;

/// As long as the gateway allows a name and a label: 40 characters and 60.
const LONG_NAME: &str = "The household's meal planner and lists";
const LONG_LABEL: &str = "Plans the week's dinners around what is in season and cheap";
const LONG_PREVIEW: &str =
    "Here is this week's list, sorted by aisle so that the trip is quick and nothing is missed";
const LONG_WORD: &str =
    "https://example.org/a/very/long/address/without/any/spaces/that/keeps/on/going/and/going";

/// The harness's size for a page of `size`: egui's test harness frames
/// what it presents in an 8-point margin, which a browser does not.
fn page(size: egui::Vec2) -> egui::Vec2 {
    size + egui::vec2(16.0, 16.0)
}

/// The Tutor, unread with a long label and preview, and a pinned Bot whose
/// name, label and preview are all long, the preview one unbroken word.
fn long_list(seq: u64) -> BotList {
    let mut tutor = bot(1, "Tutor", LONG_LABEL, BotStatus::Idle, Some(LONG_PREVIEW));
    tutor.unread = true;
    let mut planner = bot(
        2,
        LONG_NAME,
        LONG_LABEL,
        BotStatus::Working,
        Some(LONG_WORD),
    );
    planner.pinned = true;
    BotList {
        unread_bots: 1,
        bots: vec![tutor, planner],
        seq: Seq(seq),
    }
}

impl Fixture {
    /// A page of `size` whose system appearance is `theme`, with text at
    /// `scale` times its size.
    fn shown_as(theme: egui::Theme, size: egui::Vec2, scale: f32) -> Self {
        let mut fixture = Self::new();
        fixture.harness.input_mut().system_theme = Some(theme);
        fixture.harness.state_mut().app_mut().preferences.font_scale = scale;
        fixture.resize(page(size));
        fixture
    }

    fn theme(&self) -> &'static str {
        if self.harness.ctx.global_style().visuals.dark_mode {
            "dark"
        } else {
            "light"
        }
    }

    /// Presentable, and drawn in the theme the `theme` fact says.
    fn assert_drawn(&self, theme: &str, at: &str) {
        assert_eq!(self.fact("theme"), theme.into(), "{at}");
        assert_eq!(self.theme(), theme, "{at}");
        self.assert_presentable();
    }
}

#[test]
fn the_page_follows_the_systems_appearance() {
    let mut fixture = Fixture::new();
    assert_eq!(
        fixture.app().preferences.appearance,
        AppearancePreference::System
    );
    // A browser that says nothing is drawn dark, as before.
    assert_eq!(fixture.fact("theme"), "dark".into());
    for (theme, name) in [
        (egui::Theme::Light, "light"),
        (egui::Theme::Dark, "dark"),
        (egui::Theme::Light, "light"),
    ] {
        fixture.harness.input_mut().system_theme = Some(theme);
        fixture.harness.run();
        assert_eq!(fixture.fact("theme"), name.into());
        assert_eq!(fixture.theme(), name);
    }
}

#[test]
fn every_state_presents_in_both_themes_at_each_width_and_with_large_text() {
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        // A phone upright and a desktop as the browser smoke has them, and
        // the tablet the navigation tests use.
        for size in [PHONE, TABLET, egui::vec2(DESKTOP.x, 800.0)] {
            for scale in [1.0, 1.5] {
                let name = if theme == egui::Theme::Light {
                    "light"
                } else {
                    "dark"
                };
                let at = |state: &str| format!("{state}, {name}, {size:?}, text {scale}");
                let mut fixture = Fixture::shown_as(theme, size, scale);
                // Signed out.
                fixture.reply(ApiReply::Session(Err(unauthenticated())));
                fixture.assert_drawn(name, &at("signed out"));
                // The list loading, failed, empty and then long.
                let _ = fixture.sent();
                fixture.sign_in();
                fixture.assert_drawn(name, &at("loading"));
                fixture.reply(ApiReply::Bots(Err(CommandError::unreachable())));
                assert!(fixture.shows(&CommandError::unreachable().message));
                fixture.assert_drawn(name, &at("failed"));
                fixture.click("Refresh Bots");
                let _ = fixture.sent();
                fixture.reply(ApiReply::Bots(Ok(BotList {
                    unread_bots: 0,
                    bots: Vec::new(),
                    seq: Seq(1),
                })));
                fixture.assert_drawn(name, &at("empty"));
                fixture.click("Refresh Bots");
                let _ = fixture.sent();
                fixture.reply(ApiReply::Bots(Ok(long_list(2))));
                assert!(fixture.shows(LONG_NAME), "{}", at("long"));
                fixture.assert_drawn(name, &at("long"));
                // A conversation loading, then long messages and a long
                // draft.
                fixture.open("Tutor");
                fixture.assert_drawn(name, &at("conversation loading"));
                fixture.reply(ApiReply::Messages(
                    TUTOR,
                    Ok(MessagePage {
                        messages: vec![
                            message(1, Author::User, LONG_PREVIEW, MessageState::Complete),
                            message(2, Author::Bot, LONG_WORD, MessageState::Complete),
                            message(
                                3,
                                Author::Bot,
                                &LONG_LABEL.repeat(8),
                                MessageState::Complete,
                            ),
                        ],
                        older: None,
                        seq: Seq(2),
                    }),
                ));
                fixture.send(BokkieIntent::EditDraft(LONG_PREVIEW.repeat(3)));
                fixture.assert_drawn(name, &at("conversation"));
                // A conversation that failed to load.
                if size != PHONE {
                    fixture.open(LONG_NAME);
                    let _ = fixture.sent();
                    fixture.reply(ApiReply::Messages(
                        BotId(2),
                        Err(CommandError::unreachable()),
                    ));
                    fixture.assert_drawn(name, &at("conversation failed"));
                }
            }
        }
    }
}

#[test]
fn the_layout_changes_at_exactly_720_and_1040_points() {
    for (width, layout) in [
        (719.0, "stack"),
        (720.0, "split"),
        (1_039.0, "split"),
        (1_040.0, "columns"),
    ] {
        let mut fixture = Fixture::new();
        fixture.resize(page(egui::vec2(width, 900.0)));
        fixture.signed_in_and_live(10);
        assert_eq!(fixture.fact("layout"), layout.into(), "{width}");
        fixture.assert_presentable();
        fixture.open("Tutor");
        let _ = fixture.sent();
        fixture.reply(ApiReply::Messages(
            TUTOR,
            Ok(MessagePage {
                messages: super::conversation::hello(),
                older: None,
                seq: Seq(10),
            }),
        ));
        assert_eq!(fixture.fact("layout"), layout.into(), "{width}");
        fixture.assert_presentable();
    }
}
