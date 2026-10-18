//! Bokkie's web client, a Stobie application.
//!
//! The application declares its state, actions, UI and how it applies
//! changes; the Stobie host owns observation, shortcuts, agent inspection and
//! repaint. The browser is the only transport target: a native build exists
//! only so that host tests can run.
//!
//! - [`app`] is the application: sign-in and sign-out, the Bot list, and
//!   one Bot's conversation, kept current by the event socket.
//! - [`api`] describes each HTTP command and decodes its reply;
//!   `browser` sends them with `fetch` and runs the event WebSocket.
//!
//! Its platform-independent core performs no I/O and is tested natively:
//!
//! - [`wire`] decodes the gateway's event frames and HTTP responses,
//!   including its error shape, and encodes the client's frames;
//! - [`store`] merges HTTP snapshots with the event log, assembling streaming
//!   replies by character offset;
//! - [`connection`] is the event WebSocket's reconnect policy: resume from the
//!   last event processed, back off from 250 ms to 8 s, retry at once when
//!   the page is visible again, reload on `reset`, and stop when signed out;
//! - [`socket`] drives that policy for a transport, which only performs its
//!   I/O, and filters what reaches the application;
//! - [`usage`] words the usage meter and the limit-reached banner.

pub mod api;
pub mod app;
#[cfg(target_arch = "wasm32")]
pub mod browser;
pub mod connection;
pub mod socket;
pub mod store;
mod ui;
pub mod usage;
pub mod wire;

pub use app::{APPLICATION_NAME, App, BokkieAction, BokkieIntent};

// The browser entry point: `stobie build-web` builds this library for the web.
#[cfg(target_arch = "wasm32")]
stobie_ui_egui::web_handle!(App, |_| App::new(Box::new(|intents| Box::new(
    browser::BrowserApi::new(intents)
))));

#[cfg(test)]
mod tests;
