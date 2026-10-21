//! The Bot panel: its header, routines, recent runs, grants, notes and
//! activity at a phone's, a tablet's and a desktop's width in both themes,
//! and how its recent runs and activity keep current.

use bokkie_protocol::api::BotStatus;
use egui::accesskit::{Role as AccessRole, Toggled};
use egui_kittest::kittest::NodeT;

use super::super::navigation::{DESKTOP, PHONE, TABLET};
use super::*;

const WEEKLY: RoutineId = RoutineId(4);
const NOTES: &str = "Likes **fractions**.\n\n- Halves\n- Quarters";

/// A paused routine, beside the Morning digest.
fn weekly() -> Routine {
    Routine {
        paused: true,
        next_run_at: None,
        ..routine(WEEKLY, "Weekly review", 2_500)
    }
}

fn weekly_run(id: i64, state: RunState) -> RoutineRun {
    RoutineRun {
        routine_id: WEEKLY,
        trigger: RunTrigger::Schedule,
        ..run(id, state, &[])
    }
}

fn page(runs: Vec<RoutineRun>) -> RoutineRunPage {
    RoutineRunPage { runs, older: None }
}

/// The run's marker and reply in the conversation.
fn shown_run() -> Vec<Message> {
    vec![
        marker(30, 21),
        message(31, Author::Bot, "Routine done.", MessageState::Complete),
    ]
}

/// The phone-width page the panel's state tests use, tall enough that the
/// whole panel is in view.
const TALL: egui::Vec2 = egui::vec2(390.0, 3_000.0);

impl Fixture {
    /// Ada's Tutor, holding `messages`, with notes and web.search granted,
    /// and its panel open and loaded on a page of `size` in `theme`: two
    /// routines, one paused; three recent runs, one failed; Markdown notes;
    /// and two tool calls, one in a run.
    pub(super) fn panel_at(theme: egui::Theme, size: egui::Vec2, messages: Vec<Message>) -> Self {
        let mut fixture = Fixture::new();
        fixture.harness.input_mut().system_theme = Some(theme);
        // The harness frames the page in an 8-point margin.
        fixture.resize(size + egui::vec2(16.0, 16.0));
        fixture.sent.set_now(Timestamp(NOW));
        fixture.signed_in_and_live_as(session(user()), 10);
        fixture.open_tutor(messages, 10);
        fixture.tutor_updated(11, vec![Grant::Notes, Grant::WebSearch]);
        fixture.click("Settings");
        let _ = fixture.sent();
        fixture.reply(ApiReply::Notes(TUTOR, Ok(notes(NOTES))));
        fixture.reply(ApiReply::ToolCalls(
            TUTOR,
            None,
            Ok(ToolCallPage {
                calls: vec![
                    ToolCall {
                        routine_run_id: Some(RoutineRunId(21)),
                        ..call(9, "notes.read", ToolCallOutcome::Ok)
                    },
                    call(8, "web.fetch", ToolCallOutcome::Refused),
                ],
                older: None,
            }),
        ));
        fixture.reply(ApiReply::Routines(
            TUTOR,
            Ok(RoutineList {
                routines: vec![digest(), weekly()],
                seq: Seq(11),
            }),
        ));
        assert_eq!(
            fixture.sent(),
            [
                ApiRequest::LoadRecentRuns(TUTOR, DIGEST),
                ApiRequest::LoadRecentRuns(TUTOR, WEEKLY)
            ]
        );
        assert_eq!(fixture.fact("recent_runs_state"), "loading".into());
        fixture.reply(ApiReply::RecentRuns(
            TUTOR,
            DIGEST,
            Ok(page(vec![
                run(21, RunState::Succeeded, &[30, 31]),
                RoutineRun {
                    trigger: RunTrigger::Schedule,
                    ..run(20, RunState::Failed, &[12])
                },
            ])),
        ));
        fixture.reply(ApiReply::RecentRuns(
            TUTOR,
            WEEKLY,
            Ok(page(vec![weekly_run(15, RunState::Missed)])),
        ));
        assert_eq!(fixture.fact("recent_runs_state"), "loaded".into());
        fixture
    }

    /// The switch named `label`: whether it is one, and on.
    fn switch_on(&self, label: &str) -> bool {
        let node = self.harness.get_by_label(label);
        let node = node.accesskit_node();
        assert_eq!(node.role(), AccessRole::Switch, "{label}");
        node.toggled() == Some(Toggled::True)
    }
}

#[test]
fn the_panel_presents_each_section_at_each_width_in_both_themes() {
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        let name = if theme == egui::Theme::Light {
            "light"
        } else {
            "dark"
        };
        for (size, panes) in [
            (PHONE, "detail"),
            (TABLET, "list,detail"),
            (DESKTOP, "list,content,detail"),
        ] {
            let at = |section: &str| format!("{section}, {name}, {size:?}");
            let mut fixture = Fixture::panel_at(theme, size, hello());
            assert_eq!(fixture.fact("panes"), panes.into(), "{}", at("panes"));
            assert_eq!(fixture.fact("detail"), "settings".into());
            // The header, at the top: the Bot's avatar, name and label. On
            // a phone the panel is all there is, so they are its own.
            assert!(fixture.shows("Maths help"), "{}", at("header"));
            if size == PHONE {
                assert_eq!(fixture.harness.query_all_by_label("Tutor").count(), 1);
                assert_eq!(fixture.harness.query_all_by_label("Maths help").count(), 1);
            }
            let sections: [(&str, &[&str]); 5] = [
                (
                    "Routines",
                    &[
                        "Every day at 07:00 · Next run today at 8:30 PM",
                        "Run now Morning digest",
                        "Pause Morning digest",
                        "Edit routine Morning digest",
                        "History Morning digest",
                        "Every day at 07:00 · Paused",
                        "Resume Weekly review",
                    ],
                ),
                (
                    "Recent runs",
                    &[
                        "Succeeded · run now · 6:30 PM",
                        "Show run 21 of Morning digest",
                        "Failed · scheduled · 6:30 PM",
                        "The Bot could not reply",
                        "Show run 20 of Morning digest",
                        "Missed · scheduled · 6:30 PM",
                    ],
                ),
                (
                    "Grants",
                    &[
                        "Grant notes",
                        "Grant web.fetch",
                        crate::settings::view::EXFILTRATION_NOTICE,
                    ],
                ),
                (
                    "Notes",
                    &[
                        "Likes fractions.",
                        "Halves",
                        "Last changed today at 3:30 PM",
                        "Clear notes",
                    ],
                ),
                (
                    "Activity",
                    &[
                        "notes.read · done · 120 ms · 5:30 PM · routine run",
                        "web.fetch · refused · 120 ms · 5:30 PM",
                    ],
                ),
            ];
            // As the page opens, then on a page as wide but tall enough to
            // hold the whole panel, since Stobie presents only what is in
            // view and a scroll area cuts a control at either edge.
            assert_eq!(fixture.fact("theme"), name.into(), "{}", at("top"));
            fixture.assert_presentable();
            fixture.resize(egui::vec2(size.x, 3_000.0) + egui::vec2(16.0, 16.0));
            assert_eq!(fixture.fact("panes"), panes.into(), "{}", at("tall"));
            for (heading, texts) in sections {
                assert!(fixture.shows(heading), "{}", at(heading));
                for text in texts {
                    assert!(fixture.shows(text), "{text}: {}", at(heading));
                }
            }
            assert_eq!(fixture.fact("theme"), name.into(), "{}", at("tall"));
            fixture.assert_presentable();
            // Each grant is a switch that shows whether it is held, and a
            // run in a routine run is said so.
            assert!(fixture.switch_on("Grant notes"), "{}", at("grants"));
            assert!(fixture.switch_on("Grant web.search"), "{}", at("grants"));
            assert!(!fixture.switch_on("Grant web.fetch"), "{}", at("grants"));
            assert!(!fixture.shows("Resume Morning digest"));
            assert!(!fixture.shows("Pause Weekly review"));
            // The notes read as Markdown.
            assert!(!fixture.shows("Likes **fractions**."));
        }
    }
}

#[test]
fn the_recent_runs_gather_each_routines_newest_and_follow_their_events() {
    let mut fixture = Fixture::panel_at(egui::Theme::Dark, TALL, hello());
    assert_eq!(fixture.fact("recent_runs"), 3_u64.into());
    assert_eq!(fixture.fact("recent_run"), 21.into());
    assert_eq!(fixture.fact("recent_run_state"), "succeeded".into());
    // Run now: the run joins the recent runs as it goes, never back.
    fixture.click("Run now Morning digest");
    assert_eq!(
        fixture.sent(),
        [
            ApiRequest::LoadRuns(TUTOR, DIGEST, None),
            ApiRequest::RunRoutine(TUTOR, DIGEST)
        ]
    );
    fixture.event(
        12,
        Event::RoutineRunUpdated {
            run: run(22, RunState::Running, &[40]),
        },
    );
    assert_eq!(fixture.fact("recent_runs"), 4_u64.into());
    assert_eq!(fixture.fact("recent_run"), 22.into());
    assert_eq!(fixture.fact("recent_run_state"), "running".into());
    fixture.reply(ApiReply::RoutineStarted(
        TUTOR,
        DIGEST,
        Ok(run(22, RunState::Waiting, &[])),
    ));
    assert_eq!(fixture.fact("recent_run_state"), "running".into());
    fixture.event(
        13,
        Event::RoutineRunUpdated {
            run: run(22, RunState::Succeeded, &[40, 41]),
        },
    );
    assert_eq!(fixture.fact("recent_run_state"), "succeeded".into());
    // A page that comes when none was asked for changes nothing.
    fixture.reply(ApiReply::RecentRuns(
        TUTOR,
        DIGEST,
        Ok(page(vec![run(23, RunState::Succeeded, &[50])])),
    ));
    assert_eq!(fixture.fact("recent_run"), 22.into());
    // The newest eight show, across the routines.
    for (seq, id) in (14..20).zip(24..30) {
        fixture.event(
            seq,
            Event::RoutineRunUpdated {
                run: weekly_run(id, RunState::Skipped),
            },
        );
    }
    assert_eq!(fixture.fact("recent_runs"), 8_u64.into());
    assert_eq!(fixture.fact("recent_run"), 29.into());
    assert!(
        !fixture.shows("Missed · scheduled · 6:30 PM"),
        "past the eighth"
    );
    // A routine deleted takes its runs with it.
    fixture.event(
        20,
        Event::RoutineDeleted {
            bot_id: TUTOR,
            routine_id: WEEKLY,
        },
    );
    assert_eq!(fixture.fact("recent_runs"), 3_u64.into());
    assert_eq!(fixture.fact("recent_run"), 22.into());
    // Refresh gathers them again; a page that fails says why.
    let _ = fixture.sent();
    fixture.click("Refresh");
    let sent = fixture.sent();
    assert!(sent.contains(&ApiRequest::ListRoutines(TUTOR)), "{sent:?}");
    fixture.reply(ApiReply::Routines(
        TUTOR,
        Ok(RoutineList {
            routines: vec![digest()],
            seq: Seq(20),
        }),
    ));
    assert_eq!(fixture.sent(), [ApiRequest::LoadRecentRuns(TUTOR, DIGEST)]);
    assert!(fixture.shows("Loading runs…"));
    fixture.reply(ApiReply::RecentRuns(
        TUTOR,
        DIGEST,
        Err(CommandError::unreachable()),
    ));
    assert_eq!(fixture.fact("recent_runs_state"), "failed".into());
    assert!(fixture.shows(&CommandError::unreachable().message));
    // What was gathered stays.
    assert_eq!(fixture.fact("recent_runs"), 3_u64.into());
}

#[test]
fn a_routine_with_no_runs_says_so() {
    let mut fixture = Fixture::new();
    fixture.settings_loaded(vec![digest()]);
    assert_eq!(fixture.fact("recent_runs"), 0_u64.into());
    assert!(fixture.shows("Recent runs"));
    assert!(fixture.shows("No runs yet"));
}

#[test]
fn show_in_the_recent_runs_keeps_the_panel_only_beside_the_conversation() {
    for (size, kept) in [(DESKTOP, true), (TABLET, false), (PHONE, false)] {
        let mut fixture = Fixture::panel_at(egui::Theme::Light, size, shown_run());
        fixture.reveal("Show run 21 of Morning digest");
        fixture.click("Show run 21 of Morning digest");
        assert_eq!(fixture.fact("showing"), 30.into(), "{size:?}");
        assert_eq!(fixture.fact("showing_error"), "".into(), "{size:?}");
        let detail = if kept { "settings" } else { "" };
        assert_eq!(fixture.fact("detail"), detail.into(), "{size:?}");
        assert_eq!(fixture.fact("content"), "conversation".into(), "{size:?}");
        assert!(fixture.shows("Routine done."), "{size:?}");
    }
}

#[test]
fn a_run_in_its_history_and_the_recent_runs_has_one_show_in_each() {
    let mut fixture = Fixture::panel_at(egui::Theme::Light, TALL, shown_run());
    let show = |fixture: &Fixture, action| fixture.availability_with(action, 21);
    // Only the recent runs list run 21 until its routine's history opens.
    assert_eq!(show(&fixture, BokkieAction::ShowRun), Availability::Hidden);
    assert_eq!(
        show(&fixture, BokkieAction::ShowRecentRun),
        Availability::Enabled
    );
    fixture.click("History Morning digest");
    let _ = fixture.sent();
    fixture.reply(ApiReply::Runs(
        TUTOR,
        DIGEST,
        None,
        Ok(page(vec![run(21, RunState::Succeeded, &[30, 31])])),
    ));
    // Each list's Show is its own action, so one ID names one control in
    // each and an agent's Show by ID is never ambiguous.
    assert_eq!(show(&fixture, BokkieAction::ShowRun), Availability::Enabled);
    assert_eq!(
        show(&fixture, BokkieAction::ShowRecentRun),
        Availability::Enabled
    );
    assert!(fixture.shows("Show run 21"));
    assert!(fixture.shows("Show run 21 of Morning digest"));
    // A run with no messages in the recent runs offers no Show.
    assert_eq!(
        fixture.availability_with(BokkieAction::ShowRecentRun, 15),
        Availability::Hidden
    );
    fixture.act_on(BokkieAction::ShowRecentRun, 21);
    assert_eq!(fixture.fact("showing"), 30.into());
    assert_eq!(
        fixture.fact("detail"),
        "".into(),
        "a phone closes the panel"
    );
}

#[test]
fn a_turn_that_ends_loads_the_activity_and_the_notes_again() {
    let mut fixture = Fixture::panel_at(egui::Theme::Dark, TALL, hello());
    let status = |status| Event::BotStatusChanged {
        bot_id: TUTOR,
        status,
    };
    fixture.event(12, status(BotStatus::Working));
    assert!(fixture.sent().is_empty());
    fixture.event(13, status(BotStatus::Idle));
    assert_eq!(
        fixture.sent(),
        [
            ApiRequest::LoadToolCalls(TUTOR, None),
            ApiRequest::LoadNotes(TUTOR)
        ]
    );
    assert_eq!(fixture.fact("tool_calls_state"), "loading".into());
    // While they load, another turn's end asks for nothing more.
    fixture.event(14, status(BotStatus::Working));
    fixture.event(15, status(BotStatus::Idle));
    assert!(fixture.sent().is_empty());
    fixture.reply(ApiReply::ToolCalls(
        TUTOR,
        None,
        Ok(ToolCallPage {
            calls: vec![
                call(10, "notes.append", ToolCallOutcome::Ok),
                call(9, "notes.read", ToolCallOutcome::Ok),
                call(8, "web.fetch", ToolCallOutcome::Refused),
            ],
            older: None,
        }),
    ));
    fixture.reply(ApiReply::Notes(TUTOR, Ok(notes("Likes halves."))));
    assert_eq!(fixture.fact("tool_calls"), 3_u64.into());
    assert!(fixture.shows("notes.append · done · 120 ms · 5:30 PM"));
    assert!(fixture.shows("Likes halves."));
    // Notes about to be cleared are not loaded over.
    fixture.click("Clear notes");
    fixture.event(16, status(BotStatus::Working));
    fixture.event(17, status(BotStatus::Idle));
    assert_eq!(fixture.sent(), [ApiRequest::LoadToolCalls(TUTOR, None)]);
    // With the panel closed, a turn's end loads nothing.
    fixture.click("Back");
    let _ = fixture.sent();
    fixture.event(18, status(BotStatus::Working));
    fixture.event(19, status(BotStatus::Idle));
    assert!(fixture.sent().is_empty());
}
