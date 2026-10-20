//! Web Push: notifications on a user's subscribed devices.
//!
//! A push is queued in the transaction of the change that causes it (a
//! [`Batch`]), as a row in `pushes`, and the batch wakes the sender once it
//! commits. What causes one:
//!
//! - **A `notify` call outside a routine's run** ([`notified`]).
//! - **A routine's run ending** ([`run_ended`]). A run that succeeded pushes
//!   once: the Bot's `notify` text if it sent one in the run (a question
//!   before a result, the newest first), or else, unless the routine pushes
//!   only on `notify`, the first line of the run's reply. A run that failed
//!   or was interrupted pushes that, or that the routine paused, when this
//!   run paused it. A missed run pushes, except one missed because the
//!   account is disabled. A skipped run does not.
//!
//! The sender ([`Push::deliver_pending`]) settles each queued push in
//! order, and drops it, with its reason, when the account is disabled, the
//! Bot's conversation is on one of the user's devices
//! ([`Presence::is_shown`]), sending is not configured, the user has no
//! live subscription, the push waited longer than [`QUEUE_EXPIRY_MS`], or
//! the user has had [`HOURLY_LIMIT`] pushes in the past hour. Otherwise it
//! encrypts the payload (RFC 8291) for each of the user's subscriptions and
//! posts it to each push service through the gateway's [`HttpClient`] with
//! the cached VAPID token ([`vapid`]), retrying with backoff on 429 and
//! 5xx. A push service answering 404 or 410, or 403 with
//! `VapidPkHashMismatch`, ends the subscription.
//!
//! The payload is Stobie's declarative Web Push message, under
//! [`PAYLOAD_BYTES`]: the title, the body, the Bot's conversation as
//! `navigate`, a tag per Bot (so a device keeps the newest push for each
//! Bot), and the user's unread Bot count as `app_badge`. Every push shows a
//! notification: none is sent without a title and body.

pub mod subscription;
pub mod vapid;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use axum::http::header::{AUTHORIZATION, CONTENT_ENCODING, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method};
use bokkie_protocol::api::{NotificationKind, RoutinePush, RunState};
use bokkie_protocol::{BotId, NotificationId, RoutineRunId, Timestamp, UserId};
use bytes::Bytes;
use serde_json::json;
use sqlx::sqlite::SqliteConnection;
use sqlx::{Row, SqlitePool};

use self::subscription::Target;
use self::vapid::{Key, Tokens};
use crate::clock::{Clock, DAY_MS, HOUR_MS};
use crate::config::{Config, PublicOrigin};
use crate::error::{Failure, Result};
use crate::events::Batch;
use crate::http::{HttpClient, HttpError, Network, Request, Response};
use crate::presence::Presence;
use crate::routines::{reasons, store as runs};

/// At most this many pushes go to a user in an hour; more are dropped.
pub const HOURLY_LIMIT: i64 = 20;
/// How long a push service keeps a push for a device that is offline.
pub const TTL_SECONDS: u32 = 24 * 60 * 60;
/// A push not sent this long after it was queued is dropped.
pub const QUEUE_EXPIRY_MS: i64 = HOUR_MS;
/// How long settled pushes are kept.
pub const RETENTION_MS: i64 = DAY_MS;
/// The largest payload, in bytes, before encryption: Apple's limit is
/// 4096 after it, which adds about a hundred.
pub const PAYLOAD_BYTES: usize = 3_000;
/// The longest title, in characters.
pub const TITLE_CHARS: usize = 80;
/// The longest line taken from a reply, in characters.
pub const LINE_CHARS: usize = 200;
/// How long one request to a push service may take.
pub const DELIVERY_TIMEOUT: Duration = Duration::from_secs(30);
/// How much of a push service's answer is read.
const RESPONSE_BYTES: usize = 4_096;
/// How often the sender looks for queued pushes when nothing wakes it.
pub const SENDER_INTERVAL: Duration = Duration::from_secs(30);

/// What caused a push.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// A `notify` call outside a routine's run.
    Notify,
    /// A routine's run that succeeded.
    Run,
    /// A routine's run that failed or was interrupted.
    Failure,
    /// A routine's run that was missed.
    Miss,
    /// A routine that its failed runs paused.
    Pause,
}

impl Cause {
    pub fn name(self) -> &'static str {
        match self {
            Self::Notify => "notify",
            Self::Run => "run",
            Self::Failure => "failure",
            Self::Miss => "miss",
            Self::Pause => "pause",
        }
    }
}

/// How urgently a push service should deliver a push (RFC 8030).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Normal,
    /// A question the Bot needs answered.
    High,
}

impl Urgency {
    pub fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::High => "high",
        }
    }

    fn of(kind: NotificationKind) -> Self {
        match kind {
            NotificationKind::Question => Self::High,
            NotificationKind::Result => Self::Normal,
        }
    }
}

// ---------------------------------------------------------------------------
// Queueing

struct NewPush<'a> {
    user: UserId,
    bot: BotId,
    cause: Cause,
    notification: Option<NotificationId>,
    run: Option<RoutineRunId>,
    urgency: Urgency,
    title: &'a str,
    body: &'a str,
}

/// The name of the user's Bot, unless the account is disabled.
async fn bot_name(conn: &mut SqliteConnection, user: UserId, bot: BotId) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT b.name FROM bots b JOIN users u ON u.id = b.user_id
         WHERE b.id = ? AND b.user_id = ? AND u.disabled = 0",
    )
    .bind(bot.0)
    .bind(user.0)
    .fetch_optional(conn)
    .await?)
}

async fn queue(batch: &mut Batch, now: Timestamp, push: NewPush<'_>) -> Result<()> {
    let title = clean(push.title, TITLE_CHARS);
    let body = clean(push.body, usize::MAX);
    let done = sqlx::query(
        "INSERT INTO pushes (user_id, bot_id, cause, notification_id, routine_run_id, urgency,
                             title, body, created_at)
         SELECT b.user_id, b.id, ?, ?, ?, ?, ?, ?, ?
         FROM bots b JOIN users u ON u.id = b.user_id
         WHERE b.id = ? AND b.user_id = ? AND u.disabled = 0",
    )
    .bind(push.cause.name())
    .bind(push.notification.map(|n| n.0))
    .bind(push.run.map(|r| r.0))
    .bind(push.urgency.name())
    .bind(&title)
    .bind(&body)
    .bind(now.0)
    .bind(push.bot.0)
    .bind(push.user.0)
    .execute(&mut **batch)
    .await?;
    if done.rows_affected() > 0 {
        batch.wake_pushes();
    }
    Ok(())
}

/// The text a notification shows: its title, then its body.
fn notification_text(title: &str, body: &str) -> String {
    if body.is_empty() {
        title.to_owned()
    } else {
        format!("{title}\n{body}")
    }
}

/// Queues the push for a notification a Bot sent outside a routine's run.
/// A notification in a run's turn is left to the run's own push.
pub async fn notified(
    batch: &mut Batch,
    now: Timestamp,
    user: UserId,
    bot: BotId,
    notification: NotificationId,
    request: &crate::tools::notify::Request,
) -> Result<()> {
    let Some(name) = bot_name(&mut *batch, user, bot).await? else {
        return Ok(());
    };
    queue(
        batch,
        now,
        NewPush {
            user,
            bot,
            cause: Cause::Notify,
            notification: Some(notification),
            run: None,
            urgency: Urgency::of(request.kind),
            title: &name,
            body: &notification_text(&request.title, &request.body),
        },
    )
    .await
}

/// A notification a run's turn sent.
struct RunNotification {
    id: NotificationId,
    kind: NotificationKind,
    title: String,
    body: String,
}

/// The notification a run's push carries: its newest question, or else
/// its newest notification.
async fn run_notification(
    conn: &mut SqliteConnection,
    run: &runs::RunRow,
) -> Result<Option<RunNotification>> {
    let row = sqlx::query(
        "SELECT id, kind, title, body FROM notifications
         WHERE user_id = ? AND routine_run_id = ?
         ORDER BY kind = 'question' DESC, id DESC LIMIT 1",
    )
    .bind(run.user.0)
    .bind(run.id.0)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|row| RunNotification {
        id: NotificationId(row.get("id")),
        kind: crate::bots::store::notification_kind_from(row.get::<&str, _>("kind")),
        title: row.get("title"),
        body: row.get("body"),
    }))
}

/// The first line of the run's last reply, if it has one.
async fn reply_line(conn: &mut SqliteConnection, run: &runs::RunRow) -> Result<Option<String>> {
    let Some(turn) = &run.turn_id else {
        return Ok(None);
    };
    let text: Option<String> = sqlx::query_scalar(
        "SELECT text FROM messages
         WHERE user_id = ? AND bot_id = ? AND turn_id = ? AND author = 'bot' AND kind = 'text'
           AND trim(text) <> ''
         ORDER BY id DESC LIMIT 1",
    )
    .bind(run.user.0)
    .bind(run.bot.0)
    .bind(turn)
    .fetch_optional(conn)
    .await?;
    Ok(text.as_deref().and_then(first_line))
}

/// The first line of Markdown text with words in it, without its block
/// and emphasis markers, at most [`LINE_CHARS`].
pub fn first_line(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line
            .trim()
            .trim_start_matches(['#', '>', '-', '*', '+'])
            .replace("**", "")
            .replace("__", "")
            .replace('`', "");
        let line = clean(line.trim(), LINE_CHARS);
        (!line.is_empty()).then_some(line)
    })
}

/// A sentence, ending with a full stop unless it already ends a sentence.
fn sentence(text: &str) -> String {
    let text = text.trim();
    if text.ends_with(['.', '!', '?']) {
        text.to_owned()
    } else {
        format!("{text}.")
    }
}

/// Queues the push for a routine's run that just ended in `state`.
/// `paused` says this outcome paused the routine.
pub async fn run_ended(
    batch: &mut Batch,
    now: Timestamp,
    run: &runs::RunRow,
    state: RunState,
    paused: bool,
) -> Result<()> {
    let user = run.user;
    // A notification sent in the run's turn before the run was recorded
    // running had a push queued of its own: the run's push carries it now.
    sqlx::query(
        "UPDATE pushes SET state = 'merged', settled_at = ?1
         WHERE user_id = ?2 AND state = 'queued' AND cause = 'notify'
           AND notification_id IN (SELECT id FROM notifications
                                   WHERE user_id = ?2 AND routine_run_id = ?3)",
    )
    .bind(now.0)
    .bind(user.0)
    .bind(run.id.0)
    .execute(&mut **batch)
    .await?;
    let Some(name) = bot_name(&mut *batch, user, run.bot).await? else {
        return Ok(());
    };
    let Some(routine) = runs::routine_of_user(&mut **batch, user, run.routine).await? else {
        return Ok(());
    };
    let title = format!("{name} · {}", routine.name);
    let reason = run.reason.as_deref();
    let (cause, urgency, body, notification) = match state {
        RunState::Succeeded => match run_notification(&mut *batch, run).await? {
            Some(notification) => {
                // One that already went out alone is not sent again.
                let sent: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM pushes WHERE user_id = ? AND notification_id = ?
                                      AND cause = 'notify' AND state <> 'merged')",
                )
                .bind(user.0)
                .bind(notification.id.0)
                .fetch_one(&mut **batch)
                .await?;
                if sent {
                    return Ok(());
                }
                (
                    Cause::Run,
                    Urgency::of(notification.kind),
                    notification_text(&notification.title, &notification.body),
                    Some(notification.id),
                )
            }
            None if routine.push == RoutinePush::EveryRun => (
                Cause::Run,
                Urgency::Normal,
                reply_line(&mut *batch, run)
                    .await?
                    .unwrap_or_else(|| "Finished, with no reply.".into()),
                None,
            ),
            None => return Ok(()),
        },
        // Its owner stopped it themselves.
        RunState::Interrupted if reason == Some(reasons::STOPPED) => return Ok(()),
        RunState::Failed | RunState::Interrupted if paused => (
            Cause::Pause,
            Urgency::Normal,
            format!(
                "{} Paused after {} failed runs in a row.",
                sentence(reason.unwrap_or("The run did not finish")),
                routine.failures
            ),
            None,
        ),
        RunState::Failed | RunState::Interrupted => (
            Cause::Failure,
            Urgency::Normal,
            sentence(reason.unwrap_or("The run did not finish")),
            None,
        ),
        RunState::Missed if reason != Some(reasons::DISABLED) => (
            Cause::Miss,
            Urgency::Normal,
            format!(
                "Missed a run. {}",
                sentence(reason.unwrap_or("It did not run"))
            ),
            None,
        ),
        _ => return Ok(()),
    };
    queue(
        batch,
        now,
        NewPush {
            user,
            bot: run.bot,
            cause,
            notification,
            run: Some(run.id),
            urgency,
            title: &title,
            body: &body,
        },
    )
    .await
}

/// Text fit for a notification: no control or layout characters but line
/// breaks, at most `chars` characters, ending with an ellipsis when cut.
fn clean(text: &str, chars: usize) -> String {
    let kept: String = text
        .chars()
        .filter(|c| *c == '\n' || !(c.is_control() || crate::tools::notify::is_layout_control(*c)))
        .collect();
    let kept = kept.trim();
    if kept.chars().count() <= chars {
        return kept.to_owned();
    }
    let mut cut: String = kept.chars().take(chars.saturating_sub(1)).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

// ---------------------------------------------------------------------------
// The payload

/// Stobie's declarative Web Push message, at most [`PAYLOAD_BYTES`]: the
/// body is cut to fit.
pub fn payload(title: &str, body: &str, navigate: &str, tag: &str, badge: u32) -> Vec<u8> {
    let encode = |body: &str| {
        serde_json::to_vec(&json!({
            "web_push": 8030,
            "mutable": true,
            "notification": {
                "title": title,
                "body": body,
                "navigate": navigate,
                "tag": tag,
                "app_badge": badge,
            },
        }))
        .unwrap_or_default()
    };
    let whole = encode(body);
    if whole.len() <= PAYLOAD_BYTES {
        return whole;
    }
    // The longest cut body that fits.
    let chars: Vec<char> = body.chars().collect();
    let cut = |n: usize| {
        let mut text: String = chars[..n].iter().collect();
        text.truncate(text.trim_end().len());
        text.push('…');
        text
    };
    let (mut fits, mut too_long) = (0, chars.len());
    while too_long - fits > 1 {
        let middle = (fits + too_long) / 2;
        if encode(&cut(middle)).len() <= PAYLOAD_BYTES {
            fits = middle;
        } else {
            too_long = middle;
        }
    }
    encode(&cut(fits))
}

// ---------------------------------------------------------------------------
// The sender

/// How deliveries are retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    /// Retries after the first attempt.
    pub retries: u32,
    /// The first wait, doubled for each retry after it.
    pub backoff: Duration,
    /// The longest wait, whatever a push service's `Retry-After` asks.
    pub longest: Duration,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            retries: 3,
            backoff: Duration::from_secs(2),
            longest: Duration::from_secs(60),
        }
    }
}

/// How a push was settled without being sent, or that it is to be sent.
enum Decision {
    Send(Vec<Target>),
    Drop(&'static str),
}

/// A queued push.
#[derive(Debug, Clone)]
struct Queued {
    id: i64,
    user: UserId,
    bot: BotId,
    cause: String,
    notification: Option<i64>,
    urgency: Urgency,
    title: String,
    body: String,
    created_at: Timestamp,
}

/// What one attempt to deliver came to.
enum Attempt {
    Delivered,
    /// The subscription is gone for good.
    Gone(String),
    /// Refused, and not worth retrying.
    Refused(String),
    /// Worth retrying, after the push service's `Retry-After` if it gave
    /// one.
    Retry(Option<Duration>, String),
}

/// The deliveries in flight, which [`Push::settle`] waits for.
#[derive(Debug, Default)]
struct InFlight {
    count: Mutex<usize>,
    idle: tokio::sync::Notify,
}

struct Delivering(Arc<InFlight>);

impl Delivering {
    fn start(in_flight: &Arc<InFlight>) -> Self {
        *in_flight
            .count
            .lock()
            .unwrap_or_else(PoisonError::into_inner) += 1;
        Self(in_flight.clone())
    }
}

impl Drop for Delivering {
    fn drop(&mut self) {
        let mut count = self.0.count.lock().unwrap_or_else(PoisonError::into_inner);
        *count -= 1;
        if *count == 0 {
            self.0.idle.notify_waiters();
        }
    }
}

/// The Web Push service: the VAPID key, the routes' subscriptions and the
/// sender.
#[derive(Debug)]
pub struct Push {
    db: SqlitePool,
    clock: Clock,
    presence: Arc<Presence>,
    origin: PublicOrigin,
    contact: Option<String>,
    dir: PathBuf,
    key: Mutex<Option<Arc<Key>>>,
    tokens: Mutex<Tokens>,
    client: RwLock<Arc<HttpClient>>,
    retry: RwLock<Retry>,
    /// Woken by every batch that queues a push.
    queued: Arc<tokio::sync::Notify>,
    in_flight: Arc<InFlight>,
}

impl Push {
    pub fn new(
        config: &Config,
        db: SqlitePool,
        clock: Clock,
        presence: Arc<Presence>,
        queued: Arc<tokio::sync::Notify>,
    ) -> Self {
        Self {
            db,
            clock,
            presence,
            origin: config.origin.clone(),
            contact: config.push_contact.as_ref().map(|c| c.as_str().to_owned()),
            dir: vapid::dir(&config.data_dir),
            key: Mutex::default(),
            tokens: Mutex::default(),
            client: RwLock::new(Arc::new(HttpClient::new(Network::system()))),
            retry: RwLock::default(),
            queued,
            in_flight: Arc::default(),
        }
    }

    /// The VAPID key, read from the data directory, or generated there on
    /// first use.
    pub fn key(&self) -> std::result::Result<Arc<Key>, String> {
        let mut key = self.key.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(key) = &*key {
            return Ok(key.clone());
        }
        let loaded = Arc::new(Key::load_or_create(&self.dir)?);
        *key = Some(loaded.clone());
        Ok(loaded)
    }

    /// Whether pushes are sent: only with a contact configured.
    pub fn sending(&self) -> bool {
        self.contact.is_some()
    }

    fn client(&self) -> Arc<HttpClient> {
        self.client
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Sends through `network` from now on: the test seam for a mock push
    /// service. Only the `test-network` feature builds it.
    #[cfg(feature = "test-network")]
    pub fn use_network(&self, network: Network) {
        *self.client.write().unwrap_or_else(PoisonError::into_inner) =
            Arc::new(HttpClient::new(network));
    }

    /// Retries as `retry` says from now on, for tests.
    #[cfg(feature = "test-network")]
    pub fn use_retry(&self, retry: Retry) {
        *self.retry.write().unwrap_or_else(PoisonError::into_inner) = retry;
    }

    /// Settles pushes a stopped gateway left being delivered: whether they
    /// arrived is unknown, so they count as failed.
    pub async fn recover(&self) -> Result<()> {
        let done = sqlx::query(
            "UPDATE pushes SET state = 'failed', settled_at = ? WHERE state = 'sending'",
        )
        .bind(self.clock.now().0)
        .execute(&self.db)
        .await?;
        if done.rows_affected() > 0 {
            tracing::warn!(
                pushes = done.rows_affected(),
                "pushes were being delivered when the gateway stopped"
            );
        }
        Ok(())
    }

    /// Sends queued pushes whenever a batch queues one, and every `every`.
    pub async fn run(self: Arc<Self>, every: Duration) {
        if !self.sending() {
            tracing::warn!(
                "BOKKIE_PUSH_CONTACT is not set: the gateway serves its push key but sends no push"
            );
        }
        loop {
            if let Err(failure) = self.deliver_pending().await {
                tracing::error!(message = %failure.message, "sending pushes failed");
            }
            tokio::select! {
                _ = self.queued.notified() => {}
                _ = tokio::time::sleep(every) => {}
            }
        }
    }

    /// Waits until no delivery is in flight.
    pub async fn settle(&self) {
        loop {
            let idle = self.in_flight.idle.notified();
            tokio::pin!(idle);
            idle.as_mut().enable();
            if *self
                .in_flight
                .count
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                == 0
            {
                return;
            }
            idle.await;
        }
    }

    /// Settles every queued push: drops it with its reason, or starts
    /// delivering it. Deliveries go on in their own tasks.
    pub async fn deliver_pending(self: &Arc<Self>) -> Result<()> {
        let mut after = 0i64;
        loop {
            let rows = sqlx::query(
                "SELECT id, user_id, bot_id, cause, notification_id, urgency, title, body,
                        created_at
                 FROM pushes WHERE state = 'queued' AND id > ? ORDER BY id LIMIT 32",
            )
            .bind(after)
            .fetch_all(&self.db)
            .await?;
            if rows.is_empty() {
                return Ok(());
            }
            for row in &rows {
                let queued = Queued {
                    id: row.get("id"),
                    user: UserId(row.get("user_id")),
                    bot: BotId(row.get("bot_id")),
                    cause: row.get("cause"),
                    notification: row.get("notification_id"),
                    urgency: match row.get::<&str, _>("urgency") {
                        "high" => Urgency::High,
                        _ => Urgency::Normal,
                    },
                    title: row.get("title"),
                    body: row.get("body"),
                    created_at: Timestamp(row.get("created_at")),
                };
                after = queued.id;
                self.claim(queued).await?;
            }
        }
    }

    async fn decide(
        &self,
        conn: &mut SqliteConnection,
        now: Timestamp,
        push: &Queued,
    ) -> Result<Decision> {
        if now.0 - push.created_at.0 >= QUEUE_EXPIRY_MS {
            return Ok(Decision::Drop("expired"));
        }
        let disabled: Option<bool> = sqlx::query_scalar("SELECT disabled FROM users WHERE id = ?")
            .bind(push.user.0)
            .fetch_optional(&mut *conn)
            .await?;
        if disabled != Some(false) {
            return Ok(Decision::Drop("disabled"));
        }
        if push.cause == Cause::Notify.name()
            && let Some(notification) = push.notification
        {
            let in_run: Option<bool> = sqlx::query_scalar(
                "SELECT routine_run_id IS NOT NULL FROM notifications WHERE id = ? AND user_id = ?",
            )
            .bind(notification)
            .bind(push.user.0)
            .fetch_optional(&mut *conn)
            .await?;
            if in_run == Some(true) {
                return Ok(Decision::Drop("merged"));
            }
        }
        if self.presence.is_shown(push.user, push.bot) {
            return Ok(Decision::Drop("shown"));
        }
        if !self.sending() {
            return Ok(Decision::Drop("unconfigured"));
        }
        let targets = subscription::live(&mut *conn, now, push.user).await?;
        if targets.is_empty() {
            return Ok(Decision::Drop("unsubscribed"));
        }
        let recent: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pushes
             WHERE user_id = ? AND state IN ('sending', 'sent', 'failed') AND sent_at > ?",
        )
        .bind(push.user.0)
        .bind(now.0 - HOUR_MS)
        .fetch_one(&mut *conn)
        .await?;
        if recent >= HOURLY_LIMIT {
            return Ok(Decision::Drop("limited"));
        }
        Ok(Decision::Send(targets))
    }

    async fn claim(self: &Arc<Self>, push: Queued) -> Result<()> {
        let now = self.clock.now();
        let mut tx = crate::db::write(&self.db).await?;
        let decision = self.decide(&mut tx, now, &push).await?;
        let targets = match decision {
            Decision::Drop(state) => {
                sqlx::query(
                    "UPDATE pushes SET state = ?, settled_at = ? WHERE id = ? AND state = 'queued'",
                )
                .bind(state)
                .bind(now.0)
                .bind(push.id)
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                if state == "limited" {
                    tracing::warn!(user = %push.user, bot = %push.bot, push = push.id, cause = push.cause, limit = HOURLY_LIMIT, "dropped a push: the user's hourly limit is reached");
                } else {
                    tracing::info!(user = %push.user, bot = %push.bot, push = push.id, cause = push.cause, outcome = state, "did not send a push");
                }
                return Ok(());
            }
            Decision::Send(targets) => targets,
        };
        sqlx::query(
            "UPDATE pushes SET state = 'sending', sent_at = ? WHERE id = ? AND state = 'queued'",
        )
        .bind(now.0)
        .bind(push.id)
        .execute(&mut *tx)
        .await?;
        let badge = crate::bots::store::unread_bots(&mut *tx, push.user).await?;
        tx.commit().await?;
        let payload = Arc::new(payload(
            &push.title,
            &push.body,
            &format!("{}/?bot={}", self.origin.as_str(), push.bot),
            &format!("bot-{}", push.bot),
            badge,
        ));
        let delivering = Delivering::start(&self.in_flight);
        let this = self.clone();
        tokio::spawn(async move {
            this.deliver(&push, targets, payload).await;
            drop(delivering);
        });
        Ok(())
    }

    /// Delivers a claimed push to each of its subscriptions at once, and
    /// records whether any took it.
    async fn deliver(self: &Arc<Self>, push: &Queued, targets: Vec<Target>, payload: Arc<Vec<u8>>) {
        let devices = targets.len();
        let mut deliveries = tokio::task::JoinSet::new();
        for target in targets {
            let this = self.clone();
            let payload = payload.clone();
            let (user, urgency) = (push.user, push.urgency);
            deliveries
                .spawn(async move { this.deliver_to(user, &target, &payload, urgency).await });
        }
        let mut delivered = 0;
        while let Some(done) = deliveries.join_next().await {
            if matches!(done, Ok(true)) {
                delivered += 1;
            }
        }
        let state = if delivered > 0 { "sent" } else { "failed" };
        let settled = sqlx::query(
            "UPDATE pushes SET state = ?, settled_at = ? WHERE id = ? AND state = 'sending'",
        )
        .bind(state)
        .bind(self.clock.now().0)
        .bind(push.id)
        .execute(&self.db)
        .await;
        if let Err(error) = settled {
            tracing::error!(push = push.id, %error, "recording a push's delivery failed");
        }
        tracing::info!(user = %push.user, bot = %push.bot, push = push.id, cause = push.cause, delivered, devices, "sent a push");
    }

    /// Delivers to one subscription, retrying as [`Retry`] says. Returns
    /// whether the push service took it.
    async fn deliver_to(
        &self,
        user: UserId,
        target: &Target,
        payload: &[u8],
        urgency: Urgency,
    ) -> bool {
        let service = target.endpoint.host_str().unwrap_or_default().to_owned();
        let retry = *self.retry.read().unwrap_or_else(PoisonError::into_inner);
        let mut attempt = 0u32;
        loop {
            let wait = match self.attempt(target, payload, urgency).await {
                Attempt::Delivered => return true,
                Attempt::Gone(why) => {
                    match subscription::remove(&self.db, target).await {
                        Ok(_) => {
                            tracing::info!(user = %user, %service, %why, "a push subscription ended")
                        }
                        Err(failure) => {
                            tracing::error!(user = %user, message = %failure.message, "removing a push subscription failed")
                        }
                    }
                    return false;
                }
                Attempt::Refused(why) => {
                    tracing::warn!(user = %user, %service, %why, "a push service refused a push");
                    return false;
                }
                Attempt::Retry(after, why) => {
                    if attempt >= retry.retries {
                        tracing::warn!(user = %user, %service, %why, attempts = attempt + 1, "gave up delivering a push");
                        return false;
                    }
                    tracing::info!(user = %user, %service, %why, "retrying a push");
                    after
                        .unwrap_or_else(|| retry.backoff.saturating_mul(1 << attempt.min(16)))
                        .min(retry.longest)
                }
            };
            tokio::time::sleep(wait).await;
            attempt += 1;
        }
    }

    async fn attempt(&self, target: &Target, payload: &[u8], urgency: Urgency) -> Attempt {
        let Some(contact) = &self.contact else {
            return Attempt::Refused("sending is not configured".into());
        };
        let key = match self.key() {
            Ok(key) => key,
            Err(error) => return Attempt::Refused(error),
        };
        let audience = target.endpoint.origin().ascii_serialization();
        let authorization = self
            .tokens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .authorization(&key, &audience, contact, self.clock.now());
        let Ok(device) = p256::PublicKey::from_sec1_bytes(&target.p256dh) else {
            return Attempt::Gone("its stored key is not a P-256 key".into());
        };
        if target.auth.len() != 16 {
            return Attempt::Gone("its stored secret is not 16 bytes".into());
        }
        let secret = web_push_native::Auth::clone_from_slice(&target.auth);
        let body = match web_push_native::encrypt(payload.to_vec(), &device, &secret) {
            Ok(body) => body,
            Err(error) => return Attempt::Refused(format!("encrypting failed: {error}")),
        };
        let Ok(authorization) = HeaderValue::from_str(&authorization) else {
            return Attempt::Refused("the VAPID header is not a header value".into());
        };
        let request = Request {
            method: Method::POST,
            url: target.endpoint.clone(),
            headers: vec![
                (
                    HeaderName::from_static("ttl"),
                    HeaderValue::from(TTL_SECONDS),
                ),
                (
                    HeaderName::from_static("urgency"),
                    HeaderValue::from_static(urgency.name()),
                ),
                (CONTENT_ENCODING, HeaderValue::from_static("aes128gcm")),
                (
                    CONTENT_TYPE,
                    HeaderValue::from_static("application/octet-stream"),
                ),
                (AUTHORIZATION, authorization),
            ],
            body: Bytes::from(body),
        };
        match tokio::time::timeout(
            DELIVERY_TIMEOUT,
            self.client().send(request, RESPONSE_BYTES),
        )
        .await
        {
            Err(_) => Attempt::Retry(None, "timed out".into()),
            Ok(Err(error @ (HttpError::InvalidUrl(_) | HttpError::NotPublic(_)))) => {
                Attempt::Refused(error.to_string())
            }
            Ok(Err(error)) => Attempt::Retry(None, error.to_string()),
            Ok(Ok(response)) => classify(&response),
        }
    }
}

/// What a push service's answer means.
fn classify(response: &Response) -> Attempt {
    let status = response.status.as_u16();
    let why = format!("{status} {}", reason(&response.body));
    match status {
        200..=299 => Attempt::Delivered,
        404 | 410 => Attempt::Gone(why),
        // The subscription was made for another key: it can never work.
        403 if why.contains("VapidPkHashMismatch") => Attempt::Gone(why),
        429 | 500..=599 => Attempt::Retry(retry_after(&response.headers), why),
        _ => Attempt::Refused(why),
    }
}

/// A push service's reason for its answer, such as Apple's
/// `{"reason":"BadJwtToken"}`, bounded and printable.
fn reason(body: &[u8]) -> String {
    let text = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["reason"].as_str().map(str::to_owned))
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    text.chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// A `Retry-After` in seconds.
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get("retry-after")?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Deletes settled pushes older than [`RETENTION_MS`], and subscriptions
/// whose sessions expired. Returns how many pushes.
pub async fn prune(conn: &mut SqliteConnection, now: Timestamp) -> Result<u64> {
    subscription::prune(&mut *conn, now).await?;
    let done = sqlx::query(
        "DELETE FROM pushes WHERE created_at <= ? AND state NOT IN ('queued', 'sending')",
    )
    .bind(now.0 - RETENTION_MS)
    .execute(conn)
    .await
    .map_err(Failure::from)?;
    Ok(done.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_payload_is_stobies_declarative_message_within_the_limit() {
        let encoded = payload(
            "Chef",
            "Dinner plan ready",
            "https://b.test/?bot=7",
            "bot-7",
            2,
        );
        let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(
            value,
            json!({"web_push": 8030, "mutable": true, "notification": {
                "title": "Chef", "body": "Dinner plan ready", "navigate": "https://b.test/?bot=7",
                "tag": "bot-7", "app_badge": 2}})
        );
        // A body too long for the limit is cut to fit, with an ellipsis.
        for body in [
            "x".repeat(10_000),
            "a\n".repeat(3_000),
            "\"".repeat(3_000),
            "é".repeat(4_000),
            "😀".repeat(2_000),
        ] {
            let encoded = payload("Chef", &body, "https://b.test/?bot=7", "bot-7", 1);
            assert!(encoded.len() <= PAYLOAD_BYTES, "{}", encoded.len());
            assert!(encoded.len() > PAYLOAD_BYTES - 16, "{}", encoded.len());
            let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
            assert!(
                value["notification"]["body"]
                    .as_str()
                    .unwrap()
                    .ends_with('…')
            );
        }
    }

    #[test]
    fn the_first_line_of_a_reply_drops_markdown_and_is_bounded() {
        assert_eq!(
            first_line("\n\n## **Today**: rain\nmore").as_deref(),
            Some("Today: rain")
        );
        assert_eq!(first_line("- `one` item").as_deref(), Some("one item"));
        assert_eq!(
            first_line("> quoted\u{202e}evil").as_deref(),
            Some("quotedevil")
        );
        assert_eq!(first_line("  \n---\n\nWords").as_deref(), Some("Words"));
        assert_eq!(first_line(" \n\t\n"), None);
        let long = first_line(&"w".repeat(1_000)).unwrap();
        assert_eq!(long.chars().count(), LINE_CHARS);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn notification_text_is_cleaned_and_bounded() {
        assert_eq!(clean(" a\u{7}b\u{2028}c\u{202a}\n d ", 80), "abc\n d");
        assert_eq!(clean("abcdef", 4), "abc…");
        assert_eq!(sentence("Done"), "Done.");
        assert_eq!(sentence("Done?"), "Done?");
    }

    #[test]
    fn push_service_answers_are_classified() {
        let answer = |status: u16, body: &str, retry: Option<&str>| {
            let mut headers = HeaderMap::new();
            if let Some(retry) = retry {
                headers.insert("retry-after", HeaderValue::from_str(retry).unwrap());
            }
            classify(&Response {
                status: axum::http::StatusCode::from_u16(status).unwrap(),
                headers,
                body: Bytes::from(body.to_owned()),
                truncated: false,
                address: "127.0.0.1:443".parse().unwrap(),
            })
        };
        assert!(matches!(answer(201, "", None), Attempt::Delivered));
        assert!(matches!(answer(410, "", None), Attempt::Gone(_)));
        assert!(matches!(answer(404, "", None), Attempt::Gone(_)));
        assert!(matches!(
            answer(403, r#"{"reason":"VapidPkHashMismatch"}"#, None),
            Attempt::Gone(_)
        ));
        match answer(403, r#"{"reason":"BadJwtToken"}"#, None) {
            Attempt::Refused(why) => assert_eq!(why, "403 BadJwtToken"),
            _ => panic!("BadJwtToken is refused"),
        }
        assert!(matches!(answer(413, "", None), Attempt::Refused(_)));
        assert!(matches!(answer(400, "", None), Attempt::Refused(_)));
        match answer(429, "", Some("7")) {
            Attempt::Retry(after, _) => assert_eq!(after, Some(Duration::from_secs(7))),
            _ => panic!("429 is retried"),
        }
        assert!(matches!(
            answer(503, "", Some("soon")),
            Attempt::Retry(None, _)
        ));
        assert!(matches!(answer(500, "", None), Attempt::Retry(None, _)));
    }
}
