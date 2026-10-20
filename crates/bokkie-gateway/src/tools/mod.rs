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
pub mod seen;
pub mod web_search;

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
    Tool {
        name: "seen_check",
        grant: Grant::Seen,
        description: "Check which items you have already shown your owner, by arXiv id, DOI or \
                      link, at most 50 at a time. Each comes back with its canonical id and \
                      whether it was seen, with the title it was shown under and how many \
                      days ago. Items are remembered for a year after you last showed them.",
        deadline: LOCAL_DEADLINE,
        schema: || {
            json!({
                "type": "object",
                "properties": {"items": {
                    "type": "array",
                    "description": "arXiv ids, DOIs or links, at most 50.",
                    "items": {"type": "string"},
                }},
                "required": ["items"],
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "seen_mark",
        grant: Grant::Seen,
        description: "Record items as shown to your owner now, by arXiv id, DOI or link, with \
                      their titles, at most 50 at a time. Mark what you showed, so that you \
                      do not show it again.",
        deadline: LOCAL_DEADLINE,
        schema: || {
            json!({
                "type": "object",
                "properties": {"items": {
                    "type": "array",
                    "description": "The items you showed, at most 50.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string",
                                   "description": "The item's arXiv id, DOI or link."},
                            "title": {"type": "string", "description": "Its title."},
                            "url": {"type": "string",
                                    "description": "The link you gave, if not the id."},
                        },
                        "required": ["id", "title"],
                        "additionalProperties": false,
                    },
                }},
                "required": ["items"],
                "additionalProperties": false,
            })
        },
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

/// A called tool's name for logs, with its namespace, bounded: the model
/// chose it.
pub fn display_name(tool: &str, namespace: Option<&str>) -> String {
    let name = match namespace {
        Some(namespace) => format!("{namespace}/{tool}"),
        None => tool.to_owned(),
    };
    name.chars().take(64).collect()
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
        "seen_check" => seen::check_tool(caller, arguments).await,
        "seen_mark" => seen::mark_tool(caller, arguments).await,
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
        "seen_check" | "seen_mark" => seen::summary(tool.name, arguments),
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
        let name = display_name(&call.tool, call.namespace.as_deref());
        // The thread names the Bot; the user is the app server's own. The
        // arguments name neither. A call from a thread with no Bot has no
        // owner to read its log entry, so only the gateway's log has it.
        let bot = match store::bot_by_thread(&self.db, user, &call.thread_id).await {
            Ok(Some(bot)) => bot,
            Ok(None) => {
                tracing::warn!(user = %user, tool = %name, "a tool call from a thread with no Bot");
                return DynamicToolCallResponse::text(false, NOT_AVAILABLE);
            }
            Err(failure) => {
                tracing::error!(user = %user, message = %failure.message, "resolving a tool call failed");
                return DynamicToolCallResponse::text(false, NOT_AVAILABLE);
            }
        };
        let Some(tool) = find(&call.tool, call.namespace.as_deref()) else {
            // The read loop lets through only the thread's own tools, so
            // this is a defence in depth; the Bot's owner sees it.
            tracing::warn!(user = %user, bot = %bot.id, tool = %name, "a call for an unknown tool");
            self.record(
                user,
                bot.id,
                call,
                &name,
                ToolCallOutcome::Refused,
                "unknown tool".into(),
                started,
            )
            .await;
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
        self.record(
            user,
            bot.id,
            call,
            tool.name,
            outcome,
            summary(tool, &call.arguments),
            started,
        )
        .await;
        tracing::info!(user = %user, bot = %bot.id, tool = tool.name, ?outcome, "served a tool call");
        match output {
            Ok(text) => DynamicToolCallResponse::text(true, text),
            Err(error) => DynamicToolCallResponse::text(false, error),
        }
    }

    /// Records a completed web search, item `item_id` of `user`'s thread,
    /// in its Bot's tool-call log. `granted` says whether the thread
    /// started with `web.search`; a search on any other thread is a
    /// violation that the app server's read loop has already acted on, and
    /// is recorded as refused.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_web_search(
        &self,
        user: UserId,
        thread_id: &str,
        turn_id: &str,
        item_id: &str,
        query: &str,
        action: Option<&Value>,
        granted: bool,
        duration: Duration,
    ) {
        let bot = match store::bot_by_thread(&self.db, user, thread_id).await {
            Ok(Some(bot)) => bot,
            Ok(None) => return,
            Err(failure) => {
                tracing::error!(user = %user, message = %failure.message, "resolving a web search failed");
                return;
            }
        };
        let outcome = if granted {
            ToolCallOutcome::Ok
        } else {
            ToolCallOutcome::Refused
        };
        let entry = audit::Entry {
            user,
            bot: bot.id,
            thread_id,
            turn_id,
            call_id: item_id,
            tool: web_search::TOOL,
            summary: web_search::summary(query, action),
            outcome,
            duration_ms: duration.as_millis() as u64,
        };
        if let Err(failure) = audit::record(&self.db, self.clock.now(), entry).await {
            tracing::error!(user = %user, bot = %bot.id, message = %failure.message, "recording a web search failed");
        }
        tracing::info!(user = %user, bot = %bot.id, ?outcome, "recorded a web search");
    }

    /// Records a call in its Bot's tool-call log.
    #[allow(clippy::too_many_arguments)]
    async fn record(
        &self,
        user: UserId,
        bot: BotId,
        call: &DynamicToolCallParams,
        tool: &str,
        outcome: ToolCallOutcome,
        summary: String,
        started: Instant,
    ) {
        let entry = audit::Entry {
            user,
            bot,
            thread_id: &call.thread_id,
            turn_id: &call.turn_id,
            call_id: &call.call_id,
            tool,
            summary,
            outcome,
            duration_ms: started.elapsed().as_millis() as u64,
        };
        if let Err(failure) = audit::record(&self.db, self.clock.now(), entry).await {
            tracing::error!(user = %user, bot = %bot, message = %failure.message, "recording a tool call failed");
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
        let seen: GrantSet = [Grant::Seen].into();
        let names: Vec<&str> = granted(&seen).map(|t| t.name).collect();
        assert_eq!(names, ["seen_check", "seen_mark"]);
        let both: GrantSet = [Grant::Seen, Grant::Notes].into();
        assert_eq!(granted(&both).count(), 5);
        // Grants without tools yet bring none, and web.search brings Codex's
        // own web search, not a Bokkie tool.
        for grant in [Grant::Notify, Grant::WebSearch, Grant::WebFetch] {
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

    #[test]
    fn called_names_in_logs_are_bounded() {
        assert_eq!(display_name("notes_read", None), "notes_read");
        assert_eq!(display_name("x", Some("web")), "web/x");
        assert_eq!(display_name(&"y".repeat(10_000), None).len(), 64);
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
