//! Routines and their runs in the database.
//!
//! Every function that serves a user takes that user's id and puts it in
//! the query's `WHERE` clause, as [`crate::bots::store`] does, so a routine
//! or run of another user's reads as absent. Only the scheduler's own
//! queries ([`due`], [`active_runs`]) read every user's, to act for each
//! of them in turn.

use bokkie_protocol::api::{Routine, RoutinePush, RoutineRun, RunState, RunTrigger, Schedule};
use bokkie_protocol::{BotId, MessageId, RoutineId, RoutineRunId, Timestamp, UserId};
use sqlx::Row;
use sqlx::sqlite::{SqliteConnection, SqliteExecutor};

use crate::clock::DAY_MS;
use crate::error::{Failure, Result};

/// How many routines a user may have.
pub const MAX_ROUTINES: i64 = 20;
/// The longest routine name, in characters.
pub const MAX_NAME_CHARS: usize = 60;
/// The longest prompt, in characters.
pub const MAX_PROMPT_CHARS: usize = 8_000;
/// How long runs are kept.
pub const RUN_RETENTION_MS: i64 = 90 * DAY_MS;
/// How many failed runs in a row pause a routine.
pub const FAILURES_TO_PAUSE: u32 = 3;

/// A routine row.
#[derive(Debug, Clone)]
pub struct RoutineRow {
    pub id: RoutineId,
    pub user: UserId,
    pub bot: BotId,
    pub name: String,
    pub prompt: String,
    pub schedule: Schedule,
    pub push: RoutinePush,
    pub paused: bool,
    pub failures: u32,
    pub next_run_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl RoutineRow {
    pub fn to_api(&self) -> Routine {
        Routine {
            id: self.id,
            bot_id: self.bot,
            name: self.name.clone(),
            prompt: self.prompt.clone(),
            schedule: self.schedule.clone(),
            push: self.push,
            paused: self.paused,
            failures: self.failures,
            next_run_at: self.next_run_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

const ROUTINE_COLUMNS: &str = "id, user_id, bot_id, name, prompt, schedule, push, paused, \
     failures, next_run_at, created_at, updated_at";

fn push_name(push: RoutinePush) -> &'static str {
    match push {
        RoutinePush::EveryRun => "every_run",
        RoutinePush::NotifyOnly => "notify_only",
    }
}

fn push_from(name: &str) -> RoutinePush {
    match name {
        "notify_only" => RoutinePush::NotifyOnly,
        _ => RoutinePush::EveryRun,
    }
}

fn schedule_to_stored(schedule: &Schedule) -> Result<String> {
    serde_json::to_string(schedule).map_err(Failure::internal)
}

fn routine_from(row: &sqlx::sqlite::SqliteRow) -> Result<RoutineRow> {
    Ok(RoutineRow {
        id: RoutineId(row.get("id")),
        user: UserId(row.get("user_id")),
        bot: BotId(row.get("bot_id")),
        name: row.get("name"),
        prompt: row.get("prompt"),
        schedule: serde_json::from_str(row.get("schedule")).map_err(Failure::internal)?,
        push: push_from(row.get::<&str, _>("push")),
        paused: row.get::<i64, _>("paused") != 0,
        failures: u32::try_from(row.get::<i64, _>("failures")).unwrap_or(u32::MAX),
        next_run_at: row.get::<Option<i64>, _>("next_run_at").map(Timestamp),
        created_at: Timestamp(row.get("created_at")),
        updated_at: Timestamp(row.get("updated_at")),
    })
}

fn routines_from(rows: &[sqlx::sqlite::SqliteRow]) -> Result<Vec<RoutineRow>> {
    rows.iter().map(routine_from).collect()
}

// ---------------------------------------------------------------------------
// Validation

fn check_text(field: &str, value: &str, max: usize, one_line: bool) -> Result<String> {
    let value = value.trim().to_owned();
    let length = value.chars().count();
    if length == 0 || length > max {
        return Err(Failure::invalid(format!(
            "{field} needs 1 to {max} characters"
        )));
    }
    if one_line && value.contains('\n') {
        return Err(Failure::invalid(format!("{field} is one line")));
    }
    if value
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(Failure::invalid(format!("{field} has control characters")));
    }
    Ok(value)
}

pub fn check_name(name: &str) -> Result<String> {
    check_text("A routine's name", name, MAX_NAME_CHARS, true)
}

pub fn check_prompt(prompt: &str) -> Result<String> {
    check_text("A routine's prompt", prompt, MAX_PROMPT_CHARS, false)
}

fn unique_name<T>(result: std::result::Result<T, sqlx::Error>) -> Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(Failure::conflict(
            "This Bot already has a routine with that name",
        )),
        Err(e) => Err(e.into()),
    }
}

// ---------------------------------------------------------------------------
// Routines

/// How many routines the user has.
pub async fn count(db: impl SqliteExecutor<'_>, user: UserId) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM routines WHERE user_id = ?")
            .bind(user.0)
            .fetch_one(db)
            .await?,
    )
}

/// A routine to create, already checked.
#[derive(Debug)]
pub struct NewRoutine<'a> {
    pub bot: BotId,
    pub name: &'a str,
    pub prompt: &'a str,
    pub schedule: &'a Schedule,
    pub push: RoutinePush,
    pub paused: bool,
    pub next_run_at: Option<Timestamp>,
}

/// Creates a routine on one of the user's Bots. Another user's Bot reads
/// as not found.
pub async fn create(
    db: impl SqliteExecutor<'_>,
    now: Timestamp,
    user: UserId,
    new: NewRoutine<'_>,
) -> Result<RoutineRow> {
    let row = unique_name(
        sqlx::query(&format!(
            "INSERT INTO routines (user_id, bot_id, name, prompt, schedule, push, paused,
                                   next_run_at, created_at, updated_at)
             SELECT user_id, id, ?, ?, ?, ?, ?, ?, ?, ? FROM bots WHERE id = ? AND user_id = ?
             RETURNING {ROUTINE_COLUMNS}"
        ))
        .bind(new.name)
        .bind(new.prompt)
        .bind(schedule_to_stored(new.schedule)?)
        .bind(push_name(new.push))
        .bind(new.paused)
        .bind(new.next_run_at.map(|t| t.0))
        .bind(now.0)
        .bind(now.0)
        .bind(new.bot.0)
        .bind(user.0)
        .fetch_optional(db)
        .await,
    )?;
    row.as_ref()
        .map(routine_from)
        .transpose()?
        .ok_or_else(Failure::not_found)
}

/// One of the user's routines, on the Bot named.
pub async fn routine(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    bot: BotId,
    id: RoutineId,
) -> Result<Option<RoutineRow>> {
    let row = sqlx::query(&format!(
        "SELECT {ROUTINE_COLUMNS} FROM routines WHERE id = ? AND bot_id = ? AND user_id = ?"
    ))
    .bind(id.0)
    .bind(bot.0)
    .bind(user.0)
    .fetch_optional(db)
    .await?;
    row.as_ref().map(routine_from).transpose()
}

/// One of the user's routines, by its id alone.
pub async fn routine_of_user(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    id: RoutineId,
) -> Result<Option<RoutineRow>> {
    let row = sqlx::query(&format!(
        "SELECT {ROUTINE_COLUMNS} FROM routines WHERE id = ? AND user_id = ?"
    ))
    .bind(id.0)
    .bind(user.0)
    .fetch_optional(db)
    .await?;
    row.as_ref().map(routine_from).transpose()
}

/// The routines of one of the user's Bots, by id.
pub async fn list_for_bot(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    bot: BotId,
) -> Result<Vec<RoutineRow>> {
    let rows = sqlx::query(&format!(
        "SELECT {ROUTINE_COLUMNS} FROM routines WHERE bot_id = ? AND user_id = ? ORDER BY id"
    ))
    .bind(bot.0)
    .bind(user.0)
    .fetch_all(db)
    .await?;
    routines_from(&rows)
}

/// Every routine of the user's, by id.
pub async fn list_for_user(db: impl SqliteExecutor<'_>, user: UserId) -> Result<Vec<RoutineRow>> {
    let rows = sqlx::query(&format!(
        "SELECT {ROUTINE_COLUMNS} FROM routines WHERE user_id = ? ORDER BY id"
    ))
    .bind(user.0)
    .fetch_all(db)
    .await?;
    routines_from(&rows)
}

/// Writes a routine's fields back, as changed by its owner or the
/// scheduler. Returns the row as stored.
pub async fn save(
    db: impl SqliteExecutor<'_>,
    now: Timestamp,
    routine: &RoutineRow,
) -> Result<RoutineRow> {
    let row = unique_name(
        sqlx::query(&format!(
            "UPDATE routines SET name = ?, prompt = ?, schedule = ?, push = ?, paused = ?,
                 failures = ?, next_run_at = ?, updated_at = ?
             WHERE id = ? AND bot_id = ? AND user_id = ? RETURNING {ROUTINE_COLUMNS}"
        ))
        .bind(&routine.name)
        .bind(&routine.prompt)
        .bind(schedule_to_stored(&routine.schedule)?)
        .bind(push_name(routine.push))
        .bind(routine.paused)
        .bind(i64::from(routine.failures))
        .bind(routine.next_run_at.map(|t| t.0))
        .bind(now.0)
        .bind(routine.id.0)
        .bind(routine.bot.0)
        .bind(routine.user.0)
        .fetch_optional(db)
        .await,
    )?;
    row.as_ref()
        .map(routine_from)
        .transpose()?
        .ok_or_else(Failure::not_found)
}

/// Deletes one of the user's routines with its runs. Returns whether it
/// existed.
pub async fn delete(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    bot: BotId,
    id: RoutineId,
) -> Result<bool> {
    let done = sqlx::query("DELETE FROM routines WHERE id = ? AND bot_id = ? AND user_id = ?")
        .bind(id.0)
        .bind(bot.0)
        .bind(user.0)
        .execute(db)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// For the scheduler: every user's routines that are due at `now`.
pub async fn due(db: impl SqliteExecutor<'_>, now: Timestamp) -> Result<Vec<RoutineRow>> {
    let rows = sqlx::query(&format!(
        "SELECT {ROUTINE_COLUMNS} FROM routines
         WHERE paused = 0 AND next_run_at IS NOT NULL AND next_run_at <= ?
         ORDER BY next_run_at, id"
    ))
    .bind(now.0)
    .fetch_all(db)
    .await?;
    routines_from(&rows)
}

/// Counts a finished run against its routine: a failure adds one to its
/// failures and pauses it at [`FAILURES_TO_PAUSE`]; a success clears them.
/// Returns the routine if it changed.
pub async fn count_outcome(
    conn: &mut SqliteConnection,
    now: Timestamp,
    run: &RunRow,
    failed: bool,
) -> Result<Option<RoutineRow>> {
    let Some(mut routine) = routine_of_user(&mut *conn, run.user, run.routine).await? else {
        return Ok(None);
    };
    if failed {
        routine.failures = routine.failures.saturating_add(1);
        if routine.failures >= FAILURES_TO_PAUSE && !routine.paused {
            routine.paused = true;
            routine.next_run_at = None;
            tracing::info!(user = %run.user, routine = %routine.id, "pausing a routine after failed runs");
        }
    } else if routine.failures > 0 {
        routine.failures = 0;
    } else {
        return Ok(None);
    }
    save(&mut *conn, now, &routine).await.map(Some)
}

// ---------------------------------------------------------------------------
// Runs

/// A run row.
#[derive(Debug, Clone)]
pub struct RunRow {
    pub id: RoutineRunId,
    pub user: UserId,
    pub bot: BotId,
    pub routine: RoutineId,
    pub trigger: RunTrigger,
    pub scheduled_for: Timestamp,
    pub queued_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub state: RunState,
    pub reason: Option<String>,
    pub waiting_for: Option<Waiting>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub deadline_passed_at: Option<Timestamp>,
    pub tokens: Option<u64>,
}

impl RunRow {
    pub fn to_api(&self, messages: Vec<MessageId>) -> RoutineRun {
        RoutineRun {
            id: self.id,
            routine_id: self.routine,
            bot_id: self.bot,
            trigger: self.trigger,
            scheduled_for: self.scheduled_for,
            started_at: self.started_at,
            finished_at: self.finished_at,
            state: self.state,
            reason: self.reason.clone(),
            messages,
            tokens: self.tokens,
        }
    }
}

/// What a waiting run last waited for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waiting {
    /// The Bot's running turn.
    Busy,
    /// The household's ChatGPT sign-in.
    SignedOut,
}

const RUN_COLUMNS: &str = "id, user_id, bot_id, routine_id, trigger, scheduled_for, queued_at, \
     started_at, finished_at, state, reason, waiting_for, thread_id, turn_id, \
     deadline_passed_at, tokens";

pub fn state_name(state: RunState) -> &'static str {
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

fn state_from(name: &str) -> RunState {
    match name {
        "waiting" => RunState::Waiting,
        "running" => RunState::Running,
        "succeeded" => RunState::Succeeded,
        "missed" => RunState::Missed,
        "skipped" => RunState::Skipped,
        "interrupted" => RunState::Interrupted,
        _ => RunState::Failed,
    }
}

fn trigger_name(trigger: RunTrigger) -> &'static str {
    match trigger {
        RunTrigger::Schedule => "schedule",
        RunTrigger::Manual => "manual",
    }
}

fn waiting_name(waiting: Waiting) -> &'static str {
    match waiting {
        Waiting::Busy => "busy",
        Waiting::SignedOut => "signed_out",
    }
}

fn run_from(row: &sqlx::sqlite::SqliteRow) -> RunRow {
    RunRow {
        id: RoutineRunId(row.get("id")),
        user: UserId(row.get("user_id")),
        bot: BotId(row.get("bot_id")),
        routine: RoutineId(row.get("routine_id")),
        trigger: if row.get::<&str, _>("trigger") == "manual" {
            RunTrigger::Manual
        } else {
            RunTrigger::Schedule
        },
        scheduled_for: Timestamp(row.get("scheduled_for")),
        queued_at: Timestamp(row.get("queued_at")),
        started_at: row.get::<Option<i64>, _>("started_at").map(Timestamp),
        finished_at: row.get::<Option<i64>, _>("finished_at").map(Timestamp),
        state: state_from(row.get::<&str, _>("state")),
        reason: row.get("reason"),
        waiting_for: match row.get::<Option<&str>, _>("waiting_for") {
            Some("busy") => Some(Waiting::Busy),
            Some("signed_out") => Some(Waiting::SignedOut),
            _ => None,
        },
        thread_id: row.get("thread_id"),
        turn_id: row.get("turn_id"),
        deadline_passed_at: row
            .get::<Option<i64>, _>("deadline_passed_at")
            .map(Timestamp),
        tokens: row
            .get::<Option<i64>, _>("tokens")
            .map(|t| u64::try_from(t).unwrap_or(0)),
    }
}

/// A run to record. A run that ends at once (missed or skipped) is
/// recorded finished.
#[derive(Debug, Clone, Copy)]
pub struct NewRun<'a> {
    pub trigger: RunTrigger,
    pub scheduled_for: Timestamp,
    pub state: RunState,
    pub reason: Option<&'a str>,
}

pub async fn insert_run(
    db: impl SqliteExecutor<'_>,
    now: Timestamp,
    routine: &RoutineRow,
    new: NewRun<'_>,
) -> Result<RunRow> {
    let finished = new.state.is_finished().then_some(now.0);
    let row = sqlx::query(&format!(
        "INSERT INTO routine_runs (user_id, bot_id, routine_id, trigger, scheduled_for,
                                   queued_at, finished_at, state, reason)
         SELECT user_id, bot_id, id, ?, ?, ?, ?, ?, ? FROM routines WHERE id = ? AND user_id = ?
         RETURNING {RUN_COLUMNS}"
    ))
    .bind(trigger_name(new.trigger))
    .bind(new.scheduled_for.0)
    .bind(now.0)
    .bind(finished)
    .bind(state_name(new.state))
    .bind(new.reason)
    .bind(routine.id.0)
    .bind(routine.user.0)
    .fetch_optional(db)
    .await?;
    row.as_ref().map(run_from).ok_or_else(Failure::not_found)
}

/// One of the user's runs.
pub async fn run(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    id: RoutineRunId,
) -> Result<Option<RunRow>> {
    let row = sqlx::query(&format!(
        "SELECT {RUN_COLUMNS} FROM routine_runs WHERE id = ? AND user_id = ?"
    ))
    .bind(id.0)
    .bind(user.0)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(run_from))
}

/// The routine's run that is waiting or running, if any.
pub async fn active_run(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    routine: RoutineId,
) -> Result<Option<RunRow>> {
    let row = sqlx::query(&format!(
        "SELECT {RUN_COLUMNS} FROM routine_runs
         WHERE routine_id = ? AND user_id = ? AND state IN ('waiting', 'running')
         ORDER BY id LIMIT 1"
    ))
    .bind(routine.0)
    .bind(user.0)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(run_from))
}

/// For the scheduler: every user's runs in `state`, oldest first.
pub async fn active_runs(db: impl SqliteExecutor<'_>, state: RunState) -> Result<Vec<RunRow>> {
    let rows = sqlx::query(&format!(
        "SELECT {RUN_COLUMNS} FROM routine_runs WHERE state = ? ORDER BY id"
    ))
    .bind(state_name(state))
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(run_from).collect())
}

/// The user's running runs, of one Bot or all of them.
pub async fn running_runs_of(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    bot: Option<BotId>,
) -> Result<Vec<RunRow>> {
    let rows = sqlx::query(&format!(
        "SELECT {RUN_COLUMNS} FROM routine_runs
         WHERE user_id = ? AND state = 'running' AND (? IS NULL OR bot_id = ?) ORDER BY id"
    ))
    .bind(user.0)
    .bind(bot.map(|b| b.0))
    .bind(bot.map(|b| b.0))
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(run_from).collect())
}

/// The user's run of a Codex turn, if a routine ran it.
pub async fn run_of_turn(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    turn_id: &str,
) -> Result<Option<RunRow>> {
    let row = sqlx::query(&format!(
        "SELECT {RUN_COLUMNS} FROM routine_runs WHERE user_id = ? AND turn_id = ?
         ORDER BY id DESC LIMIT 1"
    ))
    .bind(user.0)
    .bind(turn_id)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(run_from))
}

/// Records what a waiting run waits for. Returns the run if it changed.
pub async fn set_waiting(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    id: RoutineRunId,
    waiting: Waiting,
    reason: &str,
) -> Result<Option<RunRow>> {
    let row = sqlx::query(&format!(
        "UPDATE routine_runs SET waiting_for = ?1, reason = ?2
         WHERE id = ?3 AND user_id = ?4 AND state = 'waiting'
           AND (waiting_for IS NOT ?1 OR reason IS NOT ?2)
         RETURNING {RUN_COLUMNS}"
    ))
    .bind(waiting_name(waiting))
    .bind(reason)
    .bind(id.0)
    .bind(user.0)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(run_from))
}

/// Moves a waiting run's time due to a later one it stands for. Returns
/// the run if it moved.
pub async fn reschedule_waiting(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    id: RoutineRunId,
    scheduled_for: Timestamp,
) -> Result<Option<RunRow>> {
    let row = sqlx::query(&format!(
        "UPDATE routine_runs SET scheduled_for = ?1
         WHERE id = ?2 AND user_id = ?3 AND state = 'waiting' AND scheduled_for < ?1
         RETURNING {RUN_COLUMNS}"
    ))
    .bind(scheduled_for.0)
    .bind(id.0)
    .bind(user.0)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(run_from))
}

/// Marks a waiting run running in a Codex turn.
pub async fn set_running(
    db: impl SqliteExecutor<'_>,
    now: Timestamp,
    user: UserId,
    id: RoutineRunId,
    thread_id: &str,
    turn_id: &str,
) -> Result<Option<RunRow>> {
    let row = sqlx::query(&format!(
        "UPDATE routine_runs SET state = 'running', started_at = ?, thread_id = ?, turn_id = ?,
             waiting_for = NULL, reason = NULL
         WHERE id = ? AND user_id = ? AND state = 'waiting' RETURNING {RUN_COLUMNS}"
    ))
    .bind(now.0)
    .bind(thread_id)
    .bind(turn_id)
    .bind(id.0)
    .bind(user.0)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(run_from))
}

/// Ends a run that is waiting or running, with the tokens its turn used,
/// if it had one and Codex reported them. Returns it if it had not ended.
pub async fn finish(
    db: impl SqliteExecutor<'_>,
    now: Timestamp,
    user: UserId,
    id: RoutineRunId,
    state: RunState,
    reason: Option<&str>,
) -> Result<Option<RunRow>> {
    finish_from(db, now, user, id, state, reason, "'waiting', 'running'").await
}

/// Ends a run that is still waiting, leaving one that started alone.
/// Returns it if it was waiting.
pub async fn finish_waiting(
    db: impl SqliteExecutor<'_>,
    now: Timestamp,
    user: UserId,
    id: RoutineRunId,
    state: RunState,
    reason: Option<&str>,
) -> Result<Option<RunRow>> {
    finish_from(db, now, user, id, state, reason, "'waiting'").await
}

async fn finish_from(
    db: impl SqliteExecutor<'_>,
    now: Timestamp,
    user: UserId,
    id: RoutineRunId,
    state: RunState,
    reason: Option<&str>,
    from: &'static str,
) -> Result<Option<RunRow>> {
    let row = sqlx::query(&format!(
        "UPDATE routine_runs SET state = ?, reason = ?, finished_at = ?, waiting_for = NULL,
             tokens = (SELECT sum(t.tokens) FROM token_usage t
                       WHERE t.user_id = routine_runs.user_id
                         AND t.turn_id = routine_runs.turn_id)
         WHERE id = ? AND user_id = ? AND state IN ({from})
         RETURNING {RUN_COLUMNS}"
    ))
    .bind(state_name(state))
    .bind(reason)
    .bind(now.0)
    .bind(id.0)
    .bind(user.0)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(run_from))
}

/// Records that the gateway interrupted a run's turn for running too long.
pub async fn set_deadline_passed(
    db: impl SqliteExecutor<'_>,
    now: Timestamp,
    user: UserId,
    id: RoutineRunId,
) -> Result<()> {
    sqlx::query(
        "UPDATE routine_runs SET deadline_passed_at = ?
         WHERE id = ? AND user_id = ? AND state = 'running' AND deadline_passed_at IS NULL",
    )
    .bind(now.0)
    .bind(id.0)
    .bind(user.0)
    .execute(db)
    .await?;
    Ok(())
}

/// The messages a run added to the conversation: its marker, then the
/// replies of its turn.
pub async fn run_messages(db: impl SqliteExecutor<'_>, run: &RunRow) -> Result<Vec<MessageId>> {
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM messages
         WHERE user_id = ?1 AND bot_id = ?2
           AND (routine_run_id = ?3 OR (?4 IS NOT NULL AND turn_id = ?4))
         ORDER BY id",
    )
    .bind(run.user.0)
    .bind(run.bot.0)
    .bind(run.id.0)
    .bind(run.turn_id.as_deref())
    .fetch_all(db)
    .await?;
    Ok(ids.into_iter().map(MessageId).collect())
}

/// A page of a routine's runs, newest first, older than `before`.
pub async fn runs_page(
    conn: &mut SqliteConnection,
    user: UserId,
    routine: RoutineId,
    before: Option<RoutineRunId>,
    limit: u32,
) -> Result<(Vec<RoutineRun>, Option<RoutineRunId>)> {
    let limit = limit.clamp(1, 200);
    let rows = sqlx::query(&format!(
        "SELECT {RUN_COLUMNS} FROM routine_runs
         WHERE routine_id = ? AND user_id = ? AND (? IS NULL OR id < ?)
         ORDER BY id DESC LIMIT ?"
    ))
    .bind(routine.0)
    .bind(user.0)
    .bind(before.map(|r| r.0))
    .bind(before.map(|r| r.0))
    .bind(i64::from(limit) + 1)
    .fetch_all(&mut *conn)
    .await?;
    let mut runs: Vec<RunRow> = rows.iter().map(run_from).collect();
    let older = if runs.len() > limit as usize {
        runs.truncate(limit as usize);
        runs.last().map(|r| r.id)
    } else {
        None
    };
    let mut page = Vec::with_capacity(runs.len());
    for run in &runs {
        let messages = run_messages(&mut *conn, run).await?;
        page.push(run.to_api(messages));
    }
    Ok((page, older))
}

/// Deletes finished runs queued more than [`RUN_RETENTION_MS`] ago.
pub async fn prune(conn: &mut SqliteConnection, now: Timestamp) -> Result<u64> {
    let done = sqlx::query(
        "DELETE FROM routine_runs
         WHERE queued_at <= ? AND state NOT IN ('waiting', 'running')",
    )
    .bind(now.0 - RUN_RETENTION_MS)
    .execute(conn)
    .await?;
    Ok(done.rows_affected())
}
