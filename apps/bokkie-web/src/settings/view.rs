//! A Bot's settings screen and the time zone section: plain stand-ins,
//! short controls sharing a wrapped row and the rest on rows of their own.

use bokkie_protocol::api::{Grant, Role, Routine, RoutinePush, RoutineRun, RunTrigger, Weekday};
use bokkie_protocol::api::{ToolCall, ToolCallOutcome};
use stobie_ui_egui::local_time::LocalTime;
use stobie_ui_egui::{
    ActionEmphasis, BoundAction, PresentationContext, PresentationScope, ReadingTextSpec,
    SemanticUiId, TextRole, observe_visible_only,
};

use super::{
    Clear, History, ROUTINE_AT_ID, ROUTINE_EVERY_ID, ROUTINE_NAME_ID, ROUTINE_PROMPT_ID, Repeat,
    RoutineForm, Settings, TIME_ZONE_ID, day_action, day_name, grant_action, grantable,
    member_may_grant, repeat_action, run_state_text, schedule_text,
};
use crate::app::{App, BokkieAction, BokkieIntent, Open, SignedIn};
use crate::ui::{BokkieFrame, line, submitted, text_area, text_field};

/// What every grants view says of reading the web.
pub(crate) const EXFILTRATION_NOTICE: &str =
    "A Bot that reads the web can leak what you tell it through the addresses it fetches.";

pub(crate) fn present(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    app: &App,
    signed_in: &SignedIn,
    open: &Open,
) {
    let Some(settings) = &open.settings else {
        return;
    };
    frame.action(ui, p, BokkieAction::Back, ActionEmphasis::Normal);
    let bot = app.store().bot(open.bot);
    let name = bot.map_or("Bot", |bot| bot.name.as_str());
    p.heading(ui, "title", &format!("{name} settings"));
    frame.action(ui, p, BokkieAction::Refresh, ActionEmphasis::Normal);
    let local = app.local_time();
    // Times said as today or yesterday stay true past midnight.
    frame.request_repaint_after("settings days", crate::times::until_the_day_changes(&local));
    observe_visible_only(ui, |ui| {
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                let mut rows = frame.presentation(
                    ui,
                    PresentationScope::new("settings"),
                    SemanticUiId::root(),
                );
                let p = &mut rows;
                grants_ui(
                    ui,
                    p,
                    frame,
                    signed_in,
                    open,
                    bot.map(|b| b.grants.as_slice()),
                );
                ui.separator();
                notes_ui(ui, p, frame, &local, settings);
                ui.separator();
                tool_calls_ui(ui, p, frame, &local, settings);
                ui.separator();
                routines_ui(ui, p, frame, app, signed_in, &local, settings);
            });
    });
}

fn grant_about(grant: Grant) -> &'static str {
    match grant {
        Grant::Notes => "Keeps notes that it reads at the start of each turn.",
        Grant::Seen => "Remembers what it has already shown you.",
        Grant::Notify => "Sends you notifications.",
        Grant::WebFetch => "Reads web pages.",
        Grant::WebSearch => "Searches the web. Web search is not available yet.",
    }
}

fn grants_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    signed_in: &SignedIn,
    open: &Open,
    grants: Option<&[Grant]>,
) {
    p.heading(ui, "grants-title", "Grants");
    let Some(grants) = grants else {
        p.status(ui, "grants-status", "Loading the Bot…", false);
        return;
    };
    let member = signed_in.user.role != Role::Admin;
    for grant in Grant::ALL {
        let held = grants.contains(&grant);
        // Never offered until it exists; one held can still be taken away.
        if !held && !grantable(grant) {
            continue;
        }
        frame.toggle(ui, p, ("grant", grant.name()), grant_action(grant), held);
        let mut about = grant_about(grant).to_owned();
        if member && !held && !member_may_grant(grant) {
            about.push_str(&format!(" Only an admin can grant {}.", grant.name()));
        }
        p.content(
            ui,
            ("grant-about", grant.name()),
            &about,
            line(TextRole::Secondary, 2),
        );
    }
    p.content(
        ui,
        "exfiltration",
        EXFILTRATION_NOTICE,
        line(TextRole::Secondary, 3),
    );
    if open.updating {
        p.status(ui, "grants-saving", "Saving…", false);
    } else if let Some(error) = &open.update_error {
        p.status(ui, "grants-saving", error, true);
    }
}

fn notes_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    local: &LocalTime,
    settings: &Settings,
) {
    p.heading(ui, "notes-title", "Notes");
    if let Some(error) = &settings.notes_error {
        p.status(ui, "notes-status", error, true);
    }
    match &settings.notes {
        None if settings.notes_loading => {
            p.status(ui, "notes-loading", "Loading notes…", false);
        }
        None => {}
        Some(notes) if notes.text.is_empty() => {
            p.content(
                ui,
                "notes-empty",
                "The Bot has no notes",
                line(TextRole::Secondary, 1),
            );
        }
        Some(notes) => {
            p.reading_text(
                ui,
                "notes-text",
                &notes.text,
                ReadingTextSpec::new(TextRole::Body),
            );
            if let Some(at) = notes.updated_at {
                let when = crate::times::on_day_at(local, at);
                p.content(
                    ui,
                    "notes-changed",
                    &format!("Last changed {when}"),
                    line(TextRole::Caption, 1),
                );
            }
        }
    }
    match settings.clear {
        Clear::Idle => {
            frame.action(ui, p, BokkieAction::ClearNotes, ActionEmphasis::Normal);
        }
        Clear::Confirming => {
            p.status(
                ui,
                "notes-confirm",
                "Clear the Bot's notes for good? This cannot be undone.",
                false,
            );
            ui.horizontal_wrapped(|ui| {
                frame.action(
                    ui,
                    p,
                    BokkieAction::ConfirmClearNotes,
                    ActionEmphasis::Primary,
                );
                frame.action(ui, p, BokkieAction::KeepNotes, ActionEmphasis::Normal);
            });
        }
        Clear::Clearing => {
            p.status(ui, "notes-confirm", "Clearing…", false);
        }
    }
}

fn outcome_text(outcome: ToolCallOutcome) -> &'static str {
    match outcome {
        ToolCallOutcome::Ok => "done",
        ToolCallOutcome::Failed => "failed",
        ToolCallOutcome::Refused => "refused",
        ToolCallOutcome::TimedOut => "timed out",
    }
}

fn call_text(local: &LocalTime, call: &ToolCall) -> String {
    let mut text = format!(
        "{} · {} · {} ms · {}",
        call.tool,
        outcome_text(call.outcome),
        call.duration_ms,
        crate::times::message_time(local, call.at)
    );
    if call.routine_run_id.is_some() {
        text.push_str(" · routine run");
    }
    text
}

fn tool_calls_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    local: &LocalTime,
    settings: &Settings,
) {
    p.heading(ui, "calls-title", "Recent tool calls");
    if settings.calls_loaded && settings.calls.is_empty() {
        p.content(
            ui,
            "calls-empty",
            "No tool calls yet",
            line(TextRole::Secondary, 1),
        );
    }
    for call in &settings.calls {
        p.content(
            ui,
            ("call", call.id),
            &call_text(local, call),
            line(TextRole::Caption, 2),
        );
        if !call.summary.is_empty() {
            p.content(
                ui,
                ("call-summary", call.id),
                &call.summary,
                line(TextRole::Secondary, 3),
            );
        }
    }
    if settings.calls_loading.is_some() {
        p.status(ui, "calls-status", "Loading tool calls…", false);
    } else if let Some(error) = &settings.calls_error {
        p.status(ui, "calls-status", error, true);
    }
    if settings.calls_older.is_some() {
        frame.action(ui, p, BokkieAction::OlderToolCalls, ActionEmphasis::Normal);
    }
}

fn routine_text(local: &LocalTime, routine: &Routine) -> String {
    let mut text = schedule_text(&routine.schedule);
    if routine.paused {
        text.push_str(" · Paused");
    } else if let Some(next) = routine.next_run_at {
        text.push_str(&format!(
            " · Next run {}",
            crate::times::on_day_at(local, next)
        ));
    }
    match routine.push {
        RoutinePush::EveryRun => {}
        RoutinePush::NotifyOnly => text.push_str(" · Notifies only when asked"),
    }
    if routine.failures > 0 {
        text.push_str(&format!(" · Failed {} in a row", routine.failures));
    }
    text
}

fn routines_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    app: &App,
    signed_in: &SignedIn,
    local: &LocalTime,
    settings: &Settings,
) {
    p.heading(ui, "routines-title", "Routines");
    let zone = &signed_in.user.time_zone;
    p.content(
        ui,
        "routines-about",
        &format!("Routines run in your time zone, {zone}."),
        line(TextRole::Secondary, 2),
    );
    let routines = app.open_bot().and_then(|bot| app.store().routines(bot));
    if let Some(error) = &settings.routines_error {
        p.status(ui, "routines-status", error, true);
    } else if routines.is_none() {
        p.status(ui, "routines-status", "Loading routines…", false);
    }
    if let Some(error) = &settings.routine_error {
        p.status(ui, "routine-status", error, true);
    }
    let Some(routines) = routines else {
        return;
    };
    if routines.is_empty() {
        p.content(
            ui,
            "routines-empty",
            "No routines yet",
            line(TextRole::Secondary, 1),
        );
    }
    for routine in routines.routines() {
        let key = routine.id.0;
        p.content(ui, ("routine", key), &routine.name, line(TextRole::Body, 2));
        p.content(
            ui,
            ("routine-details", key),
            &routine_text(local, routine),
            line(TextRole::Secondary, 2),
        );
        let bound = |action| BoundAction::new(action, key).with_subject(&routine.name);
        let history = settings
            .history
            .as_ref()
            .filter(|h| h.routine == routine.id);
        ui.horizontal_wrapped(|ui| {
            frame.action(
                ui,
                p,
                bound(BokkieAction::RunRoutine),
                ActionEmphasis::Normal,
            );
            frame.toggle(
                ui,
                p,
                ("paused", key),
                bound(BokkieAction::PauseRoutine),
                routine.paused,
            );
            frame.action(
                ui,
                p,
                bound(BokkieAction::EditRoutine),
                ActionEmphasis::Normal,
            );
            frame.toggle(
                ui,
                p,
                ("history", key),
                bound(BokkieAction::RoutineHistory),
                history.is_some(),
            );
            frame.action(
                ui,
                p,
                bound(BokkieAction::DeleteRoutine),
                ActionEmphasis::Normal,
            );
        });
        if settings.busy.contains(&routine.id) {
            p.status(ui, ("routine-busy", key), "Saving…", false);
        }
        if let Some((_, busy)) = settings.delete.filter(|(id, _)| *id == routine.id) {
            if busy {
                p.status(ui, ("routine-delete", key), "Deleting…", false);
            } else {
                p.status(
                    ui,
                    ("routine-delete", key),
                    &format!(
                        "Delete {} and its run history for good? Its runs stay in the conversation.",
                        routine.name
                    ),
                    false,
                );
                ui.horizontal_wrapped(|ui| {
                    frame.action(
                        ui,
                        p,
                        BokkieAction::ConfirmDeleteRoutine,
                        ActionEmphasis::Primary,
                    );
                    frame.action(ui, p, BokkieAction::KeepRoutine, ActionEmphasis::Normal);
                });
            }
        }
        if let Some(form) = settings
            .form
            .as_ref()
            .filter(|form| form.editing == Some(routine.id))
        {
            form_ui(ui, p, frame, signed_in, form);
        }
        if let Some(history) = history {
            history_ui(ui, p, frame, local, history);
        }
        ui.separator();
    }
    match settings.form.as_ref() {
        Some(form) if form.editing.is_none() => form_ui(ui, p, frame, signed_in, form),
        _ => {
            frame.action(ui, p, BokkieAction::NewRoutine, ActionEmphasis::Normal);
        }
    }
}

fn repeat_label(repeat: Repeat) -> &'static str {
    match repeat {
        Repeat::Daily => "every day",
        Repeat::Weekdays => "weekdays",
        Repeat::Days => "on chosen days",
        Repeat::Hours => "every few hours",
    }
}

fn form_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    signed_in: &SignedIn,
    form: &RoutineForm,
) {
    let title = if form.editing.is_some() {
        "Edit routine"
    } else {
        "New routine"
    };
    p.heading(ui, "routine-form-title", title);
    ui.add_enabled_ui(!form.busy, |ui| {
        let (name, field) = text_field(ui, p, ROUTINE_NAME_ID, "Name", &form.name, false);
        if name != form.name {
            frame.emit(BokkieIntent::EditRoutineName(name));
        }
        if submitted(ui, &field) {
            frame.trigger(BokkieAction::SaveRoutine);
        }
        let (prompt, _) = text_area(
            ui,
            p,
            ROUTINE_PROMPT_ID,
            "Prompt: what the Bot is asked each run",
            &form.prompt,
        );
        if prompt != form.prompt {
            frame.emit(BokkieIntent::EditRoutinePrompt(prompt));
        }
    });
    p.content(
        ui,
        "repeat-about",
        &format!("Repeats {}", repeat_label(form.repeat)),
        line(TextRole::Secondary, 1),
    );
    ui.horizontal_wrapped(|ui| {
        for repeat in Repeat::ALL {
            frame.toggle(
                ui,
                p,
                ("repeat", repeat_label(repeat)),
                repeat_action(repeat),
                form.repeat == repeat,
            );
        }
    });
    if form.repeat == Repeat::Days {
        ui.horizontal_wrapped(|ui| {
            for day in Weekday::ALL {
                frame.toggle(
                    ui,
                    p,
                    ("day", day_name(day)),
                    day_action(day),
                    form.days.contains(&day),
                );
            }
        });
    }
    ui.add_enabled_ui(!form.busy, |ui| {
        if form.repeat == Repeat::Hours {
            let (every, _) = text_field(
                ui,
                p,
                ROUTINE_EVERY_ID,
                "Every how many hours: 1, 2, 3, 4, 6, 8 or 12",
                &form.every,
                false,
            );
            if every != form.every {
                frame.emit(BokkieIntent::EditRoutineEvery(every));
            }
        }
        let label = if form.repeat == Repeat::Hours {
            "Starting at, such as 07:00"
        } else {
            "At, such as 07:00"
        };
        let (at, field) = text_field(ui, p, ROUTINE_AT_ID, label, &form.at, false);
        if at != form.at {
            frame.emit(BokkieIntent::EditRoutineAt(at));
        }
        if submitted(ui, &field) {
            frame.trigger(BokkieAction::SaveRoutine);
        }
    });
    p.content(
        ui,
        "routine-zone",
        &format!("Times are in your time zone, {}.", signed_in.user.time_zone),
        line(TextRole::Secondary, 2),
    );
    frame.toggle(
        ui,
        p,
        "routine-push",
        BokkieAction::RoutinePush,
        form.push == RoutinePush::EveryRun,
    );
    p.content(
        ui,
        "routine-push-about",
        "Off, it notifies you only when the Bot asks to.",
        line(TextRole::Secondary, 2),
    );
    if let Some(problem) = form.problem().filter(|_| !form.busy) {
        p.content(
            ui,
            "routine-problem",
            &problem,
            line(TextRole::Secondary, 2),
        );
    }
    ui.horizontal_wrapped(|ui| {
        frame.action(ui, p, BokkieAction::SaveRoutine, ActionEmphasis::Primary);
        frame.action(ui, p, BokkieAction::CancelRoutine, ActionEmphasis::Normal);
    });
    if form.busy {
        p.status(ui, "routine-form-status", "Saving…", false);
    } else if let Some(error) = &form.error {
        p.status(ui, "routine-form-status", error, true);
    }
}

fn run_text(local: &LocalTime, run: &RoutineRun) -> String {
    let trigger = match run.trigger {
        RunTrigger::Schedule => "scheduled",
        RunTrigger::Manual => "run now",
    };
    let when = run.started_at.unwrap_or(run.scheduled_for);
    format!(
        "{} · {trigger} · {}",
        run_state_text(run.state),
        crate::times::message_time(local, when)
    )
}

fn history_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    local: &LocalTime,
    history: &History,
) {
    let key = history.routine.0;
    p.heading(ui, ("history-title", key), "Run history");
    if history.loaded && history.runs.is_empty() {
        p.content(
            ui,
            ("history-empty", key),
            "No runs yet",
            line(TextRole::Secondary, 1),
        );
    }
    for run in history.newest_first() {
        let id = run.id.0;
        p.content(
            ui,
            ("run", id),
            &run_text(local, run),
            line(TextRole::Caption, 2),
        );
        if let Some(reason) = run.reason.as_deref().filter(|r| !r.is_empty()) {
            p.content(ui, ("run-reason", id), reason, line(TextRole::Secondary, 3));
        }
        if !run.messages.is_empty() {
            frame.action(
                ui,
                p,
                BoundAction::new(BokkieAction::ShowRun, id).with_subject(format!("run {id}")),
                ActionEmphasis::Normal,
            );
        }
    }
    if history.loading.is_some() {
        p.status(ui, ("history-status", key), "Loading runs…", false);
    } else if let Some(error) = &history.error {
        p.status(ui, ("history-status", key), error, true);
    }
    if history.older.is_some() {
        frame.action(ui, p, BokkieAction::OlderRuns, ActionEmphasis::Normal);
    }
}

/// The user's time zone, which routines run in and times are shown in.
pub(crate) fn time_zone_ui(
    ui: &mut egui::Ui,
    p: &mut PresentationContext,
    frame: &mut BokkieFrame<'_, '_>,
    signed_in: &SignedIn,
) {
    p.heading(ui, "time-zone-title", "Time zone");
    p.content(
        ui,
        "time-zone-about",
        &format!(
            "Times show and routines run in {}. Enter another IANA name, such as Europe/London.",
            signed_in.user.time_zone
        ),
        line(TextRole::Secondary, 3),
    );
    let value = signed_in
        .zone_draft
        .as_deref()
        .unwrap_or(&signed_in.user.time_zone);
    ui.add_enabled_ui(!signed_in.zone_saving, |ui| {
        let (zone, field) = text_field(ui, p, TIME_ZONE_ID, "Time zone", value, false);
        if zone != value {
            frame.emit(BokkieIntent::EditTimeZone(zone));
        }
        if submitted(ui, &field) {
            frame.trigger(BokkieAction::SaveTimeZone);
        }
    });
    frame.action(ui, p, BokkieAction::SaveTimeZone, ActionEmphasis::Normal);
    if signed_in.zone_saving {
        p.status(ui, "time-zone-status", "Saving…", false);
    } else if let Some(error) = &signed_in.zone_error {
        p.status(ui, "time-zone-status", error, true);
    }
}
