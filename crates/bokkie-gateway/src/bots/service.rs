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
    Author, BotStatus, Delivery, Message, MessageState, SendMessageResponse,
};
use bokkie_protocol::events::Event;
use bokkie_protocol::{BotId, MessageId, UserId};
use sqlx::SqlitePool;
use tokio::sync::mpsc;

use super::store::{self, BotRow, NewMessage};
use crate::accounts::Account;
use crate::clock::Clock;
use crate::codex::{CodexError, RuntimeEvent, RuntimeEventKind, Supervisor};
use crate::error::{Failure, Result};
use crate::events::EventLog;

/// How often streamed text is written out. Deltas arriving in between are
/// coalesced into one write.
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(100);

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

/// The conversation service shared by the routes and the event router.
#[derive(Debug)]
pub struct Conversations {
    db: SqlitePool,
    clock: Clock,
    supervisor: Arc<Supervisor>,
    events: Arc<EventLog>,
    live: std::sync::Mutex<Live>,
    /// Serialises sends to one Bot.
    sends: std::sync::Mutex<HashMap<BotId, Arc<tokio::sync::Mutex<()>>>>,
}

fn unavailable(error: CodexError) -> Failure {
    match error {
        CodexError::Refused(_) | CodexError::ThreadRefused(_) => Failure::unavailable(
            "This Bot is unavailable because a safety check failed. An admin has been told",
        ),
        CodexError::AppServer(_) => Failure::unavailable("The Bots' runtime is unavailable"),
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
            live: Default::default(),
            sends: Default::default(),
        }
    }

    fn live(&self) -> std::sync::MutexGuard<'_, Live> {
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Appends an event to the user's log. A failure is logged: the change
    /// itself has happened, and clients catch up from HTTP after a reset.
    async fn emit(&self, user: UserId, event: Event) {
        if let Err(failure) = self.events.append(user, event).await {
            tracing::error!(user = %user, message = %failure.message, "appending an event failed");
        }
    }

    pub fn is_working(&self, bot: BotId) -> bool {
        self.live().turns.contains_key(&bot)
    }

    fn send_lock(&self, bot: BotId) -> Arc<tokio::sync::Mutex<()>> {
        self.sends
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(bot)
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
            if let Some(message) = store::finish_message(
                &self.db,
                self.clock.now(),
                user,
                MessageId(*id),
                None,
                MessageState::Failed,
            )
            .await?
            {
                self.emit(user, Event::MessageCompleted { message }).await;
            }
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
        let lock = self.send_lock(bot_id);
        let _sending = lock.lock().await;
        let bot = store::bot(&self.db, user, bot_id)
            .await?
            .ok_or_else(Failure::not_found)?;
        if bot.refused_reason.is_some() {
            return Err(unavailable(CodexError::Refused(
                crate::codex::guard::Refusal::new("refused"),
            )));
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

        let message = store::insert_message(
            &self.db,
            self.clock.now(),
            user,
            NewMessage {
                bot: bot.id,
                author: Author::User,
                text: &text,
                state: MessageState::Complete,
                turn: None,
            },
        )
        .await?;
        self.emit(
            user,
            Event::MessageCreated {
                message: message.clone(),
            },
        )
        .await;

        // Steer the running turn, if there is one.
        let active = self.live().turns.get(&bot.id).cloned();
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
                Err(CodexError::AppServer(error)) => {
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
        let turn = runtime.start_turn(&thread_id, &text, profile).await;
        let turn = self.refusing(user, &bot, turn).await?;
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
                            thread_id,
                            turn_id: turn.id,
                        },
                    )
                    .is_none()
        };
        if started {
            self.status(user, bot.id, BotStatus::Working).await;
        }
        Ok(SendMessageResponse {
            message,
            delivery: Delivery::NewTurn,
        })
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
                store::set_refused(&self.db, self.clock.now(), user, bot.id, &refusal.0).await?;
                self.emit(
                    user,
                    Event::BotStatusChanged {
                        bot_id: bot.id,
                        status: BotStatus::Refused,
                    },
                )
                .await;
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
            store::append_text(&self.db, self.clock.now(), user, message_id, &delta).await?;
            self.emit(
                user,
                Event::MessageDelta {
                    bot_id,
                    message_id,
                    offset,
                    delta,
                },
            )
            .await;
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
                                    thread_id: started.thread_id,
                                    turn_id: started.turn.id,
                                },
                            )
                            .is_none();
                        if was_idle {
                            self.status(user, bot.id, BotStatus::Working).await;
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
                            let message = store::finish_message(
                                &self.db,
                                self.clock.now(),
                                user,
                                reply.message,
                                Some(text),
                                MessageState::Complete,
                            )
                            .await?;
                            if let Some(message) = message {
                                self.emit(user, Event::MessageCompleted { message }).await;
                            }
                        }
                    }
                }
                ServerNotification::TurnCompleted(done) => {
                    if let Some(bot) = store::bot_by_thread(&self.db, user, &done.thread_id).await?
                    {
                        let ended = {
                            let mut live = self.live();
                            live.finished(&done.turn.id);
                            let current = live
                                .turns
                                .get(&bot.id)
                                .is_some_and(|t| t.turn_id == done.turn.id);
                            if current {
                                live.turns.remove(&bot.id);
                            }
                            current
                        };
                        let state = match done.turn.status {
                            TurnStatus::Completed | TurnStatus::InProgress => {
                                MessageState::Complete
                            }
                            TurnStatus::Interrupted => MessageState::Interrupted,
                            TurnStatus::Failed => MessageState::Failed,
                        };
                        self.finish_streaming(user, Some(bot.id), state).await?;
                        if ended && bot.refused_reason.is_none() {
                            self.status(user, bot.id, BotStatus::Idle).await;
                        }
                    }
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
                    store::set_refused(&self.db, self.clock.now(), user, bot.id, &reason).await?;
                    self.live().turns.remove(&bot.id);
                    self.finish_streaming(user, Some(bot.id), MessageState::Failed)
                        .await?;
                    self.status(user, bot.id, BotStatus::Refused).await;
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
                    bots.iter()
                        .filter(|bot| live.turns.remove(bot).is_some())
                        .copied()
                        .collect()
                };
                self.finish_streaming(user, None, MessageState::Failed)
                    .await?;
                for bot in stopped {
                    self.status(user, bot, BotStatus::Idle).await;
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
        let message = store::insert_message(
            &self.db,
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
        self.emit(
            user,
            Event::MessageCreated {
                message: message.clone(),
            },
        )
        .await;
        let mut live = self.live();
        if !live.finished.iter().any(|t| t == turn_id) {
            live.turns.entry(bot.id).or_insert_with(|| ActiveTurn {
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
        for message in store::streaming_messages(&self.db, user, bot).await? {
            if let Some(message) =
                store::finish_message(&self.db, self.clock.now(), user, message, None, state)
                    .await?
            {
                self.emit(user, Event::MessageCompleted { message }).await;
            }
        }
        Ok(())
    }

    async fn status(&self, user: UserId, bot_id: BotId, status: BotStatus) {
        self.emit(user, Event::BotStatusChanged { bot_id, status })
            .await;
    }
}
