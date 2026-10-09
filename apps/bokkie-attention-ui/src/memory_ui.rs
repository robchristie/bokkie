//! Inspectable recall entries, separate from role and execution settings.
use super::*;
use bokkie_operator_api::{
    MemoryCommandRequest, MemoryEntry, MemoryKind, MemoryList, MemoryMutation, MemoryProvenance,
    MemorySource,
};

#[derive(Clone)]
struct MemoryDraft {
    original: Option<MemoryEntry>,
    kind: MemoryKind,
    provenance: MemoryProvenance,
    content: String,
    source_reference: String,
    source_context: String,
    confirm_remove: bool,
}
impl Default for MemoryDraft {
    fn default() -> Self {
        Self {
            original: None,
            kind: MemoryKind::Preference,
            provenance: MemoryProvenance::Explicit,
            content: String::new(),
            source_reference: "Settings".into(),
            source_context: "Preference entered by the operator".into(),
            confirm_remove: false,
        }
    }
}
impl MemoryDraft {
    fn from_entry(entry: &MemoryEntry) -> Self {
        Self {
            original: Some(entry.clone()),
            kind: entry.kind,
            provenance: entry.provenance,
            content: entry.content.clone().unwrap_or_default(),
            ..Self::default()
        }
    }
    fn request(&self, remove: bool) -> Result<MemoryCommandRequest, String> {
        let valid = |s: &str, max| {
            !s.trim().is_empty()
                && s.len() <= max
                && !s.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
        };
        let mutation = if remove {
            if self.original.is_none() || !self.confirm_remove {
                return Err("Review and confirm the selected memory removal.".into());
            }
            MemoryMutation::Remove
        } else {
            if !valid(&self.content, 2048) {
                return Err("Enter memory content within 2 KiB.".into());
            }
            if self.original.is_some() {
                MemoryMutation::Correct {
                    content: self.content.clone(),
                }
            } else {
                if !valid(&self.source_reference, 256) || !valid(&self.source_context, 512) {
                    return Err(
                        "Add a source reference and the supporting context or observation.".into(),
                    );
                }
                MemoryMutation::Create {
                    kind: self.kind,
                    provenance: self.provenance,
                    content: self.content.clone(),
                    sources: vec![MemorySource {
                        reference: self.source_reference.clone(),
                        context: self.source_context.clone(),
                    }],
                    task_id: None,
                }
            }
        };
        Ok(MemoryCommandRequest {
            command_id: uuid::Uuid::new_v4().to_string(),
            entry_id: self.original.as_ref().map(|e| e.id.clone()),
            expected_revision: self.original.as_ref().map_or(0, |e| e.revision),
            mutation,
        })
    }
}

#[derive(Default)]
pub(super) struct MemoryState {
    pub open: bool,
    entries: Vec<MemoryEntry>,
    next_after: Option<String>,
    reading: Option<(u64, Option<String>, bool)>,
    draft: Option<MemoryDraft>,
    pending: Option<MemoryCommandRequest>,
    current: bool,
    busy: bool,
    conflict: bool,
    error: Option<String>,
    notice: Option<String>,
}
impl MemoryState {
    pub(super) fn reset_session(&mut self) {
        self.current = false;
        self.busy = false;
        self.reading = None;
    }
    fn accept(&mut self, list: MemoryList, append: bool, replace: bool) {
        if append {
            self.entries.extend(list.entries);
        } else {
            self.entries = list.entries;
        }
        self.next_after = list.next_after;
        self.current = true;
        if replace {
            self.draft = None;
            self.conflict = false;
        } else if let Some(original) = self.draft.as_ref().and_then(|d| d.original.as_ref()) {
            self.conflict = self
                .entries
                .iter()
                .find(|e| e.id == original.id)
                .is_none_or(|e| e.revision != original.revision);
        }
    }
    fn command(&mut self, remove: bool) -> Result<MemoryCommandRequest, String> {
        if self.busy || self.reading.is_some() || !self.current {
            return Err("Read current memory before saving.".into());
        }
        if let Some(request) = &self.pending {
            return Ok(request.clone());
        }
        if self.conflict {
            return Err("Memory changed elsewhere. Reload before saving.".into());
        }
        let request = self
            .draft
            .as_ref()
            .ok_or("Choose or add a memory entry.")?
            .request(remove)?;
        self.pending = Some(request.clone());
        Ok(request)
    }
    fn failed(&mut self, error: &ApiFailure) {
        self.busy = false;
        self.error = Some(error.to_string());
        if matches!(error, ApiFailure::Conflict(_)) {
            self.conflict = true;
            self.pending = None;
        } else if matches!(error, ApiFailure::Rejected(_)) {
            self.pending = None;
        }
    }
}
pub(super) fn is_request(request: &ApiRequest) -> bool {
    matches!(
        request,
        ApiRequest::Memory { .. } | ApiRequest::SaveMemory(_)
    )
}

impl AttentionApp {
    pub(super) fn open_memory(&mut self, context: &egui::Context) {
        self.memory.open = true;
        self.refresh_memory(None, false, context);
    }
    pub(super) fn refresh_open_memory(&mut self, context: &egui::Context) {
        if self.agent_settings.open && self.memory.open {
            // A new session needs a current read before an uncertain command can
            // be retried. Retain its exact receipt and the operator's draft.
            self.refresh_memory(None, false, context);
        }
    }
    fn refresh_memory(&mut self, after: Option<String>, replace: bool, context: &egui::Context) {
        if self.session.is_none()
            || self.transport.is_none()
            || self.memory.reading.is_some()
            || self.memory.busy
            || (replace && self.memory.pending.is_some())
        {
            return;
        }
        let generation = self.fresh_generation();
        self.memory.reading = Some((generation, after.clone(), replace));
        self.dispatch(ApiRequest::Memory { after, generation }, context);
    }
    pub(super) fn memory_response(
        &mut self,
        request: ApiRequest,
        result: Result<ApiPayload, ApiFailure>,
        context: &egui::Context,
    ) {
        let read = match &request {
            ApiRequest::Memory { generation, after } => {
                let Some((expected, cursor, replace)) = self.memory.reading.clone() else {
                    return;
                };
                if expected != *generation || cursor != *after {
                    return;
                }
                self.memory.reading = None;
                Some((after.is_some(), replace))
            }
            ApiRequest::SaveMemory(save) => {
                if self.memory.pending.as_ref() != Some(save) {
                    return;
                }
                self.memory.busy = false;
                None
            }
            _ => return,
        };
        match result {
            Ok(ApiPayload::Memory(list)) if read.is_some() => {
                if self
                    .session
                    .as_ref()
                    .is_none_or(|s| !s.matches(&list.service))
                {
                    return;
                }
                let (append, replace) = read.unwrap();
                self.memory.accept(list, append, replace);
                self.memory.error = None;
            }
            Ok(ApiPayload::MemorySaved(saved)) if read.is_none() => {
                if self
                    .session
                    .as_ref()
                    .is_none_or(|s| !s.matches(&saved.service))
                {
                    return;
                }
                self.memory.pending = None;
                self.memory.draft = None;
                self.memory.conflict = false;
                self.memory.current = false;
                self.memory.notice = Some(if saved.entry.removed { "Removed from recall. Its source stays marked so the same outcome cannot recreate it." } else { "Saved. New requests can use this memory when relevant." }.into());
                self.refresh_memory(None, true, context);
            }
            Err(error) => {
                if read.is_none() {
                    self.memory.failed(&error);
                } else {
                    self.memory.current = false;
                    self.memory.error = Some(error.to_string());
                }
                if matches!(error, ApiFailure::SessionChanged(_)) {
                    self.restart_session(&error.to_string(), context);
                }
            }
            _ => {
                self.memory.current = false;
                self.memory.error = Some("Bokkie returned an unexpected memory response.".into());
            }
        }
    }
    pub(super) fn show_memory(
        &mut self,
        ui: &mut egui::Ui,
        tokens: DesignTokens,
        nodes: &mut Vec<UiNode>,
        text: &mut Vec<TextLayoutObservation>,
    ) {
        let bounds = ui.available_rect_before_wrap();
        let width = bounds.width().min(660.0);
        let rect = bounds.shrink2(egui::vec2((bounds.width() - width) / 2.0, 0.0));
        let mut back = false;
        let mut reload = false;
        let mut load_more = false;
        let mut save = false;
        let mut remove = false;
        let connected = self.session.is_some();
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            let mut p = PresentationContext::new(ui, tokens, self.preferences.font_scale, PresentationScope::new("bokkie.memory"), SemanticUiId::new("bokkie.memory"));
            let state = &mut self.memory;
            p.heading(ui, "heading", "Memory");
            back = button(ui, "back", "Back to Settings", true, &mut p);
            memory_text(ui, "scope", "Useful preferences, decisions and observed knowledge. Current requests and project-owned knowledge take precedence. Memory does not change permissions or execution settings.", &mut p);
            let remaining = ui.available_rect_before_wrap();
            let footer = egui::Rect::from_min_max(egui::pos2(remaining.left(), (remaining.bottom() - 112.0).max(remaining.top())), remaining.max);
            let content = egui::Rect::from_min_max(remaining.min, egui::pos2(remaining.right(), (footer.top() - 8.0).max(remaining.top())));
            let ready = state.current && connected && !state.busy && state.reading.is_none();
            ui.scope_builder(egui::UiBuilder::new().max_rect(content), |ui| {
                ui.set_clip_rect(content.intersect(ui.clip_rect()));
                egui::ScrollArea::vertical().id_salt("memory-scroll").auto_shrink([false, false]).show(ui, |ui| {
                    if state.reading.is_some() { memory_text(ui, "loading", "Reading memory…", &mut p); }
                    if !state.current || !connected { memory_text(ui, "stale", "Reload memory when connected before making changes.", &mut p); }
                    if button(ui, "add", "Add memory", ready && state.pending.is_none(), &mut p) { state.draft = Some(MemoryDraft::default()); state.conflict = false; state.notice = None; }
                    if state.entries.is_empty() && state.current { memory_text(ui, "empty", "No saved memory. Accepted workspace outcomes are recalled when their task is selected.", &mut p); }
                    for entry in &state.entries {
                        let summary = entry.content.as_deref().unwrap_or_default().chars().take(72).collect::<String>();
                        if button(ui, &format!("entry-{}", entry.id), &format!("{} · {}", kind_label(entry.kind), summary), ready && state.pending.is_none(), &mut p) { state.draft = Some(MemoryDraft::from_entry(entry)); state.conflict = false; state.notice = None; }
                    }
                    if state.next_after.is_some() { load_more = button(ui, "more", "Load more memory", ready && state.pending.is_none(), &mut p); }
                    if let Some(draft) = &mut state.draft {
                        ui.separator();
                        ui.add_enabled_ui(ready && !state.conflict && state.pending.is_none(), |ui| {
                            if let Some(original) = &draft.original {
                                memory_text(ui, "revision", &format!("{} · {} · revision {}{}", kind_label(original.kind), provenance_label(original.provenance), original.revision, if original.corrected { " · corrected by you" } else { "" }), &mut p);
                                for (index, source) in original.sources.iter().enumerate() {
                                    memory_text(ui, &format!("source-{index}"), &format!("Source: {}\n{}", source.reference, source.context), &mut p);
                                }
                                if let Some(task) = &original.task_id { memory_text(ui, "task", &format!("Task context: {task}"), &mut p); }
                                memory_text(ui, "correction-help", "Corrections retain the original provenance and sources. Task results and history stay unchanged.", &mut p);
                            } else {
                                ui.horizontal_wrapped(|ui| {
                                    for kind in [MemoryKind::Preference, MemoryKind::TaskOutcome, MemoryKind::Decision, MemoryKind::OperationalKnowledge] {
                                        let response = ui.selectable_value(&mut draft.kind, kind, kind_label(kind));
                                        control(ui, &response, &format!("kind-{}", kind_label(kind)), kind_label(kind), &mut p);
                                        if response.changed() && kind != MemoryKind::Preference && draft.source_context == "Preference entered by the operator" {
                                            draft.source_reference.clear();
                                            draft.source_context.clear();
                                        }
                                    }
                                });
                                ui.horizontal_wrapped(|ui| {
                                    for provenance in [MemoryProvenance::Explicit, MemoryProvenance::Inferred] {
                                        let response = ui.selectable_value(&mut draft.provenance, provenance, provenance_label(provenance));
                                        control(ui, &response, &format!("provenance-{}", provenance_label(provenance)), provenance_label(provenance), &mut p);
                                    }
                                });
                                field(ui, "source-reference", "Source reference", &mut draft.source_reference, 1, &mut p);
                                field(ui, "source-context", "Source context or observation", &mut draft.source_context, 3, &mut p);
                            }
                            field(ui, "content", "Memory content", &mut draft.content, 5, &mut p);
                            if draft.original.is_some() {
                                let response = ui.checkbox(&mut draft.confirm_remove, "Confirm removal of this memory from future recall");
                                control(ui, &response, "confirm-remove", "Confirm memory removal", &mut p);
                            }
                        });
                    }
                    if let Some(error) = &state.error { memory_text(ui, "error", &format!("Memory needs attention: {error}"), &mut p); }
                    if state.conflict { memory_text(ui, "conflict", "This memory changed elsewhere. Your correction is retained. Reload memory deliberately to inspect the current entry.", &mut p); }
                    if state.pending.is_some() && !state.busy { memory_text(ui, "uncertain", "The result is uncertain. Retry the exact saved request to reconcile it.", &mut p); }
                });
            });
            ui.scope_builder(egui::UiBuilder::new().max_rect(footer), |ui| {
                ui.set_clip_rect(footer.intersect(ui.clip_rect()));
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    let valid = state.draft.as_ref().is_some_and(|d| d.request(false).is_ok());
                    save = button(ui, "save", if state.busy { "Saving…" } else if state.pending.is_some() { "Retry saved request" } else { "Save memory" }, ready && (state.pending.is_some() || (!state.conflict && valid)), &mut p);
                    let can_remove = state.draft.as_ref().is_some_and(|d| d.request(true).is_ok());
                    remove = button(ui, "remove", "Remove memory", ready && state.pending.is_none() && !state.conflict && can_remove, &mut p);
                    reload = button(ui, "reload", "Reload memory", connected && !state.busy && state.reading.is_none() && state.pending.is_none(), &mut p);
                });
                if let Some(notice) = &state.notice { memory_text(ui, "notice", notice, &mut p); }
            });
            let observations = p.finish(ui);
            nodes.extend(observations.semantic_nodes);
            text.extend(observations.text_layouts);
        });
        let mut node = UiNode::container(
            SemanticUiId::new("bokkie.memory"),
            Some(SemanticUiId::root()),
            UiRole::Section,
            rect.into(),
        );
        node.name = "Memory".into();
        nodes.push(node);
        let context = ui.ctx().clone();
        if back {
            self.memory.open = false;
        }
        if reload {
            self.refresh_memory(None, true, &context);
        }
        if load_more {
            self.refresh_memory(self.memory.next_after.clone(), false, &context);
        }
        if save || remove {
            self.submit_memory(remove, &context);
        }
    }
    fn submit_memory(&mut self, remove: bool, context: &egui::Context) {
        match self.memory.command(remove) {
            Ok(request) => {
                self.memory.busy = true;
                self.memory.error = None;
                self.memory.notice = None;
                self.dispatch(ApiRequest::SaveMemory(request), context);
            }
            Err(error) => self.memory.error = Some(error),
        }
    }
}
fn kind_label(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Preference => "Preference",
        MemoryKind::TaskOutcome => "Task outcome",
        MemoryKind::Decision => "Decision",
        MemoryKind::OperationalKnowledge => "Operational knowledge",
    }
}
fn provenance_label(provenance: MemoryProvenance) -> &'static str {
    match provenance {
        MemoryProvenance::Explicit => "Explicit",
        MemoryProvenance::Inferred => "Inferred",
    }
}
fn memory_text(ui: &mut egui::Ui, key: &str, value: &str, p: &mut PresentationContext) {
    // The shared presentation contract supports eight lines per label. Render
    // source context in bounded consecutive labels so it remains inspectable.
    let characters = value.chars().collect::<Vec<_>>();
    for (index, chunk) in characters.chunks(128).enumerate() {
        memory_text_part(
            ui,
            &format!("{key}-{index}"),
            &chunk.iter().collect::<String>(),
            p,
        );
    }
}
fn memory_text_part(ui: &mut egui::Ui, key: &str, value: &str, p: &mut PresentationContext) {
    let response = p.content(
        ui,
        key,
        value,
        ContentTextSpec {
            role: TextRole::Body,
            overflow: TextOverflow::Wrap,
            max_lines: 8,
            interaction: TextInteraction::Selectable,
        },
    );
    let rect = response.interact_rect.intersect(ui.clip_rect());
    if rect.is_positive() {
        let mut node = UiNode::container(
            SemanticUiId::new(format!("bokkie.memory.{key}")),
            Some(SemanticUiId::new("bokkie.memory")),
            UiRole::Section,
            rect.into(),
        );
        node.name = value.into();
        p.observe_node(ui, node);
    }
}
fn control(
    ui: &egui::Ui,
    response: &egui::Response,
    key: &str,
    label: &str,
    p: &mut PresentationContext,
) {
    let rect = response.interact_rect.intersect(ui.clip_rect());
    if !rect.is_positive() {
        return;
    }
    let mut node = UiNode::container(
        SemanticUiId::new(format!("bokkie.memory.{key}")),
        Some(SemanticUiId::new("bokkie.memory")),
        if key == "content" || key.starts_with("source-") {
            UiRole::Section
        } else {
            UiRole::Button
        },
        rect.into(),
    );
    node.name = label.into();
    node.enabled = response.enabled();
    node.focused = response.has_focus();
    p.observe_node(ui, node);
}
fn button(
    ui: &mut egui::Ui,
    key: &str,
    label: &str,
    enabled: bool,
    p: &mut PresentationContext,
) -> bool {
    let response = ui
        .push_id(key, |ui| {
            p.native(ui, NativeTextControlKind::Button, |ui| {
                (ui.add_enabled(enabled, egui::Button::new(label).wrap()), ())
            })
        })
        .inner
        .0;
    control(ui, &response, key, label, p);
    response.clicked()
}
fn field(
    ui: &mut egui::Ui,
    key: &str,
    label: &str,
    value: &mut String,
    rows: usize,
    p: &mut PresentationContext,
) {
    memory_text(ui, &format!("{key}-label"), label, p);
    let response = ui.add(
        egui::TextEdit::multiline(value)
            .id_salt(("memory", key))
            .desired_rows(rows)
            .desired_width(f32::INFINITY),
    );
    control(ui, &response, key, label, p);
}

#[cfg(test)]
#[path = "memory_ui/tests.rs"]
mod tests;
