//! Bokkie's web client, a Stobie application.
//!
//! The application declares its state, actions, UI and how it applies
//! changes; the Stobie host owns observation, shortcuts, agent inspection and
//! repaint. The browser is the only transport target: a native build exists
//! only so that host tests can run.
//!
//! Its platform-independent core performs no I/O and is tested natively:
//!
//! - [`wire`] decodes the gateway's event frames and HTTP responses,
//!   including its error shape, and encodes the client's frames;
//! - [`store`] merges HTTP snapshots with the event log, assembling streaming
//!   replies by character offset;
//! - [`connection`] is the event WebSocket's reconnect policy: resume from the
//!   last event processed, back off from 250 ms to 8 s, retry at once when
//!   the page is visible again, reload on `reset`, and stop when signed out.
//!
//! The transports report what happened and carry out the core's commands.

pub mod connection;
pub mod store;
pub mod wire;

use serde::Serialize;
use stobie_ui_egui::{
    Action, ActionEmphasis, ActionTarget, Application, ApplicationIdentity, Availability, Frame,
    Observation, PresentationScope, SemanticUiId, application_identity,
};

/// The application's name, as the page and the host report it.
pub const APPLICATION_NAME: &str = "Bokkie";

/// Every user action, declared once. Buttons, shortcuts and agents share
/// them; IDs are `bokkie.<kebab-case variant>`.
#[derive(Action, Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[action(prefix = "bokkie")]
pub enum BokkieAction {
    #[action(label = "Reload", description = "Reload Bokkie from the server")]
    Reload,
}

/// Data changes from the UI and from transports.
#[derive(Debug)]
pub enum BokkieIntent {}

/// The application state. The UI only reads it; `apply` and `perform`
/// change it.
#[derive(Debug, Default)]
pub struct App {
    status: String,
}

impl App {
    pub fn new() -> Self {
        Self {
            status: "Starting".into(),
        }
    }

    pub fn status(&self) -> &str {
        &self.status
    }
}

impl Application for App {
    type Action = BokkieAction;
    type Intent = BokkieIntent;
    type UiState = ();

    fn identity(&self) -> ApplicationIdentity {
        application_identity!(APPLICATION_NAME)
    }

    fn availability(&self, target: ActionTarget<BokkieAction>) -> Availability {
        match target.action {
            BokkieAction::Reload => Availability::Enabled,
        }
    }

    fn ui(&self, ui: &mut egui::Ui, _: &mut (), frame: &mut Frame<'_, BokkieAction, BokkieIntent>) {
        egui::CentralPanel::default().show(ui, |ui| {
            let mut p =
                frame.presentation(ui, PresentationScope::new("bokkie"), SemanticUiId::root());
            p.heading(ui, "title", APPLICATION_NAME);
            p.status(ui, "status", &self.status, false);
            frame.action(ui, &mut p, BokkieAction::Reload, ActionEmphasis::Normal);
        });
    }

    fn apply(&mut self, intent: BokkieIntent, _: &mut ()) -> Result<bool, String> {
        match intent {}
    }

    fn perform(&mut self, target: ActionTarget<BokkieAction>, _: &mut ()) -> Result<bool, String> {
        match target.action {
            BokkieAction::Reload => reload_page().map(|()| false),
        }
    }

    fn report_error(&mut self, message: String) {
        self.status = message;
    }

    fn observe(&self, observation: &mut Observation<'_>) {
        observation.fact("status", self.status.as_str());
    }
}

/// Reload the page, which fetches the newest client the server holds.
#[cfg(target_arch = "wasm32")]
fn reload_page() -> Result<(), String> {
    web_sys::window()
        .ok_or_else(|| "There is no page to reload".to_owned())?
        .location()
        .reload()
        .map_err(|_| "The page could not be reloaded".to_owned())
}

#[cfg(not(target_arch = "wasm32"))]
fn reload_page() -> Result<(), String> {
    Err("Reloading needs the browser".into())
}

// The browser entry point: `stobie build-web` builds this library for the web.
#[cfg(target_arch = "wasm32")]
stobie_ui_egui::web_handle!(App, |_| App::new());

#[cfg(test)]
mod tests;
