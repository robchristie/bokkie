use super::*;
#[path = "workspace_ui.rs"]
mod workspace_ui;
use bokkie_operator_api::{
    ConversationAction, ConversationConfirmRequest, ConversationSelectRequest, ConversationSummary,
    ConversationTurnRequest, ConversationView, ManagedCatalogueEntry, ManagedTaskDefinition,
    ManagedTaskDetail, ManagedTrigger,
};

#[derive(Default)]
pub(super) struct ConversationState {
    pub open: bool,
    pub(super) id: Option<String>,
    view: Option<ConversationView>,
    history: Vec<ConversationSummary>,
    catalogue: Vec<ManagedCatalogueEntry>,
    panel: Option<ConversationPanel>,
    notification_label: String,
    notification_revision: Option<i64>,
    notification_refresh_pending: bool,
    pending_task_link: Option<String>,
    query: String,
    applied_query: String,
    next_after: Option<String>,
    pub(super) text: String,
    consult_adviser: bool,
    pending: Option<ApiRequest>,
    in_flight: bool,
    reading: Option<(String, u64)>,
    catalogue_busy: bool,
    task_view: TaskView,
    error: Option<String>,
    select_after_load: Option<String>,
    poll_at: Option<Instant>,
    editor: Option<workspace_ui::Editor>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ConversationPanel {
    Tasks,
    History,
    Details,
    Notifications,
}

#[derive(Clone, Copy, Default, PartialEq)]
enum TaskView {
    Today,
    Upcoming,
    Input,
    #[default]
    All,
}
impl TaskView {
    fn key(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Upcoming => "upcoming",
            Self::Input => "input",
            Self::All => "all",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Upcoming => "Upcoming",
            Self::Input => "Needs your input",
            Self::All => "All tasks",
        }
    }
}

impl ConversationState {
    pub(super) fn home() -> Self {
        Self {
            open: true,
            id: Some(uuid::Uuid::new_v4().to_string()),
            ..Default::default()
        }
    }

    fn switch(&mut self, id: String, task: Option<String>) {
        self.id = Some(id);
        self.view = None;
        self.text.clear();
        self.pending = None;
        self.reading = None;
        self.error = None;
        self.select_after_load = task;
        self.poll_at = None;
        self.panel = None;
        self.editor = None;
    }

    fn turn_request(&self, text: String) -> Option<ConversationTurnRequest> {
        let view = self.view.as_ref()?;
        Some(ConversationTurnRequest {
            command_id: engineering_command_id(),
            conversation_id: view.id.clone(),
            expected_revision: view.revision,
            text,
            consult_adviser: self.consult_adviser && view.adviser_available,
        })
    }

    fn begin_read(&mut self, generation: u64) -> Option<ApiRequest> {
        if self.in_flight {
            return None;
        }
        let id = self.id.clone()?;
        if self.reading.is_some() {
            return None;
        }
        self.reading = Some((id.clone(), generation));
        Some(ApiRequest::Conversation { id, generation })
    }

    fn observe_notification_revision(&mut self, revision: Option<i64>) {
        let Some(revision) = revision else {
            return;
        };
        if self.notification_revision == Some(revision) {
            return;
        }
        let previously_observed = self.notification_revision.replace(revision).is_some();
        // A first read already contains configuration observed before it starts.
        // A changed configuration, or one observed during/after a read, needs one
        // fresh projection. This is a read only: no turn, mutation or model run.
        self.notification_refresh_pending |=
            previously_observed || self.view.is_some() || self.reading.is_some();
    }

    fn begin_notification_refresh(&mut self, generation: u64) -> Option<ApiRequest> {
        if !self.notification_refresh_pending || !self.open || self.pending.is_some() {
            return None;
        }
        let request = self.begin_read(generation)?;
        self.notification_refresh_pending = false;
        Some(request)
    }

    fn finish_read(&mut self, id: &str, generation: u64) -> bool {
        if self.id.as_deref() != Some(id)
            || self
                .reading
                .as_ref()
                .is_none_or(|(owner, expected)| owner != id || *expected != generation)
        {
            return false;
        }
        self.reading = None;
        true
    }

    pub fn reset_session(&mut self) {
        if let Some(view) = &mut self.view {
            view.review = None;
        }
        if matches!(self.pending, Some(ApiRequest::ConversationConfirm(_))) {
            self.pending = None;
        }
        self.in_flight = false;
        self.reading = None;
        self.catalogue_busy = false;
        self.poll_at = None;
    }

    fn accept(&mut self, view: ConversationView, session: &ApiSession) {
        if self.id.as_deref() != Some(&view.id) || !session.matches(&view.service) {
            return;
        }
        if self
            .view
            .as_ref()
            .is_some_and(|old| old.revision > view.revision)
        {
            return;
        }
        // A retained turn can be reconciled after an uncertain network response.
        if self.pending.as_ref().is_some_and(|pending| match pending {
            ApiRequest::ConversationTurn(turn) => view
                .messages
                .iter()
                .any(|m| m.request_id == turn.command_id),
            ApiRequest::ConversationConfirm(confirm) => view
                .receipt
                .as_ref()
                .is_some_and(|r| r.command_id == confirm.command_id),
            _ => false,
        }) {
            if matches!(self.pending, Some(ApiRequest::ConversationTurn(_))) {
                self.text.clear();
            }
            self.pending = None;
        }
        let workspace_active = view.task.as_ref().is_some_and(|t| {
            let workspace_definition = t
                .active
                .as_ref()
                .is_some_and(|r| r.definition.workspace.is_some());
            t.runs.iter().any(|r| match &r.workspace {
                Some(workspace) => !workspace.cessation_verified || workspace.status == "attention",
                None => {
                    workspace_definition && !matches!(r.state.as_str(), "completed" | "cancelled")
                }
            })
        });
        self.poll_at =
            (view.busy || workspace_active).then(|| Instant::now() + Duration::from_secs(2));
        self.view = Some(view);
    }
}

pub(super) fn is_request(request: &ApiRequest) -> bool {
    matches!(
        request,
        ApiRequest::Conversations
            | ApiRequest::Conversation { .. }
            | ApiRequest::ConversationTurn(_)
            | ApiRequest::ConversationSelect(_)
            | ApiRequest::EditTask { .. }
            | ApiRequest::WorkspaceAction(_)
            | ApiRequest::ConversationConfirm(_)
            | ApiRequest::Catalogue { .. }
    )
}

impl AttentionApp {
    pub(super) fn open_conversation(&mut self, task: Option<String>, context: &egui::Context) {
        if task.is_some() && (self.conversation.pending.is_some() || self.conversation.in_flight) {
            return;
        }
        self.agent_settings.open = false;
        self.handoff.open = false;
        self.conversation.open = true;
        if task.is_some() {
            self.conversation
                .switch(uuid::Uuid::new_v4().to_string(), task);
        } else if self.conversation.id.is_none() {
            self.conversation.id = Some(uuid::Uuid::new_v4().to_string());
        }
        self.refresh_conversation(context);
    }

    pub(super) fn open_conversation_tasks(&mut self, context: &egui::Context) {
        self.open_conversation(None, context);
        self.conversation.panel = Some(ConversationPanel::Tasks);
    }

    pub(super) fn open_conversation_history(&mut self, context: &egui::Context) {
        self.open_conversation(None, context);
        self.conversation.panel = Some(ConversationPanel::History);
    }

    fn open_saved_conversation(&mut self, id: String, context: &egui::Context) {
        self.conversation.switch(id, None);
        self.refresh_conversation(context);
    }

    pub(super) fn refresh_conversation(&mut self, context: &egui::Context) {
        if !self.conversation.open || self.session.is_none() {
            return;
        }
        self.dispatch(ApiRequest::Conversations, context);
        if !self.conversation.catalogue_busy {
            self.conversation.catalogue_busy = true;
            self.conversation.applied_query = self.conversation.query.clone();
            self.dispatch(
                ApiRequest::Catalogue {
                    query: self.conversation.query.clone(),
                    after: None,
                    view: self.conversation.task_view.key().into(),
                },
                context,
            );
        }
        let generation = self.fresh_generation();
        if let Some(request) = self.conversation.begin_read(generation) {
            self.dispatch(request, context);
        }
    }

    pub(super) fn conversation_response(
        &mut self,
        request: ApiRequest,
        result: Result<ApiPayload, ApiFailure>,
        context: &egui::Context,
    ) {
        let mutation = matches!(
            request,
            ApiRequest::ConversationTurn(_)
                | ApiRequest::ConversationSelect(_)
                | ApiRequest::EditTask { .. }
                | ApiRequest::WorkspaceAction(_)
                | ApiRequest::ConversationConfirm(_)
        );
        let request_id = match &request {
            ApiRequest::Conversation { id, .. } => Some(id.as_str()),
            ApiRequest::ConversationTurn(r) => Some(r.conversation_id.as_str()),
            ApiRequest::ConversationSelect(r) => Some(r.conversation_id.as_str()),
            ApiRequest::EditTask { request, .. } => Some(request.conversation_id.as_str()),
            ApiRequest::WorkspaceAction(r) => Some(r.conversation_id.as_str()),
            ApiRequest::ConversationConfirm(r) => Some(r.conversation_id.as_str()),
            _ => None,
        };
        if request_id.is_some() && request_id != self.conversation.id.as_deref() {
            return;
        }
        if mutation {
            self.conversation.in_flight = false;
        }
        if let ApiRequest::Conversation { id, generation } = &request
            && !self.conversation.finish_read(id, *generation)
        {
            return;
        }
        if matches!(request, ApiRequest::Catalogue { .. }) {
            self.conversation.catalogue_busy = false;
        }
        match result {
            Ok(ApiPayload::Conversation(view)) => {
                let Some(session) = self.session.as_ref() else {
                    return;
                };
                if !session.matches(&view.service) {
                    return;
                }
                self.conversation.error = None;
                if mutation {
                    if matches!(request, ApiRequest::ConversationTurn(_)) {
                        self.conversation.text.clear();
                    }
                    self.conversation.pending = None;
                }
                if let Some(draft) = &view.handoff_draft {
                    self.handoff.observe_draft(draft.clone());
                }
                self.conversation.accept(*view, session);
                if let Some(task) = self.conversation.select_after_load.take() {
                    self.select_conversation_task(task, context);
                }
            }
            Ok(ApiPayload::Conversations(list)) => {
                if self
                    .session
                    .as_ref()
                    .is_some_and(|s| s.matches(&list.service))
                {
                    self.conversation.history = list.items;
                }
            }
            Ok(ApiPayload::Catalogue(page)) => {
                if let ApiRequest::Catalogue { query, after, view } = request
                    && query == self.conversation.applied_query
                    && view == self.conversation.task_view.key()
                {
                    if after.is_none() {
                        self.conversation.catalogue.clear();
                    }
                    for entry in page.items {
                        if !self
                            .conversation
                            .catalogue
                            .iter()
                            .any(|old| old.id == entry.id)
                        {
                            self.conversation.catalogue.push(entry);
                        }
                    }
                    self.conversation.next_after = page.next_after;
                }
            }
            Err(error) => {
                self.conversation.error = Some(error.to_string());
                if matches!(error, ApiFailure::SessionChanged(_)) {
                    self.restart_session(&error.to_string(), context);
                } else {
                    if matches!(error, ApiFailure::Conflict(_) | ApiFailure::Rejected(_)) {
                        self.conversation.pending = None;
                        if let Some(view) = &mut self.conversation.view {
                            view.review = None;
                        }
                    }
                    self.conversation.poll_at = Some(Instant::now() + RECONNECT_DELAY);
                }
            }
            _ => {}
        }
    }

    fn select_conversation_task(&mut self, task_id: String, context: &egui::Context) {
        let Some(view) = &self.conversation.view else {
            return;
        };
        let request = ApiRequest::ConversationSelect(ConversationSelectRequest {
            command_id: engineering_command_id(),
            conversation_id: view.id.clone(),
            expected_revision: view.revision,
            task_id,
        });
        self.send_conversation_mutation(request, context);
    }

    fn send_conversation_mutation(&mut self, request: ApiRequest, context: &egui::Context) {
        self.conversation.pending = Some(request.clone());
        self.conversation.in_flight = true;
        self.conversation.error = None;
        self.dispatch(request, context);
    }

    pub(super) fn drive_conversation_poll(&mut self, context: &egui::Context) {
        if self.session.is_none() {
            return;
        }
        self.conversation
            .observe_notification_revision(notifications_ui::configuration_revision());
        if self.conversation.pending_task_link.is_none() {
            self.conversation.pending_task_link = notifications_ui::take_task_link();
            if self.conversation.pending_task_link.is_some() {
                self.conversation.open = true;
            }
        }
        if !self.conversation.in_flight
            && self.conversation.pending.is_none()
            && let Some(task) = self.conversation.pending_task_link.take()
        {
            self.conversation.open = true;
            if self.conversation.view.is_some() {
                self.select_conversation_task(task, context);
            } else {
                self.conversation.select_after_load = Some(task);
                self.refresh_conversation(context);
            }
        }
        if !self.conversation.open {
            return;
        }
        if self.conversation.notification_refresh_pending
            && !self.conversation.in_flight
            && self.conversation.reading.is_none()
            && self.conversation.pending.is_none()
        {
            let generation = self.fresh_generation();
            if let Some(request) = self.conversation.begin_notification_refresh(generation) {
                self.dispatch(request, context);
            }
        }
        if let Some(at) = self.conversation.poll_at {
            if at <= Instant::now()
                && self.conversation.reading.is_none()
                && !self.conversation.in_flight
            {
                self.conversation.poll_at = None;
                let generation = self.fresh_generation();
                if let Some(request) = self.conversation.begin_read(generation) {
                    self.dispatch(request, context);
                }
            } else {
                context.request_repaint_after(
                    at.saturating_duration_since(Instant::now())
                        .max(Duration::from_millis(100)),
                );
            }
        }
    }

    pub(super) fn show_conversation(
        &mut self,
        ui: &mut egui::Ui,
        nodes: &mut Vec<UiNode>,
        intents: &mut Vec<OperatorIntent>,
    ) {
        let context = ui.ctx().clone();
        let mut action = None;
        let safe = self.session.is_some()
            && self.model.connection.decisions_safe()
            && !self.conversation.in_flight
            && self.conversation.reading.is_none();
        let busy = self.conversation.view.as_ref().is_some_and(|v| v.busy);
        let mutable = safe && !busy && self.conversation.pending.is_none();
        let state = &mut self.conversation;
        observe(
            ui.max_rect(),
            "bokkie.conversation",
            "Conversation workspace",
            UiRole::Section,
            true,
            nodes,
        );
        ui.horizontal_wrapped(|ui| {
            ui.heading("Conversation");
            if button(
                ui,
                "bokkie.conversation.new",
                "New conversation",
                !state.in_flight && state.pending.is_none(),
                nodes,
            ) {
                action = Some(ConversationUiAction::New);
            }
            if button(
                ui,
                "bokkie.conversation.history",
                "Recent chats",
                true,
                nodes,
            ) {
                state.panel = if state.panel == Some(ConversationPanel::History) {
                    None
                } else {
                    Some(ConversationPanel::History)
                };
            }
            if button(
                ui,
                "bokkie.home.notifications",
                "Notifications",
                true,
                nodes,
            ) {
                if state.notification_label.is_empty() {
                    state.notification_label = "This device".into();
                }
                state.panel = if state.panel == Some(ConversationPanel::Notifications) {
                    None
                } else {
                    notifications_ui::refresh();
                    Some(ConversationPanel::Notifications)
                };
            }
            if ui.max_rect().width() < 1000.0
                && state
                    .view
                    .as_ref()
                    .is_some_and(|view| view.selected_task_id.is_some())
                && button(
                    ui,
                    "bokkie.conversation.details",
                    "Task details",
                    true,
                    nodes,
                )
            {
                state.panel = if state.panel == Some(ConversationPanel::Details) {
                    None
                } else {
                    Some(ConversationPanel::Details)
                };
            }
        });
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            for view in [TaskView::Today, TaskView::Upcoming, TaskView::Input] {
                if button(
                    ui,
                    &format!("bokkie.schedule.{}", view.key()),
                    view.label(),
                    !state.catalogue_busy,
                    nodes,
                ) {
                    action = Some(ConversationUiAction::TaskView(view));
                }
            }
        });
        ui.add_space(6.0);
        let bounds = ui.available_rect_before_wrap();
        let wide = bounds.width() >= 1000.0;
        let panel = state.panel.or_else(|| {
            (wide
                && state
                    .view
                    .as_ref()
                    .is_some_and(|view| view.selected_task_id.is_some()))
            .then_some(ConversationPanel::Details)
        });
        let sidebar = wide && panel.is_some();
        let main_width = if sidebar {
            bounds.width() - 344.0
        } else {
            bounds.width()
        };
        let main = egui::Rect::from_min_size(bounds.min, egui::vec2(main_width, bounds.height()));
        // The composer and transcript have independent, bounded rectangles. Long
        // messages, reviews and task history cannot move the input below the screen.
        let footer_height = if state.pending.is_some() || state.error.is_some() {
            174.0
        } else {
            138.0
        } + if state
            .view
            .as_ref()
            .is_some_and(|view| view.adviser_available)
        {
            28.0
        } else {
            0.0
        };
        let composer = egui::Rect::from_min_max(
            egui::pos2(main.left(), (main.bottom() - footer_height).max(main.top())),
            main.max,
        );
        let transcript = egui::Rect::from_min_max(
            main.min,
            egui::pos2(main.right(), (composer.top() - 8.0).max(main.top())),
        );
        let reading_width = main.width().min(760.0);
        let reading_inset = (main.width() - reading_width) / 2.0;
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(composer.shrink2(egui::vec2(reading_inset, 0.0))),
            |ui| {
                ui.set_clip_rect(composer.intersect(ui.clip_rect()));
                conversation_composer(ui, state, safe, busy, mutable, nodes, &mut action);
            },
        );
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(if wide || panel.is_none() {
                transcript.shrink2(egui::vec2(reading_inset, 0.0))
            } else {
                transcript
            }),
            |ui| {
                ui.set_clip_rect(transcript.intersect(ui.clip_rect()));
                if !wide && let Some(panel) = panel {
                    conversation_panel(ui, state, panel, true, mutable, nodes, &mut action);
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt(("conversation-transcript", &state.id))
                        .stick_to_bottom(true)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_max_width(reading_width);
                            ui.add_space(8.0);
                            conversation_transcript(
                                ui,
                                state,
                                self.session.as_ref(),
                                mutable,
                                nodes,
                                &mut action,
                            );
                        });
                }
            },
        );
        if sidebar && let Some(panel) = panel {
            let rect =
                egui::Rect::from_min_max(egui::pos2(main.right() + 20.0, bounds.top()), bounds.max);
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                ui.set_clip_rect(rect.intersect(ui.clip_rect()));
                conversation_panel(
                    ui,
                    state,
                    panel,
                    panel != ConversationPanel::Details,
                    mutable,
                    nodes,
                    &mut action,
                );
            });
        }
        match action {
            Some(ConversationUiAction::EditTask) => {
                if let Some(view) = &self.conversation.view {
                    self.conversation.editor = view
                        .task
                        .as_ref()
                        .and_then(|t| workspace_ui::Editor::task(view.id.clone(), t));
                }
            }
            Some(ConversationUiAction::Run { run, cancel }) => {
                if let Some(view) = &self.conversation.view {
                    self.conversation.editor = Some(workspace_ui::Editor::Run {
                        conversation_id: view.id.clone(),
                        run,
                        answer: String::new(),
                        cancel,
                    });
                }
            }
            Some(ConversationUiAction::Handoff(draft)) => {
                self.open_handoff_draft(*draft, &context);
            }
            Some(ConversationUiAction::New) => {
                self.conversation = ConversationState {
                    open: true,
                    ..Default::default()
                };
                self.open_conversation(None, &context);
            }
            Some(ConversationUiAction::Open(id)) => {
                self.open_saved_conversation(id, &context);
            }
            Some(
                ConversationUiAction::Search
                | ConversationUiAction::More
                | ConversationUiAction::TaskView(_),
            ) => {
                if let Some(ConversationUiAction::TaskView(view)) = action {
                    self.conversation.task_view = view;
                    self.conversation.panel = Some(ConversationPanel::Tasks);
                }
                let after = if matches!(action, Some(ConversationUiAction::More)) {
                    self.conversation.next_after.clone()
                } else {
                    None
                };
                self.conversation.applied_query = self.conversation.query.clone();
                self.conversation.catalogue_busy = true;
                self.dispatch(
                    ApiRequest::Catalogue {
                        query: self.conversation.query.clone(),
                        after,
                        view: self.conversation.task_view.key().into(),
                    },
                    &context,
                );
            }
            Some(ConversationUiAction::Select(id)) => {
                self.conversation.panel = None;
                self.select_conversation_task(id, &context);
            }
            Some(ConversationUiAction::Legacy(id)) => {
                self.conversation.open = false;
                intents.push(OperatorIntent::Select {
                    obligation_id: id,
                    destination: Some(TIMELINE_PANE_ID),
                });
            }
            Some(ConversationUiAction::Retry) => {
                if let Some(request) = self.conversation.pending.clone() {
                    self.send_conversation_mutation(request, &context);
                }
            }
            Some(ConversationUiAction::Send | ConversationUiAction::Preview) => {
                self.conversation.panel = None;
                let text = if matches!(action, Some(ConversationUiAction::Preview)) {
                    "Preview this task".into()
                } else {
                    self.conversation.text.clone()
                };
                if let Some(request) = self.conversation.turn_request(text) {
                    let request = ApiRequest::ConversationTurn(request);
                    self.send_conversation_mutation(request, &context);
                }
            }
            Some(ConversationUiAction::Confirm) => {
                if let Some(view) = &self.conversation.view
                    && review_is_current(view, self.session.as_ref())
                    && let Some(review) = &view.review
                {
                    let request = ApiRequest::ConversationConfirm(ConversationConfirmRequest {
                        command_id: engineering_command_id(),
                        conversation_id: view.id.clone(),
                        proposal_id: review.id.clone(),
                        session_id: review.session_id.clone(),
                    });
                    self.send_conversation_mutation(request, &context);
                }
            }
            Some(ConversationUiAction::Example(text)) => self.conversation.text = text.into(),
            None => {}
        }
        if let Some(editor) = &mut self.conversation.editor {
            let (open, request) = editor.draw(&context, mutable, nodes);
            if !open {
                self.conversation.editor = None;
            }
            if let Some(request) = request {
                self.send_conversation_mutation(request, &context);
            }
        }
    }
}

enum ConversationUiAction {
    EditTask,
    Run {
        run: Box<bokkie_operator_api::WorkspaceRun>,
        cancel: bool,
    },
    Handoff(Box<bokkie_operator_api::HandoffDraft>),
    New,
    Open(String),
    Search,
    More,
    TaskView(TaskView),
    Select(String),
    Legacy(String),
    Send,
    Preview,
    Retry,
    Confirm,
    Example(&'static str),
}
fn conversation_composer(
    ui: &mut egui::Ui,
    state: &mut ConversationState,
    safe: bool,
    busy: bool,
    mutable: bool,
    nodes: &mut Vec<UiNode>,
    action: &mut Option<ConversationUiAction>,
) {
    ui.separator();
    if let Some(error) = &state.error {
        ui.add(egui::Label::new(format!("Request needs attention: {error}")).truncate())
            .on_hover_text(error);
    }
    egui::Frame::group(ui.style())
        .inner_margin(10.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("conversation-composer-scroll")
                .max_height(52.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let response = ui.add_enabled(
                        state.pending.is_none() && !state.in_flight,
                        egui::TextEdit::multiline(&mut state.text)
                            .id_salt("conversation-text")
                            .desired_rows(2)
                            .desired_width(f32::INFINITY)
                            .hint_text("What would you like to organise?"),
                    );
                    observe(
                        response.rect.intersect(ui.clip_rect()),
                        "bokkie.conversation.text",
                        "Message to Bokkie",
                        UiRole::Section,
                        response.enabled(),
                        nodes,
                    );
                });
            let available = state
                .view
                .as_ref()
                .is_some_and(|view| view.runtime_available);
            if state
                .view
                .as_ref()
                .is_some_and(|view| view.adviser_available)
            {
                let response = ui.add_enabled(
                    state.pending.is_none() && !state.in_flight,
                    egui::Checkbox::new(&mut state.consult_adviser, "Consult Astra"),
                );
                observe(
                    response.rect,
                    "bokkie.conversation.consult-adviser",
                    "Consult Astra",
                    UiRole::Section,
                    response.enabled(),
                    nodes,
                );
            }
            ui.horizontal_wrapped(|ui| {
                if state.pending.is_some() {
                    if button(
                        ui,
                        "bokkie.conversation.retry",
                        "Retry saved request",
                        safe && !busy,
                        nodes,
                    ) {
                        *action = Some(ConversationUiAction::Retry);
                    }
                    ui.small("Retry the saved request.");
                } else {
                    if button(
                        ui,
                        "bokkie.conversation.send",
                        "Send message",
                        mutable
                            && available
                            && !state.text.trim().is_empty()
                            && state.text.chars().count() <= 8_192,
                        nodes,
                    ) {
                        *action = Some(ConversationUiAction::Send);
                    }
                    if busy {
                        ui.small(
                            state
                                .view
                                .as_ref()
                                .and_then(|view| view.activity.as_deref())
                                .unwrap_or("Bokkie is preparing a response…"),
                        );
                    }
                }
            });
        });
}

fn conversation_panel(
    ui: &mut egui::Ui,
    state: &mut ConversationState,
    panel: ConversationPanel,
    show_back: bool,
    mutable: bool,
    nodes: &mut Vec<UiNode>,
    action: &mut Option<ConversationUiAction>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.strong(match panel {
            ConversationPanel::Tasks => "Tasks and drafts",
            ConversationPanel::History => "Recent conversations",
            ConversationPanel::Details => "Task details",
            ConversationPanel::Notifications => "Notifications",
        });
        if show_back
            && button(
                ui,
                "bokkie.conversation.panel-back",
                "Back to chat",
                true,
                nodes,
            )
        {
            state.panel = None;
        }
    });
    ui.separator();
    egui::ScrollArea::vertical()
        .id_salt("conversation-context")
        .auto_shrink([false, false])
        .show(ui, |ui| match panel {
            ConversationPanel::Notifications => {
                notifications_ui::show(ui, &mut state.notification_label, mutable, nodes);
            }
            ConversationPanel::History => {
                for summary in &state.history {
                    let label = if summary.last_text.is_empty() {
                        "Untitled conversation"
                    } else {
                        &summary.last_text
                    };
                    if button(
                        ui,
                        &format!("bokkie.conversation.history.{}", summary.id),
                        &label.chars().take(90).collect::<String>(),
                        !state.in_flight && state.pending.is_none(),
                        nodes,
                    ) {
                        *action = Some(ConversationUiAction::Open(summary.id.clone()));
                    }
                    ui.add_space(8.0);
                }
                if state.history.is_empty() {
                    ui.label("Your saved conversations will appear here.");
                }
            }
            ConversationPanel::Tasks => {
                ui.horizontal_wrapped(|ui| {
                    for view in [TaskView::Today, TaskView::Upcoming, TaskView::Input, TaskView::All] {
                        let response = ui.selectable_label(state.task_view == view, view.label());
                        observe(response.rect, &format!("bokkie.tasks.view.{}", view.key()), view.label(), UiRole::Button, !state.catalogue_busy, nodes);
                        if response.clicked() && !state.catalogue_busy {
                            *action = Some(ConversationUiAction::TaskView(view));
                        }
                    }
                });
                ui.label(format!("{} · your task catalogue", state.task_view.label()));
                let response = ui.add(
                    egui::TextEdit::singleline(&mut state.query)
                        .desired_width(f32::INFINITY)
                        .hint_text("Search task names and descriptions"),
                );
                observe(
                    response.rect,
                    "bokkie.conversation.search",
                    "Search all tasks",
                    UiRole::Section,
                    true,
                    nodes,
                );
                if button(
                    ui,
                    "bokkie.conversation.search-submit",
                    "Search tasks",
                    !state.catalogue_busy,
                    nodes,
                ) {
                    *action = Some(ConversationUiAction::Search);
                }
                if state.catalogue_busy {
                    ui.label("Searching…");
                }
                for entry in &state.catalogue {
                    ui.add_space(8.0);
                    catalogue_row(ui, entry, "catalogue", mutable, nodes, action);
                    ui.separator();
                }
                if state.catalogue.is_empty() && !state.catalogue_busy {
                    ui.label(match state.task_view {
                        TaskView::Today => "Nothing due today. Completed occurrences from today appear here too.",
                        TaskView::Upcoming => "No later occurrences scheduled. Draft or resume a task in conversation.",
                        TaskView::Input => "No matching tasks need your input. Delivery problems also appear in Needs attention.",
                        TaskView::All => "No matching tasks. Start a conversation to draft one.",
                    });
                }
                if state.next_after.is_some()
                    && button(
                        ui,
                        "bokkie.conversation.more",
                        "More tasks",
                        !state.catalogue_busy,
                        nodes,
                    )
                {
                    *action = Some(ConversationUiAction::More);
                }
            }
            ConversationPanel::Details => {
                if let Some(view) = &state.view {
                    if let Some(task) = &view.task {
                        task_detail(ui, task, nodes);
                        if button(ui,"bokkie.task.edit","Edit task",mutable,nodes) {
                            *action = Some(ConversationUiAction::EditTask);
                        }
                        for run in task.runs.iter().filter_map(|r|r.workspace.as_ref()) {
                            if let Some(question) = &run.question {
                                ui.add(egui::Label::new(&question.prompt).wrap().selectable(true));
                                if question.kind == "new_authority" {
                                    ui.small("This action needs a separately reviewed permission decision. An ordinary answer cannot expand the task's scope.");
                                } else if button(ui,&format!("bokkie.workspace.answer.{}",run.execution_id),"Answer question",mutable,nodes) {
                                    *action = Some(ConversationUiAction::Run {run:Box::new(run.clone()),cancel:false});
                                }
                            }
                            if (!run.cessation_verified || run.status == "attention") && !run.cancellation_requested
                                && button(ui,&format!("bokkie.workspace.stop.{}",run.execution_id),if run.cessation_verified {"Cancel this run"} else {"Stop active run"},mutable,nodes) {
                                *action = Some(ConversationUiAction::Run {run:Box::new(run.clone()),cancel:true});
                            }
                        }
                        if button(
                            ui,
                            "bokkie.conversation.preview",
                            "Preview this task",
                            mutable && view.runtime_available,
                            nodes,
                        ) {
                            *action = Some(ConversationUiAction::Preview);
                        }
                    } else if let Some(task_id) = &view.selected_task_id
                        && button(
                            ui,
                            "bokkie.conversation.legacy-open",
                            "Open existing task details and actions",
                            true,
                            nodes,
                        )
                    {
                        *action = Some(ConversationUiAction::Legacy(task_id.clone()));
                    }
                }
            }
        });
}

fn conversation_transcript(
    ui: &mut egui::Ui,
    state: &ConversationState,
    session: Option<&ApiSession>,
    mutable: bool,
    nodes: &mut Vec<UiNode>,
    action: &mut Option<ConversationUiAction>,
) {
    let Some(view) = &state.view else {
        ui.heading("What would you like to organise?");
        ui.label(if session.is_some() {
            "Loading your conversation…"
        } else {
            "Connecting to Bokkie. Your draft stays here while the service reconnects."
        });
        return;
    };
    if !view.runtime_available {
        ui.label("Conversation runtime unavailable. Existing tasks and saved conversations remain readable.");
    }
    if view.messages.is_empty() {
        ui.add_space(32.0);
        ui.heading("What would you like to organise?");
        ui.label("Draft a reminder, find a task, or refine its instructions and timing.");
        if view.workspace_available {
            ui.label("Describe work for a registered project workspace. Bokkie keeps its scope, progress, questions and results together in one task.");
        }
        ui.add_space(16.0);
        if view.reminders_available {
            if button(
                ui,
                "bokkie.conversation.example.reminder",
                "Remind me every weekday",
                state.pending.is_none() && !state.in_flight,
                nodes,
            ) {
                *action = Some(ConversationUiAction::Example(
                    "Every weekday at 9 am, remind me to review today’s priorities.",
                ));
            }
        } else {
            ui.small("Notifications are not configured. Reminder drafts cannot be activated yet.");
        }
        if view.notes_available {
            for (id, label, text) in [
                (
                    "note",
                    "Keep a local note",
                    "Create a local note called Weekly priorities with the text: Review my priorities for the week.",
                ),
                (
                    "schedule",
                    "Plan a recurring note",
                    "Draft a local note for every Monday at 9 am Australia/Adelaide with the text: Review my priorities for the week.",
                ),
            ] {
                if button(
                    ui,
                    &format!("bokkie.conversation.example.{id}"),
                    label,
                    state.pending.is_none() && !state.in_flight,
                    nodes,
                ) {
                    *action = Some(ConversationUiAction::Example(text));
                }
            }
            ui.small("Examples fill the message box for you to edit and send.");
        }
        ui.add_space(24.0);
    }
    if let Some(task_id) = &view.selected_task_id {
        let name = view
            .task
            .as_ref()
            .and_then(|task| task.candidate.as_ref().or(task.active.as_ref()))
            .map(|revision| revision.definition.name.as_str())
            .unwrap_or(task_id);
        let label = format!("Selected task: {name}");
        let response = ui.add(egui::Label::new(egui::RichText::new(&label).strong()).wrap());
        observe(
            response.rect,
            "bokkie.conversation.selected",
            &label,
            UiRole::Section,
            true,
            nodes,
        );
        ui.add_space(12.0);
    }
    for (index, message) in view.messages.iter().enumerate() {
        if message.role == "system" {
            egui::CollapsingHeader::new("Task activity details")
                .id_salt(("conversation-activity", &view.id, index))
                .show(ui, |ui| {
                    let response = ui.add(egui::Label::new(&message.text).wrap().selectable(true));
                    observe(
                        response.rect,
                        &format!("bokkie.conversation.message.{index}"),
                        &message.text,
                        UiRole::Section,
                        true,
                        nodes,
                    );
                });
            continue;
        }
        egui::Frame::new()
            .fill(if message.role == "user" {
                ui.visuals().faint_bg_color
            } else {
                egui::Color32::TRANSPARENT
            })
            .corner_radius(10.0)
            .inner_margin(12.0)
            .show(ui, |ui| {
                ui.strong(match message.role.as_str() {
                    "user" => "You",
                    "system" => "Task activity",
                    _ => "Bokkie",
                });
                let response = ui.add(egui::Label::new(&message.text).wrap().selectable(true));
                observe(
                    response.rect,
                    &format!("bokkie.conversation.message.{index}"),
                    &message.text,
                    UiRole::Section,
                    true,
                    nodes,
                );
            });
        ui.add_space(12.0);
    }
    if let Some(draft) = &view.handoff_draft {
        ui.group(|ui| {
            ui.strong(if draft.saved_revision > 0 {
                "Saved project hand-off"
            } else {
                "Project hand-off draft"
            });
            ui.label(&draft.brief.outcome);
            if draft.saved_revision > 0 {
                ui.label(format!("Saved revision {}", draft.saved_revision));
            } else {
                ui.label(format!(
                    "Workspace query: {} · {} matching destinations",
                    draft.project_query,
                    draft.candidates.len()
                ));
            }
            if button(
                ui,
                "bokkie.conversation.handoff",
                if draft.saved_revision > 0 {
                    "Open saved hand-off"
                } else {
                    "Review project hand-off"
                },
                true,
                nodes,
            ) {
                *action = Some(ConversationUiAction::Handoff(Box::new(draft.clone())));
            }
        });
        ui.add_space(8.0);
    }
    if !view.candidates.is_empty() {
        ui.strong("Choose the task you mean");
    }
    for candidate in &view.candidates {
        catalogue_row(ui, candidate, "candidate", mutable, nodes, action);
    }
    if view.review.is_some() {
        // Staleness is not proof of confirmation: a different operator or a
        // session change can invalidate a review without accepting its action.
        if review_context_is_current(view, session) {
            review_card(ui, view, session, mutable, nodes, action);
        } else {
            egui::CollapsingHeader::new("Previous review — no longer actionable")
                .id_salt("previous-conversation-review")
                .show(ui, |ui| {
                    review_card(ui, view, session, mutable, nodes, action)
                });
        }
    }
    if let Some(receipt) = &view.receipt {
        ui.add_space(10.0);
        let draft = view.task.as_ref().is_some_and(|task| {
            task.id == receipt.task_id
                && task.configuration_revision == receipt.configuration_revision
                && task.status == bokkie_operator_api::ManagedTaskStatus::Draft
        });
        ui.strong(if draft {
            "Draft saved"
        } else {
            "Configuration saved"
        });
        ui.small(if draft {
            "This task is not active yet."
        } else {
            "Run status and results appear separately."
        });
        egui::CollapsingHeader::new("Saved receipt details")
            .id_salt(("receipt-provenance", &receipt.command_id))
            .show(ui, |ui| {
                ui.label(format!("Task ID: {}", receipt.task_id));
                ui.label(format!(
                    "Configuration revision: {}",
                    receipt.configuration_revision
                ));
                ui.label(format!("Command ID: {}", receipt.command_id));
            });
    }
    if let Some(task) = &view.task
        && !view.reminders_available
        && task
            .active
            .as_ref()
            .or(task.candidate.as_ref())
            .is_some_and(|revision| revision.definition.capability == "reminder")
    {
        ui.add(egui::Label::new("Reminder notifications are unavailable in this runtime. The saved reminder and history are retained; configure notifications before relying on its schedule.").wrap());
    }
    if let Some(task) = &view.task
        && let Some(run) = task.runs.iter().find(|run| run.result.is_some())
    {
        ui.add_space(16.0);
        egui::Frame::group(ui.style())
            .inner_margin(12.0)
            .show(ui, |ui| {
                ui.strong("Latest task result");
                ui.small(format!(
                    "Occurrence completed · {}",
                    local_time_in_zone(run.scheduled_at, &run.timezone)
                ));
                if let Some(delivery) = &run.delivery {
                    ui.add(
                        egui::Label::new(format!("Notification: {}", notification_label(delivery)))
                            .wrap(),
                    );
                    if let Some(evidence) = notification_device_evidence(delivery, &run.timezone) {
                        ui.add(egui::Label::new(evidence).wrap());
                    }
                }
                if let Some(result) = &run.result {
                    let response = ui.add(egui::Label::new(result).wrap().selectable(true));
                    observe(
                        response.rect,
                        &format!("bokkie.conversation.result.{}", run.obligation_id),
                        result,
                        UiRole::Section,
                        true,
                        nodes,
                    );
                }
            });
    }
    if let Some(error) = &view.request_error {
        ui.label(format!("Bokkie could not complete this turn: {error}"));
    }
    if view.busy {
        ui.spinner();
        let activity = view
            .activity
            .as_deref()
            .unwrap_or("Bokkie is preparing a response. This conversation is saved.");
        let response = ui.add(egui::Label::new(activity).wrap());
        observe(
            response.rect,
            "bokkie.conversation.activity",
            activity,
            UiRole::Section,
            true,
            nodes,
        );
    }
    if let Some(outcome) = &view.adviser_outcome {
        let label = match outcome.status.as_str() {
            "completed" => "Astra provided advice",
            "failed" => "Astra consultation failed",
            "interrupted" => "Astra consultation interrupted",
            "timeout" => "Astra consultation timed out",
            "dispatched" => "Consulting Astra",
            _ => "Astra consultation",
        };
        let disclosure = egui::CollapsingHeader::new(label)
            .id_salt(("conversation-adviser-outcome", &outcome.request_id))
            .default_open(outcome.error.is_some())
            .show(ui, |ui| {
                if let Some(advice) = &outcome.advice {
                    let response = ui.add(egui::Label::new(advice).wrap().selectable(true));
                    observe(
                        response.rect,
                        "bokkie.conversation.adviser-advice",
                        advice,
                        UiRole::Section,
                        true,
                        nodes,
                    );
                }
                if let Some(error) = &outcome.error {
                    let response = ui.add(egui::Label::new(error).wrap().selectable(true));
                    observe(
                        response.rect,
                        "bokkie.conversation.adviser-error",
                        error,
                        UiRole::Section,
                        true,
                        nodes,
                    );
                }
                ui.small(format!(
                    "Agent settings revision {}",
                    outcome.profile_revision
                ));
                egui::CollapsingHeader::new("Consultation details")
                    .id_salt(("conversation-adviser-provenance", &outcome.request_id))
                    .show(ui, |ui| {
                        ui.label(format!("Request: {}", outcome.request_id));
                        ui.label(format!("Status: {}", outcome.status));
                    });
            });
        observe(
            disclosure.header_response.rect,
            "bokkie.conversation.adviser-outcome",
            label,
            UiRole::Section,
            true,
            nodes,
        );
    }
    ui.add_space(12.0);
}

fn review_card(
    ui: &mut egui::Ui,
    view: &ConversationView,
    session: Option<&ApiSession>,
    mutable: bool,
    nodes: &mut Vec<UiNode>,
    action: &mut Option<ConversationUiAction>,
) {
    let Some(review) = &view.review else {
        return;
    };
    egui::Frame::group(ui.style()).inner_margin(12.0).show(ui, |ui| {
        ui.heading(match review.action { ConversationAction::Activate => "Review activation", ConversationAction::Pause => "Review pause", ConversationAction::Resume => "Review resume" });
        ui.label(&review.explanation);
        for blocker in &review.blockers { ui.label(format!("Unavailable: {blocker}")); }
        if let Some(preview) = &review.preview {
            definition(ui, &preview.definition);
            for change in &preview.changes { ui.label(format!("Change: {change}")); }
            for blocker in &preview.blockers { ui.label(format!("Unavailable: {blocker}")); }
            for occurrence in &preview.occurrences { ui.label(format!("Scheduled: {}", local_time_in_zone(*occurrence, trigger_timezone(&preview.definition.trigger)))); }
        }
        egui::CollapsingHeader::new("Review provenance").id_salt(("review-provenance", &review.id)).show(ui, |ui| {
            ui.label(format!("Task ID: {}", review.task_id));
            ui.label(format!("Configuration revision: {}", review.configuration_revision));
            ui.label(format!("Review ID: {}", review.id));
            ui.label(format!("Process session: {}", review.session_id));
            if let Some(preview) = &review.preview {
                ui.label(format!("Definition revision: {}", preview.candidate_revision));
                ui.label(format!("Capability profile revision: {}", preview.profile_revision));
            }
        });
        if !session.is_some_and(|session| session.session_id() == review.session_id) { ui.label("This review belongs to an earlier session. Ask Bokkie for a fresh review."); }
        if !view.task.as_ref().is_some_and(|task| task.configuration_revision == review.configuration_revision) { ui.label("This task has changed since this review. Ask Bokkie for a fresh preview before another action."); }
        if button(ui, "bokkie.conversation.confirm", "Confirm reviewed action", mutable && review_is_current(view, session), nodes) { *action = Some(ConversationUiAction::Confirm); }
        ui.small("Confirmation saves this change. It does not mean a run has completed.");
    });
}

pub(super) fn observe(
    rect: egui::Rect,
    id: &str,
    name: &str,
    role: UiRole,
    enabled: bool,
    nodes: &mut Vec<UiNode>,
) {
    let mut node = UiNode::container(
        SemanticUiId::new(id),
        Some(if id == "bokkie.conversation" {
            SemanticUiId::root()
        } else {
            SemanticUiId::new("bokkie.conversation")
        }),
        role,
        rect.into(),
    );
    node.name = name.into();
    node.enabled = enabled;
    nodes.push(node);
}
pub(super) fn button(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    enabled: bool,
    nodes: &mut Vec<UiNode>,
) -> bool {
    let navigation = matches!(
        id,
        "bokkie.conversation.new"
            | "bokkie.conversation.history"
            | "bokkie.conversation.details"
            | "bokkie.conversation.panel-back"
            | "bokkie.home.notifications"
    );
    let wrap = if navigation {
        let text_width = ui
            .painter()
            .layout_no_wrap(
                label.to_owned(),
                egui::TextStyle::Button.resolve(ui.style()),
                ui.visuals().text_color(),
            )
            .size()
            .x;
        let control_width = text_width + 2.0 * ui.spacing().button_padding.x;
        // Move the parent navigation cursor before creating this button's ID scope.
        if ui.available_size_before_wrap().x < control_width {
            ui.end_row();
        }
        egui::TextWrapMode::Extend
    } else {
        egui::TextWrapMode::Wrap
    };
    let response = ui
        .push_id(id, |ui| {
            ui.add_enabled(enabled, egui::Button::new(label).wrap_mode(wrap))
        })
        .inner;
    record_native_text_control(&response, NativeTextControlKind::Button);
    let observed = if id == "bokkie.conversation.handoff" {
        response.rect.intersect(ui.clip_rect())
    } else {
        response.rect
    };
    if observed.is_positive() {
        observe(observed, id, label, UiRole::Button, enabled, nodes);
    }
    response.clicked()
}
fn catalogue_row(
    ui: &mut egui::Ui,
    entry: &ManagedCatalogueEntry,
    scope: &str,
    enabled: bool,
    nodes: &mut Vec<UiNode>,
    action: &mut Option<ConversationUiAction>,
) {
    if button(
        ui,
        &format!("bokkie.conversation.{scope}.{}", entry.id),
        &format!("{} · {}", entry.name, task_status(&entry.status)),
        enabled,
        nodes,
    ) {
        *action = Some(
            match entry
                .summary
                .as_ref()
                .and_then(|summary| summary.conversation_id.as_ref())
            {
                Some(id) => ConversationUiAction::Open(id.clone()),
                None => ConversationUiAction::Select(entry.id.clone()),
            },
        );
    }
    ui.add(egui::Label::new(&entry.description).wrap());
    if let Some(summary) = &entry.summary {
        if let Some(at) = summary.next_at {
            ui.add(
                egui::Label::new(format!(
                    "Next: {}",
                    local_time_in_zone(at, &summary.timezone)
                ))
                .wrap(),
            );
        }
        if summary.needs_input {
            ui.label("Needs your input · review the draft or open Needs attention for recovery");
        }
        if let Some(result) = &summary.latest_result {
            ui.add(
                egui::Label::new(format!("Latest result: {result}"))
                    .wrap()
                    .selectable(true),
            );
        }
    }
}
fn task_status(status: &str) -> &str {
    match status {
        "draft" => "Draft · not scheduled",
        "active" => "Schedule confirmed",
        "paused" => "Paused",
        "completed" => "Completed",
        "pending" => "Scheduled",
        "running" => "Running",
        "dispatching" => "Waiting for the execution host",
        "waiting" => "Waiting for your answer",
        "cancelling" => "Stopping",
        "stopped" => "Stopped",
        "attention" => "Needs your input",
        "awaiting_approval" => "Awaiting review",
        "retry_scheduled" => "Retry scheduled",
        "cancelled" => "Cancelled",
        _ => status,
    }
}
fn definition(ui: &mut egui::Ui, value: &ManagedTaskDefinition) {
    let retained_review = value
        .workspace
        .as_ref()
        .and_then(|w| w.review_retained_work.as_ref());
    if let Some(review) = retained_review {
        ui.strong("Review retained work");
        ui.add(egui::Label::new(format!("Verify existing delivery from execution {} against this revision's completion criteria. This occurrence starts no workspace implementation.", review.source.execution_id)).wrap());
        ui.add(egui::Label::new(&review.summary).wrap().selectable(true));
        egui::CollapsingHeader::new("Proposed completion evidence").show(ui, |ui| {
            for criterion in &review.criteria {
                ui.label(format!(
                    "{}: {}",
                    criterion.id,
                    if criterion.satisfied {
                        "Proposed as satisfied"
                    } else {
                        "Unresolved"
                    }
                ));
                for evidence in &criterion.evidence {
                    ui.add(egui::Label::new(evidence).wrap().selectable(true));
                }
            }
        });
    }
    if let Some(workspace) = &value.workspace {
        ui.label(if workspace.result_contract.is_delivery() {
            "Result: reviewed source delivery"
        } else {
            "Result: independently reviewed evidence report"
        });
        if !workspace.repository_scope.is_empty() {
            ui.add(
                egui::Label::new(format!(
                    "Selected repositories: {}",
                    workspace.repository_scope.join(", ")
                ))
                .wrap()
                .selectable(true),
            );
        }
        ui.add(
            egui::Label::new(egui::RichText::new(&workspace.brief.outcome).heading())
                .wrap()
                .selectable(true),
        );
        ui.add(
            egui::Label::new(format!(
                "Workspace: {} on {}",
                workspace.project.registration.name, workspace.project.registration.host
            ))
            .wrap()
            .selectable(true),
        );
        for (label, text) in [
            ("Relevant context", &workspace.brief.context),
            ("Scope and constraints", &workspace.brief.constraints),
            ("Completion criteria", &workspace.brief.acceptance),
            ("Decision rules", &workspace.decision_rules),
        ] {
            if !text.is_empty() {
                ui.strong(label);
                ui.add(egui::Label::new(text).wrap().selectable(true));
            }
        }
        ui.add(
            egui::Label::new(format!(
                "Permitted actions: {}",
                workspace.permitted_actions.join(", ")
            ))
            .wrap(),
        );
        ui.add(
            egui::Label::new(format!(
                "Execution limits: {} seconds, {} turns, {} observed tokens",
                workspace.limits.max_seconds,
                workspace.limits.max_turns,
                workspace.limits.max_tokens
            ))
            .wrap(),
        );
        ui.small("The workspace owns verification and independent review. Process completion alone cannot complete this task.");
    } else {
        ui.add(egui::Label::new(egui::RichText::new(&value.name).strong()).wrap());
        ui.add(egui::Label::new(&value.purpose).wrap().selectable(true));
        ui.add(
            egui::Label::new(&value.instructions)
                .wrap()
                .selectable(true),
        );
    }
    ui.label(match &value.trigger {
        ManagedTrigger::Immediate => "Timing: once, immediately after confirmation".into(),
        ManagedTrigger::Once {
            local_datetime,
            timezone,
        } => format!("Timing: {local_datetime} ({timezone})"),
        ManagedTrigger::Recurring { cron, timezone } => recurring_description(cron, timezone),
    });
    ui.label(match value.capability.as_str() {
        "reminder" => "Reminder: save this text and send a notification when due",
        "local_note" => "Capability: Local note",
        "workspace" if retained_review.is_some() => {
            "Workspace task: review retained delivery evidence"
        }
        "workspace" => "Workspace task: execute the reviewed assignment",
        _ => "This task requires a capability that is unavailable.",
    });
    if value.capability == "reminder" {
        ui.add(
            egui::Label::new(format!("Notification destination: {}", value.destination))
                .wrap()
                .selectable(true),
        );
        ui.small(
            "An occurrence saves its result first. Notification delivery is tracked separately.",
        );
    } else if value.workspace.is_some() {
        ui.label("Results: in this task, with source and verification evidence");
    } else {
        ui.label(match value.destination.as_str() {
            "task_results" => "Results: In-app task results",
            _ => "This task requires a result destination that is unavailable.",
        });
    }
    for effect in &value.effects {
        ui.label(match effect.as_str() {
            "store_local_result" if value.workspace.is_some() => {
                "Retain the workspace result and run history"
            }
            "store_local_result" => "Save the supplied text as a local result",
            "send_notification" => "Send the reminder to the reviewed destination",
            "workspace_execution" if retained_review.is_some() => {
                "Verify retained delivery evidence through this workspace host"
            }
            "workspace_execution" => "Run the agreed assignment through this workspace",
            _ => "This task requests an effect that is unavailable.",
        });
    }
    for context in &value.context_refs {
        ui.label(format!("Context: {context}"));
    }
    egui::CollapsingHeader::new("Definition provenance")
        .id_salt(("definition-provenance", &value.name))
        .show(ui, |ui| {
            ui.label(format!("Capability identifier: {}", value.capability));
            ui.label(format!(
                "Capability profile revision: {}",
                value.profile_revision
            ));
            ui.label(format!(
                "Result destination identifier: {}",
                value.destination
            ));
            ui.label(format!("Effect identifiers: {}", value.effects.join(", ")));
            ui.label(format!("Attempt limit: {}", value.max_attempts));
            ui.label(format!(
                "Output limit: {} characters",
                value.max_output_chars
            ));
            if let ManagedTrigger::Recurring { cron, timezone } = &value.trigger {
                ui.label(format!("Cron expression: {cron}"));
                ui.label(format!("Time zone: {timezone}"));
            }
        });
}

fn recurring_description(expression: &str, timezone: &str) -> String {
    let mut fields = expression.split_whitespace().collect::<Vec<_>>();
    if fields.len() == 5 {
        fields.insert(0, "0");
    }
    let simple = || -> Option<String> {
        if !(fields.len() == 6 || fields.len() == 7)
            || fields[0] != "0"
            || fields[3] != "*"
            || fields[4] != "*"
            || (fields.len() == 7 && fields[6] != "*")
        {
            return None;
        }
        let minute: u32 = fields[1].parse().ok()?;
        let hour: u32 = fields[2].parse().ok()?;
        if minute > 59 || hour > 23 {
            return None;
        }
        let cadence = match fields[5].to_ascii_lowercase().as_str() {
            "*" | "?" => "Every day",
            "mon-fri" | "2-6" => "Monday to Friday",
            "sun" | "sunday" | "1" => "Every Sunday",
            "mon" | "monday" | "2" => "Every Monday",
            "tue" | "tues" | "tuesday" | "3" => "Every Tuesday",
            "wed" | "wednesday" | "4" => "Every Wednesday",
            "thu" | "thurs" | "thursday" | "5" => "Every Thursday",
            "fri" | "friday" | "6" => "Every Friday",
            "sat" | "saturday" | "7" => "Every Saturday",
            _ => return None,
        };
        let period = if hour < 12 { "am" } else { "pm" };
        let clock_hour = if hour.is_multiple_of(12) {
            12
        } else {
            hour % 12
        };
        Some(format!(
            "{cadence} at {clock_hour}:{minute:02} {period} ({timezone})"
        ))
    };
    simple().unwrap_or_else(|| format!("Custom recurring schedule ({timezone}). Use the upcoming run dates to check the timing."))
}

fn review_context_is_current(view: &ConversationView, session: Option<&ApiSession>) -> bool {
    let Some(review) = &view.review else {
        return false;
    };
    session.is_some_and(|session| {
        session.matches(&view.service) && session.session_id() == review.session_id
    }) && view.selected_task_id.as_deref() == Some(&review.task_id)
        && view.task.as_ref().is_some_and(|task| {
            task.id == review.task_id
                && task.configuration_revision == review.configuration_revision
        })
}

fn review_is_current(view: &ConversationView, session: Option<&ApiSession>) -> bool {
    let Some(review) = &view.review else {
        return false;
    };
    review_context_is_current(view, session)
        && !view.busy
        && review.blockers.is_empty()
        && review.preview.as_ref().is_none_or(|preview| {
            preview.blockers.is_empty()
                && preview.task_id == review.task_id
                && preview.configuration_revision == review.configuration_revision
                && preview.session_id == review.session_id
        })
}

fn task_detail(ui: &mut egui::Ui, task: &ManagedTaskDetail, nodes: &mut Vec<UiNode>) {
    ui.separator();
    ui.label(format!(
        "Task status: {}",
        task_status(match task.status {
            bokkie_operator_api::ManagedTaskStatus::Draft => "draft",
            bokkie_operator_api::ManagedTaskStatus::Active => "active",
            bokkie_operator_api::ManagedTaskStatus::Paused => "paused",
            bokkie_operator_api::ManagedTaskStatus::Completed => "completed",
        })
    ));
    if let Some(at) = task.next_wake_at {
        let timezone = task
            .active
            .as_ref()
            .map(|active| trigger_timezone(&active.definition.trigger))
            .unwrap_or("Australia/Adelaide");
        ui.label(format!("Next run: {}", local_time_in_zone(at, timezone)));
    }
    if let Some(active) = &task.active {
        egui::CollapsingHeader::new(format!("Active definition · revision {}", active.revision))
            .show(ui, |ui| definition(ui, &active.definition));
    }
    if let Some(candidate) = &task.candidate {
        egui::CollapsingHeader::new(format!(
            "Candidate definition · revision {}",
            candidate.revision
        ))
        .default_open(true)
        .show(ui, |ui| definition(ui, &candidate.definition));
    }
    for run in &task.runs {
        let status = run
            .workspace
            .as_ref()
            .map_or_else(|| task_status(&run.state), |w| task_status(&w.status));
        let title = format!(
            "{} · {}",
            if run.result.is_some() {
                "Completed"
            } else {
                status
            },
            short_local_time(run.scheduled_at, &run.timezone)
        );
        let response = egui::CollapsingHeader::new(&title)
        .id_salt(("task-run", &run.obligation_id))
        .default_open(run.result.is_some() || run.workspace.as_ref().is_some_and(|w|!w.cessation_verified || w.status=="attention"))
        .show(ui, |ui| {
            ui.add(egui::Label::new(local_time_in_zone(run.scheduled_at, &run.timezone)).wrap());
            ui.small(format!("Definition revision {}", run.definition_revision));
            if let Some(workspace) = &run.workspace {
                ui.add(egui::Label::new(format!("Workspace: {}",task_status(&workspace.status))).wrap());
                ui.add(egui::Label::new(&workspace.progress).wrap().selectable(true));
                if let Some(recovery) = &workspace.recovery {
                    let label = format!("Report recovered from retained delivery evidence on {}. The original execution stopped before submitting its report; its history and limits are preserved.", local_time_in_zone(recovery.recovered_at, &run.timezone));
                    let response = ui.add(egui::Label::new(&label).wrap());
                    observe(response.rect, &format!("bokkie.workspace.recovery.{}", workspace.execution_id), &label, UiRole::Section, true, nodes);
                }
                if workspace.cancellation_requested && !workspace.cessation_verified {ui.label("Stop requested; waiting for the host to account for this execution and its descendants.");}
                if let Some(result) = &workspace.result {
                    if run.result.is_none() {ui.add(egui::Label::new(&result.summary).wrap().selectable(true));}
                    for delivery in &result.deliveries {ui.hyperlink_to(format!("Delivered change in {}",delivery.repository),&delivery.pull_request);}
                    if let Some(report) = &result.report {
                        ui.strong("Evidence report");
                        let rendered = ui.add(egui::Label::new(&report.markdown).wrap().selectable(true));
                        observe(rendered.rect,&format!("bokkie.workspace.report.{}",workspace.execution_id),&report.markdown,UiRole::Section,true,nodes);
                        for (index, source) in report.sources.iter().enumerate() {ui.hyperlink_to(format!("Source {}: {}", index + 1, source.url),&source.url);}
                        ui.small(format!("Report identity: {}",report.digest));
                    }
                    egui::CollapsingHeader::new("Completion evidence").id_salt(("workspace-evidence", &workspace.execution_id)).show(ui, |ui| {
                        for criterion in &result.criteria {
                            ui.label(format!("{}: {}", criterion.id, if criterion.satisfied {"Satisfied"} else {"Unresolved"}));
                            for evidence in &criterion.evidence {ui.add(egui::Label::new(evidence).wrap().selectable(true));}
                        }
                    });
                    for limitation in &result.limitations {ui.add(egui::Label::new(format!("Limit: {limitation}")).wrap());}
                }
                if let Some(verification) = &workspace.verification {
                    egui::CollapsingHeader::new(if verification.passed {"Result verification passed"} else {"Result verification pending"}).id_salt(("workspace-verification", &workspace.execution_id)).show(ui, |ui| {
                        for evidence in &verification.evidence {ui.add(egui::Label::new(evidence).wrap().selectable(true));}
                    });
                }
            }
            if let Some(result) = &run.result {
                let response = ui.add(egui::Label::new(result).wrap().selectable(true));
                observe(
                    response.rect,
                    &format!("bokkie.conversation.history-result.{}", run.obligation_id),
                    result,
                    UiRole::Section,
                    true,
                    nodes,
                );
            } else if run.workspace.is_none() {
                ui.label("No local result yet.");
            }
            if let Some(delivery) = &run.delivery {
                ui.add(egui::Label::new(format!("Notification: {}", notification_label(delivery))).wrap().selectable(true));
                ui.add(egui::Label::new(format!("Destination: {}", delivery.destination)).wrap());
                if let Some(evidence) = notification_device_evidence(delivery, &run.timezone) {
                    ui.add(egui::Label::new(evidence).wrap());
                }
                if let Some(at) = delivery.next_retry_at {
                    ui.label(format!("Next delivery attempt: {}", local_time_in_zone(at, &run.timezone)));
                }
                if matches!(delivery.status.as_str(), "needs_attention" | "uncertain") {
                    ui.label(if delivery.push.is_some() {
                        "Open Needs attention to resolve this delivery without resending or review an available recovery. The result is already saved; its device and deadline remain pinned."
                    } else {
                        "Open Needs attention to review recovery. The reminder result is already saved."
                    });
                }
                egui::CollapsingHeader::new("Delivery history and provenance").id_salt((&delivery.id,"history")).show(ui, |ui| {
                    ui.label(format!("Delivery identity: {}", delivery.id));
                    ui.add(egui::Label::new(&delivery.detail).wrap().selectable(true));
                    for attempt in &delivery.attempts { ui.add(egui::Label::new(format!("{attempt:?}")).wrap()); }
                });
            }
        });
        if let Some(workspace) = &run.workspace {
            observe(
                response.header_response.rect,
                &format!("bokkie.workspace.run-heading.{}", workspace.execution_id),
                &title,
                UiRole::Section,
                true,
                nodes,
            );
        }
    }
}
fn trigger_timezone(trigger: &ManagedTrigger) -> &str {
    match trigger {
        ManagedTrigger::Immediate => "Australia/Adelaide",
        ManagedTrigger::Once { timezone, .. } | ManagedTrigger::Recurring { timezone, .. } => {
            timezone
        }
    }
}
pub(super) fn local_time_in_zone(seconds: i64, timezone: &str) -> String {
    let Ok(zone) = timezone.parse::<chrono_tz::Tz>() else {
        return format!("Invalid time zone: {timezone}");
    };
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|date| {
            format!(
                "{} ({timezone})",
                date.with_timezone(&zone).format("%a %d %b %Y, %H:%M %Z")
            )
        })
        .unwrap_or_else(|| "Invalid scheduled time".into())
}
fn short_local_time(seconds: i64, timezone: &str) -> String {
    let Ok(zone) = timezone.parse::<chrono_tz::Tz>() else {
        return "Invalid time".into();
    };
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|date| {
            date.with_timezone(&zone)
                .format("%a %d %b, %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "Invalid time".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bokkie_operator_api::{
        API_CONTRACT_VERSION, BOKKIE_BUILD_ID, ConversationMessage, ConversationReview,
        SUPPORTED_SCHEMA_VERSION, ServiceIdentity, SessionBootstrap,
    };

    fn session(id: &str) -> ApiSession {
        ApiSession::from_bootstrap(SessionBootstrap {
            service: ServiceIdentity {
                build: BOKKIE_BUILD_ID.into(),
                api_contract_version: API_CONTRACT_VERSION,
                schema_version: SUPPORTED_SCHEMA_VERSION,
                process_id: 42,
                session_id: id.into(),
            },
            mutation_token: "a".repeat(64),
        })
        .unwrap()
    }
    fn view(id: &str, session_id: &str, revision: i64) -> ConversationView {
        let service = ServiceIdentity {
            build: BOKKIE_BUILD_ID.into(),
            api_contract_version: API_CONTRACT_VERSION,
            schema_version: SUPPORTED_SCHEMA_VERSION,
            process_id: 42,
            session_id: session_id.into(),
        };
        ConversationView {
            service,
            id: id.into(),
            revision,
            selected_task_id: None,
            messages: vec![],
            candidates: vec![],
            review: None,
            task: None,
            busy: false,
            request_error: None,
            runtime_available: true,
            adviser_available: false,
            activity: None,
            adviser_outcome: None,
            handoff_draft: None,
            notes_available: true,
            workspace_available: false,
            reminders_available: true,
            receipt: None,
        }
    }

    #[test]
    fn manual_adviser_choice_is_capability_checked_and_pinned_for_uncertain_retry() {
        let mut current = view("chat", "current", 5);
        current.adviser_available = true;
        let mut state = ConversationState {
            open: true,
            id: Some("chat".into()),
            view: Some(current),
            text: "Retain this request".into(),
            consult_adviser: true,
            ..Default::default()
        };
        let request = state.turn_request(state.text.clone()).unwrap();
        assert!(request.consult_adviser);
        state.pending = Some(ApiRequest::ConversationTurn(request.clone()));
        state.consult_adviser = false;
        state.reset_session();
        assert_eq!(
            state.pending,
            Some(ApiRequest::ConversationTurn(request.clone()))
        );
        state.consult_adviser = true;
        state.view.as_mut().unwrap().adviser_available = false;
        assert!(
            !state
                .turn_request(state.text.clone())
                .unwrap()
                .consult_adviser
        );
        assert_eq!(state.pending, Some(ApiRequest::ConversationTurn(request)));
    }

    #[test]
    fn adviser_activity_does_not_move_the_composer_or_lose_its_draft() {
        for (width, height) in [(1440.0, 900.0), (390.0, 844.0)] {
            let context = egui::Context::default();
            Appearance::default().apply(&context);
            let mut app = super::super::tests::test_app();
            let mut current = view("chat", "current", 5);
            current.busy = true;
            current.adviser_available = true;
            app.conversation = ConversationState {
                open: true,
                id: Some("chat".into()),
                view: Some(current),
                text: "Keep this unsent text.\n".repeat(150),
                consult_adviser: true,
                ..Default::default()
            };
            let before = app.conversation.text.clone();
            for activity in ["Consulting Astra", "Bokkie continuing"] {
                app.conversation.view.as_mut().unwrap().activity = Some(activity.into());
                if activity == "Bokkie continuing" {
                    app.conversation.view.as_mut().unwrap().adviser_outcome =
                        Some(bokkie_operator_api::ConversationAdviserOutcome {
                            request_id: "saved-adviser-request".into(),
                            profile_revision: 7,
                            status: "failed".into(),
                            advice: None,
                            error: Some(
                                "Astra did not return advice within the saved limit.".into(),
                            ),
                        });
                }
                let mut nodes = vec![];
                for _ in 0..3 {
                    nodes.clear();
                    context
                        .run_ui(
                            egui::RawInput {
                                screen_rect: Some(egui::Rect::from_min_size(
                                    egui::Pos2::ZERO,
                                    egui::vec2(width, height),
                                )),
                                ..Default::default()
                            },
                            |ui| app.show_conversation(ui, &mut nodes, &mut vec![]),
                        )
                        .textures_delta
                        .clear();
                }
                for id in [
                    "bokkie.conversation.text",
                    "bokkie.conversation.consult-adviser",
                    "bokkie.conversation.send",
                ] {
                    let node = nodes
                        .iter()
                        .find(|node| node.id == SemanticUiId::new(id))
                        .unwrap_or_else(|| panic!("missing {id} at {width}"));
                    assert!(
                        node.rect.min_x >= 0.0
                            && node.rect.max_x <= width
                            && node.rect.max_y <= height,
                        "{id}: {:?}",
                        node.rect
                    );
                }
                assert!(nodes.iter().any(|node| node.id
                    == SemanticUiId::new("bokkie.conversation.activity")
                    && node.name == activity));
                if activity == "Bokkie continuing" {
                    assert!(nodes.iter().any(|node| node.id
                        == SemanticUiId::new("bokkie.conversation.adviser-outcome")
                        && node.name == "Astra consultation failed"));
                    assert!(nodes.iter().any(|node| node.id
                        == SemanticUiId::new("bokkie.conversation.adviser-error")
                        && node.name.contains("saved limit")));
                }
                assert_eq!(app.conversation.text, before);
                assert!(app.conversation.consult_adviser);
            }
        }
    }

    #[test]
    fn global_navigation_preserves_conversation_context_and_unsent_message() {
        let mut app = super::super::tests::test_app();
        let current = session("current");
        app.session = Some(current.clone());
        let mut saved = view("chat", "current", 5);
        saved.selected_task_id = Some("selected-task".into());
        saved.messages.push(ConversationMessage {
            role: "assistant".into(),
            text: "A retained response".into(),
            request_id: "done".into(),
        });
        app.conversation = ConversationState {
            open: true,
            id: Some("chat".into()),
            view: Some(saved.clone()),
            text: "An unsent message\nwith two lines".into(),
            consult_adviser: true,
            panel: Some(ConversationPanel::Details),
            ..Default::default()
        };
        let context = egui::Context::default();
        app.open_agent_settings(&context);
        assert!(app.agent_settings.open);
        app.apply_intents(vec![OperatorIntent::Navigate(INBOX_PANE_ID)], &context);
        assert!(!app.agent_settings.open);
        assert!(!app.conversation.open);
        app.open_conversation_tasks(&context);
        assert_eq!(app.conversation.panel, Some(ConversationPanel::Tasks));
        app.open_agent_settings(&context);
        app.open_conversation_history(&context);
        assert_eq!(app.conversation.panel, Some(ConversationPanel::History));
        app.open_agent_settings(&context);
        app.open_conversation(None, &context);
        assert!(!app.agent_settings.open);
        assert!(app.conversation.open);
        assert_eq!(app.conversation.id.as_deref(), Some("chat"));
        assert_eq!(app.conversation.text, "An unsent message\nwith two lines");
        assert!(app.conversation.consult_adviser);
        assert_eq!(
            app.conversation.view.as_ref().unwrap().messages,
            saved.messages
        );
        assert_eq!(
            app.conversation.view.as_ref().unwrap().selected_task_id,
            saved.selected_task_id
        );
        assert!(app.conversation.pending.is_none());
        assert!(!app.model.action_busy);
    }

    #[test]
    fn notification_revision_refreshes_capability_once_without_discarding_composer_or_history() {
        let session = session("current");
        let mut state = ConversationState {
            open: true,
            id: Some("chat".into()),
            text: "Keep this unsent draft".into(),
            ..Default::default()
        };
        state.observe_notification_revision(Some(3));
        assert!(state.begin_notification_refresh(1).is_none());
        let mut initial = view("chat", "current", 5);
        initial.reminders_available = false;
        initial.messages.push(ConversationMessage {
            role: "user".into(),
            text: "Saved transcript".into(),
            request_id: "saved-request".into(),
        });
        state.accept(initial.clone(), &session);
        state.observe_notification_revision(Some(4));
        assert_eq!(
            state.begin_notification_refresh(2),
            Some(ApiRequest::Conversation {
                id: "chat".into(),
                generation: 2
            })
        );
        state.observe_notification_revision(Some(4));
        assert!(state.begin_notification_refresh(3).is_none());
        assert!(state.finish_read("chat", 2));
        let mut refreshed = initial.clone();
        refreshed.reminders_available = true;
        state.accept(refreshed, &session);
        assert!(state.view.as_ref().unwrap().reminders_available);
        assert_eq!(state.view.as_ref().unwrap().messages, initial.messages);
        assert_eq!(state.text, "Keep this unsent draft");
        assert_eq!(state.id.as_deref(), Some("chat"));
        assert!(state.pending.is_none());
        state.observe_notification_revision(Some(4));
        assert!(state.begin_notification_refresh(4).is_none());
        state.observe_notification_revision(Some(5)); // Disabling also refreshes the same projection.
        assert!(matches!(
            state.begin_notification_refresh(5),
            Some(ApiRequest::Conversation { .. })
        ));
    }

    #[test]
    fn notification_revision_change_during_an_existing_read_waits_for_one_fresh_read() {
        let mut state = ConversationState {
            open: true,
            id: Some("chat".into()),
            ..Default::default()
        };
        state.observe_notification_revision(Some(3));
        assert!(state.begin_read(1).is_some());
        state.observe_notification_revision(Some(4));
        assert!(state.begin_notification_refresh(2).is_none());
        assert!(state.notification_refresh_pending);
        assert!(state.finish_read("chat", 1));
        assert!(state.begin_notification_refresh(2).is_some());
        assert!(!state.notification_refresh_pending);
        assert!(state.finish_read("chat", 2));
        state.observe_notification_revision(Some(4));
        assert!(state.begin_notification_refresh(3).is_none());
    }

    #[test]
    fn reminder_history_stays_within_a_narrow_details_column() {
        use bokkie_operator_api::{ManagedDelivery, ManagedRun, ManagedTaskStatus};
        for width in [324.0, 390.0] {
            let context = egui::Context::default();
            let task = ManagedTaskDetail {
                id: "task".into(),
                configuration_revision: 1,
                status: ManagedTaskStatus::Active,
                active: None,
                candidate: None,
                next_wake_at: None,
                runs: vec![ManagedRun {
                    obligation_id: "occurrence".into(),
                    definition_revision: 1,
                    profile_revision: "reminder-v1".into(),
                    scheduled_at: 1790033400,
                    admitted_at: Some(1790033400),
                    state: "completed".into(),
                    result: Some(
                        "Review today's priorities and choose the work that matters most.".into(),
                    ),
                    timezone: "Australia/Adelaide".into(),
                    workspace: None,
                    delivery: Some(ManagedDelivery {
                        id: "delivery".into(),
                        status: "accepted_by_relay".into(),
                        detail: "Synthetic acceptance".into(),
                        destination: "fixture-recipient@example.invalid".into(),
                        subject: "Review priorities".into(),
                        body: "Review priorities".into(),
                        next_retry_at: None,
                        attempts: vec![],
                        recovery: None,
                        push: None,
                    }),
                }],
            };
            for _ in 0..3 {
                let mut nodes = vec![];
                let mut content_width = 0.0;
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
                            task_detail(ui, &task, &mut nodes);
                            content_width = ui.min_rect().width();
                        },
                    )
                    .textures_delta
                    .clear();
                assert!(
                    content_width <= width,
                    "History expanded its {width}-pixel column to {content_width}"
                );
                let result = nodes
                    .iter()
                    .find(|n| {
                        n.id == SemanticUiId::new("bokkie.conversation.history-result.occurrence")
                    })
                    .unwrap();
                assert!(result.rect.max_x <= width && result.rect.max_y > result.rect.min_y);
            }
        }
    }

    #[test]
    fn composer_retains_send_control_with_a_long_multiline_draft() {
        for width in [340.0, 440.0, 760.0] {
            let context = egui::Context::default();
            let mut state = ConversationState {
                text: "A long line in an unsent local note.\n".repeat(150),
                ..Default::default()
            };
            for _ in 0..3 {
                let mut nodes = Vec::new();
                context
                    .run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(width, 138.0),
                            )),
                            ..Default::default()
                        },
                        |ui| {
                            conversation_composer(
                                ui, &mut state, true, false, true, &mut nodes, &mut None,
                            )
                        },
                    )
                    .textures_delta
                    .clear();
                for id in ["bokkie.conversation.text", "bokkie.conversation.send"] {
                    let node = nodes
                        .iter()
                        .find(|node| node.id == SemanticUiId::new(id))
                        .unwrap();
                    assert!(
                        node.rect.max_y <= 138.0,
                        "{id} below composer at {width}: {:?}",
                        node.rect
                    );
                    assert!(node.rect.max_x <= width, "{id} overflows at {width}");
                }
            }
            assert!(state.text.contains("A long line"));
            assert!(state.pending.is_none());
        }
    }

    #[test]
    fn retained_turn_reconciles_only_its_durable_message() {
        let request = ApiRequest::ConversationTurn(ConversationTurnRequest {
            command_id: "command-1".into(),
            conversation_id: "chat".into(),
            expected_revision: 0,
            text: "retain me".into(),
            consult_adviser: false,
        });
        let mut state = ConversationState {
            id: Some("chat".into()),
            pending: Some(request.clone()),
            text: "retain me".into(),
            ..Default::default()
        };
        state.accept(view("chat", "current", 1), &session("current"));
        assert_eq!(state.pending, Some(request));
        assert_eq!(state.text, "retain me");
        let mut saved = view("chat", "current", 2);
        saved.messages.push(ConversationMessage {
            role: "user".into(),
            text: "retain me".into(),
            request_id: "command-1".into(),
        });
        state.accept(saved, &session("current"));
        assert!(state.pending.is_none());
        assert!(state.text.is_empty());
    }
    #[test]
    fn stale_identity_revision_and_other_chat_cannot_replace_current_view() {
        let mut state = ConversationState {
            id: Some("chat".into()),
            ..Default::default()
        };
        state.accept(view("chat", "current", 5), &session("current"));
        state.accept(view("other", "current", 9), &session("current"));
        state.accept(view("chat", "old", 9), &session("current"));
        state.accept(view("chat", "current", 4), &session("current"));
        assert_eq!(state.view.unwrap(), view("chat", "current", 5));
    }
    #[test]
    fn rotation_discards_confirmation_but_retains_unacknowledged_turn() {
        let mut old = view("chat", "old", 1);
        old.review = Some(ConversationReview {
            id: "review".into(),
            action: ConversationAction::Activate,
            task_id: "task".into(),
            configuration_revision: 1,
            session_id: "old".into(),
            preview: None,
            explanation: "Activate".into(),
            blockers: vec![],
        });
        let mut state = ConversationState {
            id: Some("chat".into()),
            view: Some(old),
            pending: Some(ApiRequest::ConversationConfirm(
                ConversationConfirmRequest {
                    command_id: "confirm".into(),
                    conversation_id: "chat".into(),
                    proposal_id: "review".into(),
                    session_id: "old".into(),
                },
            )),
            ..Default::default()
        };
        state.reset_session();
        assert!(state.pending.is_none());
        assert!(state.view.as_ref().unwrap().review.is_none());
        state.pending = Some(ApiRequest::ConversationTurn(ConversationTurnRequest {
            command_id: "turn".into(),
            conversation_id: "chat".into(),
            expected_revision: 1,
            text: "Keep".into(),
            consult_adviser: false,
        }));
        state.reset_session();
        assert!(matches!(
            state.pending,
            Some(ApiRequest::ConversationTurn(_))
        ));
    }
    #[test]
    fn scheduled_dates_use_adelaide_daylight_and_standard_time() {
        let summer = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .timestamp();
        let winter = chrono::DateTime::parse_from_rfc3339("2026-07-01T00:00:00Z")
            .unwrap()
            .timestamp();
        assert!(local_time_in_zone(summer, "Australia/Adelaide").contains("10:30 ACDT"));
        assert!(local_time_in_zone(winter, "Australia/Adelaide").contains("09:30 ACST"));
    }
    #[test]
    fn recurring_schedules_have_readable_common_patterns_and_honest_fallback() {
        for (cron, expected) in [
            ("0 30 9 * * *", "Every day at 9:30 am"),
            ("0 9 * * Mon-Fri", "Monday to Friday at 9:00 am"),
            ("15 8 * * Mon", "Every Monday at 8:15 am"),
            ("0 5 17 * * Mon-Fri *", "Monday to Friday at 5:05 pm"),
            ("0 0 0 * * MON", "Every Monday at 12:00 am"),
            ("0 15 12 * * 2", "Every Monday at 12:15 pm"),
        ] {
            assert_eq!(
                recurring_description(cron, "Australia/Adelaide"),
                format!("{expected} (Australia/Adelaide)")
            );
        }
        for cron in [
            "*/30 * * * * *",
            "0 0 9 1 * *",
            "0 0 9 * * MON 2027",
            "0 80 26 * * *",
            "0 0 9 * * MON,WED",
        ] {
            let description = recurring_description(cron, "Australia/Adelaide");
            assert!(description.starts_with("Custom recurring schedule"));
            assert!(!description.contains(cron));
        }
    }

    #[test]
    fn single_confirmation_requires_exact_current_backend_review() {
        let mut current = view("chat", "current", 1);
        current.selected_task_id = Some("task".into());
        current.task = Some(ManagedTaskDetail {
            id: "task".into(),
            configuration_revision: 3,
            status: bokkie_operator_api::ManagedTaskStatus::Paused,
            active: None,
            candidate: None,
            next_wake_at: None,
            runs: vec![],
        });
        current.review = Some(ConversationReview {
            id: "review".into(),
            action: ConversationAction::Resume,
            task_id: "task".into(),
            configuration_revision: 3,
            session_id: "current".into(),
            preview: None,
            explanation: "Resume the active definition".into(),
            blockers: vec![],
        });
        assert!(review_is_current(&current, Some(&session("current"))));
        assert!(!review_is_current(&current, Some(&session("old"))));
        assert!(!review_is_current(&current, None));
        let mut changed = current.clone();
        changed.task.as_mut().unwrap().configuration_revision += 1;
        assert!(!review_is_current(&changed, Some(&session("current"))));
        changed = current.clone();
        changed.selected_task_id = Some("other".into());
        assert!(!review_is_current(&changed, Some(&session("current"))));
        changed = current.clone();
        changed.busy = true;
        assert!(review_context_is_current(
            &changed,
            Some(&session("current"))
        ));
        assert!(!review_is_current(&changed, Some(&session("current"))));
        changed = current;
        changed
            .review
            .as_mut()
            .unwrap()
            .blockers
            .push("Unavailable".into());
        assert!(review_context_is_current(
            &changed,
            Some(&session("current"))
        ));
        assert!(!review_is_current(&changed, Some(&session("current"))));
    }
    #[test]
    fn long_transcript_and_error_keep_composer_inside_the_viewport() {
        for (width, height) in [(1440.0, 800.0), (480.0, 700.0), (390.0, 650.0)] {
            let context = egui::Context::default();
            let mut app = super::super::tests::test_app();
            let mut current = view("chat", "current", 1);
            current.messages = (0..30)
                .map(|index| ConversationMessage {
                    request_id: format!("request-{index}"),
                    role: if index % 2 == 0 {
                        "user".into()
                    } else {
                        "assistant".into()
                    },
                    text: "Long conversation text that should remain in the transcript. "
                        .repeat(20),
                })
                .collect();
            app.session = Some(session("current"));
            app.conversation.view = Some(current);
            app.conversation.error = Some(
                "A recoverable connection failure with detailed diagnostic context. ".repeat(20),
            );
            app.conversation.text = "Keep this unsent draft.\n".repeat(150);
            for _ in 0..3 {
                let mut nodes = Vec::new();
                context
                    .run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(width, height),
                            )),
                            ..Default::default()
                        },
                        |ui| app.show_conversation(ui, &mut nodes, &mut Vec::new()),
                    )
                    .textures_delta
                    .clear();
                for id in ["bokkie.conversation.text", "bokkie.conversation.send"] {
                    let node = nodes
                        .iter()
                        .find(|node| node.id == SemanticUiId::new(id))
                        .unwrap();
                    assert!(
                        node.rect.max_y <= height,
                        "{id} below viewport at {width}: {:?}",
                        node.rect
                    );
                    assert!(
                        node.rect.min_y >= height - 190.0,
                        "{id} is not anchored at {width}: {:?}",
                        node.rect
                    );
                    assert!(node.rect.max_x <= width, "{id} overflows at {width}");
                }
            }
        }
    }

    #[test]
    fn long_candidates_messages_and_definition_fit_narrow_viewport() {
        for width in [390.0, 480.0] {
            let context = egui::Context::default();
            for _ in 0..3 {
                context.run_ui(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 3000.0))),
                    ..Default::default()
                }, |ui| {
                    let mut nodes = Vec::new();
                    let mut action = None;
                    let entry = ManagedCatalogueEntry {
                        id: "task".into(), kind: "managed_task".into(), status: "draft".into(),
                        name: "A long task name with ordinary words and enough detail to wrap across several lines".into(),
                        description: "Long task description ".repeat(12),
                        summary: None,
                    };
                    catalogue_row(ui, &entry, "candidate", true, &mut nodes, &mut action);
                    assert!(nodes[0].rect.max_x <= width, "candidate overflow at {width}");
                    let message = egui::Frame::group(ui.style()).inner_margin(12.0).show(ui, |ui| {
                        ui.add(egui::Label::new("Long response describing the proposed schedule and changes. ".repeat(20)).wrap().selectable(true));
                    });
                    assert!(message.response.rect.right() <= width, "message overflow at {width}");
                    let mut task = ManagedTaskDefinition::local_note(entry.name, entry.description);
                    task.context_refs.push("a-long-context-reference-with-no-spaces".repeat(8));
                    task.trigger = ManagedTrigger::Recurring { cron: "0 30 9 * * Mon-Fri".into(), timezone: "Australia/Adelaide".into() };
                    let preview = egui::Frame::group(ui.style()).inner_margin(12.0).show(ui, |ui| definition(ui, &task));
                    assert!(preview.response.rect.right() <= width, "preview overflow at {width}");
                }).textures_delta.clear();
            }
        }
    }
    #[test]
    fn dates_follow_definition_timezone_including_daylight_saving() {
        let summer = chrono::DateTime::parse_from_rfc3339("2026-07-01T00:00:00Z")
            .unwrap()
            .timestamp();
        assert!(local_time_in_zone(summer, "Europe/London").contains("01:00 BST (Europe/London)"));
        assert!(
            local_time_in_zone(summer, "America/New_York")
                .contains("30 Jun 2026, 20:00 EDT (America/New_York)")
        );
        assert_eq!(
            trigger_timezone(&ManagedTrigger::Immediate),
            "Australia/Adelaide"
        );
        assert_eq!(
            trigger_timezone(&ManagedTrigger::Recurring {
                cron: "0 0 9 * * *".into(),
                timezone: "Europe/London".into()
            }),
            "Europe/London"
        );
        assert_eq!(
            local_time_in_zone(summer, "bad/zone"),
            "Invalid time zone: bad/zone"
        );
    }
    #[test]
    fn history_switch_owns_new_read_and_ignores_delayed_previous_responses() {
        let mut app = super::super::tests::test_app();
        let context = egui::Context::default();
        app.session = Some(session("current"));
        app.conversation.open = true;
        app.conversation.switch("chat-a".into(), None);
        let generation = app.fresh_generation();
        let delayed = app.conversation.begin_read(generation).unwrap();

        app.open_saved_conversation("chat-b".into(), &context);
        assert_eq!(
            app.conversation.reading.as_ref().map(|(id, _)| id.as_str()),
            Some("chat-b")
        );
        for response in [
            Ok(ApiPayload::Conversation(Box::new(view(
                "chat-a", "current", 4,
            )))),
            Err(ApiFailure::Other("Delayed connection failure".into())),
            Err(ApiFailure::SessionChanged("Delayed stale session".into())),
        ] {
            app.conversation_response(delayed.clone(), response, &context);
            assert_eq!(
                app.conversation.reading.as_ref().map(|(id, _)| id.as_str()),
                Some("chat-b")
            );
            assert!(app.conversation.view.is_none());
            assert!(app.conversation.error.is_none());
            assert!(app.conversation.begin_read(99).is_none());
        }
        app.conversation_response(
            ApiRequest::Conversation {
                id: "chat-b".into(),
                generation: app.conversation.reading.as_ref().unwrap().1,
            },
            Ok(ApiPayload::Conversation(Box::new(view(
                "chat-b", "current", 2,
            )))),
            &context,
        );
        assert!(app.conversation.reading.is_none());
        assert_eq!(app.conversation.view.as_ref().unwrap().id, "chat-b");
        assert_eq!(
            app.conversation.begin_read(100),
            Some(ApiRequest::Conversation {
                id: "chat-b".into(),
                generation: 100,
            })
        );
    }

    #[test]
    fn task_context_switch_reads_new_chat_before_selecting_requested_task() {
        let mut app = super::super::tests::test_app();
        let context = egui::Context::default();
        app.session = Some(session("current"));
        app.conversation.open = true;
        app.conversation
            .switch("chat-a".into(), Some("old-task".into()));
        let generation = app.fresh_generation();
        let delayed = app.conversation.begin_read(generation).unwrap();

        app.open_conversation(Some("new-task".into()), &context);
        let next_id = app.conversation.id.clone().unwrap();
        assert_ne!(next_id, "chat-a");
        assert_eq!(
            app.conversation.reading.as_ref().map(|(id, _)| id.as_str()),
            Some(next_id.as_str())
        );
        app.conversation_response(
            delayed,
            Ok(ApiPayload::Conversation(Box::new(view(
                "chat-a", "current", 4,
            )))),
            &context,
        );
        assert_eq!(
            app.conversation.reading.as_ref().map(|(id, _)| id.as_str()),
            Some(next_id.as_str())
        );
        assert_eq!(
            app.conversation.select_after_load.as_deref(),
            Some("new-task")
        );
        assert!(app.conversation.pending.is_none());

        app.conversation_response(
            ApiRequest::Conversation {
                id: next_id.clone(),
                generation: app.conversation.reading.as_ref().unwrap().1,
            },
            Ok(ApiPayload::Conversation(Box::new(view(
                &next_id, "current", 0,
            )))),
            &context,
        );
        assert!(app.conversation.reading.is_none());
        assert!(app.conversation.select_after_load.is_none());
        let Some(ApiRequest::ConversationSelect(selection)) = &app.conversation.pending else {
            panic!("the new conversation must select its requested task after loading");
        };
        assert_eq!(selection.conversation_id, next_id);
        assert_eq!(selection.task_id, "new-task");
        assert_eq!(selection.expected_revision, 0);
    }
    #[test]
    fn returning_to_same_conversation_rejects_its_previous_read_generation() {
        let mut app = super::super::tests::test_app();
        let context = egui::Context::default();
        app.session = Some(session("current"));
        app.conversation.open = true;
        app.open_saved_conversation("chat-a".into(), &context);
        let old_generation = app.conversation.reading.as_ref().unwrap().1;
        let delayed = ApiRequest::Conversation {
            id: "chat-a".into(),
            generation: old_generation,
        };

        app.open_saved_conversation("chat-b".into(), &context);
        app.open_saved_conversation("chat-a".into(), &context);
        let current_owner = app.conversation.reading.clone().unwrap();
        assert_eq!(current_owner.0, "chat-a");
        assert!(current_owner.1 > old_generation);
        for response in [
            Ok(ApiPayload::Conversation(Box::new(view(
                "chat-a", "current", 99,
            )))),
            Err(ApiFailure::Other("Old A connection failed".into())),
            Err(ApiFailure::SessionChanged("Old A session failed".into())),
        ] {
            app.conversation_response(delayed.clone(), response, &context);
            assert_eq!(app.conversation.reading.as_ref(), Some(&current_owner));
            assert!(app.conversation.view.is_none());
            assert!(app.conversation.error.is_none());
            assert!(
                app.session
                    .as_ref()
                    .unwrap()
                    .matches(&view("chat-a", "current", 0).service)
            );
        }
        app.conversation_response(
            ApiRequest::Conversation {
                id: "chat-a".into(),
                generation: current_owner.1,
            },
            Ok(ApiPayload::Conversation(Box::new(view(
                "chat-a", "current", 2,
            )))),
            &context,
        );
        assert!(app.conversation.reading.is_none());
        assert_eq!(app.conversation.view.as_ref().unwrap().revision, 2);
    }
}
