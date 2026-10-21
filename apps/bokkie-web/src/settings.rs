//! A Bot's panel, opened from its conversation: its routines with their
//! run history, its recent runs, its grants, its notes and its activity,
//! the recent tool calls. Also the user's own time zone, which routines
//! run in and times are shown in.
//!
//! The routines themselves live in the [`Store`](crate::store::Store),
//! which keeps them current from the event log; everything else here loads
//! when the panel opens and again on Refresh. The recent runs gather each
//! routine's newest few, and they and a routine's history take
//! [`Event::RoutineRunUpdated`](bokkie_protocol::events::Event) directly,
//! keeping whichever copy of a run is further on. The gateway sends no
//! event for a tool call, so the activity and the notes load again when a
//! turn of the Bot's ends.

pub(crate) mod view;

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use bokkie_protocol::api::{
    Bot, BotNotes, CreateRoutineRequest, Grant, Role, Routine, RoutinePush, RoutineRun, RunState,
    Schedule, SetTimeZoneRequest, TimeOfDay, ToolCall, UpdateBotRequest, UpdateRoutineRequest,
    Weekday,
};
use bokkie_protocol::{BotId, RoutineId, RoutineRunId};

use crate::api::{ApiReply, ApiRequest};
use crate::app::{App, BokkieAction, Session};
use crate::store::Snapshot;
use crate::wire::CommandError;

pub(crate) use view::{present, time_zone_ui};

/// The routine form's name field's stable ID.
pub const ROUTINE_NAME_ID: &str = "bokkie.routine-name";
/// The routine form's prompt field's stable ID.
pub const ROUTINE_PROMPT_ID: &str = "bokkie.routine-prompt";
/// The routine form's time field's stable ID.
pub const ROUTINE_AT_ID: &str = "bokkie.routine-at";
/// The routine form's every-few-hours field's stable ID.
pub const ROUTINE_EVERY_ID: &str = "bokkie.routine-every";
/// The time zone field's stable ID.
pub const TIME_ZONE_ID: &str = "bokkie.time-zone";

/// How many runs the recent runs show, newest first, across the routines.
pub(crate) const RECENT_SHOWN: usize = 8;

/// The longest routine name the gateway accepts, in characters.
const NAME_CHARS: usize = 60;
/// The longest routine prompt the gateway accepts, in characters.
const PROMPT_CHARS: usize = 8_000;

/// The grants a member may add to their own Bots; the others, which reach
/// outside the household, only an admin may. The gateway enforces it.
pub(crate) fn member_may_grant(grant: Grant) -> bool {
    matches!(grant, Grant::Notes | Grant::Seen | Grant::Notify)
}

/// The action that turns a grant on or off.
pub(crate) fn grant_action(grant: Grant) -> BokkieAction {
    match grant {
        Grant::Notes => BokkieAction::GrantNotes,
        Grant::Seen => BokkieAction::GrantSeen,
        Grant::Notify => BokkieAction::GrantNotify,
        Grant::WebFetch => BokkieAction::GrantWebFetch,
        Grant::WebSearch => BokkieAction::GrantWebSearch,
    }
}

fn action_grant(action: BokkieAction) -> Option<Grant> {
    Grant::ALL
        .into_iter()
        .find(|grant| grant_action(*grant) == action)
}

/// The action that chooses a day of the week.
pub(crate) fn day_action(day: Weekday) -> BokkieAction {
    match day {
        Weekday::Mon => BokkieAction::RoutineMon,
        Weekday::Tue => BokkieAction::RoutineTue,
        Weekday::Wed => BokkieAction::RoutineWed,
        Weekday::Thu => BokkieAction::RoutineThu,
        Weekday::Fri => BokkieAction::RoutineFri,
        Weekday::Sat => BokkieAction::RoutineSat,
        Weekday::Sun => BokkieAction::RoutineSun,
    }
}

fn action_day(action: BokkieAction) -> Option<Weekday> {
    Weekday::ALL
        .into_iter()
        .find(|day| day_action(*day) == action)
}

/// The action that chooses how a routine repeats.
pub(crate) fn repeat_action(repeat: Repeat) -> BokkieAction {
    match repeat {
        Repeat::Daily => BokkieAction::RepeatDaily,
        Repeat::Weekdays => BokkieAction::RepeatWeekdays,
        Repeat::Days => BokkieAction::RepeatDays,
        Repeat::Hours => BokkieAction::RepeatHours,
    }
}

fn action_repeat(action: BokkieAction) -> Option<Repeat> {
    Repeat::ALL
        .into_iter()
        .find(|repeat| repeat_action(*repeat) == action)
}

/// One Bot's settings, open on screen in place of its conversation, which
/// stays loaded behind them.
#[derive(Debug, Default)]
pub(crate) struct Settings {
    /// The notes, once loaded.
    pub(crate) notes: Option<BotNotes>,
    pub(crate) notes_loading: bool,
    /// Why the notes last failed to load or clear.
    pub(crate) notes_error: Option<String>,
    pub(crate) clear: Clear,
    /// The tool calls loaded, newest first.
    pub(crate) calls: Vec<ToolCall>,
    pub(crate) calls_loaded: bool,
    /// The cursor for earlier calls, or `None` at the first.
    pub(crate) calls_older: Option<i64>,
    /// The cursor of the page of calls loading, if one is.
    pub(crate) calls_loading: Option<Option<i64>>,
    pub(crate) calls_error: Option<String>,
    /// Why the routines last failed to load, until they load.
    pub(crate) routines_error: Option<String>,
    /// The routine being made or changed.
    pub(crate) form: Option<RoutineForm>,
    /// The routine whose deletion awaits confirmation, or is under way.
    pub(crate) delete: Option<(RoutineId, bool)>,
    /// Routines with a pause, a resume or a run under way.
    pub(crate) busy: BTreeSet<RoutineId>,
    /// Why the last pause, resume, run or deletion failed.
    pub(crate) routine_error: Option<String>,
    /// The run history shown, of one routine at a time.
    pub(crate) history: Option<History>,
    /// Each routine's newest few runs, gathered for the recent runs, with
    /// those the events have added since.
    pub(crate) recent: BTreeMap<RoutineRunId, RoutineRun>,
    /// The routines whose newest runs are loading.
    pub(crate) recent_loading: BTreeSet<RoutineId>,
    /// Why a routine's newest runs last failed to load.
    pub(crate) recent_error: Option<String>,
}

impl Settings {
    /// The recent runs shown, newest first.
    pub(crate) fn recent_runs(&self) -> impl Iterator<Item = &RoutineRun> {
        self.recent.values().rev().take(RECENT_SHOWN)
    }

    /// A run the panel shows, in the history or the recent runs.
    fn shown_run(&self, run: RoutineRunId) -> Option<&RoutineRun> {
        self.history
            .as_ref()
            .and_then(|h| h.runs.get(&run))
            .or_else(|| self.recent.get(&run))
    }

    /// The routines' state, as the `routines` fact names it.
    fn routines_state(&self, app: &App, bot: BotId) -> &'static str {
        if app.store().routines(bot).is_some() {
            "loaded"
        } else if self.routines_error.is_some() {
            "failed"
        } else {
            "loading"
        }
    }
}

/// Clearing the notes, which asks first.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Clear {
    #[default]
    Idle,
    Confirming,
    Clearing,
}

/// How a routine repeats, as the form offers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Repeat {
    Daily,
    Weekdays,
    Days,
    Hours,
}

impl Repeat {
    pub(crate) const ALL: [Self; 4] = [Self::Daily, Self::Weekdays, Self::Days, Self::Hours];
}

/// A routine being made, or changed.
#[derive(Debug)]
pub(crate) struct RoutineForm {
    /// The routine changed, or `None` for a new one.
    pub(crate) editing: Option<RoutineId>,
    pub(crate) name: String,
    pub(crate) prompt: String,
    pub(crate) repeat: Repeat,
    pub(crate) days: BTreeSet<Weekday>,
    /// Every how many hours, as typed.
    pub(crate) every: String,
    /// The time of day, as typed.
    pub(crate) at: String,
    pub(crate) push: RoutinePush,
    /// A save is under way.
    pub(crate) busy: bool,
    /// Why the last save failed.
    pub(crate) error: Option<String>,
}

impl RoutineForm {
    fn new() -> Self {
        Self {
            editing: None,
            name: String::new(),
            prompt: String::new(),
            repeat: Repeat::Daily,
            days: BTreeSet::new(),
            every: "4".into(),
            at: "07:00".into(),
            push: RoutinePush::EveryRun,
            busy: false,
            error: None,
        }
    }

    fn of(routine: &Routine) -> Self {
        let mut form = Self {
            editing: Some(routine.id),
            name: routine.name.clone(),
            prompt: routine.prompt.clone(),
            push: routine.push,
            ..Self::new()
        };
        let at = match &routine.schedule {
            Schedule::Daily { at } => {
                form.repeat = Repeat::Daily;
                at
            }
            Schedule::Weekdays { at } => {
                form.repeat = Repeat::Weekdays;
                at
            }
            Schedule::Days { days, at } => {
                form.repeat = Repeat::Days;
                form.days = days.iter().copied().collect();
                at
            }
            Schedule::Hours { every, at } => {
                form.repeat = Repeat::Hours;
                form.every = every.to_string();
                at
            }
        };
        form.at = at.to_string();
        form
    }

    /// The schedule entered, or why it is not one.
    pub(crate) fn schedule(&self) -> Result<Schedule, String> {
        let at: TimeOfDay = self
            .at
            .trim()
            .parse()
            .map_err(|_| "Enter the time as hours and minutes, such as 07:00".to_owned())?;
        Ok(match self.repeat {
            Repeat::Daily => Schedule::Daily { at },
            Repeat::Weekdays => Schedule::Weekdays { at },
            Repeat::Days if self.days.is_empty() => return Err("Choose at least one day".into()),
            Repeat::Days => Schedule::Days {
                days: self.days.iter().copied().collect(),
                at,
            },
            Repeat::Hours => match self.every.trim().parse::<u8>() {
                Ok(every) if Schedule::EVERY_HOURS.contains(&every) => {
                    Schedule::Hours { every, at }
                }
                _ => return Err("Run every 1, 2, 3, 4, 6, 8 or 12 hours".into()),
            },
        })
    }

    /// Why the form cannot be saved as it stands, if it cannot.
    pub(crate) fn problem(&self) -> Option<String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Some("Enter a name".into());
        }
        if name.chars().count() > NAME_CHARS {
            return Some(format!("Keep the name to {NAME_CHARS} characters"));
        }
        if name.contains(['\n', '\r']) {
            return Some("Keep the name to one line".into());
        }
        let prompt = self.prompt.trim();
        if prompt.is_empty() {
            return Some("Enter a prompt".into());
        }
        if prompt.chars().count() > PROMPT_CHARS {
            return Some("Keep the prompt to 8,000 characters".into());
        }
        self.schedule().err()
    }
}

/// One routine's run history, newest first, as far as it is loaded.
#[derive(Debug)]
pub(crate) struct History {
    pub(crate) routine: RoutineId,
    pub(crate) runs: BTreeMap<RoutineRunId, RoutineRun>,
    /// The cursor for earlier runs, or `None` at the first.
    pub(crate) older: Option<RoutineRunId>,
    pub(crate) loaded: bool,
    /// The cursor of the page of runs loading, if one is.
    pub(crate) loading: Option<Option<RoutineRunId>>,
    pub(crate) error: Option<String>,
}

impl History {
    fn new(routine: RoutineId) -> Self {
        Self {
            routine,
            runs: BTreeMap::new(),
            older: None,
            loaded: false,
            loading: Some(None),
            error: None,
        }
    }

    /// Keep a copy of a run unless the one held is further on. Returns
    /// whether it changed. Runs carry no sequence number, so a page and the
    /// events about it can arrive in either order; a run only moves on, from
    /// waiting to running to its outcome, gathering messages.
    pub(crate) fn merge(&mut self, run: RoutineRun) -> bool {
        run.routine_id == self.routine && merge_run(&mut self.runs, run)
    }

    /// The runs, newest first.
    pub(crate) fn newest_first(&self) -> impl Iterator<Item = &RoutineRun> {
        self.runs.values().rev()
    }
}

/// Keep a copy of a run in `runs` unless the one held is further on.
/// Returns whether it changed. Runs carry no sequence number, so a page and
/// the events about it can arrive in either order; a run only moves on,
/// from waiting to running to its outcome, gathering messages.
fn merge_run(runs: &mut BTreeMap<RoutineRunId, RoutineRun>, run: RoutineRun) -> bool {
    let rank = |run: &RoutineRun| match run.state {
        RunState::Waiting => 0,
        RunState::Running => 1,
        _ => 2,
    };
    match runs.get(&run.id) {
        Some(held) if *held == run => false,
        Some(held) if (rank(held), held.messages.len()) > (rank(&run), run.messages.len()) => false,
        _ => {
            runs.insert(run.id, run);
            true
        }
    }
}

/// What the gateway lets a user name as a schedule, in words, such as
/// "Every day at 07:00".
pub(crate) fn schedule_text(schedule: &Schedule) -> String {
    match schedule {
        Schedule::Daily { at } => format!("Every day at {at}"),
        Schedule::Weekdays { at } => format!("Weekdays at {at}"),
        Schedule::Days { days, at } => {
            let days: Vec<&str> = days.iter().map(|day| day_name(*day)).collect();
            format!("{} at {at}", days.join(", "))
        }
        // The gateway runs it at `at` and at every multiple of `every` hours
        // from it, through the whole day, so name the day's first and last
        // runs rather than `at`.
        Schedule::Hours { every: 1, at } if at.minute() == 0 => "Every hour on the hour".into(),
        Schedule::Hours { every: 1, at } => {
            format!("Every hour at {:02} minutes past", at.minute())
        }
        Schedule::Hours { every, at } => {
            let step = u16::from(*every) * 60;
            let minutes = u16::from(at.hour()) * 60 + u16::from(at.minute());
            let first = minutes % step.max(1);
            let last = first + (24 * 60 / step.max(1)).saturating_sub(1) * step;
            let time = |minutes: u16| format!("{:02}:{:02}", minutes / 60, minutes % 60);
            format!("Every {every} hours from {} to {}", time(first), time(last))
        }
    }
}

/// A day's short name, as the form's controls show it.
pub(crate) fn day_name(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "Mon",
        Weekday::Tue => "Tue",
        Weekday::Wed => "Wed",
        Weekday::Thu => "Thu",
        Weekday::Fri => "Fri",
        Weekday::Sat => "Sat",
        Weekday::Sun => "Sun",
    }
}

/// A run's state in words.
pub(crate) fn run_state_text(state: RunState) -> &'static str {
    match state {
        RunState::Waiting => "Waiting",
        RunState::Running => "Running",
        RunState::Succeeded => "Succeeded",
        RunState::Failed => "Failed",
        RunState::Missed => "Missed",
        RunState::Skipped => "Skipped",
        RunState::Interrupted => "Interrupted",
    }
}

/// A run's state, as the `run_state` fact names it.
fn run_state_name(state: RunState) -> &'static str {
    match state {
        RunState::Waiting => "waiting",
        RunState::Running => "running",
        RunState::Succeeded => "succeeded",
        RunState::Failed => "failed",
        RunState::Missed => "missed",
        RunState::Skipped => "skipped",
        RunState::Interrupted => "interrupted",
    }
}

type Reason = Option<Cow<'static, str>>;

fn because(reason: &'static str) -> Reason {
    Some(Cow::Borrowed(reason))
}

impl App {
    /// The open Bot's settings, if they are open.
    pub(crate) fn settings(&self) -> Option<&Settings> {
        self.open()?.settings.as_ref()
    }

    fn settings_mut(&mut self) -> Option<&mut Settings> {
        match &mut self.session {
            Session::SignedIn(signed_in) => signed_in.open.as_mut()?.settings.as_mut(),
            _ => None,
        }
    }

    /// The settings of `bot`, if they are open.
    fn settings_of(&mut self, bot: BotId) -> Option<&mut Settings> {
        match &mut self.session {
            Session::SignedIn(signed_in) => signed_in
                .open
                .as_mut()
                .filter(|open| open.bot == bot)?
                .settings
                .as_mut(),
            _ => None,
        }
    }

    /// Open the open Bot's settings, and load what they show.
    fn open_settings(&mut self) -> bool {
        let Session::SignedIn(signed_in) = &mut self.session else {
            return false;
        };
        let Some(open) = signed_in.open.as_mut() else {
            return false;
        };
        open.settings = Some(Settings::default());
        let bot = open.bot;
        self.load_settings(bot);
        true
    }

    /// Load the settings' notes, tool calls and routines, and the history
    /// shown, again.
    pub(crate) fn load_settings(&mut self, bot: BotId) {
        if self.settings_of(bot).is_none() {
            return;
        }
        self.load_settings_extras(bot);
        self.load(Snapshot::Routines(bot));
    }

    /// Load what the settings show that the store does not keep: the
    /// notes, the first page of tool calls and of the history shown.
    pub(crate) fn load_settings_extras(&mut self, bot: BotId) {
        let Some(settings) = self.settings_of(bot) else {
            return;
        };
        settings.notes_loading = true;
        settings.calls_loading = Some(None);
        let history = settings.history.as_mut().map(|history| {
            history.loading = Some(None);
            history.routine
        });
        self.send(ApiRequest::LoadNotes(bot));
        self.send(ApiRequest::LoadToolCalls(bot, None));
        if let Some(routine) = history {
            self.send(ApiRequest::LoadRuns(bot, routine, None));
        }
    }

    /// Close the open Bot's settings, back to its conversation. Returns
    /// whether they were open.
    pub(crate) fn close_settings(&mut self) -> bool {
        let Session::SignedIn(signed_in) = &mut self.session else {
            return false;
        };
        let Some(open) = signed_in.open.as_mut() else {
            return false;
        };
        if open.settings.take().is_none() {
            return false;
        }
        let bot = open.bot;
        self.store.close_routines(bot);
        true
    }

    /// The reply to a command of the settings or the time zone, or the reply
    /// back if it is another's.
    pub(crate) fn settings_reply(&mut self, reply: ApiReply) -> Result<bool, Box<ApiReply>> {
        let ended = |error: &crate::wire::CommandError| error.is_unauthenticated();
        match reply {
            ApiReply::Bot(id, result) => Ok(self.grant_from(id, result)),
            ApiReply::TimeZoneSet(result) => {
                let Session::SignedIn(signed_in) = &mut self.session else {
                    return Ok(false);
                };
                if !signed_in.zone_saving {
                    return Ok(false);
                }
                signed_in.zone_saving = false;
                match result {
                    Ok(session) if session.user.id == signed_in.user.id => {
                        signed_in.zone_draft = None;
                        signed_in.zone_error = None;
                        signed_in.account_changed(session.user);
                    }
                    Ok(_) => return Ok(self.settings_ended()),
                    Err(error) if ended(&error) => return Ok(self.settings_ended()),
                    // The name stays, to correct.
                    Err(error) => signed_in.zone_error = Some(error.message),
                }
                Ok(true)
            }
            ApiReply::Routines(bot, result) => {
                let snapshot = Snapshot::Routines(bot);
                if !self.loading(snapshot) {
                    return Ok(false);
                }
                let again = self.answered(snapshot);
                if self.settings_of(bot).is_none() {
                    // Closed meanwhile; the store has already let them go.
                    return Ok(false);
                }
                match result {
                    Ok(list) => {
                        let routines: Vec<RoutineId> =
                            list.routines.iter().map(|routine| routine.id).collect();
                        if let Some(settings) = self.settings_of(bot) {
                            settings.routines_error = None;
                            settings.recent_error = None;
                            settings.recent_loading.extend(routines.iter().copied());
                        }
                        let applied = self.store.load_routines(bot, list);
                        self.load_all(applied.reload);
                        for routine in routines {
                            self.send(ApiRequest::LoadRecentRuns(bot, routine));
                        }
                    }
                    Err(error) if ended(&error) => return Ok(self.settings_ended()),
                    Err(_) if again => {}
                    Err(error) => {
                        if let Some(settings) = self.settings_of(bot) {
                            settings.routines_error = Some(error.message);
                        }
                        self.store.load_failed(snapshot);
                    }
                }
                if again {
                    self.load(snapshot);
                }
                Ok(true)
            }
            ApiReply::Notes(bot, result) => {
                let Some(settings) = self.settings_of(bot).filter(|s| s.notes_loading) else {
                    return Ok(false);
                };
                settings.notes_loading = false;
                match result {
                    Ok(notes) => {
                        settings.notes = Some(notes);
                        settings.notes_error = None;
                    }
                    Err(error) if ended(&error) => return Ok(self.settings_ended()),
                    Err(error) => settings.notes_error = Some(error.message),
                }
                Ok(true)
            }
            ApiReply::NotesCleared(bot, result) => {
                let Some(settings) = self.settings_of(bot).filter(|s| s.clear == Clear::Clearing)
                else {
                    return Ok(false);
                };
                settings.clear = Clear::Idle;
                match result {
                    Ok(()) => {
                        settings.notes = Some(BotNotes {
                            text: String::new(),
                            updated_at: None,
                        });
                        settings.notes_error = None;
                    }
                    Err(error) if ended(&error) => return Ok(self.settings_ended()),
                    Err(error) => settings.notes_error = Some(error.message),
                }
                Ok(true)
            }
            ApiReply::ToolCalls(bot, before, result) => {
                let Some(settings) = self
                    .settings_of(bot)
                    .filter(|s| s.calls_loading == Some(before))
                else {
                    return Ok(false);
                };
                settings.calls_loading = None;
                match result {
                    Ok(page) => {
                        settings.calls_error = None;
                        if before.is_none() {
                            settings.calls = page.calls;
                        } else {
                            let last = settings.calls.last().map(|call| call.id);
                            settings.calls.extend(
                                page.calls
                                    .into_iter()
                                    .filter(|call| last.is_none_or(|last| call.id < last)),
                            );
                        }
                        // A first page loaded again replaces the earlier
                        // pages, so the next page follows it.
                        settings.calls_older = page.older;
                        settings.calls_loaded = true;
                    }
                    Err(error) if ended(&error) => return Ok(self.settings_ended()),
                    Err(error) => settings.calls_error = Some(error.message),
                }
                Ok(true)
            }
            ApiReply::Runs(bot, routine, before, result) => {
                let Some(history) = self
                    .settings_of(bot)
                    .and_then(|s| s.history.as_mut())
                    .filter(|h| h.routine == routine && h.loading == Some(before))
                else {
                    return Ok(false);
                };
                history.loading = None;
                match result {
                    Ok(page) => {
                        history.error = None;
                        if before.is_some() || !history.loaded {
                            history.older = page.older;
                        }
                        history.loaded = true;
                        for run in page.runs {
                            history.merge(run);
                        }
                    }
                    Err(error) if ended(&error) => return Ok(self.settings_ended()),
                    Err(error) => history.error = Some(error.message),
                }
                Ok(true)
            }
            ApiReply::RecentRuns(bot, routine, result) => {
                let Some(settings) = self
                    .settings_of(bot)
                    .filter(|s| s.recent_loading.contains(&routine))
                else {
                    return Ok(false);
                };
                settings.recent_loading.remove(&routine);
                match result {
                    Ok(page) => {
                        for run in page
                            .runs
                            .into_iter()
                            .filter(|run| run.routine_id == routine)
                        {
                            merge_run(&mut settings.recent, run);
                        }
                    }
                    Err(error) if ended(&error) => return Ok(self.settings_ended()),
                    Err(error) => settings.recent_error = Some(error.message),
                }
                Ok(true)
            }
            ApiReply::RoutineCreated(bot, result) => {
                let Some(settings) = self.settings_of(bot) else {
                    return Ok(false);
                };
                let Some(form) = settings
                    .form
                    .as_mut()
                    .filter(|form| form.busy && form.editing.is_none())
                else {
                    return Ok(false);
                };
                form.busy = false;
                match result {
                    Ok(routine) => {
                        settings.form = None;
                        let _ = self.store.routine_changed(routine);
                    }
                    Err(error) if ended(&error) => return Ok(self.settings_ended()),
                    // The form stays, to correct.
                    Err(error) => form.error = Some(error.message),
                }
                Ok(true)
            }
            ApiReply::RoutineUpdated(bot, routine, result) => {
                let Some(settings) = self.settings_of(bot) else {
                    return Ok(false);
                };
                if let Some(form) = settings
                    .form
                    .as_mut()
                    .filter(|form| form.busy && form.editing == Some(routine))
                {
                    form.busy = false;
                    match result {
                        Ok(routine) => {
                            settings.form = None;
                            let _ = self.store.routine_changed(routine);
                        }
                        Err(error) if ended(&error) => return Ok(self.settings_ended()),
                        Err(error) => form.error = Some(error.message),
                    }
                } else if settings.busy.remove(&routine) {
                    match result {
                        Ok(routine) => {
                            let _ = self.store.routine_changed(routine);
                        }
                        Err(error) if ended(&error) => return Ok(self.settings_ended()),
                        Err(error) => settings.routine_error = Some(error.message),
                    }
                } else {
                    return Ok(false);
                }
                Ok(true)
            }
            ApiReply::RoutineDeleted(bot, routine, result) => {
                let Some(settings) = self
                    .settings_of(bot)
                    .filter(|s| s.delete == Some((routine, true)))
                else {
                    return Ok(false);
                };
                settings.delete = None;
                match result {
                    Ok(()) => {
                        forget_routine(settings, routine);
                        let _ = self.store.routine_deleted(bot, routine);
                    }
                    Err(error) if ended(&error) => return Ok(self.settings_ended()),
                    Err(error) => settings.routine_error = Some(error.message),
                }
                Ok(true)
            }
            ApiReply::RoutineStarted(bot, routine, result) => {
                let Some(settings) = self.settings_of(bot).filter(|s| s.busy.contains(&routine))
                else {
                    return Ok(false);
                };
                settings.busy.remove(&routine);
                match result {
                    Ok(run) => {
                        if let Some(history) = &mut settings.history {
                            history.merge(run.clone());
                        }
                        merge_run(&mut settings.recent, run);
                    }
                    Err(error) if ended(&error) => return Ok(self.settings_ended()),
                    Err(error) => settings.routine_error = Some(error.message),
                }
                Ok(true)
            }
            reply => Err(Box::new(reply)),
        }
    }

    /// A run's update from the event socket, for the history shown and the
    /// recent runs. Returns whether it changed.
    pub(crate) fn run_updated(&mut self, run: &RoutineRun) -> bool {
        let Some(settings) = self.settings_of(run.bot_id) else {
            return false;
        };
        let history = settings
            .history
            .as_mut()
            .is_some_and(|history| history.merge(run.clone()));
        merge_run(&mut settings.recent, run.clone()) || history
    }

    /// A turn of `bot`'s ended: what it did may have changed its activity
    /// and its notes, which no event reports, so load them again unless
    /// they are loading or the notes are being cleared. Returns whether
    /// anything changed.
    pub(crate) fn turn_ended(&mut self, bot: BotId) -> bool {
        let Some(settings) = self.settings_of(bot) else {
            return false;
        };
        let calls = settings.calls_loading.is_none();
        let notes = !settings.notes_loading && settings.clear == Clear::Idle;
        if calls {
            settings.calls_loading = Some(None);
            self.send(ApiRequest::LoadToolCalls(bot, None));
        }
        if notes && let Some(settings) = self.settings_of(bot) {
            settings.notes_loading = true;
            self.send(ApiRequest::LoadNotes(bot));
        }
        calls || notes
    }

    /// A routine deleted, as the event socket says: its history and any
    /// question about it go too.
    pub(crate) fn routine_gone(&mut self, bot: BotId, routine: RoutineId) -> bool {
        self.settings_of(bot)
            .is_some_and(|settings| forget_routine(settings, routine))
    }

    /// Why a settings action is unavailable, if it is; `None` for an action
    /// that is not the settings'.
    pub(crate) fn settings_availability(&self, action: BokkieAction) -> Option<Reason> {
        let signed_in = match &self.session {
            Session::SignedIn(signed_in) => Some(signed_in),
            _ => None,
        };
        let settings = self.settings();
        let need = || {
            if settings.is_none() {
                because("Open the Bot's settings first")
            } else {
                None
            }
        };
        let form = settings.and_then(|s| s.form.as_ref());
        let editing = |form: Option<&RoutineForm>| match form {
            None => because("No routine is being edited"),
            Some(form) if form.busy => because("Saving the routine"),
            Some(_) => None,
        };
        Some(match action {
            BokkieAction::BotSettings => match self.open() {
                None => because("No conversation is open"),
                Some(_) => None,
            },
            BokkieAction::SaveTimeZone => match signed_in {
                None => because("Not signed in"),
                Some(s) if s.zone_saving => because("Saving the time zone"),
                Some(s) => match s.zone_draft.as_deref().map(str::trim) {
                    Some("") => because("Enter a time zone, such as Europe/London"),
                    None => because("That is your time zone already"),
                    Some(zone) if zone == s.user.time_zone => {
                        because("That is your time zone already")
                    }
                    Some(_) => None,
                },
            },
            action if action_grant(action).is_some() => {
                let grant = action_grant(action)?;
                if let Some(reason) = need() {
                    return Some(Some(reason));
                }
                let bot = match self.can_update() {
                    Ok(bot) => bot,
                    Err(reason) => return Some(because(reason)),
                };
                if bot.grants.contains(&grant) {
                    // Any grant can be taken away.
                    None
                } else if signed_in.is_some_and(|s| s.user.role != Role::Admin)
                    && !member_may_grant(grant)
                {
                    Some(format!("Only an admin can grant {}", grant.name()).into())
                } else {
                    None
                }
            }
            BokkieAction::ClearNotes => match settings {
                None => need(),
                Some(s) if s.clear == Clear::Confirming => because("Clear the notes or keep them"),
                Some(s) if s.clear == Clear::Clearing => because("Clearing the notes"),
                Some(s) => match &s.notes {
                    None => because("Loading the notes"),
                    Some(notes) if notes.text.is_empty() => because("There are no notes"),
                    Some(_) => None,
                },
            },
            BokkieAction::ConfirmClearNotes | BokkieAction::KeepNotes => match settings {
                None => need(),
                Some(s) if s.clear == Clear::Confirming => None,
                Some(s) if s.clear == Clear::Clearing => because("Clearing the notes"),
                Some(_) => because("Choose Clear notes first"),
            },
            BokkieAction::OlderToolCalls => match settings {
                None => need(),
                Some(s) if s.calls_loading.is_some() => because("Loading tool calls"),
                Some(s) if s.calls_older.is_none() => because("No earlier tool calls"),
                Some(_) => None,
            },
            BokkieAction::NewRoutine => match settings {
                None => need(),
                Some(_) if form.is_some() => because("Save or close the routine first"),
                Some(_)
                    if self
                        .open_bot()
                        .and_then(|b| self.store().routines(b))
                        .is_none() =>
                {
                    because("Loading the routines")
                }
                Some(_) => None,
            },
            BokkieAction::EditRoutine => match settings {
                None => need(),
                Some(_) if form.is_some() => because("Save or close the routine first"),
                Some(_) => None,
            },
            BokkieAction::SaveRoutine => match (editing(form), form) {
                (Some(reason), _) => Some(reason),
                (None, form) => form.and_then(RoutineForm::problem).map(Cow::Owned),
            },
            BokkieAction::CancelRoutine | BokkieAction::RoutinePush => editing(form),
            action if action_repeat(action).is_some() => editing(form),
            action if action_day(action).is_some() => match editing(form) {
                Some(reason) => Some(reason),
                None if form.is_some_and(|form| form.repeat != Repeat::Days) => {
                    because("Choose On chosen days first")
                }
                None => None,
            },
            BokkieAction::PauseRoutine
            | BokkieAction::ResumeRoutine
            | BokkieAction::RunRoutine
            | BokkieAction::RoutineHistory
            | BokkieAction::ShowRun
            | BokkieAction::ShowRecentRun => need(),
            BokkieAction::DeleteRoutine => match settings {
                None => need(),
                Some(s) if s.delete.is_some_and(|(_, busy)| busy) => because("Deleting a routine"),
                Some(s) if s.delete.is_some() => because("Delete the routine or keep it first"),
                Some(_) => None,
            },
            BokkieAction::ConfirmDeleteRoutine | BokkieAction::KeepRoutine => match settings {
                None => need(),
                Some(s) => match s.delete {
                    None => because("Choose a routine to delete first"),
                    Some((_, true)) => because("Deleting the routine"),
                    Some(_) => None,
                },
            },
            BokkieAction::OlderRuns => match settings.map(|s| s.history.as_ref()) {
                None => need(),
                Some(None) => because("Open a routine's history first"),
                Some(Some(h)) if h.loading.is_some() => because("Loading runs"),
                Some(Some(h)) if h.older.is_none() => because("No earlier runs"),
                Some(Some(_)) => None,
            },
            _ => return None,
        })
    }

    /// Whether a settings row action's row is listed: a routine of the
    /// open Bot's, or a run shown, in a history or the recent runs, with
    /// its messages. `None` for an action that is not one of them.
    pub(crate) fn settings_lists(&self, action: BokkieAction, id: i64) -> Option<bool> {
        let routines = || {
            self.settings()
                .and_then(|_| self.store().routines(self.open_bot()?))
        };
        Some(match action {
            BokkieAction::EditRoutine
            | BokkieAction::PauseRoutine
            | BokkieAction::ResumeRoutine
            | BokkieAction::RunRoutine
            | BokkieAction::DeleteRoutine
            | BokkieAction::RoutineHistory => {
                routines().is_some_and(|routines| routines.routine(RoutineId(id)).is_some())
            }
            BokkieAction::ShowRun => self
                .settings()
                .and_then(|s| s.history.as_ref()?.runs.get(&RoutineRunId(id)))
                .is_some_and(|run| !run.messages.is_empty()),
            BokkieAction::ShowRecentRun => self
                .settings()
                .and_then(|s| s.recent_runs().find(|run| run.id == RoutineRunId(id)))
                .is_some_and(|run| !run.messages.is_empty()),
            _ => return None,
        })
    }

    /// Why one routine's control is unavailable, if it is.
    pub(crate) fn routine_availability(&self, action: BokkieAction, id: i64) -> Reason {
        let settings = self.settings()?;
        let routine = RoutineId(id);
        let saving = settings
            .form
            .as_ref()
            .is_some_and(|form| form.busy && form.editing == Some(routine));
        let paused = self
            .open_bot()
            .and_then(|bot| self.store().routines(bot)?.routine(routine))
            .map(|routine| routine.paused);
        match action {
            BokkieAction::PauseRoutine
            | BokkieAction::ResumeRoutine
            | BokkieAction::RunRoutine
            | BokkieAction::DeleteRoutine
                if settings
                    .delete
                    .is_some_and(|(id, busy)| id == routine && busy) =>
            {
                because("Deleting the routine")
            }
            BokkieAction::PauseRoutine
            | BokkieAction::ResumeRoutine
            | BokkieAction::RunRoutine
            | BokkieAction::EditRoutine
                if settings.busy.contains(&routine) || saving =>
            {
                because("Saving the routine")
            }
            BokkieAction::PauseRoutine if paused == Some(true) => {
                because("The routine is paused already")
            }
            BokkieAction::ResumeRoutine if paused == Some(false) => {
                because("The routine is not paused")
            }
            _ => None,
        }
    }

    /// Perform a settings action without an argument; `None` for an action
    /// that is not the settings'.
    pub(crate) fn settings_perform(&mut self, action: BokkieAction) -> Option<bool> {
        // The conversation header's toggle shows or hides the settings.
        if action == BokkieAction::BotSettings {
            return Some(self.close_settings() || self.open_settings());
        }
        if action == BokkieAction::SaveTimeZone {
            let Session::SignedIn(signed_in) = &mut self.session else {
                return Some(false);
            };
            let zone = signed_in.zone_draft.as_deref()?.trim().to_owned();
            signed_in.zone_saving = true;
            signed_in.zone_error = None;
            self.send(ApiRequest::SetTimeZone(SetTimeZoneRequest {
                time_zone: zone,
            }));
            return Some(true);
        }
        if let Some(grant) = action_grant(action) {
            return Some(self.toggle_grant(grant));
        }
        let bot = self.open_bot()?;
        let settings = self.settings_mut()?;
        if let Some(repeat) = action_repeat(action) {
            settings.form.as_mut()?.repeat = repeat;
            return Some(true);
        }
        if let Some(day) = action_day(action) {
            let days = &mut settings.form.as_mut()?.days;
            if !days.remove(&day) {
                days.insert(day);
            }
            return Some(true);
        }
        match action {
            BokkieAction::ClearNotes => settings.clear = Clear::Confirming,
            BokkieAction::KeepNotes => settings.clear = Clear::Idle,
            BokkieAction::ConfirmClearNotes => {
                settings.clear = Clear::Clearing;
                settings.notes_error = None;
                self.send(ApiRequest::ClearNotes(bot));
            }
            BokkieAction::OlderToolCalls => {
                let before = settings.calls_older?;
                settings.calls_loading = Some(Some(before));
                settings.calls_error = None;
                self.send(ApiRequest::LoadToolCalls(bot, Some(before)));
            }
            BokkieAction::NewRoutine => settings.form = Some(RoutineForm::new()),
            BokkieAction::RoutinePush => {
                let form = settings.form.as_mut()?;
                form.push = match form.push {
                    RoutinePush::EveryRun => RoutinePush::NotifyOnly,
                    RoutinePush::NotifyOnly => RoutinePush::EveryRun,
                };
            }
            BokkieAction::CancelRoutine => settings.form = None,
            BokkieAction::SaveRoutine => return Some(self.save_routine(bot)),
            BokkieAction::KeepRoutine => settings.delete = None,
            BokkieAction::ConfirmDeleteRoutine => {
                let (routine, _) = settings.delete?;
                settings.delete = Some((routine, true));
                settings.routine_error = None;
                self.send(ApiRequest::DeleteRoutine(bot, routine));
            }
            BokkieAction::OlderRuns => {
                let history = settings.history.as_mut()?;
                let before = history.older?;
                history.loading = Some(Some(before));
                history.error = None;
                let routine = history.routine;
                self.send(ApiRequest::LoadRuns(bot, routine, Some(before)));
            }
            _ => return None,
        }
        Some(true)
    }

    /// Perform a settings action on one row; `None` for an action that is
    /// not one of them.
    pub(crate) fn settings_perform_with(
        &mut self,
        action: BokkieAction,
        id: i64,
    ) -> Option<Result<bool, String>> {
        if !matches!(
            action,
            BokkieAction::EditRoutine
                | BokkieAction::PauseRoutine
                | BokkieAction::ResumeRoutine
                | BokkieAction::RunRoutine
                | BokkieAction::DeleteRoutine
                | BokkieAction::RoutineHistory
                | BokkieAction::ShowRun
                | BokkieAction::ShowRecentRun
        ) {
            return None;
        }
        let Some(bot) = self.open_bot() else {
            return Some(Err("No conversation is open".into()));
        };
        if matches!(action, BokkieAction::ShowRun | BokkieAction::ShowRecentRun) {
            return Some(self.show_run(RoutineRunId(id)));
        }
        let routine_id = RoutineId(id);
        let Some(routine) = self
            .store()
            .routines(bot)
            .and_then(|routines| routines.routine(routine_id))
            .cloned()
        else {
            return Some(Err(format!("There is no routine {id}")));
        };
        let Some(settings) = self.settings_mut() else {
            return Some(Err("Open the Bot's settings first".into()));
        };
        match action {
            BokkieAction::EditRoutine => settings.form = Some(RoutineForm::of(&routine)),
            BokkieAction::PauseRoutine | BokkieAction::ResumeRoutine => {
                settings.busy.insert(routine_id);
                settings.routine_error = None;
                let changes = UpdateRoutineRequest {
                    paused: Some(action == BokkieAction::PauseRoutine),
                    ..Default::default()
                };
                self.send(ApiRequest::UpdateRoutine(bot, routine_id, changes));
            }
            BokkieAction::RunRoutine => {
                settings.busy.insert(routine_id);
                settings.routine_error = None;
                // The run's progress shows in its history.
                if settings.history.as_ref().map(|h| h.routine) != Some(routine_id) {
                    settings.history = Some(History::new(routine_id));
                    self.send(ApiRequest::LoadRuns(bot, routine_id, None));
                }
                self.send(ApiRequest::RunRoutine(bot, routine_id));
            }
            BokkieAction::DeleteRoutine => {
                settings.delete = Some((routine_id, false));
                settings.routine_error = None;
            }
            BokkieAction::RoutineHistory => {
                if settings.history.as_ref().map(|h| h.routine) == Some(routine_id) {
                    settings.history = None;
                } else {
                    settings.history = Some(History::new(routine_id));
                    self.send(ApiRequest::LoadRuns(bot, routine_id, None));
                }
            }
            _ => return None,
        }
        Some(Ok(true))
    }

    /// Turn a grant of the open Bot's on or off, as the switch shows it. The
    /// rest of the new set comes from the Bot as the gateway has it now,
    /// not from the store's copy, which may not yet show a change made on
    /// another device.
    fn toggle_grant(&mut self, grant: Grant) -> bool {
        let Some(bot) = self.open_bot().and_then(|bot| self.store().bot(bot)) else {
            return false;
        };
        let on = !bot.grants.contains(&grant);
        let Session::SignedIn(signed_in) = &mut self.session else {
            return false;
        };
        let Some(open) = signed_in.open.as_mut() else {
            return false;
        };
        open.updating = true;
        open.grant = Some((grant, on));
        open.update_error = None;
        let bot = open.bot;
        self.send(ApiRequest::LoadBot(bot));
        true
    }

    /// The Bot as the gateway has it now, for a grant change: send the
    /// change.
    fn grant_from(&mut self, id: BotId, result: Result<Bot, CommandError>) -> bool {
        let Session::SignedIn(signed_in) = &mut self.session else {
            return false;
        };
        let Some(open) = signed_in.open.as_mut().filter(|open| open.bot == id) else {
            return false;
        };
        let Some((grant, on)) = open.grant.take() else {
            return false;
        };
        match result {
            Ok(bot) => {
                let mut grants: BTreeSet<Grant> = bot.grants.iter().copied().collect();
                if on {
                    grants.insert(grant);
                } else {
                    grants.remove(&grant);
                }
                let changes = UpdateBotRequest {
                    grants: Some(grants.into_iter().collect()),
                    ..Default::default()
                };
                self.send(ApiRequest::UpdateBot(id, changes));
            }
            Err(error) if error.is_unauthenticated() => return self.settings_ended(),
            Err(error) => {
                open.updating = false;
                open.update_error = Some(error.message);
            }
        }
        true
    }

    /// Save the routine form: a new routine, or only what changed of one.
    fn save_routine(&mut self, bot: BotId) -> bool {
        let held = self
            .settings()
            .and_then(|s| s.form.as_ref())
            .and_then(|form| form.editing)
            .and_then(|id| self.store().routines(bot)?.routine(id).cloned());
        let Some(form) = self.settings_mut().and_then(|s| s.form.as_mut()) else {
            return false;
        };
        let Ok(schedule) = form.schedule() else {
            return false;
        };
        let name = form.name.trim().to_owned();
        let prompt = form.prompt.trim().to_owned();
        let request = match (form.editing, held) {
            (None, _) => ApiRequest::CreateRoutine(
                bot,
                CreateRoutineRequest {
                    name,
                    prompt,
                    schedule,
                    push: form.push,
                    paused: false,
                },
            ),
            (Some(id), Some(held)) => {
                let changes = UpdateRoutineRequest {
                    name: (name != held.name).then_some(name),
                    prompt: (prompt != held.prompt).then_some(prompt),
                    schedule: (schedule != held.schedule).then_some(schedule),
                    push: (form.push != held.push).then_some(form.push),
                    paused: None,
                };
                if changes == UpdateRoutineRequest::default() {
                    if let Some(settings) = self.settings_mut() {
                        settings.form = None;
                    }
                    return true;
                }
                ApiRequest::UpdateRoutine(bot, id, changes)
            }
            (Some(_), None) => {
                form.error = Some("The routine has been deleted".into());
                return true;
            }
        };
        form.busy = true;
        form.error = None;
        self.send(request);
        true
    }

    /// Show a run's marker in the conversation: close the panel unless the
    /// conversation stands beside it, then bring the marker into view,
    /// loading earlier messages until the conversation holds it.
    fn show_run(&mut self, run: RoutineRunId) -> Result<bool, String> {
        let Some(marker) = self
            .settings()
            .and_then(|s| s.shown_run(run))
            .and_then(|run| run.messages.first().copied())
        else {
            return Err(format!("Run {run} has no messages to show"));
        };
        if !self
            .panes()
            .contains(&stobie_ui_egui::NavigationPane::Content)
        {
            self.close_settings();
        }
        self.go_to(marker);
        Ok(true)
    }

    /// Facts about the open Bot's settings and the user's time zone.
    pub(crate) fn observe_settings(&self, observation: &mut stobie_ui_egui::Observation<'_>) {
        use crate::app::bounded;
        let signed_in = match &self.session {
            Session::SignedIn(signed_in) => Some(signed_in),
            _ => None,
        };
        let bot = self.open_bot().and_then(|bot| self.store().bot(bot));
        observation.fact(
            "grants",
            bot.map_or(String::new(), |bot| {
                let names: Vec<&str> = bot.grants.iter().map(|grant| grant.name()).collect();
                names.join(",")
            }),
        );
        observation.fact(
            "time_zone",
            signed_in.map_or("", |s| s.user.time_zone.as_str()),
        );
        observation.fact(
            "time_zone_error",
            bounded(
                signed_in
                    .and_then(|s| s.zone_error.as_deref())
                    .unwrap_or(""),
            ),
        );
        let settings = self.settings();
        observation.fact(
            "notes",
            match settings {
                None => "",
                Some(s) if s.notes.is_some() => "loaded",
                Some(s) if s.notes_loading => "loading",
                Some(s) if s.notes_error.is_some() => "failed",
                Some(_) => "",
            },
        );
        observation.fact(
            "notes_chars",
            settings
                .and_then(|s| s.notes.as_ref())
                .map_or(0, |notes| notes.text.chars().count()),
        );
        observation.fact(
            "notes_clear",
            match settings.map(|s| s.clear) {
                None | Some(Clear::Idle) => "",
                Some(Clear::Confirming) => "confirming",
                Some(Clear::Clearing) => "clearing",
            },
        );
        observation.fact("tool_calls", settings.map_or(0, |s| s.calls.len()));
        observation.fact(
            "tool_calls_state",
            match settings {
                None => "",
                Some(s) if s.calls_loading.is_some() => "loading",
                Some(s) if s.calls_error.is_some() => "failed",
                Some(s) if s.calls_older.is_some() => "more",
                Some(s) if s.calls_loaded => "loaded",
                Some(_) => "",
            },
        );
        let routines = self
            .open_bot()
            .filter(|_| settings.is_some())
            .and_then(|bot| self.store().routines(bot));
        observation.fact(
            "routines",
            match (settings, self.open_bot()) {
                (Some(s), Some(bot)) => s.routines_state(self, bot),
                _ => "",
            },
        );
        observation.fact("routine_count", routines.map_or(0, |r| r.len()));
        observation.fact(
            "routine_form",
            match settings.and_then(|s| s.form.as_ref()) {
                None => "",
                Some(form) if form.busy => "saving",
                Some(form) if form.editing.is_some() => "editing",
                Some(_) => "new",
            },
        );
        observation.fact(
            "routine_error",
            bounded(
                settings
                    .and_then(|s| {
                        s.form
                            .as_ref()
                            .and_then(|form| form.error.as_deref())
                            .or(s.routine_error.as_deref())
                            .or(s.routines_error.as_deref())
                    })
                    .unwrap_or(""),
            ),
        );
        observation.fact(
            "routine_delete",
            settings
                .and_then(|s| s.delete)
                .map_or(0, |(routine, _)| routine.0),
        );
        let history = settings.and_then(|s| s.history.as_ref());
        observation.fact("history", history.map_or(0, |h| h.routine.0));
        observation.fact("runs", history.map_or(0, |h| h.runs.len()));
        let newest = history.and_then(|h| h.newest_first().next());
        observation.fact("run", newest.map_or(0, |run| run.id.0));
        observation.fact(
            "run_state",
            newest.map_or("", |run| run_state_name(run.state)),
        );
        observation.fact("run_messages", newest.map_or(0, |run| run.messages.len()));
        observation.fact(
            "recent_runs",
            settings.map_or(0, |s| s.recent_runs().count()),
        );
        observation.fact(
            "recent_runs_state",
            match settings {
                None => "",
                Some(s) if !s.recent_loading.is_empty() || routines.is_none() => "loading",
                Some(s) if s.recent_error.is_some() => "failed",
                Some(_) => "loaded",
            },
        );
        let recent = settings.and_then(|s| s.recent_runs().next());
        observation.fact("recent_run", recent.map_or(0, |run| run.id.0));
        observation.fact(
            "recent_run_state",
            recent.map_or("", |run| run_state_name(run.state)),
        );
    }
}

impl App {
    /// End the session from inside a reply handler, reporting a change.
    fn settings_ended(&mut self) -> bool {
        self.session_ended();
        true
    }
}

/// Drop what the settings hold of a deleted routine. Returns whether they
/// held anything.
fn forget_routine(settings: &mut Settings, routine: RoutineId) -> bool {
    let mut changed = false;
    if settings.history.as_ref().map(|h| h.routine) == Some(routine) {
        settings.history = None;
        changed = true;
    }
    if settings.delete.is_some_and(|(id, _)| id == routine) {
        settings.delete = None;
        changed = true;
    }
    if settings.busy.remove(&routine) {
        changed = true;
    }
    if settings.recent_loading.remove(&routine) {
        changed = true;
    }
    let before = settings.recent.len();
    settings.recent.retain(|_, run| run.routine_id != routine);
    changed || settings.recent.len() != before
}
