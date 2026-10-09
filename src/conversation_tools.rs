//! Model-facing proposals. Store commands and operator confirmation own effects.
use crate::{StoreError, conversation::ConversationOperation};
use bokkie_operator_api::{ConversationAction, ManagedTaskDefinition, ManagedTrigger};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn tools(managed_selected: bool, legacy_selected: bool) -> Value {
    tools_with_adviser(managed_selected, legacy_selected, false)
}

pub fn tools_with_adviser(managed_selected: bool, legacy_selected: bool, automatic: bool) -> Value {
    let text = |description: &str| json!({"type":"string","description":description});
    let object = |properties: Value, required: &[&str]| json!({"type":"object","properties":properties,"required":required,"additionalProperties":false});
    let tool = |name: &str, description: &str, input: Value| json!({"type":"function","name":name,"description":description,"inputSchema":input,"deferLoading":false});
    let trigger = json!({"description":"When the task should occur. Use the configured IANA time zone. Cron is internal: weekdays 9am is '0 9 * * Mon-Fri', Monday 9am is '0 9 * * Mon'. Immediate requires an explicit request for now.","anyOf":[
        object(json!({"kind":{"type":"string","const":"immediate"}}),&["kind"]),
        object(json!({"kind":{"type":"string","const":"once"},"local_datetime":text("Local YYYY-MM-DDTHH:MM, resolved from the supplied clock."),"timezone":text("IANA time zone, for example Australia/Adelaide.")}),&["kind","local_datetime","timezone"]),
        object(json!({"kind":{"type":"string","const":"recurring"},"cron":text("Five-field cron expression expressing the requested recurrence."),"timezone":text("IANA time zone, for example Australia/Adelaide.")}),&["kind","cron","timezone"])
    ]});
    let mut result = vec![
        tool(
            "bokkie_prepare_handoff",
            "Prepare a concise DEVELOPMENT hand-off to an existing project workspace when the operator asks to implement work there or prepare a hand-off. This is separate from scheduled tasks and engineering supervision. Supply a short project identifying phrase and relevant brief fields. The backend resolves registrations and the operator selects the exact destination, edits and saves. Never invent a project identity or launch route. No task, worker, execution acceptance or permission is created. Ask for missing outcome or project through bokkie_discuss when necessary.",
            object(
                json!({
                    "project_query":text("Project name or identifying phrase explicitly requested by the operator; not a guessed destination."),
                    "brief":object(json!({
                        "outcome":text("Requested development outcome, concise and concrete."),
                        "context":text("Only relevant decisions and context from this discussion; no transcript, credentials or unrelated private material. Empty when absent."),
                        "constraints":text("Scope boundaries and constraints. Preserve the operator's restrictions; do not grant new permissions."),
                        "acceptance":text("Concrete checkable acceptance criteria for the receiving workspace."),
                        "references":{"type":"array","maxItems":12,"items":text("Relevant HTTP/HTTPS source link actually supplied in the discussion. Never invent links; [] when absent.")}
                    }), &["outcome","context","constraints","acceptance","references"])
                }),
                &["project_query", "brief"],
            ),
        ),
        tool(
            "bokkie_discuss",
            "Discuss an exploratory idea, answer feedback or ask about material missing information. This does NOT save a draft. For a sufficiently specified request to create or refine a task, use bokkie_save_draft instead. A request to keep a task inactive still permits saving its draft.",
            object(
                json!({
                    "message":text("Helpful concise discussion or a necessary question; never claim a change was saved or activated."),
                    "reason":{"type":"string","enum":["exploration","clarification","feedback","answer"]}
                }),
                &["message", "reason"],
            ),
        ),
        tool(
            "bokkie_lookup",
            "Find existing tasks by a short identifying phrase. Use when the user asks to find existing work. Returned candidates require explicit operator selection; this call cannot edit a task. A sufficiently specified NEW reminder can be saved directly without a lookup.",
            object(
                json!({"query":text("Short task name, identity or distinctive descriptive words.")}),
                &["query"],
            ),
        ),
    ];
    if !legacy_selected {
        result.push(tool("bokkie_save_draft", "Save a NEW inactive task or a candidate revision of the selected managed task. Use for requests such as 'set up a weekday reminder, don’t activate it yet', text refinements, or 'make it Monday mornings'. This never activates or changes active behaviour. Supply the complete intended user-facing definition, preserving unchanged selected fields. The backend supplies profile identity, permitted effects, result destination and finite execution defaults. Research/email ideas may be saved but their unavailable capability blocks activation.", object(json!({
            "name":text("Concise task name used for later discovery."),
            "purpose":text("What the task is intended to achieve."),
            "instructions":text("For reminder or local_note, the exact text to bring back to the operator. For unavailable capabilities, describe intended behaviour without claiming it can execute."),
            "capability":{"type":"string","enum":["reminder","local_note","research_finder","email_monitor","unavailable"],"description":"Use reminder for a notification when due; its reviewed destination is supplied by the backend. Use local_note only when a local in-app note is requested. Preserve the selected capability on ordinary revisions. Research retrieval and email monitoring must use their own unavailable capabilities."},
            "trigger":trigger,
            "context_refs":{"type":"array","items":text("A relevant context reference; references are retained as data, never fetched or executed."),"maxItems":20}
        }), &["name","purpose","instructions","capability","trigger","context_refs"])));
    }
    if managed_selected {
        result.push(tool("bokkie_preview", "Show what the selected saved draft would do, including availability, differences and next occurrences. No execution or activation. Use when asked what will happen.", object(json!({}), &[])));
        result.push(tool("bokkie_propose", "Prepare an operator review card for activation, pause or resume of the selected task. This does not perform the action. A model-generated yes or approval never confirms it.", object(json!({"action":{"type":"string","enum":["activate","pause","resume"]}}), &["action"])));
    }
    if automatic {
        let schema = &mut result
            .iter_mut()
            .find(|t| t["name"] == "bokkie_discuss")
            .unwrap()["inputSchema"];
        schema["properties"]["reason"]["enum"]
            .as_array_mut()
            .unwrap()
            .push(json!("difficulty"));
        schema["properties"]["difficulty"] = json!({"type":"object","description":"Use only for two incompatible explicit requirements in current_request that Bokkie cannot reconcile. Do not use for missing information, unavailable capabilities, ordinary complexity or suggested adviser use.","properties":{
            "condition":{"type":"string","const":"conflicting_requirements"},
            "question":{"type":"string","minLength":1,"maxLength":1024},
            "requirements":{"type":"array","minItems":2,"maxItems":2,"uniqueItems":true,"items":{"type":"string","minLength":1,"maxLength":512}}
        },"required":["condition","question","requirements"],"additionalProperties":false});
    }
    let mut workspace_tool = result[0].clone();
    workspace_tool["name"] = json!("bokkie_workspace_task");
    workspace_tool["description"] = json!(
        "Create or revise a visible workspace task when asked to implement work in a selected project. Supply its explicit project phrase, outcome, relevant context, scope/constraints and checkable acceptance. The receiving workspace owns its normal planning, verification, independent review and delivery. Bokkie saves the same versioned definition used by direct editing and prepares a review; it does not execute until the operator confirms. Routine decisions proceed inside the reviewed scope; missing information, inconclusive evidence or new authority require a question. For a finite cross-project pilot/assessment/rollout use the registered portfolio workspace and retain the whole agreed assignment. Use bokkie_prepare_handoff only when an optional manual brief/export is explicitly requested. Never invent destinations or permissions."
    );
    result.insert(0, workspace_tool);
    Value::Array(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolProposal {
    tool: String,
    arguments: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HandoffProposal {
    project_query: String,
    brief: bokkie_operator_api::HandoffBrief,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Draft {
    name: String,
    purpose: String,
    instructions: String,
    capability: Capability,
    trigger: ManagedTrigger,
    context_refs: Vec<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Capability {
    Reminder,
    LocalNote,
    ResearchFinder,
    EmailMonitor,
    Unavailable,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Discussion {
    message: String,
    reason: DiscussionReason,
    #[serde(default)]
    difficulty: Option<Difficulty>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Difficulty {
    condition: DifficultyCondition,
    question: String,
    requirements: [String; 2],
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum DifficultyCondition {
    ConflictingRequirements,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum DiscussionReason {
    Exploration,
    Clarification,
    Feedback,
    Answer,
    Difficulty,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lookup {
    query: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Propose {
    action: ConversationAction,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, StoreError> {
    serde_json::from_value(value).map_err(|error| {
        StoreError::Invalid(format!("invalid conversation tool proposal: {error}"))
    })
}

pub fn operation(
    output: Value,
    offered: &Value,
    base: Option<&ManagedTaskDefinition>,
) -> Result<ConversationOperation, StoreError> {
    operation_with_profiles(output, offered, base, &[])
}

pub fn operation_with_profiles(
    output: Value,
    offered: &Value,
    base: Option<&ManagedTaskDefinition>,
    profiles: &[bokkie_operator_api::ManagedCapabilityProfile],
) -> Result<ConversationOperation, StoreError> {
    operation_with_adviser(output, offered, base, profiles, "")
}

pub fn operation_with_adviser(
    output: Value,
    offered: &Value,
    base: Option<&ManagedTaskDefinition>,
    profiles: &[bokkie_operator_api::ManagedCapabilityProfile],
    current_request: &str,
) -> Result<ConversationOperation, StoreError> {
    let proposal: ToolProposal = decode(output)?;
    if !offered
        .as_array()
        .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == proposal.tool))
    {
        return Err(StoreError::Invalid(
            "conversation selected an unavailable tool".into(),
        ));
    }
    match proposal.tool.as_str() {
        "bokkie_prepare_handoff" | "bokkie_workspace_task" => {
            let draft: HandoffProposal = decode(proposal.arguments)?;
            crate::handoffs::validate_brief(&draft.brief)?;
            if draft.project_query.trim().is_empty()
                || draft.project_query.len() > 160
                || draft.project_query.contains('\0')
            {
                return Err(StoreError::Invalid(
                    "Supply a bounded explicit project query".into(),
                ));
            }
            if proposal.tool == "bokkie_workspace_task" {
                Ok(ConversationOperation::PrepareWorkspace {
                    project_query: draft.project_query,
                    brief: Box::new(draft.brief),
                })
            } else {
                Ok(ConversationOperation::PrepareHandoff {
                    project_query: draft.project_query,
                    brief: Box::new(draft.brief),
                })
            }
        }
        "bokkie_discuss" => {
            let Discussion {
                message,
                reason,
                difficulty,
            } = decode(proposal.arguments)?;
            match (reason, difficulty) {
                (
                    DiscussionReason::Difficulty,
                    Some(Difficulty {
                        condition: DifficultyCondition::ConflictingRequirements,
                        question,
                        requirements,
                    }),
                ) => {
                    let automatic = offered.as_array().is_some_and(|tools| {
                        tools.iter().any(|tool| {
                            tool["name"] == "bokkie_discuss"
                                && tool["inputSchema"]["properties"]
                                    .get("difficulty")
                                    .is_some()
                        })
                    });
                    let bounded = |value: &str, max: usize| {
                        !value.trim().is_empty() && value.len() <= max && !value.contains('\0')
                    };
                    if !automatic
                        || !bounded(&question, 1024)
                        || requirements
                            .iter()
                            .any(|quote| !bounded(quote, 512) || !current_request.contains(quote))
                        || requirements[0].trim() == requirements[1].trim()
                        || requirements[0].contains(&requirements[1])
                        || requirements[1].contains(&requirements[0])
                    {
                        return Err(StoreError::Invalid("Astra consultation requires two distinct bounded requirement quotes from the current request and an enabled difficulty condition".into()));
                    }
                    Ok(ConversationOperation::Consult {
                        question,
                        requirements,
                    })
                }
                (DiscussionReason::Difficulty, None) | (_, Some(_)) => Err(StoreError::Invalid(
                    "Difficulty must accompany only the difficulty discussion reason".into(),
                )),
                (_, None) => Ok(ConversationOperation::Discuss { message }),
            }
        }
        "bokkie_lookup" => {
            let Lookup { query } = decode(proposal.arguments)?;
            Ok(ConversationOperation::Lookup { query })
        }
        "bokkie_save_draft" => {
            let draft: Draft = decode(proposal.arguments)?;
            let capability = match draft.capability {
                Capability::Reminder => "reminder",
                Capability::LocalNote => "local_note",
                Capability::ResearchFinder => "research_finder",
                Capability::EmailMonitor => "email_monitor",
                Capability::Unavailable => "unavailable",
            };
            let creating = base.is_none_or(|current| current.capability != capability);
            let mut definition = base
                .filter(|current| current.capability == capability)
                .cloned()
                .unwrap_or_else(|| {
                    if capability == "reminder" {
                        let destination = profiles
                            .iter()
                            .find(|p| p.capability == "reminder")
                            .map_or("Not configured", |p| p.destination.as_str());
                        ManagedTaskDefinition::reminder(
                            &draft.name,
                            &draft.instructions,
                            destination,
                        )
                    } else {
                        ManagedTaskDefinition::local_note(&draft.name, &draft.instructions)
                    }
                });
            definition.name = draft.name;
            definition.purpose = draft.purpose;
            definition.instructions = draft.instructions;
            definition.trigger = draft.trigger;
            definition.context_refs = draft.context_refs;
            if capability == "reminder" && (creating || definition.destination == "Not configured")
            {
                if let Some(profile) = profiles.iter().find(|p| p.capability == "reminder") {
                    definition.destination = profile.destination.clone();
                    definition.profile_revision = profile.revision.clone();
                    definition.max_output_chars =
                        definition.max_output_chars.min(profile.max_output_chars);
                }
            }
            if !matches!(capability, "local_note" | "reminder") {
                definition.capability = capability.into();
                if base.is_none_or(|current| current.capability != capability) {
                    definition.profile_revision = "unavailable".into();
                    definition.effects.clear();
                }
            }
            Ok(ConversationOperation::SaveDefinition {
                definition: Box::new(definition),
                message: "Review its behaviour and timing below.".into(),
            })
        }
        "bokkie_preview" => {
            let _: Empty = decode(proposal.arguments)?;
            Ok(ConversationOperation::Preview)
        }
        "bokkie_propose" => {
            let Propose { action } = decode(proposal.arguments)?;
            Ok(ConversationOperation::Propose { action })
        }
        _ => Err(StoreError::Invalid("unknown conversation tool".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft() -> Value {
        json!({"tool":"bokkie_save_draft","arguments":{"name":"Queue review","purpose":"Choose a paper","instructions":"Read the queue.","capability":"local_note","trigger":{"kind":"recurring","cron":"0 9 * * Mon-Fri","timezone":"Australia/Adelaide"},"context_refs":[]}})
    }
    #[test]
    fn reminder_destination_is_backend_owned_and_preserved_on_revision() {
        let mut proposal = draft();
        proposal["arguments"]["capability"] = json!("reminder");
        let profiles = [bokkie_operator_api::ManagedCapabilityProfile::reminder(
            "fixture@example.invalid",
        )];
        let ConversationOperation::SaveDefinition { definition, .. } =
            operation_with_profiles(proposal.clone(), &tools(false, false), None, &profiles)
                .unwrap()
        else {
            panic!()
        };
        assert_eq!(definition.destination, "fixture@example.invalid");
        assert_eq!(definition.capability, "reminder");
        assert_eq!(
            definition.effects,
            ["store_local_result", "send_notification"]
        );
        let other = [bokkie_operator_api::ManagedCapabilityProfile::reminder(
            "other@example.invalid",
        )];
        let ConversationOperation::SaveDefinition {
            definition: edited, ..
        } = operation_with_profiles(
            proposal.clone(),
            &tools(true, false),
            Some(&definition),
            &other,
        )
        .unwrap()
        else {
            panic!()
        };
        assert_eq!(edited.destination, "fixture@example.invalid");
        proposal["arguments"]["destination"] = json!("unrequested@example.invalid");
        assert!(operation_with_profiles(proposal, &tools(false, false), None, &profiles).is_err());
    }
    #[test]
    fn defaults_and_authority_are_owned_by_backend() {
        let ConversationOperation::SaveDefinition { definition, .. } =
            operation(draft(), &tools(false, false), None).unwrap()
        else {
            panic!()
        };
        assert_eq!(definition.profile_revision, "local-note-v1");
        assert_eq!(definition.effects, ["store_local_result"]);
        assert_eq!(definition.destination, "task_results");
        assert_eq!(
            (definition.max_attempts, definition.max_output_chars),
            (3, 8192)
        );
        for field in [
            "actor",
            "task_id",
            "effects",
            "profile_revision",
            "max_attempts",
            "approved",
        ] {
            let mut forged = draft();
            forged["arguments"][field] = json!("forged");
            assert!(
                operation(forged, &tools(false, false), None).is_err(),
                "{field}"
            );
        }
    }
    #[test]
    fn unavailable_drafts_do_not_acquire_note_effects_and_legacy_cannot_convert() {
        let mut proposal = draft();
        proposal["arguments"]["capability"] = json!("email_monitor");
        let ConversationOperation::SaveDefinition { definition, .. } =
            operation(proposal.clone(), &tools(false, false), None).unwrap()
        else {
            panic!()
        };
        assert_eq!(definition.profile_revision, "unavailable");
        assert!(definition.effects.is_empty());
        assert!(operation(proposal, &tools(false, true), None).is_err());
        assert!(
            operation(
                json!({"tool":"bokkie_preview","arguments":{}}),
                &tools(false, false),
                None
            )
            .is_err()
        );
        assert!(
            operation(
                json!({"tool":"bokkie_propose","arguments":{"action":"activate","approved":true}}),
                &tools(true, false),
                None
            )
            .is_err()
        );
    }
    #[test]
    fn revision_preserves_existing_trusted_bounds_and_profile() {
        let mut current = ManagedTaskDefinition::local_note("Existing", "Original");
        current.max_attempts = 2;
        current.max_output_chars = 512;
        let ConversationOperation::SaveDefinition { definition, .. } =
            operation(draft(), &tools(true, false), Some(&current)).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            (definition.max_attempts, definition.max_output_chars),
            (2, 512)
        );
        assert_eq!(definition.profile_revision, current.profile_revision);
        assert_eq!(definition.instructions, "Read the queue.");
    }
    #[test]
    fn automatic_difficulty_requires_two_distinct_current_request_quotes_and_closed_fields() {
        let request = "Only at 9 am. Only at 10 am.";
        let proposal = json!({"tool":"bokkie_discuss","arguments":{"message":"I cannot reconcile these requirements","reason":"difficulty","difficulty":{"condition":"conflicting_requirements","question":"Which takes priority?","requirements":["Only at 9 am","Only at 10 am"]}}});
        let offered = tools_with_adviser(false, false, true);
        assert!(matches!(
            operation_with_adviser(proposal.clone(), &offered, None, &[], request),
            Ok(ConversationOperation::Consult { .. })
        ));
        assert!(
            operation_with_adviser(proposal.clone(), &tools(false, false), None, &[], request)
                .is_err()
        );
        for quotes in [
            json!(["Only at 9 am", "Only at 9 am"]),
            json!(["Only at 9 am", "Invented requirement"]),
            json!(["Only at 9 am"]),
            json!(["Only at 9 am", "Only at 10 am", "third"]),
            json!(["Only at 9 am", "at 9 am"]),
        ] {
            let mut invalid = proposal.clone();
            invalid["arguments"]["difficulty"]["requirements"] = quotes;
            assert!(operation_with_adviser(invalid, &offered, None, &[], request).is_err());
        }
        for (field, value) in [
            ("condition", json!("ordinary_complexity")),
            ("question", json!("")),
            ("approved", json!(true)),
        ] {
            let mut invalid = proposal.clone();
            invalid["arguments"]["difficulty"][field] = value;
            assert!(operation_with_adviser(invalid, &offered, None, &[], request).is_err());
        }
        let mut invalid = proposal.clone();
        invalid["arguments"]["reason"] = json!("answer");
        assert!(operation_with_adviser(invalid, &offered, None, &[], request).is_err());
        let mut invalid = proposal;
        invalid["arguments"]
            .as_object_mut()
            .unwrap()
            .remove("difficulty");
        assert!(operation_with_adviser(invalid, &offered, None, &[], request).is_err());
    }
}
