//! The tool-call log: every brokered call, with its user, Bot, thread and
//! turn, the routine run whose turn it was, the tool, a bounded summary of
//! its arguments, its outcome and how long it took. Kept [`RETENTION_MS`];
//! each Bot's owner reads its entries.

use bokkie_protocol::api::{ToolCall, ToolCallOutcome};
use bokkie_protocol::{BotId, RoutineRunId, Timestamp, UserId};
use sqlx::Row;
use sqlx::sqlite::{SqliteConnection, SqliteExecutor};

use crate::clock::DAY_MS;
use crate::error::Result;

/// How long entries are kept.
pub const RETENTION_MS: i64 = 90 * DAY_MS;

/// The longest summary stored, in characters.
pub const SUMMARY_CHARS: usize = 200;

/// The longest id or tool name stored, in characters.
pub(crate) const ID_CHARS: usize = 128;

/// One call to record.
#[derive(Debug)]
pub struct Entry<'a> {
    pub user: UserId,
    pub bot: BotId,
    pub thread_id: &'a str,
    pub turn_id: &'a str,
    pub call_id: &'a str,
    pub tool: &'a str,
    pub summary: String,
    pub outcome: ToolCallOutcome,
    pub duration_ms: u64,
}

pub(crate) fn bounded(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

fn outcome_name(outcome: ToolCallOutcome) -> &'static str {
    match outcome {
        ToolCallOutcome::Ok => "ok",
        ToolCallOutcome::Failed => "failed",
        ToolCallOutcome::Refused => "refused",
        ToolCallOutcome::TimedOut => "timed_out",
    }
}

fn outcome_from(name: &str) -> ToolCallOutcome {
    match name {
        "ok" => ToolCallOutcome::Ok,
        "refused" => ToolCallOutcome::Refused,
        "timed_out" => ToolCallOutcome::TimedOut,
        _ => ToolCallOutcome::Failed,
    }
}

/// Records a call on one of the user's Bots, with the routine run whose
/// turn made it, if a routine's and the run is already recorded running in
/// that turn; [`attribute_to_run`] gives the run the calls made before.
pub async fn record(db: impl SqliteExecutor<'_>, now: Timestamp, entry: Entry<'_>) -> Result<()> {
    sqlx::query(
        "INSERT INTO tool_calls (bot_id, user_id, thread_id, turn_id, call_id, tool, summary,
                                 outcome, duration_ms, at, routine_run_id)
         SELECT id, user_id, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                (SELECT r.id FROM routine_runs r
                 WHERE r.user_id = ?10 AND r.bot_id = ?9 AND r.thread_id = ?1
                   AND r.turn_id = ?2
                 ORDER BY r.id DESC LIMIT 1)
         FROM bots WHERE id = ?9 AND user_id = ?10",
    )
    .bind(bounded(entry.thread_id, ID_CHARS))
    .bind(bounded(entry.turn_id, ID_CHARS))
    .bind(bounded(entry.call_id, ID_CHARS))
    .bind(bounded(entry.tool, ID_CHARS))
    .bind(bounded(&entry.summary, SUMMARY_CHARS))
    .bind(outcome_name(entry.outcome))
    .bind(i64::try_from(entry.duration_ms).unwrap_or(i64::MAX))
    .bind(now.0)
    .bind(entry.bot.0)
    .bind(entry.user.0)
    .execute(db)
    .await?;
    Ok(())
}

/// Gives the routine run whose turn this is the calls already recorded
/// for it without one: calls the model made after Codex started the turn
/// but before the run was recorded running in it. Returns how many.
pub async fn attribute_to_run(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    bot: BotId,
    thread_id: &str,
    turn_id: &str,
    run: RoutineRunId,
) -> Result<u64> {
    let done = sqlx::query(
        "UPDATE tool_calls SET routine_run_id = ?
         WHERE user_id = ? AND bot_id = ? AND thread_id = ? AND turn_id = ?
           AND routine_run_id IS NULL",
    )
    .bind(run.0)
    .bind(user.0)
    .bind(bot.0)
    .bind(bounded(thread_id, ID_CHARS))
    .bind(bounded(turn_id, ID_CHARS))
    .execute(db)
    .await?;
    Ok(done.rows_affected())
}

/// A page of the Bot's calls, newest first, older than `before`.
pub async fn page(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    bot: BotId,
    before: Option<i64>,
    limit: u32,
) -> Result<(Vec<ToolCall>, Option<i64>)> {
    let limit = limit.clamp(1, 200);
    let rows = sqlx::query(
        "SELECT id, tool, summary, outcome, duration_ms, at, routine_run_id FROM tool_calls
         WHERE bot_id = ? AND user_id = ? AND (? IS NULL OR id < ?)
         ORDER BY id DESC LIMIT ?",
    )
    .bind(bot.0)
    .bind(user.0)
    .bind(before)
    .bind(before)
    .bind(i64::from(limit) + 1)
    .fetch_all(db)
    .await?;
    let mut calls: Vec<ToolCall> = rows
        .iter()
        .map(|row| ToolCall {
            id: row.get("id"),
            tool: row.get("tool"),
            summary: row.get("summary"),
            outcome: outcome_from(row.get::<&str, _>("outcome")),
            duration_ms: u64::try_from(row.get::<i64, _>("duration_ms")).unwrap_or(0),
            at: Timestamp(row.get("at")),
            routine_run_id: row
                .get::<Option<i64>, _>("routine_run_id")
                .map(RoutineRunId),
        })
        .collect();
    let older = if calls.len() > limit as usize {
        calls.truncate(limit as usize);
        calls.last().map(|c| c.id)
    } else {
        None
    };
    Ok((calls, older))
}

/// Deletes entries older than [`RETENTION_MS`]. Returns how many.
pub async fn prune(conn: &mut SqliteConnection, now: Timestamp) -> Result<u64> {
    let done = sqlx::query("DELETE FROM tool_calls WHERE at <= ?")
        .bind(now.0 - RETENTION_MS)
        .execute(conn)
        .await?;
    Ok(done.rows_affected())
}
