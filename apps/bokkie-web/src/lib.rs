//! Bokkie's web client.
//!
//! So far this is the client's platform-independent core, which the browser
//! application builds on and which is tested natively:
//!
//! - [`wire`] decodes the gateway's event frames and HTTP responses,
//!   including its error shape, and encodes the client's frames;
//! - [`store`] merges HTTP snapshots with the event log, assembling streaming
//!   replies by character offset;
//! - [`connection`] is the event WebSocket's reconnect policy: resume from the
//!   last event processed, back off from 250 ms to 8 s, retry at once when
//!   the page is visible again, reload on `reset`, and stop when signed out.
//!
//! None of it performs I/O. The transports report what happened and carry
//! out the commands.

pub mod connection;
pub mod store;
pub mod wire;
