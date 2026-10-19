//! The client's copy of the signed-in user's Bots and conversations: HTTP
//! snapshots merged with the event log.
//!
//! The gateway's snapshots ([`BotList`] and [`MessagePage`]) each carry the
//! sequence number they reflect. They reflect every event up to and including
//! it, and may reflect some later ones. The [`Store`] therefore applies to
//! each part of its state only the events after that part's snapshot:
//!
//! - the Bot list after [`BotList::seq`];
//! - each loaded conversation after its own [`MessagePage::seq`].
//!
//! Events that arrive while a snapshot is still loading are held, and
//! replayed onto it when it arrives. Streaming text is assembled from
//! [`Event::MessageDelta`] by its character offset, so a delta the store
//! already holds, in full or in part, adds only what is new. A delta that
//! starts beyond the text held, or names a message the conversation does not
//! hold, shows that the copy is incomplete: the store asks for that
//! conversation to be reloaded rather than guessing. [`Event::MessageCompleted`]
//! replaces whatever was assembled.
//!
//! Older messages load a page at a time ([`Snapshot::Older`]), from the
//! conversation's `older` cursor. Events are held while that page loads too.
//! The page adds only messages the conversation does not hold, since those
//! it holds are already kept current, and the held events after the page's
//! own `seq` are replayed onto the messages it adds. A page requested for a
//! copy of the conversation that has since been reloaded, or whose cursor
//! has moved, is dropped. Reloading a conversation's newest page starts it
//! afresh, without the older pages loaded before.
//!
//! The store expects events in log order with no duplicates, as
//! [`Connection`](crate::connection::Connection) delivers them. A repeated
//! event changes nothing, but an old event applied after newer ones, such as
//! an earlier status, would undo them.

use std::collections::{BTreeMap, BTreeSet};

use bokkie_protocol::api::{
    Bot, BotList, Message, MessagePage, MessagePreview, MessageState, Usage,
};
use bokkie_protocol::events::{Event, EventEnvelope, Seq};
use bokkie_protocol::{BotId, MessageId};

/// How many characters of a conversation's latest message the Bot list
/// shows, as the gateway's own previews hold.
pub const PREVIEW_CHARS: usize = 140;

/// At most this many events are held while snapshots load; the gateway keeps
/// no more than this in its log either. Beyond it, the snapshots still loading
/// are loaded again.
const MAX_HELD: usize = 5_000;

/// A part of the state that loads from one HTTP snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Snapshot {
    /// `GET /api/v1/bots`.
    Bots,
    /// `GET /api/v1/bots/{id}/messages`.
    Messages(BotId),
    /// `GET /api/v1/bots/{id}/messages?before=`: the page before the oldest
    /// message the conversation holds.
    Older(BotId),
}

/// What applying a snapshot or an event did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[must_use]
pub struct Applied {
    /// Whether presented state changed.
    pub changed: bool,
    /// Snapshots to load now. The store already counts them as loading.
    pub reload: BTreeSet<Snapshot>,
}

impl Applied {
    fn changed(changed: bool) -> Self {
        Self {
            changed,
            reload: BTreeSet::new(),
        }
    }

    fn reload(snapshot: Snapshot) -> Self {
        Self {
            changed: false,
            reload: BTreeSet::from([snapshot]),
        }
    }

    fn merge(&mut self, other: Self) {
        self.changed |= other.changed;
        self.reload.extend(other.reload);
    }
}

/// One Bot's conversation, as far as it is loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    seq: Seq,
    messages: BTreeMap<MessageId, Message>,
    older: Option<MessageId>,
    /// The cursor of the older page requested for this copy, until it loads.
    requested_older: Option<MessageId>,
}

impl Conversation {
    /// The messages held, oldest first.
    pub fn messages(&self) -> impl DoubleEndedIterator<Item = &Message> {
        self.messages.values()
    }

    pub fn message(&self, id: MessageId) -> Option<&Message> {
        self.messages.get(&id)
    }

    /// The `before` cursor for older messages, or `None` at the start.
    pub fn older(&self) -> Option<MessageId> {
        self.older
    }

    /// The sequence number of the snapshot this conversation loaded from.
    pub fn seq(&self) -> Seq {
        self.seq
    }
}

/// The signed-in user's Bots and the conversations the client has open.
#[derive(Debug, Default)]
pub struct Store {
    bots: BTreeMap<BotId, Bot>,
    /// `None` until the Bot list has loaded.
    bots_seq: Option<Seq>,
    /// The newest message the store knows of in each Bot's conversation. The
    /// Bot list's previews do not name their message, so a Bot is missing
    /// here until an event or a loaded conversation shows it.
    newest: BTreeMap<BotId, MessageId>,
    conversations: BTreeMap<BotId, Conversation>,
    /// The latest error each Bot reported.
    errors: BTreeMap<BotId, String>,
    usage: Option<Usage>,
    /// A `usage_updated` event arrived since the usage meter was last
    /// requested over HTTP, so a reply to that request may be older.
    usage_event_seen: bool,
    /// Snapshots requested and not yet loaded.
    loading: BTreeSet<Snapshot>,
    /// Events received while anything was loading.
    held: Vec<EventEnvelope>,
    /// Snapshots that were loading when held events had to be dropped.
    stale: BTreeSet<Snapshot>,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the Bot list has loaded at least once.
    pub fn bots_loaded(&self) -> bool {
        self.bots_seq.is_some()
    }

    pub fn bot(&self, id: BotId) -> Option<&Bot> {
        self.bots.get(&id)
    }

    /// Every Bot, hidden ones included, in no particular order.
    pub fn bots(&self) -> impl Iterator<Item = &Bot> {
        self.bots.values()
    }

    /// The Bots the conversation list shows, in the gateway's order: pinned
    /// first, then by most recent activity.
    pub fn visible_bots(&self) -> Vec<&Bot> {
        let mut bots: Vec<&Bot> = self.bots.values().filter(|bot| !bot.hidden).collect();
        bots.sort_by(|a, b| {
            let activity = |bot: &Bot| bot.last_message.as_ref().map_or(bot.updated_at, |m| m.at);
            b.pinned
                .cmp(&a.pinned)
                .then_with(|| activity(b).cmp(&activity(a)))
                .then_with(|| a.id.cmp(&b.id))
        });
        bots
    }

    pub fn conversation(&self, bot: BotId) -> Option<&Conversation> {
        self.conversations.get(&bot)
    }

    /// The latest error a Bot reported, if any.
    pub fn error(&self, bot: BotId) -> Option<&str> {
        self.errors.get(&bot).map(String::as_str)
    }

    /// Forget a Bot's latest error, as when the user sends it a new message.
    /// Returns whether there was one.
    pub fn clear_error(&mut self, bot: BotId) -> bool {
        self.errors.remove(&bot).is_some()
    }

    /// The usage meter, once loaded or reported by an event.
    pub fn usage(&self) -> Option<&Usage> {
        self.usage.as_ref()
    }

    /// The usage meter is about to be requested over HTTP.
    pub fn request_usage(&mut self) {
        self.usage_event_seen = false;
    }

    /// The usage meter from HTTP. It has no sequence number, so it applies
    /// only if no `usage_updated` event arrived since it was requested,
    /// which would be at least as new. The exception is the subscription's
    /// windows: the gateway may read them afresh for this request without
    /// announcing them, so they fill a meter whose events have none yet.
    /// Returns whether the meter changed.
    pub fn load_usage(&mut self, usage: Usage) -> bool {
        if self.usage_event_seen {
            return match &mut self.usage {
                Some(held) if held.subscription.is_empty() && !usage.subscription.is_empty() => {
                    held.subscription = usage.subscription;
                    true
                }
                _ => false,
            };
        }
        if self.usage.as_ref() == Some(&usage) {
            return false;
        }
        self.usage = Some(usage);
        true
    }

    pub fn is_loading(&self, snapshot: Snapshot) -> bool {
        self.loading.contains(&snapshot)
    }

    /// Mark a snapshot as loading, so that events arriving meanwhile are held
    /// for it. Returns false if it was already loading.
    pub fn request(&mut self, snapshot: Snapshot) -> bool {
        self.loading.insert(snapshot)
    }

    /// Everything to load again, as after a `reset` frame or on the first
    /// connection: the Bot list and every open conversation. Each is marked
    /// as loading.
    pub fn reload_all(&mut self) -> BTreeSet<Snapshot> {
        let all: BTreeSet<Snapshot> = std::iter::once(Snapshot::Bots)
            .chain(
                self.conversations
                    .keys()
                    .map(|bot| Snapshot::Messages(*bot)),
            )
            .collect();
        self.loading.extend(all.iter().copied());
        all
    }

    /// A snapshot failed to load. It stops counting as loading; request it
    /// again to retry.
    pub fn load_failed(&mut self, snapshot: Snapshot) {
        self.loading.remove(&snapshot);
        self.stale.remove(&snapshot);
        if let Snapshot::Older(bot) = snapshot
            && let Some(conversation) = self.conversations.get_mut(&bot)
        {
            conversation.requested_older = None;
        }
        self.settle();
    }

    /// Mark the page before a conversation's oldest message as loading, and
    /// return its `before` cursor. Returns `None`, loading nothing, when the
    /// conversation is not loaded or already starts at its first message.
    pub fn request_older(&mut self, bot: BotId) -> Option<MessageId> {
        let snapshot = Snapshot::Older(bot);
        let cursor = self.conversations.get_mut(&bot).and_then(|conversation| {
            conversation.requested_older = conversation.older;
            conversation.older
        });
        match cursor {
            Some(_) => {
                self.loading.insert(snapshot);
            }
            None => self.load_failed(snapshot),
        }
        cursor
    }

    /// Stop keeping a conversation. Events for it are then ignored.
    pub fn close_conversation(&mut self, bot: BotId) {
        self.conversations.remove(&bot);
        for snapshot in [Snapshot::Messages(bot), Snapshot::Older(bot)] {
            self.loading.remove(&snapshot);
            self.stale.remove(&snapshot);
        }
        self.settle();
    }

    /// Load the Bot list and apply the held events that followed it.
    pub fn load_bots(&mut self, list: BotList) -> Applied {
        let mut applied = Applied::changed(true);
        self.bots = list.bots.into_iter().map(|bot| (bot.id, bot)).collect();
        self.bots_seq = Some(list.seq);
        self.newest.retain(|bot, _| self.bots.contains_key(bot));
        self.errors.retain(|bot, _| self.bots.contains_key(bot));
        self.loading.remove(&Snapshot::Bots);
        for envelope in std::mem::take(&mut self.held) {
            if envelope.seq > list.seq {
                applied.merge(self.apply_to_bots(&envelope));
            }
            self.held.push(envelope);
        }
        applied.merge(self.reload_if_stale(Snapshot::Bots));
        self.settle();
        self.finish(applied)
    }

    /// Load the newest page of a conversation and apply the held events that
    /// followed it.
    pub fn load_messages(&mut self, bot: BotId, page: MessagePage) -> Applied {
        let mut applied = Applied::changed(true);
        if let Some(newest) = page.messages.iter().map(|message| message.id).max() {
            self.note_newest(bot, newest);
        }
        self.conversations.insert(
            bot,
            Conversation {
                seq: page.seq,
                messages: page
                    .messages
                    .into_iter()
                    .map(|message| (message.id, message))
                    .collect(),
                older: page.older,
                requested_older: None,
            },
        );
        let snapshot = Snapshot::Messages(bot);
        self.loading.remove(&snapshot);
        for envelope in std::mem::take(&mut self.held) {
            if envelope.seq > page.seq {
                applied.merge(self.apply_to_conversation(bot, &envelope));
            }
            self.held.push(envelope);
        }
        applied.merge(self.reload_if_stale(snapshot));
        self.settle();
        self.finish(applied)
    }

    /// Add a page of older messages, requested with `before`, to the copy of
    /// the conversation it was requested for, and replay onto them the held
    /// events that followed the page. A page for a copy reloaded since, or
    /// for a cursor that has moved, changes nothing. A page that missed held
    /// events, or that cannot account for a delta, is loaded again.
    pub fn load_older(&mut self, bot: BotId, before: MessageId, page: MessagePage) -> Applied {
        let snapshot = Snapshot::Older(bot);
        self.loading.remove(&snapshot);
        let stale = self.stale.remove(&snapshot);
        let applied = match self.conversations.get_mut(&bot) {
            Some(conversation)
                if conversation.requested_older == Some(before)
                    && conversation.older == Some(before) =>
            {
                conversation.requested_older = None;
                let mut earlier: BTreeMap<MessageId, Message> = page
                    .messages
                    .into_iter()
                    .filter(|message| {
                        message.bot_id == bot
                            && message.id < before
                            && !conversation.messages.contains_key(&message.id)
                    })
                    .map(|message| (message.id, message))
                    .collect();
                let mut complete = !stale;
                for envelope in self.held.iter().filter(|envelope| envelope.seq > page.seq) {
                    match &envelope.event {
                        Event::MessageDelta {
                            message_id,
                            offset,
                            delta,
                            ..
                        } => {
                            if let Some(message) = earlier.get_mut(message_id) {
                                complete &= append_delta(message, *offset, delta).is_some();
                            }
                        }
                        Event::MessageCompleted { message }
                            if earlier.contains_key(&message.id) =>
                        {
                            earlier.insert(message.id, message.clone());
                        }
                        _ => {}
                    }
                }
                if complete {
                    let changed = !earlier.is_empty() || conversation.older != page.older;
                    conversation.messages.extend(earlier);
                    conversation.older = page.older;
                    Applied::changed(changed)
                } else {
                    Applied::reload(snapshot)
                }
            }
            // Reloaded, closed or moved on since the page was requested.
            _ => Applied::default(),
        };
        self.settle();
        self.finish(applied)
    }

    /// Apply one event from the log.
    pub fn apply(&mut self, envelope: &EventEnvelope) -> Applied {
        if !self.loading.is_empty() {
            if self.held.len() >= MAX_HELD {
                self.held.clear();
                self.stale.extend(self.loading.iter().copied());
            }
            self.held.push(envelope.clone());
        }
        let mut applied = match message_bot(&envelope.event) {
            Some(bot) => self.apply_to_conversation(bot, envelope),
            None => Applied::default(),
        };
        // After the conversation, so a preview can show its assembled text.
        applied.merge(self.apply_to_bots(envelope));
        if let Event::UsageUpdated { usage } = &envelope.event {
            applied.changed |= self.usage.as_ref() != Some(usage);
            self.usage = Some(usage.clone());
            self.usage_event_seen = true;
        }
        self.finish(applied)
    }

    /// Report only reloads that are not already under way, and count them
    /// as loading.
    fn finish(&mut self, mut applied: Applied) -> Applied {
        applied
            .reload
            .retain(|snapshot| self.loading.insert(*snapshot));
        applied
    }

    fn reload_if_stale(&mut self, snapshot: Snapshot) -> Applied {
        if self.stale.remove(&snapshot) {
            Applied::reload(snapshot)
        } else {
            Applied::default()
        }
    }

    fn settle(&mut self) {
        if self.loading.is_empty() {
            self.held.clear();
        }
    }

    fn note_newest(&mut self, bot: BotId, message: MessageId) {
        let newest = self.newest.entry(bot).or_insert(message);
        *newest = (*newest).max(message);
    }

    /// The part of an event that concerns the Bot list.
    fn apply_to_bots(&mut self, envelope: &EventEnvelope) -> Applied {
        let Some(seq) = self.bots_seq else {
            return Applied::default();
        };
        if envelope.seq <= seq {
            return Applied::default();
        }
        match &envelope.event {
            Event::BotCreated { bot } | Event::BotUpdated { bot } => {
                let changed = self.bots.get(&bot.id) != Some(bot);
                self.bots.insert(bot.id, bot.clone());
                Applied::changed(changed)
            }
            Event::BotStatusChanged { bot_id, status } => match self.bots.get_mut(bot_id) {
                Some(bot) => {
                    let changed = bot.status != *status;
                    bot.status = *status;
                    Applied::changed(changed)
                }
                None => Applied::reload(Snapshot::Bots),
            },
            Event::MessageCreated { message } => {
                let newest = self.newest.get(&message.bot_id).copied();
                self.note_newest(message.bot_id, message.id);
                match newest {
                    Some(newest) if newest > message.id => Applied::default(),
                    // Already known, from a loaded page or as a repeat:
                    // only the conversation's copy is newer than the event.
                    Some(newest) if newest == message.id => {
                        match self.held_preview(message.bot_id, message.id) {
                            Some(preview) => self.set_preview(message.bot_id, preview),
                            None => Applied::default(),
                        }
                    }
                    _ => {
                        let preview = self
                            .held_preview(message.bot_id, message.id)
                            .unwrap_or_else(|| preview(message));
                        self.set_preview(message.bot_id, preview)
                    }
                }
            }
            Event::MessageDelta {
                bot_id, message_id, ..
            } => {
                if self.newest.get(bot_id) != Some(message_id) {
                    return Applied::default();
                }
                match self.held_preview(*bot_id, *message_id) {
                    Some(preview) => self.set_preview(*bot_id, preview),
                    None => Applied::default(),
                }
            }
            Event::MessageCompleted { message } => match self.newest.get(&message.bot_id) {
                Some(newest) if *newest > message.id => Applied::default(),
                Some(_) => {
                    self.note_newest(message.bot_id, message.id);
                    self.set_preview(message.bot_id, preview(message))
                }
                // The list's preview may or may not be this message; ask the
                // gateway rather than guess.
                None if self.bots.contains_key(&message.bot_id) => Applied::reload(Snapshot::Bots),
                None => Applied::default(),
            },
            Event::BotError { bot_id, message } => {
                let changed = self.errors.get(bot_id) != Some(message);
                self.errors.insert(*bot_id, message.clone());
                Applied::changed(changed)
            }
            Event::UsageUpdated { .. } => Applied::default(),
        }
    }

    /// A preview of a message from the open conversation's copy, which may
    /// be ahead of the event being applied.
    fn held_preview(&self, bot: BotId, message: MessageId) -> Option<MessagePreview> {
        self.conversations.get(&bot)?.message(message).map(preview)
    }

    fn set_preview(&mut self, bot: BotId, preview: MessagePreview) -> Applied {
        match self.bots.get_mut(&bot) {
            Some(entry) => {
                let changed = entry.last_message.as_ref() != Some(&preview);
                entry.last_message = Some(preview);
                Applied::changed(changed)
            }
            None => Applied::reload(Snapshot::Bots),
        }
    }

    /// The part of an event that concerns one open conversation.
    fn apply_to_conversation(&mut self, bot: BotId, envelope: &EventEnvelope) -> Applied {
        let Some(conversation) = self.conversations.get_mut(&bot) else {
            return Applied::default();
        };
        if envelope.seq <= conversation.seq {
            return Applied::default();
        }
        match &envelope.event {
            Event::MessageCreated { message } => {
                if let Some(held) = conversation.messages.get(&message.id) {
                    // A snapshot that reflected later events can be ahead of
                    // the message's creation: keep what it already shows.
                    let ahead = held.state != MessageState::Streaming
                        || held.text.starts_with(message.text.as_str());
                    if ahead {
                        return Applied::default();
                    }
                }
                conversation.messages.insert(message.id, message.clone());
                Applied::changed(true)
            }
            Event::MessageDelta {
                message_id,
                offset,
                delta,
                ..
            } => {
                let Some(message) = conversation.messages.get_mut(message_id) else {
                    return Applied::reload(Snapshot::Messages(bot));
                };
                match append_delta(message, *offset, delta) {
                    Some(changed) => Applied::changed(changed),
                    None => Applied::reload(Snapshot::Messages(bot)),
                }
            }
            Event::MessageCompleted { message } => {
                let changed = conversation.messages.get(&message.id) != Some(message);
                conversation.messages.insert(message.id, message.clone());
                Applied::changed(changed)
            }
            _ => Applied::default(),
        }
    }
}

/// Add the part of a delta that a streaming message does not hold yet.
/// Returns whether the text grew, or `None` when the delta starts beyond the
/// text held, so the copy is incomplete. A message no longer streaming
/// takes no deltas.
fn append_delta(message: &mut Message, offset: u64, delta: &str) -> Option<bool> {
    if message.state != MessageState::Streaming {
        return Some(false);
    }
    let held = message.text.chars().count() as u64;
    if offset > held {
        return None;
    }
    let known = usize::try_from(held - offset).unwrap_or(usize::MAX);
    let new: String = delta.chars().skip(known).collect();
    message.text.push_str(&new);
    Some(!new.is_empty())
}

/// The conversation an event changes, if it changes one.
fn message_bot(event: &Event) -> Option<BotId> {
    match event {
        Event::MessageCreated { message } | Event::MessageCompleted { message } => {
            Some(message.bot_id)
        }
        Event::MessageDelta { bot_id, .. } => Some(*bot_id),
        _ => None,
    }
}

/// A conversation list preview of a message, as the gateway makes one.
fn preview(message: &Message) -> MessagePreview {
    MessagePreview {
        author: message.author,
        text: message.text.chars().take(PREVIEW_CHARS).collect(),
        at: message.updated_at,
    }
}

#[cfg(test)]
mod tests;
