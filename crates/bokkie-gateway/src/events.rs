//! The per-user event log behind the event WebSocket.
//!
//! Every change a client should hear about is appended here: it gets the
//! user's next sequence number, is stored, and is broadcast to that user's
//! open connections. Only the gateway writes the log, so it allocates
//! sequence numbers in memory under a per-user lock, starting from the
//! stored maximum. They survive restarts. The log keeps the newest
//! [`EventLog::retention`] events per user.

use std::collections::HashMap;
use std::sync::Arc;

use bokkie_protocol::events::{Event, EventEnvelope, Seq};
use bokkie_protocol::{Timestamp, UserId};
use sqlx::{Row, SqlitePool};
use tokio::sync::broadcast;

use crate::clock::Clock;
use crate::error::{Failure, Result};

/// How many events each user's log keeps by default.
pub const DEFAULT_RETENTION: u64 = 5_000;
/// How many unsent events one connection may fall behind by before it is
/// disconnected.
pub const DEFAULT_CAPACITY: usize = 1_024;

#[derive(Debug)]
struct UserLog {
    /// The last sequence number written, once read from the database.
    last: tokio::sync::Mutex<Option<u64>>,
    live: broadcast::Sender<EventEnvelope>,
}

/// The gateway's event logs, one per user.
#[derive(Debug)]
pub struct EventLog {
    db: SqlitePool,
    clock: Clock,
    retention: u64,
    capacity: usize,
    users: std::sync::Mutex<HashMap<UserId, Arc<UserLog>>>,
}

impl EventLog {
    pub fn new(db: SqlitePool, clock: Clock) -> Self {
        Self::with_limits(db, clock, DEFAULT_RETENTION, DEFAULT_CAPACITY)
    }

    pub fn with_limits(db: SqlitePool, clock: Clock, retention: u64, capacity: usize) -> Self {
        Self {
            db,
            clock,
            retention: retention.max(1),
            capacity: capacity.max(1),
            users: Default::default(),
        }
    }

    pub fn retention(&self) -> u64 {
        self.retention
    }

    fn user(&self, user: UserId) -> Arc<UserLog> {
        self.users
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(user)
            .or_insert_with(|| {
                Arc::new(UserLog {
                    last: tokio::sync::Mutex::new(None),
                    live: broadcast::channel(self.capacity).0,
                })
            })
            .clone()
    }

    async fn stored_last(&self, user: UserId) -> Result<u64> {
        let last: Option<i64> = sqlx::query_scalar("SELECT max(seq) FROM events WHERE user_id = ?")
            .bind(user.0)
            .fetch_one(&self.db)
            .await?;
        Ok(last.unwrap_or(0) as u64)
    }

    /// Appends an event to the user's log and broadcasts it.
    pub async fn append(&self, user: UserId, event: Event) -> Result<EventEnvelope> {
        let log = self.user(user);
        let mut last = log.last.lock().await;
        let previous = match *last {
            Some(previous) => previous,
            None => self.stored_last(user).await?,
        };
        let envelope = EventEnvelope {
            seq: Seq(previous + 1),
            at: self.clock.now(),
            event,
        };
        let body = serde_json::to_string(&envelope.event).map_err(Failure::internal)?;
        sqlx::query("INSERT INTO events (user_id, seq, at, body) VALUES (?, ?, ?, ?)")
            .bind(user.0)
            .bind(envelope.seq.0 as i64)
            .bind(envelope.at.0)
            .bind(body)
            .execute(&self.db)
            .await?;
        *last = Some(envelope.seq.0);
        if envelope.seq.0.is_multiple_of(256) && envelope.seq.0 > self.retention {
            sqlx::query("DELETE FROM events WHERE user_id = ? AND seq <= ?")
                .bind(user.0)
                .bind((envelope.seq.0 - self.retention) as i64)
                .execute(&self.db)
                .await?;
        }
        // Nobody listening is fine.
        let _ = log.live.send(envelope.clone());
        Ok(envelope)
    }

    /// The newest sequence number in the user's log, or zero.
    pub async fn latest(&self, user: UserId) -> Result<Seq> {
        let log = self.user(user);
        let mut last = log.last.lock().await;
        let value = match *last {
            Some(value) => value,
            None => {
                let value = self.stored_last(user).await?;
                *last = Some(value);
                value
            }
        };
        Ok(Seq(value))
    }

    /// The oldest retained sequence number, if the log holds any event.
    pub async fn earliest(&self, user: UserId) -> Result<Option<Seq>> {
        let first: Option<i64> =
            sqlx::query_scalar("SELECT min(seq) FROM events WHERE user_id = ?")
                .bind(user.0)
                .fetch_one(&self.db)
                .await?;
        Ok(first.map(|s| Seq(s as u64)))
    }

    /// Up to `limit` events after `after`, oldest first.
    pub async fn since(&self, user: UserId, after: Seq, limit: u32) -> Result<Vec<EventEnvelope>> {
        let rows = sqlx::query(
            "SELECT seq, at, body FROM events WHERE user_id = ? AND seq > ? ORDER BY seq LIMIT ?",
        )
        .bind(user.0)
        .bind(after.0 as i64)
        .bind(i64::from(limit))
        .fetch_all(&self.db)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(EventEnvelope {
                    seq: Seq(row.get::<i64, _>("seq") as u64),
                    at: Timestamp(row.get("at")),
                    event: serde_json::from_str(row.get("body")).map_err(Failure::internal)?,
                })
            })
            .collect()
    }

    /// The user's live events from now on.
    pub fn subscribe(&self, user: UserId) -> broadcast::Receiver<EventEnvelope> {
        self.user(user).live.subscribe()
    }
}
