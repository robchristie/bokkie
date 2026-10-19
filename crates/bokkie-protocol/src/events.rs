//! The per-user event log and the frames of the event WebSocket at
//! [`EVENTS_PATH`](crate::EVENTS_PATH).
//!
//! Each user has one ordered log. Every event in it has a sequence number
//! that starts at 1 and increases by one, with no gaps. The gateway keeps a
//! bounded tail of the log.
//!
//! A connection works like this:
//!
//! 1. The client opens the WebSocket with its session cookie and sends
//!    [`ClientFrame::Resume`] with the last sequence number it processed, or
//!    `None` on first start.
//! 2. If that number is outside the retained log (events after it have been
//!    dropped, or it is beyond the newest event), the gateway sends
//!    [`ServerFrame::Reset`]. The client then reloads its state over HTTP and
//!    continues from the sequence number the reset names.
//! 3. The gateway replays every retained event after that number, in order,
//!    then sends [`ServerFrame::Live`]. From then on events arrive as they
//!    happen.
//!
//! Sequence numbers are durable: they survive gateway restarts.
//!
//! HTTP snapshots ([`BotList`](crate::api::BotList) and
//! [`MessagePage`](crate::api::MessagePage)) carry a `seq`: they reflect
//! every event up to and including it, and may reflect some later ones. To
//! start from a snapshot, open the WebSocket first, load the snapshot, and
//! apply events after its `seq`. Applying an event twice is harmless:
//! messages replace by id, and [`Event::MessageDelta`] carries the offset it
//! starts at.
//!
//! Streaming text arrives as [`Event::MessageDelta`]. The gateway coalesces
//! the app server's deltas, so one event may carry many model tokens. A
//! client that falls too far behind is disconnected rather than allowed to
//! slow the gateway down; it reconnects and resumes from its last sequence
//! number.

use serde::{Deserialize, Serialize};

use crate::api::{Bot, BotStatus, Message, Routine, RoutineRun, Usage, User};
#[cfg(doc)]
use crate::api::{BotList, MessagePage};
use crate::{BotId, MessageId, RoutineId, Timestamp};

/// A position in a user's event log. Sequence numbers start at 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seq(pub u64);

impl Seq {
    /// The position before the first event.
    pub const ZERO: Self = Self(0);

    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// One entry of the event log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub seq: Seq,
    pub at: Timestamp,
    pub event: Event,
}

/// Something that changed for the signed-in user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A Bot was created.
    BotCreated { bot: Bot },
    /// A Bot's profile, pin or visibility changed.
    BotUpdated { bot: Bot },
    /// A Bot started or finished work, or was refused.
    BotStatusChanged { bot_id: BotId, status: BotStatus },
    /// A message was added to a conversation. A Bot reply starts in
    /// [`MessageState::Streaming`](crate::api::MessageState::Streaming) with
    /// empty or partial text.
    MessageCreated { message: Message },
    /// Text appended to a streaming Bot reply. `offset` is the length of
    /// the reply before this delta, in Unicode scalar values (Rust `char`s).
    /// Skip what the client already has; reload the message if `offset` is
    /// beyond the text the client holds.
    MessageDelta {
        bot_id: BotId,
        message_id: MessageId,
        offset: u64,
        delta: String,
    },
    /// A message reached its final text and state. The text replaces
    /// everything assembled from deltas.
    MessageCompleted { message: Message },
    /// The usage meter changed.
    UsageUpdated { usage: Usage },
    /// A Bot could not complete a request. The message is safe to show.
    BotError { bot_id: BotId, message: String },
    /// The user's account changed: an admin changed its display name,
    /// permissions or oversight, or the user their time zone. `user` is
    /// the account as it now is.
    AccountUpdated { user: User },
    /// A routine was created.
    RoutineCreated { routine: Routine },
    /// A routine changed: it was edited, paused or resumed, ran, or was
    /// rescheduled.
    RoutineUpdated { routine: Routine },
    /// A routine was deleted, with its run history.
    RoutineDeleted {
        bot_id: BotId,
        routine_id: RoutineId,
    },
    /// A routine run was queued, started or ended. `run` is the run as it
    /// now is; [`RunState::is_finished`](crate::api::RunState::is_finished)
    /// says whether this is its end.
    RoutineRunUpdated { run: RoutineRun },
}

/// A frame from the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    /// Must be the first frame. `after` is the last sequence number the
    /// client processed, or `None` to start from the current end of the log.
    Resume { after: Option<Seq> },
    /// Keeps the connection alive; the gateway answers with
    /// [`ServerFrame::Pong`].
    Ping,
}

/// A frame from the gateway.
// Frames are decoded one at a time and handed on at once, so the size of
// the event variant costs nothing worth a box.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    /// The events the client asked for are no longer retained. Reload state
    /// over HTTP; events after `after` follow.
    Reset {
        after: Seq,
    },
    /// One event, replayed or live.
    Event(EventEnvelope),
    /// Replay is complete. `latest` is the newest sequence number so far.
    Live {
        latest: Seq,
    },
    Pong,
}
