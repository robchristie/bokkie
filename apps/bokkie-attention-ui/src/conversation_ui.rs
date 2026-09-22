use super::*;
use bokkie_operator_api::{
    ConversationAction, ConversationConfirmRequest, ConversationSelectRequest, ConversationSummary,
    ConversationTurnRequest, ConversationView, ManagedCatalogueEntry, ManagedTaskDefinition,
    ManagedTaskDetail, ManagedTrigger,
};

#[derive(Default)]
pub(super) struct ConversationState {
    pub open: bool,
    id: Option<String>,
    view: Option<ConversationView>,
    history: Vec<ConversationSummary>,
    catalogue: Vec<ManagedCatalogueEntry>,
    query: String,
    applied_query: String,
    next_after: Option<String>,
    text: String,
    pending: Option<ApiRequest>,
    in_flight: bool,
    reading: Option<(String, u64)>,
    catalogue_busy: bool,
    error: Option<String>,
    select_after_load: Option<String>,
    poll_at: Option<Instant>,
}

impl ConversationState {
    fn switch(&mut self, id: String, task: Option<String>) {
        self.id = Some(id);
        self.view = None;
        self.text.clear();
        self.pending = None;
        self.reading = None;
        self.error = None;
        self.select_after_load = task;
        self.poll_at = None;
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
        self.poll_at = view.busy.then(|| Instant::now() + Duration::from_secs(1));
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
            | ApiRequest::ConversationConfirm(_)
            | ApiRequest::Catalogue { .. }
    )
}

impl AttentionApp {
    pub(super) fn open_conversation(&mut self, task: Option<String>, context: &egui::Context) {
        if task.is_some() && (self.conversation.pending.is_some() || self.conversation.in_flight) {
            return;
        }
        self.conversation.open = true;
        if task.is_some() || self.conversation.id.is_none() {
            self.conversation
                .switch(uuid::Uuid::new_v4().to_string(), task);
        }
        self.refresh_conversation(context);
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
                | ApiRequest::ConversationConfirm(_)
        );
        let request_id = match &request {
            ApiRequest::Conversation { id, .. } => Some(id.as_str()),
            ApiRequest::ConversationTurn(r) => Some(r.conversation_id.as_str()),
            ApiRequest::ConversationSelect(r) => Some(r.conversation_id.as_str()),
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
                if let ApiRequest::Catalogue { query, after } = request
                    && query == self.conversation.applied_query
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
        if !self.conversation.open || self.session.is_none() {
            return;
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
                "bokkie.conversation.back",
                "Attention desk",
                true,
                nodes,
            ) {
                state.open = false;
            }
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
                "bokkie.conversation.engineering",
                "Engineering intake",
                true,
                nodes,
            ) {
                intents.push(OperatorIntent::ComposeEngineering);
            }
        });
        ui.label("Describe a task, find existing work, or refine its instructions and timing.");
        egui::ScrollArea::vertical().id_salt("conversation-workspace").show(ui, |ui| {
            egui::CollapsingHeader::new("Recent conversations").show(ui, |ui| {
                for summary in &state.history {
                    let label = if summary.last_text.is_empty() { "Untitled conversation" } else { &summary.last_text };
                    if button(ui, &format!("bokkie.conversation.history.{}", summary.id), &label.chars().take(90).collect::<String>(), !state.in_flight && state.pending.is_none(), nodes) { action = Some(ConversationUiAction::Open(summary.id.clone())); }
                }
                if state.history.is_empty() { ui.label("Your saved conversations will appear here."); }
            });
            egui::CollapsingHeader::new("Find tasks and drafts").default_open(state.view.as_ref().is_none_or(|v| v.messages.is_empty())).show(ui, |ui| {
                let response = ui.add(egui::TextEdit::singleline(&mut state.query).desired_width(f32::INFINITY).hint_text("Search all tasks by name or description"));
                observe(response.rect, "bokkie.conversation.search", "Search all tasks", UiRole::Section, true, nodes);
                if button(ui, "bokkie.conversation.search-submit", "Search tasks", !state.catalogue_busy, nodes) { action = Some(ConversationUiAction::Search); }
                if state.catalogue_busy { ui.label("Searching…"); }
                egui::ScrollArea::vertical().id_salt("conversation-catalogue").max_height(240.0).show(ui, |ui| {
                    for entry in &state.catalogue {
                        catalogue_row(ui, entry, "catalogue", mutable, nodes, &mut action);
                    }
                });
                if state.next_after.is_some() && button(ui, "bokkie.conversation.more", "More tasks", !state.catalogue_busy, nodes) { action = Some(ConversationUiAction::More); }
            });
            ui.separator();
            if let Some(view) = &state.view {
                if !view.runtime_available { ui.label("Conversation runtime unavailable. Existing tasks and saved conversations remain readable."); }
                if !view.notes_available { ui.label("Local note execution is unavailable in this runtime."); }
                if let Some(task_id) = &view.selected_task_id {
                    let name = view.task.as_ref().and_then(|t| t.candidate.as_ref().or(t.active.as_ref())).map(|r| r.definition.name.as_str()).unwrap_or(task_id);
                    let response = ui.label(egui::RichText::new(format!("Selected task: {name}")).strong());
                    observe(response.rect, "bokkie.conversation.selected", &format!("Selected task: {name}"), UiRole::Section, true, nodes);
                    if view.task.is_none() && button(ui, "bokkie.conversation.legacy-open", "Open existing task details and actions", true, nodes) { action = Some(ConversationUiAction::Legacy(task_id.clone())); }
                } else { ui.label("No task selected. Choose a result explicitly when several tasks match."); }
                for (index, message) in view.messages.iter().enumerate() {
                    egui::Frame::group(ui.style()).inner_margin(12.0).show(ui, |ui| {
                        ui.label(egui::RichText::new(if message.role == "user" { "You" } else { "Bokkie" }).strong());
                        let response = ui.add(egui::Label::new(&message.text).wrap().selectable(true));
                        observe(response.rect, &format!("bokkie.conversation.message.{index}"), &message.text, UiRole::Section, true, nodes);
                    });
                    ui.add_space(6.0);
                }
                if !view.candidates.is_empty() { ui.label("Choose the task you mean:"); }
                for candidate in &view.candidates { catalogue_row(ui, candidate, "candidate", mutable, nodes, &mut action); }
                if let Some(task) = &view.task {
                    task_detail(ui, task, nodes);
                    if button(ui, "bokkie.conversation.preview", "Preview this task", mutable && view.runtime_available, nodes) { action = Some(ConversationUiAction::Preview); }
                }
                if let Some(review) = &view.review {
                    egui::Frame::group(ui.style()).inner_margin(12.0).show(ui, |ui| {
                        ui.heading(match review.action {
                            ConversationAction::Activate => "Activate task",
                            ConversationAction::Pause => "Pause future runs",
                            ConversationAction::Resume => "Resume future runs",
                        });
                        ui.label(&review.explanation);

                        for blocker in &review.blockers { ui.label(format!("Unavailable: {blocker}")); }
                        if let Some(preview) = &review.preview {
                            definition(ui, &preview.definition);
                            for change in &preview.changes { ui.label(format!("Change: {change}")); }
                            for blocker in &preview.blockers { ui.label(format!("Unavailable: {blocker}")); }
                            for occurrence in &preview.occurrences { ui.label(format!("Scheduled: {}", local_time_in_zone(*occurrence, trigger_timezone(&preview.definition.trigger)))); }
                        }
                        egui::CollapsingHeader::new("Review provenance")
                            .id_salt(("review-provenance", &review.id))
                            .show(ui, |ui| {
                                ui.label(format!("Task ID: {}", review.task_id));
                                ui.label(format!("Configuration revision: {}", review.configuration_revision));
                                ui.label(format!("Review ID: {}", review.id));
                                ui.label(format!("Process session: {}", review.session_id));
                                if let Some(preview) = &review.preview {
                                    ui.label(format!("Definition revision: {}", preview.candidate_revision));
                                    ui.label(format!("Capability profile revision: {}", preview.profile_revision));
                                }
                            });
                        let session_current = self.session.as_ref().is_some_and(|s| s.session_id() == review.session_id);
                        let revision_current = view.task.as_ref().is_some_and(|task| task.configuration_revision == review.configuration_revision);
                        let enabled = mutable && review_is_current(view, self.session.as_ref());
                        if !session_current { ui.label("This review belongs to an earlier session. Ask Bokkie for a fresh review."); }
                        if !revision_current { ui.label("This task has changed since this review. Ask Bokkie for a fresh preview before another action."); }
                        if button(ui, "bokkie.conversation.confirm", "Confirm reviewed action", enabled, nodes) { action = Some(ConversationUiAction::Confirm); }
                        ui.label("Confirmation saves this change. Run completion and local results appear separately in the task history.");
                    });
                }
                if let Some(receipt) = &view.receipt {
                    ui.label("Change saved. Run completion and local results appear separately in the task history.");
                    egui::CollapsingHeader::new("Saved receipt details").id_salt(("receipt-provenance", &receipt.command_id)).show(ui, |ui| {
                        ui.label(format!("Task ID: {}", receipt.task_id));
                        ui.label(format!("Configuration revision: {}", receipt.configuration_revision));
                        ui.label(format!("Command ID: {}", receipt.command_id));
                    });
                }
                if let Some(error) = &view.request_error { ui.label(format!("Bokkie could not complete this turn: {error}")); }
                if busy { ui.spinner(); ui.label("Bokkie is preparing a response. This conversation is saved."); }
            } else { ui.label("Loading conversation…"); }
            if let Some(error) = &state.error { ui.label(format!("Request needs attention: {error}")); }
            ui.add_space(12.0);
            let response = ui.add_enabled(state.pending.is_none() && !state.in_flight, egui::TextEdit::multiline(&mut state.text).id_salt("conversation-text").desired_rows(4).desired_width(f32::INFINITY).hint_text("What would you like Bokkie to do?"));
            observe(response.rect, "bokkie.conversation.text", "Message to Bokkie", UiRole::Section, response.enabled(), nodes);
            let available = state.view.as_ref().is_some_and(|v| v.runtime_available);
            if state.pending.is_some() {
                ui.label("The request may already be saved. Retry retains its original identity.");
                if button(ui, "bokkie.conversation.retry", "Retry saved request", safe && !busy, nodes) { action = Some(ConversationUiAction::Retry); }
            } else if button(ui, "bokkie.conversation.send", "Send message", mutable && available && !state.text.trim().is_empty() && state.text.chars().count() <= 8_192, nodes) { action = Some(ConversationUiAction::Send); }
        });
        match action {
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
            Some(ConversationUiAction::Search | ConversationUiAction::More) => {
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
                    },
                    &context,
                );
            }
            Some(ConversationUiAction::Select(id)) => self.select_conversation_task(id, &context),
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
                if let Some(view) = &self.conversation.view {
                    let request = ApiRequest::ConversationTurn(ConversationTurnRequest {
                        command_id: engineering_command_id(),
                        conversation_id: view.id.clone(),
                        expected_revision: view.revision,
                        text: if matches!(action, Some(ConversationUiAction::Preview)) {
                            "Preview this task".into()
                        } else {
                            self.conversation.text.clone()
                        },
                    });
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
            None => {}
        }
    }
}

enum ConversationUiAction {
    New,
    Open(String),
    Search,
    More,
    Select(String),
    Legacy(String),
    Send,
    Preview,
    Retry,
    Confirm,
}
fn observe(
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
fn button(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    enabled: bool,
    nodes: &mut Vec<UiNode>,
) -> bool {
    let navigation = matches!(
        id,
        "bokkie.conversation.back" | "bokkie.conversation.new" | "bokkie.conversation.engineering"
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
    observe(response.rect, id, label, UiRole::Button, enabled, nodes);
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
        &format!("{} · {} · {}", entry.name, entry.kind, entry.status),
        enabled,
        nodes,
    ) {
        *action = Some(ConversationUiAction::Select(entry.id.clone()));
    }
    ui.add(egui::Label::new(&entry.description).wrap());
}
fn definition(ui: &mut egui::Ui, value: &ManagedTaskDefinition) {
    ui.label(egui::RichText::new(&value.name).strong());
    ui.add(egui::Label::new(&value.purpose).wrap().selectable(true));
    ui.add(
        egui::Label::new(&value.instructions)
            .wrap()
            .selectable(true),
    );
    ui.label(match &value.trigger {
        ManagedTrigger::Immediate => "Timing: once, immediately after confirmation".into(),
        ManagedTrigger::Once {
            local_datetime,
            timezone,
        } => format!("Timing: {local_datetime} ({timezone})"),
        ManagedTrigger::Recurring { cron, timezone } => recurring_description(cron, timezone),
    });
    ui.label(match value.capability.as_str() {
        "local_note" => "Capability: Local note",
        _ => "This task requires a capability that is unavailable.",
    });
    ui.label(match value.destination.as_str() {
        "task_results" => "Results: In-app task results",
        _ => "This task requires a result destination that is unavailable.",
    });
    for effect in &value.effects {
        ui.label(match effect.as_str() {
            "store_local_result" => "Save the supplied text as a local result",
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

fn review_is_current(view: &ConversationView, session: Option<&ApiSession>) -> bool {
    let Some(review) = &view.review else {
        return false;
    };
    session.is_some_and(|session| {
        session.matches(&view.service) && session.session_id() == review.session_id
    }) && !view.busy
        && view.selected_task_id.as_deref() == Some(&review.task_id)
        && view.task.as_ref().is_some_and(|task| {
            task.id == review.task_id
                && task.configuration_revision == review.configuration_revision
        })
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
    ui.label(format!("Task status: {:?}", task.status));
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
        egui::CollapsingHeader::new(format!(
            "{} · {} · revision {}",
            run.state,
            local_time(run.scheduled_at),
            run.definition_revision
        ))
        .default_open(run.result.is_some())
        .show(ui, |ui| {
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
            } else {
                ui.label("No local result yet.");
            }
        });
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
fn local_time(seconds: i64) -> String {
    local_time_in_zone(seconds, "Australia/Adelaide")
}
fn local_time_in_zone(seconds: i64, timezone: &str) -> String {
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
            notes_available: true,
            receipt: None,
        }
    }
    #[test]
    fn retained_turn_reconciles_only_its_durable_message() {
        let request = ApiRequest::ConversationTurn(ConversationTurnRequest {
            command_id: "command-1".into(),
            conversation_id: "chat".into(),
            expected_revision: 0,
            text: "retain me".into(),
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
        assert!(local_time(summer).contains("10:30 ACDT"));
        assert!(local_time(winter).contains("09:30 ACST"));
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
        assert!(!review_is_current(&changed, Some(&session("current"))));
        changed = current;
        changed
            .review
            .as_mut()
            .unwrap()
            .blockers
            .push("Unavailable".into());
        assert!(!review_is_current(&changed, Some(&session("current"))));
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
