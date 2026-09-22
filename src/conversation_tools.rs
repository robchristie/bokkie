//! Model-facing proposals. Store commands and operator confirmation own effects.
use crate::{StoreError, conversation::ConversationOperation};
use bokkie_operator_api::{ConversationAction, ManagedTaskDefinition, ManagedTrigger};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn tools(managed_selected: bool, legacy_selected: bool) -> Value {
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
            "instructions":text("For local_note, the exact reminder text to display. For unavailable capabilities, describe intended behaviour without claiming it can execute."),
            "capability":{"type":"string","enum":["local_note","research_finder","email_monitor","unavailable"],"description":"local_note only stores supplied text in Bokkie. Research retrieval and email monitoring must use their own unavailable capabilities."},
            "trigger":trigger,
            "context_refs":{"type":"array","items":text("A relevant context reference; references are retained as data, never fetched or executed."),"maxItems":20}
        }), &["name","purpose","instructions","capability","trigger","context_refs"])));
    }
    if managed_selected {
        result.push(tool("bokkie_preview", "Show what the selected saved draft would do, including availability, differences and next occurrences. No execution or activation. Use when asked what will happen.", object(json!({}), &[])));
        result.push(tool("bokkie_propose", "Prepare an operator review card for activation, pause or resume of the selected task. This does not perform the action. A model-generated yes or approval never confirms it.", object(json!({"action":{"type":"string","enum":["activate","pause","resume"]}}), &["action"])));
    }
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
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum DiscussionReason {
    Exploration,
    Clarification,
    Feedback,
    Answer,
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
        "bokkie_discuss" => {
            let Discussion {
                message,
                reason: _reason,
            } = decode(proposal.arguments)?;
            Ok(ConversationOperation::Discuss { message })
        }
        "bokkie_lookup" => {
            let Lookup { query } = decode(proposal.arguments)?;
            Ok(ConversationOperation::Lookup { query })
        }
        "bokkie_save_draft" => {
            let draft: Draft = decode(proposal.arguments)?;
            let capability = match draft.capability {
                Capability::LocalNote => "local_note",
                Capability::ResearchFinder => "research_finder",
                Capability::EmailMonitor => "email_monitor",
                Capability::Unavailable => "unavailable",
            };
            let mut definition = base
                .filter(|current| current.capability == capability)
                .cloned()
                .unwrap_or_else(|| {
                    ManagedTaskDefinition::local_note(&draft.name, &draft.instructions)
                });
            definition.name = draft.name;
            definition.purpose = draft.purpose;
            definition.instructions = draft.instructions;
            definition.trigger = draft.trigger;
            definition.context_refs = draft.context_refs;
            if capability != "local_note" {
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
}
