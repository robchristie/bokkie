use std::{
    cell::RefCell,
    rc::Rc,
    sync::mpsc::{self, Receiver, Sender},
    time::Duration,
};
use web_time::Instant;

use bokkie_operator_api::{
    ApprovalSubject, AttentionCause, DurableLiveness, ExceptionReason, ObligationTopic,
    OperatorObligation, TopicItem, TopicSource,
};
use eframe::egui;
use polyorama_core::{DockNodeId, PaneId, Workspace, virtual_rows};
use polyorama_ui_egui::{
    ActionButtonSpec, ActionEmphasis, ActionKey, ActionScope, ActionSpec, ActionTarget,
    Availability, DesignTokens, DockBehaviour, DockTextContext, NativeTextControlKind,
    PanePresenter, SemanticUiId, StatusTone, TextInteraction, TextLayoutObservation, TextOverflow,
    TextRole, UiNode, UiPreferences, UiRole, action_button, action_semantic_node,
    application_bar_frame, application_bar_height, apply_design_system, dock_workspace,
    measured_content_label, property_row, record_native_text_control, section_heading,
    status_badge,
};
use serde::Serialize;
use serde_json::Value;

use crate::{
    APPLICATION_NAME,
    model::{
        AppModel, Confirmation, ConnectionState, GardenerConfirmation, INBOX_PANE_ID,
        LifecycleAction, OBLIGATIONS_PANE_ID, OperatorStateLabel, StateFilter, TIMELINE_PANE_ID,
        consequence_label, operator_workspace,
    },
    transport::{
        ActionRequest, ApiFailure, ApiMessage, ApiPayload, ApiRequest, ApiSession, Transport,
    },
    ui_observation::{
        InteractionObservation, TestSnapshot, VirtualisationObservation, finish_snapshot, root_node,
    },
};

const NARROW_WORKSPACE_WIDTH: f32 = 760.0;
const POLL_MAX: Duration = Duration::from_secs(30);
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
enum ShellAction {
    Refresh,
}

impl ActionKey for ShellAction {
    fn stable_id(self) -> &'static str {
        "refresh_operator_state"
    }

    fn specification(self) -> ActionSpec<Self> {
        ActionSpec {
            id: self,
            label: "Refresh",
            description: "Read a current snapshot and selected obligation topic",
            compact_label: None,
            shortcut: None,
            scope: ActionScope::Application,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
enum ConfirmationAction {
    Submit,
    Dismiss,
}

impl ActionKey for ConfirmationAction {
    fn stable_id(self) -> &'static str {
        match self {
            Self::Submit => "confirm_lifecycle_action",
            Self::Dismiss => "dismiss_lifecycle_confirmation",
        }
    }

    fn specification(self) -> ActionSpec<Self> {
        match self {
            Self::Submit => ActionSpec {
                id: self,
                label: "Confirm action",
                description: "Submit this deliberately reviewed lifecycle action to Bokkie",
                compact_label: None,
                shortcut: None,
                scope: ActionScope::Application,
            },
            Self::Dismiss => ActionSpec {
                id: self,
                label: "Keep state unchanged",
                description: "Close this confirmation without submitting a lifecycle action",
                compact_label: None,
                shortcut: None,
                scope: ActionScope::Application,
            },
        }
    }
}

impl ActionKey for LifecycleAction {
    fn stable_id(self) -> &'static str {
        match self {
            Self::Approve => "approve_obligation",
            Self::Reject => "reject_obligation",
            Self::Retry => "retry_obligation",
            Self::Cancel => "cancel_obligation",
            Self::ApproveGardenerProposal => "approve_exact_gardener_proposal",
            Self::RejectGardenerProposal => "reject_exact_gardener_proposal",
        }
    }

    fn specification(self) -> ActionSpec<Self> {
        let (label, description, compact_label) = match self {
            Self::Approve => ("Approve", "Approve the current generic occurrence", None),
            Self::Reject => (
                "Reject",
                "Reject the current generic occurrence into attention",
                None,
            ),
            Self::Retry => ("Retry", "Reopen eligible attention work for retry", None),
            Self::Cancel => ("Cancel", "Cancel eligible non-terminal work", None),
            Self::ApproveGardenerProposal => (
                "Approve exact proposal",
                "Approve only the displayed immutable gardener proposal",
                Some("Approve proposal"),
            ),
            Self::RejectGardenerProposal => (
                "Reject exact proposal",
                "Reject only the displayed immutable gardener proposal",
                Some("Reject proposal"),
            ),
        };
        ActionSpec {
            id: self,
            label,
            description,
            compact_label,
            shortcut: None,
            scope: ActionScope::Pane,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum OperatorIntent {
    Refresh,
    Select {
        obligation_id: String,
        destination: Option<PaneId>,
    },
    Navigate(PaneId),
    Search(String),
    Filter(StateFilter),
    BeginAction(LifecycleAction),
    UpdateConfirmation {
        actor: String,
        note: String,
    },
    DismissConfirmation,
    SubmitConfirmation,
}

#[derive(Debug)]
struct TopicRequestGate {
    next_generation: u64,
    latest: Option<(String, u64)>,
    pending: usize,
}

impl Default for TopicRequestGate {
    fn default() -> Self {
        Self {
            next_generation: 1,
            latest: None,
            pending: 0,
        }
    }
}

impl TopicRequestGate {
    fn begin(&mut self, obligation_id: String) -> u64 {
        let generation = self.next_generation;
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .expect("topic request generation exhausted");
        self.latest = Some((obligation_id, generation));
        self.pending = self.pending.saturating_add(1);
        generation
    }

    fn finish(&mut self, obligation_id: &str, generation: u64) -> bool {
        self.pending = self.pending.saturating_sub(1);
        self.latest
            .as_ref()
            .is_some_and(|(latest_id, latest)| latest_id == obligation_id && *latest == generation)
    }
}

pub struct AttentionApp {
    workspace: Workspace,
    dock: DockBehaviour,
    model: AppModel,
    transport: Option<Transport>,
    session: Option<ApiSession>,
    sender: Sender<ApiMessage>,
    receiver: Receiver<ApiMessage>,
    preferences: UiPreferences,
    next_poll_at: Option<Instant>,
    topic_requests: TopicRequestGate,
    frame_number: u64,
    last_test_snapshot: TestSnapshot,
    test_observer: Option<Rc<RefCell<TestSnapshot>>>,
}

impl AttentionApp {
    pub fn new(creation: &eframe::CreationContext<'_>) -> Self {
        Self::new_observed(creation, None)
    }

    pub(crate) fn new_observed(
        creation: &eframe::CreationContext<'_>,
        test_observer: Option<Rc<RefCell<TestSnapshot>>>,
    ) -> Self {
        let preferences = UiPreferences::default();
        apply_design_system(&creation.egui_ctx, preferences);
        let (sender, receiver) = mpsc::channel();
        #[cfg(not(target_arch = "wasm32"))]
        let base =
            std::env::var("BOKKIE_API_BASE").unwrap_or_else(|_| "http://127.0.0.1:7744".to_owned());
        #[cfg(not(target_arch = "wasm32"))]
        let (transport, transport_error) = match Transport::new(&base) {
            Ok(transport) => (Some(transport), None),
            Err(error) => (None, Some(error)),
        };
        #[cfg(target_arch = "wasm32")]
        let (transport, transport_error) = (Some(Transport::new()), None::<String>);
        let mut model = AppModel::default();
        if let Some(error) = transport_error {
            model.mark_stale(error);
        }
        let mut app = Self {
            workspace: operator_workspace(),
            dock: DockBehaviour::default(),
            model,
            transport,
            session: None,
            sender,
            receiver,
            preferences,
            next_poll_at: None,
            topic_requests: TopicRequestGate::default(),
            frame_number: 0,
            last_test_snapshot: TestSnapshot::default(),
            test_observer,
        };
        app.dispatch(ApiRequest::Bootstrap, &creation.egui_ctx);
        app
    }

    pub fn test_snapshot(&self) -> TestSnapshot {
        self.last_test_snapshot.clone()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn write_native_test_snapshot(&self) {
        let Ok(path) = std::env::var("BOKKIE_UI_TEST_SNAPSHOT_PATH") else {
            return;
        };
        if let Ok(json) = serde_json::to_vec_pretty(&self.last_test_snapshot) {
            let temporary = format!("{path}.tmp");
            if std::fs::write(&temporary, json).is_ok() {
                let _ = std::fs::rename(temporary, path);
            }
        }
    }

    fn dispatch(&mut self, request: ApiRequest, context: &egui::Context) {
        let Some(transport) = &self.transport else {
            self.model.mark_stale("Transport is unavailable");
            return;
        };
        match &request {
            ApiRequest::Bootstrap => self.model.snapshot_busy = true,
            ApiRequest::Snapshot => self.model.snapshot_busy = true,
            ApiRequest::Topic { .. } => self.model.topic_busy = true,
            ApiRequest::Act(_) => self.model.action_busy = true,
        }
        transport.send(
            request,
            self.session.as_ref(),
            self.sender.clone(),
            context.clone(),
        );
    }

    fn poll_transport(&mut self, context: &egui::Context) {
        while let Ok(message) = self.receiver.try_recv() {
            match (message.request, message.result) {
                (ApiRequest::Bootstrap, Ok(ApiPayload::Bootstrap(session))) => {
                    self.session = Some(session);
                    self.dispatch(ApiRequest::Snapshot, context);
                }
                (ApiRequest::Snapshot, Ok(ApiPayload::Snapshot(snapshot))) => {
                    let poll = poll_delay(&snapshot);
                    self.model.apply_snapshot(snapshot);
                    self.next_poll_at = Some(Instant::now() + poll);
                    if let Some(obligation_id) = self.model.selected_obligation.clone() {
                        self.request_topic(obligation_id, context);
                    }
                }
                (
                    ApiRequest::Topic {
                        obligation_id,
                        generation,
                    },
                    Ok(ApiPayload::Topic(topic)),
                ) => {
                    let current = self.finish_topic_request(&obligation_id, generation);
                    if current && self.model.selected_obligation.as_deref() == Some(&obligation_id)
                    {
                        self.model.topic = Some(topic);
                        self.model.topic_error = None;
                    }
                }
                (ApiRequest::Act(action), Ok(ApiPayload::ActionAccepted)) => {
                    self.model
                        .record_action_accepted(action.action.specification().label);
                    self.dispatch(ApiRequest::Snapshot, context);
                }
                (ApiRequest::Act(_), Err(ApiFailure::Conflict(message))) => {
                    self.model.record_transition_conflict(&message);
                    self.dispatch(ApiRequest::Snapshot, context);
                }
                (_, Err(ApiFailure::SessionChanged(message))) => {
                    self.session = None;
                    self.model.record_session_change(&message);
                    self.next_poll_at = None;
                    self.dispatch(ApiRequest::Bootstrap, context);
                }
                (ApiRequest::Bootstrap, Err(error)) => {
                    self.session = None;
                    self.model
                        .mark_stale(format!("Session bootstrap failed: {error}"));
                    self.next_poll_at = Some(Instant::now() + RECONNECT_DELAY);
                }
                (ApiRequest::Snapshot, Err(error)) => {
                    self.model
                        .mark_stale(format!("Snapshot refresh failed: {error}"));
                    self.next_poll_at = Some(Instant::now() + RECONNECT_DELAY);
                }
                (
                    ApiRequest::Topic {
                        obligation_id,
                        generation,
                    },
                    Err(error),
                ) => {
                    let current = self.finish_topic_request(&obligation_id, generation);
                    if current && self.model.selected_obligation.as_deref() == Some(&obligation_id)
                    {
                        self.model.topic_error = Some(format!("Topic refresh failed: {error}"));
                        self.model
                            .mark_stale(format!("Selected topic could not be refreshed: {error}"));
                        self.next_poll_at = Some(Instant::now() + RECONNECT_DELAY);
                    }
                }
                (ApiRequest::Act(_), Err(error)) => {
                    self.model.action_busy = false;
                    self.model
                        .mark_stale(format!("Lifecycle request failed: {error}"));
                    self.next_poll_at = Some(Instant::now() + RECONNECT_DELAY);
                }
                (_, Ok(_)) => {
                    self.model
                        .mark_stale("Bokkie returned an unexpected response");
                }
            }
        }
    }

    fn drive_polling(&mut self, context: &egui::Context) {
        let Some(deadline) = self.next_poll_at else {
            return;
        };
        let now = Instant::now();
        if now >= deadline && !self.model.snapshot_busy && !self.model.action_busy {
            self.next_poll_at = None;
            let request = if self.session.is_some() {
                ApiRequest::Snapshot
            } else {
                ApiRequest::Bootstrap
            };
            self.dispatch(request, context);
        } else if deadline > now {
            context.request_repaint_after(deadline.duration_since(now));
        }
    }

    fn request_topic(&mut self, obligation_id: String, context: &egui::Context) {
        let generation = self.topic_requests.begin(obligation_id.clone());
        self.dispatch(
            ApiRequest::Topic {
                obligation_id,
                generation,
            },
            context,
        );
    }

    fn finish_topic_request(&mut self, obligation_id: &str, generation: u64) -> bool {
        let current = self.topic_requests.finish(obligation_id, generation);
        self.model.topic_busy = self.topic_requests.pending > 0;
        current
    }

    fn apply_intents(&mut self, intents: Vec<OperatorIntent>, context: &egui::Context) {
        for intent in intents {
            match intent {
                OperatorIntent::Refresh => {
                    self.next_poll_at = None;
                    let request = if self.session.is_some() {
                        ApiRequest::Snapshot
                    } else {
                        ApiRequest::Bootstrap
                    };
                    self.dispatch(request, context);
                }
                OperatorIntent::Select {
                    obligation_id,
                    destination,
                } => {
                    if self.model.select(obligation_id.clone()) || self.model.topic.is_none() {
                        self.request_topic(obligation_id, context);
                    }
                    if let Some(destination) = destination {
                        self.workspace.activate(destination);
                    }
                }
                OperatorIntent::Navigate(pane) => self.workspace.activate(pane),
                OperatorIntent::Search(search) => self.model.search = search,
                OperatorIntent::Filter(filter) => self.model.state_filter = filter,
                OperatorIntent::BeginAction(action) => {
                    if let Err(error) = self.model.begin_confirmation(action) {
                        self.model.status = error;
                    }
                    context.request_repaint();
                }
                OperatorIntent::UpdateConfirmation { actor, note } => {
                    if let Some(confirmation) = self.model.confirmation.as_mut() {
                        confirmation.actor = actor;
                        confirmation.note = note;
                    }
                }
                OperatorIntent::DismissConfirmation => {
                    self.model.confirmation = None;
                    context.request_repaint();
                }
                OperatorIntent::SubmitConfirmation => self.submit_confirmation(context),
            }
        }
    }

    fn submit_confirmation(&mut self, context: &egui::Context) {
        let Some(confirmation) = self.model.confirmation.clone() else {
            return;
        };
        if confirmation.action.requires_decision_body() && confirmation.actor.trim().is_empty() {
            self.model.status = "An operator actor is required".to_owned();
            return;
        }
        let Some(current) = self.model.selected() else {
            self.model.status = "The selected obligation is no longer present".to_owned();
            return;
        };
        if !self.model.confirmation_matches_current_state(&confirmation)
            || self
                .model
                .action_availability(confirmation.action, current)
                .is_err()
        {
            self.model.status =
                "The confirmation no longer matches current state; dismiss and review again"
                    .to_owned();
            return;
        }
        let current_instance_id =
            exact_gardener_subject(current).map(|subject| subject.instance_id);
        let confirmed_instance_id = confirmation
            .gardener
            .as_ref()
            .map(|gardener| gardener.instance_id.as_str());
        if confirmation.action.is_gardener() && current_instance_id != confirmed_instance_id {
            self.model.status =
                "The immutable proposal identity changed; dismiss and review again".to_owned();
            return;
        }
        self.dispatch(
            ApiRequest::Act(Box::new(ActionRequest {
                action: confirmation.action,
                obligation_id: confirmation.obligation_id,
                fingerprint: confirmation
                    .gardener
                    .as_ref()
                    .map(|gardener| gardener.fingerprint.clone()),
                proposal_instance_id: confirmation.gardener.map(|gardener| gardener.instance_id),
                precondition: confirmation.precondition,
                actor: confirmation.actor,
                note: confirmation.note,
            })),
            context,
        );
    }

    fn confirmation_submit_unavailable_reason(
        &self,
        confirmation: &Confirmation,
    ) -> Option<String> {
        if confirmation.action.requires_decision_body() && confirmation.actor.trim().is_empty() {
            return Some("An operator actor is required".to_owned());
        }
        let Some(current) = self.model.selected() else {
            return Some("The selected obligation is no longer present".to_owned());
        };
        if !self.model.confirmation_matches_current_state(confirmation) {
            return Some(
                "The confirmation no longer matches the selected current state".to_owned(),
            );
        }
        if let Err(reason) = self.model.action_availability(confirmation.action, current) {
            return Some(reason);
        }
        if confirmation.action.is_gardener()
            && exact_gardener_subject(current).map(|subject| subject.instance_id)
                != confirmation
                    .gardener
                    .as_ref()
                    .map(|gardener| gardener.instance_id.as_str())
        {
            return Some("The immutable proposal identity no longer matches".to_owned());
        }
        None
    }
}

impl eframe::App for AttentionApp {
    fn ui(&mut self, root_ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame_number = self.frame_number.saturating_add(1);
        let context = root_ui.ctx().clone();
        self.poll_transport(&context);
        self.drive_polling(&context);
        let tokens = self
            .preferences
            .tokens(context.theme() == egui::Theme::Dark);
        let mut intents = Vec::new();
        let mut semantic_nodes = vec![root_node(root_ui.max_rect())];
        let mut text_observations = Vec::new();
        let mut virtualisation = VirtualisationObservation::default();
        let bar = egui::Panel::top("bokkie-application-bar")
            .frame(application_bar_frame(&tokens))
            .exact_size(application_bar_height(&tokens, self.preferences.font_scale))
            .show(root_ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.heading(APPLICATION_NAME);
                    ui.separator();
                    let connection = connection_label(&self.model);
                    let compact_connection = compact_connection_label(&self.model);
                    ui.label(if ui.available_width() < 700.0 {
                        &compact_connection
                    } else {
                        &connection
                    })
                    .on_hover_text(&connection);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let availability = if self.model.action_busy {
                            Availability::Disabled {
                                reason: "A lifecycle request is in progress".into(),
                            }
                        } else if self.model.snapshot_busy {
                            Availability::Disabled {
                                reason: "A snapshot refresh is already in progress".into(),
                            }
                        } else {
                            Availability::Enabled
                        };
                        let target = ActionTarget::application(ShellAction::Refresh);
                        let response = action_button(
                            ui,
                            ActionButtonSpec {
                                target,
                                availability: availability.clone(),
                                selected: false,
                                emphasis: ActionEmphasis::Quiet,
                                compact: false,
                            },
                            &tokens,
                            self.preferences.font_scale,
                            &mut text_observations,
                        );
                        semantic_nodes.push(action_semantic_node(
                            &response,
                            target,
                            &availability,
                            false,
                            SemanticUiId::root(),
                        ));
                        if response.clicked() {
                            intents.push(OperatorIntent::Refresh);
                        }
                    });
                });
            });
        let mut bar_node = UiNode::container(
            SemanticUiId::new("bokkie.application-bar"),
            Some(SemanticUiId::root()),
            UiRole::ApplicationBar,
            bar.response.rect.into(),
        );
        bar_node.name = APPLICATION_NAME.to_owned();
        semantic_nodes.push(bar_node);

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(root_ui, |ui| {
                let narrow = ui.available_width() < NARROW_WORKSPACE_WIDTH;
                if narrow {
                    show_narrow_navigation(ui, self.workspace.active_pane, &tokens, &mut intents);
                }
                let selected = self.model.selected_obligation.as_deref();
                let inbox = InboxReadModel {
                    obligations: self.model.exceptions().collect(),
                    selected,
                    captured_at: self
                        .model
                        .snapshot
                        .as_ref()
                        .map(|snapshot| snapshot.captured_at),
                    connection: &self.model.connection,
                    loading: self.model.snapshot.is_none() && self.model.snapshot_busy,
                    selection_destination: Some(if narrow {
                        OBLIGATIONS_PANE_ID
                    } else {
                        TIMELINE_PANE_ID
                    }),
                };
                let obligations = ObligationsReadModel {
                    obligations: self.model.filtered_obligations(),
                    total: self.model.obligations().len(),
                    selected,
                    search: &self.model.search,
                    filter: self.model.state_filter,
                    captured_at: self
                        .model
                        .snapshot
                        .as_ref()
                        .map(|snapshot| snapshot.captured_at),
                    loading: self.model.snapshot.is_none() && self.model.snapshot_busy,
                    selection_destination: narrow.then_some(TIMELINE_PANE_ID),
                };
                let timeline = TimelineReadModel {
                    obligation: self.model.selected(),
                    topic: self.model.topic.as_ref(),
                    topic_error: self.model.topic_error.as_deref(),
                    loading: self.model.topic_busy,
                    connection: &self.model.connection,
                    action_busy: self.model.action_busy,
                    snapshot_busy: self.model.snapshot_busy,
                };
                let mut presenter = OperatorPanePresenter {
                    inbox,
                    obligations,
                    timeline,
                    intents: &mut intents,
                    tokens,
                    font_scale: self.preferences.font_scale,
                    text: Vec::new(),
                    semantic_nodes: Vec::new(),
                    virtualisation: VirtualisationObservation::default(),
                };
                if narrow {
                    presenter.pane_ui(ui, self.workspace.active_pane, ui.max_rect());
                } else {
                    let _ = dock_workspace(
                        ui,
                        &mut self.workspace,
                        &mut self.dock,
                        &mut presenter,
                        DockTextContext {
                            tokens,
                            font_scale: self.preferences.font_scale,
                        },
                    );
                }
                text_observations.extend(presenter.text);
                semantic_nodes.extend(presenter.semantic_nodes);
                virtualisation = presenter.virtualisation;
            });

        if let Some(confirmation) = self.model.confirmation.clone() {
            let submit_unavailable = self.confirmation_submit_unavailable_reason(&confirmation);
            show_confirmation(
                &context,
                &confirmation,
                self.model.action_busy,
                submit_unavailable.as_deref(),
                &tokens,
                self.preferences.font_scale,
                &mut intents,
                &mut semantic_nodes,
                &mut text_observations,
            );
        }
        self.apply_intents(intents, &context);
        let confirmation = self.model.confirmation.as_ref();
        self.last_test_snapshot = finish_snapshot(
            &context,
            self.frame_number,
            semantic_nodes,
            text_observations,
            virtualisation,
            InteractionObservation {
                selected_obligation: self.model.selected_obligation.clone(),
                active_pane: self.workspace.active_pane.0,
                connection: freshness_state(&self.model.connection).to_owned(),
                status: self.model.status.clone(),
                snapshot_busy: self.model.snapshot_busy,
                topic_busy: self.model.topic_busy,
                action_busy: self.model.action_busy,
                confirmation_action: confirmation.map(|value| value.action.stable_id().to_owned()),
                confirmation_obligation: confirmation.map(|value| value.obligation_id.clone()),
                confirmation_occurrence: confirmation.map(|value| value.occurrence),
                confirmation_consequence: confirmation.map(|value| value.consequence.clone()),
                confirmation_fingerprint: confirmation
                    .and_then(|value| value.gardener.as_ref())
                    .map(|value| value.fingerprint.clone()),
                confirmation_prompt: confirmation
                    .and_then(|value| value.gardener.as_ref())
                    .map(|value| value.prompt.clone()),
                confirmation_conflict: confirmation.and_then(|value| value.conflict.clone()),
            },
        );
        if let Some(observer) = &self.test_observer {
            *observer.borrow_mut() = self.last_test_snapshot.clone();
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.write_native_test_snapshot();
    }
}

struct InboxReadModel<'a> {
    obligations: Vec<&'a OperatorObligation>,
    selected: Option<&'a str>,
    captured_at: Option<i64>,
    connection: &'a ConnectionState,
    loading: bool,
    selection_destination: Option<PaneId>,
}

struct ObligationsReadModel<'a> {
    obligations: Vec<&'a OperatorObligation>,
    total: usize,
    selected: Option<&'a str>,
    search: &'a str,
    filter: StateFilter,
    captured_at: Option<i64>,
    loading: bool,
    selection_destination: Option<PaneId>,
}

struct TimelineReadModel<'a> {
    obligation: Option<&'a OperatorObligation>,
    topic: Option<&'a ObligationTopic>,
    topic_error: Option<&'a str>,
    loading: bool,
    connection: &'a ConnectionState,
    action_busy: bool,
    snapshot_busy: bool,
}

struct OperatorPanePresenter<'a> {
    inbox: InboxReadModel<'a>,
    obligations: ObligationsReadModel<'a>,
    timeline: TimelineReadModel<'a>,
    intents: &'a mut Vec<OperatorIntent>,
    tokens: DesignTokens,
    font_scale: f32,
    text: Vec<TextLayoutObservation>,
    semantic_nodes: Vec<UiNode>,
    virtualisation: VirtualisationObservation,
}

impl PanePresenter for OperatorPanePresenter<'_> {
    fn title(&self, pane: PaneId) -> &'static str {
        match pane {
            INBOX_PANE_ID => "Inbox",
            OBLIGATIONS_PANE_ID => "Obligations",
            TIMELINE_PANE_ID => "Timeline",
            _ => "Unknown pane",
        }
    }

    fn pane_ui(&mut self, ui: &mut egui::Ui, pane: PaneId, pane_rect: egui::Rect) {
        let mut pane_node = UiNode::container(
            SemanticUiId::pane(pane),
            Some(SemanticUiId::root()),
            UiRole::Pane,
            pane_rect.into(),
        );
        pane_node.name = self.title(pane).to_owned();
        pane_node.pane = Some(pane);
        self.semantic_nodes.push(pane_node);
        match pane {
            INBOX_PANE_ID => show_inbox(
                ui,
                &self.inbox,
                self.intents,
                &self.tokens,
                self.font_scale,
                &mut self.text,
                &mut self.semantic_nodes,
            ),
            OBLIGATIONS_PANE_ID => show_obligations(
                ui,
                &self.obligations,
                self.intents,
                &self.tokens,
                self.font_scale,
                &mut self.text,
                &mut self.semantic_nodes,
                &mut self.virtualisation,
            ),
            TIMELINE_PANE_ID => show_timeline(
                ui,
                &self.timeline,
                self.intents,
                &self.tokens,
                self.font_scale,
                &mut self.text,
                &mut self.semantic_nodes,
            ),
            _ => {}
        }
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

fn show_narrow_navigation(
    ui: &mut egui::Ui,
    active: PaneId,
    tokens: &DesignTokens,
    intents: &mut Vec<OperatorIntent>,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = tokens.spacing.unit.0;
        for (pane, label) in [
            (INBOX_PANE_ID, "Inbox"),
            (OBLIGATIONS_PANE_ID, "Obligations"),
            (TIMELINE_PANE_ID, "Timeline"),
        ] {
            ui.push_id(("narrow-pane-navigation", pane.0), |ui| {
                let response = ui.selectable_label(active == pane, label);
                record_native_text_control(&response, NativeTextControlKind::Selectable);
                if response.clicked() {
                    intents.push(OperatorIntent::Navigate(pane));
                }
            });
        }
    });
    ui.separator();
}

fn show_inbox(
    ui: &mut egui::Ui,
    read: &InboxReadModel<'_>,
    intents: &mut Vec<OperatorIntent>,
    tokens: &DesignTokens,
    font_scale: f32,
    text: &mut Vec<TextLayoutObservation>,
    semantic_nodes: &mut Vec<UiNode>,
) {
    egui::ScrollArea::vertical()
        .id_salt("bokkie-inbox-scroll")
        .show(ui, |ui| {
            section_heading(ui, 1_001, "Genuine exceptions", tokens, font_scale, text);
            status_badge(
                ui,
                1_002,
                &format!(
                    "{} exception{} · {}",
                    read.obligations.len(),
                    if read.obligations.len() == 1 { "" } else { "s" },
                    freshness_state(read.connection)
                ),
                connection_tone(read.connection),
                tokens,
                font_scale,
                text,
            );
            if read.loading {
                empty_message(ui, "Loading operator exceptions…", tokens, font_scale, text);
            } else if read.obligations.is_empty() {
                empty_message(
                    ui,
                    "Inbox clear — no backend-projected exceptions",
                    tokens,
                    font_scale,
                    text,
                );
            }
            for obligation in &read.obligations {
                ui.separator();
                let why = obligation
                    .exception
                    .as_ref()
                    .map(exception_label)
                    .unwrap_or_else(|| "No projected exception".to_owned());
                let consequence = available_consequences(obligation);
                let semantic_label = format!(
                    "{}\n{}\n{} · occurrence {} · {}\n{} · {}",
                    obligation.description,
                    why,
                    obligation.id,
                    obligation.occurrence,
                    relative_time(obligation.updated_at, read.captured_at),
                    consequence,
                    freshness_state(read.connection),
                );
                let response = ledger_row(
                    ui,
                    "inbox",
                    &obligation.id,
                    read.selected == Some(obligation.id.as_str()),
                    164.0,
                    &semantic_label,
                    tokens,
                    INBOX_PANE_ID,
                    semantic_nodes,
                    |ui| {
                        measured_content_label(
                            ui,
                            row_text_instance("inbox", &obligation.id, 1),
                            &obligation.description,
                            TextRole::Body,
                            TextOverflow::Wrap,
                            2,
                            TextInteraction::Inert,
                            tokens,
                            font_scale,
                            text,
                        );
                        measured_content_label(
                            ui,
                            row_text_instance("inbox", &obligation.id, 2),
                            &why,
                            TextRole::Error,
                            TextOverflow::Wrap,
                            2,
                            TextInteraction::Inert,
                            tokens,
                            font_scale,
                            text,
                        );
                        measured_content_label(
                            ui,
                            row_text_instance("inbox", &obligation.id, 3),
                            &format!(
                                "{} · occurrence {} · {} · {}",
                                obligation.id,
                                obligation.occurrence,
                                relative_time(obligation.updated_at, read.captured_at),
                                freshness_state(read.connection)
                            ),
                            TextRole::Secondary,
                            TextOverflow::Ellipsis,
                            1,
                            TextInteraction::Inert,
                            tokens,
                            font_scale,
                            text,
                        );
                        measured_content_label(
                            ui,
                            row_text_instance("inbox", &obligation.id, 4),
                            &consequence,
                            TextRole::Secondary,
                            TextOverflow::Wrap,
                            2,
                            TextInteraction::Inert,
                            tokens,
                            font_scale,
                            text,
                        );
                    },
                );
                if response.clicked() {
                    intents.push(OperatorIntent::Select {
                        obligation_id: obligation.id.clone(),
                        destination: read.selection_destination,
                    });
                }
            }
        });
}

#[allow(clippy::too_many_arguments)]
fn show_obligations(
    ui: &mut egui::Ui,
    read: &ObligationsReadModel<'_>,
    intents: &mut Vec<OperatorIntent>,
    tokens: &DesignTokens,
    font_scale: f32,
    text: &mut Vec<TextLayoutObservation>,
    semantic_nodes: &mut Vec<UiNode>,
    virtualisation: &mut VirtualisationObservation,
) {
    section_heading(
        ui,
        2_001,
        &format!("All obligations · {}", read.total),
        tokens,
        font_scale,
        text,
    );
    let mut search = read.search.to_owned();
    let search_response = ui.add(
        egui::TextEdit::singleline(&mut search)
            .id_salt("obligation-search")
            .hint_text("Search identity, description or error")
            .desired_width(f32::INFINITY),
    );
    record_native_text_control(&search_response, NativeTextControlKind::Selectable);
    if search_response.changed() {
        intents.push(OperatorIntent::Search(search));
    }
    let mut filter = read.filter;
    let combo = egui::ComboBox::from_id_salt("obligation-state-filter")
        .selected_text(
            StateFilter::OPTIONS
                .iter()
                .find_map(|(value, label)| (*value == filter).then_some(*label))
                .unwrap_or("All states"),
        )
        .show_ui(ui, |ui| {
            for (value, label) in StateFilter::OPTIONS {
                ui.selectable_value(&mut filter, value, label);
            }
        });
    record_native_text_control(&combo.response, NativeTextControlKind::ComboBox);
    if filter != read.filter {
        intents.push(OperatorIntent::Filter(filter));
    }
    ui.label(format!("{} matching", read.obligations.len()));
    if read.loading {
        empty_message(ui, "Loading obligations…", tokens, font_scale, text);
        return;
    }
    if read.obligations.is_empty() {
        empty_message(
            ui,
            if read.total == 0 {
                "This database contains no obligations"
            } else {
                "No obligations match the current search and state filter"
            },
            tokens,
            font_scale,
            text,
        );
        return;
    }
    const ROW_HEIGHT: f32 = 176.0;
    const OVERSCAN: usize = 4;
    egui::ScrollArea::vertical()
        .id_salt("bokkie-obligations-scroll")
        .show_viewport(ui, |ui, viewport| {
            let rows = virtual_rows(
                viewport.top(),
                viewport.height(),
                ROW_HEIGHT,
                read.obligations.len(),
                OVERSCAN,
            );
            virtualisation.total_rows = read.obligations.len();
            virtualisation.visible_rows = (rows.visible.start, rows.visible.end);
            virtualisation.materialised_rows = (rows.materialised.start, rows.materialised.end);
            let origin = ui.min_rect().min;
            ui.set_min_height(read.obligations.len() as f32 * ROW_HEIGHT);
            for index in rows.materialised {
                let obligation = read.obligations[index];
                let row_rect = egui::Rect::from_min_size(
                    origin + egui::vec2(0.0, index as f32 * ROW_HEIGHT),
                    egui::vec2(ui.available_width(), ROW_HEIGHT),
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(row_rect), |ui| {
                    let semantic_label = obligation_row_text(obligation, read.captured_at);
                    let response = ledger_row(
                        ui,
                        "obligation",
                        &obligation.id,
                        read.selected == Some(obligation.id.as_str()),
                        ROW_HEIGHT - 4.0,
                        &semantic_label,
                        tokens,
                        OBLIGATIONS_PANE_ID,
                        semantic_nodes,
                        |ui| {
                            measured_content_label(
                                ui,
                                row_text_instance("obligation", &obligation.id, 1),
                                &obligation.description,
                                TextRole::Body,
                                TextOverflow::Wrap,
                                2,
                                TextInteraction::Inert,
                                tokens,
                                font_scale,
                                text,
                            );
                            measured_content_label(
                                ui,
                                row_text_instance("obligation", &obligation.id, 2),
                                &format!(
                                    "{} · {} · occurrence {} · attempts {}/{}",
                                    obligation.id,
                                    obligation.state.label(),
                                    obligation.occurrence,
                                    obligation.attempts_made,
                                    obligation.max_attempts
                                ),
                                TextRole::Secondary,
                                TextOverflow::Ellipsis,
                                1,
                                TextInteraction::Inert,
                                tokens,
                                font_scale,
                                text,
                            );
                            measured_content_label(
                                ui,
                                row_text_instance("obligation", &obligation.id, 3),
                                &liveness_label(obligation.liveness.as_ref()),
                                TextRole::Secondary,
                                TextOverflow::Ellipsis,
                                1,
                                TextInteraction::Inert,
                                tokens,
                                font_scale,
                                text,
                            );
                            measured_content_label(
                                ui,
                                row_text_instance("obligation", &obligation.id, 4),
                                &recurrence_label(obligation),
                                TextRole::Secondary,
                                TextOverflow::Ellipsis,
                                1,
                                TextInteraction::Inert,
                                tokens,
                                font_scale,
                                text,
                            );
                            measured_content_label(
                                ui,
                                row_text_instance("obligation", &obligation.id, 5),
                                &format!(
                                    "Error: {} · Evidence: {} · updated {}",
                                    obligation.last_error.as_deref().unwrap_or("No last error"),
                                    obligation
                                        .last_evidence
                                        .as_deref()
                                        .unwrap_or("No last evidence"),
                                    relative_time(obligation.updated_at, read.captured_at)
                                ),
                                TextRole::Secondary,
                                TextOverflow::Wrap,
                                2,
                                TextInteraction::Inert,
                                tokens,
                                font_scale,
                                text,
                            );
                        },
                    );
                    if response.clicked() {
                        intents.push(OperatorIntent::Select {
                            obligation_id: obligation.id.clone(),
                            destination: read.selection_destination,
                        });
                    }
                });
            }
        });
}

fn show_timeline(
    ui: &mut egui::Ui,
    read: &TimelineReadModel<'_>,
    intents: &mut Vec<OperatorIntent>,
    tokens: &DesignTokens,
    font_scale: f32,
    text: &mut Vec<TextLayoutObservation>,
    semantic_nodes: &mut Vec<UiNode>,
) {
    egui::ScrollArea::vertical()
        .id_salt("bokkie-timeline-scroll")
        .show(ui, |ui| {
            let Some(obligation) = read.obligation else {
                section_heading(ui, 3_001, "Evidence timeline", tokens, font_scale, text);
                empty_message(
                    ui,
                    "Select an exception or obligation to inspect its durable topic",
                    tokens,
                    font_scale,
                    text,
                );
                return;
            };
            section_heading(ui, 3_001, &obligation.description, tokens, font_scale, text);
            status_badge(
                ui,
                3_002,
                &format!(
                    "{} · occurrence {} · {}",
                    obligation.state.label(),
                    obligation.occurrence,
                    freshness_state(read.connection)
                ),
                state_tone(obligation),
                tokens,
                font_scale,
                text,
            );
            property_row(
                ui,
                3_010,
                "Obligation",
                &obligation.id,
                tokens,
                font_scale,
                text,
            );
            property_row(
                ui,
                3_011,
                "Durable liveness",
                &liveness_label(obligation.liveness.as_ref()),
                tokens,
                font_scale,
                text,
            );
            if let Some(subject) = exact_gardener_subject(obligation) {
                property_row(
                    ui,
                    3_012,
                    "Repository",
                    subject.repository,
                    tokens,
                    font_scale,
                    text,
                );
                property_row(
                    ui,
                    3_013,
                    "Goal fingerprint",
                    subject.fingerprint,
                    tokens,
                    font_scale,
                    text,
                );
                measured_content_label(
                    ui,
                    3_014,
                    subject.prompt,
                    TextRole::Body,
                    TextOverflow::Wrap,
                    3,
                    TextInteraction::Selectable,
                    tokens,
                    font_scale,
                    text,
                );
                property_row(
                    ui,
                    3_015,
                    "Proposal instance",
                    subject.instance_id,
                    tokens,
                    font_scale,
                    text,
                );
                property_row(
                    ui,
                    3_016,
                    "Generation",
                    &subject.generation.to_string(),
                    tokens,
                    font_scale,
                    text,
                );
                property_row(
                    ui,
                    3_017,
                    "Source commit",
                    subject.source_commit,
                    tokens,
                    font_scale,
                    text,
                );
                property_row(
                    ui,
                    3_018,
                    "Source observation",
                    &subject.source_observation_id.to_string(),
                    tokens,
                    font_scale,
                    text,
                );
                property_row(
                    ui,
                    3_019,
                    "Source inspection",
                    subject.source_inspection_id,
                    tokens,
                    font_scale,
                    text,
                );
            }
            section_heading(ui, 3_020, "Current actions", tokens, font_scale, text);
            ui.horizontal_wrapped(|ui| {
                for action in LifecycleAction::ALL {
                    let availability = read
                        .obligation
                        .map(|obligation| {
                            let capability = action.capability(obligation);
                            if read.action_busy {
                                Availability::Disabled {
                                    reason: "Another lifecycle request is in progress".into(),
                                }
                            } else if read.snapshot_busy {
                                Availability::Disabled {
                                    reason: "A current-state refresh is in progress".into(),
                                }
                            } else if read.loading {
                                Availability::Disabled {
                                    reason: "The selected evidence topic is still refreshing"
                                        .into(),
                                }
                            } else if !read.connection.decisions_safe() {
                                Availability::Disabled {
                                    reason: "Retained data may be stale; refresh before deciding"
                                        .into(),
                                }
                            } else if capability.available {
                                Availability::Enabled
                            } else {
                                Availability::Disabled {
                                    reason: crate::model::disabled_reason(capability).into(),
                                }
                            }
                        })
                        .unwrap_or_else(|| Availability::Disabled {
                            reason: "Select an obligation first".into(),
                        });
                    let enabled = availability.enabled();
                    let target = ActionTarget::pane(action, TIMELINE_PANE_ID);
                    let response = action_button(
                        ui,
                        ActionButtonSpec {
                            target,
                            availability: availability.clone(),
                            selected: false,
                            emphasis: if matches!(
                                action,
                                LifecycleAction::Approve | LifecycleAction::ApproveGardenerProposal
                            ) {
                                ActionEmphasis::Primary
                            } else {
                                ActionEmphasis::Quiet
                            },
                            compact: false,
                        },
                        tokens,
                        font_scale,
                        text,
                    );
                    semantic_nodes.push(action_semantic_node(
                        &response,
                        target,
                        &availability,
                        false,
                        SemanticUiId::pane(TIMELINE_PANE_ID),
                    ));
                    if response.clicked() && enabled {
                        intents.push(OperatorIntent::BeginAction(action));
                    }
                }
            });
            if let Some(error) = read.topic_error {
                status_badge(
                    ui,
                    3_030,
                    error,
                    StatusTone::Error,
                    tokens,
                    font_scale,
                    text,
                );
            }
            ui.separator();
            section_heading(ui, 3_040, "Durable topic", tokens, font_scale, text);
            if read.loading && read.topic.is_none() {
                empty_message(ui, "Loading durable evidence…", tokens, font_scale, text);
            } else if let Some(topic) = read.topic {
                if topic.items.is_empty() {
                    empty_message(
                        ui,
                        "No durable topic items exist for this obligation",
                        tokens,
                        font_scale,
                        text,
                    );
                }
                for item in &topic.items {
                    show_topic_item(ui, item, topic.captured_at, tokens, font_scale, text);
                }
            }
        });
}

fn show_topic_item(
    ui: &mut egui::Ui,
    item: &TopicItem,
    captured_at: i64,
    tokens: &DesignTokens,
    font_scale: f32,
    text: &mut Vec<TextLayoutObservation>,
) {
    ui.separator();
    section_heading(
        ui,
        topic_text_instance(&item.stable_id, 0),
        &format!(
            "{} · {}",
            source_label(item.source),
            event_label(&item.event_type)
        ),
        tokens,
        font_scale,
        text,
    );
    property_row(
        ui,
        topic_text_instance(&item.stable_id, 1),
        "When",
        &format!(
            "{} · Unix {}",
            relative_time(item.occurred_at, Some(captured_at)),
            item.occurred_at
        ),
        tokens,
        font_scale,
        text,
    );
    property_row(
        ui,
        topic_text_instance(&item.stable_id, 2),
        "Stable ID",
        &item.stable_id,
        tokens,
        font_scale,
        text,
    );
    property_row(
        ui,
        topic_text_instance(&item.stable_id, 3),
        "Source sequence",
        &item.source_sequence,
        tokens,
        font_scale,
        text,
    );
    if let Some(occurrence) = item.occurrence {
        property_row(
            ui,
            topic_text_instance(&item.stable_id, 4),
            "Occurrence",
            &occurrence.to_string(),
            tokens,
            font_scale,
            text,
        );
    }
    for (field_index, (label, value)) in common_evidence(&item.evidence).into_iter().enumerate() {
        property_row(
            ui,
            topic_text_instance(&item.stable_id, 10 + field_index as u64),
            label,
            &value,
            tokens,
            font_scale,
            text,
        );
    }
    egui::CollapsingHeader::new("Raw durable evidence")
        .id_salt(("raw-evidence", &item.stable_id))
        .show(ui, |ui| {
            let raw = serde_json::to_string_pretty(&item.evidence)
                .unwrap_or_else(|_| "Evidence could not be formatted".to_owned());
            measured_content_label(
                ui,
                topic_text_instance(&item.stable_id, 90),
                &raw,
                TextRole::MonospaceTechnical,
                TextOverflow::Wrap,
                24,
                TextInteraction::Selectable,
                tokens,
                font_scale,
                text,
            );
        });
}

#[allow(clippy::too_many_arguments)]
fn show_confirmation(
    context: &egui::Context,
    confirmation: &Confirmation,
    busy: bool,
    submit_unavailable: Option<&str>,
    tokens: &DesignTokens,
    font_scale: f32,
    intents: &mut Vec<OperatorIntent>,
    semantic_nodes: &mut Vec<UiNode>,
    text: &mut Vec<TextLayoutObservation>,
) {
    let shown = egui::Window::new("Confirm lifecycle action")
        .id(egui::Id::new("bokkie-lifecycle-confirmation"))
        .frame(
            egui::Frame::window(&context.style_of(context.theme()))
                .fill(tokens.colours.surface_raised.into()),
        )
        .collapsible(false)
        .resizable(true)
        .default_width(560.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(context, |ui| {
            ui.heading(confirmation.action.specification().label);
            if let Some(conflict) = &confirmation.conflict {
                ui.colored_label(tokens.colours.status_warning, conflict);
            }
            ui.label(format!(
                "Obligation {} · occurrence {}",
                confirmation.obligation_id, confirmation.occurrence
            ));
            ui.label(format!("Consequence: {}", confirmation.consequence));
            if let Some(gardener) = &confirmation.gardener {
                ui.separator();
                for line in gardener_confirmation_provenance(gardener) {
                    ui.label(line);
                }
                ui.label("Exact immutable prompt:");
                egui::ScrollArea::vertical()
                    .id_salt("confirmation-prompt")
                    .max_height(220.0)
                    .show(ui, |ui| {
                        ui.add(egui::Label::new(&gardener.prompt).selectable(true).wrap());
                    });
            }
            let mut actor = confirmation.actor.clone();
            let mut note = confirmation.note.clone();
            if confirmation.action.requires_decision_body() {
                ui.separator();
                ui.label("Operator actor");
                ui.add(
                    egui::TextEdit::singleline(&mut actor)
                        .id_salt("confirmation-actor")
                        .desired_width(f32::INFINITY),
                );
                ui.label("Note (optional)");
                ui.add(
                    egui::TextEdit::multiline(&mut note)
                        .id_salt("confirmation-note")
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
            }
            if actor != confirmation.actor || note != confirmation.note {
                intents.push(OperatorIntent::UpdateConfirmation { actor, note });
            }
            ui.separator();
            ui.horizontal(|ui| {
                let submit_availability =
                    submit_unavailable.map_or(Availability::Enabled, |reason| {
                        Availability::Disabled {
                            reason: reason.to_owned().into(),
                        }
                    });
                let submit_enabled = submit_availability.enabled();
                let target = ActionTarget::application(ConfirmationAction::Submit);
                let response = action_button(
                    ui,
                    ActionButtonSpec {
                        target,
                        availability: submit_availability.clone(),
                        selected: false,
                        emphasis: ActionEmphasis::Primary,
                        compact: false,
                    },
                    tokens,
                    font_scale,
                    text,
                );
                semantic_nodes.push(action_semantic_node(
                    &response,
                    target,
                    &submit_availability,
                    false,
                    SemanticUiId::new("bokkie.lifecycle-confirmation"),
                ));
                if response.clicked() && submit_enabled {
                    intents.push(OperatorIntent::SubmitConfirmation);
                }
                let dismiss_availability = if busy {
                    Availability::Disabled {
                        reason: "The lifecycle request is already in progress".into(),
                    }
                } else {
                    Availability::Enabled
                };
                let dismiss_enabled = dismiss_availability.enabled();
                let target = ActionTarget::application(ConfirmationAction::Dismiss);
                let response = action_button(
                    ui,
                    ActionButtonSpec {
                        target,
                        availability: dismiss_availability.clone(),
                        selected: false,
                        emphasis: ActionEmphasis::Quiet,
                        compact: false,
                    },
                    tokens,
                    font_scale,
                    text,
                );
                semantic_nodes.push(action_semantic_node(
                    &response,
                    target,
                    &dismiss_availability,
                    false,
                    SemanticUiId::new("bokkie.lifecycle-confirmation"),
                ));
                if response.clicked() && dismiss_enabled {
                    intents.push(OperatorIntent::DismissConfirmation);
                }
                if busy {
                    ui.label("Applying through Bokkie…");
                }
            });
        });
    if let Some(shown) = shown {
        let mut node = UiNode::container(
            SemanticUiId::new("bokkie.lifecycle-confirmation"),
            Some(SemanticUiId::root()),
            UiRole::Section,
            shown.response.rect.into(),
        );
        node.name = "Confirm lifecycle action".to_owned();
        semantic_nodes.push(node);
    }
}

fn gardener_confirmation_provenance(gardener: &GardenerConfirmation) -> [String; 7] {
    [
        format!("Repository: {}", gardener.repository),
        format!("Goal fingerprint: {}", gardener.fingerprint),
        format!("Proposal instance: {}", gardener.instance_id),
        format!("Generation: {}", gardener.generation),
        format!("Source commit: {}", gardener.source_commit),
        format!("Source observation: {}", gardener.source_observation_id),
        format!("Source inspection: {}", gardener.source_inspection_id),
    ]
}

#[allow(clippy::too_many_arguments)]
fn ledger_row(
    ui: &mut egui::Ui,
    scope: &'static str,
    stable_id: &str,
    selected: bool,
    height: f32,
    semantic_label: &str,
    tokens: &DesignTokens,
    pane: PaneId,
    semantic_nodes: &mut Vec<UiNode>,
    content: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let (_, rect) = ui.allocate_space(egui::vec2(ui.available_width().max(1.0), height));
    let response = ui.interact(
        rect,
        egui::Id::new(("bokkie-ledger-row", scope, stable_id)),
        egui::Sense::click(),
    );
    if response.clicked() {
        response.request_focus();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            true,
            selected,
            semantic_label,
        )
    });
    ui.ctx().accesskit_node_builder(response.id, |node| {
        use egui::accesskit::{Action, Role};
        node.set_role(Role::ListBoxOption);
        node.set_label(semantic_label);
        node.set_author_id(format!("bokkie.{scope}-row.{stable_id}"));
        node.set_selected(selected);
        node.add_action(Action::Click);
    });
    semantic_nodes.push(UiNode {
        id: SemanticUiId::new(format!("bokkie.{scope}-row.{stable_id}")),
        parent: Some(SemanticUiId::pane(pane)),
        role: UiRole::ResultRow,
        name: semantic_label.to_owned(),
        description: None,
        rect: response.rect.into(),
        enabled: true,
        focused: response.has_focus(),
        selected,
        checked: None,
        expanded: None,
        pane: Some(pane),
        domain_reference: None,
        actions: Vec::new(),
        text_selectable: false,
        disabled_reason: None,
    });
    if selected || response.hovered() {
        ui.painter()
            .rect_filled(rect, 0.0, tokens.colours.selection_background);
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            0.0,
            egui::Stroke::new(1.0, tokens.colours.focus_ring),
            egui::StrokeKind::Inside,
        );
    }
    let content_rect = rect.shrink(tokens.spacing.inline.0);
    ui.scope_builder(
        egui::UiBuilder::new()
            .id_salt(("bokkie-ledger-row-content", scope, stable_id))
            .max_rect(content_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            ui.set_clip_rect(ui.clip_rect().intersect(content_rect));
            content(ui);
        },
    );
    response
}

fn empty_message(
    ui: &mut egui::Ui,
    message: &str,
    tokens: &DesignTokens,
    font_scale: f32,
    text: &mut Vec<TextLayoutObservation>,
) {
    measured_content_label(
        ui,
        stable_text_instance(message),
        message,
        TextRole::Secondary,
        TextOverflow::Wrap,
        3,
        TextInteraction::Selectable,
        tokens,
        font_scale,
        text,
    );
}

fn stable_text_instance(text: &str) -> u64 {
    text.bytes()
        .fold(14_695_981_039_346_656_037, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
        })
        // Polyorama property rows derive two child identities as `2n` and
        // `2n + 1`; keep application-owned stable hashes inside that domain.
        & (u64::MAX >> 2)
}

fn row_text_instance(scope: &str, stable_id: &str, field: u8) -> u64 {
    stable_text_instance(&format!("{scope}:{stable_id}:{field}"))
}

fn topic_text_instance(stable_id: &str, field: u64) -> u64 {
    stable_text_instance(&format!("topic:{stable_id}:{field}"))
}

fn connection_label(model: &AppModel) -> String {
    let refreshed = model.last_successful_refresh.map_or_else(
        || "never refreshed".to_owned(),
        |at| format!("last current at Unix {at}"),
    );
    match &model.connection {
        ConnectionState::Loading => format!("Loading · {refreshed}"),
        ConnectionState::Current if model.snapshot_busy => {
            format!("Refreshing · retained snapshot {refreshed}")
        }
        ConnectionState::Current => format!("Current · {refreshed}"),
        ConnectionState::Stale { reason } => format!("Stale · {refreshed} · {reason}"),
    }
}

fn compact_connection_label(model: &AppModel) -> String {
    let state = match &model.connection {
        ConnectionState::Loading => "Loading",
        ConnectionState::Current if model.snapshot_busy => "Refreshing",
        ConnectionState::Current => "Current",
        ConnectionState::Stale { .. } => "Stale",
    };
    model
        .last_successful_refresh
        .map_or_else(|| state.to_owned(), |at| format!("{state} · Unix {at}"))
}

fn freshness_state(connection: &ConnectionState) -> &'static str {
    match connection {
        ConnectionState::Loading => "loading",
        ConnectionState::Current => "current",
        ConnectionState::Stale { .. } => "stale retained data",
    }
}

fn connection_tone(connection: &ConnectionState) -> StatusTone {
    match connection {
        ConnectionState::Loading => StatusTone::Neutral,
        ConnectionState::Current => StatusTone::Success,
        ConnectionState::Stale { .. } => StatusTone::Warning,
    }
}

fn state_tone(obligation: &OperatorObligation) -> StatusTone {
    match obligation.state {
        bokkie_operator_api::OperatorObligationState::Attention => StatusTone::Error,
        bokkie_operator_api::OperatorObligationState::AwaitingApproval
        | bokkie_operator_api::OperatorObligationState::RetryScheduled => StatusTone::Warning,
        bokkie_operator_api::OperatorObligationState::Completed => StatusTone::Success,
        _ => StatusTone::Neutral,
    }
}

fn relative_time(then: i64, captured_at: Option<i64>) -> String {
    let Some(now) = captured_at else {
        return format!("updated Unix {then}");
    };
    let delta = now.saturating_sub(then);
    if delta < 0 {
        format!("in {}s", delta.unsigned_abs())
    } else if delta < 60 {
        format!("{delta}s ago")
    } else if delta < 3_600 {
        format!("{}m ago", delta / 60)
    } else if delta < 86_400 {
        format!("{}h ago", delta / 3_600)
    } else {
        format!("{}d ago", delta / 86_400)
    }
}

fn exception_label(reason: &ExceptionReason) -> String {
    match reason {
        ExceptionReason::AwaitingApproval { subject } => match subject {
            ApprovalSubject::Generic => "Awaiting a generic operator decision".to_owned(),
            ApprovalSubject::GardenerProposal {
                fingerprint,
                instance_id,
                generation,
                ..
            } => format!(
                "Awaiting exact gardener proposal instance {instance_id} · generation {generation} · goal {fingerprint}"
            ),
        },
        ExceptionReason::ExpiredLease {
            generation,
            expires_at,
            ..
        } => {
            format!("Lease generation {generation} expired at Unix {expires_at}; awaiting recovery")
        }
        ExceptionReason::Attention { cause, error, .. } => {
            let cause = match cause {
                AttentionCause::Rejected { actor, note } => note.as_ref().map_or_else(
                    || format!("Rejected by {actor}"),
                    |note| format!("Rejected by {actor}: {note}"),
                ),
                AttentionCause::AttemptsExhausted => "Retry attempts exhausted".to_owned(),
                AttentionCause::NonRetryableFailure => "Non-retryable failure".to_owned(),
                AttentionCause::RecurrenceFailure => "Recurrence scheduling failed".to_owned(),
                AttentionCause::GardenerVerificationBlocking { summary } => {
                    format!("Blocking verification: {summary}")
                }
                AttentionCause::GardenerVerificationInconclusive { summary } => {
                    format!("Inconclusive verification: {summary}")
                }
                AttentionCause::PersistedFailure => "Persisted failure requires review".to_owned(),
            };
            error
                .as_ref()
                .map_or(cause.clone(), |error| format!("{cause} · {error}"))
        }
    }
}

fn liveness_label(liveness: Option<&DurableLiveness>) -> String {
    match liveness {
        Some(DurableLiveness::FutureWake { wake_at }) => format!("Future wake at Unix {wake_at}"),
        Some(DurableLiveness::ActiveLease {
            token,
            generation,
            expires_at,
        }) => format!(
            "Active lease generation {generation}, expires Unix {expires_at}, token {token}"
        ),
        Some(DurableLiveness::HumanAttention { reason }) => {
            format!("Human attention: {}", exception_label(reason))
        }
        None => "Terminal — no durable liveness required".to_owned(),
    }
}

fn recurrence_label(obligation: &OperatorObligation) -> String {
    match (
        obligation.recurrence_cron.as_deref(),
        obligation.recurrence_timezone.as_deref(),
    ) {
        (Some(cron), Some(timezone)) => format!("{cron} · {timezone}"),
        _ => "One-off".to_owned(),
    }
}

fn obligation_row_text(obligation: &OperatorObligation, captured_at: Option<i64>) -> String {
    let error = obligation.last_error.as_deref().unwrap_or("No last error");
    let evidence = obligation
        .last_evidence
        .as_deref()
        .unwrap_or("No last evidence");
    format!(
        "{}\n{} · {} · occurrence {} · attempts {}/{}\n{}\n{}\nError: {} · Evidence: {} · updated {}",
        obligation.description,
        obligation.id,
        obligation.state.label(),
        obligation.occurrence,
        obligation.attempts_made,
        obligation.max_attempts,
        liveness_label(obligation.liveness.as_ref()),
        recurrence_label(obligation),
        error,
        evidence,
        relative_time(obligation.updated_at, captured_at),
    )
}

fn available_consequences(obligation: &OperatorObligation) -> String {
    let mut consequences = LifecycleAction::ALL
        .into_iter()
        .filter_map(|action| {
            let capability = action.capability(obligation);
            capability.available.then(|| consequence_label(capability))
        })
        .collect::<Vec<_>>();
    consequences.dedup();
    if consequences.is_empty() {
        "No lifecycle action currently authorised".to_owned()
    } else {
        consequences.join(" or ")
    }
}

struct ExactGardenerSubject<'a> {
    repository: &'a str,
    fingerprint: &'a str,
    instance_id: &'a str,
    generation: u32,
    source_commit: &'a str,
    source_observation_id: i64,
    source_inspection_id: &'a str,
    prompt: &'a str,
}

fn exact_gardener_subject(obligation: &OperatorObligation) -> Option<ExactGardenerSubject<'_>> {
    match obligation.exception.as_ref()? {
        ExceptionReason::AwaitingApproval {
            subject:
                ApprovalSubject::GardenerProposal {
                    repository,
                    fingerprint,
                    instance_id,
                    generation,
                    source_commit,
                    source_observation_id,
                    source_inspection_id,
                    prompt,
                    ..
                },
        } => Some(ExactGardenerSubject {
            repository,
            fingerprint,
            instance_id,
            generation: *generation,
            source_commit,
            source_observation_id: *source_observation_id,
            source_inspection_id,
            prompt,
        }),
        _ => None,
    }
}

fn source_label(source: TopicSource) -> &'static str {
    match source {
        TopicSource::AuditEvent => "Obligation audit",
        TopicSource::ApprovalDecision => "Approval decision",
        TopicSource::Attempt => "Execution attempt",
        TopicSource::GardenerInspection => "Gardener inspection",
        TopicSource::GardenerProposal => "Gardener proposal",
        TopicSource::GardenerProposalInstance => "Gardener proposal instance",
        TopicSource::GardenerObservation => "Gardener observation",
        TopicSource::GardenerEvent => "Gardener event",
        TopicSource::GardenerImplementationRun => "Implementation run",
        TopicSource::GardenerRunEvent => "Implementation run event",
    }
}

fn event_label(event: &str) -> String {
    let mut output = String::new();
    for (index, word) in event
        .split(['_', '-'])
        .filter(|word| !word.is_empty())
        .enumerate()
    {
        if index > 0 {
            output.push(' ');
        }
        if index == 0 {
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                output.extend(first.to_uppercase());
                output.extend(chars);
            }
        } else {
            output.push_str(word);
        }
    }
    if output.is_empty() {
        "Recorded event".to_owned()
    } else {
        output
    }
}

fn common_evidence(evidence: &Value) -> Vec<(&'static str, String)> {
    const FIELDS: [(&str, &str); 55] = [
        ("id", "Evidence ID"),
        ("obligation_id", "Obligation ID"),
        (
            "implementation_obligation_id",
            "Implementation obligation ID",
        ),
        ("attempt_id", "Attempt ID"),
        ("attempt", "Attempt"),
        ("attempt_number", "Attempt number"),
        ("lease_generation", "Lease generation"),
        ("lease_token", "Lease token"),
        ("fingerprint", "Goal fingerprint"),
        ("proposal_fingerprint", "Goal fingerprint"),
        ("proposal_instance_id", "Proposal instance"),
        ("generation", "Proposal generation"),
        ("source_observation_id", "Source observation"),
        ("source_inspection_id", "Source inspection"),
        ("inspection_id", "Inspection ID"),
        ("implementation_run_id", "Implementation run ID"),
        ("run_id", "Run ID"),
        ("repository", "Repository"),
        ("codex_thread_id", "Codex task ID"),
        ("codex_exec_id", "Codex execution ID"),
        ("codex_turn_id", "Codex turn ID"),
        ("implementation_thread_id", "Implementation task ID"),
        ("implementation_turn_id", "Implementation turn ID"),
        ("verification_thread_id", "Verification task ID"),
        ("verification_turn_id", "Verification turn ID"),
        ("source_commit", "Source commit"),
        ("base_commit", "Base commit"),
        ("head_commit", "Head commit"),
        ("commit", "Git commit"),
        ("git_commit", "Git commit"),
        ("pushed_head", "Pushed head"),
        ("branch", "Git branch"),
        ("pull_request_url", "Pull request"),
        ("pull_request_number", "Pull request number"),
        ("pull_request_head", "Pull request head"),
        ("verification_head", "Verification head"),
        ("verification_reported_head", "Verification reported head"),
        ("verification_outcome", "Verification outcome"),
        ("verification_verdict", "Verification verdict"),
        ("verification_summary", "Verification summary"),
        ("verification", "Verification"),
        ("phase", "Run phase"),
        ("outcome", "Attempt outcome"),
        ("retryable", "Retryable"),
        ("approval_decision", "Approval decision"),
        ("decision", "Decision"),
        ("observation_count", "Observation count"),
        ("prompt", "Immutable prompt"),
        ("prompt_digest", "Prompt digest"),
        ("actor", "Actor"),
        ("note", "Decision note"),
        ("error", "Error"),
        ("evidence", "Evidence"),
        ("from_state", "From state"),
        ("to_state", "To state"),
    ];
    let Some(object) = evidence.as_object() else {
        return vec![("Evidence value", value_text(evidence))];
    };
    let mut output = FIELDS
        .into_iter()
        .filter_map(|(key, label)| {
            object
                .get(key)
                .filter(|value| !value.is_null())
                .map(|value| (label, value_text(value)))
        })
        .collect::<Vec<_>>();
    if let Some(Value::String(details)) = object.get("details_json")
        && let Ok(parsed) = serde_json::from_str::<Value>(details)
    {
        output.extend(common_evidence(&parsed));
    }
    output
}

fn value_text(value: &Value) -> String {
    value.as_str().map_or_else(
        || serde_json::to_string(value).unwrap_or_else(|_| "unavailable".to_owned()),
        ToOwned::to_owned,
    )
}

fn poll_delay(snapshot: &bokkie_operator_api::OperatorSnapshot) -> Duration {
    let until_wake = snapshot
        .obligations
        .iter()
        .filter_map(|obligation| obligation.next_wake_at)
        .map(|wake| wake.saturating_sub(snapshot.captured_at).max(1) as u64)
        .min()
        .map(Duration::from_secs)
        .unwrap_or(POLL_MAX);
    until_wake.min(POLL_MAX)
}

#[cfg(test)]
mod tests {
    use bokkie_operator_api::{
        ActionCapability, ActionConsequence, ActionPrecondition, DisabledReason,
        OperatorCapabilities, OperatorObligationState, OperatorSnapshot,
    };

    use super::*;

    fn capability(available: bool, consequence: ActionConsequence) -> ActionCapability {
        ActionCapability {
            available,
            disabled_reason: (!available).then_some(DisabledReason::StateDoesNotPermit),
            consequence,
            precondition: available.then(|| ActionPrecondition {
                obligation_id: "fixture".to_owned(),
                occurrence: 2,
                state_revision: 1,
                gardener_fingerprint: None,
                gardener_proposal_instance_id: None,
                gardener_source_commit: None,
                gardener_source_observation_id: None,
                gardener_source_inspection_id: None,
                gardener_generation: None,
            }),
        }
    }

    fn fixture(index: usize) -> OperatorObligation {
        OperatorObligation {
            id: format!("obligation-{index:06}"),
            description: "Long deterministic description ".repeat(4),
            state: OperatorObligationState::RetryScheduled,
            occurrence: 2,
            scheduled_at: 100,
            next_wake_at: Some(110),
            recurrence_cron: Some("0 0 * * *".to_owned()),
            recurrence_timezone: Some("Australia/Adelaide".to_owned()),
            approval_required: false,
            attempts_made: 1,
            max_attempts: 3,
            retry_base_seconds: 1,
            retry_max_seconds: 60,
            last_error: Some("retry attention".to_owned()),
            last_evidence: Some("evidence".repeat(30)),
            created_at: 80,
            updated_at: 101,
            exception: None,
            liveness: Some(DurableLiveness::FutureWake { wake_at: 110 }),
            capabilities: OperatorCapabilities {
                approve: capability(false, ActionConsequence::ScheduleCurrentOccurrence),
                reject: capability(false, ActionConsequence::MoveToAttention),
                retry: capability(false, ActionConsequence::ReopenForRetry),
                cancel: capability(true, ActionConsequence::CancelObligation),
                approve_gardener_proposal: capability(
                    false,
                    ActionConsequence::ScheduleExactGardenerProposal,
                ),
                reject_gardener_proposal: capability(
                    false,
                    ActionConsequence::RejectExactGardenerProposal,
                ),
            },
        }
    }

    #[test]
    fn large_fixture_uses_a_bounded_materialised_range() {
        let obligations = (0..50_000).map(fixture).collect::<Vec<_>>();
        let range = virtual_rows(500_000.0, 720.0, 144.0, obligations.len(), 4);
        assert!(range.visible.start > 0);
        assert!(range.materialised.len() <= 14);
        assert!(range.materialised.end < obligations.len());
    }

    #[test]
    fn polling_is_bounded_and_uses_the_earliest_durable_wake() {
        let mut early = fixture(1);
        early.next_wake_at = Some(103);
        let mut late = fixture(2);
        late.next_wake_at = Some(500);
        assert_eq!(
            poll_delay(&OperatorSnapshot {
                captured_at: 100,
                service: None,
                obligations: vec![late, early]
            }),
            Duration::from_secs(3)
        );
        assert_eq!(
            poll_delay(&OperatorSnapshot {
                captured_at: 100,
                service: None,
                obligations: vec![fixture(3)]
            }),
            Duration::from_secs(10)
        );
        assert_eq!(
            poll_delay(&OperatorSnapshot {
                captured_at: 100,
                service: None,
                obligations: Vec::new()
            }),
            POLL_MAX
        );
    }

    #[test]
    fn topic_language_exposes_provenance_without_making_json_primary() {
        let evidence = serde_json::json!({
            "attempt_id": "attempt-long-id",
            "proposal_fingerprint": "fingerprint-long-id",
            "codex_thread_id": "task-id",
            "head_commit": "0123456789abcdef",
            "pull_request_url": "https://example.invalid/pr/1",
            "verification_outcome": "inconclusive",
            "uncommon_raw_field": {"kept": true}
        });
        let common = common_evidence(&evidence);
        assert!(common.iter().any(|(label, _)| *label == "Attempt ID"));
        assert!(common.iter().any(|(label, _)| *label == "Codex task ID"));
        assert!(common.iter().any(|(label, _)| *label == "Pull request"));
        assert!(
            !common
                .iter()
                .any(|(label, _)| *label == "uncommon_raw_field")
        );
        assert!(
            serde_json::to_string_pretty(&evidence)
                .unwrap()
                .contains("uncommon_raw_field")
        );
        assert_eq!(
            event_label("verification_inconclusive"),
            "Verification inconclusive"
        );
    }

    #[test]
    fn retry_and_cancel_consequences_are_backend_capability_driven() {
        let obligation = fixture(1);
        assert!(available_consequences(&obligation).contains("cancel"));
        assert!(!available_consequences(&obligation).contains("retry"));
    }

    #[test]
    fn exception_language_covers_retry_and_verification_attention() {
        let retry = ExceptionReason::Attention {
            cause: AttentionCause::AttemptsExhausted,
            error: Some("runner failed".to_owned()),
            evidence: Some("attempt evidence".to_owned()),
        };
        let blocking = ExceptionReason::Attention {
            cause: AttentionCause::GardenerVerificationBlocking {
                summary: "exact-head review found a blocker".to_owned(),
            },
            error: None,
            evidence: Some("verification report".to_owned()),
        };
        let inconclusive = ExceptionReason::Attention {
            cause: AttentionCause::GardenerVerificationInconclusive {
                summary: "reported head did not match".to_owned(),
            },
            error: None,
            evidence: None,
        };
        assert!(exception_label(&retry).contains("attempts exhausted"));
        assert!(exception_label(&blocking).contains("Blocking verification"));
        assert!(exception_label(&inconclusive).contains("Inconclusive verification"));
    }

    #[test]
    fn lifecycle_actions_have_unique_stable_semantic_targets() {
        let ids = LifecycleAction::ALL
            .map(|action| action.stable_id())
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), LifecycleAction::ALL.len());
        for action in LifecycleAction::ALL {
            let target = ActionTarget::pane(action, TIMELINE_PANE_ID);
            assert!(target.semantic_id().starts_with("action."));
            assert!(target.semantic_id().ends_with(".pane.3"));
            assert_eq!(action.specification().scope, ActionScope::Pane);
        }
    }

    #[test]
    fn out_of_order_same_obligation_topic_responses_apply_only_the_latest_generation() {
        let mut gate = TopicRequestGate::default();
        let older = gate.begin("same-obligation".to_owned());
        let newer = gate.begin("same-obligation".to_owned());
        assert!(newer > older);
        assert_eq!(gate.pending, 2);

        assert!(!gate.finish("same-obligation", older));
        assert_eq!(gate.pending, 1);
        assert!(gate.finish("same-obligation", newer));
        assert_eq!(gate.pending, 0);

        let newest = gate.begin("same-obligation".to_owned());
        let stale = gate.begin("same-obligation".to_owned());
        assert!(gate.finish("same-obligation", stale));
        assert!(!gate.finish("same-obligation", newest));
        assert_eq!(gate.pending, 0);
    }

    #[test]
    fn confirmation_actions_have_unique_stable_application_targets() {
        let submit = ActionTarget::application(ConfirmationAction::Submit);
        let dismiss = ActionTarget::application(ConfirmationAction::Dismiss);
        assert_eq!(submit.semantic_id(), "action.confirm_lifecycle_action");
        assert_eq!(
            dismiss.semantic_id(),
            "action.dismiss_lifecycle_confirmation"
        );
        assert_ne!(submit.semantic_id(), dismiss.semantic_id());
        assert_eq!(
            ConfirmationAction::Submit.specification().scope,
            ActionScope::Application
        );
        assert_eq!(
            ConfirmationAction::Dismiss.specification().scope,
            ActionScope::Application
        );
        for lifecycle in LifecycleAction::ALL {
            assert_ne!(
                lifecycle.stable_id(),
                ConfirmationAction::Submit.stable_id()
            );
            assert_ne!(
                lifecycle.stable_id(),
                ConfirmationAction::Dismiss.stable_id()
            );
        }
    }

    #[test]
    fn gardener_confirmation_renders_exact_source_bound_provenance() {
        let gardener = GardenerConfirmation {
            repository: "robchristie/bokkie".to_owned(),
            fingerprint: "goal-fingerprint".to_owned(),
            instance_id: "proposal-instance-3".to_owned(),
            generation: 3,
            source_commit: "c".repeat(40),
            source_observation_id: 17,
            source_inspection_id: "inspection-3".to_owned(),
            prompt: "Implement the exact reviewed goal".to_owned(),
        };
        assert_eq!(
            gardener_confirmation_provenance(&gardener),
            [
                "Repository: robchristie/bokkie".to_owned(),
                "Goal fingerprint: goal-fingerprint".to_owned(),
                "Proposal instance: proposal-instance-3".to_owned(),
                "Generation: 3".to_owned(),
                format!("Source commit: {}", "c".repeat(40)),
                "Source observation: 17".to_owned(),
                "Source inspection: inspection-3".to_owned(),
            ]
        );
        assert_eq!(gardener.prompt, "Implement the exact reviewed goal");
    }
}
