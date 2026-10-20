//! Where the signed-in screens stand in Stobie's navigation recipe: the Bot
//! list is the list pane; the open conversation, the household's ChatGPT
//! sign-in or the admin section is the content pane, one at a time; and
//! the open Bot's settings are the detail pane, shown or hidden from the
//! conversation's header. The layout follows the width the page last had,
//! and what is open follows only the state, so a resize loses nothing.

use bokkie_protocol::api::Bot;
use stobie_ui_egui::{NavigationLayout, NavigationPane, Observation};

use crate::app::{App, Session};

impl App {
    /// The navigation's layout at the width of the last pass.
    pub(crate) fn layout(&self) -> NavigationLayout {
        self.layout
    }

    /// What the content pane holds, as the `content` fact names it:
    /// `conversation`, `household`, `admin`, or empty while nothing is open.
    pub(crate) fn content(&self) -> &'static str {
        let Session::SignedIn(signed_in) = &self.session else {
            return "";
        };
        if signed_in.admin.is_some() {
            "admin"
        } else if signed_in.household.is_some() {
            "household"
        } else if signed_in.open.is_some() {
            "conversation"
        } else {
            ""
        }
    }

    /// Whether the open Bot's settings are shown, in the detail pane.
    pub(crate) fn detail_shown(&self) -> bool {
        self.content() == "conversation" && self.settings().is_some()
    }

    /// The deepest level open: the list, the content or the detail.
    pub(crate) fn level(&self) -> NavigationPane {
        if self.detail_shown() {
            NavigationPane::Detail
        } else if self.content().is_empty() {
            NavigationPane::List
        } else {
            NavigationPane::Content
        }
    }

    /// The panes on screen, from the start of the line, as the recipe lays
    /// them out for this layout and level.
    pub(crate) fn panes(&self) -> Vec<NavigationPane> {
        use NavigationPane::{Content, Detail, Empty, List};
        if !matches!(self.session, Session::SignedIn(_)) {
            return Vec::new();
        }
        let level = self.level();
        match (self.layout, level) {
            (NavigationLayout::Stack, level) => vec![level],
            (_, List) => vec![List, Empty],
            (NavigationLayout::Split, level) => vec![List, level],
            (NavigationLayout::Columns, Detail) => vec![List, Content, Detail],
            (NavigationLayout::Columns, level) => vec![List, level],
        }
    }

    /// The pane that carries the notices, the connection's state and the
    /// limit banner, so they show once: the content when it is on screen,
    /// the list otherwise, and none while a phone shows the settings.
    pub(crate) fn banner_pane(&self) -> Option<NavigationPane> {
        let panes = self.panes();
        [NavigationPane::Content, NavigationPane::List]
            .into_iter()
            .find(|pane| panes.contains(pane))
    }

    /// The Bots the list shows: hidden ones too while Show hidden is on.
    pub(crate) fn shown_bots(&self) -> Vec<&Bot> {
        match &self.session {
            Session::SignedIn(signed_in) if signed_in.show_hidden => self.store().listed_bots(),
            Session::SignedIn(_) => self.store().visible_bots(),
            _ => Vec::new(),
        }
    }

    /// The pinned Bots in the strip above the list, as the `strip` fact
    /// lists them: only on a phone, while the list is on screen.
    pub(crate) fn strip(&self) -> Vec<&Bot> {
        if self.layout != NavigationLayout::Stack || self.panes() != [NavigationPane::List] {
            return Vec::new();
        }
        let mut bots = self.shown_bots();
        bots.retain(|bot| bot.pinned);
        bots
    }

    /// Whether the open conversation is on screen, for the read marker and
    /// the `viewing` frames.
    pub(crate) fn conversation_visible(&self) -> bool {
        match self.screen() {
            "conversation" => true,
            "bot_settings" => self.panes().contains(&NavigationPane::Content),
            _ => false,
        }
    }

    /// Close whatever the content pane holds: the open conversation with
    /// its settings, the household's sign-in with its code, or the admin
    /// section.
    pub(crate) fn close_content(&mut self) {
        let Session::SignedIn(signed_in) = &mut self.session else {
            return;
        };
        signed_in.household = None;
        signed_in.admin = None;
        if let Some(open) = signed_in.open.take() {
            self.store.close_conversation(open.bot);
            self.store.close_routines(open.bot);
        }
    }

    /// The navigation's facts: `layout`, `content`, `detail` and `panes`.
    pub(crate) fn observe_navigation(&self, observation: &mut Observation<'_>) {
        let signed_in = matches!(self.session, Session::SignedIn(_));
        observation.fact("layout", if signed_in { self.layout.as_str() } else { "" });
        observation.fact("content", self.content());
        observation.fact("detail", if self.detail_shown() { "settings" } else { "" });
        let panes: Vec<&str> = self.panes().into_iter().map(pane_name).collect();
        observation.fact("panes", panes.join(","));
    }
}

/// A pane's name in the `panes` fact.
pub(crate) fn pane_name(pane: NavigationPane) -> &'static str {
    match pane {
        NavigationPane::List => "list",
        NavigationPane::Content => "content",
        NavigationPane::Detail => "detail",
        NavigationPane::Empty => "empty",
    }
}
