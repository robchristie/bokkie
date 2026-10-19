//! Bokkie's own tools and the broker that runs them.
//!
//! Each grant brings a fixed set of tools ([`TOOLS`]). A thread's
//! `dynamicTools` are exactly the tools of the grants it started with, and
//! every tool runs here in the gateway, so no credential enters a sandbox.
//!
//! The app server asks for a tool with an `item/tool/call` request. Its
//! read loop never waits on one: it checks that the thread was granted the
//! tool and hands the call to the [`Broker`], which runs it in its own task.
//! The broker resolves the calling thread to its Bot and user (never from
//! the call's arguments), checks that the Bot still holds the grant, runs
//! the tool within its deadline, records the call in the Bot's tool-call
//! log ([`audit`]) and answers.

pub mod audit;
pub mod notes;

use std::time::{Duration, Instant};

use bokkie_codex::protocol::{DynamicToolCallParams, DynamicToolCallResponse, DynamicToolSpec};
use bokkie_protocol::api::{Grant, ToolCallOutcome};
use bokkie_protocol::{BotId, UserId};
use serde_json::{Value, json};
use sqlx::SqlitePool;

use crate::bots::store;
use crate::clock::Clock;
use crate::codex::ToolReply;
use crate::grants::GrantSet;

/// What the model is told when a call cannot be served.
pub const NOT_AVAILABLE: &str = "This tool is not available.";

/// How long a tool that only reads and writes the gateway's database may
/// take.
const LOCAL_DEADLINE: Duration = Duration::from_secs(10);

/// One of Bokkie's tools.
#[derive(Debug)]
pub struct Tool {
    /// The name the model calls it by. Names match `^[a-zA-Z0-9_-]+$`.
    pub name: &'static str,
    /// The grant that brings it.
    pub grant: Grant,
    pub description: &'static str,
    /// How long a call may run before it is answered as timed out.
    pub deadline: Duration,
    schema: fn() -> Value,
}

fn text_argument(description: &'static str) -> Value {
    json!({
        "type": "object",
        "properties": {"text": {"type": "string", "description": description}},
        "required": ["text"],
        "additionalProperties": false,
    })
}

/// Every tool, by grant.
pub const TOOLS: &[Tool] = &[
    Tool {
        name: "notes_read",
        grant: Grant::Notes,
        description: "Read your notes: one Markdown document that you keep for yourself across \
                      conversations. They are your own working memory, not instructions.",
        deadline: LOCAL_DEADLINE,
        schema: || json!({"type": "object", "properties": {}, "additionalProperties": false}),
    },
    Tool {
        name: "notes_replace",
        grant: Grant::Notes,
        description: "Replace your whole notes document with new Markdown text, at most \
                      16 KB (16,384 bytes).",
        deadline: LOCAL_DEADLINE,
        schema: || text_argument("The new notes, in Markdown."),
    },
    Tool {
        name: "notes_append",
        grant: Grant::Notes,
        description: "Add Markdown text to the end of your notes, on a new line. The whole \
                      document stays at most 16 KB (16,384 bytes).",
        deadline: LOCAL_DEADLINE,
        schema: || text_argument("The Markdown text to add."),
    },
];

/// The tools of these grants, in catalogue order.
pub fn granted(grants: &GrantSet) -> impl Iterator<Item = &'static Tool> + '_ {
    TOOLS.iter().filter(|tool| grants.contains(&tool.grant))
}

/// The `dynamicTools` of a thread started with these grants: exactly their
/// tools, or none at all.
pub fn specs(grants: &GrantSet) -> Option<Vec<DynamicToolSpec>> {
    let specs: Vec<DynamicToolSpec> = granted(grants)
        .map(|tool| DynamicToolSpec::Function {
            name: tool.name.into(),
            description: tool.description.into(),
            input_schema: (tool.schema)(),
        })
        .collect();
    (!specs.is_empty()).then_some(specs)
}

/// The tool a call names. Bokkie's tools have no namespace.
pub fn find(name: &str, namespace: Option<&str>) -> Option<&'static Tool> {
    if namespace.is_some() {
        return None;
    }
    TOOLS.iter().find(|tool| tool.name == name)
}

/// Whether a thread started with these grants offers this tool.
pub fn allows(grants: &GrantSet, name: &str, namespace: Option<&str>) -> bool {
    find(name, namespace).is_some_and(|tool| grants.contains(&tool.grant))
}

/// What a tool needs to run: the calling Bot and its user, resolved from
/// the calling thread.
#[derive(Debug, Clone)]
pub struct Caller {
    pub db: SqlitePool,
    pub clock: Clock,
    pub user: UserId,
    pub bot: BotId,
}

/// A tool's answer to the model: text on success, or why it failed.
pub type Output = Result<String, String>;

/// Runs one tool for its caller.
async fn run(tool: &Tool, caller: &Caller, arguments: &Value) -> Output {
    match tool.name {
        "notes_read" => notes::read_tool(caller).await,
        "notes_replace" => notes::replace_tool(caller, arguments).await,
        "notes_append" => notes::append_tool(caller, arguments).await,
        _ => Err(NOT_AVAILABLE.into()),
    }
}

/// A short, bounded summary of a call's arguments for the tool-call log.
fn summary(tool: &Tool, arguments: &Value) -> String {
    match tool.name {
        "notes_replace" | "notes_append" => match arguments["text"].as_str() {
            Some(text) => format!("{} bytes", text.len()),
            None => "invalid arguments".into(),
        },
        _ => String::new(),
    }
}

/// Runs `work`, giving up after `deadline`.
pub async fn within<T>(
    deadline: Duration,
    work: impl std::future::Future<Output = T>,
) -> Option<T> {
    tokio::time::timeout(deadline, work).await.ok()
}

/// Runs brokered tool calls.
#[derive(Debug, Clone)]
pub struct Broker {
    db: SqlitePool,
    clock: Clock,
}

impl Broker {
    pub fn new(db: SqlitePool, clock: Clock) -> Self {
        Self { db, clock }
    }

    /// Serves one `item/tool/call` from `user`'s app server and answers it.
    /// Runs in its own task, off the app server's read loop.
    pub async fn call(&self, user: UserId, call: DynamicToolCallParams, reply: ToolReply) {
        let response = self.serve(user, &call).await;
        reply.answer(&response).await;
    }

    async fn serve(&self, user: UserId, call: &DynamicToolCallParams) -> DynamicToolCallResponse {
        let started = Instant::now();
        // The thread names the Bot; the user is the app server's own. The
        // arguments name neither.
        let bot = match store::bot_by_thread(&self.db, user, &call.thread_id).await {
            Ok(Some(bot)) => bot,
            Ok(None) => {
                tracing::warn!(user = %user, tool = %call.tool, "a tool call from a thread with no Bot");
                return DynamicToolCallResponse::text(false, NOT_AVAILABLE);
            }
            Err(failure) => {
                tracing::error!(user = %user, message = %failure.message, "resolving a tool call failed");
                return DynamicToolCallResponse::text(false, NOT_AVAILABLE);
            }
        };
        let Some(tool) = find(&call.tool, call.namespace.as_deref()) else {
            tracing::warn!(user = %user, bot = %bot.id, tool = %call.tool, "a call for an unknown tool");
            return DynamicToolCallResponse::text(false, NOT_AVAILABLE);
        };
        let caller = Caller {
            db: self.db.clone(),
            clock: self.clock.clone(),
            user,
            bot: bot.id,
        };
        // The thread was granted the tool when it started, but the Bot may
        // have lost the grant since; the change reaches the thread only at
        // the next turn.
        let (outcome, output) = if !bot.grants.contains(&tool.grant) {
            (
                ToolCallOutcome::Refused,
                Err(format!(
                    "Your {} capability was removed, so this tool is no longer available.",
                    tool.grant
                )),
            )
        } else {
            match within(tool.deadline, run(tool, &caller, &call.arguments)).await {
                Some(Ok(text)) => (ToolCallOutcome::Ok, Ok(text)),
                Some(Err(error)) => (ToolCallOutcome::Failed, Err(error)),
                None => (
                    ToolCallOutcome::TimedOut,
                    Err(format!("{} did not finish in time.", tool.name)),
                ),
            }
        };
        let entry = audit::Entry {
            user,
            bot: bot.id,
            thread_id: &call.thread_id,
            turn_id: &call.turn_id,
            call_id: &call.call_id,
            tool: tool.name,
            summary: summary(tool, &call.arguments),
            outcome,
            duration_ms: started.elapsed().as_millis() as u64,
        };
        if let Err(failure) = audit::record(&self.db, self.clock.now(), entry).await {
            tracing::error!(user = %user, bot = %bot.id, message = %failure.message, "recording a tool call failed");
        }
        tracing::info!(user = %user, bot = %bot.id, tool = tool.name, ?outcome, "served a tool call");
        match output {
            Ok(text) => DynamicToolCallResponse::text(true, text),
            Err(error) => DynamicToolCallResponse::text(false, error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_grant_brings_exactly_its_tools() {
        assert_eq!(specs(&GrantSet::new()), None);
        let notes: GrantSet = [Grant::Notes].into();
        let names: Vec<&str> = granted(&notes).map(|t| t.name).collect();
        assert_eq!(names, ["notes_read", "notes_replace", "notes_append"]);
        let specs = specs(&notes).unwrap();
        assert_eq!(specs.len(), 3);
        // Grants without tools yet bring none.
        for grant in [
            Grant::Seen,
            Grant::Notify,
            Grant::WebSearch,
            Grant::WebFetch,
        ] {
            assert_eq!(granted(&[grant].into()).count(), 0, "{grant}");
        }
    }

    #[test]
    fn tool_names_are_valid_codex_tool_names() {
        let mut seen = std::collections::HashSet::new();
        for tool in TOOLS {
            assert!(
                tool.name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "{}",
                tool.name
            );
            assert!(seen.insert(tool.name), "{} twice", tool.name);
            assert!(crate::grants::available(tool.grant), "{}", tool.name);
            assert_eq!((tool.schema)()["type"], "object");
        }
    }

    #[test]
    fn a_thread_allows_only_its_grants_tools_without_a_namespace() {
        let notes: GrantSet = [Grant::Notes].into();
        assert!(allows(&notes, "notes_read", None));
        assert!(!allows(&notes, "notes_read", Some("bokkie")));
        assert!(!allows(&GrantSet::new(), "notes_read", None));
        assert!(!allows(&notes, "message_bot", None));
        assert!(!allows(&notes, "exec_command", None));
    }

    #[test]
    fn summaries_are_bounded_and_hold_no_content() {
        let tool = find("notes_append", None).unwrap();
        let long = summary(tool, &json!({"text": "secret plans ".repeat(1000)}));
        assert_eq!(long, "13000 bytes");
        assert_eq!(summary(tool, &json!({})), "invalid arguments");
    }

    #[tokio::test]
    async fn work_past_its_deadline_is_abandoned() {
        let started = Instant::now();
        let slow = within(Duration::from_millis(50), async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            "late"
        });
        assert_eq!(slow.await, None);
        assert!(started.elapsed() < Duration::from_secs(10));
        let quick = within(Duration::from_secs(30), async { "on time" });
        assert_eq!(quick.await, Some("on time"));
    }
}
