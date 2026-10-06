use super::*;
use bokkie_operator_api::{
    AdviserRoleSettings, AgentRoleSettings, AgentSettingsSaveRequest, AgentSettingsView,
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct AgentRoleDraft {
    model: String,
    effort: String,
    instructions: String,
    timeout: String,
    context_bytes: String,
    output_bytes: String,
    model_calls: String,
}

impl AgentRoleDraft {
    fn from_role(role: &AgentRoleSettings) -> Self {
        Self {
            model: role.model.clone(),
            effort: role.effort.clone(),
            instructions: role.additional_instructions.clone(),
            timeout: role.timeout_seconds.to_string(),
            context_bytes: role.max_context_bytes.to_string(),
            output_bytes: role.max_output_bytes.to_string(),
            model_calls: role.max_model_calls.to_string(),
        }
    }

    fn validate(
        &self,
        view: &AgentSettingsView,
        maximum_calls: u8,
    ) -> Result<AgentRoleSettings, String> {
        if view.models.is_empty() {
            return Err(view.unavailable_reason.clone().unwrap_or_else(|| {
                "Model choices are unavailable. Reload when the runtime is ready.".into()
            }));
        }
        let model = view
            .models
            .iter()
            .find(|option| option.model == self.model)
            .ok_or("Choose an available model.")?;
        if model.supported_efforts.is_empty() || !model.supported_efforts.contains(&self.effort) {
            return Err("Choose a thinking level supported by this model.".into());
        }
        if self.instructions.len() > 8_192 || self.instructions.contains('\0') {
            return Err(
                "Additional instructions must contain at most 8,192 bytes and no NUL character."
                    .into(),
            );
        }
        let ceilings = view
            .ceilings
            .as_ref()
            .ok_or("Execution limits are unavailable.")?;
        let timeout_seconds = bounded_number(
            &self.timeout,
            "Time per model call",
            1,
            ceilings.timeout_seconds,
        )?;
        let max_context_bytes = bounded_number(
            &self.context_bytes,
            "Input context limit",
            1_024,
            ceilings.max_context_bytes as u64,
        )? as usize;
        let max_output_bytes = bounded_number(
            &self.output_bytes,
            "Response limit",
            1_024,
            ceilings.max_output_bytes as u64,
        )? as usize;
        let max_model_calls = bounded_number(
            &self.model_calls,
            "Model calls per request",
            1,
            u64::from(ceilings.max_model_calls.min(maximum_calls)),
        )? as u8;
        Ok(AgentRoleSettings {
            model: self.model.clone(),
            effort: self.effort.clone(),
            additional_instructions: self.instructions.clone(),
            timeout_seconds,
            max_context_bytes,
            max_output_bytes,
            max_model_calls,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AdviserSettingsDraft {
    role: AgentRoleDraft,
    automatic_consultation: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AgentSettingsDraft {
    expected_revision: i64,
    main: AgentRoleDraft,
    adviser_enabled: bool,
    adviser: Option<AdviserSettingsDraft>,
}

impl AgentSettingsDraft {
    fn from_view(view: &AgentSettingsView) -> Option<Self> {
        let profile = view.profile.as_ref()?;
        Some(Self {
            expected_revision: profile.revision,
            main: AgentRoleDraft::from_role(&profile.main),
            adviser_enabled: profile.adviser.is_some(),
            adviser: profile
                .adviser
                .as_ref()
                .map(|adviser| AdviserSettingsDraft {
                    role: AgentRoleDraft::from_role(&adviser.role),
                    automatic_consultation: adviser.automatic_consultation,
                }),
        })
    }

    fn validate(&self, view: &AgentSettingsView) -> Result<AgentRoleSettings, String> {
        self.main
            .validate(view, if self.adviser_enabled { 4 } else { 2 })
    }

    fn validate_adviser(
        &self,
        view: &AgentSettingsView,
    ) -> Result<Option<AdviserRoleSettings>, String> {
        if !self.adviser_enabled {
            return Ok(None);
        }
        let draft = self
            .adviser
            .as_ref()
            .ok_or("Choose an available adviser model.")?;
        let role = draft
            .role
            .validate(view, 1)
            .map_err(|error| format!("Adviser: {error}"))?;
        Ok(Some(AdviserRoleSettings {
            role,
            automatic_consultation: draft.automatic_consultation,
        }))
    }

    fn set_adviser_enabled(&mut self, enabled: bool, view: &AgentSettingsView) {
        self.adviser_enabled = enabled;
        if enabled {
            if self.adviser.is_none() {
                let option = view.models.iter().find(|option| {
                    option.model == "gpt-6-astra" && !option.supported_efforts.is_empty()
                });
                let effort = option
                    .map(|option| {
                        if option
                            .supported_efforts
                            .iter()
                            .any(|effort| effort == "high")
                        {
                            "high".to_owned()
                        } else {
                            option.default_effort.clone()
                        }
                    })
                    .unwrap_or_default();
                let mut role = self.main.clone();
                role.model = option
                    .map(|option| option.model.clone())
                    .unwrap_or_default();
                role.effort = effort;
                role.instructions.clear();
                role.model_calls = "1".into();
                self.adviser = Some(AdviserSettingsDraft {
                    role,
                    automatic_consultation: false,
                });
            }
            if let Some(ceiling) = &view.ceilings {
                let limit = ceiling.max_model_calls.min(4);
                if self
                    .main
                    .model_calls
                    .parse::<u8>()
                    .is_ok_and(|calls| calls < limit)
                {
                    self.main.model_calls = limit.to_string();
                }
            }
        } else if self
            .main
            .model_calls
            .parse::<u8>()
            .is_ok_and(|calls| calls > 2)
        {
            self.main.model_calls = "2".into();
        }
    }
}

fn bounded_number(value: &str, label: &str, minimum: u64, maximum: u64) -> Result<u64, String> {
    let number = value
        .trim()
        .parse::<u64>()
        .map_err(|_| format!("{label} must be a whole number from {minimum} to {maximum}."))?;
    if number < minimum || number > maximum {
        return Err(format!("{label} must be from {minimum} to {maximum}."));
    }
    Ok(number)
}

#[derive(Default)]
pub(super) struct AgentSettingsState {
    pub open: bool,
    view: Option<AgentSettingsView>,
    draft: Option<AgentSettingsDraft>,
    reading: Option<(u64, bool)>,
    pending: Option<AgentSettingsSaveRequest>,
    saving: bool,
    current: bool,
    conflict: bool,
    error: Option<String>,
    saved_notice: bool,
}

impl AgentSettingsState {
    pub(super) fn reset_session(&mut self) {
        self.reading = None;
        self.current = false;
        self.saving = false;
        self.saved_notice = false;
        // An uncertain save retains its exact receipt and reviewed payload.
    }

    fn accept(&mut self, view: AgentSettingsView, replace_draft: bool) {
        if replace_draft || self.draft.is_none() {
            self.draft = AgentSettingsDraft::from_view(&view);
            self.conflict = false;
        }
        if !replace_draft && self.pending.is_none() {
            self.conflict |= self
                .draft
                .as_ref()
                .zip(view.profile.as_ref())
                .is_some_and(|(draft, profile)| draft.expected_revision != profile.revision);
        }
        self.current = true;
        self.view = Some(view);
    }

    fn save_request(&mut self) -> Result<AgentSettingsSaveRequest, String> {
        if self.saving || self.reading.is_some() || !self.current {
            return Err("Wait for a current settings read before saving.".into());
        }
        if self.conflict {
            return Err(
                "Settings changed elsewhere. Reload saved settings before saving again.".into(),
            );
        }
        if let Some(request) = &self.pending {
            return Ok(request.clone());
        }
        let view = self
            .view
            .as_ref()
            .ok_or("Agent settings have not loaded.")?;
        let draft = self
            .draft
            .as_ref()
            .ok_or("No effective profile is available.")?;
        let request = AgentSettingsSaveRequest {
            command_id: uuid::Uuid::new_v4().to_string(),
            expected_revision: draft.expected_revision,
            main: draft.validate(view)?,
            adviser: draft.validate_adviser(view)?,
        };
        self.pending = Some(request.clone());
        Ok(request)
    }

    fn failed_save(&mut self, error: &ApiFailure) {
        self.saving = false;
        self.saved_notice = false;
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
        ApiRequest::AgentSettings { .. } | ApiRequest::SaveAgentSettings(_)
    )
}

impl AttentionApp {
    pub(super) fn open_agent_settings(&mut self, context: &egui::Context) {
        self.handoff.open = false;
        self.agent_settings.open = true;
        self.refresh_agent_settings(false, context);
    }

    pub(super) fn refresh_agent_settings(&mut self, replace_draft: bool, context: &egui::Context) {
        if !self.agent_settings.open
            || self.session.is_none()
            || self.transport.is_none()
            || self.agent_settings.reading.is_some()
            || self.agent_settings.saving
            || (replace_draft && self.agent_settings.pending.is_some())
        {
            return;
        }
        let generation = self.fresh_generation();
        self.agent_settings.reading = Some((generation, replace_draft));
        self.dispatch(ApiRequest::AgentSettings { generation }, context);
    }

    pub(super) fn agent_settings_response(
        &mut self,
        request: ApiRequest,
        result: Result<ApiPayload, ApiFailure>,
        context: &egui::Context,
    ) {
        let replace_draft = match &request {
            ApiRequest::AgentSettings { generation } => {
                let Some((expected, replace)) = self.agent_settings.reading else {
                    return;
                };
                if expected != *generation {
                    return;
                }
                self.agent_settings.reading = None;
                replace
            }
            ApiRequest::SaveAgentSettings(save) => {
                if self.agent_settings.pending.as_ref() != Some(save) {
                    return;
                }
                self.agent_settings.saving = false;
                true
            }
            _ => return,
        };
        let mutation = matches!(request, ApiRequest::SaveAgentSettings(_));
        match result {
            Ok(ApiPayload::AgentSettings(view)) => {
                if self
                    .session
                    .as_ref()
                    .is_none_or(|session| !session.matches(&view.service))
                {
                    return;
                }
                self.agent_settings.error = None;
                self.agent_settings.accept(*view, replace_draft);
                if mutation {
                    self.agent_settings.pending = None;
                    self.agent_settings.saved_notice = true;
                }
            }
            Err(error) => {
                if mutation {
                    self.agent_settings.failed_save(&error);
                } else {
                    self.agent_settings.current = false;
                    self.agent_settings.error = Some(error.to_string());
                }
                if matches!(error, ApiFailure::SessionChanged(_)) {
                    self.restart_session(&error.to_string(), context);
                }
            }
            _ => {
                self.agent_settings.current = false;
                self.agent_settings.error =
                    Some("Bokkie returned an unexpected settings response.".into());
            }
        }
    }

    pub(super) fn show_agent_settings(
        &mut self,
        ui: &mut egui::Ui,
        tokens: DesignTokens,
        nodes: &mut Vec<UiNode>,
        text: &mut Vec<TextLayoutObservation>,
    ) {
        let bounds = ui.available_rect_before_wrap();
        let width = bounds.width().min(660.0);
        let rect = bounds.shrink2(egui::vec2((bounds.width() - width) / 2.0, 0.0));
        let mut return_home = false;
        let mut projects = false;
        let mut reload = false;
        let mut save = false;
        let session_available = self.session.is_some();
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            let mut presentation = PresentationContext::new(ui, tokens, self.preferences.font_scale,
                PresentationScope::new("bokkie.settings"), SemanticUiId::new("bokkie.settings"));
            let state = &mut self.agent_settings;
            presentation.heading(ui, "heading", "Settings");
            ui.horizontal_wrapped(|ui| {
                return_home = settings_button(ui, "return", "Return to conversation", true, &mut presentation);
                projects = settings_button(ui, "projects", "Project workspaces", true, &mut presentation);
            });
            settings_text(ui, "scope", "Main conversational role. Saved changes apply to new requests; running requests and retries keep their original settings.", TextRole::Secondary, &mut presentation);
            ui.add_space(8.0);
            let remaining = ui.available_rect_before_wrap();
            let footer = egui::Rect::from_min_max(egui::pos2(remaining.left(), (remaining.bottom() - 94.0).max(remaining.top())), remaining.max);
            let content = egui::Rect::from_min_max(remaining.min, egui::pos2(remaining.right(), (footer.top() - 8.0).max(remaining.top())));
            ui.scope_builder(egui::UiBuilder::new().max_rect(content), |ui| {
                ui.set_clip_rect(content.intersect(ui.clip_rect()));
                egui::ScrollArea::vertical().id_salt("agent-settings-scroll").auto_shrink([false, false]).show(ui, |ui| {
                    if state.reading.is_some() { settings_text(ui, "loading", "Reading saved settings…", TextRole::Secondary, &mut presentation); }
                    if !state.current || !session_available {
                        settings_text(ui, "stale", "Settings are not verified for the current service. Reload when connected.", TextRole::Body, &mut presentation);
                    }
                    if let Some(view) = &state.view {
                        if let Some(profile) = &view.profile {
                            let effective = state.current && session_available && view.effective;
                            settings_text(ui, "effective", &format!("{} revision {} · {} · {}", if effective { "Effective saved" } else { "Saved, unavailable" }, profile.revision, profile.main.model, effort_label(&profile.main.effort)), TextRole::Secondary, &mut presentation);
                            let adviser_summary = profile.adviser.as_ref().map(|adviser| format!("Saved Astra adviser · {} · {} · {}", adviser.role.model, effort_label(&adviser.role.effort), if adviser.automatic_consultation { "when requirements conflict" } else { "manual consultation" })).unwrap_or_else(|| "Saved Astra adviser · Off".into());
                            settings_text(ui, "saved-adviser", &adviser_summary, TextRole::Secondary, &mut presentation);
                        }
                        if !view.effective {
                            settings_text(ui, "unavailable", view.unavailable_reason.as_deref().unwrap_or("The conversation runtime is unavailable."), TextRole::Body, &mut presentation);
                        }
                        if let Some(draft) = &mut state.draft {
                            let editable = state.current && session_available && !view.models.is_empty() && view.ceilings.is_some() && state.pending.is_none() && !state.saving && state.reading.is_none() && !state.conflict;
                            ui.add_enabled_ui(editable, |ui| {
                                role_model_controls(ui, &mut draft.main, view, "", &mut presentation);
                                ui.add_space(8.0);
                                settings_text(ui, "instructions-label", "Additional instructions", TextRole::Body, &mut presentation);
                                let response = ui.add(egui::TextEdit::multiline(&mut draft.main.instructions).id_salt("agent-settings-instructions").desired_rows(4).desired_width(f32::INFINITY).hint_text("How should Bokkie help you?"));
                                control_node(ui, &response, "instructions", "Additional instructions", UiRole::Section, &mut presentation);
                                settings_text(ui, "instructions-help", "Added to Bokkie’s core guidance. Permissions and approval rules stay in force.", TextRole::Secondary, &mut presentation);
                                adviser_editor(ui, draft, view, &mut presentation);
                                let advanced = egui::CollapsingHeader::new("Advanced execution limits").id_salt("agent-settings-advanced").show(ui, |ui| {
                                    if let Some(ceilings) = &view.ceilings {
                                        number_field(ui, "timeout", "Time per model call (seconds)", &mut draft.main.timeout, 1, ceilings.timeout_seconds, &mut presentation);
                                        number_field(ui, "context", "Input context limit (bytes)", &mut draft.main.context_bytes, 1_024, ceilings.max_context_bytes as u64, &mut presentation);
                                        number_field(ui, "output", "Response limit (bytes)", &mut draft.main.output_bytes, 1_024, ceilings.max_output_bytes as u64, &mut presentation);
                                        number_field(ui, "calls", "Total model calls per request", &mut draft.main.model_calls, 1, u64::from(ceilings.max_model_calls.min(if draft.adviser_enabled { 4 } else { 2 })), &mut presentation);
                                        if draft.adviser_enabled { settings_text(ui, "shared-budget", "All model calls, including Astra, share this total request budget. Astra is consulted at most once.", TextRole::Secondary, &mut presentation); }
                                        settings_text(ui, "limits-help", "1,024 bytes = 1 KiB. Limits cannot exceed this deployment’s safety ceilings.", TextRole::Secondary, &mut presentation);
                                    }
                                });
                                control_node(ui, &advanced.header_response, "advanced", "Advanced execution limits", UiRole::Section, &mut presentation);
                            });
                            if let Err(error) = draft.validate(view).and_then(|_| draft.validate_adviser(view)) {
                                settings_text(ui, "validation", &error, TextRole::Body, &mut presentation);
                            }
                        }
                    } else if state.reading.is_none() {
                        settings_text(ui, "empty", "Connect to Bokkie to read the effective conversational profile.", TextRole::Body, &mut presentation);
                    }
                    if let Some(error) = &state.error { settings_text(ui, "error", &format!("Settings need attention: {error}"), TextRole::Body, &mut presentation); }
                    if state.conflict { settings_text(ui, "conflict", "Settings changed elsewhere. Your entered values are retained. Reload saved settings deliberately to replace this draft.", TextRole::Body, &mut presentation); }
                    if state.pending.is_some() && !state.saving { settings_text(ui, "uncertain", "The save result is uncertain. Retry the exact saved request to reconcile it; entered values stay locked until it is resolved.", TextRole::Body, &mut presentation); }
                });
            });
            ui.scope_builder(egui::UiBuilder::new().max_rect(footer), |ui| {
                ui.set_clip_rect(footer.intersect(ui.clip_rect()));
                ui.separator();
                let valid = state.view.as_ref().zip(state.draft.as_ref()).is_some_and(|(view, draft)| draft.validate(view).is_ok() && draft.validate_adviser(view).is_ok());
                let ready = state.current && session_available && !state.saving && state.reading.is_none() && !state.conflict;
                ui.horizontal_wrapped(|ui| {
                    save = settings_button(ui, "save", if state.saving { "Saving…" } else if state.pending.is_some() { "Retry saved request" } else { "Save changes" }, ready && (state.pending.is_some() || valid), &mut presentation);
                    reload = settings_button(ui, "reload", "Reload saved settings", session_available && !state.saving && state.reading.is_none() && state.pending.is_none(), &mut presentation);
                });
                if state.saved_notice { settings_text(ui, "saved", "Saved. New requests will use this revision.", TextRole::Secondary, &mut presentation); }
            });
            let observations = presentation.finish(ui);
            nodes.extend(observations.semantic_nodes);
            text.extend(observations.text_layouts);
        });
        let mut node = UiNode::container(
            SemanticUiId::new("bokkie.settings"),
            Some(SemanticUiId::root()),
            UiRole::Section,
            rect.into(),
        );
        node.name = "Agent settings".into();
        nodes.push(node);
        let context = ui.ctx().clone();
        if return_home {
            self.open_conversation(None, &context);
        }
        if projects {
            self.open_project_workspaces(&context);
        }
        if reload {
            self.refresh_agent_settings(true, &context);
        }
        if save {
            match self.agent_settings.save_request() {
                Ok(request) => {
                    self.agent_settings.saving = true;
                    self.agent_settings.error = None;
                    self.agent_settings.saved_notice = false;
                    self.dispatch(ApiRequest::SaveAgentSettings(request), &context);
                }
                Err(error) => self.agent_settings.error = Some(error),
            }
        }
    }
}

fn role_model_controls(
    ui: &mut egui::Ui,
    role: &mut AgentRoleDraft,
    view: &AgentSettingsView,
    prefix: &str,
    presentation: &mut PresentationContext,
) {
    settings_text(
        ui,
        &format!("{prefix}model-label"),
        "Model",
        TextRole::Body,
        presentation,
    );
    let selected = view
        .models
        .iter()
        .find(|option| option.model == role.model)
        .map(|option| option.display_name.as_str())
        .unwrap_or(if role.model.is_empty() {
            "Choose a model"
        } else {
            &role.model
        });
    let previous = role.model.clone();
    let combo = egui::ComboBox::from_id_salt(("agent-settings-model", prefix))
        .selected_text(selected)
        .width(ui.available_width().min(360.0))
        .show_ui(ui, |ui| {
            for option in &view.models {
                let response = ui.add_enabled(
                    !option.supported_efforts.is_empty(),
                    egui::Button::selectable(role.model == option.model, &option.display_name),
                );
                control_node(
                    ui,
                    &response,
                    &format!("{prefix}model-option.{}", option.model),
                    &option.display_name,
                    UiRole::Button,
                    presentation,
                );
                if response.clicked() {
                    role.model = option.model.clone();
                }
            }
        });
    control_node(
        ui,
        &combo.response,
        &format!("{prefix}model"),
        "Model",
        UiRole::Section,
        presentation,
    );
    if previous != role.model
        && let Some(option) = view.models.iter().find(|option| option.model == role.model)
        && !option.supported_efforts.contains(&role.effort)
    {
        role.effort = option.default_effort.clone();
    }
    ui.add_space(8.0);
    settings_text(
        ui,
        &format!("{prefix}effort-label"),
        "Thinking level",
        TextRole::Body,
        presentation,
    );
    let combo = egui::ComboBox::from_id_salt(("agent-settings-effort", prefix))
        .selected_text(if role.effort.is_empty() {
            "Choose a thinking level"
        } else {
            effort_label(&role.effort)
        })
        .width(ui.available_width().min(360.0))
        .show_ui(ui, |ui| {
            if let Some(model) = view.models.iter().find(|option| option.model == role.model) {
                for effort in &model.supported_efforts {
                    let response =
                        ui.selectable_value(&mut role.effort, effort.clone(), effort_label(effort));
                    control_node(
                        ui,
                        &response,
                        &format!("{prefix}effort-option.{effort}"),
                        effort_label(effort),
                        UiRole::Button,
                        presentation,
                    );
                }
            }
        });
    control_node(
        ui,
        &combo.response,
        &format!("{prefix}effort"),
        "Thinking level",
        UiRole::Section,
        presentation,
    );
}

fn adviser_editor(
    ui: &mut egui::Ui,
    draft: &mut AgentSettingsDraft,
    view: &AgentSettingsView,
    presentation: &mut PresentationContext,
) {
    let adviser = egui::CollapsingHeader::new("Astra adviser (optional)")
        .id_salt("agent-settings-adviser")
        .show(ui, |ui| adviser_body(ui, draft, view, presentation));
    control_node(
        ui,
        &adviser.header_response,
        "adviser",
        "Astra adviser (optional)",
        UiRole::Section,
        presentation,
    );
}

fn adviser_body(
    ui: &mut egui::Ui,
    draft: &mut AgentSettingsDraft,
    view: &AgentSettingsView,
    presentation: &mut PresentationContext,
) {
    let mut enabled = draft.adviser_enabled;
    let response = ui.checkbox(&mut enabled, "Enable Astra adviser");
    control_node(
        ui,
        &response,
        "adviser-enabled",
        "Enable Astra adviser",
        UiRole::Section,
        presentation,
    );
    if response.changed() {
        draft.set_adviser_enabled(enabled, view);
    }
    settings_text(
        ui,
        "adviser-scope",
        "Astra answers one bounded question in this conversation. All calls, including Astra, share Bokkie’s total request budget.",
        TextRole::Secondary,
        presentation,
    );
    if !draft.adviser_enabled {
        return;
    }
    if let Some(adviser) = &mut draft.adviser {
        role_model_controls(ui, &mut adviser.role, view, "adviser-", presentation);
        ui.add_space(8.0);
        settings_text(
            ui,
            "adviser-instructions-label",
            "Additional adviser instructions",
            TextRole::Body,
            presentation,
        );
        let response = ui.add(
            egui::TextEdit::multiline(&mut adviser.role.instructions)
                .id_salt("agent-settings-adviser-instructions")
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        control_node(
            ui,
            &response,
            "adviser-instructions",
            "Additional adviser instructions",
            UiRole::Section,
            presentation,
        );
        let response = ui.checkbox(
            &mut adviser.automatic_consultation,
            "Consult when requirements conflict",
        );
        control_node(
            ui,
            &response,
            "adviser-automatic",
            "Consult when requirements conflict",
            UiRole::Section,
            presentation,
        );
        settings_text(
            ui,
            "adviser-automatic-condition",
            "Bokkie identifies two incompatible requirements quoted from the current request and asks one bounded reconciliation question. At most once per request.",
            TextRole::Secondary,
            presentation,
        );
        let limits = egui::CollapsingHeader::new("Adviser execution limits")
            .id_salt("agent-settings-adviser-advanced")
            .show(ui, |ui| {
                if let Some(ceilings) = &view.ceilings {
                    number_field(
                        ui,
                        "adviser-timeout",
                        "Time per adviser call (seconds)",
                        &mut adviser.role.timeout,
                        1,
                        ceilings.timeout_seconds,
                        presentation,
                    );
                    number_field(
                        ui,
                        "adviser-context",
                        "Adviser input context limit (bytes)",
                        &mut adviser.role.context_bytes,
                        1_024,
                        ceilings.max_context_bytes as u64,
                        presentation,
                    );
                    number_field(
                        ui,
                        "adviser-output",
                        "Adviser response limit (bytes)",
                        &mut adviser.role.output_bytes,
                        1_024,
                        ceilings.max_output_bytes as u64,
                        presentation,
                    );
                    settings_text(
                        ui,
                        "adviser-one-call",
                        "One adviser call per request. 1,024 bytes = 1 KiB.",
                        TextRole::Secondary,
                        presentation,
                    );
                }
            });
        control_node(
            ui,
            &limits.header_response,
            "adviser-advanced",
            "Adviser execution limits",
            UiRole::Section,
            presentation,
        );
        if let Err(error) = adviser.role.validate(view, 1) {
            settings_text(
                ui,
                "adviser-validation",
                &format!("Adviser: {error}"),
                TextRole::Body,
                presentation,
            );
        }
    }
}

fn settings_text(
    ui: &mut egui::Ui,
    key: &str,
    value: &str,
    role: TextRole,
    presentation: &mut PresentationContext,
) {
    presentation.content(
        ui,
        key,
        value,
        ContentTextSpec {
            role,
            overflow: TextOverflow::Wrap,
            max_lines: 8,
            interaction: TextInteraction::Selectable,
        },
    );
}
fn effort_label(effort: &str) -> &str {
    match effort {
        "none" => "None",
        "minimal" => "Minimal",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Extra high",
        "max" => "Maximum",
        "ultra" => "Ultra",
        value => value,
    }
}
fn settings_button(
    ui: &mut egui::Ui,
    key: &str,
    label: &str,
    enabled: bool,
    presentation: &mut PresentationContext,
) -> bool {
    let response = ui
        .push_id(key, |ui| {
            presentation.native(ui, NativeTextControlKind::Button, |ui| {
                (ui.add_enabled(enabled, egui::Button::new(label)), ())
            })
        })
        .inner
        .0;
    control_node(ui, &response, key, label, UiRole::Button, presentation);
    response.clicked()
}
fn control_node(
    ui: &egui::Ui,
    response: &egui::Response,
    key: &str,
    label: &str,
    role: UiRole,
    presentation: &mut PresentationContext,
) {
    if !matches!(
        key,
        "instructions"
            | "timeout"
            | "context"
            | "output"
            | "calls"
            | "adviser-instructions"
            | "adviser-timeout"
            | "adviser-context"
            | "adviser-output"
    ) {
        record_native_text_control(
            response,
            match key {
                "model" | "effort" | "adviser-model" | "adviser-effort" => {
                    NativeTextControlKind::ComboBox
                }
                _ if role == UiRole::Button => NativeTextControlKind::Button,
                _ => NativeTextControlKind::Selectable,
            },
        );
    }
    // Popup and form clips can sit inside the viewport. Publish their visible hit region.
    let rect = response.interact_rect.intersect(ui.clip_rect());
    if !rect.is_positive() {
        return;
    }
    let mut node = UiNode::container(
        SemanticUiId::new(format!("bokkie.settings.{key}")),
        Some(SemanticUiId::new("bokkie.settings")),
        role,
        rect.into(),
    );
    node.name = label.into();
    node.enabled = response.enabled();
    node.focused = response.has_focus();
    presentation.observe_node(ui, node);
}
fn number_field(
    ui: &mut egui::Ui,
    key: &str,
    label: &str,
    value: &mut String,
    minimum: u64,
    maximum: u64,
    presentation: &mut PresentationContext,
) {
    settings_text(
        ui,
        &format!("{key}-label"),
        label,
        TextRole::Body,
        presentation,
    );
    let response = ui.add(
        egui::TextEdit::singleline(value)
            .id_salt(("agent-settings-limit", key))
            .desired_width(140.0),
    );
    control_node(ui, &response, key, label, UiRole::Section, presentation);
    let invalid = bounded_number(value, label, minimum, maximum).is_err();
    let guidance = if invalid {
        format!("Enter a whole number from {minimum} to {maximum}.")
    } else {
        format!("{minimum}–{maximum}")
    };
    settings_text(
        ui,
        &format!("{key}-range"),
        &guidance,
        if invalid {
            TextRole::Body
        } else {
            TextRole::Secondary
        },
        presentation,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use bokkie_operator_api::{
        API_CONTRACT_VERSION, AgentModelOption, AgentProfileRevision, BOKKIE_BUILD_ID,
        SUPPORTED_SCHEMA_VERSION, ServiceIdentity, SessionBootstrap,
    };

    fn view(revision: i64) -> AgentSettingsView {
        let main = AgentRoleSettings {
            model: "gpt-main".into(),
            effort: "high".into(),
            additional_instructions: "Be clear.".into(),
            timeout_seconds: 120,
            max_context_bytes: 32_768,
            max_output_bytes: 8_192,
            max_model_calls: 2,
        };
        AgentSettingsView {
            service: ServiceIdentity {
                build: BOKKIE_BUILD_ID.into(),
                api_contract_version: API_CONTRACT_VERSION,
                schema_version: SUPPORTED_SCHEMA_VERSION,
                process_id: 42,
                session_id: "current".into(),
            },
            profile: Some(AgentProfileRevision {
                revision,
                contract_version: 1,
                main: main.clone(),
                adviser: None,
            }),
            models: vec![
                AgentModelOption {
                    model: "gpt-main".into(),
                    display_name: "Main model".into(),
                    supported_efforts: vec!["medium".into(), "high".into()],
                    default_effort: "medium".into(),
                },
                AgentModelOption {
                    model: "gpt-other".into(),
                    display_name: "Other model".into(),
                    supported_efforts: vec!["low".into()],
                    default_effort: "low".into(),
                },
            ],
            ceilings: Some(main),
            effective: true,
            unavailable_reason: None,
        }
    }
    fn ready_state() -> AgentSettingsState {
        let mut state = AgentSettingsState::default();
        state.accept(view(7), true);
        state
    }
    fn app() -> AttentionApp {
        let mut app = super::super::tests::test_app();
        app.session = Some(
            ApiSession::from_bootstrap(SessionBootstrap {
                service: view(7).service,
                mutation_token: "a".repeat(64),
            })
            .unwrap(),
        );
        app.agent_settings = ready_state();
        app.agent_settings.open = true;
        app
    }

    fn adviser_view(revision: i64) -> AgentSettingsView {
        let mut view = view(revision);
        view.ceilings.as_mut().unwrap().max_model_calls = 4;
        view.models.push(AgentModelOption {
            model: "gpt-6-astra".into(),
            display_name: "Astra".into(),
            supported_efforts: vec!["medium".into(), "high".into()],
            default_effort: "medium".into(),
        });
        view
    }

    #[test]
    fn clipped_control_semantics_keep_only_the_visible_interaction_region() {
        let context = egui::Context::default();
        Appearance::default().apply(&context);
        let app = app();
        let tokens = app.theme.resolve(
            app.preferences
                .theme_variant(context.theme() == egui::Theme::Dark),
            app.preferences.density_variant(),
            TypographyProfile::Reading,
        );
        let clip = egui::Rect::from_min_max(egui::pos2(20.0, 20.0), egui::pos2(180.0, 100.0));
        let mut observed = None;
        context
            .run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(220.0, 220.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    ui.set_clip_rect(clip);
                    let mut presentation = PresentationContext::new(
                        ui,
                        tokens,
                        1.0,
                        PresentationScope::new("bokkie.settings"),
                        SemanticUiId::new("bokkie.settings"),
                    );
                    let mut partial = ui.interact(
                        egui::Rect::from_min_max(egui::pos2(10.0, 70.0), egui::pos2(200.0, 130.0)),
                        egui::Id::new("partial-option"),
                        egui::Sense::click(),
                    );
                    // A native control may paint beyond its narrower interactive region.
                    partial.interact_rect =
                        egui::Rect::from_min_max(egui::pos2(30.0, 80.0), egui::pos2(170.0, 120.0));
                    control_node(
                        ui,
                        &partial,
                        "model-option.partial",
                        "Partly visible option",
                        UiRole::Button,
                        &mut presentation,
                    );
                    let hidden = ui.interact(
                        egui::Rect::from_min_max(egui::pos2(20.0, 110.0), egui::pos2(180.0, 150.0)),
                        egui::Id::new("hidden-form-field"),
                        egui::Sense::click(),
                    );
                    control_node(
                        ui,
                        &hidden,
                        "timeout",
                        "Behind the fixed footer",
                        UiRole::Section,
                        &mut presentation,
                    );
                    observed = Some(presentation.finish(ui));
                },
            )
            .textures_delta
            .clear();
        let observed = observed.unwrap();
        assert_eq!(observed.semantic_nodes.len(), 1);
        let node = &observed.semantic_nodes[0];
        assert_eq!(
            node.id,
            SemanticUiId::new("bokkie.settings.model-option.partial")
        );
        assert_eq!(
            (
                node.rect.min_x,
                node.rect.min_y,
                node.rect.max_x,
                node.rect.max_y
            ),
            (30.0, 80.0, 170.0, 100.0)
        );
    }

    #[test]
    fn scroll_clipped_option_nodes_cannot_point_at_the_footer_beneath_them() {
        let context = egui::Context::default();
        Appearance::default().apply(&context);
        let app = app();
        let tokens = app.theme.resolve(
            app.preferences
                .theme_variant(context.theme() == egui::Theme::Dark),
            app.preferences.density_variant(),
            TypographyProfile::Reading,
        );
        let mut observed = None;
        let mut popup_clip = egui::Rect::NOTHING;
        let mut footer = egui::Rect::NOTHING;
        for _ in 0..3 {
            context
                .run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(390.0, 240.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        let mut presentation = PresentationContext::new(
                            ui,
                            tokens,
                            1.0,
                            PresentationScope::new("bokkie.settings"),
                            SemanticUiId::new("bokkie.settings"),
                        );
                        egui::ScrollArea::vertical()
                            .id_salt("clipped-settings-options")
                            .max_height(70.0)
                            .show(ui, |ui| {
                                popup_clip = ui.clip_rect();
                                for index in 0..12 {
                                    let response = ui.button(format!("Model option {index}"));
                                    control_node(
                                        ui,
                                        &response,
                                        &format!("model-option.{index}"),
                                        "Model option",
                                        UiRole::Button,
                                        &mut presentation,
                                    );
                                }
                            });
                        let response = ui.button("Save changes");
                        footer = response.rect;
                        control_node(
                            ui,
                            &response,
                            "save",
                            "Save changes",
                            UiRole::Button,
                            &mut presentation,
                        );
                        observed = Some(presentation.finish(ui));
                    },
                )
                .textures_delta
                .clear();
        }
        let observed = observed.unwrap();
        let options: Vec<_> = observed
            .semantic_nodes
            .iter()
            .filter(|node| node.id.0.starts_with("bokkie.settings.model-option."))
            .collect();
        assert!(!options.is_empty() && options.len() < 12);
        for node in options {
            let hit = egui::Rect::from_min_max(
                egui::pos2(node.rect.min_x, node.rect.min_y),
                egui::pos2(node.rect.max_x, node.rect.max_y),
            );
            assert!(popup_clip.contains_rect(hit));
            assert!(!footer.contains(hit.center()));
            assert!(hit.bottom() <= footer.top());
        }
        assert!(
            observed
                .semantic_nodes
                .iter()
                .any(|node| node.id == SemanticUiId::new("bokkie.settings.save"))
        );
    }

    #[test]
    fn adviser_enable_uses_advertised_defaults_and_expands_only_the_shared_budget() {
        let view = adviser_view(7);
        let mut draft = AgentSettingsDraft::from_view(&view).unwrap();
        assert!(!draft.adviser_enabled);
        assert!(draft.validate_adviser(&view).unwrap().is_none());
        draft.set_adviser_enabled(true, &view);
        let adviser = draft.validate_adviser(&view).unwrap().unwrap();
        assert_eq!(adviser.role.model, "gpt-6-astra");
        assert_eq!(adviser.role.effort, "high");
        assert_eq!(adviser.role.max_model_calls, 1);
        assert!(!adviser.automatic_consultation);
        assert_eq!(draft.validate(&view).unwrap().max_model_calls, 4);
        draft.adviser.as_mut().unwrap().role.instructions = "Retain this adviser draft".into();
        draft.main.model_calls = "5".into();
        assert!(draft.validate(&view).is_err());
        draft.set_adviser_enabled(false, &view);
        assert_eq!(draft.validate(&view).unwrap().max_model_calls, 2);
        assert!(draft.validate_adviser(&view).unwrap().is_none());
        draft.set_adviser_enabled(true, &view);
        assert_eq!(
            draft.adviser.as_ref().unwrap().role.instructions,
            "Retain this adviser draft"
        );
        draft.adviser.as_mut().unwrap().role.model_calls = "2".into();
        assert!(draft.validate_adviser(&view).is_err());
    }

    #[test]
    fn adviser_defaults_never_invent_a_model_or_an_unadvertised_thinking_level() {
        let mut view = view(7);
        view.ceilings.as_mut().unwrap().max_model_calls = 4;
        let mut draft = AgentSettingsDraft::from_view(&view).unwrap();
        draft.set_adviser_enabled(true, &view);
        assert!(draft.adviser.as_ref().unwrap().role.model.is_empty());
        assert!(draft.validate_adviser(&view).is_err());
        draft.adviser.as_mut().unwrap().role.model = "gpt-main".into();
        draft.adviser.as_mut().unwrap().role.effort = "high".into();
        assert!(draft.validate_adviser(&view).is_ok());
        view.models.push(AgentModelOption {
            model: "gpt-6-astra".into(),
            display_name: "Astra".into(),
            supported_efforts: vec!["low".into()],
            default_effort: "low".into(),
        });
        let mut draft = AgentSettingsDraft::from_view(&view).unwrap();
        draft.set_adviser_enabled(true, &view);
        assert_eq!(draft.adviser.as_ref().unwrap().role.effort, "low");
        view.models.last_mut().unwrap().supported_efforts.clear();
        let mut draft = AgentSettingsDraft::from_view(&view).unwrap();
        draft.set_adviser_enabled(true, &view);
        assert!(draft.adviser.as_ref().unwrap().role.model.is_empty());
    }

    #[test]
    fn uncertain_adviser_save_retains_the_complete_role_policy_and_shared_budget() {
        let view = adviser_view(7);
        let mut state = AgentSettingsState::default();
        state.accept(view.clone(), true);
        let draft = state.draft.as_mut().unwrap();
        draft.set_adviser_enabled(true, &view);
        draft.adviser.as_mut().unwrap().automatic_consultation = true;
        draft.adviser.as_mut().unwrap().role.instructions =
            "Quote the conflicting requirements".into();
        let request = state.save_request().unwrap();
        assert_eq!(request.main.max_model_calls, 4);
        assert!(request.adviser.as_ref().unwrap().automatic_consultation);
        state.failed_save(&ApiFailure::Other("lost acknowledgement".into()));
        state.reset_session();
        state.accept(adviser_view(8), false);
        assert_eq!(state.save_request().unwrap(), request);
        state.failed_save(&ApiFailure::Conflict("new saved revision".into()));
        assert_eq!(
            state
                .draft
                .as_ref()
                .unwrap()
                .adviser
                .as_ref()
                .unwrap()
                .role
                .instructions,
            "Quote the conflicting requirements"
        );
        assert!(state.save_request().is_err());
    }

    #[test]
    fn enabled_adviser_controls_remain_readable_at_desktop_and_narrow_widths() {
        for width in [660.0, 358.0] {
            let context = egui::Context::default();
            Appearance::default().apply(&context);
            let view = adviser_view(7);
            let mut draft = AgentSettingsDraft::from_view(&view).unwrap();
            draft.set_adviser_enabled(true, &view);
            let app = app();
            let tokens = app.theme.resolve(
                app.preferences
                    .theme_variant(context.theme() == egui::Theme::Dark),
                app.preferences.density_variant(),
                TypographyProfile::Reading,
            );
            let mut observations: Option<PresentationObservations> = None;
            for _ in 0..3 {
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
                            let mut presentation = PresentationContext::new(
                                ui,
                                tokens,
                                1.0,
                                PresentationScope::new("bokkie.settings"),
                                SemanticUiId::new("bokkie.settings"),
                            );
                            adviser_body(ui, &mut draft, &view, &mut presentation);
                            observations = Some(presentation.finish(ui));
                        },
                    )
                    .textures_delta
                    .clear();
            }
            let observations = observations.unwrap();
            for key in [
                "adviser-enabled",
                "adviser-model",
                "adviser-effort",
                "adviser-instructions",
                "adviser-automatic",
                "adviser-advanced",
            ] {
                let node = observations
                    .semantic_nodes
                    .iter()
                    .find(|node| node.id == SemanticUiId::new(format!("bokkie.settings.{key}")))
                    .unwrap_or_else(|| panic!("missing {key} at {width}"));
                assert!(
                    node.rect.min_x >= 0.0 && node.rect.max_x <= width && node.rect.max_y <= 844.0,
                    "{key}: {:?} at {width}",
                    node.rect
                );
            }
            assert!(polyorama_ui_egui::audit_text_layouts(&observations.text_layouts).is_empty());
        }
    }

    #[test]
    fn settings_validation_checks_exact_capabilities_utf8_and_deployment_limits() {
        let view = view(7);
        let baseline = AgentSettingsDraft::from_view(&view).unwrap();
        assert_eq!(
            baseline.validate(&view).unwrap(),
            view.profile.as_ref().unwrap().main
        );
        for (field, bad) in [
            ("model", "invented"),
            ("effort", "ultra"),
            ("timeout", "0"),
            ("timeout", "121"),
            ("context", "1023"),
            ("context", "32769"),
            ("output", "99999999999999999999999999"),
            ("calls", "3"),
            ("calls", "1.5"),
        ] {
            let mut draft = baseline.clone();
            match field {
                "model" => draft.main.model = bad.into(),
                "effort" => draft.main.effort = bad.into(),
                "timeout" => draft.main.timeout = bad.into(),
                "context" => draft.main.context_bytes = bad.into(),
                "output" => draft.main.output_bytes = bad.into(),
                "calls" => draft.main.model_calls = bad.into(),
                _ => unreachable!(),
            }
            assert!(draft.validate(&view).is_err(), "{field}={bad}");
        }
        let mut draft = baseline.clone();
        draft.main.instructions = "é".repeat(4096);
        assert!(draft.validate(&view).is_ok());
        draft.main.instructions.push('é');
        assert!(draft.validate(&view).is_err());
        draft.main.instructions = "a\0b".into();
        assert!(draft.validate(&view).is_err());
        let mut unavailable = view.clone();
        unavailable.effective = false;
        assert!(baseline.validate(&unavailable).is_ok());
        unavailable = view.clone();
        unavailable.models[0].supported_efforts.clear();
        assert!(baseline.validate(&unavailable).is_err());
        unavailable = view.clone();
        unavailable.ceilings = None;
        assert!(baseline.validate(&unavailable).is_err());
    }

    #[test]
    fn unavailable_saved_profile_can_be_repaired_only_with_live_supported_choices() {
        let mut unavailable = view(7);
        unavailable.effective = false;
        unavailable.unavailable_reason = Some("Saved model no longer available".into());
        unavailable.profile.as_mut().unwrap().main.model = "removed-model".into();
        let mut state = AgentSettingsState::default();
        state.accept(unavailable.clone(), true);
        assert!(state.save_request().is_err());
        assert_eq!(state.draft.as_ref().unwrap().main.model, "removed-model");
        state.draft.as_mut().unwrap().main.model = "gpt-main".into();
        assert_eq!(state.save_request().unwrap().main.model, "gpt-main");
        state.pending = None;
        unavailable.models.clear();
        state.accept(unavailable, false);
        assert!(state.save_request().is_err());
    }

    #[test]
    fn uncertain_save_retains_exact_payload_through_refresh_and_session_restart() {
        let mut state = ready_state();
        state.draft.as_mut().unwrap().main.instructions = "Retain this exact draft".into();
        let request = state.save_request().unwrap();
        state.saving = true;
        assert!(state.save_request().is_err());
        state.failed_save(&ApiFailure::Other("lost response".into()));
        state.reset_session();
        assert!(state.save_request().is_err());
        state.accept(view(8), false);
        assert_eq!(
            state.draft.as_ref().unwrap().main.instructions,
            "Retain this exact draft"
        );
        assert_eq!(state.save_request().unwrap(), request);
        assert_eq!(state.save_request().unwrap().expected_revision, 7);
    }

    #[test]
    fn conflict_and_definite_rejection_retain_entered_values_and_reload_is_deliberate() {
        let mut state = ready_state();
        state.draft.as_mut().unwrap().main.instructions = "Keep entered values".into();
        state.save_request().unwrap();
        state.failed_save(&ApiFailure::Conflict("newer revision".into()));
        assert!(state.pending.is_none());
        assert!(state.save_request().is_err());
        state.accept(view(8), false);
        assert_eq!(
            state.draft.as_ref().unwrap().main.instructions,
            "Keep entered values"
        );
        assert!(state.conflict);
        assert!(state.save_request().is_err());
        state.accept(view(8), true);
        assert!(!state.conflict);
        assert_eq!(state.draft.as_ref().unwrap().expected_revision, 8);
        let request = state.save_request().unwrap();
        state.failed_save(&ApiFailure::Rejected("invalid value".into()));
        assert!(state.pending.is_none());
        assert_eq!(
            state.draft.as_ref().unwrap().main.instructions,
            request.main.additional_instructions
        );
        assert_ne!(state.save_request().unwrap().command_id, request.command_id);
    }

    #[test]
    fn settings_responses_are_fenced_by_read_generation_save_identity_and_session() {
        let context = egui::Context::default();
        let mut app = app();
        app.agent_settings.draft.as_mut().unwrap().main.instructions = "Keep draft".into();
        app.agent_settings.reading = Some((12, true));
        app.agent_settings_response(
            ApiRequest::AgentSettings { generation: 11 },
            Ok(ApiPayload::AgentSettings(Box::new(view(8)))),
            &context,
        );
        assert_eq!(
            app.agent_settings
                .view
                .as_ref()
                .unwrap()
                .profile
                .as_ref()
                .unwrap()
                .revision,
            7
        );
        assert_eq!(app.agent_settings.reading, Some((12, true)));
        let mut wrong = view(9);
        wrong.service.session_id = "old".into();
        app.agent_settings_response(
            ApiRequest::AgentSettings { generation: 12 },
            Ok(ApiPayload::AgentSettings(Box::new(wrong))),
            &context,
        );
        assert_eq!(
            app.agent_settings.draft.as_ref().unwrap().main.instructions,
            "Keep draft"
        );
        let request = app.agent_settings.save_request().unwrap();
        let mut older = request.clone();
        older.command_id = "older-request".into();
        app.agent_settings_response(
            ApiRequest::SaveAgentSettings(older),
            Ok(ApiPayload::AgentSettings(Box::new(view(8)))),
            &context,
        );
        assert_eq!(app.agent_settings.pending.as_ref(), Some(&request));
        app.agent_settings_response(
            ApiRequest::SaveAgentSettings(request),
            Ok(ApiPayload::AgentSettings(Box::new(view(8)))),
            &context,
        );
        assert!(app.agent_settings.pending.is_none());
        assert!(app.agent_settings.saved_notice);
        assert_eq!(
            app.agent_settings.draft.as_ref().unwrap().expected_revision,
            8
        );
    }

    #[test]
    fn settings_controls_and_sticky_save_fit_desktop_and_narrow_viewports() {
        use eframe::App;
        for (width, height) in [(1440.0, 900.0), (390.0, 844.0)] {
            let context = egui::Context::default();
            Appearance::default().apply(&context);
            let mut app = app();
            let mut frame = eframe::Frame::_new_kittest();
            for _ in 0..3 {
                context
                    .run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(width, height),
                            )),
                            ..Default::default()
                        },
                        |ui| app.ui(ui, &mut frame),
                    )
                    .textures_delta
                    .clear();
            }
            let snapshot = app.test_snapshot();
            assert!(
                snapshot.ui_snapshot.semantic_audit.is_empty(),
                "{width}: {:?}",
                snapshot.ui_snapshot.semantic_audit
            );
            assert!(
                snapshot.ui_snapshot.text_audit.is_empty(),
                "{width}: {:?}",
                snapshot.ui_snapshot.text_audit
            );
            let pane = snapshot
                .ui_snapshot
                .nodes
                .iter()
                .find(|node| node.id == SemanticUiId::new("bokkie.settings"))
                .unwrap();
            let return_control = snapshot
                .ui_snapshot
                .nodes
                .iter()
                .find(|node| node.id == SemanticUiId::new("bokkie.settings.return"))
                .unwrap();
            let style = context.style_of(context.theme());
            let label_width = context.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(
                        "Return to conversation".into(),
                        egui::TextStyle::Button.resolve(&style),
                        style.visuals.text_color(),
                    )
                    .size()
                    .x
            });
            assert!(
                return_control.rect.max_x - return_control.rect.min_x
                    >= label_width + style.spacing.button_padding.x * 2.0 - 1.0,
                "Return label wrapped at {width}: {:?}",
                return_control.rect
            );
            assert!(
                return_control.rect.min_x >= pane.rect.min_x
                    && return_control.rect.max_x <= pane.rect.max_x,
                "Return control escaped Settings pane at {width}"
            );
            for id in [
                "bokkie.settings.open",
                "bokkie.settings.return",
                "bokkie.settings.model",
                "bokkie.settings.effort",
                "bokkie.settings.instructions",
                "bokkie.settings.advanced",
                "bokkie.settings.adviser",
                "bokkie.settings.save",
                "bokkie.settings.reload",
            ] {
                let node = snapshot
                    .ui_snapshot
                    .nodes
                    .iter()
                    .find(|node| node.id == SemanticUiId::new(id))
                    .unwrap_or_else(|| panic!("missing {id} at {width}"));
                let rect = egui::Rect::from_min_max(
                    egui::pos2(node.rect.min_x, node.rect.min_y),
                    egui::pos2(node.rect.max_x, node.rect.max_y),
                );
                assert!(
                    rect.left() >= 0.0
                        && rect.right() <= width
                        && rect.top() >= 0.0
                        && rect.bottom() <= height,
                    "{id}: {rect:?} at {width}"
                );
            }
        }
    }
}
