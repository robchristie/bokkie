use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui;
use polyorama_core::{DockNodeId, PaneId, Workspace};
use polyorama_ui_egui::{
    ActionButtonSpec, ActionEmphasis, ActionKey, ActionScope, ActionSpec, ActionTarget,
    Availability, DesignTokens, DockBehaviour, DockTextContext, PanePresenter, TextInteraction,
    TextLayoutObservation, TextOverflow, TextRole, UiPreferences, action_button,
    apply_design_system, diagnostic_row, dock_workspace, measured_content_label, section_heading,
};
use serde::Serialize;

use crate::{
    APPLICATION_NAME,
    model::{
        ATTENTION_PANE_ID, AttentionIntent, AuditEventReadModel, ObligationReadModel,
        fixed_workspace,
    },
    transport::{ApiMessage, ApiPayload, ApiRequest, Transport},
};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
enum AttentionAction {
    Refresh,
    Cancel,
}

impl ActionKey for AttentionAction {
    fn stable_id(self) -> &'static str {
        match self {
            Self::Refresh => "refresh_attention",
            Self::Cancel => "cancel_obligation",
        }
    }

    fn specification(self) -> ActionSpec<Self> {
        match self {
            Self::Refresh => ActionSpec {
                id: self,
                label: "Refresh",
                description: "Read current obligation and audit state",
                compact_label: None,
                shortcut: None,
                scope: ActionScope::Pane,
            },
            Self::Cancel => ActionSpec {
                id: self,
                label: "Cancel fixture",
                description: "Cancel this non-terminal fixture through Bokkie",
                compact_label: Some("Cancel"),
                shortcut: None,
                scope: ActionScope::Pane,
            },
        }
    }
}

pub struct AttentionApp {
    workspace: Workspace,
    dock: DockBehaviour,
    transport: Option<Transport>,
    sender: Sender<ApiMessage>,
    receiver: Receiver<ApiMessage>,
    obligation: Option<ObligationReadModel>,
    events: Vec<AuditEventReadModel>,
    busy: bool,
    status: String,
    error: Option<String>,
    preferences: UiPreferences,
}

impl AttentionApp {
    pub fn new(creation: &eframe::CreationContext<'_>) -> Self {
        let preferences = UiPreferences::default();
        apply_design_system(&creation.egui_ctx, preferences);
        let (sender, receiver) = mpsc::channel();
        #[cfg(not(target_arch = "wasm32"))]
        let transport =
            std::env::var("BOKKIE_API_BASE").unwrap_or_else(|_| "http://127.0.0.1:7744".to_owned());
        #[cfg(not(target_arch = "wasm32"))]
        let (transport, error) = match Transport::new(&transport) {
            Ok(transport) => (Some(transport), None),
            Err(error) => (None, Some(error)),
        };
        #[cfg(target_arch = "wasm32")]
        let (transport, error) = (Some(Transport::new()), None);
        let mut app = Self {
            workspace: fixed_workspace(),
            dock: DockBehaviour::default(),
            transport,
            sender,
            receiver,
            obligation: None,
            events: Vec::new(),
            busy: false,
            status: "Connecting to Bokkie".to_owned(),
            error,
            preferences,
        };
        app.dispatch(ApiRequest::List, &creation.egui_ctx);
        app
    }

    fn dispatch(&mut self, request: ApiRequest, context: &egui::Context) {
        let Some(transport) = &self.transport else {
            self.status = "Transport unavailable".to_owned();
            return;
        };
        self.busy = true;
        self.error = None;
        transport.send(request, self.sender.clone(), context.clone());
    }

    fn poll_transport(&mut self, context: &egui::Context) {
        while let Ok(message) = self.receiver.try_recv() {
            self.busy = false;
            match message.result {
                Ok(ApiPayload::Obligations(obligations)) => {
                    self.obligation = obligations.into_iter().next();
                    self.status = self.obligation.as_ref().map_or_else(
                        || "No obligations in this temporary service".to_owned(),
                        |obligation| format!("Read {} from Bokkie", obligation.id),
                    );
                    if let Some(obligation) = &self.obligation {
                        self.dispatch(
                            ApiRequest::Events {
                                obligation_id: obligation.id.clone(),
                            },
                            context,
                        );
                    } else {
                        self.events.clear();
                    }
                }
                Ok(ApiPayload::Events(events)) => {
                    self.events = events;
                    if let Some(last) = self.events.last() {
                        self.status = format!("Observed durable {} event", last.event_type);
                    }
                }
                Ok(ApiPayload::Cancelled(obligation)) => {
                    self.status = format!("Cancellation accepted for {}", obligation.id);
                    self.obligation = Some(obligation);
                    self.dispatch(ApiRequest::List, context);
                }
                Err(error) => {
                    self.error = Some(format!("{}: {error}", request_label(&message.request)));
                    self.status = "Bokkie request failed".to_owned();
                }
            }
        }
    }

    fn apply_intents(&mut self, intents: Vec<AttentionIntent>, context: &egui::Context) {
        for intent in intents {
            match intent {
                AttentionIntent::Refresh => self.dispatch(ApiRequest::List, context),
                AttentionIntent::Cancel { obligation_id } => {
                    self.dispatch(ApiRequest::Cancel { obligation_id }, context)
                }
            }
        }
    }
}

impl eframe::App for AttentionApp {
    fn ui(&mut self, root_ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = root_ui.ctx().clone();
        self.poll_transport(&context);
        let tokens = self
            .preferences
            .tokens(context.theme() == egui::Theme::Dark);
        egui::Panel::top("bokkie-application-bar")
            .frame(polyorama_ui_egui::application_bar_frame(&tokens))
            .exact_size(polyorama_ui_egui::application_bar_height(
                &tokens,
                self.preferences.font_scale,
            ))
            .show(root_ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.heading(APPLICATION_NAME);
                    ui.separator();
                    ui.label(&self.status);
                });
            });
        let obligation = self.obligation.clone();
        let events = self.events.clone();
        let error = self.error.clone();
        let mut intents = Vec::new();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(root_ui, |ui| {
                let read = AttentionReadModel {
                    obligation: obligation.as_ref(),
                    events: &events,
                    busy: self.busy,
                    error: error.as_deref(),
                };
                let mut pane = AttentionPane {
                    read,
                    intents: &mut intents,
                    tokens,
                    font_scale: self.preferences.font_scale,
                    text: Vec::new(),
                };
                let _ = dock_workspace(
                    ui,
                    &mut self.workspace,
                    &mut self.dock,
                    &mut pane,
                    DockTextContext {
                        tokens,
                        font_scale: self.preferences.font_scale,
                    },
                );
            });
        self.apply_intents(intents, &context);
    }
}

struct AttentionReadModel<'a> {
    obligation: Option<&'a ObligationReadModel>,
    events: &'a [AuditEventReadModel],
    busy: bool,
    error: Option<&'a str>,
}

struct AttentionPane<'a> {
    read: AttentionReadModel<'a>,
    intents: &'a mut Vec<AttentionIntent>,
    tokens: DesignTokens,
    font_scale: f32,
    text: Vec<TextLayoutObservation>,
}

impl PanePresenter for AttentionPane<'_> {
    fn title(&self, pane: PaneId) -> &'static str {
        if pane == ATTENTION_PANE_ID {
            "Attention probe"
        } else {
            "Unknown pane"
        }
    }

    fn pane_ui(&mut self, ui: &mut egui::Ui, pane: PaneId, _pane_rect: egui::Rect) {
        if pane != ATTENTION_PANE_ID {
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("attention-pane-scroll")
            .show(ui, |ui| {
                ui.add_space(self.tokens.spacing.section.0);
                section_heading(
                    ui,
                    1,
                    "Temporary service fixture",
                    &self.tokens,
                    self.font_scale,
                    &mut self.text,
                );
                if let Some(error) = self.read.error {
                    measured_content_label(
                        ui,
                        2,
                        error,
                        TextRole::Error,
                        TextOverflow::Wrap,
                        3,
                        TextInteraction::Selectable,
                        &self.tokens,
                        self.font_scale,
                        &mut self.text,
                    );
                }
                if let Some(obligation) = self.read.obligation {
                    measured_content_label(
                        ui,
                        3,
                        &obligation.description,
                        TextRole::Body,
                        TextOverflow::Wrap,
                        2,
                        TextInteraction::Selectable,
                        &self.tokens,
                        self.font_scale,
                        &mut self.text,
                    );
                    diagnostic_row(
                        ui,
                        4,
                        "ID",
                        &obligation.id,
                        &self.tokens,
                        self.font_scale,
                        &mut self.text,
                    );
                    diagnostic_row(
                        ui,
                        5,
                        "State",
                        obligation.state.label(),
                        &self.tokens,
                        self.font_scale,
                        &mut self.text,
                    );
                    diagnostic_row(
                        ui,
                        6,
                        "Occurrence",
                        &obligation.occurrence.to_string(),
                        &self.tokens,
                        self.font_scale,
                        &mut self.text,
                    );
                    diagnostic_row(
                        ui,
                        7,
                        "Scheduled",
                        &obligation.scheduled_at.to_string(),
                        &self.tokens,
                        self.font_scale,
                        &mut self.text,
                    );
                    diagnostic_row(
                        ui,
                        8,
                        "Approval",
                        if obligation.approval_required {
                            "required"
                        } else {
                            "not required"
                        },
                        &self.tokens,
                        self.font_scale,
                        &mut self.text,
                    );
                    ui.horizontal(|ui| {
                        if pane_action(
                            ui,
                            AttentionAction::Refresh,
                            !self.read.busy,
                            pane,
                            &self.tokens,
                            self.font_scale,
                            &mut self.text,
                        )
                        .clicked()
                            && !self.read.busy
                        {
                            self.intents.push(AttentionIntent::Refresh);
                        }
                        let can_cancel = obligation.can_cancel() && !self.read.busy;
                        if pane_action(
                            ui,
                            AttentionAction::Cancel,
                            can_cancel,
                            pane,
                            &self.tokens,
                            self.font_scale,
                            &mut self.text,
                        )
                        .clicked()
                            && can_cancel
                        {
                            self.intents.push(AttentionIntent::Cancel {
                                obligation_id: obligation.id.clone(),
                            });
                        }
                    });
                    ui.add_space(self.tokens.spacing.section.0);
                    section_heading(
                        ui,
                        9,
                        "Durable audit",
                        &self.tokens,
                        self.font_scale,
                        &mut self.text,
                    );
                    if let Some(event) = self.read.events.last() {
                        diagnostic_row(
                            ui,
                            10,
                            "Latest event",
                            &event.event_type,
                            &self.tokens,
                            self.font_scale,
                            &mut self.text,
                        );
                        diagnostic_row(
                            ui,
                            11,
                            "Sequence",
                            &event.sequence.to_string(),
                            &self.tokens,
                            self.font_scale,
                            &mut self.text,
                        );
                        diagnostic_row(
                            ui,
                            12,
                            "Transition",
                            &format!("{:?} → {}", event.from_state, event.to_state.label()),
                            &self.tokens,
                            self.font_scale,
                            &mut self.text,
                        );
                    } else {
                        measured_content_label(
                            ui,
                            13,
                            "No audit events loaded",
                            TextRole::Secondary,
                            TextOverflow::Ellipsis,
                            1,
                            TextInteraction::Inert,
                            &self.tokens,
                            self.font_scale,
                            &mut self.text,
                        );
                    }
                } else if self.read.error.is_none() {
                    measured_content_label(
                        ui,
                        14,
                        if self.read.busy {
                            "Loading obligations…"
                        } else {
                            "No obligations found"
                        },
                        TextRole::Secondary,
                        TextOverflow::Ellipsis,
                        1,
                        TextInteraction::Inert,
                        &self.tokens,
                        self.font_scale,
                        &mut self.text,
                    );
                }
            });
    }

    fn record_text_layout(&mut self, observation: TextLayoutObservation) {
        self.text.push(observation);
    }

    fn record_splitter_rect(
        &mut self,
        _node: DockNodeId,
        _rect: egui::Rect,
        _horizontal: bool,
        _focused: bool,
    ) {
    }
}

fn pane_action(
    ui: &mut egui::Ui,
    action: AttentionAction,
    enabled: bool,
    pane: PaneId,
    tokens: &DesignTokens,
    font_scale: f32,
    text: &mut Vec<TextLayoutObservation>,
) -> egui::Response {
    action_button(
        ui,
        ActionButtonSpec {
            target: ActionTarget::pane(action, pane),
            availability: if enabled {
                Availability::Enabled
            } else {
                Availability::Disabled {
                    reason: "Request in progress or lifecycle action unavailable".into(),
                }
            },
            selected: false,
            emphasis: if action == AttentionAction::Cancel {
                ActionEmphasis::Primary
            } else {
                ActionEmphasis::Quiet
            },
            compact: false,
        },
        tokens,
        font_scale,
        text,
    )
}

fn request_label(request: &ApiRequest) -> &'static str {
    match request {
        ApiRequest::List => "Read obligations",
        ApiRequest::Events { .. } => "Read audit events",
        ApiRequest::Cancel { .. } => "Cancel obligation",
    }
}
