//! Project destinations and immutable brief snapshots; this surface never starts execution.
use super::*;
use bokkie_operator_api::{
    HandoffActivityKind, HandoffActivityRequest, HandoffDraft, HandoffSaveRequest, HandoffSnapshot,
    HandoffView, ProjectDestination, ProjectList, ProjectRegistration, ProjectSaveRequest,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Serialize, Deserialize)]
struct BriefEditor {
    draft: HandoffDraft,
    selected: Option<ProjectDestination>,
    references: String,
}
impl BriefEditor {
    fn new(draft: HandoffDraft) -> Self {
        // Ambiguous matches always need a deliberate choice; no first-row default.
        let selected = (draft.candidates.len() == 1).then(|| draft.candidates[0].clone());
        let references = draft.brief.references.join("\n");
        Self {
            draft,
            selected,
            references,
        }
    }
    fn request(&self) -> Result<HandoffSaveRequest, String> {
        let project = self
            .selected
            .as_ref()
            .ok_or("Choose the intended project workspace.")?;
        if self.draft.brief.outcome.trim().is_empty() {
            return Err("Describe the requested outcome.".into());
        }
        let mut brief = self.draft.brief.clone();
        brief.references = self
            .references
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect();
        Ok(HandoffSaveRequest {
            command_id: engineering_command_id(),
            draft_id: self.draft.id.clone(),
            expected_revision: self.draft.saved_revision,
            project_id: project.id.clone(),
            project_revision: project.revision,
            brief,
        })
    }
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
enum Pending {
    Project(ProjectSaveRequest),
    Brief(HandoffSaveRequest),
    Activity(HandoffActivityRequest),
}
impl Pending {
    fn request(&self) -> ApiRequest {
        match self {
            Self::Project(v) => ApiRequest::SaveProject(v.clone()),
            Self::Brief(v) => ApiRequest::SaveHandoff(v.clone()),
            Self::Activity(v) => ApiRequest::HandoffActivity(v.clone()),
        }
    }
}
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Retained {
    editors: BTreeMap<String, BriefEditor>,
    editor: Option<String>,
    project_editor: Option<ProjectSaveRequest>,
    pending: Option<Pending>,
    selected: Option<(String, i64)>,
    note: String,
}
#[derive(Default)]
pub(super) struct HandoffState {
    pub(super) open: bool,
    projects_open: bool,
    projects: Option<ProjectList>,
    projects_current: bool,
    list: Vec<HandoffSnapshot>,
    next_after: Option<String>,
    view: Option<HandoffView>,
    view_current: bool,
    projects_read: Option<u64>,
    list_read: Option<u64>,
    view_read: Option<(String, Option<i64>, u64)>,
    retained: Retained,
    in_flight: bool,
    error: Option<String>,
    guidance: bool,
    copy_status: String,
    #[cfg(target_arch = "wasm32")]
    last_persisted: Option<String>,
    storage_error: Option<String>,
}
impl HandoffState {
    pub(super) fn observe_draft(&mut self, draft: HandoffDraft) {
        // Saved snapshots belong to the server. A read must never recreate an
        // obsolete model brief after an operator has saved an edited revision.
        if draft.saved_revision > 0 && !self.retained.editors.contains_key(&draft.id) {
            return;
        }
        self.retained
            .editors
            .entry(draft.id.clone())
            .and_modify(|editor| editor.draft.candidates = draft.candidates.clone())
            .or_insert_with(|| BriefEditor::new(draft));
    }
    fn use_current_project_revision(&mut self) {
        if !self.projects_current {
            return;
        }
        if let Some(editor) = &mut self.retained.project_editor
            && let Some(current) = self
                .projects
                .as_ref()
                .and_then(|p| p.items.iter().find(|p| p.id == editor.project_id))
        {
            editor.expected_revision = current.revision;
            self.error = None;
        }
    }
    pub(super) fn reset_session(&mut self) {
        self.projects_current = false;
        self.view_current = false;
        self.projects_read = None;
        self.list_read = None;
        self.view_read = None;
        self.in_flight = false;
        // Exact pending commands and all entered text survive a changed session.
    }
}
pub(super) fn is_request(request: &ApiRequest) -> bool {
    matches!(
        request,
        ApiRequest::Projects { .. }
            | ApiRequest::SaveProject(_)
            | ApiRequest::Handoffs { .. }
            | ApiRequest::Handoff { .. }
            | ApiRequest::SaveHandoff(_)
            | ApiRequest::HandoffActivity(_)
    )
}

impl AttentionApp {
    pub(super) fn open_handoffs(&mut self, context: &egui::Context) {
        self.agent_settings.open = false;
        self.handoff.open = true;
        self.handoff.projects_open = false;
        self.refresh_handoff(context);
    }
    pub(super) fn open_project_workspaces(&mut self, context: &egui::Context) {
        self.open_handoffs(context);
        self.handoff.projects_open = true;
    }
    pub(super) fn open_handoff_draft(&mut self, draft: HandoffDraft, context: &egui::Context) {
        if draft.saved_revision > 0 {
            self.handoff.retained.editor = None;
            self.handoff.retained.selected = Some((draft.id.clone(), draft.saved_revision));
            self.open_handoffs(context);
            return;
        }
        self.handoff.retained.editor = Some(draft.id.clone());
        self.handoff.observe_draft(draft);
        self.open_handoffs(context);
    }
    pub(super) fn refresh_handoff(&mut self, context: &egui::Context) {
        if !self.handoff.open || self.session.is_none() {
            return;
        }
        if self.handoff.projects_read.is_none() {
            let generation = self.fresh_generation();
            self.handoff.projects_read = Some(generation);
            self.dispatch(ApiRequest::Projects { generation }, context);
        }
        if self.handoff.list_read.is_none() {
            self.read_handoff_list(None, context);
        }
        if self.handoff.view_read.is_none()
            && let Some((id, revision)) = self.handoff.retained.selected.clone()
        {
            self.read_handoff(id, Some(revision), context);
        }
    }
    fn read_handoff_list(&mut self, after: Option<String>, context: &egui::Context) {
        let generation = self.fresh_generation();
        self.handoff.list_read = Some(generation);
        self.dispatch(ApiRequest::Handoffs { after, generation }, context);
    }
    fn read_handoff(&mut self, id: String, revision: Option<i64>, context: &egui::Context) {
        let generation = self.fresh_generation();
        self.handoff.view_read = Some((id.clone(), revision, generation));
        self.handoff.view_current = false;
        if self.handoff.view.as_ref().is_some_and(|v| {
            v.snapshot.id != id || revision.is_some_and(|r| r != v.snapshot.revision)
        }) {
            self.handoff.view = None;
        }
        if let Some(revision) = revision {
            self.handoff.retained.selected = Some((id.clone(), revision));
        }
        self.handoff.guidance = false;
        self.handoff.copy_status.clear();
        self.dispatch(
            ApiRequest::Handoff {
                id,
                revision,
                generation,
            },
            context,
        );
    }
    fn submit_handoff(&mut self, pending: Pending, context: &egui::Context) {
        if self.handoff.in_flight || self.session.is_none() {
            return;
        }
        self.handoff.retained.pending = Some(pending.clone());
        self.handoff.in_flight = true;
        self.handoff.error = None;
        // Persist the exact envelope before sending; a lost response can only retry this command.
        self.persist_handoff_local_state();
        self.dispatch(pending.request(), context);
    }
    fn handoff_activity(
        &mut self,
        kind: HandoffActivityKind,
        note: String,
        context: &egui::Context,
    ) {
        let Some(view) = self.handoff.view.as_ref() else {
            return;
        };
        if !self.handoff.view_current || self.handoff.retained.pending.is_some() {
            return;
        }
        self.submit_handoff(
            Pending::Activity(HandoffActivityRequest {
                command_id: engineering_command_id(),
                handoff_id: view.snapshot.id.clone(),
                revision: view.snapshot.revision,
                kind,
                note,
            }),
            context,
        );
    }
    pub(super) fn handoff_response(
        &mut self,
        request: ApiRequest,
        result: Result<ApiPayload, ApiFailure>,
        context: &egui::Context,
    ) {
        let mutation = matches!(
            request,
            ApiRequest::SaveProject(_)
                | ApiRequest::SaveHandoff(_)
                | ApiRequest::HandoffActivity(_)
        );
        match &request {
            ApiRequest::Projects { generation }
                if self.handoff.projects_read == Some(*generation) =>
            {
                self.handoff.projects_read = None
            }
            ApiRequest::Handoffs { generation, .. }
                if self.handoff.list_read == Some(*generation) =>
            {
                self.handoff.list_read = None
            }
            ApiRequest::Handoff {
                id,
                revision,
                generation,
            } if self.handoff.view_read.as_ref() == Some(&(id.clone(), *revision, *generation)) => {
                self.handoff.view_read = None
            }
            _ if mutation
                && self
                    .handoff
                    .retained
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.request() == request) =>
            {
                self.handoff.in_flight = false
            }
            _ => return,
        }
        let identity_matches = match &result {
            Ok(ApiPayload::Projects(v)) => {
                self.session.as_ref().is_some_and(|s| s.matches(&v.service))
            }
            Ok(ApiPayload::Handoffs(v)) => {
                self.session.as_ref().is_some_and(|s| s.matches(&v.service))
            }
            Ok(ApiPayload::Handoff(v)) => {
                self.session.as_ref().is_some_and(|s| s.matches(&v.service))
            }
            Err(_) => true,
            _ => false,
        };
        if !identity_matches {
            self.handoff.error =
                Some("Hand-off response could not be verified for the current service.".into());
            return;
        }
        match result {
            Ok(ApiPayload::Projects(v)) => {
                self.handoff.projects_current = true;
                self.handoff.projects = Some(v);
                if mutation {
                    self.handoff.retained.pending = None;
                    self.handoff.retained.project_editor = None;
                }
            }
            Ok(ApiPayload::Handoffs(v)) => {
                if matches!(request, ApiRequest::Handoffs { after: None, .. }) {
                    self.handoff.list.clear();
                }
                for item in v.items {
                    if !self.handoff.list.iter().any(|old| old.id == item.id) {
                        self.handoff.list.push(item);
                    }
                }
                self.handoff.next_after = v.next_after;
            }
            Ok(ApiPayload::Handoff(v)) => {
                if let ApiRequest::Handoff { id, revision, .. } = &request
                    && (id != &v.snapshot.id || revision.is_some_and(|r| r != v.snapshot.revision))
                {
                    self.handoff.error =
                        Some("Bokkie returned a different hand-off revision.".into());
                    return;
                }
                if let ApiRequest::SaveHandoff(save) = &request {
                    self.handoff.retained.editors.remove(&save.draft_id);
                    self.handoff.retained.editor = None;
                    self.read_handoff_list(None, context);
                }
                if let ApiRequest::HandoffActivity(activity) = &request {
                    if v.snapshot.id != activity.handoff_id
                        || v.snapshot.revision != activity.revision
                    {
                        self.handoff.error =
                            Some("Bokkie returned a different activity record or revision.".into());
                        return;
                    }
                    // A settled request belongs to its submitted record. Navigation
                    // and newer note text must survive a delayed response or retry.
                    if self.handoff.retained.selected.as_ref()
                        != Some(&(activity.handoff_id.clone(), activity.revision))
                    {
                        self.handoff.retained.pending = None;
                        return;
                    }
                    if activity.kind == HandoffActivityKind::ResultNote
                        && self.handoff.retained.note == activity.note
                    {
                        self.handoff.retained.note.clear();
                    }
                }
                self.handoff.retained.selected = Some((v.snapshot.id.clone(), v.snapshot.revision));
                self.handoff.view = Some(*v);
                self.handoff.view_current = true;
                if mutation {
                    self.handoff.retained.pending = None;
                }
            }
            Err(error) => {
                self.handoff.error = Some(match &error {
                    ApiFailure::Other(detail) if detail.contains("Failed to fetch") => {
                        "Bokkie did not confirm this request. Your text is retained.".into()
                    }
                    _ => error.to_string(),
                });
                if mutation && matches!(error, ApiFailure::Rejected(_) | ApiFailure::Conflict(_)) {
                    self.handoff.retained.pending = None;
                }
                if matches!(request, ApiRequest::Projects { .. }) {
                    self.handoff.projects_current = false;
                }
                if matches!(request, ApiRequest::Handoff { .. }) {
                    self.handoff.view_current = false;
                }
                if matches!(error, ApiFailure::Conflict(_))
                    && let ApiRequest::SaveHandoff(save) = &request
                {
                    self.read_handoff(save.draft_id.clone(), None, context);
                }
                if matches!(error, ApiFailure::SessionChanged(_)) {
                    self.restart_session(&error.to_string(), context);
                }
            }
            _ => self.handoff.error = Some("Unexpected project or hand-off response.".into()),
        }
    }

    pub(super) fn show_handoff(&mut self, ui: &mut egui::Ui, nodes: &mut Vec<UiNode>) {
        let context = ui.ctx().clone();
        let bounds = ui.available_rect_before_wrap();
        let width = bounds.width().min(780.0);
        let rect = bounds.shrink2(egui::vec2((bounds.width() - width) / 2.0, 0.0));
        let mut pane = UiNode::container(
            SemanticUiId::new("bokkie.handoff"),
            Some(SemanticUiId::root()),
            UiRole::Section,
            rect.into(),
        );
        pane.name = "Project workspace hand-offs".into();
        nodes.push(pane);
        let mut action = None;
        let mutable = self.session.is_some()
            && !self.handoff.in_flight
            && self.handoff.retained.pending.is_none();
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.heading(if self.handoff.projects_open { "Project workspaces" } else { "Project hand-offs" });
            ui.horizontal_wrapped(|ui| {
                if handoff_button(ui, "return", "Return to conversation", true, nodes) { action = Some(UiAction::Home); }
                if handoff_button(ui, "history", "Saved hand-offs", true, nodes) { action = Some(UiAction::History); }
                if !self.handoff.projects_open && handoff_button(ui, "projects", "Project workspaces", true, nodes) { action = Some(UiAction::Projects); }
            });
            ui.add_space(8.0);
            let remaining = ui.available_rect_before_wrap();
            let footer_height = if self.handoff.error.is_some() || self.handoff.retained.pending.is_some() { 180.0 } else if rect.width() < 500.0 { 136.0 } else { 112.0 };
            let footer = egui::Rect::from_min_max(egui::pos2(remaining.left(), (remaining.bottom() - footer_height).max(remaining.top())), remaining.max);
            let content = egui::Rect::from_min_max(remaining.min, egui::pos2(remaining.right(), (footer.top() - 8.0).max(remaining.top())));
            ui.scope_builder(egui::UiBuilder::new().max_rect(content), |ui| {
                ui.set_clip_rect(content.intersect(ui.clip_rect()));
                egui::ScrollArea::vertical().id_salt(("handoff-content", self.handoff.projects_open, &self.handoff.retained.editor, &self.handoff.retained.selected))
                    .auto_shrink([false, false]).show(ui, |ui| {
                    if self.handoff.projects_open {
                        self.handoff_project_body(ui, nodes, mutable, &mut action);
                    } else if let Some(id) = self.handoff.retained.editor.clone() {
                        self.handoff_editor_body(ui, nodes, &id, mutable, &mut action);
                    } else if self.handoff.retained.selected.is_some() {
                        self.handoff_saved_body(ui, nodes, mutable, &mut action);
                    } else {
                        ui.label("Prepare development work in conversation, then choose a workspace and save its brief.");
                        for editor in self.handoff.retained.editors.values() {
                            if handoff_button(ui, &format!("draft.{}", editor.draft.id), &format!("Draft: {}", editor.draft.brief.outcome), true, nodes) { action = Some(UiAction::Draft(editor.draft.id.clone())); }
                        }
                        ui.add_space(10.0);
                        ui.strong("Saved briefs");
                        if self.handoff.list.is_empty() { ui.label("No saved hand-offs yet."); }
                        for snapshot in &self.handoff.list {
                            let label = format!("{}\n{} · {} · revision {}", snapshot.brief.outcome, snapshot.project.registration.name, snapshot.project.registration.host, snapshot.revision);
                            if handoff_button(ui, &format!("saved.{}", snapshot.id), &label, true, nodes) { action = Some(UiAction::Saved(snapshot.id.clone(), snapshot.revision)); }
                        }
                        if self.handoff.next_after.is_some() && handoff_button(ui, "more", "More hand-offs", self.handoff.list_read.is_none(), nodes) { action = Some(UiAction::More); }
                    }
                });
            });
            ui.scope_builder(egui::UiBuilder::new().max_rect(footer), |ui| {
                ui.set_clip_rect(footer.intersect(ui.clip_rect()));
                ui.separator();
                if let Some(error) = &self.handoff.storage_error { ui.label(error); }
                if let Some(error) = &self.handoff.error { ui.label(format!("Needs attention: {error}")); }
                if self.handoff.in_flight { ui.label("Saving…"); }
                else if self.handoff.retained.pending.is_some() {
                    ui.label("The result is uncertain. Retry the exact request to reconcile it.");
                    if handoff_button(ui, "retry", "Retry exact request", self.session.is_some(), nodes) { action = Some(UiAction::Retry); }
                } else {
                    ui.horizontal_wrapped(|ui| {
                        if self.handoff.projects_open && self.handoff.retained.project_editor.is_some() {
                            if handoff_button(ui, "project-save", "Save workspace", mutable && self.handoff.projects_current, nodes) { action = Some(UiAction::SaveProject); }
                            if handoff_button(ui, "project-cancel", "Close editor", true, nodes) { action = Some(UiAction::CloseProject); }
                        } else if let Some(id) = &self.handoff.retained.editor {
                            let valid = self.handoff.retained.editors.get(id).is_some_and(|e| e.request().is_ok());
                            if handoff_button(ui, "save", "Save hand-off", mutable && self.handoff.projects_current && valid, nodes) { action = Some(UiAction::SaveBrief); }
                            if handoff_button(ui, "keep-draft", "Keep draft and view saved", true, nodes) { action = Some(UiAction::History); }
                        } else if self.handoff.view.is_some() && self.handoff.retained.selected.is_some() && !self.handoff.projects_open {
                            if handoff_button(ui, "copy", "Copy complete brief", mutable && self.handoff.view_current, nodes) { action = Some(UiAction::Copy); }
                            if handoff_button(ui, "opening", "How to open workspace", mutable && self.handoff.view_current, nodes) { action = Some(UiAction::Opening); }
                        }
                        if handoff_button(ui, "refresh", "Refresh", self.session.is_some() && !self.handoff.in_flight, nodes) { action = Some(UiAction::Refresh); }
                    });
                    if !self.handoff.copy_status.is_empty() { ui.label(&self.handoff.copy_status); }
                }
            });
        });
        match action {
            Some(UiAction::Home) => self.open_conversation(None, &context),
            Some(UiAction::Projects) => self.open_project_workspaces(&context),
            Some(UiAction::History) => {
                self.handoff.retained.editor = None;
                self.handoff.retained.selected = None;
                self.handoff.view = None;
                self.handoff.projects_open = false;
                self.refresh_handoff(&context);
            }
            Some(UiAction::Draft(id)) => {
                self.handoff.retained.editor = Some(id);
            }
            Some(UiAction::Saved(id, revision)) => {
                self.handoff.retained.editor = None;
                self.read_handoff(id, Some(revision), &context);
            }
            Some(UiAction::More) => {
                self.read_handoff_list(self.handoff.next_after.clone(), &context)
            }
            Some(UiAction::Refresh) => self.refresh_handoff(&context),
            Some(UiAction::NewProject) => {
                self.handoff.retained.project_editor = Some(ProjectSaveRequest {
                    command_id: String::new(),
                    project_id: uuid::Uuid::new_v4().to_string(),
                    expected_revision: 0,
                    registration: ProjectRegistration::default(),
                })
            }
            Some(UiAction::EditProject(project)) => {
                self.handoff.retained.project_editor = Some(ProjectSaveRequest {
                    command_id: String::new(),
                    project_id: project.id,
                    expected_revision: project.revision,
                    registration: project.registration,
                })
            }
            Some(UiAction::CloseProject) => self.handoff.retained.project_editor = None,
            Some(UiAction::SaveProject) => {
                if let Some(mut save) = self.handoff.retained.project_editor.clone() {
                    save.command_id = engineering_command_id();
                    self.submit_handoff(Pending::Project(save), &context);
                }
            }
            Some(UiAction::SaveBrief) => {
                let result = self
                    .handoff
                    .retained
                    .editor
                    .as_ref()
                    .and_then(|id| self.handoff.retained.editors.get(id))
                    .map(BriefEditor::request);
                match result {
                    Some(Ok(save)) => self.submit_handoff(Pending::Brief(save), &context),
                    Some(Err(e)) => self.handoff.error = Some(e),
                    None => {}
                }
            }
            Some(UiAction::Retry) => {
                if let Some(pending) = self.handoff.retained.pending.clone() {
                    self.submit_handoff(pending, &context);
                }
            }
            Some(UiAction::RebaseProject) => self.handoff.use_current_project_revision(),
            Some(UiAction::RebaseBrief(revision)) => {
                if let Some(editor) = self
                    .handoff
                    .retained
                    .editor
                    .as_ref()
                    .and_then(|id| self.handoff.retained.editors.get_mut(id))
                {
                    editor.draft.saved_revision = revision;
                    self.handoff.error = None;
                }
            }
            Some(UiAction::EditSaved) => {
                if let Some(v) = &self.handoff.view {
                    let draft = HandoffDraft {
                        id: v.snapshot.id.clone(),
                        conversation_id: v.snapshot.conversation_id.clone(),
                        source_request_id: v.snapshot.source_request_id.clone(),
                        project_query: v.snapshot.project.registration.name.clone(),
                        brief: v.snapshot.brief.clone(),
                        candidates: vec![v.snapshot.project.clone()],
                        saved_revision: v.latest_revision,
                    };
                    self.handoff.retained.editor = Some(draft.id.clone());
                    self.handoff
                        .retained
                        .editors
                        .entry(draft.id.clone())
                        .or_insert_with(|| BriefEditor::new(draft));
                }
            }
            Some(UiAction::Copy) => self.copy_saved_brief(&context),
            Some(UiAction::Opening) => {
                self.handoff.guidance = true;
                self.handoff_activity(
                    HandoffActivityKind::ManualOpeningViewed,
                    String::new(),
                    &context,
                );
            }
            Some(UiAction::Result) => self.handoff_activity(
                HandoffActivityKind::ResultNote,
                self.handoff.retained.note.clone(),
                &context,
            ),
            Some(UiAction::Problem) => self.handoff_activity(
                HandoffActivityKind::OpeningProblemReported,
                self.handoff.retained.note.clone(),
                &context,
            ),
            None => {}
        }
    }
    fn handoff_project_body(
        &mut self,
        ui: &mut egui::Ui,
        nodes: &mut Vec<UiNode>,
        mutable: bool,
        action: &mut Option<UiAction>,
    ) {
        ui.label("Register an existing Codex project. The host and absolute workspace path identify where you will open it.");
        if let Some(editor) = self.handoff.retained.project_editor.as_mut() {
            ui.add_enabled_ui(mutable, |ui| {
                handoff_field(
                    ui,
                    nodes,
                    "project-name",
                    "Project name",
                    &mut editor.registration.name,
                    1,
                );
                handoff_field(
                    ui,
                    nodes,
                    "project-host",
                    "Host",
                    &mut editor.registration.host,
                    1,
                );
                handoff_field(
                    ui,
                    nodes,
                    "project-workspace",
                    "Absolute workspace path",
                    &mut editor.registration.workspace,
                    1,
                );
                let mut project_id = editor
                    .registration
                    .codex_project_id
                    .clone()
                    .unwrap_or_default();
                handoff_field(
                    ui,
                    nodes,
                    "project-codex-id",
                    "Codex project ID (optional)",
                    &mut project_id,
                    1,
                );
                editor.registration.codex_project_id =
                    (!project_id.trim().is_empty()).then_some(project_id);
                let mut host_id = editor
                    .registration
                    .codex_host_id
                    .clone()
                    .unwrap_or_default();
                handoff_field(
                    ui,
                    nodes,
                    "project-codex-host",
                    "Codex host ID (optional)",
                    &mut host_id,
                    1,
                );
                editor.registration.codex_host_id = (!host_id.trim().is_empty()).then_some(host_id);
                handoff_field(
                    ui,
                    nodes,
                    "project-context",
                    "Project context",
                    &mut editor.registration.context,
                    3,
                );
                if let Some(current) = self.handoff.projects.as_ref().and_then(|p| p.items.iter().find(|p| p.id == editor.project_id))
                    && current.revision != editor.expected_revision {
                    ui.separator();
                    ui.label(format!("Currently saved revision {}: {} · {}\n{}", current.revision, current.registration.name, current.registration.host, current.registration.workspace));
                    ui.label("Review this saved identity against your entered fields. Using its revision retains all of your entered values.");
                    if handoff_button(ui, "project-rebase", "Use current registration revision", mutable && self.handoff.projects_current, nodes) { *action = Some(UiAction::RebaseProject); }
                }

            });
        } else {
            if handoff_button(
                ui,
                "project-new",
                "Add workspace",
                mutable && self.handoff.projects_current,
                nodes,
            ) {
                *action = Some(UiAction::NewProject);
            }
            if let Some(projects) = &self.handoff.projects {
                for project in &projects.items {
                    if handoff_button(
                        ui,
                        &format!("project-edit.{}", project.id),
                        &format!(
                            "{} · {}\n{}",
                            project.registration.name,
                            project.registration.host,
                            project.registration.workspace
                        ),
                        mutable,
                        nodes,
                    ) {
                        *action = Some(UiAction::EditProject(project.clone()));
                    }
                }
                if projects.items.is_empty() {
                    ui.label("No project workspaces are registered.");
                }
            }
        }
    }
    fn handoff_editor_body(
        &mut self,
        ui: &mut egui::Ui,
        nodes: &mut Vec<UiNode>,
        id: &str,
        mutable: bool,
        action: &mut Option<UiAction>,
    ) {
        let Some(editor) = self.handoff.retained.editors.get_mut(id) else {
            return;
        };
        ui.label("Choose the workspace and review the brief before saving.");
        ui.label(format!("Workspace query: {}", editor.draft.project_query));
        let candidates = self
            .handoff
            .projects
            .as_ref()
            .map(|p| &p.items)
            .unwrap_or(&editor.draft.candidates);
        ui.strong("Project workspace");
        if editor.draft.candidates.is_empty() {
            ui.label("No destination matched this request. Register the intended existing project using Project workspaces above, then return to conversation to refresh the matches. You can also deliberately choose another registered workspace below.");
        } else if editor.selected.is_none() {
            ui.label("Choose the intended project, host and workspace path.");
        }
        if let Some(current) = &self.handoff.view
            && self.handoff.view_current
            && current.snapshot.id == editor.draft.id
            && current.snapshot.revision == current.latest_revision
            && current.latest_revision != editor.draft.saved_revision
        {
            ui.separator();
            ui.label(format!(
                "Latest saved revision {}: {}",
                current.latest_revision, current.snapshot.brief.outcome
            ));
            ui.add(
                egui::Label::new(&current.snapshot.complete_brief)
                    .selectable(true)
                    .wrap(),
            );
            ui.label("Review the latest saved brief before using its revision as the baseline for your retained edits.");
            if handoff_button(
                ui,
                "brief-rebase",
                "Use current saved revision",
                mutable,
                nodes,
            ) {
                *action = Some(UiAction::RebaseBrief(current.latest_revision));
            }
        }
        ui.add_enabled_ui(mutable, |ui| {
            for project in candidates {
                let selected = editor
                    .selected
                    .as_ref()
                    .is_some_and(|p| p.id == project.id && p.revision == project.revision);
                let label = format!(
                    "{}{} · {}\n{}",
                    if selected { "Selected: " } else { "" },
                    project.registration.name,
                    project.registration.host,
                    project.registration.workspace
                );
                if handoff_button(ui, &format!("choose.{}", project.id), &label, true, nodes) {
                    editor.selected = Some(project.clone());
                }
            }
            if let Some(project) = &editor.selected {
                ui.label(format!(
                    "Destination: {} on {}",
                    project.registration.name, project.registration.host
                ));
                ui.add(
                    egui::Label::new(&project.registration.workspace)
                        .selectable(true)
                        .wrap(),
                );
                if !project.registration.context.is_empty() {
                    ui.label(&project.registration.context);
                }
            }
            handoff_field(
                ui,
                nodes,
                "outcome",
                "Requested outcome",
                &mut editor.draft.brief.outcome,
                2,
            );
            handoff_field(
                ui,
                nodes,
                "context",
                "Relevant context",
                &mut editor.draft.brief.context,
                3,
            );
            handoff_field(
                ui,
                nodes,
                "constraints",
                "Constraints",
                &mut editor.draft.brief.constraints,
                2,
            );
            handoff_field(
                ui,
                nodes,
                "acceptance",
                "Acceptance criteria",
                &mut editor.draft.brief.acceptance,
                2,
            );
            handoff_field(
                ui,
                nodes,
                "references",
                "References (one per line)",
                &mut editor.references,
                2,
            );
        });
    }
    fn handoff_saved_body(
        &mut self,
        ui: &mut egui::Ui,
        nodes: &mut Vec<UiNode>,
        mutable: bool,
        action: &mut Option<UiAction>,
    ) {
        let Some(view) = &self.handoff.view else {
            ui.label("Loading saved brief…");
            return;
        };
        let snapshot = &view.snapshot;
        ui.strong(format!("Saved brief · revision {}", snapshot.revision));
        ui.label(format!(
            "Created {}",
            conversation_ui::local_time_in_zone(snapshot.created_at, "Australia/Adelaide")
        ));
        ui.label(format!(
            "{} · {}",
            snapshot.project.registration.name, snapshot.project.registration.host
        ));
        ui.add(
            egui::Label::new(&snapshot.project.registration.workspace)
                .selectable(true)
                .wrap(),
        );
        if !self.handoff.view_current {
            ui.label(
                "Retained brief: refresh to verify the current service before recording activity.",
            );
        }
        ui.label(
            "Prepared for manual opening. No execution acknowledgement is available to Bokkie.",
        );
        if snapshot.revision < view.latest_revision
            && handoff_button(ui, "latest", "View latest revision", true, nodes)
        {
            *action = Some(UiAction::Saved(snapshot.id.clone(), view.latest_revision));
        }
        if let Some(previous) = view.previous_revision
            && handoff_button(ui, "previous", "View previous revision", true, nodes)
        {
            *action = Some(UiAction::Saved(snapshot.id.clone(), previous));
        }
        if handoff_button(
            ui,
            "edit",
            "Edit as a new revision",
            mutable && self.handoff.view_current && snapshot.revision == view.latest_revision,
            nodes,
        ) {
            *action = Some(UiAction::EditSaved);
        }
        ui.separator();
        ui.add(
            egui::Label::new(&snapshot.complete_brief)
                .selectable(true)
                .wrap(),
        );
        if self.handoff.guidance {
            ui.separator();
            ui.strong("Open the existing project in Codex");
            ui.label(format!(
                "1. In Codex, select the existing project {} on host {}.",
                snapshot.project.registration.name, snapshot.project.registration.host
            ));
            ui.label(format!(
                "2. Confirm its workspace is {}.",
                snapshot.project.registration.workspace
            ));
            ui.label("3. Start a fresh chat and paste the complete saved brief. The project’s own workflow decides the next action.");
            ui.label(
                "4. Use the return link in the brief to come back to this exact saved revision.",
            );
            if let Some(id) = &snapshot.project.registration.codex_project_id {
                ui.label(format!("Codex project identity: {id}"));
            }
            if let Some(id) = &snapshot.project.registration.codex_host_id {
                ui.label(format!("Codex host identity: {id}"));
            }
        }
        ui.separator();
        ui.strong("Operator-entered result note");
        ui.label("Record what you observed in the workspace. This note does not establish execution or verified completion.");
        handoff_field(
            ui,
            nodes,
            "note",
            "Result or opening problem",
            &mut self.handoff.retained.note,
            3,
        );
        ui.horizontal_wrapped(|ui| {
            let enabled = mutable
                && self.handoff.view_current
                && !self.handoff.retained.note.trim().is_empty();
            if handoff_button(ui, "result", "Save result note", enabled, nodes) {
                *action = Some(UiAction::Result);
            }
            if handoff_button(ui, "problem", "Report opening problem", enabled, nodes) {
                *action = Some(UiAction::Problem);
            }
        });
        for (index, activity) in view.activities.iter().enumerate() {
            let label = match activity.kind {
                HandoffActivityKind::CopyRequested => {
                    "Clipboard copy requested; platform delivery unverified"
                }
                HandoffActivityKind::CopySucceeded => "Browser reported brief copied",
                HandoffActivityKind::CopyFailed => "Copy unavailable — selectable brief shown",
                HandoffActivityKind::ManualOpeningViewed => "Manual opening guidance viewed",
                HandoffActivityKind::OpeningProblemReported => "Operator-entered opening problem",
                HandoffActivityKind::ResultNote => "Operator-entered result note",
            };
            let text = if activity.note.is_empty() {
                label.to_owned()
            } else {
                format!("{label}: {}", activity.note)
            };
            let text = format!(
                "{text}\n{} · {}",
                activity.provenance,
                conversation_ui::local_time_in_zone(activity.created_at, "Australia/Adelaide")
            );
            let response = ui.add(egui::Label::new(&text).selectable(true).wrap());
            handoff_observe(
                response.rect.intersect(ui.clip_rect()),
                &format!("bokkie.handoff.activity.{index}"),
                &text,
                UiRole::Section,
                true,
                nodes,
            );
        }
    }
}

enum UiAction {
    Home,
    Projects,
    History,
    Draft(String),
    Saved(String, i64),
    More,
    Refresh,
    NewProject,
    EditProject(ProjectDestination),
    CloseProject,
    SaveProject,
    SaveBrief,
    Retry,
    RebaseProject,
    RebaseBrief(i64),
    EditSaved,
    Copy,
    Opening,
    Result,
    Problem,
}
fn handoff_button(
    ui: &mut egui::Ui,
    key: &str,
    label: &str,
    enabled: bool,
    nodes: &mut Vec<UiNode>,
) -> bool {
    let single_line = !label.contains('\n');
    if single_line && ui.layout().is_horizontal() {
        let width = ui
            .painter()
            .layout_no_wrap(
                label.into(),
                egui::TextStyle::Button.resolve(ui.style()),
                ui.visuals().text_color(),
            )
            .size()
            .x
            + 2.0 * ui.spacing().button_padding.x;
        if ui.available_size_before_wrap().x < width {
            ui.end_row();
        }
    }
    let response = ui
        .push_id(("handoff", key), |ui| {
            ui.add_enabled(
                enabled,
                egui::Button::new(label).wrap_mode(if single_line {
                    egui::TextWrapMode::Extend
                } else {
                    egui::TextWrapMode::Wrap
                }),
            )
        })
        .inner;
    record_native_text_control(&response, NativeTextControlKind::Button);
    handoff_observe(
        response.rect.intersect(ui.clip_rect()),
        &format!("bokkie.handoff.{key}"),
        label,
        UiRole::Button,
        enabled && ui.is_enabled(),
        nodes,
    );
    response.clicked()
}
fn handoff_field(
    ui: &mut egui::Ui,
    nodes: &mut Vec<UiNode>,
    key: &str,
    label: &str,
    value: &mut String,
    rows: usize,
) {
    ui.label(label);
    let edit = if rows == 1 {
        egui::TextEdit::singleline(value)
    } else {
        egui::TextEdit::multiline(value).desired_rows(rows)
    };
    let response = ui.add(
        edit.id_salt(("handoff-field", key))
            .desired_width(f32::INFINITY),
    );
    let visible = response.rect.intersect(ui.clip_rect());
    if visible.is_positive() {
        handoff_observe(
            visible,
            &format!("bokkie.handoff.{key}"),
            label,
            UiRole::Section,
            ui.is_enabled(),
            nodes,
        );
    }
    ui.add_space(4.0);
}

fn handoff_observe(
    rect: egui::Rect,
    id: &str,
    name: &str,
    role: UiRole,
    enabled: bool,
    nodes: &mut Vec<UiNode>,
) {
    if !rect.is_positive() {
        return;
    }
    let mut node = UiNode::container(
        SemanticUiId::new(id),
        Some(SemanticUiId::new("bokkie.handoff")),
        role,
        rect.into(),
    );
    node.name = name.into();
    node.enabled = enabled;
    nodes.push(node);
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(inline_js = "
export function handoff_load() { return window.__BOKKIE_HANDOFF?.load() ?? null; }
export function handoff_store(value) { return window.__BOKKIE_HANDOFF?.store(value) ?? false; }
export function handoff_link() { return window.__BOKKIE_HANDOFF?.takeLink() ?? null; }
export function handoff_prepare(value) { window.__BOKKIE_HANDOFF?.prepareCopy(value); }
export function handoff_copy() { window.__BOKKIE_HANDOFF?.copy(); }
export function handoff_copy_result() { return window.__BOKKIE_HANDOFF?.takeCopyResult() ?? null; }
")]
extern "C" {
    fn handoff_load() -> Option<String>;
    fn handoff_store(value: &str) -> bool;
    fn handoff_link() -> Option<String>;
    fn handoff_prepare(value: &str);
    fn handoff_copy();
    fn handoff_copy_result() -> Option<String>;
}
#[cfg(target_arch = "wasm32")]
#[derive(Serialize, Deserialize)]
struct LocalState {
    conversation_id: Option<String>,
    unsent: String,
    handoff: Retained,
}
impl AttentionApp {
    pub(super) fn restore_handoff_local_state(&mut self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(value) = handoff_load()
            && value.len() <= 262_144
            && let Ok(saved) = serde_json::from_str::<LocalState>(&value)
        {
            self.conversation.id = saved.conversation_id;
            self.conversation.text = saved.unsent;
            self.handoff.retained = saved.handoff;
            self.handoff.last_persisted = Some(value);
        }
    }
    pub(super) fn persist_handoff_local_state(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {
            // Tokens and fetched transcripts never enter this local draft record.
            let value = serde_json::json!({ "conversation_id": self.conversation.id, "unsent": self.conversation.text, "handoff": self.handoff.retained }).to_string();
            if self.handoff.last_persisted.as_ref() == Some(&value) {
                return;
            }
            if value.len() > 262_144 {
                self.handoff.storage_error = Some("Local drafts exceed the storage limit. Keep this window open until you save the current brief.".into());
                return;
            }
            if handoff_store(&value) {
                self.handoff.last_persisted = Some(value);
                self.handoff.storage_error = None;
            } else {
                self.handoff.storage_error = Some("Local draft storage is unavailable. Keep this window open until you save the brief.".into());
            }
        }
    }
    pub(super) fn drive_handoff_browser(&mut self, context: &egui::Context) {
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(value) = handoff_link()
                && let Ok((id, revision)) = serde_json::from_str::<(String, i64)>(&value)
            {
                self.handoff.retained.editor = None;
                self.handoff.retained.selected = Some((id.clone(), revision));
                self.open_handoffs(context);
                if self.session.is_some() {
                    self.read_handoff(id, Some(revision), context);
                }
            }
            if let Some(view) = &self.handoff.view {
                handoff_prepare(&serde_json::json!({ "id": view.snapshot.id, "revision": view.snapshot.revision, "text": view.snapshot.complete_brief }).to_string());
            }
            if !self.handoff.in_flight
                && self.handoff.retained.pending.is_none()
                && let Some(value) = handoff_copy_result()
                && let Ok((id, revision, success)) =
                    serde_json::from_str::<(String, i64, bool)>(&value)
                && self
                    .handoff
                    .view
                    .as_ref()
                    .is_some_and(|v| v.snapshot.id == id && v.snapshot.revision == revision)
            {
                self.handoff.copy_status = if success { "Browser reported the complete brief copied." } else { "Copy was unavailable. Select and copy the complete brief in the open text panel." }.into();
                self.handoff_activity(
                    if success {
                        HandoffActivityKind::CopySucceeded
                    } else {
                        HandoffActivityKind::CopyFailed
                    },
                    String::new(),
                    context,
                );
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = context;
    }
    fn copy_saved_brief(&mut self, context: &egui::Context) {
        #[cfg(target_arch = "wasm32")]
        {
            handoff_copy();
            let _ = context;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(view) = &self.handoff.view {
            context.copy_text(view.snapshot.complete_brief.clone());
            self.handoff.copy_status = "Copy requested. Clipboard delivery cannot be verified here; the complete brief above remains selectable.".into();
            self.handoff_activity(
                HandoffActivityKind::CopyRequested,
                "Platform clipboard delivery could not be verified".into(),
                context,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn service() -> bokkie_operator_api::ServiceIdentity {
        bokkie_operator_api::ServiceIdentity {
            build: bokkie_operator_api::BOKKIE_BUILD_ID.into(),
            api_contract_version: bokkie_operator_api::API_CONTRACT_VERSION,
            schema_version: bokkie_operator_api::SUPPORTED_SCHEMA_VERSION,
            process_id: 42,
            session_id: "current".into(),
        }
    }
    fn project(id: &str, host: &str) -> ProjectDestination {
        ProjectDestination {
            id: id.into(),
            revision: 3,
            registration: ProjectRegistration {
                name: "Atlas".into(),
                host: host.into(),
                workspace: format!("/work/{host}/atlas"),
                ..Default::default()
            },
        }
    }
    fn draft() -> HandoffDraft {
        HandoffDraft {
            id: "10000000-0000-4000-8000-000000000001".into(),
            conversation_id: "chat".into(),
            source_request_id: "request".into(),
            project_query: "Atlas".into(),
            brief: bokkie_operator_api::HandoffBrief {
                outcome: "Add a searchable project list".into(),
                ..Default::default()
            },
            candidates: vec![project("one", "Nostromo"), project("two", "Sulaco")],
            saved_revision: 0,
        }
    }
    fn app() -> AttentionApp {
        let mut app = super::super::tests::test_app();
        app.session = Some(
            ApiSession::from_bootstrap(bokkie_operator_api::SessionBootstrap {
                service: service(),
                mutation_token: "a".repeat(64),
            })
            .unwrap(),
        );
        app.handoff.projects = Some(ProjectList {
            service: service(),
            items: draft().candidates,
        });
        app.handoff.projects_current = true;
        app.handoff.observe_draft(draft());
        app.handoff.retained.editor = Some(draft().id);
        app
    }
    #[test]
    fn ambiguous_destinations_require_selection_and_save_pins_the_exact_registration() {
        let mut editor = BriefEditor::new(draft());
        assert!(editor.selected.is_none());
        assert!(editor.request().is_err());
        editor.selected = Some(editor.draft.candidates[1].clone());
        editor.references =
            "https://example.test/requirements\n\nhttps://example.test/design".into();
        let request = editor.request().unwrap();
        assert_eq!(
            (request.project_id.as_str(), request.project_revision),
            ("two", 3)
        );
        assert_eq!(request.expected_revision, 0);
        assert_eq!(request.brief.references.len(), 2);
    }
    #[test]
    fn refresh_and_session_restart_keep_unsent_text_brief_edits_and_the_exact_pending_command() {
        let mut app = app();
        app.conversation.text = "Do not send this message".into();
        let editor = app.handoff.retained.editors.get_mut(&draft().id).unwrap();
        editor.draft.brief.context = "Keep the current selection during search".into();
        editor.selected = Some(editor.draft.candidates[1].clone());
        let pending = Pending::Brief(editor.request().unwrap());
        app.handoff.retained.pending = Some(pending.clone());
        app.handoff.reset_session();
        app.handoff.observe_draft(draft());
        let persisted = serde_json::to_string(&app.handoff.retained).unwrap();
        let restored: Retained = serde_json::from_str(&persisted).unwrap();
        assert_eq!(restored.pending.unwrap().request(), pending.request());
        assert_eq!(
            restored.editors[&draft().id].draft.brief.context,
            "Keep the current selection during search"
        );
        assert_eq!(app.conversation.text, "Do not send this message");
        assert!(!app.handoff.projects_current);
    }
    #[test]
    fn out_of_order_reads_and_old_mutations_cannot_replace_current_edits_or_pending_receipts() {
        let mut app = app();
        let context = egui::Context::default();
        app.handoff.projects_read = Some(7);
        app.handoff_response(
            ApiRequest::Projects { generation: 6 },
            Ok(ApiPayload::Projects(ProjectList {
                service: service(),
                items: vec![],
            })),
            &context,
        );
        assert_eq!(app.handoff.projects_read, Some(7));
        assert_eq!(app.handoff.projects.as_ref().unwrap().items.len(), 2);
        let editor = app.handoff.retained.editors.get_mut(&draft().id).unwrap();
        editor.selected = Some(editor.draft.candidates[0].clone());
        let request = editor.request().unwrap();
        app.handoff.retained.pending = Some(Pending::Brief(request.clone()));
        let mut other = request.clone();
        other.command_id = "another-command".into();
        app.handoff_response(
            ApiRequest::SaveHandoff(other),
            Err(ApiFailure::Rejected("old request".into())),
            &context,
        );
        assert_eq!(
            app.handoff.retained.pending.as_ref().unwrap().request(),
            ApiRequest::SaveHandoff(request)
        );
        assert!(app.handoff.error.is_none());
    }
    #[test]
    fn definite_conflict_retains_edits_but_uncertain_failure_locks_the_exact_envelope() {
        for failure in [
            ApiFailure::Conflict("destination changed".into()),
            ApiFailure::Other("response lost".into()),
        ] {
            let mut app = app();
            let editor = app.handoff.retained.editors.get_mut(&draft().id).unwrap();
            editor.selected = Some(editor.draft.candidates[0].clone());
            editor.draft.brief.constraints = "Retain this constraint".into();
            let request = editor.request().unwrap();
            app.handoff.retained.pending = Some(Pending::Brief(request.clone()));
            app.handoff.in_flight = true;
            app.handoff_response(
                ApiRequest::SaveHandoff(request.clone()),
                Err(failure.clone()),
                &egui::Context::default(),
            );
            assert!(!app.handoff.in_flight);
            assert_eq!(
                app.handoff.retained.editors[&draft().id]
                    .draft
                    .brief
                    .constraints,
                "Retain this constraint"
            );
            assert_eq!(
                app.handoff.retained.pending.is_some(),
                matches!(failure, ApiFailure::Other(_))
            );
        }
    }
    #[test]
    fn settings_navigation_keeps_unsent_text_and_projects_do_not_depend_on_model_availability() {
        let mut app = app();
        app.conversation.text = "Retain the composer".into();
        app.open_project_workspaces(&egui::Context::default());
        assert!(app.handoff.open && app.handoff.projects_open);
        assert_eq!(app.conversation.text, "Retain the composer");
        assert!(app.handoff.projects_current);
        app.open_conversation(None, &egui::Context::default());
        assert!(!app.handoff.open);
        assert_eq!(app.conversation.text, "Retain the composer");
    }
    #[test]
    fn editor_has_a_visible_sticky_save_and_scroll_bounded_fields_at_desktop_and_phone_widths() {
        for width in [1440.0, 390.0] {
            let mut app = app();
            let context = egui::Context::default();
            let mut nodes = Vec::new();
            context
                .run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 844.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        egui::CentralPanel::default()
                            .show(ui, |ui| app.show_handoff(ui, &mut nodes));
                    },
                )
                .textures_delta
                .clear();
            let save = nodes
                .iter()
                .find(|n| n.id.0 == "bokkie.handoff.save")
                .unwrap();
            assert!(!save.enabled, "Ambiguous project should block save");
            assert!(save.rect.min_x >= 0.0 && save.rect.max_x <= width && save.rect.max_y <= 844.0);
            for n in nodes
                .iter()
                .filter(|n| n.id.0.starts_with("bokkie.handoff."))
            {
                assert!(n.rect.max_x <= width + 1.0, "{} exceeded {width}", n.id.0);
            }
        }
    }
    #[test]
    fn project_conflict_requires_explicit_current_revision_adoption_and_retains_entered_fields() {
        let mut app = app();
        let entered = ProjectRegistration {
            name: "Atlas revised name".into(),
            host: "Sulaco".into(),
            workspace: "/entered/atlas".into(),
            context: "Keep these entered project fields".into(),
            ..Default::default()
        };
        app.handoff.retained.project_editor = Some(ProjectSaveRequest {
            command_id: "original".into(),
            project_id: "one".into(),
            expected_revision: 1,
            registration: entered.clone(),
        });
        app.handoff.projects_read = Some(7);
        app.handoff_response(
            ApiRequest::Projects { generation: 7 },
            Ok(ApiPayload::Projects(ProjectList {
                service: service(),
                items: vec![project("one", "Nostromo")],
            })),
            &egui::Context::default(),
        );
        let editor = app.handoff.retained.project_editor.as_ref().unwrap();
        assert_eq!(
            editor.expected_revision, 1,
            "Refresh must not silently rebase the editor"
        );
        assert_eq!(editor.registration, entered);
        app.handoff.use_current_project_revision();
        let editor = app.handoff.retained.project_editor.as_ref().unwrap();
        assert_eq!(editor.expected_revision, 3);
        assert_eq!(editor.registration, entered);
        app.handoff.projects_current = false;
        app.handoff
            .retained
            .project_editor
            .as_mut()
            .unwrap()
            .expected_revision = 1;
        app.handoff.use_current_project_revision();
        assert_eq!(
            app.handoff
                .retained
                .project_editor
                .as_ref()
                .unwrap()
                .expected_revision,
            1,
            "Stale service data cannot supply a review baseline"
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_copy_is_a_durable_request_and_never_a_verified_clipboard_success() {
        let mut app = app();
        let draft = draft();
        app.handoff.view_current = true;
        app.handoff.view = Some(HandoffView {
            service: service(),
            snapshot: HandoffSnapshot {
                id: draft.id,
                revision: 1,
                conversation_id: draft.conversation_id,
                source_request_id: draft.source_request_id,
                project: draft.candidates[0].clone(),
                brief: draft.brief,
                created_at: 1,
                return_path: "/ui/?handoff=10000000-0000-4000-8000-000000000001&revision=1".into(),
                complete_brief: "Complete exact saved brief".into(),
            },
            activities: vec![],
            previous_revision: None,
            latest_revision: 1,
        });
        app.copy_saved_brief(&egui::Context::default());
        let Some(Pending::Activity(activity)) = app.handoff.retained.pending.as_ref() else {
            panic!("Native copy must retain its audit envelope");
        };
        assert_eq!(activity.kind, HandoffActivityKind::CopyRequested);
        assert_eq!(
            activity.note,
            "Platform clipboard delivery could not be verified"
        );
        assert_eq!(activity.revision, 1);
        assert!(app.handoff.copy_status.contains("cannot be verified"));
    }
    #[test]
    fn home_reads_open_the_saved_revision_without_resurrecting_obsolete_model_text() {
        let mut app = app();
        app.handoff.retained.editors.clear();
        let mut saved = draft();
        saved.saved_revision = 2;
        saved.brief.context = "Obsolete original model context".into();
        app.handoff.observe_draft(saved.clone());
        assert!(app.handoff.retained.editors.is_empty());
        app.open_handoff_draft(saved.clone(), &egui::Context::default());
        assert_eq!(app.handoff.retained.selected, Some((saved.id.clone(), 2)));
        assert!(app.handoff.retained.editor.is_none());
        assert!(app.handoff.retained.editors.is_empty());
        let mut entered = BriefEditor::new(saved.clone());
        entered.draft.brief.context = "Keep these unsaved edits to a saved brief".into();
        app.handoff
            .retained
            .editors
            .insert(saved.id.clone(), entered);
        app.handoff.observe_draft(saved.clone());
        assert_eq!(
            app.handoff.retained.editors[&saved.id].draft.brief.context,
            "Keep these unsaved edits to a saved brief"
        );
    }
    #[test]
    fn delayed_result_note_responses_and_exact_retries_preserve_new_text_and_navigation() {
        for uncertain in [false, true] {
            for destination in [
                Some((draft().id, 1)),
                Some(("another-record".into(), 2)),
                None,
            ] {
                for entered in ["Submitted note", "A newer unsent note"] {
                    let mut app = app();
                    let submitted = HandoffActivityRequest {
                        command_id: "original-note-command".into(),
                        handoff_id: draft().id,
                        revision: 1,
                        kind: HandoffActivityKind::ResultNote,
                        note: "Submitted note".into(),
                    };
                    let request = ApiRequest::HandoffActivity(submitted.clone());
                    app.handoff.retained.pending = Some(Pending::Activity(submitted.clone()));
                    app.handoff.retained.selected = Some((submitted.handoff_id.clone(), 1));
                    app.handoff.retained.note = submitted.note.clone();
                    app.handoff.in_flight = true;
                    if uncertain {
                        app.handoff_response(
                            request.clone(),
                            Err(ApiFailure::Other("response lost".into())),
                            &egui::Context::default(),
                        );
                        // Reload/restart restores the exact request, not the newly typed note.
                        let raw = serde_json::to_string(&app.handoff.retained).unwrap();
                        app.handoff.retained = serde_json::from_str(&raw).unwrap();
                        app.handoff.reset_session();
                        assert_eq!(
                            app.handoff.retained.pending.as_ref().unwrap().request(),
                            request
                        );
                    }
                    app.handoff.retained.note = entered.into();
                    app.handoff.retained.selected = destination.clone();
                    app.handoff.in_flight = true;
                    let d = draft();
                    let response = HandoffView {
                        service: service(),
                        snapshot: HandoffSnapshot {
                            id: d.id,
                            revision: 1,
                            conversation_id: d.conversation_id,
                            source_request_id: d.source_request_id,
                            project: d.candidates[0].clone(),
                            brief: d.brief,
                            created_at: 1,
                            return_path: "/ui/".into(),
                            complete_brief: "Saved brief".into(),
                        },
                        activities: vec![],
                        previous_revision: None,
                        latest_revision: 1,
                    };
                    app.handoff_response(
                        request,
                        Ok(ApiPayload::Handoff(Box::new(response))),
                        &egui::Context::default(),
                    );
                    let same_record = destination == Some((submitted.handoff_id, 1));
                    assert_eq!(
                        app.handoff.retained.note,
                        if same_record && entered == submitted.note {
                            ""
                        } else {
                            entered
                        }
                    );
                    assert_eq!(
                        app.handoff.retained.selected, destination,
                        "An old activity must not navigate away from the chosen surface"
                    );
                    assert!(app.handoff.retained.pending.is_none());
                    assert!(!app.handoff.in_flight);
                }
            }
        }
    }
}
