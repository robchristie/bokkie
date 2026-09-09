use super::*;
use crate::model::TaskSettingsDraft;
use bokkie_operator_api::{InstructionMode, OperatorTaskKind, TaskConfigurationUpdate};

fn body(ui: &mut egui::Ui, key: &str, value: &str, presentation: &mut PresentationContext) {
    presentation.content(
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
}

fn button(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    enabled: bool,
    presentation: &mut PresentationContext,
) -> bool {
    let (response, ()) = ui
        .push_id(id, |ui| {
            presentation.native(ui, NativeTextControlKind::Button, |ui| {
                (ui.add_enabled(enabled, egui::Button::new(label)), ())
            })
        })
        .inner;
    let mut node = UiNode::container(
        SemanticUiId::new(id),
        Some(
            if id.starts_with("bokkie.task.settings.") && id != "bokkie.task.settings.edit" {
                SemanticUiId::new("bokkie.task.settings")
            } else {
                SemanticUiId::pane(TIMELINE_PANE_ID)
            },
        ),
        UiRole::Button,
        response.rect.into(),
    );
    node.name = label.to_owned();
    node.enabled = enabled;
    node.focused = response.has_focus();
    presentation.observe_node(ui, node);
    response.clicked()
}

fn open_task(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    parent: bool,
    intents: &mut Vec<OperatorIntent>,
    presentation: &mut PresentationContext,
) {
    let semantic = format!(
        "bokkie.task.{}.{id}",
        if parent { "parent" } else { "open" }
    );
    if button(ui, &semantic, label, true, presentation) {
        intents.push(OperatorIntent::Select {
            obligation_id: id.to_owned(),
            destination: Some(TIMELINE_PANE_ID),
        });
    }
}

pub(super) fn show_task_detail(
    ui: &mut egui::Ui,
    read: &TimelineReadModel<'_>,
    obligation: &OperatorObligation,
    intents: &mut Vec<OperatorIntent>,
    presentation: &mut PresentationContext,
) {
    let Some(task) = &obligation.task else {
        return;
    };
    if let Some(parent_id) = &task.parent_task_id
        && let Some(parent) = read
            .related_obligations
            .iter()
            .find(|item| item.id == *parent_id)
    {
        open_task(
            ui,
            parent_id,
            &format!("Back to {}", attention_title(parent)),
            true,
            intents,
            presentation,
        );
    }
    if let Some(engineering) = &task.engineering {
        presentation.heading(ui, "engineering-heading", "Engineering outcome");
        presentation.property_row(
            ui,
            "engineering-owner",
            "Responsibility",
            &engineering.responsibility,
        );
        full_text(
            ui,
            "engineering-next",
            &engineering.next_action,
            presentation,
        );
        full_text(
            ui,
            "engineering-acceptance",
            &engineering.acceptance,
            presentation,
        );
        egui::CollapsingHeader::new("Current request")
            .id_salt(("engineering-intent", &obligation.id))
            .show(ui, |ui| {
                full_text(ui, "engineering-intent", &engineering.intent, presentation);
            });
        if engineering.criteria.is_empty() {
            body(
                ui,
                "engineering-criteria-pending",
                "Bokkie will formalise the acceptance criteria from your intent.",
                presentation,
            );
        }
        for (index, criterion) in engineering.criteria.iter().enumerate() {
            full_text(
                ui,
                &format!("engineering-criterion-{index}"),
                criterion,
                presentation,
            );
        }
        let enabled = read.connection.decisions_safe()
            && !read.action_busy
            && !read.snapshot_busy
            && engineering.accepts_follow_up;
        if button(
            ui,
            "bokkie.engineering.follow-up",
            "Follow up or change request",
            enabled,
            presentation,
        ) {
            intents.push(OperatorIntent::FollowUpEngineering {
                expected: engineering.expected.clone(),
                question_id: None,
                prompt: None,
            });
        }
        if button(
            ui,
            "bokkie.engineering.cancel",
            "Cancel outcome…",
            enabled,
            presentation,
        ) {
            intents.push(OperatorIntent::CancelEngineering {
                expected: engineering.expected.clone(),
            });
        }
        if engineering.earlier_questions > 0 {
            body(
                ui,
                "engineering-earlier-questions",
                &format!(
                    "{} earlier questions retained in the durable record",
                    engineering.earlier_questions
                ),
                presentation,
            );
        }
        for question in &engineering.questions {
            full_text(
                ui,
                &format!("engineering-question-{}", question.id),
                &question.prompt,
                presentation,
            );
            if let Some(answer) = &question.answer {
                full_text(
                    ui,
                    &format!("engineering-answer-{}", question.id),
                    answer,
                    presentation,
                );
            } else if question.needs_operator {
                if button(
                    ui,
                    &format!("bokkie.engineering.answer.{}", question.id),
                    "Answer this decision",
                    enabled,
                    presentation,
                ) {
                    intents.push(OperatorIntent::FollowUpEngineering {
                        expected: engineering.expected.clone(),
                        question_id: Some(question.id.clone()),
                        prompt: Some(question.prompt.clone()),
                    });
                }
            } else {
                body(
                    ui,
                    &format!("engineering-routine-{}", question.id),
                    "Bokkie supervisor is responsible for this question.",
                    presentation,
                );
            }
        }
        egui::CollapsingHeader::new("Conversation")
            .id_salt(("engineering-conversation", &obligation.id))
            .show(ui, |ui| {
                if engineering.earlier_messages > 0 {
                    ui.label(format!(
                        "{} earlier messages retained in the durable record",
                        engineering.earlier_messages
                    ));
                }
                for (index, message) in engineering.messages.iter().enumerate() {
                    full_text(
                        ui,
                        &format!("engineering-message-{index}"),
                        &format!("{}: {}", message.actor, message.text),
                        presentation,
                    );
                }
            });
    }
    if let Some(configuration) = &task.configuration {
        ui.separator();
        presentation.heading(ui, "settings-heading", "Task settings");
        presentation.property_row(
            ui,
            "task-repository",
            "Repository",
            &format!(
                "{} · {}",
                configuration.repository, configuration.default_branch
            ),
        );
        presentation.property_row(
            ui,
            "task-schedule",
            "Schedule",
            &format!(
                "{} · {}",
                configuration.inspection_cron, configuration.inspection_timezone
            ),
        );
        body(
            ui,
            "task-policy",
            &configuration.approval_policy,
            presentation,
        );
        presentation.property_row(
            ui,
            "task-proposal-limit",
            "Proposal limit",
            &configuration.proposal_limit.to_string(),
        );
        row_line(
            ui,
            "guidance-mode",
            &format!(
                "{} · settings revision {}",
                mode_label(configuration.instruction_mode),
                configuration.revision
            ),
            TextRole::Secondary,
            presentation,
        );
        egui::CollapsingHeader::new("Effective inspection guidance")
            .id_salt(("task-guidance", &obligation.id))
            .show(ui, |ui| {
                presentation.heading(ui, "defaults-heading", "Default guidance");
                full_text(
                    ui,
                    "default-guidance",
                    &configuration.default_instructions,
                    presentation,
                );
                presentation.heading(
                    ui,
                    "task-instructions-heading",
                    mode_label(configuration.instruction_mode),
                );
                full_text(
                    ui,
                    "task-instructions",
                    if configuration.instructions.is_empty() {
                        "No task-specific instructions"
                    } else {
                        &configuration.instructions
                    },
                    presentation,
                );
                presentation.heading(
                    ui,
                    "effective-heading",
                    "Effective guidance for new inspections",
                );
                full_text(
                    ui,
                    "effective-guidance",
                    &configuration.effective_instructions,
                    presentation,
                );
                presentation.property_row(
                    ui,
                    "task-checkout",
                    "Registered checkout",
                    &configuration.checkout_path,
                );
            });
        let enabled = read.connection.decisions_safe()
            && !read.action_busy
            && !read.snapshot_busy
            && !read.loading
            && obligation.state != bokkie_operator_api::OperatorObligationState::Running;
        if button(
            ui,
            "bokkie.task.settings.edit",
            "Edit inspection guidance",
            enabled,
            presentation,
        ) {
            intents.push(OperatorIntent::BeginSettings);
        }
        if !enabled {
            row_line(
                ui,
                "settings-unavailable",
                if obligation.state == bokkie_operator_api::OperatorObligationState::Running {
                    "Wait for the active inspection to finish before editing guidance"
                } else {
                    "Refresh current task state to edit guidance"
                },
                TextRole::Secondary,
                presentation,
            );
        }
    }
    if task.kind == OperatorTaskKind::GardenerInspection {
        show_inspections(ui, read, presentation);
        ui.separator();
        presentation.heading(ui, "proposals-heading", "Proposals and resulting work");
        row_line(
            ui,
            "immutable-proposals",
            "Each proposal retains its inspected source and exact prompt. Settings edits apply to future inspections.",
            TextRole::Secondary,
            presentation,
        );
        let proposals = read
            .topic
            .into_iter()
            .flat_map(|topic| topic.items.iter().rev())
            .filter(|item| item.source == TopicSource::GardenerProposalInstance)
            .collect::<Vec<_>>();
        if proposals.is_empty() {
            body(
                ui,
                "no-proposals",
                if read.loading {
                    "Loading proposals…"
                } else {
                    "No source-bound proposals recorded yet"
                },
                presentation,
            );
        }
        for item in proposals {
            presentation.scoped(
                ui,
                ("task-proposal", &item.stable_id),
                |ui, presentation| {
                    let evidence = &item.evidence;
                    let child_id = string(evidence, "implementation_obligation_id");
                    let child = read.related_obligations.iter().find(|child| {
                        child.id == child_id
                            && child.task.as_ref().is_some_and(|task| {
                                task.parent_task_id.as_deref() == Some(&obligation.id)
                                    && task.proposal_instance_id.as_deref()
                                        == evidence.get("id").and_then(Value::as_str)
                            })
                    });
                    ui.add_space(8.0);
                    body(ui, "prompt", string(evidence, "prompt"), presentation);
                    let state = child
                        .map(|item| item.state.label())
                        .unwrap_or("Recorded proposal");
                    row_line(
                        ui,
                        "proposal-state",
                        &format!(
                            "{} · generation {} · source {}",
                            state,
                            evidence
                                .get("generation")
                                .map(value_text)
                                .unwrap_or_default(),
                            short_commit(string(evidence, "source_commit"))
                        ),
                        TextRole::Secondary,
                        presentation,
                    );
                    if let Some(child) = child {
                        open_task(
                            ui,
                            &child.id,
                            "Open implementation task",
                            false,
                            intents,
                            presentation,
                        );
                    }
                    egui::CollapsingHeader::new("Exact proposal and source")
                        .id_salt(("task-proposal-source", &item.stable_id))
                        .show(ui, |ui| {
                            full_text(
                                ui,
                                "immutable-prompt",
                                string(evidence, "prompt"),
                                presentation,
                            );
                            for (key, label) in [
                                ("id", "Proposal instance"),
                                ("source_commit", "Source commit"),
                                ("source_inspection_id", "Inspection"),
                                ("proposal_fingerprint", "Goal fingerprint"),
                            ] {
                                presentation.property_row(ui, key, label, string(evidence, key));
                            }
                        });
                },
            );
        }
    } else if task.kind == OperatorTaskKind::GardenerImplementation {
        if exact_gardener_subject(obligation).is_none()
            && let Some(proposal) =
                read.topic
                    .into_iter()
                    .flat_map(|topic| &topic.items)
                    .find(|item| {
                        item.source == TopicSource::GardenerProposalInstance
                            && item.evidence.get("id").and_then(Value::as_str)
                                == task.proposal_instance_id.as_deref()
                    })
        {
            presentation.heading(ui, "approved-prompt-heading", "Immutable proposal prompt");
            full_text(
                ui,
                "approved-proposal-prompt",
                string(&proposal.evidence, "prompt"),
                presentation,
            );
        }
        show_results(ui, read, presentation);
    } else {
        body(
            ui,
            "simulated-execution",
            "This task uses simulated execution. Its lifecycle and evidence are durable; it does not run a coding agent.",
            presentation,
        );
    }
}

fn show_inspections(
    ui: &mut egui::Ui,
    read: &TimelineReadModel<'_>,
    presentation: &mut PresentationContext,
) {
    ui.separator();
    presentation.heading(ui, "inspection-heading", "Latest inspection");
    let inspections = read
        .topic
        .into_iter()
        .flat_map(|topic| topic.items.iter().rev())
        .filter(|item| item.source == TopicSource::GardenerInspection)
        .collect::<Vec<_>>();
    if let Some(latest) = inspections.first() {
        show_inspection(
            ui,
            latest,
            read.topic.map(|topic| topic.captured_at),
            presentation,
        );
    } else {
        body(
            ui,
            "no-inspections",
            if read.loading {
                "Loading inspection runs…"
            } else {
                "No inspection has run yet"
            },
            presentation,
        );
    }
    if inspections.len() > 1 {
        egui::CollapsingHeader::new(format!("Inspection history · {} runs", inspections.len()))
            .id_salt("task-inspection-history")
            .show(ui, |ui| {
                for item in inspections.iter().skip(1) {
                    presentation.scoped(
                        ui,
                        ("inspection-history", &item.stable_id),
                        |ui, presentation| {
                            show_inspection(
                                ui,
                                item,
                                read.topic.map(|topic| topic.captured_at),
                                presentation,
                            )
                        },
                    );
                }
            });
    }
}

fn show_inspection(
    ui: &mut egui::Ui,
    item: &TopicItem,
    captured_at: Option<i64>,
    presentation: &mut PresentationContext,
) {
    let result = item
        .evidence
        .get("result_json")
        .and_then(Value::as_str)
        .and_then(|value| serde_json::from_str::<Value>(value).ok());
    let summary = result
        .as_ref()
        .and_then(|value| value.get("summary"))
        .and_then(Value::as_str)
        .unwrap_or(
            if item
                .evidence
                .get("completed_at")
                .is_some_and(|value| !value.is_null())
            {
                "Inspection completed"
            } else {
                "Inspection started; no completed result recorded"
            },
        );
    body(ui, "inspection-summary", summary, presentation);
    row_line(
        ui,
        "inspection-source",
        &format!(
            "Run {} · started {} · {} · source {}",
            item.occurrence.unwrap_or_default(),
            relative_time(item.occurred_at, captured_at),
            string(&item.evidence, "id"),
            short_commit(string(&item.evidence, "source_commit"))
        ),
        TextRole::Secondary,
        presentation,
    );
    if let Some(configuration) = item
        .evidence
        .get("configuration")
        .filter(|value| !value.is_null())
    {
        egui::CollapsingHeader::new("Guidance used for this inspection")
            .id_salt(("inspection-settings", &item.stable_id))
            .show(ui, |ui| {
                presentation.property_row(
                    ui,
                    "inspection-settings-revision",
                    "Settings revision",
                    &configuration
                        .get("revision")
                        .map(value_text)
                        .unwrap_or_default(),
                );
                full_text(
                    ui,
                    "inspection-effective-guidance",
                    string(configuration, "effective_instructions"),
                    presentation,
                );
            });
    }
}

fn show_results(
    ui: &mut egui::Ui,
    read: &TimelineReadModel<'_>,
    presentation: &mut PresentationContext,
) {
    ui.separator();
    presentation.heading(ui, "results-heading", "Results and verification");
    let runs = read
        .topic
        .into_iter()
        .flat_map(|topic| topic.items.iter().rev())
        .filter(|item| item.source == TopicSource::GardenerImplementationRun)
        .collect::<Vec<_>>();
    if runs.is_empty() {
        body(
            ui,
            "no-results",
            "No implementation run recorded yet",
            presentation,
        );
    }
    for item in runs {
        presentation.scoped(ui, ("task-result", &item.stable_id), |ui, presentation| {
            for (key, label) in [
                ("phase", "Run phase"),
                ("verification_verdict", "Verification"),
                ("verification_summary", "Verification summary"),
                ("verification_head", "Verified head"),
                ("pull_request_url", "Pull request"),
                ("pull_request_head", "Pull request head"),
                ("publication_state", "Publication state"),
            ] {
                if let Some(value) = item.evidence.get(key).filter(|value| !value.is_null()) {
                    presentation.property_row(ui, key, label, &value_text(value));
                }
            }
        });
    }
}

fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("Not recorded")
}
fn short_commit(value: &str) -> &str {
    value.get(..12).unwrap_or(value)
}
fn mode_label(mode: InstructionMode) -> &'static str {
    match mode {
        InstructionMode::Extend => "Add to default guidance",
        InstructionMode::Replace => "Replace default guidance",
    }
}
fn effective_guidance(defaults: &str, update: &TaskConfigurationUpdate) -> String {
    match update.instruction_mode {
        InstructionMode::Extend if update.instructions.is_empty() => defaults.to_owned(),
        InstructionMode::Extend => format!("{defaults}\n\n{}", update.instructions),
        InstructionMode::Replace => update.instructions.clone(),
    }
}

fn full_text(ui: &mut egui::Ui, key: &str, value: &str, presentation: &mut PresentationContext) {
    egui::ScrollArea::vertical()
        .id_salt(key)
        .max_height(180.0)
        .show(ui, |ui| {
            presentation.native(ui, NativeTextControlKind::Selectable, |ui| {
                (ui.add(egui::Label::new(value).wrap().selectable(true)), ())
            });
        });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn show_settings(
    context: &egui::Context,
    draft: &TaskSettingsDraft,
    model: &AppModel,
    tokens: &DesignTokens,
    font_scale: f32,
    intents: &mut Vec<OperatorIntent>,
    nodes: &mut Vec<UiNode>,
    text: &mut Vec<TextLayoutObservation>,
) {
    let mut edited = draft.clone();
    let configuration = model
        .selected()
        .and_then(|item| item.task.as_ref())
        .and_then(|task| task.configuration.as_ref());
    let unavailable = model.settings_unavailable_reason(draft);
    let window = egui::Window::new(if draft.reviewing { "Review task settings" } else { "Edit inspection guidance" })
        .id(egui::Id::new("bokkie-task-settings")).collapsible(false).resizable(false)
        .default_width(520.0).default_height(640.0).max_width((context.content_rect().width() - 32.0).max(240.0))
        .max_height(context.content_rect().height() - 48.0).vscroll(true)
        .show(context, |ui| {
            let mut presentation = PresentationContext::new(ui, *tokens, font_scale,
                PresentationScope::new("bokkie.task.settings"), SemanticUiId::new("bokkie.task.settings"));
            row_line(ui, "revision", &format!("Reviewing settings revision {}", draft.update.expected_revision), TextRole::Secondary, &mut presentation);
            body(ui, "settings-scope", "Guidance changes apply to future inspections. Repository, schedule, approval policy and runner safety remain fixed. Existing proposals and approved prompts retain their exact contents.", &mut presentation);
            if let Some(error) = &draft.error { body(ui, "settings-error", error, &mut presentation); }
            if draft.reviewing {
                presentation.heading(ui, "review-mode", mode_label(draft.update.instruction_mode));
                if let Some(configuration) = configuration {
                    full_text(ui, "review-effective", &effective_guidance(&configuration.default_instructions, &draft.update), &mut presentation);
                }
                presentation.property_row(ui, "review-actor", "Actor", &draft.update.actor);
                presentation.property_row(ui, "review-note", "Note", draft.update.note.as_deref().unwrap_or("No note"));
            } else {
                ui.add_enabled_ui(!model.action_busy, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for (mode, id) in [(InstructionMode::Extend, "extend"), (InstructionMode::Replace, "replace")] {
                            let response = ui.selectable_value(&mut edited.update.instruction_mode, mode, mode_label(mode));
                            observe_control(ui, &response, &format!("bokkie.task.settings.{id}"), mode_label(mode), &mut presentation);
                        }
                    });
                    ui.label("Task-specific instructions");
                    let response = ui.add(egui::TextEdit::multiline(&mut edited.update.instructions).id_salt("task-settings-instructions").desired_rows(7).desired_width(f32::INFINITY));
                    observe_control(ui, &response, "bokkie.task.settings.instructions", "Task-specific instructions", &mut presentation);
                    ui.label("Actor");
                    let response = ui.add(egui::TextEdit::singleline(&mut edited.update.actor).id_salt("task-settings-actor").desired_width(f32::INFINITY));
                    observe_control(ui, &response, "bokkie.task.settings.actor", "Actor", &mut presentation);
                    ui.label("Audit note (optional)");
                    let mut note = edited.update.note.clone().unwrap_or_default();
                    let response = ui.add(egui::TextEdit::singleline(&mut note).id_salt("task-settings-note").desired_width(f32::INFINITY));
                    observe_control(ui, &response, "bokkie.task.settings.note", "Audit note", &mut presentation);
                    edited.update.note = (!note.is_empty()).then_some(note);
                });
            }
            if let Some(reason) = &unavailable { body(ui, "settings-unavailable", reason, &mut presentation); }
            let changed_revision = configuration.is_some_and(|configuration| configuration.revision != draft.update.expected_revision);
            if draft.error.is_some() || changed_revision {
                let can_review = model.connection.decisions_safe() && !model.snapshot_busy && !model.topic_busy && !model.action_busy
                    && model.selected().is_some_and(|item| item.state != bokkie_operator_api::OperatorObligationState::Running);
                if button(ui, "bokkie.task.settings.review-current", "Review current settings", can_review, &mut presentation) {
                    intents.push(OperatorIntent::ReviewCurrentSettings);
                }
            }
            ui.horizontal_wrapped(|ui| {
                if draft.reviewing {
                    if button(ui, "bokkie.task.settings.save", "Save task settings", unavailable.is_none() && !model.action_busy, &mut presentation) {
                        intents.push(OperatorIntent::SubmitSettings);
                    }
                    if button(ui, "bokkie.task.settings.back", "Back to editing", !model.action_busy, &mut presentation) { edited.reviewing = false; }
                } else if button(ui, "bokkie.task.settings.review", "Review changes", unavailable.is_none() && !model.action_busy, &mut presentation) {
                    edited.reviewing = true;
                }
                if button(ui, "bokkie.task.settings.dismiss", "Close", !model.action_busy, &mut presentation) {
                    intents.push(OperatorIntent::DismissSettings);
                }
            });
            let observations = presentation.finish(ui);
            nodes.extend(observations.semantic_nodes);
            text.extend(observations.text_layouts);
        });
    if let Some(window) = window {
        let mut node = UiNode::container(
            SemanticUiId::new("bokkie.task.settings"),
            Some(SemanticUiId::root()),
            UiRole::Section,
            window.response.rect.into(),
        );
        node.name = if draft.reviewing {
            "Review task settings"
        } else {
            "Edit inspection guidance"
        }
        .to_owned();
        nodes.push(node);
    }
    if edited != *draft {
        intents.push(OperatorIntent::UpdateSettings(edited));
    }
}

fn observe_control(
    ui: &egui::Ui,
    response: &egui::Response,
    id: &str,
    label: &str,
    presentation: &mut PresentationContext,
) {
    let mut node = UiNode::container(
        SemanticUiId::new(id),
        Some(SemanticUiId::new("bokkie.task.settings")),
        UiRole::Section,
        response.rect.into(),
    );
    node.name = label.to_owned();
    node.focused = response.has_focus();
    node.enabled = response.enabled();
    presentation.observe_node(ui, node);
}
