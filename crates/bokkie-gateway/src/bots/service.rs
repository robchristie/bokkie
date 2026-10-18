//! Conversations: sending to a Bot, stopping it, and turning what its app
//! server reports into stored messages.
//!
//! A Bot maps to one Codex thread. The first message starts the thread, with
//! the Bot's profile in its developer instructions. Codex fixes developer
//! instructions when a thread starts (0.163.0 ignores them on
//! `thread/resume`), so a later change to the profile reaches the model as
//! a `bokkie-profile` context fragment on the next turn.
//!
//! A message sent while the Bot is mid-turn steers that turn. "Stop" maps to
//! `turn/interrupt`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use bokkie_codex::protocol::{
    AdditionalContextEntry, AdditionalContextKind, ServerNotification, ThreadItem, TurnStatus,
};
use bokkie_protocol::api::{
    Allowance, Author, Delivery, ErrorCode, Message, MessageState, SendMessageResponse, Usage,
};
use bokkie_protocol::events::Event;
use bokkie_protocol::{BotId, MessageId, Timestamp, UserId};
use sqlx::SqlitePool;
use tokio::sync::mpsc;

use super::store::{self, BotRow, NewMessage};
use crate::accounts::Account;
use crate::clock::Clock;
use crate::codex::{CodexError, RuntimeEvent, RuntimeEventKind, Supervisor};
use crate::error::{Failure, Result};
use crate::events::{Batch, EventLog};
use crate::usage::{self, SubscriptionCache};

/// How often streamed text is written out. Deltas arriving in between are
/// coalesced into one write.
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(100);

/// How often the gateway looks for allowance and subscription windows that
/// rolled over.
pub const ROLLOVER_INTERVAL: Duration = Duration::from_secs(60);

/// How long reading a user's subscription windows after a reset may take.
const RATE_LIMITS_TIMEOUT: Duration = Duration::from_secs(10);

/// The key of the profile context fragment.
pub const PROFILE_CONTEXT: &str = "bokkie-profile";

/// The developer instructions a Bot's thread starts with.
pub fn instructions(owner: &str, bot: &BotRow) -> String {
    format!(
        "You are {name}, a Bot in Bokkie, a private household assistant. You work for {owner}.\n\
         Your job: {label}\n\n\
         The rules {owner} set for you:\n{rules}\n\n\
         You can only talk in this conversation. You cannot run commands, read or change \
         files, browse the web or contact anyone. If you are asked to, say that you cannot \
         do that yet.\n\
         If a later <{PROFILE_CONTEXT}> message gives you a new name, job or rules, follow \
         the latest one.",
        name = bot.name,
        label = if bot.label.is_empty() {
            "general help"
        } else {
            &bot.label
        },
        rules = if bot.description.is_empty() {
            "(none)"
        } else {
            &bot.description
        },
    )
}

/// The context fragment that carries a changed profile.
pub fn profile_update(bot: &BotRow) -> String {
    format!(
        "Your profile changed. You are now {name}. Your job: {label}\n\nYour rules:\n{rules}",
        name = bot.name,
        label = if bot.label.is_empty() {
            "general help"
        } else {
            &bot.label
        },
        rules = if bot.description.is_empty() {
            "(none)"
        } else {
            &bot.description
        },
    )
}

fn context(text: String) -> BTreeMap<String, AdditionalContextEntry> {
    [(
        PROFILE_CONTEXT.to_owned(),
        AdditionalContextEntry {
            kind: AdditionalContextKind::Application,
            value: text,
        },
    )]
    .into()
}

#[derive(Debug, Clone)]
struct ActiveTurn {
    user: UserId,
    thread_id: String,
    turn_id: String,
}

#[derive(Debug)]
struct Reply {
    user: UserId,
    bot: BotId,
    message: MessageId,
    /// Streamed text not yet written out.
    pending: String,
    /// How much text is written out, in `char`s: the next delta's offset.
    written: u64,
}

#[derive(Debug, Default)]
struct Live {
    /// The running turn of each Bot.
    turns: HashMap<BotId, ActiveTurn>,
    /// Bot replies still streaming, by the user, thread and Codex item id.
    /// The item id alone is the model's message id, which a provider need
    /// not keep unique across threads.
    replies: HashMap<(UserId, String, String), Reply>,
    /// Recently finished turns. A turn can finish before `turn/start`
    /// answers, and must not then be recorded as running.
    finished: std::collections::VecDeque<String>,
    /// Turns interrupted because the user's token allowance ran out, by the
    /// user and Codex turn id, until they end.
    token_stops: std::collections::HashSet<(UserId, String)>,
}

impl Live {
    const FINISHED_KEPT: usize = 256;

    fn finished(&mut self, turn_id: &str) {
        if self.finished.len() == Self::FINISHED_KEPT {
            self.finished.pop_front();
        }
        self.finished.push_back(turn_id.to_owned());
    }
}

/// Held while a message to one Bot is sent.
type SendLock = Arc<tokio::sync::Mutex<()>>;

/// The conversation service shared by the routes and the event router.
#[derive(Debug)]
pub struct Conversations {
    db: SqlitePool,
    clock: Clock,
    supervisor: Arc<Supervisor>,
    events: Arc<EventLog>,
    subscription: SubscriptionCache,
    live: std::sync::Mutex<Live>,
    /// Serialises the sends to each Bot.
    sends: std::sync::Mutex<HashMap<(UserId, BotId), SendLock>>,
}

fn limit_reached() -> Failure {
    Failure::new(
        ErrorCode::LimitReached,
        "You have used your allowance for now. It refills over the next day or week",
    )
}

fn unavailable(error: CodexError) -> Failure {
    match error {
        CodexError::Refused(_) | CodexError::ThreadRefused(_) => Failure::unavailable(
            "This Bot is unavailable because a safety check failed. An admin has been told",
        ),
        CodexError::AppServer(_) | CodexError::Rejected(_) => {
            Failure::unavailable("The Bots' runtime is unavailable")
        }
    }
}

impl Conversations {
    pub fn new(
        db: SqlitePool,
        clock: Clock,
        supervisor: Arc<Supervisor>,
        events: Arc<EventLog>,
    ) -> Self {
        Self {
            db,
            clock,
            supervisor,
            events,
            subscription: Default::default(),
            live: Default::default(),
            sends: Default::default(),
        }
    }

    fn live(&self) -> std::sync::MutexGuard<'_, Live> {
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Appends an event that announces no database change. A failure, which
    /// means the database failed, is logged: the event is lost, and
    /// sequence numbers stay contiguous.
    async fn emit(&self, user: UserId, event: Event) {
        if let Err(failure) = self.events.append(user, event).await {
            tracing::error!(user = %user, message = %failure.message, "appending an event failed");
        }
    }

    /// Commits a batch from the event router, where nobody waits on the
    /// result: a failure is logged, and the change and its events are both
    /// rolled back.
    async fn commit(&self, batch: Batch) {
        let user = batch.user();
        if let Err(failure) = batch.commit().await {
            tracing::error!(user = %user, message = %failure.message, "committing a change failed");
        }
    }

    pub fn is_working(&self, bot: BotId) -> bool {
        self.live().turns.contains_key(&bot)
    }

    fn send_lock(&self, user: UserId, bot: BotId) -> SendLock {
        self.sends
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry((user, bot))
            .or_default()
            .clone()
    }

    /// Marks replies left streaming by a previous gateway run as failed.
    pub async fn recover(&self) -> Result<()> {
        let left: Vec<(i64, i64)> =
            sqlx::query_as("SELECT id, user_id FROM messages WHERE state = 'streaming'")
                .fetch_all(&self.db)
                .await?;
        for (id, user) in &left {
            let user = UserId(*user);
            let mut batch = self.events.begin(user).await?;
            if let Some(message) = store::finish_message(
                &mut *batch,
                self.clock.now(),
                user,
                MessageId(*id),
                None,
                MessageState::Failed,
            )
            .await?
            {
                batch.emit(Event::MessageCompleted { message }).await?;
            }
            batch.commit().await?;
        }
        if !left.is_empty() {
            tracing::warn!(count = left.len(), "marked interrupted replies as failed");
        }
        Ok(())
    }

    /// Sends a user's message to one of their Bots.
    pub async fn send(
        &self,
        owner: &Account,
        bot_id: BotId,
        text: &str,
    ) -> Result<SendMessageResponse> {
        let user = owner.user.id;
        let text = store::check_message(text)?;
        // One send per Bot at a time, so that a Bot's thread starts once and
        // each message either steers its running turn or starts one. Sends
        // to other Bots do not wait, even on a Codex call that hangs: the
        // allowance is checked and counted in one transaction instead.
        let lock = self.send_lock(user, bot_id);
        let _sending = lock.lock().await;
        let bot = store::bot(&self.db, user, bot_id)
            .await?
            .ok_or_else(Failure::not_found)?;
        if bot.refused_reason.is_some() {
            return Err(unavailable(CodexError::Refused(
                crate::codex::guard::Refusal::new("refused"),
            )));
        }
        // The user's own allowance. A new turn needs both turns and tokens
        // left; steering a running turn needs only tokens.
        let allowance = self.allowance(user).await?;
        let active = self.live().turns.get(&bot.id).cloned();
        if tokens_used_up(&allowance) || (active.is_none() && allowance.exhausted) {
            return Err(limit_reached());
        }
        let runtime = self.supervisor.runtime(user).await.map_err(unavailable)?;

        // Make sure the Bot's thread exists and passed its checks here.
        let thread_id = match &bot.thread_id {
            None => {
                let started = runtime
                    .start_thread(instructions(&owner.user.display_name, &bot), None)
                    .await;
                let started = self.refusing(user, &bot, started).await?;
                store::set_thread(
                    &self.db,
                    user,
                    bot.id,
                    &started.thread.id,
                    bot.profile_revision,
                )
                .await?;
                started.thread.id
            }
            Some(thread_id) => {
                if !runtime.is_served(thread_id) {
                    let resumed = runtime
                        .resume_thread(thread_id, instructions(&owner.user.display_name, &bot))
                        .await;
                    self.refusing(user, &bot, resumed).await?;
                }
                thread_id.clone()
            }
        };
        let bot = store::bot(&self.db, user, bot_id)
            .await?
            .ok_or_else(Failure::not_found)?;
        let profile = (bot.profile_sent_revision < bot.profile_revision)
            .then(|| context(profile_update(&bot)));

        // A message that starts a turn counts it before it is stored, so
        // that of several sent at once, only those with a turn left are
        // stored. Counting before Codex starts the turn also means a turn
        // that runs is always counted.
        let reserved = match active {
            Some(_) => None,
            None => Some(
                self.reserve_turn(user, bot.id)
                    .await?
                    .ok_or_else(limit_reached)?,
            ),
        };
        let message = match self.store_message(user, bot.id, &text).await {
            Ok(message) => message,
            Err(failure) => {
                if let Some(turn) = reserved {
                    self.release_turn(user, turn).await;
                }
                return Err(failure);
            }
        };

        // Steer the running turn, if there is one.
        if let Some(active) = active {
            match runtime
                .steer_turn(&thread_id, &active.turn_id, &text, profile.clone())
                .await
            {
                Ok(_) => {
                    store::set_profile_sent(&self.db, user, bot.id, bot.profile_revision).await?;
                    return Ok(SendMessageResponse {
                        message,
                        delivery: Delivery::Steered,
                    });
                }
                // The turn ended in the meantime: start a new one instead.
                Err(CodexError::AppServer(error) | CodexError::Rejected(error)) => {
                    tracing::info!(bot = %bot.id, %error, "steer missed the turn; starting one");
                }
                Err(refused @ (CodexError::Refused(_) | CodexError::ThreadRefused(_))) => {
                    return Err(self
                        .refusing::<()>(user, &bot, Err(refused))
                        .await
                        .unwrap_err());
                }
            }
        }
        // If the steer missed and there is no turn left for a new one, the
        // message stays without a reply.
        let reserved = match reserved {
            Some(turn) => turn,
            None => self
                .reserve_turn(user, bot.id)
                .await?
                .ok_or_else(limit_reached)?,
        };
        let turn = match runtime.start_turn(&thread_id, &text, profile).await {
            Ok(turn) => turn,
            Err(error) => {
                // Only a turn Codex certainly did not start is taken back.
                if error.had_no_effect() {
                    self.release_turn(user, reserved).await;
                }
                return Err(self
                    .refusing::<()>(user, &bot, Err(error))
                    .await
                    .unwrap_err());
            }
        };
        store::set_profile_sent(&self.db, user, bot.id, bot.profile_revision).await?;
        // Whichever of this and the router's `turn/started` records the turn
        // first announces that the Bot is working.
        let started = {
            let mut live = self.live();
            !live.finished.contains(&turn.id)
                && live
                    .turns
                    .insert(
                        bot.id,
                        ActiveTurn {
                            user,
                            thread_id,
                            turn_id: turn.id,
                        },
                    )
                    .is_none()
        };
        if started {
            self.status(user, bot.id).await;
        }
        Ok(SendMessageResponse {
            message,
            delivery: Delivery::NewTurn,
        })
    }

    /// Stores a user's message and announces it.
    async fn store_message(&self, user: UserId, bot: BotId, text: &str) -> Result<Message> {
        let mut batch = self.events.begin(user).await?;
        let message = store::insert_message(
            &mut *batch,
            self.clock.now(),
            user,
            NewMessage {
                bot,
                author: Author::User,
                text,
                state: MessageState::Complete,
                turn: None,
            },
        )
        .await?;
        batch
            .emit(Event::MessageCreated {
                message: message.clone(),
            })
            .await?;
        batch.commit().await?;
        Ok(message)
    }

    /// Passes a result through, recording a safety refusal on the Bot.
    async fn refusing<T>(
        &self,
        user: UserId,
        bot: &BotRow,
        result: std::result::Result<T, CodexError>,
    ) -> Result<T> {
        match result {
            Ok(value) => Ok(value),
            // Only a refusal of this Bot's own thread is recorded on the
            // Bot. A refusal of the app server or the whole gateway is not
            // the Bot's, and must not outlive it.
            Err(CodexError::ThreadRefused(refusal)) => {
                tracing::error!(user = %user, bot = %bot.id, %refusal, "refusing a Bot");
                let mut batch = self.events.begin(user).await?;
                store::set_refused(&mut *batch, self.clock.now(), user, bot.id, &refusal.0).await?;
                self.emit_status(&mut batch, bot.id).await?;
                batch.commit().await?;
                Err(unavailable(CodexError::ThreadRefused(refusal)))
            }
            Err(error) => {
                tracing::warn!(user = %user, bot = %bot.id, %error, "Codex failed");
                Err(unavailable(error))
            }
        }
    }

    /// Interrupts the Bot's running turn. Returns whether one was running.
    pub async fn stop(&self, user: UserId, bot_id: BotId) -> Result<bool> {
        // The Bot must be the user's own.
        store::bot(&self.db, user, bot_id)
            .await?
            .ok_or_else(Failure::not_found)?;
        let Some(active) = self.live().turns.get(&bot_id).cloned() else {
            return Ok(false);
        };
        let Some(runtime) = self.supervisor.running(user).await else {
            return Ok(false);
        };
        runtime
            .interrupt_turn(&active.thread_id, &active.turn_id)
            .await
            .map_err(unavailable)?;
        Ok(true)
    }

    // -- Rollover -----------------------------------------------------------

    /// Announces rolled-over windows at each interval ([`ROLLOVER_INTERVAL`]
    /// in the gateway), until the gateway stops.
    pub async fn run_rollovers(self: Arc<Self>, every: Duration) {
        let mut since = self.clock.now();
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let now = self.clock.now();
            // A failed pass is retried from the same start, so nothing that
            // rolled over in it goes unannounced.
            match self.roll_over(since, now).await {
                Ok(_) => since = now,
                Err(failure) => {
                    tracing::error!(message = %failure.message, "rolling usage windows over failed");
                }
            }
        }
    }

    /// Announces the usage meter of every user with usage that left an
    /// allowance window, or with a subscription window that reset, in
    /// `(since, now]`. A reset subscription is read again from the user's
    /// app server if it is running. Then deletes usage that no window
    /// counts any more. Returns the users announced.
    pub async fn roll_over(&self, since: Timestamp, now: Timestamp) -> Result<Vec<UserId>> {
        let mut users = usage::rolled_over(&self.db, since, now).await?;
        for user in self.subscription.resetting(since, now) {
            if let Some(runtime) = self.supervisor.running(user).await {
                match tokio::time::timeout(RATE_LIMITS_TIMEOUT, runtime.rate_limits()).await {
                    Ok(Ok(response)) => self.subscription.set(user, usage::buckets(&response)),
                    Ok(Err(error)) => {
                        tracing::warn!(user = %user, %error, "reading rate limits failed");
                    }
                    Err(_) => tracing::warn!(user = %user, "reading rate limits timed out"),
                }
            }
            users.push(user);
        }
        users.sort();
        users.dedup();
        for user in &users {
            self.usage_changed(*user).await;
        }
        let mut tx = crate::db::write(&self.db).await?;
        usage::prune(&mut tx, now).await?;
        tx.commit().await?;
        Ok(users)
    }

    // -- Routing ------------------------------------------------------------

    /// Routes app-server events until the stream ends, writing streamed
    /// text out every [`FLUSH_INTERVAL`].
    pub async fn run(self: Arc<Self>, mut events: mpsc::UnboundedReceiver<RuntimeEvent>) {
        let mut tick = tokio::time::interval(FLUSH_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Some(event) => {
                        if let Err(failure) = self.handle(event).await {
                            tracing::error!(message = %failure.message, "routing an app-server event failed");
                        }
                    }
                    None => break,
                },
                _ = tick.tick() => {
                    if let Err(failure) = self.flush().await {
                        tracing::error!(message = %failure.message, "writing streamed text failed");
                    }
                }
            }
        }
    }

    /// Writes out every reply's pending streamed text.
    pub async fn flush(&self) -> Result<()> {
        let pending: Vec<(UserId, BotId, MessageId, u64, String)> = self
            .live()
            .replies
            .values_mut()
            .filter(|r| !r.pending.is_empty())
            .map(|r| {
                let text = std::mem::take(&mut r.pending);
                let offset = r.written;
                r.written += text.chars().count() as u64;
                (r.user, r.bot, r.message, offset, text)
            })
            .collect();
        for (user, bot_id, message_id, offset, delta) in pending {
            let mut batch = self.events.begin(user).await?;
            store::append_text(&mut *batch, self.clock.now(), user, message_id, &delta).await?;
            batch
                .emit(Event::MessageDelta {
                    bot_id,
                    message_id,
                    offset,
                    delta,
                })
                .await?;
            batch.commit().await?;
        }
        Ok(())
    }

    /// Handles one event from a user's app server. Every lookup is scoped to
    /// that user, so one user's app server cannot touch another's Bots.
    pub async fn handle(&self, event: RuntimeEvent) -> Result<()> {
        let user = event.user;
        match event.kind {
            RuntimeEventKind::Notification(notification) => match notification {
                ServerNotification::TurnStarted(started) => {
                    if let Some(bot) =
                        store::bot_by_thread(&self.db, user, &started.thread_id).await?
                    {
                        let was_idle = self
                            .live()
                            .turns
                            .insert(
                                bot.id,
                                ActiveTurn {
                                    user,
                                    thread_id: started.thread_id,
                                    turn_id: started.turn.id,
                                },
                            )
                            .is_none();
                        if was_idle {
                            self.status(user, bot.id).await;
                        }
                    }
                }
                ServerNotification::ItemStarted(item) => {
                    if let ThreadItem::AgentMessage { id, text } = &item.item {
                        self.reply(user, &item.thread_id, &item.turn_id, id, text)
                            .await?;
                    }
                }
                ServerNotification::AgentMessageDelta(delta) => {
                    let known = self.live().replies.contains_key(&(
                        user,
                        delta.thread_id.clone(),
                        delta.item_id.clone(),
                    ));
                    if !known {
                        self.reply(user, &delta.thread_id, &delta.turn_id, &delta.item_id, "")
                            .await?;
                    }
                    if let Some(reply) =
                        self.live()
                            .replies
                            .get_mut(&(user, delta.thread_id, delta.item_id))
                    {
                        reply.pending.push_str(&delta.delta);
                    }
                }
                ServerNotification::ItemCompleted(item) => {
                    if let ThreadItem::AgentMessage { id, text } = &item.item {
                        let reply =
                            self.live()
                                .replies
                                .remove(&(user, item.thread_id.clone(), id.clone()));
                        if let Some(reply) = reply {
                            // The final text replaces whatever streamed.
                            let mut batch = self.events.begin(user).await?;
                            let message = store::finish_message(
                                &mut *batch,
                                self.clock.now(),
                                user,
                                reply.message,
                                Some(text),
                                MessageState::Complete,
                            )
                            .await?;
                            if let Some(message) = message {
                                batch.emit(Event::MessageCompleted { message }).await?;
                            }
                            self.commit(batch).await;
                        }
                    }
                }
                ServerNotification::TurnCompleted(done) => {
                    if let Some(bot) = store::bot_by_thread(&self.db, user, &done.thread_id).await?
                    {
                        let (ended, stopped_for_tokens) = {
                            let mut live = self.live();
                            live.finished(&done.turn.id);
                            let stopped_for_tokens =
                                live.token_stops.remove(&(user, done.turn.id.clone()));
                            let current = live
                                .turns
                                .get(&bot.id)
                                .is_some_and(|t| t.turn_id == done.turn.id);
                            if current {
                                live.turns.remove(&bot.id);
                            }
                            (current, stopped_for_tokens)
                        };
                        let state = match done.turn.status {
                            TurnStatus::Completed | TurnStatus::InProgress => {
                                MessageState::Complete
                            }
                            TurnStatus::Interrupted => MessageState::Interrupted,
                            TurnStatus::Failed => MessageState::Failed,
                        };
                        self.finish_streaming(user, Some(bot.id), state).await?;
                        // Only a turn the token stop actually cut short is
                        // announced as stopped. A chat-only turn usually
                        // reports its tokens after its reply is complete.
                        if stopped_for_tokens && done.turn.status == TurnStatus::Interrupted {
                            self.emit(
                                user,
                                Event::BotError {
                                    bot_id: bot.id,
                                    message: "Your token allowance is used up, so this turn was \
                                              stopped"
                                        .into(),
                                },
                            )
                            .await;
                        }
                        if ended {
                            self.status(user, bot.id).await;
                        }
                    }
                }
                ServerNotification::ThreadTokenUsageUpdated(update) => {
                    if let Some(bot) =
                        store::bot_by_thread(&self.db, user, &update.thread_id).await?
                    {
                        let total =
                            u64::try_from(update.token_usage.total.total_tokens).unwrap_or(0);
                        let mut batch = self.events.begin(user).await?;
                        usage::record_tokens(&mut batch, self.clock.now(), user, bot.id, total)
                            .await?;
                        let allowance = self.emit_usage(&mut batch).await?;
                        match batch.commit().await {
                            Ok(_) if tokens_used_up(&allowance) => {
                                self.stop_for_tokens(user).await;
                            }
                            Ok(_) => {}
                            Err(failure) => {
                                tracing::error!(user = %user, message = %failure.message, "recording tokens failed");
                            }
                        }
                    }
                }
                ServerNotification::AccountRateLimitsUpdated(update) => {
                    self.subscription.update(user, &update.rate_limits);
                    self.usage_changed(user).await;
                }
                ServerNotification::Error(error) if !error.will_retry => {
                    tracing::warn!(user = %user, thread = %error.thread_id, message = %error.error.message, "turn error");
                    if let Some(bot) =
                        store::bot_by_thread(&self.db, user, &error.thread_id).await?
                    {
                        let message: String = error.error.message.chars().take(300).collect();
                        self.emit(
                            user,
                            Event::BotError {
                                bot_id: bot.id,
                                message,
                            },
                        )
                        .await;
                    }
                }
                _ => {}
            },
            RuntimeEventKind::Refused { thread_id, reason } => {
                if let Some(bot) = store::bot_by_thread(&self.db, user, &thread_id).await? {
                    {
                        let mut live = self.live();
                        if let Some(turn) = live.turns.remove(&bot.id) {
                            live.token_stops.remove(&(user, turn.turn_id));
                        }
                    }
                    self.finish_streaming(user, Some(bot.id), MessageState::Failed)
                        .await?;
                    let mut batch = self.events.begin(user).await?;
                    store::set_refused(&mut *batch, self.clock.now(), user, bot.id, &reason)
                        .await?;
                    self.emit_status(&mut batch, bot.id).await?;
                    self.commit(batch).await;
                }
            }
            RuntimeEventKind::Exited => {
                let bots: Vec<BotId> = store::list_bots(&self.db, user)
                    .await?
                    .into_iter()
                    .map(|b| b.id)
                    .collect();
                let stopped: Vec<BotId> = {
                    let mut live = self.live();
                    live.token_stops.retain(|(owner, _)| *owner != user);
                    bots.iter()
                        .filter(|bot| live.turns.remove(bot).is_some())
                        .copied()
                        .collect()
                };
                self.finish_streaming(user, None, MessageState::Failed)
                    .await?;
                for bot in stopped {
                    self.status(user, bot).await;
                }
            }
        }
        Ok(())
    }

    /// Starts a Bot reply for a Codex agent message item.
    async fn reply(
        &self,
        user: UserId,
        thread_id: &str,
        turn_id: &str,
        item_id: &str,
        text: &str,
    ) -> Result<Option<Message>> {
        let Some(bot) = store::bot_by_thread(&self.db, user, thread_id).await? else {
            return Ok(None);
        };
        if self
            .live()
            .replies
            .contains_key(&(user, thread_id.to_owned(), item_id.to_owned()))
        {
            return Ok(None);
        }
        let mut batch = self.events.begin(user).await?;
        let message = store::insert_message(
            &mut *batch,
            self.clock.now(),
            user,
            NewMessage {
                bot: bot.id,
                author: Author::Bot,
                text,
                state: MessageState::Streaming,
                turn: Some((turn_id, item_id)),
            },
        )
        .await?;
        batch
            .emit(Event::MessageCreated {
                message: message.clone(),
            })
            .await?;
        batch.commit().await?;
        let mut live = self.live();
        if !live.finished.iter().any(|t| t == turn_id) {
            live.turns.entry(bot.id).or_insert_with(|| ActiveTurn {
                user,
                thread_id: thread_id.into(),
                turn_id: turn_id.into(),
            });
        }
        live.replies.insert(
            (user, thread_id.into(), item_id.into()),
            Reply {
                user,
                bot: bot.id,
                message: message.id,
                pending: String::new(),
                written: text.chars().count() as u64,
            },
        );
        Ok(Some(message))
    }

    /// Ends the user's replies still streaming, for one Bot or all of them.
    async fn finish_streaming(
        &self,
        user: UserId,
        bot: Option<BotId>,
        state: MessageState,
    ) -> Result<()> {
        self.flush().await?;
        self.live()
            .replies
            .retain(|(owner, _, _), reply| *owner != user || bot.is_some_and(|b| b != reply.bot));
        let mut batch = self.events.begin(user).await?;
        for message in store::streaming_messages(&mut *batch, user, bot).await? {
            if let Some(message) =
                store::finish_message(&mut *batch, self.clock.now(), user, message, None, state)
                    .await?
            {
                batch.emit(Event::MessageCompleted { message }).await?;
            }
        }
        batch.commit().await?;
        Ok(())
    }

    /// The user's allowance now, with their current limits.
    pub async fn allowance(&self, user: UserId) -> Result<Allowance> {
        allowance(&mut *self.db.acquire().await?, self.clock.now(), user).await
    }

    /// The user's usage meter. Reads the subscription's windows from the
    /// user's app server when none have been reported yet.
    pub async fn usage(&self, user: UserId) -> Result<Usage> {
        let cached = self
            .subscription
            .get(user)
            .filter(|buckets| !usage::stale(buckets, self.clock.now()));
        let subscription = match cached {
            Some(buckets) => buckets,
            None => match self.supervisor.runtime(user).await {
                Ok(runtime) => match runtime.rate_limits().await {
                    Ok(response) => {
                        let buckets = usage::buckets(&response);
                        self.subscription.set(user, buckets.clone());
                        buckets
                    }
                    Err(error) => {
                        tracing::warn!(user = %user, %error, "reading rate limits failed");
                        Vec::new()
                    }
                },
                Err(_) => Vec::new(),
            },
        };
        Ok(usage::usage(subscription, self.allowance(user).await?))
    }

    /// Adds the user's usage meter to a batch, as the batch sees it, and
    /// returns the allowance it shows.
    pub async fn emit_usage(&self, batch: &mut Batch) -> Result<Allowance> {
        let user = batch.user();
        let allowance = allowance(batch, self.clock.now(), user).await?;
        let subscription = self.subscription.get(user).unwrap_or_default();
        batch
            .emit(Event::UsageUpdated {
                usage: usage::usage(subscription, allowance.clone()),
            })
            .await?;
        Ok(allowance)
    }

    /// Interrupts the user's running turns once their token allowance is
    /// used up, each turn once. Codex reports a turn's tokens as each model
    /// response completes, so the allowance is overshot by at most one
    /// response per running turn: a chat-only turn has usually finished its
    /// reply by then, and a turn that would ask the model again is cut
    /// short. The interrupts run in their own task, which also finds the
    /// user's app server, so routing never waits on the supervisor.
    async fn stop_for_tokens(&self, user: UserId) {
        let running: Vec<(BotId, ActiveTurn)> = {
            let mut live = self.live();
            let running: Vec<(BotId, ActiveTurn)> = live
                .turns
                .iter()
                .filter(|(_, turn)| turn.user == user)
                .map(|(bot, turn)| (*bot, turn.clone()))
                .collect();
            running
                .into_iter()
                .filter(|(_, turn)| live.token_stops.insert((user, turn.turn_id.clone())))
                .collect()
        };
        if running.is_empty() {
            return;
        }
        let supervisor = self.supervisor.clone();
        tokio::spawn(async move {
            let Some(runtime) = supervisor.running(user).await else {
                return;
            };
            for (bot_id, turn) in running {
                tracing::info!(user = %user, bot = %bot_id, "token allowance used up; stopping the turn");
                if let Err(error) = runtime.interrupt_turn(&turn.thread_id, &turn.turn_id).await {
                    tracing::info!(user = %user, bot = %bot_id, %error, "the turn was not stopped");
                }
            }
        });
    }

    async fn usage_changed(&self, user: UserId) {
        match self.events.begin(user).await {
            Ok(mut batch) => match self.emit_usage(&mut batch).await {
                Ok(_) => self.commit(batch).await,
                Err(failure) => {
                    tracing::error!(user = %user, message = %failure.message, "reading usage failed");
                }
            },
            Err(failure) => {
                tracing::error!(user = %user, message = %failure.message, "reading usage failed");
            }
        }
    }

    /// Counts a turn for the user if their allowance has one left, and
    /// announces the new count.
    async fn reserve_turn(&self, user: UserId, bot: BotId) -> Result<Option<i64>> {
        let mut batch = self.events.begin(user).await?;
        let Some(turn) = usage::reserve_turn(&mut batch, self.clock.now(), user, bot).await? else {
            return Ok(None);
        };
        self.emit_usage(&mut batch).await?;
        batch.commit().await?;
        Ok(Some(turn))
    }

    /// Takes back a counted turn that Codex refused. A failure leaves the
    /// turn counted, which costs the user one turn rather than letting a
    /// turn go uncounted.
    async fn release_turn(&self, user: UserId, turn: i64) {
        let released = async {
            let mut batch = self.events.begin(user).await?;
            usage::release_turn(&mut *batch, user, turn).await?;
            self.emit_usage(&mut batch).await?;
            batch.commit().await
        };
        if let Err(failure) = released.await {
            tracing::error!(user = %user, message = %failure.message, "releasing a turn failed");
        }
    }

    /// Adds the Bot's status to a batch. The status is read while the batch
    /// holds the user's log lock, and every change to it is followed by a
    /// status event, so the user's latest status event is always current.
    async fn emit_status(&self, batch: &mut Batch, bot_id: BotId) -> Result<()> {
        let user = batch.user();
        let Some(bot) = store::bot(&mut **batch, user, bot_id).await? else {
            return Ok(());
        };
        let status = bot.to_api(self.is_working(bot_id), None).status;
        batch.emit(Event::BotStatusChanged { bot_id, status }).await
    }

    /// Announces the Bot's current status.
    async fn status(&self, user: UserId, bot_id: BotId) {
        match self.events.begin(user).await {
            Ok(mut batch) => match self.emit_status(&mut batch, bot_id).await {
                Ok(()) => self.commit(batch).await,
                Err(failure) => {
                    tracing::error!(user = %user, message = %failure.message, "reading a Bot failed");
                }
            },
            Err(failure) => {
                tracing::error!(user = %user, message = %failure.message, "reading a Bot failed");
            }
        }
    }
}

/// Whether the user's weekly tokens are used up. Then nothing more runs:
/// no new turn, no steer, and running turns are stopped.
fn tokens_used_up(allowance: &Allowance) -> bool {
    allowance
        .limits
        .weekly_tokens
        .is_some_and(|limit| allowance.tokens_this_week >= limit)
}

/// The user's allowance at `now`, with their current limits.
async fn allowance(
    conn: &mut sqlx::SqliteConnection,
    now: Timestamp,
    user: UserId,
) -> Result<Allowance> {
    let limits = crate::accounts::account(&mut *conn, user)
        .await?
        .map(|a| a.limits)
        .unwrap_or_default();
    usage::allowance(&mut *conn, now, user, limits).await
}
