//! Wire types shared by the Bokkie gateway and its WASM client.
//!
//! The client sends commands as JSON over plain HTTPS ([`api`]) and receives
//! events over one authenticated WebSocket ([`events`]). This crate is pure
//! serde: it performs no I/O, depends on nothing platform-specific, and builds
//! for `wasm32-unknown-unknown`.
//!
//! Every event carries a per-user sequence number ([`events::Seq`]). A client
//! that reconnects sends the last number it saw and the gateway replays what
//! followed.

pub mod api;
pub mod events;

use serde::{Deserialize, Serialize};

/// The version of this wire protocol. The gateway reports it at
/// `GET /api/v1/version`, and a client refuses to talk to a gateway whose
/// major version differs.
pub const PROTOCOL_VERSION: u32 = 1;

/// The path prefix of every HTTP command.
pub const API_PREFIX: &str = "/api/v1";

/// The path of the event WebSocket.
pub const EVENTS_PATH: &str = "/api/v1/events";

/// The name of the session cookie. The `__Host-` prefix makes browsers
/// accept it only when it is `Secure`, has `Path=/` and names no `Domain`,
/// so a sibling host cannot plant a session cookie for the gateway.
pub const SESSION_COOKIE: &str = "__Host-bokkie_session";

/// Milliseconds since the Unix epoch, in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub i64);

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

id_type!(
    /// A Bokkie account.
    UserId
);
id_type!(
    /// One of a user's Bots. Each Bot has exactly one conversation.
    BotId
);
id_type!(
    /// A message in a Bot's conversation.
    MessageId
);
