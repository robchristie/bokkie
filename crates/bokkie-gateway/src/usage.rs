//! Usage: the shared subscription's rate-limit windows, and each user's own
//! allowance.
//!
//! The subscription is shared by the whole household, so an admin can give
//! each user an allowance: turns per rolling 24 hours and tokens per rolling
//! seven days. Turns are counted when the gateway starts one; tokens come
//! from `thread/tokenUsage/updated`, as the growth of each thread's total.
//! Usage is deleted once its window no longer counts it.

use std::collections::HashMap;

use bokkie_codex::protocol::{GetAccountRateLimitsResponse, RateLimitSnapshot};
use bokkie_protocol::api::{Allowance, Limits, RateLimitBucket, RateLimitWindow, Usage};
use bokkie_protocol::{BotId, Timestamp, UserId};
use sqlx::sqlite::{SqliteConnection, SqliteExecutor};

use crate::clock::DAY_MS;
use crate::error::{Failure, Result};

pub const TURN_WINDOW_MS: i64 = DAY_MS;
pub const TOKEN_WINDOW_MS: i64 = 7 * DAY_MS;

/// Counts a turn for the user, if their allowance has one left, before the
/// gateway asks Codex to start it. Run inside a batch, so the check and the
/// count cannot be split by another send. Returns the count's id, to
/// [`release_turn`] if Codex refuses the turn.
pub async fn reserve_turn(
    conn: &mut SqliteConnection,
    now: Timestamp,
    user: UserId,
    bot: BotId,
) -> Result<Option<i64>> {
    let limits = crate::accounts::account(&mut *conn, user)
        .await?
        .map(|a| a.limits)
        .unwrap_or_default();
    if allowance(&mut *conn, now, user, limits).await?.exhausted {
        return Ok(None);
    }
    let id = sqlx::query_scalar(
        "INSERT INTO turns (user_id, bot_id, at)
         SELECT user_id, id, ? FROM bots WHERE id = ? AND user_id = ? RETURNING id",
    )
    .bind(now.0)
    .bind(bot.0)
    .bind(user.0)
    .fetch_optional(conn)
    .await?;
    id.map(Some).ok_or_else(Failure::not_found)
}

/// Takes back a turn counted by [`reserve_turn`] that Codex refused.
pub async fn release_turn(db: impl SqliteExecutor<'_>, user: UserId, turn: i64) -> Result<()> {
    sqlx::query("DELETE FROM turns WHERE id = ? AND user_id = ?")
        .bind(turn)
        .bind(user.0)
        .execute(db)
        .await?;
    Ok(())
}

/// Records a thread's reported token total, storing its growth since the
/// last report. A total lower than the last one becomes the new baseline.
/// It reads and then writes, so `conn` must be in a write transaction, such
/// as an event batch.
pub async fn record_tokens(
    conn: &mut SqliteConnection,
    now: Timestamp,
    user: UserId,
    bot: BotId,
    thread_total: u64,
) -> Result<u64> {
    let reported: Option<i64> =
        sqlx::query_scalar("SELECT tokens_reported FROM bots WHERE id = ? AND user_id = ?")
            .bind(bot.0)
            .bind(user.0)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(reported) = reported else {
        return Err(Failure::not_found());
    };
    let total = i64::try_from(thread_total).unwrap_or(i64::MAX);
    // A total that fell is a new baseline, not new usage: counting it all
    // could lock a user out for a whole thread's history.
    let growth = (total - reported).max(0);
    sqlx::query("UPDATE bots SET tokens_reported = ? WHERE id = ? AND user_id = ?")
        .bind(total)
        .bind(bot.0)
        .bind(user.0)
        .execute(&mut *conn)
        .await?;
    if growth > 0 {
        sqlx::query("INSERT INTO token_usage (user_id, bot_id, at, tokens) VALUES (?, ?, ?, ?)")
            .bind(user.0)
            .bind(bot.0)
            .bind(now.0)
            .bind(growth)
            .execute(&mut *conn)
            .await?;
    }
    Ok(growth as u64)
}

/// The user's allowance and what they have used of it.
pub async fn allowance(
    db: impl SqliteExecutor<'_>,
    now: Timestamp,
    user: UserId,
    limits: Limits,
) -> Result<Allowance> {
    let (turns, tokens): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM turns WHERE user_id = ?1 AND at > ?2),
                (SELECT coalesce(sum(tokens), 0) FROM token_usage
                 WHERE user_id = ?1 AND at > ?3)",
    )
    .bind(user.0)
    .bind(now.0 - TURN_WINDOW_MS)
    .bind(now.0 - TOKEN_WINDOW_MS)
    .fetch_one(db)
    .await?;
    let turns_today = u32::try_from(turns).unwrap_or(u32::MAX);
    let tokens_this_week = u64::try_from(tokens).unwrap_or(0);
    let exhausted = limits.daily_turns.is_some_and(|l| turns_today >= l)
        || limits.weekly_tokens.is_some_and(|l| tokens_this_week >= l);
    Ok(Allowance {
        limits,
        turns_today,
        tokens_this_week,
        exhausted,
    })
}

/// The users with usage that left an allowance window in `(since, now]`.
pub async fn rolled_over(
    db: impl SqliteExecutor<'_>,
    since: Timestamp,
    now: Timestamp,
) -> Result<Vec<UserId>> {
    let users: Vec<i64> = sqlx::query_scalar(
        "SELECT user_id FROM turns WHERE at > ?1 AND at <= ?2
         UNION
         SELECT user_id FROM token_usage WHERE at > ?3 AND at <= ?4",
    )
    .bind(since.0 - TURN_WINDOW_MS)
    .bind(now.0 - TURN_WINDOW_MS)
    .bind(since.0 - TOKEN_WINDOW_MS)
    .bind(now.0 - TOKEN_WINDOW_MS)
    .fetch_all(db)
    .await?;
    Ok(users.into_iter().map(UserId).collect())
}

/// Deletes usage that no allowance window still counts.
pub async fn prune(conn: &mut SqliteConnection, now: Timestamp) -> Result<()> {
    sqlx::query("DELETE FROM turns WHERE at <= ?")
        .bind(now.0 - TURN_WINDOW_MS)
        .execute(&mut *conn)
        .await?;
    sqlx::query("DELETE FROM token_usage WHERE at <= ?")
        .bind(now.0 - TOKEN_WINDOW_MS)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Codex reports `resetsAt` in Unix seconds; the wire uses milliseconds.
fn seconds_to_timestamp(seconds: i64) -> Timestamp {
    Timestamp(seconds.saturating_mul(1000))
}

fn bucket(id: &str, snapshot: &RateLimitSnapshot) -> RateLimitBucket {
    let window = |w: &bokkie_codex::protocol::RateLimitWindow| RateLimitWindow {
        used_percent: w.used_percent,
        window_minutes: w.window_duration_mins,
        resets_at: w.resets_at.map(seconds_to_timestamp),
    };
    RateLimitBucket {
        id: snapshot.limit_id.clone().unwrap_or_else(|| id.to_owned()),
        name: snapshot.limit_name.clone(),
        primary: snapshot.primary.as_ref().map(window),
        secondary: snapshot.secondary.as_ref().map(window),
        reached: snapshot.rate_limit_reached_type.is_some()
            || [&snapshot.primary, &snapshot.secondary]
                .into_iter()
                .flatten()
                .any(|w| w.used_percent >= 100),
    }
}

/// The subscription's buckets from `account/rateLimits/read`.
pub fn buckets(response: &GetAccountRateLimitsResponse) -> Vec<RateLimitBucket> {
    match &response.rate_limits_by_limit_id {
        Some(by_id) if !by_id.is_empty() => by_id.iter().map(|(id, s)| bucket(id, s)).collect(),
        _ => vec![bucket("codex", &response.rate_limits)],
    }
}

/// Merges an `account/rateLimits/updated` snapshot into known buckets.
/// Codex sends sparse updates, so only the values present replace known
/// ones.
pub fn merge(buckets: &mut Vec<RateLimitBucket>, snapshot: &RateLimitSnapshot) {
    let updated = bucket("codex", snapshot);
    match buckets.iter_mut().find(|b| b.id == updated.id) {
        Some(existing) => {
            if updated.name.is_some() {
                existing.name = updated.name;
            }
            if updated.primary.is_some() {
                existing.primary = updated.primary;
            }
            if updated.secondary.is_some() {
                existing.secondary = updated.secondary;
            }
            existing.reached = snapshot.rate_limit_reached_type.is_some()
                || [&existing.primary, &existing.secondary]
                    .into_iter()
                    .flatten()
                    .any(|w| w.used_percent >= 100);
        }
        None => buckets.push(updated),
    }
}

/// Whether a known reset time has passed, so the cached buckets are stale.
pub fn stale(buckets: &[RateLimitBucket], now: Timestamp) -> bool {
    buckets
        .iter()
        .flat_map(|b| [&b.primary, &b.secondary])
        .flatten()
        .any(|w| w.resets_at.is_some_and(|at| at <= now))
}

/// The latest subscription buckets each user's app server reported.
#[derive(Debug, Default)]
pub struct SubscriptionCache {
    buckets: std::sync::Mutex<HashMap<UserId, Vec<RateLimitBucket>>>,
}

impl SubscriptionCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<UserId, Vec<RateLimitBucket>>> {
        self.buckets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn get(&self, user: UserId) -> Option<Vec<RateLimitBucket>> {
        self.lock().get(&user).cloned()
    }

    pub fn set(&self, user: UserId, buckets: Vec<RateLimitBucket>) {
        self.lock().insert(user, buckets);
    }

    /// The users with a known window that resets in `(since, now]`.
    pub fn resetting(&self, since: Timestamp, now: Timestamp) -> Vec<UserId> {
        self.lock()
            .iter()
            .filter(|(_, buckets)| {
                buckets
                    .iter()
                    .flat_map(|b| [&b.primary, &b.secondary])
                    .flatten()
                    .any(|w| w.resets_at.is_some_and(|at| since < at && at <= now))
            })
            .map(|(user, _)| *user)
            .collect()
    }

    pub fn update(&self, user: UserId, snapshot: &RateLimitSnapshot) -> Vec<RateLimitBucket> {
        let mut all = self.lock();
        let buckets = all.entry(user).or_default();
        merge(buckets, snapshot);
        buckets.clone()
    }
}

/// The usage meter for one user.
pub fn usage(subscription: Vec<RateLimitBucket>, allowance: Allowance) -> Usage {
    Usage {
        subscription,
        allowance,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bokkie_codex::protocol::RateLimitWindow as CodexWindow;

    fn snapshot(used: i32, reached: Option<&str>) -> RateLimitSnapshot {
        RateLimitSnapshot {
            limit_id: Some("codex".into()),
            limit_name: None,
            plan_type: Some("pro".into()),
            primary: Some(CodexWindow {
                used_percent: used,
                window_duration_mins: Some(10_080),
                resets_at: Some(1_800_600_000),
            }),
            secondary: None,
            rate_limit_reached_type: reached.map(str::to_owned),
        }
    }

    #[test]
    fn reset_times_become_milliseconds() {
        let response = GetAccountRateLimitsResponse {
            rate_limits: snapshot(12, None),
            rate_limits_by_limit_id: None,
        };
        let buckets = buckets(&response);
        assert_eq!(buckets.len(), 1);
        let primary = buckets[0].primary.as_ref().unwrap();
        assert_eq!(primary.resets_at, Some(Timestamp(1_800_600_000_000)));
        assert_eq!(primary.window_minutes, Some(10_080));
        assert!(!buckets[0].reached);
    }

    #[test]
    fn a_bucket_is_reached_when_codex_says_so_or_it_is_full() {
        assert!(bucket("codex", &snapshot(40, Some("primary"))).reached);
        assert!(bucket("codex", &snapshot(100, None)).reached);
        let mut buckets = vec![bucket("codex", &snapshot(10, None))];
        merge(&mut buckets, &snapshot(100, None));
        assert_eq!(buckets.len(), 1);
        assert!(buckets[0].reached);
    }

    #[test]
    fn sparse_updates_keep_what_they_do_not_mention() {
        let mut full = snapshot(10, None);
        full.secondary = Some(CodexWindow {
            used_percent: 40,
            window_duration_mins: Some(300),
            resets_at: Some(1_800_000_100),
        });
        full.limit_name = Some("Codex".into());
        let mut buckets = vec![bucket("codex", &full)];
        let mut sparse = snapshot(15, None);
        sparse.limit_name = None;
        merge(&mut buckets, &sparse);
        assert_eq!(buckets[0].primary.as_ref().unwrap().used_percent, 15);
        assert_eq!(buckets[0].secondary.as_ref().unwrap().used_percent, 40);
        assert_eq!(buckets[0].name.as_deref(), Some("Codex"));
        assert!(stale(&buckets, Timestamp(1_800_000_100_000)));
        assert!(!stale(&buckets, Timestamp(1_800_000_099_000)));
    }
}
