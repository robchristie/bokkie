//! Direct editing uses the same immutable definition as conversation proposals.
use super::*;
use bokkie_operator_api::{
    WorkspaceAnswer, WorkspaceRun, WorkspaceRunActionRequest, WorkspaceTaskEditRequest,
};

pub(super) enum Editor {
    Task {
        conversation_id: String,
        task_id: String,
        revision: i64,
        definition: Box<ManagedTaskDefinition>,
        context: String,
    },
    Run {
        conversation_id: String,
        run: Box<WorkspaceRun>,
        answer: String,
        cancel: bool,
    },
}

impl Editor {
    pub fn task(conversation_id: String, task: &ManagedTaskDetail) -> Option<Self> {
        let definition = task
            .candidate
            .as_ref()
            .or(task.active.as_ref())?
            .definition
            .clone();
        Some(Self::Task {
            conversation_id,
            task_id: task.id.clone(),
            revision: task.configuration_revision,
            context: definition.context_refs.join("\n"),
            definition: Box::new(definition),
        })
    }

    pub fn draw(
        &mut self,
        context: &egui::Context,
        mutable: bool,
        nodes: &mut Vec<UiNode>,
    ) -> (bool, Option<ApiRequest>) {
        let mut open = true;
        let mut request = None;
        let width = (context.content_rect().width() - 32.0).clamp(250.0, 650.0);
        let height = (context.content_rect().height() - 90.0).clamp(
            240.0,
            if matches!(self, Self::Task { .. }) {
                800.0
            } else {
                360.0
            },
        );
        egui::Window::new(match self { Self::Task { .. } => "Edit task", Self::Run { cancel:true, .. } => "Stop this run", Self::Run { .. } => "Answer workspace question" })
            .open(&mut open).default_size(egui::vec2(width,height)).max_size(egui::vec2(width,height)).min_height(height).default_pos(context.content_rect().center()-egui::vec2(width/2.0,height/2.0)).resizable(false).collapsible(false)
            .show(context, |ui| {
                match self {
                    Self::Task { conversation_id, task_id, revision, definition, context: references } => {
                        egui::ScrollArea::vertical().max_height(height-150.0).auto_shrink([false,false]).show(ui, |ui| {
                        field(ui,"Name",&mut definition.name,"bokkie.task-editor.name",false,nodes);
                        field(ui,"Outcome",&mut definition.purpose,"bokkie.task-editor.outcome",true,nodes);
                        if let Some(workspace) = &mut definition.workspace {
                            ui.label(format!("Workspace: {} on {}",workspace.project.registration.name,workspace.project.registration.host));
                            field(ui,"Relevant context",&mut workspace.brief.context,"bokkie.task-editor.context",true,nodes);
                            field(ui,"Scope and constraints",&mut workspace.brief.constraints,"bokkie.task-editor.scope",true,nodes);
                            field(ui,"Completion criteria",&mut workspace.brief.acceptance,"bokkie.task-editor.acceptance",true,nodes);
                            field(ui,"Decision rules",&mut workspace.decision_rules,"bokkie.task-editor.decisions",true,nodes);
                            ui.label(format!("Permitted actions: {}",workspace.permitted_actions.join(", ")));
                            ui.horizontal(|ui| {
                                ui.label("Maximum run time (seconds)");
                                ui.add(egui::DragValue::new(&mut workspace.limits.max_seconds).range(1..=86_400));
                            });
                            ui.small("Limits and actions cannot exceed the configured workspace profile. Changing the workspace requires a fresh conversation proposal.");
                        } else {
                            field(ui,"Instructions",&mut definition.instructions,"bokkie.task-editor.instructions",true,nodes);
                        }
                        field(ui,"Context references (one per line)",references,"bokkie.task-editor.references",true,nodes);
                        });
                        ui.separator();
                        ui.small("Saving creates a candidate revision. Review and confirm it before future work changes. Admitted runs retain their original configuration.");
                        if button(ui,"bokkie.task-editor.save","Save proposed changes",mutable,nodes) {
                            definition.context_refs = references.lines().filter(|s|!s.trim().is_empty()).map(str::to_owned).collect();
                            if let Some(workspace) = &mut definition.workspace {
                                workspace.brief.outcome = definition.purpose.clone();
                                workspace.brief.references = definition.context_refs.clone();
                                workspace.criteria = vec![bokkie_operator_api::WorkspaceCriterion { id:"outcome".into(),description:workspace.brief.acceptance.clone() }];
                                definition.instructions = workspace.brief.context.clone();
                            }
                            request = Some(ApiRequest::EditTask { task_id:task_id.clone(), request:Box::new(WorkspaceTaskEditRequest {
                                command_id:engineering_command_id(),conversation_id:conversation_id.clone(),configuration_revision:*revision,definition:*definition.clone(),
                            })});
                        }
                    }
                    Self::Run { conversation_id, run, answer, cancel } => {
                        egui::ScrollArea::vertical().max_height(height-100.0).show(ui, |ui| {
                        if *cancel {
                            ui.label("Request this execution and its descendants to stop. Bokkie retains responsibility until the host confirms that they have stopped.");
                            ui.small("Future recurring occurrences are controlled separately by pausing the task.");
                        } else if let Some(question) = &run.question {
                            ui.add(egui::Label::new(&question.prompt).wrap().selectable(true));
                            for option in &question.options {
                                if ui.button(option).clicked() { *answer = option.clone(); }
                            }
                            field(ui,"Your answer",answer,"bokkie.workspace.answer-text",true,nodes);
                        }
                        });
                        ui.separator();
                        let allowed = mutable && (*cancel || !answer.trim().is_empty());
                        if button(ui,"bokkie.workspace.run-confirm",if *cancel {"Request stop"} else {"Send answer"},allowed,nodes) {
                            request = Some(ApiRequest::WorkspaceAction(WorkspaceRunActionRequest {
                                command_id:engineering_command_id(),conversation_id:conversation_id.clone(),execution_id:run.execution_id.clone(),
                                expected_event_sequence:run.last_event_sequence,cancel:*cancel,
                                answer:if *cancel {None} else {run.question.as_ref().map(|q|WorkspaceAnswer {question_id:q.id.clone(),text:answer.clone()})},
                            }));
                        }
                    }
                }
            });
        (open && request.is_none(), request)
    }
}

fn field(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    id: &str,
    multiline: bool,
    nodes: &mut Vec<UiNode>,
) {
    ui.label(label);
    let editor = if multiline {
        egui::TextEdit::multiline(value).desired_rows(3)
    } else {
        egui::TextEdit::singleline(value)
    };
    let response = ui.add(editor.id(egui::Id::new(id)).desired_width(f32::INFINITY));
    observe(response.rect, id, label, UiRole::Section, true, nodes);
}
