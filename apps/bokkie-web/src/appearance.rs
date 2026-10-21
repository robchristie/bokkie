//! How the client looks: light or dark as the system is, at the text size
//! the preferences give. Stobie's own default is dark whatever the system
//! says, so the client asks for the system's appearance; the `theme` fact
//! says which the page follows now.

use stobie_ui_egui::{AppearancePreference, Observation, UiPreferences};

use crate::app::App;

/// The preferences the client starts with: the system's appearance, and
/// Stobie's defaults otherwise.
pub(crate) fn preferences() -> UiPreferences {
    UiPreferences {
        appearance: AppearancePreference::System,
        ..UiPreferences::default()
    }
}

impl App {
    /// The `theme` fact: `light` or `dark`, as the page was last drawn.
    pub(crate) fn observe_appearance(&self, observation: &mut Observation<'_>) {
        observation.fact(
            "theme",
            match self.theme {
                egui::Theme::Light => "light",
                egui::Theme::Dark => "dark",
            },
        );
    }
}
