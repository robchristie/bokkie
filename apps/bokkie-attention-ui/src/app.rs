#[cfg(not(target_arch = "wasm32"))]
use std::time::{SystemTime, UNIX_EPOCH};
use std::{
    cell::RefCell,
    collections::{BTreeSet, VecDeque},
    rc::Rc,
    sync::mpsc::{self, Receiver, Sender},
    time::Duration,
};
use web_time::Instant;

use bokkie_operator_api::{
    ApprovalSubject, AttentionCause, DisabledReason, DurableLiveness, ExceptionReason,
    ObligationTopic, OperatorObligation, OperatorSnapshot, TopicItem, TopicSource,
};
use eframe::egui;
use polyorama_core::{DockNodeId, PaneId, Workspace, virtual_rows};
#[cfg(test)]
use polyorama_ui_egui::apply_design_system_with_typography;
use polyorama_ui_egui::{
    ActionButtonSpec, ActionButtonState, ActionEmphasis, ActionKey, ActionScope, ActionSpec,
    ActionTarget, ApplicationTheme, Availability, ContentTextSpec, DesignTokens, DomainReference,
    NativeTextControlKind, PanePresenter, PresentationContext, PresentationObservations,
    PresentationScope, SemanticUiId, StatusTone, TextInteraction, TextLayoutObservation,
    TextOverflow, TextRole, TypographyProfile, UiNode, UiPreferences, UiRole, action_button,
    action_semantic_node, application_bar_frame, application_bar_height,
    record_native_text_control,
};
use serde::Serialize;
use serde_json::Value;

use crate::{
    APPLICATION_NAME,
    appearance::Appearance,
    model::{
        AppModel, ChangeAssembly, ChangeProgress, Confirmation, ConnectionState,
        GardenerConfirmation, INBOX_PANE_ID, LifecycleAction, OBLIGATIONS_PANE_ID,
        OperatorStateLabel, PageProgress, SnapshotAssembly, StateFilter, TIMELINE_PANE_ID,
        TopicAssembly, consequence_label, operator_workspace,
    },
    transport::{
        ActionRequest, ApiFailure, ApiMessage, ApiPayload, ApiRequest, ApiSession, Transport,
    },
    ui_observation::{
        InteractionObservation, RawPresentationObservation, TestSnapshot,
        VirtualisationObservation, finish_snapshot, root_node,
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
struct AffectedRefresh {
    generation: u64,
    watermark: i64,
    affected: BTreeSet<String>,
    pending: VecDeque<String>,
    projections: Vec<OperatorObligation>,
    waiting_for_topic: bool,
}

#[derive(Debug, Eq, PartialEq)]
enum ProjectionRefreshPlan {
    FullRebuild,
    AdvanceOnly,
    Affected(BTreeSet<String>),
}

fn projection_refresh_plan(
    mut affected: BTreeSet<String>,
    ambiguous: bool,
    mut extra_affected: BTreeSet<String>,
) -> ProjectionRefreshPlan {
    if ambiguous {
        return ProjectionRefreshPlan::FullRebuild;
    }
    affected.append(&mut extra_affected);
    if affected.is_empty() {
        ProjectionRefreshPlan::AdvanceOnly
    } else {
        ProjectionRefreshPlan::Affected(affected)
    }
}

pub struct AttentionApp {
    workspace: Workspace,
    collection: PaneId,
    model: AppModel,
    transport: Option<Transport>,
    session: Option<ApiSession>,
    sender: Sender<ApiMessage>,
    receiver: Receiver<ApiMessage>,
    preferences: UiPreferences,
    theme: ApplicationTheme,
    next_poll_at: Option<Instant>,
    deadline_obligations: BTreeSet<String>,
    next_generation: u64,
    snapshot_assembly: Option<SnapshotAssembly>,
    topic_assembly: Option<TopicAssembly>,
    change_assembly: Option<ChangeAssembly>,
    change_extra_affected: BTreeSet<String>,
    affected_refresh: Option<AffectedRefresh>,
    projection_recovery: bool,
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
        #[cfg(not(target_arch = "wasm32"))]
        let appearance = std::env::var("BOKKIE_APPEARANCE")
            .ok()
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_default();
        #[cfg(target_arch = "wasm32")]
        let appearance = Appearance::default();
        Self::new_observed_with_appearance(creation, test_observer, appearance)
    }

    pub(crate) fn new_observed_with_appearance(
        creation: &eframe::CreationContext<'_>,
        test_observer: Option<Rc<RefCell<TestSnapshot>>>,
        appearance: Appearance,
    ) -> Self {
        let preferences = appearance.preferences();
        appearance.apply(&creation.egui_ctx);
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
            collection: INBOX_PANE_ID,
            model,
            transport,
            session: None,
            sender,
            receiver,
            preferences,
            theme: appearance.theme(),
            next_poll_at: None,
            deadline_obligations: BTreeSet::new(),
            next_generation: 1,
            snapshot_assembly: None,
            topic_assembly: None,
            change_assembly: None,
            change_extra_affected: BTreeSet::new(),
            affected_refresh: None,
            projection_recovery: false,
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
            ApiRequest::SnapshotPage { .. }
            | ApiRequest::Changes { .. }
            | ApiRequest::Obligation { .. } => self.model.snapshot_busy = true,
            ApiRequest::TopicPage { .. } => self.model.topic_busy = true,
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
                    self.begin_full_rebuild(false, context);
                }
                (
                    ApiRequest::SnapshotPage { generation, .. },
                    Ok(ApiPayload::SnapshotPage(page)),
                ) => self.accept_snapshot_page(generation, page, context),
                (
                    ApiRequest::TopicPage {
                        obligation_id,
                        generation,
                        ..
                    },
                    Ok(ApiPayload::TopicPage(page)),
                ) => self.accept_topic_page(generation, &obligation_id, page, context),
                (ApiRequest::Changes { generation, .. }, Ok(ApiPayload::Changes(page))) => {
                    self.accept_change_page(generation, page, context)
                }
                (
                    ApiRequest::Obligation {
                        obligation_id,
                        generation,
                    },
                    Ok(ApiPayload::Obligation(projection)),
                ) => self.accept_obligation_projection(
                    generation,
                    &obligation_id,
                    *projection,
                    context,
                ),
                (ApiRequest::Act(action), Ok(ApiPayload::ActionAccepted)) => {
                    let affected = BTreeSet::from([action.obligation_id.clone()]);
                    self.model
                        .record_action_accepted(action.action.specification().label);
                    self.begin_changes(affected, context);
                }
                (ApiRequest::Act(_), Err(ApiFailure::Conflict(message))) => {
                    let affected = self.model.selected_obligation.clone().into_iter().collect();
                    self.model.record_transition_conflict(&message);
                    self.begin_changes(affected, context);
                }
                (request, Err(ApiFailure::SessionChanged(_)))
                    if !self.request_is_current(&request) => {}
                (_, Err(ApiFailure::SessionChanged(message))) => {
                    self.restart_session(&message, context);
                }
                (ApiRequest::Bootstrap, Err(error)) => {
                    self.session = None;
                    self.model
                        .mark_stale(format!("Session bootstrap failed: {error}"));
                    self.next_poll_at = Some(Instant::now() + RECONNECT_DELAY);
                }
                (
                    request @ (ApiRequest::SnapshotPage { .. }
                    | ApiRequest::TopicPage { .. }
                    | ApiRequest::Changes { .. }
                    | ApiRequest::Obligation { .. }),
                    Err(ApiFailure::ProjectionGap(message) | ApiFailure::InvalidCursor(message)),
                ) => {
                    if self.request_is_current(&request) {
                        self.recover_projection(&message, context);
                    }
                }
                (
                    ApiRequest::TopicPage {
                        obligation_id,
                        generation,
                        ..
                    },
                    Err(error),
                ) => {
                    if self
                        .topic_assembly
                        .as_ref()
                        .is_some_and(|walk| walk.generation == generation)
                        && self.model.selected_obligation.as_deref() == Some(&obligation_id)
                    {
                        self.model.topic_error = Some(format!("Topic refresh failed: {error}"));
                        self.fail_or_recover_projection(
                            format!("Selected topic could not be refreshed: {error}"),
                            context,
                        );
                    }
                }
                (
                    request @ (ApiRequest::SnapshotPage { .. }
                    | ApiRequest::Changes { .. }
                    | ApiRequest::Obligation { .. }),
                    Err(error),
                ) => {
                    if self.request_is_current(&request) {
                        self.fail_or_recover_projection(
                            format!("Projection refresh failed: {error}"),
                            context,
                        );
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
        if now >= deadline
            && !self.model.snapshot_busy
            && !self.model.topic_busy
            && !self.model.action_busy
        {
            self.next_poll_at = None;
            if self.session.is_some() {
                if self.model.snapshot.is_some()
                    && matches!(self.model.connection, ConnectionState::Current)
                {
                    let affected = std::mem::take(&mut self.deadline_obligations);
                    self.begin_changes(affected, context);
                } else {
                    self.begin_full_rebuild(true, context);
                }
            } else {
                self.dispatch(ApiRequest::Bootstrap, context);
            }
        } else if deadline > now {
            context.request_repaint_after(deadline.duration_since(now));
        }
    }

    fn request_topic(&mut self, obligation_id: String, context: &egui::Context) {
        let generation = self.fresh_generation();
        self.topic_assembly = Some(TopicAssembly::new(generation, obligation_id.clone()));
        self.model.topic_busy = true;
        self.dispatch(
            ApiRequest::TopicPage {
                obligation_id,
                generation,
                cursor: None,
                watermark: None,
            },
            context,
        );
    }

    fn fresh_generation(&mut self) -> u64 {
        let generation = self.next_generation;
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .expect("projection request generation exhausted");
        generation
    }

    fn request_is_current(&self, request: &ApiRequest) -> bool {
        match request {
            ApiRequest::Bootstrap | ApiRequest::Act(_) => true,
            ApiRequest::SnapshotPage { generation, .. } => self
                .snapshot_assembly
                .as_ref()
                .is_some_and(|assembly| assembly.generation == *generation),
            ApiRequest::TopicPage { generation, .. } => self
                .topic_assembly
                .as_ref()
                .is_some_and(|assembly| assembly.generation == *generation),
            ApiRequest::Changes { generation, .. } => self
                .change_assembly
                .as_ref()
                .is_some_and(|assembly| assembly.generation == *generation),
            ApiRequest::Obligation { generation, .. } => self
                .affected_refresh
                .as_ref()
                .is_some_and(|refresh| refresh.generation == *generation),
        }
    }

    fn begin_full_rebuild(&mut self, recovering: bool, context: &egui::Context) {
        let generation = self.fresh_generation();
        self.snapshot_assembly = Some(SnapshotAssembly::new(generation));
        self.topic_assembly = None;
        self.change_assembly = None;
        self.change_extra_affected.clear();
        self.affected_refresh = None;
        self.deadline_obligations.clear();
        self.next_poll_at = None;
        self.projection_recovery = recovering;
        self.model.snapshot_busy = true;
        self.model.topic_busy = false;
        self.dispatch(
            ApiRequest::SnapshotPage {
                generation,
                cursor: None,
                watermark: None,
            },
            context,
        );
    }

    fn accept_snapshot_page(
        &mut self,
        generation: u64,
        page: OperatorSnapshot,
        context: &egui::Context,
    ) {
        let Some(assembly) = self.snapshot_assembly.as_mut() else {
            return;
        };
        if assembly.generation != generation {
            return;
        }
        match assembly.push(page) {
            Ok(PageProgress::Continue { cursor, watermark }) => self.dispatch(
                ApiRequest::SnapshotPage {
                    generation,
                    cursor: Some(cursor),
                    watermark: Some(watermark),
                },
                context,
            ),
            Ok(PageProgress::Complete(snapshot)) => {
                self.snapshot_assembly = None;
                self.projection_recovery = false;
                self.model.apply_snapshot(snapshot);
                if let Some(obligation_id) = self.model.selected_obligation.clone() {
                    self.request_topic(obligation_id, context);
                } else {
                    self.schedule_poll();
                }
            }
            Err(error) => self.recover_projection(&error, context),
        }
    }

    fn accept_topic_page(
        &mut self,
        generation: u64,
        obligation_id: &str,
        page: ObligationTopic,
        context: &egui::Context,
    ) {
        let Some(assembly) = self.topic_assembly.as_mut() else {
            return;
        };
        if assembly.generation != generation
            || self.model.selected_obligation.as_deref() != Some(obligation_id)
        {
            return;
        }
        match assembly.push(page) {
            Ok(PageProgress::Continue { cursor, watermark }) => self.dispatch(
                ApiRequest::TopicPage {
                    obligation_id: obligation_id.to_owned(),
                    generation,
                    cursor: Some(cursor),
                    watermark: Some(watermark),
                },
                context,
            ),
            Ok(PageProgress::Complete(topic)) => {
                if self.affected_refresh.as_ref().is_some_and(|refresh| {
                    refresh.waiting_for_topic && topic.watermark < refresh.watermark
                }) {
                    self.recover_projection(
                        "Selected topic watermark did not cover the affected refresh",
                        context,
                    );
                    return;
                }
                self.topic_assembly = None;
                self.model.topic = Some(topic);
                self.model.topic_error = None;
                self.model.topic_busy = false;
                if self
                    .affected_refresh
                    .as_ref()
                    .is_some_and(|refresh| refresh.waiting_for_topic)
                {
                    self.finish_affected_refresh(context);
                } else if !self.model.snapshot_busy {
                    self.schedule_poll();
                }
            }
            Err(error) => self.recover_projection(&error, context),
        }
    }

    fn begin_changes(&mut self, extra_affected: BTreeSet<String>, context: &egui::Context) {
        let Some(after) = self.model.applied_watermark() else {
            self.begin_full_rebuild(false, context);
            return;
        };
        let generation = self.fresh_generation();
        self.change_assembly = Some(ChangeAssembly::new(generation, after));
        self.change_extra_affected = extra_affected;
        self.affected_refresh = None;
        self.model.snapshot_busy = true;
        self.next_poll_at = None;
        self.dispatch(
            ApiRequest::Changes {
                generation,
                after,
                through: None,
            },
            context,
        );
    }

    fn accept_change_page(
        &mut self,
        generation: u64,
        page: bokkie_operator_api::ProjectionChangePage,
        context: &egui::Context,
    ) {
        let Some(assembly) = self.change_assembly.as_mut() else {
            return;
        };
        if assembly.generation != generation {
            return;
        }
        match assembly.push(page) {
            Ok(ChangeProgress::Continue { after, through }) => self.dispatch(
                ApiRequest::Changes {
                    generation,
                    after,
                    through: Some(through),
                },
                context,
            ),
            Ok(ChangeProgress::Complete {
                watermark,
                affected,
                ambiguous,
            }) => {
                self.change_assembly = None;
                let extra_affected = std::mem::take(&mut self.change_extra_affected);
                match projection_refresh_plan(affected, ambiguous, extra_affected) {
                    ProjectionRefreshPlan::FullRebuild => self.recover_projection(
                        "A global projection change did not identify an obligation",
                        context,
                    ),
                    ProjectionRefreshPlan::AdvanceOnly => {
                        if let Err(error) = self.model.apply_incremental(
                            watermark,
                            current_unix_seconds(),
                            Vec::new(),
                        ) {
                            self.recover_projection(&error, context);
                        } else {
                            self.schedule_poll();
                        }
                    }
                    ProjectionRefreshPlan::Affected(affected) => {
                        self.begin_affected_refresh(generation, watermark, affected, context);
                    }
                }
            }
            Err(error) => self.recover_projection(&error, context),
        }
    }

    fn begin_affected_refresh(
        &mut self,
        generation: u64,
        watermark: i64,
        affected: BTreeSet<String>,
        context: &egui::Context,
    ) {
        let pending = affected.iter().cloned().collect();
        self.affected_refresh = Some(AffectedRefresh {
            generation,
            watermark,
            affected,
            pending,
            projections: Vec::new(),
            waiting_for_topic: false,
        });
        self.dispatch_next_affected(context);
    }

    fn dispatch_next_affected(&mut self, context: &egui::Context) {
        let next = self.affected_refresh.as_mut().and_then(|refresh| {
            refresh
                .pending
                .pop_front()
                .map(|id| (refresh.generation, id))
        });
        if let Some((generation, obligation_id)) = next {
            self.dispatch(
                ApiRequest::Obligation {
                    obligation_id,
                    generation,
                },
                context,
            );
            return;
        }
        let selected_affected = self.affected_refresh.as_ref().is_some_and(|refresh| {
            self.model
                .selected_obligation
                .as_ref()
                .is_some_and(|selected| refresh.affected.contains(selected))
        });
        if selected_affected {
            if let Some(refresh) = self.affected_refresh.as_mut() {
                refresh.waiting_for_topic = true;
            }
            if let Some(obligation_id) = self.model.selected_obligation.clone() {
                self.request_topic(obligation_id, context);
            }
        } else {
            self.finish_affected_refresh(context);
        }
    }

    fn accept_obligation_projection(
        &mut self,
        generation: u64,
        obligation_id: &str,
        projection: bokkie_operator_api::OperatorObligationProjection,
        context: &egui::Context,
    ) {
        let Some(refresh) = self.affected_refresh.as_mut() else {
            return;
        };
        if refresh.generation != generation {
            return;
        }
        if projection.obligation.id != obligation_id || projection.watermark < refresh.watermark {
            self.recover_projection(
                "Affected obligation projection did not match its request watermark or identity",
                context,
            );
            return;
        }
        refresh.projections.push(projection.obligation);
        self.dispatch_next_affected(context);
    }

    fn finish_affected_refresh(&mut self, context: &egui::Context) {
        let Some(refresh) = self.affected_refresh.take() else {
            return;
        };
        match self.model.apply_incremental(
            refresh.watermark,
            current_unix_seconds(),
            refresh.projections,
        ) {
            Ok(()) => self.schedule_poll(),
            Err(error) => self.recover_projection(&error, context),
        }
    }

    fn recover_projection(&mut self, reason: &str, context: &egui::Context) {
        self.model.mark_stale(format!(
            "Projection state changed: {reason}; rebuilding current state"
        ));
        if self.projection_recovery && self.snapshot_assembly.is_some() {
            self.snapshot_assembly = None;
            self.topic_assembly = None;
            self.change_assembly = None;
            self.affected_refresh = None;
            self.model.snapshot_busy = false;
            self.model.topic_busy = false;
            self.next_poll_at = Some(Instant::now() + RECONNECT_DELAY);
        } else {
            self.begin_full_rebuild(true, context);
        }
    }

    fn fail_or_recover_projection(&mut self, reason: String, context: &egui::Context) {
        self.model.mark_stale(reason);
        if self.projection_recovery || self.snapshot_assembly.is_some() {
            self.snapshot_assembly = None;
            self.topic_assembly = None;
            self.change_assembly = None;
            self.affected_refresh = None;
            self.model.snapshot_busy = false;
            self.model.topic_busy = false;
            self.next_poll_at = Some(Instant::now() + RECONNECT_DELAY);
        } else {
            self.begin_full_rebuild(true, context);
        }
    }

    fn restart_session(&mut self, message: &str, context: &egui::Context) {
        self.session = None;
        self.model.record_session_change(message);
        self.model.topic_busy = false;
        self.snapshot_assembly = None;
        self.topic_assembly = None;
        self.change_assembly = None;
        self.affected_refresh = None;
        self.change_extra_affected.clear();
        self.deadline_obligations.clear();
        self.next_poll_at = None;
        self.projection_recovery = true;
        self.dispatch(ApiRequest::Bootstrap, context);
    }

    fn schedule_poll(&mut self) {
        let Some(snapshot) = self.model.snapshot.as_ref() else {
            self.next_poll_at = Some(Instant::now() + RECONNECT_DELAY);
            return;
        };
        let (delay, obligations) = poll_plan(snapshot);
        self.deadline_obligations = obligations;
        self.next_poll_at = Some(Instant::now() + delay);
    }

    fn apply_intents(&mut self, intents: Vec<OperatorIntent>, context: &egui::Context) {
        for intent in intents {
            match intent {
                OperatorIntent::Refresh => {
                    self.next_poll_at = None;
                    if self.session.is_some() {
                        if self.model.snapshot.is_some()
                            && matches!(self.model.connection, ConnectionState::Current)
                        {
                            self.begin_changes(BTreeSet::new(), context);
                        } else {
                            self.begin_full_rebuild(true, context);
                        }
                    } else {
                        self.dispatch(ApiRequest::Bootstrap, context);
                    }
                }
                OperatorIntent::Select {
                    obligation_id,
                    destination,
                } => {
                    let changed = self.model.select(obligation_id.clone());
                    let waiting_for_affected_topic = self
                        .affected_refresh
                        .as_ref()
                        .is_some_and(|refresh| refresh.waiting_for_topic);
                    let selected_is_affected = self
                        .affected_refresh
                        .as_ref()
                        .is_some_and(|refresh| refresh.affected.contains(&obligation_id));
                    if changed && waiting_for_affected_topic && !selected_is_affected {
                        if let Some(refresh) = self.affected_refresh.as_mut() {
                            refresh.waiting_for_topic = false;
                        }
                        self.finish_affected_refresh(context);
                    }
                    if changed || self.model.topic.is_none() {
                        if selected_is_affected
                            && let Some(refresh) = self.affected_refresh.as_mut()
                        {
                            refresh.waiting_for_topic = true;
                        }
                        self.request_topic(obligation_id, context);
                    }
                    if let Some(destination) = destination {
                        self.workspace.activate(destination);
                    }
                }
                OperatorIntent::Navigate(pane) => {
                    self.collection = pane;
                    self.workspace.activate(pane);
                }
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
        let tokens = self.theme.resolve(
            self.preferences
                .theme_variant(context.theme() == egui::Theme::Dark),
            self.preferences.density_variant(),
            TypographyProfile::Reading,
        );
        let mut intents = Vec::new();
        let mut semantic_nodes = vec![root_node(root_ui.max_rect())];
        let mut text_observations = Vec::new();
        let mut raw_presentations = Vec::new();
        let mut virtualisation = VirtualisationObservation::default();
        let bar = egui::Panel::top("bokkie-application-bar")
            .frame(application_bar_frame(&tokens))
            .exact_size(application_bar_height(&tokens, self.preferences.font_scale))
            .show(root_ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.heading("Bokkie");
                    ui.add_space(12.0);
                    if ui.max_rect().width() - 32.0 >= NARROW_WORKSPACE_WIDTH {
                        show_collection_tabs(
                            ui,
                            self.collection,
                            self.model.exceptions().count(),
                            &mut intents,
                            &mut semantic_nodes,
                        );
                    }
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
                                state: ActionButtonState::Momentary,
                                emphasis: ActionEmphasis::QuietBorderless,
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
                            ActionButtonState::Momentary,
                            SemanticUiId::root(),
                        ));
                        if response.clicked() {
                            intents.push(OperatorIntent::Refresh);
                        }
                        let colour =
                            if matches!(self.model.connection, ConnectionState::Stale { .. }) {
                                tokens.colours.status_warning
                            } else {
                                tokens.colours.text_muted
                            };
                        ui.label(
                            egui::RichText::new(compact_connection_label(&self.model))
                                .color(egui::Color32::from(colour)),
                        )
                        .on_hover_text(connection_label(&self.model));
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
            .frame(
                egui::Frame::NONE
                    .fill(tokens.colours.surface_canvas.into())
                    .inner_margin(16.0),
            )
            .show(root_ui, |ui| {
                let narrow = ui.available_width() < NARROW_WORKSPACE_WIDTH;
                if narrow {
                    show_collection_navigation(
                        ui,
                        self.collection,
                        self.workspace.active_pane,
                        true,
                        self.model.exceptions().count(),
                        &mut intents,
                        &mut semantic_nodes,
                    );
                }
                let selected = self.model.selected_obligation.as_deref();
                let inbox = InboxReadModel {
                    search: &self.model.search,
                    obligations: self
                        .model
                        .exceptions()
                        .filter(|item| matches_search(item, &self.model.search))
                        .collect(),
                    selected,
                    captured_at: self
                        .model
                        .snapshot
                        .as_ref()
                        .map(|snapshot| snapshot.captured_at),
                    connection: &self.model.connection,
                    loading: self.model.snapshot.is_none() && self.model.snapshot_busy,
                    selection_destination: Some(TIMELINE_PANE_ID),
                };
                let obligations = ObligationsReadModel {
                    obligations: self.model.filtered_obligations(),
                    total: self.model.obligations().len(),
                    selected,
                    filter: self.model.state_filter,
                    captured_at: self
                        .model
                        .snapshot
                        .as_ref()
                        .map(|snapshot| snapshot.captured_at),
                    loading: self.model.snapshot.is_none() && self.model.snapshot_busy,
                    selection_destination: Some(TIMELINE_PANE_ID),
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
                    search: &self.model.search,
                    inbox,
                    obligations,
                    timeline,
                    intents: &mut intents,
                    tokens,
                    font_scale: self.preferences.font_scale,
                    text: Vec::new(),
                    raw_presentations: Vec::new(),
                    semantic_nodes: Vec::new(),
                    virtualisation: VirtualisationObservation::default(),
                };
                if narrow {
                    presenter.pane_ui(ui, self.workspace.active_pane, ui.max_rect());
                } else {
                    let available = ui.available_rect_before_wrap();
                    let list_width = (available.width() * 0.43).clamp(320.0, 500.0);
                    let list_rect = egui::Rect::from_min_max(
                        available.min,
                        egui::pos2(available.left() + list_width, available.bottom()),
                    );
                    let detail_rect = egui::Rect::from_min_max(
                        egui::pos2(list_rect.right() + 24.0, available.top()),
                        available.max,
                    );
                    ui.scope_builder(egui::UiBuilder::new().max_rect(list_rect), |ui| {
                        presenter.pane_ui(ui, self.collection, list_rect);
                    });
                    ui.scope_builder(egui::UiBuilder::new().max_rect(detail_rect), |ui| {
                        egui::Frame::NONE
                            .fill(tokens.colours.surface_panel.into())
                            .inner_margin(20.0)
                            .corner_radius(8.0)
                            .show(ui, |ui| {
                                ui.set_min_size(
                                    (detail_rect.size() - egui::vec2(40.0, 40.0))
                                        .max(egui::Vec2::ZERO),
                                );
                                presenter.pane_ui(ui, TIMELINE_PANE_ID, ui.max_rect());
                            });
                    });
                }
                text_observations.extend(presenter.text);
                raw_presentations.extend(presenter.raw_presentations);
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
        // Observe the model that produced this pass, before applying its intents.
        // A requested confirmation belongs to the next pass's painted surface.
        let confirmation = self.model.confirmation.as_ref();
        self.last_test_snapshot = finish_snapshot(
            &context,
            self.frame_number,
            semantic_nodes,
            text_observations,
            raw_presentations,
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
        self.apply_intents(intents, &context);
        if let Some(observer) = &self.test_observer {
            *observer.borrow_mut() = self.last_test_snapshot.clone();
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.write_native_test_snapshot();
    }
}

struct InboxReadModel<'a> {
    search: &'a str,
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
    search: &'a str,
    inbox: InboxReadModel<'a>,
    obligations: ObligationsReadModel<'a>,
    timeline: TimelineReadModel<'a>,
    intents: &'a mut Vec<OperatorIntent>,
    tokens: DesignTokens,
    font_scale: f32,
    text: Vec<TextLayoutObservation>,
    raw_presentations: Vec<RawPresentationObservation>,
    semantic_nodes: Vec<UiNode>,
    virtualisation: VirtualisationObservation,
}

impl PanePresenter for OperatorPanePresenter<'_> {
    fn title(&self, pane: PaneId) -> &'static str {
        match pane {
            INBOX_PANE_ID => "Needs attention",
            OBLIGATIONS_PANE_ID => "All obligations",
            TIMELINE_PANE_ID => "Detail",
            _ => "Unknown pane",
        }
    }

    fn pane_ui(&mut self, ui: &mut egui::Ui, pane: PaneId, pane_rect: egui::Rect) {
        // Pane identity survives moving between the split shell and narrow navigation.
        let mut content_ui = ui.new_child(
            egui::UiBuilder::new()
                .id(egui::Id::new(("bokkie-pane", pane.0)))
                .max_rect(ui.available_rect_before_wrap())
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        let ui = &mut content_ui;
        let mut pane_node = UiNode::container(
            SemanticUiId::pane(pane),
            Some(SemanticUiId::root()),
            UiRole::Pane,
            pane_rect.into(),
        );
        pane_node.name = self.title(pane).to_owned();
        pane_node.pane = Some(pane);
        self.semantic_nodes.push(pane_node);
        if pane != TIMELINE_PANE_ID {
            show_search(
                ui,
                self.search,
                pane,
                self.intents,
                &mut self.semantic_nodes,
            );
        }
        let observed = if pane == TIMELINE_PANE_ID {
            show_timeline(
                ui,
                &self.timeline,
                self.intents,
                &self.tokens,
                self.font_scale,
            )
        } else {
            let mut presentation = PresentationContext::new(
                ui,
                self.tokens,
                self.font_scale,
                PresentationScope::new(("bokkie.collection", pane.0)),
                SemanticUiId::pane(pane),
            );
            match pane {
                INBOX_PANE_ID => show_inbox(ui, &self.inbox, self.intents, &mut presentation),
                OBLIGATIONS_PANE_ID => show_obligations(
                    ui,
                    &self.obligations,
                    self.intents,
                    &mut presentation,
                    &mut self.virtualisation,
                ),
                _ => {}
            }
            presentation.finish(ui)
        };
        self.text.extend(observed.text_layouts);
        self.semantic_nodes.extend(observed.semantic_nodes);
        self.raw_presentations
            .extend(observed.raw_presentations.into_iter().map(Into::into));
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

fn show_collection_navigation(
    ui: &mut egui::Ui,
    collection: PaneId,
    active: PaneId,
    narrow: bool,
    attention_count: usize,
    intents: &mut Vec<OperatorIntent>,
    nodes: &mut Vec<UiNode>,
) {
    ui.horizontal(|ui| {
        if narrow && active == TIMELINE_PANE_ID {
            let response = ui.button("← Back");
            record_navigation(
                &response,
                "bokkie.back-to-list",
                "Back to collection",
                false,
                nodes,
            );
            if response.clicked() {
                intents.push(OperatorIntent::Navigate(collection));
            }
        } else {
            show_collection_tabs(ui, collection, attention_count, intents, nodes);
        }
    });
    ui.add_space(8.0);
}

fn show_collection_tabs(
    ui: &mut egui::Ui,
    collection: PaneId,
    attention_count: usize,
    intents: &mut Vec<OperatorIntent>,
    nodes: &mut Vec<UiNode>,
) {
    for (pane, id, label) in [
        (
            INBOX_PANE_ID,
            "bokkie.collection.attention",
            format!("Needs attention · {attention_count}"),
        ),
        (
            OBLIGATIONS_PANE_ID,
            "bokkie.collection.all",
            "All obligations".to_owned(),
        ),
    ] {
        let response = ui.selectable_label(collection == pane, &label);
        record_navigation(&response, id, &label, collection == pane, nodes);
        if response.clicked() {
            intents.push(OperatorIntent::Navigate(pane));
        }
    }
}

fn record_navigation(
    response: &egui::Response,
    id: &str,
    label: &str,
    selected: bool,
    nodes: &mut Vec<UiNode>,
) {
    record_native_text_control(response, NativeTextControlKind::Selectable);
    let mut node = UiNode::container(
        SemanticUiId::new(id),
        Some(SemanticUiId::root()),
        UiRole::Section,
        response.rect.into(),
    );
    node.name = label.to_owned();
    node.selected = selected;
    node.focused = response.has_focus();
    nodes.push(node);
}

fn matches_search(obligation: &OperatorObligation, search: &str) -> bool {
    let query = search.trim().to_lowercase();
    query.is_empty()
        || obligation.state.label().to_lowercase().contains(&query)
        || obligation.id.to_lowercase().contains(&query)
        || obligation.description.to_lowercase().contains(&query)
        || obligation
            .last_error
            .as_deref()
            .is_some_and(|value| value.to_lowercase().contains(&query))
        || obligation
            .exception
            .as_ref()
            .is_some_and(|reason| exception_label(reason).to_lowercase().contains(&query))
}

fn show_search(
    ui: &mut egui::Ui,
    search: &str,
    pane: PaneId,
    intents: &mut Vec<OperatorIntent>,
    nodes: &mut Vec<UiNode>,
) {
    let mut value = search.to_owned();
    let response = ui.add(
        egui::TextEdit::singleline(&mut value)
            .id_salt("obligation-search")
            .hint_text("Search obligations")
            .desired_width(f32::INFINITY),
    );
    record_native_text_control(&response, NativeTextControlKind::Selectable);
    let mut node = UiNode::container(
        SemanticUiId::new("bokkie.obligation-search"),
        Some(SemanticUiId::pane(pane)),
        UiRole::Section,
        response.rect.into(),
    );
    node.name = "Search obligations".to_owned();
    node.focused = response.has_focus();
    nodes.push(node);
    if response.changed() {
        intents.push(OperatorIntent::Search(value));
    }
    ui.add_space(12.0);
}

fn show_inbox(
    ui: &mut egui::Ui,
    read: &InboxReadModel<'_>,
    intents: &mut Vec<OperatorIntent>,
    presentation: &mut PresentationContext,
) {
    egui::ScrollArea::vertical()
        .id_salt("bokkie-inbox-scroll")
        .show(ui, |ui| {
            if read.loading {
                empty_message(ui, "loading", "Loading operator exceptions…", presentation);
            } else if read.obligations.is_empty() {
                empty_message(
                    ui,
                    "empty",
                    if read.search.trim().is_empty() {
                        "Nothing needs your attention"
                    } else {
                        "No attention items match this search"
                    },
                    presentation,
                );
            }
            for obligation in &read.obligations {
                ui.add_space(presentation.tokens().spacing.unit.0);
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
                let height = attention_row_height(presentation.tokens(), presentation.font_scale());
                let response = ledger_row(
                    ui,
                    "inbox",
                    &obligation.id,
                    read.selected == Some(obligation.id.as_str()),
                    height,
                    &semantic_label,
                    INBOX_PANE_ID,
                    presentation,
                    |ui, presentation| {
                        attention_row_line(
                            ui,
                            "title",
                            attention_title(obligation),
                            &relative_time(obligation.updated_at, read.captured_at),
                            TextRole::Body,
                            presentation,
                        );
                        attention_row_line(
                            ui,
                            "reason",
                            &why,
                            obligation_source(obligation),
                            TextRole::Secondary,
                            presentation,
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

fn show_obligations(
    ui: &mut egui::Ui,
    read: &ObligationsReadModel<'_>,
    intents: &mut Vec<OperatorIntent>,
    presentation: &mut PresentationContext,
    virtualisation: &mut VirtualisationObservation,
) {
    let mut filter = read.filter;
    presentation.native(ui, NativeTextControlKind::ComboBox, |ui| {
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
        (combo.response, ())
    });
    if filter != read.filter {
        intents.push(OperatorIntent::Filter(filter));
    }
    // Retain the native matching-count label and its existing layout.
    presentation.raw(
        ui,
        "matching-count",
        "Native collection metadata label",
        |ui| {
            ui.label(format!("{} matching", read.obligations.len()));
        },
    );
    if read.loading {
        empty_message(ui, "loading", "Loading obligations…", presentation);
        return;
    }
    if read.obligations.is_empty() {
        empty_message(
            ui,
            "empty",
            if read.total == 0 {
                "This database contains no obligations"
            } else {
                "No obligations match the current search and state filter"
            },
            presentation,
        );
        return;
    }
    let row_height = row_height(
        presentation.tokens(),
        presentation.font_scale(),
        &OBLIGATION_ROW,
    );
    const OVERSCAN: usize = 4;
    egui::ScrollArea::vertical()
        .id_salt("bokkie-obligations-scroll")
        .show_viewport(ui, |ui, viewport| {
            let rows = virtual_rows(
                viewport.top(),
                viewport.height(),
                row_height,
                read.obligations.len(),
                OVERSCAN,
            );
            virtualisation.total_rows = read.obligations.len();
            virtualisation.visible_rows = (rows.visible.start, rows.visible.end);
            virtualisation.materialised_rows = (rows.materialised.start, rows.materialised.end);
            let origin = ui.min_rect().min;
            ui.set_min_height(read.obligations.len() as f32 * row_height);
            for index in rows.materialised {
                let obligation = read.obligations[index];
                let row_rect = egui::Rect::from_min_size(
                    origin + egui::vec2(0.0, index as f32 * row_height),
                    egui::vec2(ui.available_width(), row_height),
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(row_rect), |ui| {
                    let semantic_label = obligation_row_text(obligation, read.captured_at);
                    let response = ledger_row(
                        ui,
                        "obligation",
                        &obligation.id,
                        read.selected == Some(obligation.id.as_str()),
                        row_height,
                        &semantic_label,
                        OBLIGATIONS_PANE_ID,
                        presentation,
                        |ui, presentation| {
                            row_line(
                                ui,
                                "title",
                                &obligation.description,
                                TextRole::Body,
                                presentation,
                            );
                            row_line(
                                ui,
                                "status",
                                &ledger_status(obligation, read.captured_at),
                                TextRole::Secondary,
                                presentation,
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
) -> PresentationObservations {
    egui::ScrollArea::vertical()
        .id_salt((
            "bokkie-timeline-scroll",
            read.obligation.map(|item| item.id.as_str()),
        ))
        .show(ui, |ui| {
            let Some(obligation) = read.obligation else {
                let mut presentation = PresentationContext::new(
                    ui,
                    *tokens,
                    font_scale,
                    PresentationScope::new("bokkie.detail.empty"),
                    SemanticUiId::pane(TIMELINE_PANE_ID),
                );
                presentation.heading(ui, "activity-heading", "Activity and evidence");
                empty_message(
                    ui,
                    "empty",
                    "Select an item to understand what happens next and review its evidence",
                    &mut presentation,
                );
                return presentation.finish(ui);
            };
            let mut presentation = detail_presentation(ui, obligation, *tokens, font_scale);
            presentation.content(
                ui,
                "title",
                attention_title(obligation),
                ContentTextSpec {
                    role: TextRole::ApplicationTitle,
                    overflow: TextOverflow::Wrap,
                    max_lines: 3,
                    interaction: TextInteraction::Selectable,
                },
            );
            presentation.content(
                ui,
                "state",
                &format!(
                    "{} · {}",
                    obligation.state.label(),
                    obligation_source(obligation)
                ),
                ContentTextSpec {
                    role: if state_tone(obligation) == StatusTone::Error {
                        TextRole::Error
                    } else {
                        TextRole::Secondary
                    },
                    overflow: TextOverflow::Wrap,
                    max_lines: 2,
                    interaction: TextInteraction::Inert,
                },
            );
            if let Some(reason) = &obligation.exception {
                presentation.content(
                    ui,
                    "exception",
                    &exception_label(reason),
                    ContentTextSpec {
                        role: TextRole::Body,
                        overflow: TextOverflow::Wrap,
                        max_lines: 3,
                        interaction: TextInteraction::Selectable,
                    },
                );
            }
            if let Some(subject) = exact_gardener_subject(obligation) {
                presentation.heading(ui, "proposal-heading", "Proposal");
                egui::ScrollArea::vertical()
                    .id_salt(("proposal-reader", &obligation.id))
                    .max_height(TextRole::Body.style(tokens, font_scale).line_height * 8.0)
                    .show(ui, |ui| {
                        presentation.native(ui, NativeTextControlKind::Selectable, |ui| {
                            let response = ui.add(
                                egui::Label::new(
                                    TextRole::Body
                                        .style(tokens, font_scale)
                                        .rich_text(subject.prompt),
                                )
                                .wrap()
                                .selectable(true),
                            );
                            (response, ())
                        });
                    });
            }
            presentation.heading(ui, "next-step-heading", "What happens next");
            presentation.content(
                ui,
                "next-step",
                &next_step_label(obligation, read.topic.map(|topic| topic.captured_at)),
                ContentTextSpec {
                    role: TextRole::Body,
                    overflow: TextOverflow::Wrap,
                    max_lines: 3,
                    interaction: TextInteraction::Selectable,
                },
            );
            show_detail_actions(ui, read, obligation, intents, &mut presentation);
            egui::CollapsingHeader::new("Technical provenance")
                .id_salt(("obligation-technical-details", &obligation.id))
                .show(ui, |ui| {
                    presentation.property_row(
                        ui,
                        "technical-obligation",
                        "Obligation",
                        &obligation.id,
                    );
                    presentation.property_row(
                        ui,
                        "technical-description",
                        "Description",
                        &obligation.description,
                    );
                    presentation.property_row(
                        ui,
                        "technical-durable-liveness",
                        "Durable liveness",
                        &liveness_label(obligation.liveness.as_ref()),
                    );
                    if let Some(subject) = exact_gardener_subject(obligation) {
                        presentation.property_row(
                            ui,
                            "technical-repository",
                            "Repository",
                            subject.repository,
                        );
                        presentation.property_row(
                            ui,
                            "technical-goal-fingerprint",
                            "Goal fingerprint",
                            subject.fingerprint,
                        );
                        presentation.property_row(
                            ui,
                            "technical-proposal-instance",
                            "Proposal instance",
                            subject.instance_id,
                        );
                        presentation.property_row(
                            ui,
                            "technical-generation",
                            "Generation",
                            &subject.generation.to_string(),
                        );
                        presentation.property_row(
                            ui,
                            "technical-source-commit",
                            "Source commit",
                            subject.source_commit,
                        );
                        presentation.property_row(
                            ui,
                            "technical-source-observation",
                            "Source observation",
                            &subject.source_observation_id.to_string(),
                        );
                        presentation.property_row(
                            ui,
                            "technical-source-inspection",
                            "Source inspection",
                            subject.source_inspection_id,
                        );
                    }
                    presentation.property_row(
                        ui,
                        "technical-schedule",
                        "Schedule",
                        &recurrence_label(obligation),
                    );
                });
            if let Some(error) = read.topic_error {
                presentation.badge(ui, "topic-error", error, StatusTone::Error);
            }
            ui.separator();
            presentation.heading(ui, "activity-heading", "Activity and evidence");
            if read.loading && read.topic.is_none() {
                empty_message(
                    ui,
                    "evidence-loading",
                    "Loading durable evidence…",
                    &mut presentation,
                );
            } else if let Some(topic) = read.topic {
                if topic.items.is_empty() {
                    empty_message(
                        ui,
                        "evidence-empty",
                        "No durable topic items exist for this obligation",
                        &mut presentation,
                    );
                }
                for item in topic.items.iter().rev() {
                    show_topic_item(ui, item, topic.captured_at, &mut presentation);
                }
            }
            presentation.finish(ui)
        })
        .inner
}

fn action_is_relevant(action: LifecycleAction, obligation: &OperatorObligation) -> bool {
    let capability = action.capability(obligation);
    capability.available
        || matches!(
            capability.disabled_reason,
            Some(DisabledReason::RunningClaimOwnsObligation)
        )
}

fn detail_presentation(
    ui: &mut egui::Ui,
    obligation: &OperatorObligation,
    tokens: DesignTokens,
    font_scale: f32,
) -> PresentationContext {
    PresentationContext::new(
        ui,
        tokens,
        font_scale,
        PresentationScope::new("bokkie.detail").child(&obligation.id),
        SemanticUiId::pane(TIMELINE_PANE_ID),
    )
    .with_domain_reference(DomainReference::External {
        namespace: "bokkie.obligation".to_owned(),
        id: obligation.id.clone(),
    })
}

fn show_detail_actions(
    ui: &mut egui::Ui,
    read: &TimelineReadModel<'_>,
    obligation: &OperatorObligation,
    intents: &mut Vec<OperatorIntent>,
    presentation: &mut PresentationContext,
) {
    let mut disabled_explanations = BTreeSet::new();
    ui.horizontal_wrapped(|ui| {
        for action in [
            LifecycleAction::Approve,
            LifecycleAction::ApproveGardenerProposal,
            LifecycleAction::Retry,
            LifecycleAction::Reject,
            LifecycleAction::RejectGardenerProposal,
            LifecycleAction::Cancel,
        ]
        .into_iter()
        .filter(|action| action_is_relevant(*action, obligation))
        {
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
                            reason: "The selected evidence topic is still refreshing".into(),
                        }
                    } else if !read.connection.decisions_safe() {
                        Availability::Disabled {
                            reason: "Retained data may be stale; refresh before deciding".into(),
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
            if let Availability::Disabled { reason } = &availability {
                let key = if read.action_busy {
                    "action-busy"
                } else if read.snapshot_busy {
                    "snapshot-busy"
                } else if read.loading {
                    "topic-loading"
                } else if !read.connection.decisions_safe() {
                    "stale"
                } else {
                    match action.capability(obligation).disabled_reason {
                        Some(DisabledReason::StateDoesNotPermit) => "state-does-not-permit",
                        Some(DisabledReason::RunningClaimOwnsObligation) => "running-claim",
                        Some(DisabledReason::TerminalObligation) => "terminal",
                        Some(DisabledReason::GardenerProposalRequiresExactDecision) => {
                            "exact-decision"
                        }
                        Some(DisabledReason::NotGardenerProposal) => "not-gardener-proposal",
                        None => "unavailable",
                    }
                };
                disabled_explanations.insert((reason.to_string(), key));
            }
            let enabled = availability.enabled();
            let target = ActionTarget::pane(action, TIMELINE_PANE_ID);
            let response = presentation.action(
                ui,
                ("action", action.stable_id()),
                ActionButtonSpec {
                    target,
                    availability,
                    state: ActionButtonState::Momentary,
                    emphasis: if matches!(
                        action,
                        LifecycleAction::Approve
                            | LifecycleAction::ApproveGardenerProposal
                            | LifecycleAction::Retry
                    ) {
                        ActionEmphasis::Primary
                    } else {
                        ActionEmphasis::QuietBorderless
                    },
                    compact: false,
                },
            );
            if response.clicked() && enabled {
                intents.push(OperatorIntent::BeginAction(action));
            }
        }
    });
    if !LifecycleAction::ALL
        .into_iter()
        .any(|action| action_is_relevant(action, obligation))
    {
        presentation.content(
            ui,
            "no-actions",
            "No action needed in this state",
            ContentTextSpec {
                role: TextRole::Secondary,
                overflow: TextOverflow::Wrap,
                max_lines: 3,
                interaction: TextInteraction::Selectable,
            },
        );
    }
    for (reason, key) in disabled_explanations {
        presentation.content(
            ui,
            ("disabled-reason", key),
            &reason,
            ContentTextSpec {
                role: TextRole::Secondary,
                overflow: TextOverflow::Wrap,
                max_lines: 3,
                interaction: TextInteraction::Selectable,
            },
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn show_topic_item(
    ui: &mut egui::Ui,
    item: &TopicItem,
    captured_at: i64,
    presentation: &mut PresentationContext,
) {
    presentation.scoped(ui, ("topic", &item.stable_id), |ui, presentation| {
        ui.add_space(presentation.tokens().spacing.section.0);
        presentation.heading(ui, "event-heading", &format!("{} · {}",
            source_label(item.source), event_label(&item.event_type)));
        presentation.content(ui, "occurred-at", &relative_time(item.occurred_at, Some(captured_at)),
            ContentTextSpec { role: TextRole::Secondary, overflow: TextOverflow::Wrap,
                max_lines: 2, interaction: TextInteraction::Inert });
        let fields = common_evidence(&item.evidence);
        let summary = fields.iter().filter(|(_, label, _)| evidence_summary(label)).take(2)
            .map(|(_, label, value)| if *label == "Repository" { value.clone() }
                else { format!("{label}: {value}") }).collect::<Vec<_>>().join(" · ");
        if !summary.is_empty() {
            row_line(ui, "summary", &summary, TextRole::Secondary, presentation);
        }
        egui::CollapsingHeader::new("Event technical details")
            .id_salt(("event-technical-details", &item.stable_id))
            .show(ui, |ui| {
                presentation.property_row(ui, "unix-time", "Unix time", &item.occurred_at.to_string());
                for (path, label, value) in &fields {
                    presentation.property_row(ui, ("evidence-field", path), label, value);
                }
                presentation.property_row(ui, "stable-id", "Stable ID", &item.stable_id);
                presentation.property_row(ui, "source-sequence", "Source sequence", &item.source_sequence);
                if let Some(occurrence) = item.occurrence {
                    presentation.property_row(ui, "occurrence", "Occurrence", &occurrence.to_string());
                }
            });
        let disclosure = egui::CollapsingHeader::new("Raw durable evidence")
            .id_salt(("raw-evidence", &item.stable_id))
            .show(ui, |ui| {
                let raw = serde_json::to_string_pretty(&item.evidence)
                    .unwrap_or_else(|_| "Evidence could not be formatted".to_owned());
                let height = TextRole::Body.style(presentation.tokens(), presentation.font_scale()).line_height * 12.0;
                egui::ScrollArea::vertical()
                    .id_salt(("evidence-reader", &item.stable_id))
                    .max_height(height).min_scrolled_height(height)
                    .show(ui, |ui| {
                        let style = TextRole::MonospaceTechnical.style(presentation.tokens(), presentation.font_scale());
                        let node = presentation.raw(ui, "evidence-reader",
                            "Unbounded selectable evidence uses the actual native galley; only complete painted rows form visible-tail evidence, not measured text coverage",
                            |ui| {
                                let galley = egui::WidgetText::from(style.rich_text(raw)).into_galley(
                                    ui, Some(egui::TextWrapMode::Wrap), ui.available_width(), egui::TextStyle::Monospace);
                                let response = ui.add(egui::Label::new(galley.clone()).selectable(true));
                                record_native_text_control(&response, NativeTextControlKind::Selectable);
                                // Read the galley submitted by the real selectable label, retaining
                                // only complete rows inside the current paint clip. This native
                                // reader is deliberately outside the measured-text denominator.
                                let visible = visible_reader_text(&galley, response.rect.min, ui.clip_rect());
                                let mut node = UiNode::container(
                                    SemanticUiId::new(format!("bokkie.evidence-reader.{}", item.stable_id)),
                                    Some(SemanticUiId::pane(TIMELINE_PANE_ID)), UiRole::ScrollArea,
                                    response.rect.intersect(ui.clip_rect()).into());
                                node.name = visible;
                                node.text_selectable = true;
                                node
                            });
                        presentation.observe_node(ui, node);
                    });
            });
        let mut node = UiNode::container(
            SemanticUiId::new(format!("bokkie.raw-evidence.{}", item.stable_id)),
            Some(SemanticUiId::pane(TIMELINE_PANE_ID)), UiRole::Section,
            disclosure.header_response.rect.into());
        node.name = "Raw durable evidence".to_owned();
        node.expanded = Some(disclosure.body_response.is_some());
        presentation.observe_node(ui, node);
    });
}

fn visible_reader_text(galley: &egui::Galley, origin: egui::Pos2, clip: egui::Rect) -> String {
    galley
        .rows
        .iter()
        .filter(|row| clip.contains_rect(row.rect().translate(origin.to_vec2())))
        .map(|row| row.glyphs.iter().map(|glyph| glyph.chr).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
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
                        state: ActionButtonState::Momentary,
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
                    ActionButtonState::Momentary,
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
                        state: ActionButtonState::Momentary,
                        emphasis: ActionEmphasis::QuietBorderless,
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
                    ActionButtonState::Momentary,
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

const INBOX_ROW: [(TextRole, u8); 2] = [(TextRole::Body, 1), (TextRole::Secondary, 1)];
const OBLIGATION_ROW: [(TextRole, u8); 2] = INBOX_ROW;

/// The two collections and activity summary share this one-line recipe.
fn row_line(
    ui: &mut egui::Ui,
    key: impl std::hash::Hash + std::fmt::Debug,
    value: &str,
    role: TextRole,
    presentation: &mut PresentationContext,
) {
    presentation.fixed_slot(
        ui,
        key,
        value,
        ContentTextSpec {
            role,
            overflow: TextOverflow::Ellipsis,
            max_lines: 1,
            interaction: TextInteraction::Inert,
        },
    );
}

fn attention_row_line(
    ui: &mut egui::Ui,
    key: &'static str,
    primary: &str,
    metadata: &str,
    role: TextRole,
    presentation: &mut PresentationContext,
) {
    let tokens = presentation.tokens();
    let width = ui.available_width();
    let height = role.style(tokens, presentation.font_scale()).line_height;
    let metadata_width = (width * 0.29).clamp(64.0, 130.0);
    let (_, rect) = ui.allocate_space(egui::vec2(width, height));
    let main_rect = egui::Rect::from_min_max(
        rect.min,
        egui::pos2(
            rect.right() - metadata_width - tokens.spacing.inline.0,
            rect.bottom(),
        ),
    );
    let metadata_rect = egui::Rect::from_min_max(
        egui::pos2(rect.right() - metadata_width, rect.top()),
        rect.max,
    );
    for (slot, value, slot_role, field) in [
        (main_rect, primary, role, "primary"),
        (metadata_rect, metadata, TextRole::Secondary, "metadata"),
    ] {
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(slot)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        row_line(&mut child, (key, field), value, slot_role, presentation);
    }
}

fn attention_title(obligation: &OperatorObligation) -> &str {
    if let Some(subject) = exact_gardener_subject(obligation)
        && obligation.description
            == format!(
                "Implement approved gardener proposal {}",
                subject.fingerprint
            )
    {
        "Review proposed code change"
    } else {
        &obligation.description
    }
}

fn obligation_source(obligation: &OperatorObligation) -> &str {
    exact_gardener_subject(obligation)
        .map(|subject| subject.repository)
        .unwrap_or(if obligation.id.starts_with("gardener:") {
            "Coding gardener"
        } else {
            "Bokkie"
        })
}

fn ledger_status(obligation: &OperatorObligation, captured_at: Option<i64>) -> String {
    let wake = obligation
        .next_wake_at
        .map(|at| format!("wake {}", relative_time(at, captured_at)))
        .unwrap_or_else(|| "no wake".to_owned());
    format!(
        "{} · {} · attempts {}/{}",
        obligation.state.label(),
        wake,
        obligation.attempts_made,
        obligation.max_attempts
    )
}

fn attention_row_height(tokens: &DesignTokens, font_scale: f32) -> f32 {
    row_height(tokens, font_scale, &INBOX_ROW) + tokens.spacing.unit.0
}

fn row_height(tokens: &DesignTokens, font_scale: f32, recipe: &[(TextRole, u8)]) -> f32 {
    recipe
        .iter()
        .map(|(role, lines)| role.style(tokens, font_scale).line_height * f32::from(*lines))
        .sum::<f32>()
        + tokens.spacing.block.0 * recipe.len().saturating_sub(1) as f32
        + tokens.spacing.inline.0 * 2.0
}

fn exception_text_role(reason: Option<&ExceptionReason>) -> TextRole {
    match reason {
        Some(ExceptionReason::Attention { cause, .. })
            if !matches!(
                cause,
                AttentionCause::Rejected { .. }
                    | AttentionCause::GardenerVerificationInconclusive { .. }
            ) =>
        {
            TextRole::Error
        }
        Some(ExceptionReason::ExpiredLease { .. }) => TextRole::Error,
        _ => TextRole::Body,
    }
}

fn next_step_label(obligation: &OperatorObligation, captured_at: Option<i64>) -> String {
    match obligation.liveness.as_ref() {
        Some(DurableLiveness::FutureWake { wake_at }) => {
            format!(
                "Next scheduled wake {}",
                relative_time(*wake_at, captured_at)
            )
        }
        Some(DurableLiveness::ActiveLease { expires_at, .. }) => {
            format!(
                "Work is running; lease expires {}",
                relative_time(*expires_at, captured_at)
            )
        }
        Some(DurableLiveness::HumanAttention { .. }) => [
            LifecycleAction::ApproveGardenerProposal,
            LifecycleAction::Approve,
            LifecycleAction::Retry,
            LifecycleAction::Cancel,
        ]
        .into_iter()
        .find_map(|action| {
            let capability = action.capability(obligation);
            capability
                .available
                .then(|| consequence_label(capability).to_owned())
        })
        .unwrap_or_else(|| "Review the activity and evidence before deciding".to_owned()),
        None => "No further work is scheduled".to_owned(),
    }
}

#[allow(clippy::too_many_arguments)]
fn ledger_row(
    ui: &mut egui::Ui,
    scope: &'static str,
    stable_id: &str,
    selected: bool,
    height: f32,
    semantic_label: &str,
    pane: PaneId,
    presentation: &mut PresentationContext,
    content: impl FnOnce(&mut egui::Ui, &mut PresentationContext),
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
    presentation.observe_node(
        ui,
        UiNode {
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
            domain_reference: Some(DomainReference::External {
                namespace: "bokkie.obligation".to_owned(),
                id: stable_id.to_owned(),
            }),
            actions: Vec::new(),
            text_selectable: false,
            disabled_reason: None,
        },
    );
    let tokens = *presentation.tokens();
    presentation.raw(
        ui,
        ("row-chrome", stable_id),
        "Application-owned collection selection marker, hover surface and keyboard focus ring",
        |ui| {
            if selected || response.hovered() {
                ui.painter().rect_filled(
                    rect,
                    6.0,
                    if selected {
                        tokens.colours.selection_background
                    } else {
                        tokens.colours.surface_hover
                    },
                );
            }
            if selected {
                let marker = egui::Rect::from_min_max(
                    rect.left_top() + egui::vec2(0.0, 8.0),
                    egui::pos2(rect.left() + 3.0, rect.bottom() - 8.0),
                );
                ui.painter()
                    .rect_filled(marker, 1.5, tokens.colours.selection_indicator);
            }
            if response.has_focus() && keyboard_focus_visible(ui.ctx()) {
                ui.painter().rect_stroke(
                    rect,
                    6.0,
                    egui::Stroke::new(2.0, tokens.colours.focus_ring),
                    egui::StrokeKind::Inside,
                );
            }
        },
    );
    let content_rect = rect.shrink(presentation.tokens().spacing.inline.0);
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(("bokkie-ledger-row-content", scope, stable_id))
            .max_rect(content_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(ui.clip_rect().intersect(content_rect));
    presentation.scoped(&mut child, stable_id, content);
    response
}

/// Keep a persistent selection marker independent of the keyboard focus ring.
fn keyboard_focus_visible(context: &egui::Context) -> bool {
    let keyboard = context.input(|input| {
        input.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Key {
                    key: egui::Key::Tab
                        | egui::Key::ArrowUp
                        | egui::Key::ArrowDown
                        | egui::Key::ArrowLeft
                        | egui::Key::ArrowRight,
                    pressed: true,
                    ..
                }
            )
        })
    });
    let pointer = context.input(|input| input.pointer.any_pressed());
    context.data_mut(|data| {
        let visible =
            data.get_temp_mut_or_default::<bool>(egui::Id::new("bokkie.keyboard-focus-visible"));
        if keyboard {
            *visible = true;
        }
        if pointer {
            *visible = false;
        }
        *visible
    })
}

fn empty_message(
    ui: &mut egui::Ui,
    key: &'static str,
    message: &str,
    presentation: &mut PresentationContext,
) {
    presentation.content(
        ui,
        key,
        message,
        ContentTextSpec {
            role: TextRole::Secondary,
            overflow: TextOverflow::Wrap,
            max_lines: 3,
            interaction: TextInteraction::Selectable,
        },
    );
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
        ConnectionState::Current => "Connected",
        ConnectionState::Stale { .. } => "Stale",
    };
    state.to_owned()
}

fn freshness_state(connection: &ConnectionState) -> &'static str {
    match connection {
        ConnectionState::Loading => "loading",
        ConnectionState::Current => "current",
        ConnectionState::Stale { .. } => "stale retained data",
    }
}

fn state_tone(obligation: &OperatorObligation) -> StatusTone {
    match obligation.state {
        bokkie_operator_api::OperatorObligationState::Attention
            if exception_text_role(obligation.exception.as_ref()) == TextRole::Error =>
        {
            StatusTone::Error
        }
        bokkie_operator_api::OperatorObligationState::AwaitingApproval => StatusTone::Neutral,
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
        let seconds = delta.unsigned_abs();
        if seconds < 60 {
            format!("in {seconds}s")
        } else if seconds < 3_600 {
            format!("in {}m", seconds / 60)
        } else if seconds < 86_400 {
            format!("in {}h", seconds / 3_600)
        } else {
            format!("in {}d", seconds / 86_400)
        }
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
            ApprovalSubject::Generic => "Awaiting your approval".to_owned(),
            ApprovalSubject::GardenerProposal { .. } => {
                "Review the proposed code change before implementation".to_owned()
            }
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
    let mut lines = vec![
        obligation.description.clone(),
        format!(
            "{} · {} · occurrence {} · attempts {}/{}",
            obligation.id,
            obligation.state.label(),
            obligation.occurrence,
            obligation.attempts_made,
            obligation.max_attempts
        ),
        next_step_label(obligation, captured_at),
        format!(
            "{} · updated {}",
            recurrence_label(obligation),
            relative_time(obligation.updated_at, captured_at)
        ),
    ];
    if let Some(error) = &obligation.last_error {
        lines.push(format!("Error: {error}"));
    }
    if let Some(evidence) = &obligation.last_evidence {
        lines.push(format!("Evidence: {evidence}"));
    }
    lines.join("\n")
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

fn evidence_summary(label: &str) -> bool {
    matches!(
        label,
        "Attempt"
            | "Attempt number"
            | "Repository"
            | "Pull request"
            | "Verification outcome"
            | "Verification verdict"
            | "Verification summary"
            | "Verification"
            | "Run phase"
            | "Attempt outcome"
            | "Retryable"
            | "Approval decision"
            | "Decision"
            | "Immutable prompt"
            | "Actor"
            | "Decision note"
            | "Error"
            | "Evidence"
            | "Evidence value"
            | "From state"
            | "To state"
    )
}

fn common_evidence(evidence: &Value) -> Vec<(Vec<&'static str>, &'static str, String)> {
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
        return vec![(vec!["$value"], "Evidence value", value_text(evidence))];
    };
    let mut output = FIELDS
        .into_iter()
        .filter_map(|(key, label)| {
            object
                .get(key)
                .filter(|value| !value.is_null())
                .map(|value| (vec![key], label, value_text(value)))
        })
        .collect::<Vec<_>>();
    if let Some(Value::String(details)) = object.get("details_json")
        && let Ok(parsed) = serde_json::from_str::<Value>(details)
    {
        output.extend(
            common_evidence(&parsed)
                .into_iter()
                .map(|(mut path, label, value)| {
                    path.insert(0, "details_json");
                    (path, label, value)
                }),
        );
    }
    output
}

fn value_text(value: &Value) -> String {
    value.as_str().map_or_else(
        || serde_json::to_string(value).unwrap_or_else(|_| "unavailable".to_owned()),
        ToOwned::to_owned,
    )
}

fn poll_plan(snapshot: &bokkie_operator_api::OperatorSnapshot) -> (Duration, BTreeSet<String>) {
    let deadlines = snapshot.obligations.iter().filter_map(|obligation| {
        let lease_expiry = match obligation.liveness.as_ref() {
            Some(DurableLiveness::ActiveLease { expires_at, .. }) => Some(*expires_at),
            _ => None,
        };
        obligation
            .next_wake_at
            .into_iter()
            .chain(lease_expiry)
            .min()
            .map(|deadline| {
                (
                    deadline.saturating_sub(snapshot.captured_at).max(1) as u64,
                    obligation.id.clone(),
                )
            })
    });
    let Some((earliest, _)) = deadlines.clone().min_by_key(|(seconds, _)| *seconds) else {
        return (POLL_MAX, BTreeSet::new());
    };
    let delay = Duration::from_secs(earliest).min(POLL_MAX);
    let affected = if delay < POLL_MAX || earliest == POLL_MAX.as_secs() {
        deadlines
            .filter_map(|(seconds, id)| (seconds == earliest).then_some(id))
            .collect()
    } else {
        BTreeSet::new()
    };
    (delay, affected)
}

#[cfg(not(target_arch = "wasm32"))]
fn current_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}

#[cfg(target_arch = "wasm32")]
fn current_unix_seconds() -> i64 {
    (js_sys::Date::now() / 1_000.0) as i64
}

#[cfg(test)]
mod tests {
    use bokkie_operator_api::{
        API_CONTRACT_VERSION, ActionCapability, ActionConsequence, ActionPrecondition,
        BOKKIE_BUILD_ID, DisabledReason, OperatorCapabilities, OperatorObligationState,
        OperatorSnapshot, ProjectionChange, ProjectionChangePage, ProjectionEventProvenance,
        ProjectionEventSource, SUPPORTED_SCHEMA_VERSION, ServiceIdentity, SessionBootstrap,
    };

    use super::*;

    #[test]
    fn confirmation_transition_observations_match_each_render_pass() {
        use eframe::App;

        for repeat_layout in [false, true] {
            let context = egui::Context::default();
            let mut app = test_app();
            app.model.apply_snapshot(OperatorSnapshot {
                captured_at: 100,
                service: None,
                next_cursor: None,
                watermark: 9,
                obligations: vec![fixture(1)],
            });
            app.model.selected_obligation = Some(fixture(1).id);
            let input = |events| egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1440.0, 900.0),
                )),
                events,
                ..Default::default()
            };
            let mut frame = eframe::Frame::_new_kittest();
            for _ in 0..2 {
                context
                    .run_ui(input(Vec::new()), |ui| app.ui(ui, &mut frame))
                    .textures_delta
                    .clear();
            }
            let button = app
                .test_snapshot()
                .ui_snapshot
                .nodes
                .into_iter()
                .find(|node| {
                    node.enabled
                        && node
                            .actions
                            .iter()
                            .any(|action| action.0 == LifecycleAction::Cancel.stable_id())
                })
                .expect("rendered cancellation action");
            let point = egui::pos2(
                (button.rect.min_x + button.rect.max_x) / 2.0,
                (button.rect.min_y + button.rect.max_y) / 2.0,
            );
            let pointer = |pressed| egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            context
                .run_ui(
                    input(vec![egui::Event::PointerMoved(point), pointer(true)]),
                    |ui| app.ui(ui, &mut frame),
                )
                .textures_delta
                .clear();
            let mut passes = Vec::new();
            context
                .run_ui(input(vec![pointer(false)]), |ui| {
                    app.ui(ui, &mut frame);
                    passes.push(app.test_snapshot());
                    if context.current_pass_index() == 0 {
                        // Reapplying BeginAction would replace this draft with its
                        // default actor, even if the dialog still appeared open.
                        app.model
                            .confirmation
                            .as_mut()
                            .expect("click applied its intent")
                            .actor = "transition-pass operator".to_owned();
                    }
                    if repeat_layout && context.current_pass_index() == 0 {
                        context.request_discard(
                            "exercise confirmation transition across layout passes",
                        );
                    }
                })
                .textures_delta
                .clear();
            assert!(app.model.confirmation.is_some(), "click applied its intent");
            assert_eq!(
                app.model.confirmation.as_ref().unwrap().actor,
                "transition-pass operator",
                "a repeated layout must not reapply the action and replace the draft"
            );
            assert!(passes[0].interaction.confirmation_action.is_none());
            assert_eq!(passes.len(), if repeat_layout { 2 } else { 1 });
            context
                .run_ui(input(Vec::new()), |ui| {
                    app.ui(ui, &mut frame);
                    passes.push(app.test_snapshot());
                })
                .textures_delta
                .clear();
            assert_eq!(
                passes
                    .last()
                    .unwrap()
                    .interaction
                    .confirmation_action
                    .as_deref(),
                Some(LifecycleAction::Cancel.stable_id())
            );
            for pass in passes {
                let rendered_confirmation = pass
                    .ui_snapshot
                    .nodes
                    .iter()
                    .any(|node| node.id == SemanticUiId::new("bokkie.lifecycle-confirmation"));
                assert_eq!(
                    pass.interaction.confirmation_action.is_some(),
                    rendered_confirmation,
                    "interaction and semantics must describe the same pass"
                );
            }
        }
    }

    #[test]
    fn row_recipes_contain_painted_text_at_normal_and_enlarged_scales() {
        use polyorama_ui_egui::{DensityPreference, audit_text_layouts};
        for density in [DensityPreference::Comfortable, DensityPreference::Compact] {
            for font_scale in [1.0, 1.5] {
                let context = egui::Context::default();
                let preferences = UiPreferences {
                    density,
                    font_scale,
                    ..UiPreferences::default()
                };
                apply_design_system_with_typography(
                    &context,
                    preferences,
                    TypographyProfile::Reading,
                );
                let tokens = preferences
                    .tokens(false)
                    .with_typography_profile(TypographyProfile::Reading);
                for recipe in [&INBOX_ROW[..], &OBLIGATION_ROW[..]] {
                    let mut observations = Vec::new();
                    let mut output = context.run_ui(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO, egui::vec2(400.0, 800.0))), ..Default::default() }, |ui| {
                        let mut presentation = PresentationContext::new(ui, tokens, font_scale,
                            PresentationScope::new("geometry-test"), SemanticUiId::pane(OBLIGATIONS_PANE_ID));
                        ledger_row(ui, "geometry-test", "fixture", false,
                            row_height(&tokens, font_scale, recipe), "Representative row",
                            OBLIGATIONS_PANE_ID, &mut presentation, |ui, presentation| {
                                for (role, lines) in recipe {
                                    presentation.fixed_slot(ui, format!("{role:?}"),
                                        "Representative long content that wraps across the deliberately reserved row slots",
                                        ContentTextSpec { role: *role, overflow: TextOverflow::Wrap,
                                            max_lines: *lines, interaction: TextInteraction::Inert });
                                }
                            });
                        observations = presentation.finish(ui).text_layouts;
                    });
                    output.textures_delta.clear();
                    assert_eq!(observations.len(), recipe.len());
                    assert!(
                        audit_text_layouts(&observations).is_empty(),
                        "{density:?} {font_scale}: {:?}",
                        audit_text_layouts(&observations)
                    );
                    for item in observations {
                        assert!(item.allocated_rect.max_y <= item.clip_rect.max_y + 1.0);
                        assert!(item.allocated_rect.min_y >= item.clip_rect.min_y - 1.0);
                    }
                }
            }
        }
    }

    #[test]
    fn detail_navigation_retains_collection_search_and_filter() {
        let mut app = test_app();
        let context = egui::Context::default();
        app.model.apply_snapshot(OperatorSnapshot {
            captured_at: 100,
            service: None,
            next_cursor: None,
            watermark: 9,
            obligations: vec![fixture(1)],
        });
        for collection in [INBOX_PANE_ID, OBLIGATIONS_PANE_ID] {
            app.apply_intents(
                vec![
                    OperatorIntent::Navigate(collection),
                    OperatorIntent::Search("deterministic".into()),
                ],
                &context,
            );
            let filter = app.model.state_filter;
            app.apply_intents(
                vec![OperatorIntent::Select {
                    obligation_id: fixture(1).id,
                    destination: Some(TIMELINE_PANE_ID),
                }],
                &context,
            );
            assert_eq!(app.workspace.active_pane, TIMELINE_PANE_ID);
            assert_eq!(app.collection, collection);
            app.apply_intents(vec![OperatorIntent::Navigate(app.collection)], &context);
            assert_eq!(app.workspace.active_pane, collection);
            assert_eq!(app.model.search, "deterministic");
            assert_eq!(app.model.state_filter, filter);
        }
    }

    #[test]
    fn attention_search_matches_projected_reason_and_scheduling_stays_neutral() {
        let mut obligation = fixture(1);
        assert_eq!(state_tone(&obligation), StatusTone::Neutral);
        obligation.state = OperatorObligationState::AwaitingApproval;
        obligation.exception = Some(ExceptionReason::AwaitingApproval {
            subject: ApprovalSubject::Generic,
        });
        assert!(matches_search(&obligation, "APPROVAL"));
        assert_eq!(state_tone(&obligation), StatusTone::Neutral);
        assert!(!matches_search(&obligation, "absent phrase"));
        let summary = ledger_status(&obligation, Some(100));
        assert!(summary.contains("wake"));
        assert!(summary.contains("attempts 1/3"));
        assert!(summary.contains(obligation.state.label()));
    }

    #[test]
    fn action_area_keeps_relevant_stale_actions_with_visible_explanation() {
        let context = egui::Context::default();
        let preferences = UiPreferences::default();
        apply_design_system_with_typography(&context, preferences, TypographyProfile::Reading);
        let tokens = preferences
            .tokens(false)
            .with_typography_profile(TypographyProfile::Reading);
        let obligation = fixture(1);
        assert!(action_is_relevant(LifecycleAction::Cancel, &obligation));
        assert!(!action_is_relevant(LifecycleAction::Approve, &obligation));
        assert!(!action_is_relevant(
            LifecycleAction::ApproveGardenerProposal,
            &obligation
        ));
        let stale = ConnectionState::Stale {
            reason: "lost connection".to_owned(),
        };
        let read = TimelineReadModel {
            obligation: Some(&obligation),
            topic: None,
            topic_error: None,
            loading: false,
            connection: &stale,
            action_busy: false,
            snapshot_busy: false,
        };
        let mut nodes = Vec::new();
        let mut text = Vec::new();
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            let mut presentation = detail_presentation(ui, &obligation, tokens, 1.0);
            show_detail_actions(ui, &read, &obligation, &mut Vec::new(), &mut presentation);
            let observed = presentation.finish(ui);
            text.extend(observed.text_layouts);
            nodes.extend(observed.semantic_nodes);
        });
        output.textures_delta.clear();
        assert_eq!(nodes.len(), 1);
        assert!(!nodes[0].enabled);
        assert!(nodes[0].disabled_reason.as_ref().unwrap().contains("stale"));
        // The explanation receives its own measured, selectable painted text slot.
        assert!(
            text.iter()
                .any(|item| item.interaction == TextInteraction::Selectable)
        );
    }

    #[test]
    fn repeated_detail_actions_keep_domain_identity_when_reordered() {
        let context = egui::Context::default();
        let preferences = UiPreferences::default();
        apply_design_system_with_typography(&context, preferences, TypographyProfile::Reading);
        let tokens = preferences
            .tokens(false)
            .with_typography_profile(TypographyProfile::Reading);
        let obligations = [fixture(1), fixture(2)];
        let render = |order: [usize; 2]| {
            let mut nodes = Vec::new();
            context
                .run_ui(egui::RawInput::default(), |ui| {
                    for index in order {
                        let obligation = &obligations[index];
                        let read = TimelineReadModel {
                            obligation: Some(obligation),
                            topic: None,
                            topic_error: None,
                            loading: false,
                            connection: &ConnectionState::Current,
                            action_busy: false,
                            snapshot_busy: false,
                        };
                        let mut presentation = detail_presentation(ui, obligation, tokens, 1.0);
                        show_detail_actions(
                            ui,
                            &read,
                            obligation,
                            &mut Vec::new(),
                            &mut presentation,
                        );
                        nodes.extend(presentation.finish(ui).semantic_nodes);
                    }
                })
                .textures_delta
                .clear();
            nodes
        };
        let original = render([0, 1]);
        let reordered = render([1, 0]);
        assert_eq!(original.len(), 2);
        assert_ne!(original[0].id, original[1].id);
        assert_eq!(
            original[0].actions, original[1].actions,
            "capability is independent of instance"
        );
        for (node, obligation) in original.iter().zip(&obligations) {
            assert_eq!(
                node.domain_reference,
                Some(DomainReference::External {
                    namespace: "bokkie.obligation".to_owned(),
                    id: obligation.id.clone(),
                })
            );
            let moved = reordered
                .iter()
                .find(|candidate| candidate.domain_reference == node.domain_reference)
                .unwrap();
            assert_eq!(node.id, moved.id, "reordering retains the logical control");
        }
    }

    #[test]
    fn detail_presentation_identity_survives_desktop_and_narrow_navigation() {
        use eframe::App;

        let context = egui::Context::default();
        Appearance::default().apply(&context);
        let mut app = test_app();
        app.model.apply_snapshot(OperatorSnapshot {
            captured_at: 100,
            service: None,
            next_cursor: None,
            watermark: 9,
            obligations: vec![fixture(1)],
        });
        app.model.selected_obligation = Some(fixture(1).id);
        app.workspace.active_pane = TIMELINE_PANE_ID;
        let mut frame = eframe::Frame::_new_kittest();
        let mut identities = Vec::new();
        for width in [1440.0, 420.0, 1440.0] {
            for _ in 0..2 {
                context
                    .run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(width, 900.0),
                            )),
                            ..Default::default()
                        },
                        |ui| app.ui(ui, &mut frame),
                    )
                    .textures_delta
                    .clear();
            }
            let snapshot = app.test_snapshot().ui_snapshot;
            assert!(
                snapshot.text_audit.is_empty(),
                "width {width}: {:?}",
                snapshot.text_audit
            );
            assert!(snapshot.semantic_audit.is_empty());
            let action = snapshot
                .nodes
                .iter()
                .find(|node| {
                    node.actions
                        .iter()
                        .any(|action| action.0 == LifecycleAction::Cancel.stable_id())
                })
                .unwrap();
            assert!(action.enabled);
            let title = snapshot
                .text
                .iter()
                .find(|text| text.role == TextRole::ApplicationTitle)
                .unwrap();
            identities.push((action.id.clone(), title.component_id));
        }
        assert!(identities.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn attention_rows_have_two_lines_and_visible_metadata_at_both_widths() {
        use polyorama_ui_egui::audit_text_layouts;
        let preferences = UiPreferences::default();
        let tokens = preferences
            .tokens(false)
            .with_typography_profile(TypographyProfile::Reading);
        let height = attention_row_height(&tokens, 1.0);
        assert!(
            (56.0..=72.0).contains(&height),
            "default attention row height: {height}"
        );
        for width in [340.0, 480.0] {
            let context = egui::Context::default();
            apply_design_system_with_typography(&context, preferences, TypographyProfile::Reading);
            let obligation = fixture(1);
            let mut observations = Vec::new();
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 800.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    let mut presentation = PresentationContext::new(
                        ui,
                        tokens,
                        1.0,
                        PresentationScope::new("geometry-test"),
                        SemanticUiId::pane(INBOX_PANE_ID),
                    );
                    ledger_row(
                        ui,
                        "inbox",
                        &obligation.id,
                        false,
                        height,
                        "Test row",
                        INBOX_PANE_ID,
                        &mut presentation,
                        |ui, presentation| {
                            attention_row_line(
                                ui,
                                "title",
                                &obligation.description,
                                "2m ago",
                                TextRole::Body,
                                presentation,
                            );
                            attention_row_line(
                                ui,
                                "reason",
                                "Approval needed",
                                "Bokkie",
                                TextRole::Secondary,
                                presentation,
                            );
                        },
                    );
                    observations = presentation.finish(ui).text_layouts;
                },
            );
            output.textures_delta.clear();
            assert_eq!(observations.len(), 4);
            assert!(
                audit_text_layouts(&observations).is_empty(),
                "{:?}",
                audit_text_layouts(&observations)
            );
            for item in observations {
                assert_eq!(item.declared_max_lines, 1);
                assert!(item.allocated_rect.max_y <= item.clip_rect.max_y + 1.0);
            }
        }
    }

    #[test]
    fn collection_row_identity_survives_reordering_and_narrow_layout() {
        use std::collections::BTreeMap;
        for pane in [INBOX_PANE_ID, OBLIGATIONS_PANE_ID] {
            let context = egui::Context::default();
            Appearance::default().apply(&context);
            let tokens = UiPreferences::default()
                .tokens(false)
                .with_typography_profile(TypographyProfile::Reading);
            let first = fixture(1);
            let second = fixture(2);
            let mut previous = None;
            for (width, reversed) in [(480.0, false), (340.0, true), (480.0, false)] {
                let mut observed = None;
                context
                    .run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(width, 800.0),
                            )),
                            ..Default::default()
                        },
                        |ui| {
                            // Incidental shell identity must not enter logical row identity.
                            ui.push_id(if reversed { "narrow" } else { "desktop" }, |ui| {
                                let mut presentation = PresentationContext::new(
                                    ui,
                                    tokens,
                                    1.0,
                                    PresentationScope::new(("bokkie.collection", pane.0)),
                                    SemanticUiId::pane(pane),
                                );
                                let obligations = if reversed {
                                    vec![&second, &first]
                                } else {
                                    vec![&first, &second]
                                };
                                if pane == INBOX_PANE_ID {
                                    show_inbox(
                                        ui,
                                        &InboxReadModel {
                                            search: "",
                                            obligations,
                                            selected: Some(&first.id),
                                            captured_at: Some(100),
                                            connection: &ConnectionState::Current,
                                            loading: false,
                                            selection_destination: Some(TIMELINE_PANE_ID),
                                        },
                                        &mut Vec::new(),
                                        &mut presentation,
                                    );
                                } else {
                                    show_obligations(
                                        ui,
                                        &ObligationsReadModel {
                                            obligations,
                                            total: 2,
                                            selected: Some(&first.id),
                                            filter: StateFilter::All,
                                            captured_at: Some(100),
                                            loading: false,
                                            selection_destination: Some(TIMELINE_PANE_ID),
                                        },
                                        &mut Vec::new(),
                                        &mut presentation,
                                        &mut VirtualisationObservation::default(),
                                    );
                                }
                                observed = Some(presentation.finish(ui));
                            });
                        },
                    )
                    .textures_delta
                    .clear();
                let observed = observed.unwrap();
                assert!(polyorama_ui_egui::audit_text_layouts(&observed.text_layouts).is_empty());
                let fields = if pane == INBOX_PANE_ID { 4 } else { 2 };
                assert_eq!(observed.text_layouts.len(), fields * 2);
                let identities = observed
                    .semantic_nodes
                    .iter()
                    .zip(observed.text_layouts.chunks(fields))
                    .map(|(node, text)| {
                        let DomainReference::External { namespace, id } =
                            node.domain_reference.as_ref().unwrap()
                        else {
                            panic!("row must retain its obligation identity");
                        };
                        assert_eq!(namespace, "bokkie.obligation");
                        assert_eq!(node.selected, id == &first.id);
                        let expected_scope = if pane == INBOX_PANE_ID {
                            "inbox"
                        } else {
                            "obligation"
                        };
                        assert_eq!(
                            node.id,
                            SemanticUiId::new(format!("bokkie.{expected_scope}-row.{id}"))
                        );
                        (
                            node.id.0.clone(),
                            text.iter()
                                .map(|item| item.component_id)
                                .collect::<Vec<_>>(),
                        )
                    })
                    .collect::<BTreeMap<_, _>>();
                assert_eq!(identities.len(), 2);
                assert_ne!(identities.values().next(), identities.values().nth(1));
                if let Some(previous) = previous {
                    assert_eq!(identities, previous);
                }
                previous = Some(identities);
            }
        }
    }

    #[test]
    fn evidence_properties_keep_source_paths_when_fields_are_added_or_labels_repeat() {
        let before = common_evidence(&serde_json::json!({
            "fingerprint": "first", "proposal_fingerprint": "second",
            "details_json": "{\"fingerprint\":\"nested\"}"
        }));
        let after = common_evidence(&serde_json::json!({
            "id": "new-first-field", "fingerprint": "first", "proposal_fingerprint": "second",
            "details_json": "{\"fingerprint\":\"nested\"}"
        }));
        assert_eq!(before.len(), 3);
        assert!(before.iter().all(|field| after.contains(field)));
        let identities = before
            .iter()
            .map(|(path, _, _)| {
                PresentationScope::new("evidence-test")
                    .instance(("evidence-field", path))
                    .text_instance()
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            identities.len(),
            before.len(),
            "duplicate display labels are distinct fields"
        );
    }

    #[test]
    fn actual_evidence_reader_scrolls_to_long_tail_without_claiming_measurement() {
        let context = egui::Context::default();
        Appearance::default().apply(&context);
        context.all_styles_mut(|style| style.animation_time = 0.0);
        let tokens = UiPreferences::default()
            .tokens(false)
            .with_typography_profile(TypographyProfile::Reading);
        let item = TopicItem {
            occurred_at: 100,
            source: TopicSource::AuditEvent,
            source_sequence: "7".into(),
            stable_id: "long-reader-test".into(),
            occurrence: Some(1),
            event_type: "created".into(),
            evidence: serde_json::json!({"long_text": format!("{}TAIL_MARKER", "long evidence word ".repeat(1600))}),
        };
        let mut pointer = None;
        let mut disclosure_pointer = None;
        let mut saw_tail = false;
        let mut saw_start = false;
        for pass in 0..12 {
            let mut observed = None;
            let events = pointer.map_or_else(
                || {
                    disclosure_pointer.map_or_else(Vec::new, |pos| {
                        vec![
                            egui::Event::PointerMoved(pos),
                            egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Primary,
                                pressed: true,
                                modifiers: egui::Modifiers::NONE,
                            },
                            egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Primary,
                                pressed: false,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ]
                    })
                },
                |position| {
                    vec![
                        egui::Event::PointerMoved(position),
                        egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            phase: egui::TouchPhase::Move,
                            delta: egui::vec2(0.0, -100_000.0),
                            modifiers: egui::Modifiers::NONE,
                        },
                    ]
                },
            );
            context
                .run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(420.0, 800.0),
                        )),
                        events,
                        time: Some(f64::from(pass) * 0.1),
                        ..Default::default()
                    },
                    |ui| {
                        let mut presentation = PresentationContext::new(
                            ui,
                            tokens,
                            1.0,
                            PresentationScope::new("reader-test"),
                            SemanticUiId::pane(TIMELINE_PANE_ID),
                        );
                        show_topic_item(ui, &item, 100, &mut presentation);
                        observed = Some(presentation.finish(ui));
                    },
                )
                .textures_delta
                .clear();
            let observed = observed.unwrap();
            let Some(reader) = observed
                .semantic_nodes
                .iter()
                .find(|node| node.id.0 == "bokkie.evidence-reader.long-reader-test")
            else {
                let disclosure = observed
                    .semantic_nodes
                    .iter()
                    .find(|node| node.id.0 == "bokkie.raw-evidence.long-reader-test")
                    .unwrap();
                disclosure_pointer = Some(egui::pos2(
                    (disclosure.rect.min_x + disclosure.rect.max_x) * 0.5,
                    (disclosure.rect.min_y + disclosure.rect.max_y) * 0.5,
                ));
                continue;
            };
            assert!(reader.text_selectable);
            assert_eq!(
                observed.text_layouts.len(),
                2,
                "only heading and timing are measured"
            );
            assert_eq!(observed.coverage.observed_native_controls, 0);
            assert!(observed.coverage.native_text_controls >= 1);
            assert_eq!(observed.raw_presentations.len(), 1);
            if !saw_start {
                assert!(!reader.name.contains("TAIL_MARKER"));
                saw_start = true;
            }
            saw_tail |= reader.name.contains("TAIL_MARKER");
            pointer = Some(egui::pos2(
                (reader.rect.min_x + reader.rect.max_x) * 0.5,
                (reader.rect.min_y + reader.rect.max_y) * 0.5,
            ));
        }
        assert!(
            saw_tail,
            "scrolling the actual reader must expose its complete tail row"
        );
    }

    #[test]
    fn reader_tail_is_observed_only_when_its_painted_rows_are_visible() {
        let context = egui::Context::default();
        let mut checked = false;
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            let galley = ui.painter().layout(
                "first\nsecond\nTAIL_MARKER".to_owned(),
                egui::FontId::monospace(14.0),
                egui::Color32::WHITE,
                300.0,
            );
            let first_clip = galley.rows[0].rect();
            assert!(
                !visible_reader_text(&galley, egui::Pos2::ZERO, first_clip).contains("TAIL_MARKER")
            );
            let last_clip = galley.rows.last().unwrap().rect();
            assert!(
                visible_reader_text(&galley, egui::Pos2::ZERO, last_clip).contains("TAIL_MARKER")
            );
            checked = true;
        });
        output.textures_delta.clear();
        assert!(checked);
    }

    #[test]
    fn routine_decisions_do_not_use_failure_emphasis_or_absent_metadata() {
        assert_eq!(
            exception_text_role(Some(&ExceptionReason::AwaitingApproval {
                subject: ApprovalSubject::Generic
            })),
            TextRole::Body
        );
        let mut obligation = fixture(0);
        obligation.last_error = None;
        obligation.last_evidence = None;
        let label = obligation_row_text(&obligation, Some(100));
        assert!(!label.contains("No last"));
        assert!(!label.contains("Error:"));
        assert!(!label.contains("Evidence:"));
    }

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
            failure_disposition: None,
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

    fn session(session_id: &str) -> ApiSession {
        ApiSession::from_bootstrap(SessionBootstrap {
            service: ServiceIdentity {
                build: BOKKIE_BUILD_ID.to_owned(),
                api_contract_version: API_CONTRACT_VERSION,
                schema_version: SUPPORTED_SCHEMA_VERSION,
                process_id: 42,
                session_id: session_id.to_owned(),
            },
            mutation_token: "a".repeat(64),
        })
        .unwrap()
    }

    fn service_identity() -> ServiceIdentity {
        ServiceIdentity {
            build: BOKKIE_BUILD_ID.to_owned(),
            api_contract_version: API_CONTRACT_VERSION,
            schema_version: SUPPORTED_SCHEMA_VERSION,
            process_id: 42,
            session_id: "session-one".to_owned(),
        }
    }

    fn test_app() -> AttentionApp {
        let (sender, receiver) = mpsc::channel();
        AttentionApp {
            workspace: operator_workspace(),
            collection: INBOX_PANE_ID,
            model: AppModel::default(),
            transport: None,
            session: None,
            sender,
            receiver,
            preferences: UiPreferences::default(),
            theme: Appearance::default().theme(),
            next_poll_at: None,
            deadline_obligations: BTreeSet::new(),
            next_generation: 1,
            snapshot_assembly: None,
            topic_assembly: None,
            change_assembly: None,
            change_extra_affected: BTreeSet::new(),
            affected_refresh: None,
            projection_recovery: false,
            frame_number: 0,
            last_test_snapshot: TestSnapshot::default(),
            test_observer: None,
        }
    }

    #[test]
    fn cursor_gap_enters_same_session_full_recovery_with_retained_state_stale() {
        let mut app = test_app();
        app.model.apply_snapshot(OperatorSnapshot {
            captured_at: 100,
            service: None,
            next_cursor: None,
            watermark: 9,
            obligations: vec![fixture(1)],
        });
        app.recover_projection("event envelope cursor 9", &egui::Context::default());
        assert!(app.snapshot_assembly.is_some());
        assert!(app.projection_recovery);
        assert!(matches!(
            app.model.connection,
            ConnectionState::Stale { .. }
        ));
        assert_eq!(app.model.obligations().len(), 1);
    }

    #[test]
    fn session_rotation_clears_token_and_confirmation_then_bootstraps_a_rebuild() {
        let mut app = test_app();
        app.model.apply_snapshot(OperatorSnapshot {
            captured_at: 100,
            service: None,
            next_cursor: None,
            watermark: 9,
            obligations: vec![fixture(1)],
        });
        app.model
            .begin_confirmation(LifecycleAction::Cancel)
            .unwrap();
        app.session = Some(session("old-session"));

        let context = egui::Context::default();
        app.restart_session("service restarted", &context);
        assert!(app.session.is_none());
        assert!(app.model.confirmation.is_none());
        assert!(matches!(
            app.model.connection,
            ConnectionState::Stale { .. }
        ));

        app.sender
            .send(ApiMessage {
                request: ApiRequest::Bootstrap,
                result: Ok(ApiPayload::Bootstrap(session("new-session"))),
            })
            .unwrap();
        app.poll_transport(&context);
        assert!(app.session.is_some());
        assert!(app.snapshot_assembly.is_some());
    }

    #[test]
    fn exact_gardener_decisions_route_to_affected_only_refresh() {
        for (revision, event_type) in [(41, "proposal_approved"), (42, "proposal_rejected")] {
            let obligation_id = format!("gardener:implement:instance-{revision}");
            let instance_id = format!("instance-{revision}");
            let mut changes = ChangeAssembly::new(1, revision - 1);
            let exact_change = ProjectionChange {
                revision,
                provenance: ProjectionEventProvenance::LiveAppend,
                source: ProjectionEventSource::GardenerEvent { sequence: revision },
                event_type: event_type.to_owned(),
                occurred_at: 1_788_381_000,
                obligation_id: Some(obligation_id.clone()),
                occurrence: Some(1),
                repository: Some("robchristie/bokkie".to_owned()),
                inspection_id: None,
                proposal_fingerprint: Some("goal-fingerprint".to_owned()),
                proposal_instance_id: Some(instance_id.clone()),
                run_id: None,
            };
            assert_eq!(
                exact_change.obligation_id.as_deref(),
                Some(obligation_id.as_str())
            );
            assert_eq!(
                exact_change.proposal_instance_id.as_deref(),
                Some(instance_id.as_str())
            );
            let ChangeProgress::Complete {
                affected,
                ambiguous,
                watermark,
            } = changes
                .push(ProjectionChangePage {
                    service: service_identity(),
                    requested_after: revision - 1,
                    requested_through: None,
                    next_after: None,
                    watermark: revision,
                    changes: vec![exact_change],
                })
                .unwrap()
            else {
                panic!("single gardener decision page should complete")
            };
            assert_eq!(watermark, revision);
            assert!(!ambiguous);
            assert_eq!(
                projection_refresh_plan(affected, ambiguous, BTreeSet::new()),
                ProjectionRefreshPlan::Affected(BTreeSet::from([obligation_id]))
            );
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
        let (delay, affected) = poll_plan(&OperatorSnapshot {
            captured_at: 100,
            service: None,
            next_cursor: None,
            watermark: 7,
            obligations: vec![late, early],
        });
        assert_eq!(delay, Duration::from_secs(3));
        assert_eq!(affected, BTreeSet::from(["obligation-000001".to_owned()]));
        assert_eq!(
            poll_plan(&OperatorSnapshot {
                captured_at: 100,
                service: None,
                next_cursor: None,
                watermark: 7,
                obligations: vec![fixture(3)]
            })
            .0,
            Duration::from_secs(10),
        );
        assert_eq!(
            poll_plan(&OperatorSnapshot {
                captured_at: 100,
                service: None,
                next_cursor: None,
                watermark: 7,
                obligations: Vec::new()
            })
            .0,
            POLL_MAX,
        );
    }

    #[test]
    fn active_lease_expiry_is_a_projection_refresh_deadline() {
        let mut leased = fixture(4);
        leased.next_wake_at = None;
        leased.liveness = Some(DurableLiveness::ActiveLease {
            token: "lease-token".to_owned(),
            generation: 3,
            expires_at: 106,
        });
        let (delay, affected) = poll_plan(&OperatorSnapshot {
            captured_at: 100,
            service: None,
            next_cursor: None,
            watermark: 7,
            obligations: vec![leased],
        });
        assert_eq!(delay, Duration::from_secs(6));
        assert_eq!(affected, BTreeSet::from(["obligation-000004".to_owned()]));
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
        assert!(common.iter().any(|(_, label, _)| *label == "Attempt ID"));
        assert!(common.iter().any(|(_, label, _)| *label == "Codex task ID"));
        assert!(common.iter().any(|(_, label, _)| *label == "Pull request"));
        assert!(
            !common
                .iter()
                .any(|(_, label, _)| *label == "uncommon_raw_field")
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
