//! A Bot's settings: its grants within a member's limits, its notes, its
//! tool calls and its routines with their run history, kept current by the
//! routine events; and the user's time zone.

use bokkie_protocol::api::{
    BotNotes, CreateRoutineRequest, Grant, Message, MessageKind, MessagePage, MessageRoutine,
    MessageState, Routine, RoutineList, RoutinePush, RoutineRun, RoutineRunPage, RunState,
    RunTrigger, Schedule, SetTimeZoneRequest, TimeOfDay, ToolCall, ToolCallOutcome, ToolCallPage,
    UpdateBotRequest, UpdateRoutineRequest, Weekday,
};
use bokkie_protocol::events::Event;
use bokkie_protocol::{MessageId, RoutineId, RoutineRunId};
use stobie_ui_egui::{ActionArgument, Availability};

use super::conversation::{TUTOR, hello, message};
use super::*;

const NOW: i64 = 1_800_000_000_000;
const HOUR: i64 = 3_600_000;
const DIGEST: RoutineId = RoutineId(3);

fn notes(text: &str) -> BotNotes {
    BotNotes {
        text: text.into(),
        updated_at: Some(Timestamp(NOW - 3 * HOUR)),
    }
}

fn call(id: i64, tool: &str, outcome: ToolCallOutcome) -> ToolCall {
    ToolCall {
        id,
        tool: tool.into(),
        summary: format!("Summary {id}"),
        outcome,
        duration_ms: 120,
        at: Timestamp(NOW - HOUR),
        routine_run_id: None,
    }
}

fn routine(id: RoutineId, name: &str, updated_at: i64) -> Routine {
    Routine {
        id,
        bot_id: TUTOR,
        name: name.into(),
        prompt: "Summarise the news.".into(),
        schedule: Schedule::Daily {
            at: TimeOfDay::new(7, 0).unwrap(),
        },
        push: RoutinePush::EveryRun,
        paused: false,
        failures: 0,
        next_run_at: Some(Timestamp(NOW + 2 * HOUR)),
        created_at: Timestamp(1_000),
        updated_at: Timestamp(updated_at),
    }
}

fn digest() -> Routine {
    routine(DIGEST, "Morning digest", 2_000)
}

fn run(id: i64, state: RunState, messages: &[i64]) -> RoutineRun {
    RoutineRun {
        id: RoutineRunId(id),
        routine_id: DIGEST,
        bot_id: TUTOR,
        trigger: RunTrigger::Manual,
        scheduled_for: Timestamp(NOW),
        started_at: (state != RunState::Waiting).then_some(Timestamp(NOW)),
        finished_at: state.is_finished().then_some(Timestamp(NOW + 1_000)),
        state,
        reason: (state == RunState::Failed).then(|| "The Bot could not reply".into()),
        messages: messages.iter().map(|id| MessageId(*id)).collect(),
        tokens: None,
    }
}

fn marker(id: i64, run: i64) -> Message {
    Message {
        kind: MessageKind::Marker,
        routine: Some(MessageRoutine {
            routine_id: DIGEST,
            run_id: RoutineRunId(run),
            prompt: "Summarise the news.".into(),
        }),
        ..message(
            id,
            Author::Bot,
            "Morning digest · 6:30 PM · test run",
            MessageState::Complete,
        )
    }
}

fn number(id: i64) -> ActionArgument {
    ActionArgument::Number(id as f64)
}

impl Fixture {
    /// Signed in as `user` at `NOW`, live, with the Tutor's conversation
    /// open and its settings loading.
    fn settings_as(&mut self, user: User) {
        self.tall();
        self.sent.set_now(Timestamp(NOW));
        self.signed_in_and_live_as(session(user), 10);
        self.open_tutor(hello(), 10);
        self.click("Settings");
        assert_eq!(self.fact("screen"), "bot_settings".into());
        assert_eq!(
            self.sent(),
            [
                ApiRequest::LoadNotes(TUTOR),
                ApiRequest::LoadToolCalls(TUTOR, None),
                ApiRequest::ListRoutines(TUTOR),
            ]
        );
    }

    /// Ada's Tutor settings, loaded: notes, two tool calls with more to
    /// load, and `routines`.
    fn settings_loaded(&mut self, routines: Vec<Routine>) {
        self.settings_as(user());
        self.reply(ApiReply::Notes(TUTOR, Ok(notes("Likes fractions."))));
        self.reply(ApiReply::ToolCalls(
            TUTOR,
            None,
            Ok(ToolCallPage {
                calls: vec![
                    call(9, "notes.read", ToolCallOutcome::Ok),
                    call(8, "web.fetch", ToolCallOutcome::Refused),
                ],
                older: Some(8),
            }),
        ));
        self.reply(ApiReply::Routines(
            TUTOR,
            Ok(RoutineList {
                routines,
                seq: Seq(10),
            }),
        ));
        assert_eq!(self.fact("routines"), "loaded".into());
    }

    /// A phone's width, tall enough that the whole of the settings is in
    /// view: Stobie presents only what is.
    fn tall(&mut self) {
        self.harness.set_size(egui::vec2(390.0, 3_000.0));
        self.harness.run();
    }

    fn tutor_updated(&mut self, seq: u64, grants: Vec<Grant>) {
        let mut tutor = self.app().store().bot(TUTOR).unwrap().clone();
        tutor.grants = grants;
        self.event(seq, Event::BotUpdated { bot: tutor });
    }

    fn availability_of(&self, action: BokkieAction) -> Availability {
        self.app().availability(ActionTarget::application(action))
    }

    fn availability_with(&self, action: BokkieAction, id: i64) -> Availability {
        self.app()
            .availability_with(ActionTarget::application(action), &number(id))
    }

    fn act_on(&mut self, action: BokkieAction, id: i64) {
        let target = ActionTarget::application(action);
        self.harness
            .state_mut()
            .app_mut()
            .perform_with(target, number(id), &mut ())
            .unwrap();
        self.harness.run();
    }
}

fn disabled(reason: &str) -> Availability {
    Availability::Disabled {
        reason: reason.to_owned().into(),
    }
}

#[test]
fn settings_show_the_grants_notes_tool_calls_and_routines() {
    let mut fixture = Fixture::new();
    fixture.settings_loaded(vec![digest()]);
    assert!(fixture.shows("Tutor settings"));
    assert_eq!(fixture.fact("grants"), "".into());
    assert!(fixture.shows(crate::settings::view::EXFILTRATION_NOTICE));
    assert_eq!(fixture.fact("notes"), "loaded".into());
    assert_eq!(fixture.fact("notes_chars"), 16_u64.into());
    assert!(fixture.shows("Likes fractions."));
    // Three hours before now, on Ada's clock in Adelaide.
    assert!(fixture.shows("Last changed today at 3:30 PM"));
    assert_eq!(fixture.fact("tool_calls"), 2_u64.into());
    assert_eq!(fixture.fact("tool_calls_state"), "more".into());
    assert!(fixture.shows("web.fetch · refused · 120 ms · 5:30 PM"));
    assert_eq!(fixture.fact("routine_count"), 1_u64.into());
    assert!(fixture.shows("Morning digest"));
    assert!(fixture.shows("Every day at 07:00 · Next run today at 8:30 PM"));
    // Back is the conversation, which let the routines go.
    fixture.click("Back");
    assert_eq!(fixture.fact("screen"), "conversation".into());
    assert!(fixture.app().store().routines(TUTOR).is_none());
    assert_eq!(fixture.fact("routines"), "".into());
}

#[test]
fn a_member_may_grant_only_notes_seen_and_notify() {
    let mut fixture = Fixture::new();
    fixture.settings_as(member());
    assert!(fixture.enabled(BokkieAction::GrantNotes));
    assert!(fixture.enabled(BokkieAction::GrantSeen));
    assert!(fixture.enabled(BokkieAction::GrantNotify));
    assert_eq!(
        fixture.availability_of(BokkieAction::GrantWebFetch),
        disabled("Only an admin can grant web.fetch")
    );
    assert!(fixture.shows("Reads web pages. Only an admin can grant web.fetch."));
    // Web search is never offered while it cannot be granted.
    assert_eq!(
        fixture.availability_of(BokkieAction::GrantWebSearch),
        disabled("Web search is not available yet")
    );
    assert!(!fixture.shows("Grant web.search"));
    assert!(fixture.shows(crate::settings::view::EXFILTRATION_NOTICE));
    fixture.click("Grant notes");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::UpdateBot(
            TUTOR,
            UpdateBotRequest {
                grants: Some(vec![Grant::Notes]),
                ..Default::default()
            }
        )]
    );
    assert_eq!(
        fixture.availability_of(BokkieAction::GrantSeen),
        disabled("Saving the Bot")
    );
    fixture.reply(ApiReply::BotUpdated(
        TUTOR,
        Ok(fixture.app().store().bot(TUTOR).unwrap().clone()),
    ));
    fixture.tutor_updated(11, vec![Grant::Notes]);
    assert_eq!(fixture.fact("grants"), "notes".into());
    // An admin granted what a member cannot: the member can take it away.
    fixture.tutor_updated(12, vec![Grant::Notes, Grant::WebSearch, Grant::WebFetch]);
    assert!(fixture.enabled(BokkieAction::GrantWebFetch));
    assert!(fixture.enabled(BokkieAction::GrantWebSearch));
    assert!(fixture.shows("Grant web.search"));
    fixture.click("Grant web.search");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::UpdateBot(
            TUTOR,
            UpdateBotRequest {
                grants: Some(vec![Grant::Notes, Grant::WebFetch]),
                ..Default::default()
            }
        )]
    );
    // The gateway's refusal is said.
    fixture.reply(ApiReply::BotUpdated(
        TUTOR,
        Err(error(
            403,
            ErrorCode::Forbidden,
            "Only an admin can grant web.fetch",
        )),
    ));
    assert_eq!(
        fixture.fact("profile_error"),
        "Only an admin can grant web.fetch".into()
    );
}

#[test]
fn an_admin_may_grant_web_fetch_but_not_web_search() {
    let mut fixture = Fixture::new();
    fixture.settings_loaded(Vec::new());
    assert!(fixture.enabled(BokkieAction::GrantWebFetch));
    assert_eq!(
        fixture.availability_of(BokkieAction::GrantWebSearch),
        disabled("Web search is not available yet")
    );
    fixture.click("Grant web.fetch");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::UpdateBot(
            TUTOR,
            UpdateBotRequest {
                grants: Some(vec![Grant::WebFetch]),
                ..Default::default()
            }
        )]
    );
}

#[test]
fn clearing_the_notes_asks_first() {
    let mut fixture = Fixture::new();
    fixture.settings_loaded(Vec::new());
    assert!(!fixture.enabled(BokkieAction::ConfirmClearNotes));
    fixture.click("Clear notes");
    assert_eq!(fixture.fact("notes_clear"), "confirming".into());
    assert!(fixture.sent().is_empty());
    fixture.click("Keep notes");
    assert_eq!(fixture.fact("notes_clear"), "".into());
    fixture.click("Clear notes");
    fixture.click("Clear for good");
    assert_eq!(fixture.sent(), [ApiRequest::ClearNotes(TUTOR)]);
    assert_eq!(fixture.fact("notes_clear"), "clearing".into());
    // A failure says why, and the notes stay.
    fixture.reply(ApiReply::NotesCleared(
        TUTOR,
        Err(CommandError::unreachable()),
    ));
    assert_eq!(fixture.fact("notes_chars"), 16_u64.into());
    assert!(fixture.shows("Likes fractions."));
    fixture.click("Clear notes");
    fixture.click("Clear for good");
    let _ = fixture.sent();
    fixture.reply(ApiReply::NotesCleared(TUTOR, Ok(())));
    assert_eq!(fixture.fact("notes_chars"), 0_u64.into());
    assert!(fixture.shows("The Bot has no notes"));
    assert_eq!(
        fixture.availability_of(BokkieAction::ClearNotes),
        disabled("There are no notes")
    );
}

#[test]
fn earlier_tool_calls_load_a_page_at_a_time() {
    let mut fixture = Fixture::new();
    fixture.settings_loaded(Vec::new());
    fixture.reveal("Earlier tool calls");
    fixture.click("Earlier tool calls");
    assert_eq!(fixture.sent(), [ApiRequest::LoadToolCalls(TUTOR, Some(8))]);
    assert_eq!(fixture.fact("tool_calls_state"), "loading".into());
    fixture.reply(ApiReply::ToolCalls(
        TUTOR,
        Some(8),
        Ok(ToolCallPage {
            calls: vec![call(4, "notify", ToolCallOutcome::TimedOut)],
            older: None,
        }),
    ));
    assert_eq!(fixture.fact("tool_calls"), 3_u64.into());
    assert_eq!(fixture.fact("tool_calls_state"), "loaded".into());
    assert!(!fixture.enabled(BokkieAction::OlderToolCalls));
    // Refresh loads the first page again, in place of the earlier ones, and
    // the next page follows it.
    fixture.click("Refresh");
    assert!(
        fixture
            .sent()
            .contains(&ApiRequest::LoadToolCalls(TUTOR, None))
    );
    fixture.reply(ApiReply::ToolCalls(
        TUTOR,
        None,
        Ok(ToolCallPage {
            calls: vec![call(10, "seen", ToolCallOutcome::Failed)],
            older: Some(10),
        }),
    ));
    assert_eq!(fixture.fact("tool_calls"), 1_u64.into());
    assert_eq!(fixture.fact("tool_calls_state"), "more".into());
    fixture.reveal("Earlier tool calls");
    fixture.click("Earlier tool calls");
    assert!(
        fixture
            .sent()
            .contains(&ApiRequest::LoadToolCalls(TUTOR, Some(10)))
    );
}

#[test]
fn a_schedule_every_few_hours_names_the_days_first_and_last_runs() {
    use crate::settings::schedule_text;
    let at = |text: &str| text.parse().unwrap();
    let hours = |every, time| Schedule::Hours {
        every,
        at: at(time),
    };
    // The gateway also runs it before the time entered, at 03:00.
    assert_eq!(
        schedule_text(&hours(4, "07:00")),
        "Every 4 hours from 03:00 to 23:00"
    );
    assert_eq!(
        schedule_text(&hours(12, "13:30")),
        "Every 12 hours from 01:30 to 13:30"
    );
    assert_eq!(
        schedule_text(&hours(3, "00:15")),
        "Every 3 hours from 00:15 to 21:15"
    );
    assert_eq!(schedule_text(&hours(1, "07:00")), "Every hour on the hour");
    assert_eq!(
        schedule_text(&hours(1, "06:45")),
        "Every hour at 45 minutes past"
    );
    assert_eq!(
        schedule_text(&Schedule::Daily { at: at("07:00") }),
        "Every day at 07:00"
    );
}

#[test]
fn routines_follow_their_events() {
    let mut fixture = Fixture::new();
    fixture.settings_loaded(vec![digest()]);
    fixture.event(
        11,
        Event::RoutineCreated {
            routine: routine(RoutineId(4), "Evening quiz", 3_000),
        },
    );
    assert_eq!(fixture.fact("routine_count"), 2_u64.into());
    assert!(fixture.shows("Evening quiz"));
    let mut paused = digest();
    paused.paused = true;
    paused.updated_at = Timestamp(4_000);
    fixture.event(11 + 1, Event::RoutineUpdated { routine: paused });
    assert!(fixture.shows("Every day at 07:00 · Paused"));
    fixture.event(
        13,
        Event::RoutineDeleted {
            bot_id: TUTOR,
            routine_id: RoutineId(4),
        },
    );
    assert_eq!(fixture.fact("routine_count"), 1_u64.into());
    assert!(!fixture.shows("Evening quiz"));
}

#[test]
fn a_new_routine_is_checked_before_it_is_saved() {
    let mut fixture = Fixture::new();
    fixture.settings_loaded(Vec::new());
    assert!(fixture.shows("No routines yet"));
    fixture.reveal("New routine");
    fixture.click("New routine");
    assert_eq!(fixture.fact("routine_form"), "new".into());
    assert!(fixture.shows("Times are in your time zone, Australia/Adelaide."));
    assert_eq!(
        fixture.availability_of(BokkieAction::SaveRoutine),
        disabled("Enter a name")
    );
    fixture.send(BokkieIntent::EditRoutineName("Morning digest".into()));
    assert_eq!(
        fixture.availability_of(BokkieAction::SaveRoutine),
        disabled("Enter a prompt")
    );
    fixture.send(BokkieIntent::EditRoutinePrompt(
        "Summarise the news.".into(),
    ));
    assert!(fixture.enabled(BokkieAction::SaveRoutine));
    fixture.send(BokkieIntent::EditRoutineName("x".repeat(61)));
    assert_eq!(
        fixture.availability_of(BokkieAction::SaveRoutine),
        disabled("Keep the name to 60 characters")
    );
    fixture.send(BokkieIntent::EditRoutineName("Morning digest".into()));
    fixture.send(BokkieIntent::EditRoutineAt("7am".into()));
    assert_eq!(
        fixture.availability_of(BokkieAction::SaveRoutine),
        disabled("Enter the time as hours and minutes, such as 07:00")
    );
    fixture.send(BokkieIntent::EditRoutineAt("07:30".into()));
    // Every few hours takes only what the gateway accepts.
    assert!(!fixture.enabled(BokkieAction::RoutineMon));
    fixture.reveal("Every few hours");
    fixture.click("Every few hours");
    fixture.send(BokkieIntent::EditRoutineEvery("5".into()));
    assert_eq!(
        fixture.availability_of(BokkieAction::SaveRoutine),
        disabled("Run every 1, 2, 3, 4, 6, 8 or 12 hours")
    );
    // Chosen days need one at least.
    fixture.reveal("On chosen days");
    fixture.click("On chosen days");
    assert_eq!(
        fixture.availability_of(BokkieAction::SaveRoutine),
        disabled("Choose at least one day")
    );
    fixture.click("Friday");
    fixture.click("Monday");
    fixture.reveal("Notify every run");
    fixture.click("Notify every run");
    fixture.reveal("Save routine");
    fixture.click("Save routine");
    let request = CreateRoutineRequest {
        name: "Morning digest".into(),
        prompt: "Summarise the news.".into(),
        schedule: Schedule::Days {
            days: vec![Weekday::Mon, Weekday::Fri],
            at: TimeOfDay::new(7, 30).unwrap(),
        },
        push: RoutinePush::NotifyOnly,
        paused: false,
    };
    assert_eq!(
        fixture.sent(),
        [ApiRequest::CreateRoutine(TUTOR, request.clone())]
    );
    assert_eq!(fixture.fact("routine_form"), "saving".into());
    // The gateway's refusal keeps the form, to correct.
    fixture.reply(ApiReply::RoutineCreated(
        TUTOR,
        Err(error(
            409,
            ErrorCode::Conflict,
            "This Bot already has a routine called Morning digest",
        )),
    ));
    assert_eq!(fixture.fact("routine_form"), "new".into());
    assert_eq!(
        fixture.fact("routine_error"),
        "This Bot already has a routine called Morning digest".into()
    );
    fixture.click("Save routine");
    assert_eq!(fixture.sent(), [ApiRequest::CreateRoutine(TUTOR, request)]);
    let mut created = digest();
    created.push = RoutinePush::NotifyOnly;
    fixture.reply(ApiReply::RoutineCreated(TUTOR, Ok(created)));
    assert_eq!(fixture.fact("routine_form"), "".into());
    assert_eq!(fixture.fact("routine_count"), 1_u64.into());
    assert!(
        fixture.shows("Every day at 07:00 · Next run today at 8:30 PM · Notifies only when asked")
    );
}

#[test]
fn editing_a_routine_sends_only_what_changed() {
    let mut fixture = Fixture::new();
    fixture.settings_loaded(vec![digest()]);
    fixture.click("Edit routine Morning digest");
    assert_eq!(fixture.fact("routine_form"), "editing".into());
    // Unchanged, saving only closes the form.
    fixture.reveal("Save routine");
    fixture.click("Save routine");
    assert!(fixture.sent().is_empty());
    assert_eq!(fixture.fact("routine_form"), "".into());
    fixture.click("Edit routine Morning digest");
    fixture.send(BokkieIntent::EditRoutinePrompt(
        "Summarise the weather.".into(),
    ));
    fixture.reveal("Weekdays");
    fixture.click("Weekdays");
    fixture.reveal("Save routine");
    fixture.click("Save routine");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::UpdateRoutine(
            TUTOR,
            DIGEST,
            UpdateRoutineRequest {
                prompt: Some("Summarise the weather.".into()),
                schedule: Some(Schedule::Weekdays {
                    at: TimeOfDay::new(7, 0).unwrap()
                }),
                ..Default::default()
            }
        )]
    );
    let mut changed = digest();
    changed.prompt = "Summarise the weather.".into();
    changed.updated_at = Timestamp(5_000);
    fixture.reply(ApiReply::RoutineUpdated(TUTOR, DIGEST, Ok(changed)));
    assert_eq!(fixture.fact("routine_form"), "".into());
    assert_eq!(
        fixture
            .app()
            .store()
            .routines(TUTOR)
            .unwrap()
            .routine(DIGEST)
            .unwrap()
            .prompt,
        "Summarise the weather."
    );
}

#[test]
fn a_routine_pauses_resumes_and_is_deleted_after_confirming() {
    let mut fixture = Fixture::new();
    fixture.settings_loaded(vec![digest()]);
    fixture.click("Paused Morning digest");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::UpdateRoutine(
            TUTOR,
            DIGEST,
            UpdateRoutineRequest {
                paused: Some(true),
                ..Default::default()
            }
        )]
    );
    assert_eq!(
        fixture.availability_with(BokkieAction::PauseRoutine, DIGEST.0),
        disabled("Saving the routine")
    );
    // An edit waits for the pause, whose reply is not the form's.
    assert_eq!(
        fixture.availability_with(BokkieAction::EditRoutine, DIGEST.0),
        disabled("Saving the routine")
    );
    let mut paused = digest();
    paused.paused = true;
    paused.updated_at = Timestamp(3_000);
    fixture.reply(ApiReply::RoutineUpdated(TUTOR, DIGEST, Ok(paused)));
    assert!(fixture.shows("Every day at 07:00 · Paused"));
    fixture.click("Paused Morning digest");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::UpdateRoutine(
            TUTOR,
            DIGEST,
            UpdateRoutineRequest {
                paused: Some(false),
                ..Default::default()
            }
        )]
    );
    fixture.reply(ApiReply::RoutineUpdated(
        TUTOR,
        DIGEST,
        Err(CommandError::unreachable()),
    ));
    assert_ne!(fixture.fact("routine_error"), "".into());
    // Deleting asks first.
    fixture.click("Delete Morning digest");
    assert_eq!(fixture.fact("routine_delete"), DIGEST.0.into());
    assert!(fixture.sent().is_empty());
    fixture.click("Keep routine");
    assert_eq!(fixture.fact("routine_delete"), 0.into());
    fixture.click("Delete Morning digest");
    fixture.click("Delete for good");
    assert_eq!(fixture.sent(), [ApiRequest::DeleteRoutine(TUTOR, DIGEST)]);
    fixture.reply(ApiReply::RoutineDeleted(TUTOR, DIGEST, Ok(())));
    assert_eq!(fixture.fact("routine_count"), 0_u64.into());
    assert_eq!(fixture.fact("routine_delete"), 0.into());
    // A routine that is gone takes no row action.
    assert_eq!(
        fixture.availability_with(BokkieAction::RunRoutine, DIGEST.0),
        Availability::Hidden
    );
}

#[test]
fn run_now_shows_the_run_in_its_history_as_it_goes() {
    let mut fixture = Fixture::new();
    fixture.settings_loaded(vec![digest()]);
    fixture.click("Run now Morning digest");
    assert_eq!(
        fixture.sent(),
        [
            ApiRequest::LoadRuns(TUTOR, DIGEST, None),
            ApiRequest::RunRoutine(TUTOR, DIGEST)
        ]
    );
    assert_eq!(fixture.fact("history"), DIGEST.0.into());
    // The run's events can arrive before either reply.
    fixture.event(
        11,
        Event::RoutineRunUpdated {
            run: run(21, RunState::Running, &[30]),
        },
    );
    assert_eq!(fixture.fact("run_state"), "running".into());
    fixture.reply(ApiReply::RoutineStarted(
        TUTOR,
        DIGEST,
        Ok(run(21, RunState::Waiting, &[])),
    ));
    assert_eq!(fixture.fact("run_state"), "running".into(), "never back");
    fixture.reply(ApiReply::Runs(
        TUTOR,
        DIGEST,
        None,
        Ok(RoutineRunPage {
            runs: vec![
                run(21, RunState::Waiting, &[]),
                RoutineRun {
                    trigger: RunTrigger::Schedule,
                    ..run(20, RunState::Failed, &[12])
                },
            ],
            older: Some(RoutineRunId(20)),
        }),
    ));
    assert_eq!(fixture.fact("runs"), 2_u64.into());
    assert_eq!(fixture.fact("run"), 21.into());
    assert_eq!(fixture.fact("run_state"), "running".into());
    assert!(fixture.shows("The Bot could not reply"));
    fixture.event(
        12,
        Event::RoutineRunUpdated {
            run: run(21, RunState::Succeeded, &[30, 31]),
        },
    );
    assert_eq!(fixture.fact("run_state"), "succeeded".into());
    assert_eq!(fixture.fact("run_messages"), 2_u64.into());
    // Started at now, on Ada's clock in Adelaide.
    assert!(fixture.shows("Succeeded · run now · 6:30 PM"));
    // Earlier runs load a page at a time.
    fixture.reveal("Earlier runs");
    fixture.click("Earlier runs");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::LoadRuns(TUTOR, DIGEST, Some(RoutineRunId(20)))]
    );
    fixture.reply(ApiReply::Runs(
        TUTOR,
        DIGEST,
        Some(RoutineRunId(20)),
        Ok(RoutineRunPage {
            runs: vec![run(19, RunState::Missed, &[])],
            older: None,
        }),
    ));
    assert_eq!(fixture.fact("runs"), 3_u64.into());
    // A run without messages offers nothing to show.
    assert_eq!(
        fixture.availability_with(BokkieAction::ShowRun, 19),
        Availability::Hidden
    );
    // The routine deleted elsewhere takes its history with it.
    fixture.event(
        13,
        Event::RoutineDeleted {
            bot_id: TUTOR,
            routine_id: DIGEST,
        },
    );
    assert_eq!(fixture.fact("history"), 0.into());
    assert_eq!(fixture.fact("routine_count"), 0_u64.into());
}

#[test]
fn show_run_brings_its_marker_into_view_loading_earlier_pages() {
    let mut fixture = Fixture::new();
    fixture.sent.set_now(Timestamp(NOW));
    fixture.signed_in_and_live_as(session(user()), 10);
    fixture.click("Open Tutor");
    let _ = fixture.sent();
    // The newest page does not hold the run's marker.
    fixture.reply(ApiReply::Messages(
        TUTOR,
        Ok(MessagePage {
            messages: vec![message(40, Author::User, "Hello", MessageState::Complete)],
            older: Some(MessageId(40)),
            seq: Seq(10),
        }),
    ));
    fixture.click("Settings");
    let _ = fixture.sent();
    fixture.reply(ApiReply::Routines(
        TUTOR,
        Ok(RoutineList {
            routines: vec![digest()],
            seq: Seq(10),
        }),
    ));
    fixture.click("History Morning digest");
    assert_eq!(fixture.sent(), [ApiRequest::LoadRuns(TUTOR, DIGEST, None)]);
    fixture.reply(ApiReply::Runs(
        TUTOR,
        DIGEST,
        None,
        Ok(RoutineRunPage {
            runs: vec![
                run(21, RunState::Succeeded, &[30, 31]),
                run(5, RunState::Succeeded, &[2]),
            ],
            older: None,
        }),
    ));
    fixture.reveal("Show run 21");
    fixture.click("Show run 21");
    assert_eq!(fixture.fact("screen"), "conversation".into());
    assert_eq!(fixture.fact("showing"), 30.into());
    assert_eq!(
        fixture.sent(),
        [ApiRequest::LoadOlder(TUTOR, MessageId(40))]
    );
    fixture.reply(ApiReply::Older(
        TUTOR,
        MessageId(40),
        Ok(MessagePage {
            messages: vec![
                marker(30, 21),
                message(31, Author::Bot, "Routine done.", MessageState::Complete),
            ],
            older: Some(MessageId(30)),
            seq: Seq(10),
        }),
    ));
    // Held now, so no further page loads.
    assert!(fixture.sent().is_empty());
    assert_eq!(
        fixture.fact("marker"),
        "Morning digest · 6:30 PM · test run".into()
    );
    assert!(fixture.shows("Routine run: Morning digest · 6:30 PM · test run"));
    // A run from before the conversation's start cannot be shown.
    fixture.click("Settings");
    let _ = fixture.sent();
    fixture.reply(ApiReply::Routines(
        TUTOR,
        Ok(RoutineList {
            routines: vec![digest()],
            seq: Seq(10),
        }),
    ));
    fixture.click("History Morning digest");
    let _ = fixture.sent();
    fixture.reply(ApiReply::Runs(
        TUTOR,
        DIGEST,
        None,
        Ok(RoutineRunPage {
            runs: vec![run(5, RunState::Succeeded, &[2])],
            older: None,
        }),
    ));
    fixture.act_on(BokkieAction::ShowRun, 5);
    assert_eq!(
        fixture.sent(),
        [ApiRequest::LoadOlder(TUTOR, MessageId(30))]
    );
    fixture.reply(ApiReply::Older(
        TUTOR,
        MessageId(30),
        Ok(MessagePage {
            messages: vec![message(20, Author::User, "Earlier", MessageState::Complete)],
            older: None,
            seq: Seq(10),
        }),
    ));
    assert_eq!(fixture.fact("showing"), 0.into());
    assert!(fixture.shows("That run's messages are no longer in the conversation"));
}

#[test]
fn the_time_zone_is_saved_and_times_follow_it() {
    let mut fixture = Fixture::new();
    fixture.settings_loaded(Vec::new());
    assert!(fixture.shows("Last changed today at 3:30 PM"));
    fixture.click("Back");
    fixture.click("Back");
    assert_eq!(fixture.fact("time_zone"), "Australia/Adelaide".into());
    assert_eq!(
        fixture.availability_of(BokkieAction::SaveTimeZone),
        disabled("That is your time zone already")
    );
    fixture.send(BokkieIntent::EditTimeZone("Europe/Lndon".into()));
    fixture.reveal("Save time zone");
    fixture.click("Save time zone");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::SetTimeZone(SetTimeZoneRequest {
            time_zone: "Europe/Lndon".into()
        })]
    );
    fixture.reply(ApiReply::TimeZoneSet(Err(error(
        400,
        ErrorCode::InvalidRequest,
        "Europe/Lndon is not a time zone",
    ))));
    assert_eq!(
        fixture.fact("time_zone_error"),
        "Europe/Lndon is not a time zone".into()
    );
    fixture.send(BokkieIntent::EditTimeZone("Europe/London".into()));
    fixture.click("Save time zone");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::SetTimeZone(SetTimeZoneRequest {
            time_zone: "Europe/London".into()
        })]
    );
    fixture.reply(ApiReply::TimeZoneSet(Ok(session(User {
        time_zone: "Europe/London".into(),
        ..user()
    }))));
    assert_eq!(fixture.fact("time_zone"), "Europe/London".into());
    assert_eq!(fixture.fact("time_zone_error"), "".into());
    // Times now show on the London clock.
    fixture.click("Open Tutor");
    let _ = fixture.sent();
    fixture.reply(ApiReply::Messages(
        TUTOR,
        Ok(MessagePage {
            messages: hello(),
            older: None,
            seq: Seq(10),
        }),
    ));
    fixture.click("Settings");
    let _ = fixture.sent();
    fixture.reply(ApiReply::Notes(TUTOR, Ok(notes("Likes fractions."))));
    assert!(fixture.shows("Last changed today at 5:00 AM"));
}

#[test]
fn a_reset_reloads_what_the_settings_show() {
    let mut fixture = Fixture::new();
    fixture.settings_loaded(vec![digest()]);
    fixture.click("History Morning digest");
    let _ = fixture.sent();
    fixture.frame(bokkie_protocol::events::ServerFrame::Reset { after: Seq(10) });
    let sent = fixture.sent();
    for request in [
        ApiRequest::ListRoutines(TUTOR),
        ApiRequest::LoadNotes(TUTOR),
        ApiRequest::LoadToolCalls(TUTOR, None),
        ApiRequest::LoadRuns(TUTOR, DIGEST, None),
    ] {
        assert_eq!(
            sent.iter().filter(|sent| **sent == request).count(),
            1,
            "{request:?} in {sent:?}"
        );
    }
}

#[test]
fn a_session_that_ended_during_run_now_signs_out() {
    let mut fixture = Fixture::new();
    fixture.settings_loaded(vec![digest()]);
    fixture.click("Run now Morning digest");
    let _ = fixture.sent();
    fixture.reply(ApiReply::RoutineStarted(
        TUTOR,
        DIGEST,
        Err(unauthenticated()),
    ));
    assert_eq!(fixture.fact("screen"), "signed_out".into());
}

#[test]
fn the_settings_present_their_controls_at_phone_width() {
    let mut fixture = Fixture::new();
    fixture.settings_loaded(vec![digest()]);
    fixture.assert_presentable();
    fixture.click("History Morning digest");
    fixture.reply(ApiReply::Runs(
        TUTOR,
        DIGEST,
        None,
        Ok(RoutineRunPage {
            runs: vec![run(21, RunState::Succeeded, &[30, 31])],
            older: None,
        }),
    ));
    fixture.click("Edit routine Morning digest");
    fixture.reveal("On chosen days");
    fixture.click("On chosen days");
    fixture.assert_presentable();
}
