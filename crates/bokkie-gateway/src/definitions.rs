//! Bot definitions: a TOML file an operator applies to make or change one
//! Bot of a named account, with its profile, grants and routines, and the
//! operator's "run now" for one of its routines.
//!
//! `bokkie-gateway bot apply` and `bokkie-gateway bot run` send these to
//! the serving gateway over its control socket ([`crate::control`]), which
//! runs them here, in the gateway that owns the account's event log, its
//! Bots' send locks and its scheduler.
//!
//! The command acts as the named account itself: it may do what the
//! account may do with its own requests (`POST` and `PATCH /api/v1/bots`,
//! and the routine routes), under the same checks. So a member's account
//! is held to its permissions and may hold only the grants a member may
//! add. A Bot is matched by its name in the account, whatever its case:
//! it is made when absent and changed when present. What the file gives is
//! applied, and what it leaves out is left as it is: a profile field, the
//! grants, and every routine it does not name, unless `remove_routines`
//! names it. A whole definition applies in one transaction, so a refusal
//! anywhere changes nothing, and a dry run checks everything and then
//! rolls it back.

use std::collections::BTreeSet;

use bokkie_protocol::api::{
    CreateRoutineRequest, ErrorCode, Grant, RoutinePush, Schedule, UpdateRoutineRequest, Weekday,
};
use bokkie_protocol::events::Event;
use serde::Deserialize;

use crate::AppState;
use crate::accounts::{self, Account};
use crate::bots::store::{self as bots, BotChanges, BotRow};
use crate::error::{Failure, Result};
use crate::grants::{self, GrantSet};
use crate::routines::{self, schedule, store as routine_store};

/// A Bot definition, as its file gives it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    /// The Bot's name, which also finds it in the account.
    pub name: String,
    pub label: Option<String>,
    pub description: Option<String>,
    pub avatar: Option<String>,
    /// The Bot's whole set of grants, by name, such as `web.fetch`.
    #[serde(default, deserialize_with = "grant_names")]
    pub grants: Option<Vec<Grant>>,
    #[serde(default)]
    pub routines: Vec<RoutineDefinition>,
    /// Routines to delete, by name.
    #[serde(default)]
    pub remove_routines: Vec<String>,
}

/// One routine of a definition, found on the Bot by its name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutineDefinition {
    pub name: String,
    pub prompt: String,
    pub schedule: Schedule,
    pub push: Option<RoutinePush>,
    pub paused: Option<bool>,
}

fn grant_names<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Vec<Grant>>, D::Error> {
    let names = Vec::<String>::deserialize(deserializer)?;
    names
        .iter()
        .map(|name| {
            Grant::from_name(name).ok_or_else(|| {
                let catalogue: Vec<&str> = Grant::ALL.iter().map(|g| g.name()).collect();
                serde::de::Error::custom(format!(
                    "unknown grant {name:?}; the grants are {}",
                    catalogue.join(", ")
                ))
            })
        })
        .collect::<std::result::Result<Vec<_>, _>>()
        .map(Some)
}

impl Definition {
    /// Reads a definition from its TOML text and checks what can be
    /// checked without the account: the shape, the grant names, and that
    /// no routine is named twice.
    pub fn parse(text: &str) -> std::result::Result<Self, String> {
        let definition: Self =
            toml::from_str(text).map_err(|e| format!("the definition is not valid: {e}"))?;
        let mut names = BTreeSet::new();
        for routine in &definition.routines {
            if !names.insert(fold(&routine.name)) {
                return Err(format!(
                    "the definition names the routine {:?} twice",
                    routine.name.trim()
                ));
            }
        }
        for name in &definition.remove_routines {
            if names.contains(&fold(name)) {
                return Err(format!(
                    "the definition both defines and removes the routine {:?}",
                    name.trim()
                ));
            }
        }
        Ok(definition)
    }
}

/// A name as the database compares names: trimmed, ASCII letters in either
/// case alike (SQLite's `NOCASE`).
fn fold(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

/// What applying a definition or running a routine did, one line each.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub lines: Vec<String>,
}

impl Report {
    fn say(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }
}

/// The first line of a dry run's report.
pub const DRY_RUN: &str = "Dry run: nothing was changed. Applying the definition would:";
/// The first line of an applied definition's report.
pub const APPLIED: &str = "Applied the definition:";

/// Applies `definition` to the account named `username`, or with `dry_run`
/// checks it and changes nothing. Returns what it changed, or would.
pub async fn apply(
    state: &AppState,
    username: &str,
    definition: &Definition,
    dry_run: bool,
) -> Result<Report> {
    let account = named_account(&state.db, username).await?;
    let user = account.user.id;
    let name = bots::check_name(&definition.name).map_err(|f| about("The Bot", f))?;
    // A grant change, and pausing a routine, wait for any send to the Bot
    // (as the API's do), and its lock comes before the event batch.
    let found = bots::bot_by_name(&state.db, user, &name).await?;
    let _sends = match &found {
        Some(bot) => Some(state.conversations.lock_bot(user, bot.id).await),
        None => None,
    };
    let now = state.clock.now();
    let mut batch = state.events.begin(user).await?;
    let account = accounts::account(&mut *batch, user)
        .await?
        .ok_or_else(|| no_account(username))?;
    let existing = bots::bot_by_name(&mut *batch, user, &name).await?;
    if existing.as_ref().map(|bot| bot.id) != found.as_ref().map(|bot| bot.id) {
        return Err(Failure::conflict(
            "The Bot changed while the definition was being applied; apply it again",
        ));
    }
    let who = &account.user.username;
    let mut report = Report::default();
    let mut changed = false;

    // The Bot and its profile.
    let (bot, bot_changed) = match existing {
        None => {
            if !account.user.permissions.may_create_bots {
                return Err(Failure::forbidden(format!(
                    "{who} may not create Bots; an admin makes that account's Bots"
                )));
            }
            let bot = bots::create_bot(
                &mut *batch,
                now,
                user,
                None,
                &name,
                definition.label.as_deref().unwrap_or(""),
                definition.description.as_deref().unwrap_or(""),
                definition.avatar.as_deref(),
            )
            .await
            .map_err(|f| about("The Bot", f))?;
            report.say(format!("create the Bot {name:?} in {who}'s account"));
            (bot, true)
        }
        Some(bot) => {
            let changes = profile_changes(&bot, &name, definition)?;
            let fields = changes.fields();
            if fields.is_empty() {
                (bot, false)
            } else {
                if !account.user.permissions.may_edit_bot_profiles {
                    return Err(Failure::forbidden(format!(
                        "{who} may not change its Bots' profiles"
                    )));
                }
                let told = bot.thread_id.is_some()
                    && (changes.name.is_some()
                        || changes.label.is_some()
                        || changes.description.is_some());
                let bot = bots::update_bot(&mut *batch, now, user, bot.id, changes)
                    .await
                    .map_err(|f| about("The Bot", f))?;
                let mut line = format!("change the Bot {name:?}'s {}", list(&fields));
                if told {
                    line.push_str("; its next turn is told its new profile");
                }
                report.say(line);
                (bot, true)
            }
        }
    };

    // Its grants, under the same rules as the account's own change.
    let (bot, grants_changed) = match &definition.grants {
        None => (bot, false),
        Some(requested) => {
            let grants = grants::check_change(account.user.role, &bot.grants, requested)
                .map_err(|f| about("The grants", f))?;
            if grants == bot.grants {
                (bot, false)
            } else {
                let before = bot.grants.clone();
                let had_thread = bot.thread_id.is_some();
                let bot = bots::set_grants(&mut *batch, now, user, bot.id, &grants).await?;
                let mut line = if found.is_none() {
                    format!("grant it {}", grant_list(&grants))
                } else {
                    format!(
                        "set the grants to {} (from {})",
                        grant_list(&grants),
                        grant_list(&before)
                    )
                };
                if had_thread {
                    line.push_str("; its next turn starts a fresh thread");
                }
                report.say(line);
                (bot, true)
            }
        }
    };
    if bot_changed || grants_changed {
        changed = true;
        let api = crate::routes::bots::to_api(state, &mut *batch, &bot).await?;
        let event = if found.is_none() {
            Event::BotCreated { bot: api }
        } else {
            Event::BotUpdated { bot: api }
        };
        batch.emit(event).await?;
    }

    // Its routines, each found by name.
    let zone = schedule::stored_zone(&account.user.time_zone);
    let mut existing = routine_store::list_for_bot(&mut *batch, user, bot.id).await?;
    for wanted in &definition.routines {
        let routine_name = routine_store::check_name(&wanted.name)
            .map_err(|f| about(&format!("The routine {:?}", wanted.name.trim()), f))?;
        let label = format!("the routine {routine_name:?}");
        let about_it = |f: Failure| about(&format!("The routine {routine_name:?}"), f);
        let position = existing
            .iter()
            .position(|r| r.name.eq_ignore_ascii_case(&routine_name));
        match position {
            None => {
                let routine = state
                    .routines
                    .create_in(
                        &mut batch,
                        now,
                        user,
                        bot.id,
                        &CreateRoutineRequest {
                            name: routine_name.clone(),
                            prompt: wanted.prompt.clone(),
                            schedule: wanted.schedule.clone(),
                            push: wanted.push.unwrap_or_default(),
                            paused: wanted.paused.unwrap_or(false),
                        },
                    )
                    .await
                    .map_err(|f| routine_limit(f, who))
                    .map_err(about_it)?;
                report.say(format!(
                    "create {label}: {}",
                    when(&routine.schedule, &account, &zone, routine.next_run_at)
                ));
                changed = true;
            }
            Some(index) => {
                let current = existing.remove(index);
                let prompt = routine_store::check_prompt(&wanted.prompt).map_err(about_it)?;
                let request = UpdateRoutineRequest {
                    name: (routine_name != current.name).then(|| routine_name.clone()),
                    prompt: (prompt != current.prompt).then_some(prompt),
                    schedule: (wanted.schedule != current.schedule)
                        .then(|| wanted.schedule.clone()),
                    push: wanted.push.filter(|push| *push != current.push),
                    paused: wanted.paused.filter(|paused| *paused != current.paused),
                };
                if request == UpdateRoutineRequest::default() {
                    continue;
                }
                let mut fields = Vec::new();
                for (field, set) in [
                    ("name", request.name.is_some()),
                    ("prompt", request.prompt.is_some()),
                    ("schedule", request.schedule.is_some()),
                    ("push", request.push.is_some()),
                ] {
                    if set {
                        fields.push(field);
                    }
                }
                let pausing = request.paused;
                let rescheduled = request.schedule.is_some() || request.paused == Some(false);
                let routine = state
                    .routines
                    .update_in(&mut batch, now, user, bot.id, current.id, request)
                    .await
                    .map_err(about_it)?;
                let mut line = format!("change {label}");
                if !fields.is_empty() {
                    line.push_str(&format!("'s {}", list(&fields)));
                }
                match pausing {
                    Some(true) => line.push_str(if fields.is_empty() {
                        ": pause it"
                    } else {
                        ", and pause it"
                    }),
                    Some(false) => line.push_str(if fields.is_empty() {
                        ": resume it"
                    } else {
                        ", and resume it"
                    }),
                    None => {}
                }
                if rescheduled {
                    line.push_str(&format!(
                        "; {}",
                        when(&routine.schedule, &account, &zone, routine.next_run_at)
                    ));
                }
                report.say(line);
                changed = true;
            }
        }
    }
    for name in &definition.remove_routines {
        let name = name.trim();
        let Some(index) = existing
            .iter()
            .position(|r| r.name.eq_ignore_ascii_case(name))
        else {
            report.say(format!(
                "leave the routine {name:?} out: the Bot has no routine of that name"
            ));
            continue;
        };
        let routine = existing.remove(index);
        routines::delete_in(&mut batch, user, bot.id, routine.id).await?;
        report.say(format!(
            "remove the routine {:?} and its run history",
            routine.name
        ));
        changed = true;
    }
    for routine in &existing {
        report.say(format!(
            "keep the routine {:?}, which the definition does not name",
            routine.name
        ));
    }

    if !changed {
        report.say(format!(
            "change nothing: the Bot {name:?} in {who}'s account already matches"
        ));
    }
    if dry_run {
        // Dropping the batch rolls back everything, events included.
        drop(batch);
        report.lines.insert(0, DRY_RUN.to_owned());
        return Ok(report);
    }
    batch.commit().await?;
    tracing::info!(user = %user, bot = %bot.id, changed, "applied a Bot definition");
    report.lines.insert(0, APPLIED.to_owned());
    Ok(report)
}

/// "Run now" for the routine named `routine` of the Bot named `bot` in the
/// account named `username`: a manual run, queued and started exactly as
/// the client's "Run now" does. Returns where the run then is.
pub async fn run(state: &AppState, username: &str, bot: &str, routine: &str) -> Result<Report> {
    let account = named_account(&state.db, username).await?;
    let user = account.user.id;
    let who = &account.user.username;
    let found = bots::bot_by_name(&state.db, user, bot.trim())
        .await?
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::NotFound,
                format!("{who} has no Bot named {:?}", bot.trim()),
            )
        })?;
    let routine_row = routine_store::list_for_bot(&state.db, user, found.id)
        .await?
        .into_iter()
        .find(|r| r.name.eq_ignore_ascii_case(routine.trim()))
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::NotFound,
                format!(
                    "The Bot {:?} has no routine named {:?}",
                    found.name,
                    routine.trim()
                ),
            )
        })?;
    // Starting the run's turn goes on in its own task, as the API's "Run
    // now" does, so it finishes whole however this request ends.
    let routines = state.routines.clone();
    let (bot_id, routine_id) = (found.id, routine_row.id);
    let run = tokio::spawn(async move { routines.run_now(user, bot_id, routine_id).await })
        .await
        .map_err(Failure::internal)??;
    let state_name = routine_store::state_name(run.state);
    let mut line = format!(
        "run {} of the routine {:?} on {:?}: {state_name}",
        run.id.0, routine_row.name, found.name
    );
    if let Some(reason) = &run.reason {
        line.push_str(&format!(" ({reason})"));
    }
    tracing::info!(user = %user, bot = %bot_id, routine = %routine_id, run = %run.id, "ran a routine for the operator");
    Ok(Report { lines: vec![line] })
}

/// The account `username` names, which must be enabled.
async fn named_account(db: &sqlx::SqlitePool, username: &str) -> Result<Account> {
    let account = accounts::account_by_username(db, username)
        .await?
        .ok_or_else(|| no_account(username))?;
    if account.user.disabled {
        return Err(Failure::invalid(format!(
            "The account {} is disabled",
            account.user.username
        )));
    }
    Ok(account)
}

fn no_account(username: &str) -> Failure {
    Failure::new(
        ErrorCode::NotFound,
        format!(
            "No account is named {:?}; this command creates none",
            username.trim()
        ),
    )
}

/// Names what a refusal was about.
fn about(what: &str, failure: Failure) -> Failure {
    Failure {
        message: format!("{what}: {}", failure.message),
        ..failure
    }
}

/// The routine limit, said of the account rather than "you".
fn routine_limit(failure: Failure, who: &str) -> Failure {
    if failure.code == ErrorCode::Conflict && failure.message.starts_with("You can have at most") {
        Failure {
            message: format!(
                "{who} already has {} routines, the most an account may have",
                routine_store::MAX_ROUTINES
            ),
            ..failure
        }
    } else {
        failure
    }
}

/// The profile fields of `bot` that the definition changes.
fn profile_changes(bot: &BotRow, name: &str, definition: &Definition) -> Result<BotChanges> {
    let differs = |value: &str, current: &str| (value != current).then(|| value.to_owned());
    let label = definition
        .label
        .as_deref()
        .map(bots::check_label)
        .transpose()
        .map_err(|f| about("The Bot", f))?;
    let description = definition
        .description
        .as_deref()
        .map(bots::check_description)
        .transpose()
        .map_err(|f| about("The Bot", f))?;
    let avatar = definition
        .avatar
        .as_deref()
        .map(bots::check_avatar)
        .transpose()
        .map_err(|f| about("The Bot", f))?;
    Ok(BotChanges {
        name: differs(name, &bot.name),
        label: label.and_then(|label| differs(&label, &bot.label)),
        description: description.and_then(|d| differs(&d, &bot.description)),
        avatar: avatar
            .filter(|avatar| bot.avatar.as_deref() != Some(avatar.as_str()))
            .map(Some),
        pinned: None,
        hidden: None,
    })
}

trait Fields {
    fn fields(&self) -> Vec<&'static str>;
}

impl Fields for BotChanges {
    fn fields(&self) -> Vec<&'static str> {
        let mut fields = Vec::new();
        if self.name.is_some() {
            fields.push("name");
        }
        if self.label.is_some() {
            fields.push("label");
        }
        if self.description.is_some() {
            fields.push("description");
        }
        if self.avatar.is_some() {
            fields.push("avatar");
        }
        fields
    }
}

fn list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

fn grant_list(grants: &GrantSet) -> String {
    if grants.is_empty() {
        "none".to_owned()
    } else {
        grants
            .iter()
            .map(|g| g.name())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// A schedule and its next run as the operator reads them, such as
/// `daily at 07:00, next at 07:00 on Saturday 16 January 2027
/// (Australia/Adelaide)`.
fn when(
    schedule: &Schedule,
    account: &Account,
    zone: &jiff::tz::TimeZone,
    next_run_at: Option<bokkie_protocol::Timestamp>,
) -> String {
    let when = match schedule {
        Schedule::Daily { at } => format!("daily at {at}"),
        Schedule::Weekdays { at } => format!("weekdays at {at}"),
        Schedule::Days { days, at } => {
            let days: Vec<&str> = days.iter().map(|d| day_name(*d)).collect();
            format!("on {} at {at}", list(&days))
        }
        Schedule::Hours { every, at } => format!("every {every} hours from {at}"),
    };
    match next_run_at {
        Some(at) => format!("{when}, next at {}", schedule::local_description(zone, at)),
        None => format!("{when} ({}), paused", account.user.time_zone),
    }
}

fn day_name(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "Monday",
        Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday",
        Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday",
        Weekday::Sat => "Saturday",
        Weekday::Sun => "Sunday",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bokkie_protocol::api::TimeOfDay;

    #[test]
    fn a_definition_reads_its_profile_grants_and_routines() {
        let definition = Definition::parse(
            r#"
            name = "Gardener"
            label = "Garden notes"
            description = "Keep track of the garden."
            grants = ["notes", "notify"]
            remove_routines = ["Old reminder"]

            [[routines]]
            name = "Watering"
            prompt = "Remind me to water."
            schedule = { repeat = "days", days = ["mon", "thu"], at = "18:30" }
            push = "notify_only"

            [[routines]]
            name = "Weekly review"
            prompt = "Review the week."
            schedule = { repeat = "hours", every = 6, at = "06:00" }
            paused = true
            "#,
        )
        .unwrap();
        assert_eq!(definition.name, "Gardener");
        assert_eq!(definition.grants, Some(vec![Grant::Notes, Grant::Notify]));
        assert_eq!(definition.routines.len(), 2);
        assert_eq!(
            definition.routines[0].schedule,
            Schedule::Days {
                days: vec![Weekday::Mon, Weekday::Thu],
                at: TimeOfDay::new(18, 30).unwrap()
            }
        );
        assert_eq!(definition.routines[0].push, Some(RoutinePush::NotifyOnly));
        assert_eq!(definition.routines[1].paused, Some(true));
        assert_eq!(definition.remove_routines, ["Old reminder"]);

        // Only the name is required.
        let bare = Definition::parse("name = \"Bare\"").unwrap();
        assert_eq!(bare.grants, None);
        assert!(bare.routines.is_empty());
    }

    #[test]
    fn a_definition_refuses_unknown_fields_and_grants_and_repeated_routines() {
        let unknown = Definition::parse("name = \"A\"\nlabels = \"x\"").unwrap_err();
        assert!(unknown.contains("labels"), "{unknown}");
        let grant = Definition::parse("name = \"A\"\ngrants = [\"web.serch\"]").unwrap_err();
        assert!(
            grant.contains("unknown grant \"web.serch\"")
                && grant.contains("notes, seen, notify, web.search, web.fetch"),
            "{grant}"
        );
        let time = Definition::parse(
            "name = \"A\"\n[[routines]]\nname = \"R\"\nprompt = \"P\"\n\
             schedule = { repeat = \"daily\", at = \"7:00\" }",
        )
        .unwrap_err();
        assert!(time.contains("not a time of day"), "{time}");
        let twice = Definition::parse(
            "name = \"A\"\n\
             [[routines]]\nname = \"Digest\"\nprompt = \"P\"\nschedule = { repeat = \"daily\", at = \"07:00\" }\n\
             [[routines]]\nname = \" digest\"\nprompt = \"Q\"\nschedule = { repeat = \"daily\", at = \"08:00\" }",
        )
        .unwrap_err();
        assert!(twice.contains("twice"), "{twice}");
        let both = Definition::parse(
            "name = \"A\"\nremove_routines = [\"DIGEST\"]\n\
             [[routines]]\nname = \"Digest\"\nprompt = \"P\"\nschedule = { repeat = \"daily\", at = \"07:00\" }",
        )
        .unwrap_err();
        assert!(both.contains("both defines and removes"), "{both}");
        let missing = Definition::parse("label = \"x\"").unwrap_err();
        assert!(missing.contains("name"), "{missing}");
    }

    #[test]
    fn lists_read_as_prose() {
        assert_eq!(list(&["name"]), "name");
        assert_eq!(list(&["name", "label"]), "name and label");
        assert_eq!(list(&["a", "b", "c"]), "a, b and c");
    }
}
