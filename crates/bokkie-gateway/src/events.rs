//! The per-user event log behind the event WebSocket.
//!
//! Every change a client should hear about is appended here: it gets the
//! user's next sequence number, is stored, and is broadcast to that user's
//! open connections. A change to the database and its events are written in
//! one transaction, a [`Batch`]. Only the gateway writes the log, so it
//! allocates sequence numbers in memory under a per-user lock, starting
//! from the stored maximum. They survive restarts. The log keeps the newest
//! [`EventLog::retention`] events per user.

use std::collections::HashMap;
use std::sync::Arc;

use bokkie_protocol::events::{Event, EventEnvelope, Seq};
use bokkie_protocol::{Timestamp, UserId};
use sqlx::sqlite::{Sqlite, SqliteConnection, SqliteExecutor};
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
    /// Cleared while a batch is open, so a batch that never commits makes
    /// the next one read the stored maximum again.
    last: Arc<tokio::sync::Mutex<Option<u64>>>,
    live: broadcast::Sender<EventEnvelope>,
}

async fn stored_last(db: impl SqliteExecutor<'_>, user: UserId) -> Result<u64> {
    let last: Option<i64> = sqlx::query_scalar("SELECT max(seq) FROM events WHERE user_id = ?")
        .bind(user.0)
        .fetch_one(db)
        .await?;
    Ok(last.unwrap_or(0) as u64)
}

/// A write transaction whose events join the user's log as it commits.
///
/// A change and the events that announce it are written in one SQLite
/// transaction: they commit together or not at all. The batch holds the
/// user's log lock from [`EventLog::begin`] until it commits, so the user's
/// events are numbered, stored and broadcast in commit order, and an event
/// built from what the batch reads cannot be overtaken by an older one.
///
/// While a batch is open, its holder must not start another batch or
/// append for the same user, and should read and write only through the
/// batch: other connections wait on its write lock and cannot see its
/// changes. Dropping a batch rolls everything back.
pub struct Batch {
    tx: sqlx::Transaction<'static, Sqlite>,
    last: tokio::sync::OwnedMutexGuard<Option<u64>>,
    /// The sequence number before this batch's first event.
    first: u64,
    /// The sequence number of this batch's last event, or `first`.
    previous: u64,
    pending: Vec<EventEnvelope>,
    db: SqlitePool,
    clock: Clock,
    retention: u64,
    log: Arc<UserLog>,
    user: UserId,
}

impl std::fmt::Debug for Batch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Batch")
            .field("user", &self.user)
            .field("events", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl std::ops::Deref for Batch {
    type Target = SqliteConnection;

    fn deref(&self) -> &SqliteConnection {
        &self.tx
    }
}

impl std::ops::DerefMut for Batch {
    fn deref_mut(&mut self) -> &mut SqliteConnection {
        &mut self.tx
    }
}

impl Batch {
    async fn begin(
        db: SqlitePool,
        clock: Clock,
        retention: u64,
        log: Arc<UserLog>,
        user: UserId,
    ) -> Result<Self> {
        let mut last = log.last.clone().lock_owned().await;
        let cached = last.take();
        let mut tx = crate::db::write(&db).await?;
        let first = match cached {
            Some(previous) => previous,
            None => stored_last(&mut *tx, user).await?,
        };
        Ok(Self {
            tx,
            last,
            first,
            previous: first,
            pending: Vec::new(),
            db,
            clock,
            retention,
            log,
            user,
        })
    }

    pub fn user(&self) -> UserId {
        self.user
    }

    /// Adds an event to the log, numbered next. It is broadcast once the
    /// batch commits.
    pub async fn emit(&mut self, event: Event) -> Result<()> {
        let envelope = EventEnvelope {
            seq: Seq(self.previous + 1),
            at: self.clock.now(),
            event,
        };
        let body = serde_json::to_string(&envelope.event).map_err(Failure::internal)?;
        sqlx::query("INSERT INTO events (user_id, seq, at, body) VALUES (?, ?, ?, ?)")
            .bind(self.user.0)
            .bind(envelope.seq.0 as i64)
            .bind(envelope.at.0)
            .bind(body)
            .execute(&mut *self.tx)
            .await?;
        self.previous = envelope.seq.0;
        self.pending.push(envelope);
        Ok(())
    }

    /// Commits the change and its events, then broadcasts the events.
    ///
    /// The commit runs in its own task, so a caller dropped midway (a
    /// client disconnecting from a request, say) cannot leave events
    /// stored but not broadcast.
    pub async fn commit(self) -> Result<Vec<EventEnvelope>> {
        tokio::spawn(self.finish())
            .await
            .map_err(Failure::internal)?
    }

    async fn finish(self) -> Result<Vec<EventEnvelope>> {
        let Self {
            tx,
            mut last,
            first,
            previous,
            pending,
            db,
            retention,
            log,
            user,
            ..
        } = self;
        tx.commit().await?;
        *last = Some(previous);
        // Broadcast while still holding the lock, so live order is log
        // order. Nobody listening is fine.
        for envelope in &pending {
            let _ = log.live.send(envelope.clone());
        }
        drop(last);
        if previous / 256 > first / 256 && previous > retention {
            let pruned = sqlx::query("DELETE FROM events WHERE user_id = ? AND seq <= ?")
                .bind(user.0)
                .bind((previous - retention) as i64)
                .execute(&db)
                .await;
            if let Err(error) = pruned {
                tracing::warn!(user = %user, %error, "pruning the event log failed");
            }
        }
        Ok(pending)
    }
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
                    last: Default::default(),
                    live: broadcast::channel(self.capacity).0,
                })
            })
            .clone()
    }

    async fn stored_last(&self, user: UserId) -> Result<u64> {
        stored_last(&self.db, user).await
    }

    /// Opens a write transaction for a change to the user's data and the
    /// events that announce it. See [`Batch`].
    pub async fn begin(&self, user: UserId) -> Result<Batch> {
        Batch::begin(
            self.db.clone(),
            self.clock.clone(),
            self.retention,
            self.user(user),
            user,
        )
        .await
    }

    /// Appends one event to the user's log and broadcasts it, for an event
    /// that announces no database change.
    ///
    /// The work runs in its own task, so a caller dropped midway still
    /// leaves the event stored and broadcast, or neither.
    pub async fn append(&self, user: UserId, event: Event) -> Result<EventEnvelope> {
        let job = Batch::begin(
            self.db.clone(),
            self.clock.clone(),
            self.retention,
            self.user(user),
            user,
        );
        tokio::spawn(async move {
            let mut batch = job.await?;
            batch.emit(event).await?;
            let mut sent = batch.finish().await?;
            sent.pop()
                .ok_or_else(|| Failure::internal("no event appended"))
        })
        .await
        .map_err(Failure::internal)?
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
