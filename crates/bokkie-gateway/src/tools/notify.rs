//! The `notify` tool: a Bot tells its owner something, a result or a
//! question, with a short title and an optional body.
//!
//! A call records a notification for the Bot's user, with the Bot, the
//! routine run whose turn sent it (if a routine's) and its marker in the
//! conversation, adds that marker, and, outside a routine's run, queues
//! its push ([`crate::push::notified`]), all in one event batch. In a
//! run, the run's own push carries it when the run ends. The tool-call log
//! records the call with its kind and title. A Bot may record at most
//! [`PER_TURN`] notifications in a turn and its user [`PER_HOUR`] in an
//! hour. Notifications are kept [`RETENTION_MS`]; their markers stay with
//! the conversation.

use bokkie_protocol::api::{
    Author, MessageKind, MessageState, NOTIFICATION_BODY_CHARS, NOTIFICATION_TITLE_CHARS,
    Notification, NotificationKind,
};
use bokkie_protocol::events::Event;
use bokkie_protocol::{BotId, MessageId, NotificationId, RoutineRunId, Timestamp, UserId};
use serde_json::{Value, json};
use sqlx::Row;
use sqlx::sqlite::{SqliteConnection, SqliteExecutor};

use super::audit::{self, ID_CHARS, bounded};
use super::{Caller, Output};
use crate::bots::store::{self, NewMessage, notification_kind_from, notification_kind_name};
use crate::clock::{DAY_MS, HOUR_MS};
use crate::error::{Failure, Result};
use crate::events::EventLog;

/// The tool's name.
pub const TOOL: &str = "notify";

/// What the model is told the tool does.
pub const DESCRIPTION: &str = "Notify your owner: use it when you have a result they should \
     see, or a question only they can answer, and not for routine chatter. It adds a marker \
     to this conversation and is pushed to their devices that have notifications on, unless \
     they have this conversation open; in a routine's run, the run's one push carries it \
     when the run ends. Give a one-line title of at most 80 characters, an optional body of \
     at most 500, and the kind: result or question. At most 3 a turn. Your reply in the \
     conversation still carries the detail.";

/// What the model is told after a notification is recorded.
pub const ANSWER: &str = "Notification recorded. Your owner sees it in this conversation, \
     and it is pushed to their devices that have notifications on unless they have this \
     conversation open (in a routine's run, when the run ends).";

/// How many notifications a Bot may record in one turn.
pub const PER_TURN: i64 = 3;
/// How many notifications a user's Bots may record in an hour, together.
pub const PER_HOUR: i64 = 30;

/// Whether `c` is a character that changes how text around it is laid out
/// rather than showing itself: line and paragraph separators, the
/// bidirectional embeddings, overrides, isolates and marks, the byte order
/// mark and interlinear annotation characters. Notification text refuses
/// them. Joiners (U+200C, U+200D), which emoji and some scripts need, are
/// allowed.
pub fn is_layout_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
    )
}

/// How long notifications are kept.
pub const RETENTION_MS: i64 = 90 * DAY_MS;

/// The tool's arguments.
pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": {
                "type": "string",
                "description": "One line, at most 80 characters.",
            },
            "body": {
                "type": "string",
                "description": "At most 500 characters; may be empty.",
            },
            "kind": {
                "type": "string",
                "enum": ["result", "question"],
                "description": "result for something done or found; question when you need \
                                your owner's answer.",
            },
        },
        "required": ["title", "body", "kind"],
        "additionalProperties": false,
    })
}

/// A call's arguments, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub kind: NotificationKind,
    pub title: String,
    pub body: String,
}

/// Checks a call's arguments: a one-line title of 1 to
/// [`NOTIFICATION_TITLE_CHARS`] characters with no control characters, a
/// body of at most [`NOTIFICATION_BODY_CHARS`] with no control characters
/// but line breaks and tabs, neither with a layout control
/// ([`is_layout_control`]), and a kind. Both are trimmed.
pub fn parse(arguments: &Value) -> std::result::Result<Request, String> {
    let usage = || {
        "notify needs {\"title\": \"...\", \"body\": \"...\", \"kind\": \"result\" or \
         \"question\"}."
            .to_owned()
    };
    let title = arguments["title"].as_str().ok_or_else(usage)?.trim();
    let body = match &arguments["body"] {
        Value::Null => "",
        Value::String(body) => body.trim(),
        _ => return Err(usage()),
    };
    let kind = match arguments["kind"].as_str() {
        Some("result") => NotificationKind::Result,
        Some("question") => NotificationKind::Question,
        _ => return Err(usage()),
    };
    let length = title.chars().count();
    if length == 0 || length > NOTIFICATION_TITLE_CHARS {
        return Err(format!(
            "The title needs 1 to {NOTIFICATION_TITLE_CHARS} characters; this one has {length}."
        ));
    }
    if title.chars().any(char::is_control) {
        return Err("The title must be one line, with no control characters.".into());
    }
    if title.chars().chain(body.chars()).any(is_layout_control) {
        return Err(
            "The title and body must not contain line or paragraph separators or \
             bidirectional formatting characters."
                .into(),
        );
    }
    let length = body.chars().count();
    if length > NOTIFICATION_BODY_CHARS {
        return Err(format!(
            "The body needs at most {NOTIFICATION_BODY_CHARS} characters; this one has {length}."
        ));
    }
    if body
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err("The body must have no control characters but line breaks and tabs.".into());
    }
    Ok(Request {
        kind,
        title: title.to_owned(),
        body: body.to_owned(),
    })
}

/// The tool-call log's summary of a call: its kind and title.
pub fn summary(arguments: &Value) -> String {
    match (arguments["kind"].as_str(), arguments["title"].as_str()) {
        (Some(kind), Some(title)) => {
            let title: String = title.chars().filter(|c| !c.is_control()).collect();
            bounded(
                &format!("{}: {}", bounded(kind, 16), title.trim()),
                audit::SUMMARY_CHARS,
            )
        }
        _ => "invalid arguments".into(),
    }
}

fn internal(failure: Failure) -> String {
    tracing::error!(message = %failure.message, "the notify tool failed");
    "The notification could not be recorded just now.".into()
}

/// `notify`, called in turn `turn_id` of thread `thread_id` as call
/// `call_id`.
pub async fn serve(
    caller: &Caller,
    events: &EventLog,
    thread_id: &str,
    turn_id: &str,
    call_id: &str,
    arguments: &Value,
) -> Output {
    let request = parse(arguments)?;
    let (notification, marker) = record(caller, events, thread_id, turn_id, call_id, &request)
        .await
        .map_err(internal)??;
    tracing::info!(
        user = %caller.user,
        bot = %caller.bot,
        notification = %notification,
        message = %marker,
        kind = notification_kind_name(request.kind),
        "a Bot sent a notification"
    );
    Ok(ANSWER.into())
}

/// Records the notification and its marker together, and queues its push
/// when it is not a routine's run's, unless the turn or the hour already
/// has as many as it may.
async fn record(
    caller: &Caller,
    events: &EventLog,
    thread_id: &str,
    turn_id: &str,
    call_id: &str,
    request: &Request,
) -> Result<std::result::Result<(NotificationId, MessageId), String>> {
    let now = caller.clock.now();
    let thread_id = bounded(thread_id, ID_CHARS);
    let turn_id = bounded(turn_id, ID_CHARS);
    let call_id = bounded(call_id, ID_CHARS);
    let mut batch = events.begin(caller.user).await?;
    let (in_turn, in_hour): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM notifications
                 WHERE user_id = ?1 AND bot_id = ?2 AND thread_id = ?3 AND turn_id = ?4),
                (SELECT count(*) FROM notifications WHERE user_id = ?1 AND created_at > ?5)",
    )
    .bind(caller.user.0)
    .bind(caller.bot.0)
    .bind(&thread_id)
    .bind(&turn_id)
    .bind(now.0 - HOUR_MS)
    .fetch_one(&mut *batch)
    .await?;
    if in_turn >= PER_TURN {
        return Ok(Err(format!(
            "You have sent {PER_TURN} notifications in this turn, the most a turn may: \
             nothing was recorded. Put the rest in your reply."
        )));
    }
    if in_hour >= PER_HOUR {
        tracing::warn!(user = %caller.user, bot = %caller.bot, limit = PER_HOUR, "refused a notification: the user's hourly limit is reached");
        return Ok(Err(format!(
            "Your owner has had {PER_HOUR} notifications in the past hour, the most an hour \
             may: nothing was recorded. Put it in your reply instead."
        )));
    }
    let marker = store::insert_message(
        &mut *batch,
        now,
        caller.user,
        NewMessage {
            bot: caller.bot,
            author: Author::Bot,
            text: &request.title,
            state: MessageState::Complete,
            turn: Some((&turn_id, &call_id)),
            kind: MessageKind::Marker,
            routine: None,
            notification: Some((request.kind, &request.body)),
        },
    )
    .await?;
    let (id, run): (i64, Option<i64>) = sqlx::query_as(
        "INSERT INTO notifications (user_id, bot_id, message_id, thread_id, turn_id, call_id,
                                    routine_run_id, kind, title, body, created_at)
         SELECT user_id, id, ?1, ?2, ?3, ?4,
                (SELECT r.id FROM routine_runs r
                 WHERE r.user_id = ?10 AND r.bot_id = ?9 AND r.thread_id = ?2
                   AND r.turn_id = ?3
                 ORDER BY r.id DESC LIMIT 1),
                ?5, ?6, ?7, ?8
         FROM bots WHERE id = ?9 AND user_id = ?10
         RETURNING id, routine_run_id",
    )
    .bind(marker.id.0)
    .bind(&thread_id)
    .bind(&turn_id)
    .bind(&call_id)
    .bind(notification_kind_name(request.kind))
    .bind(&request.title)
    .bind(&request.body)
    .bind(now.0)
    .bind(caller.bot.0)
    .bind(caller.user.0)
    .fetch_one(&mut *batch)
    .await?;
    let marker_id = marker.id;
    batch
        .emit(Event::MessageCreated { message: marker })
        .await?;
    if run.is_none() {
        crate::push::notified(
            &mut batch,
            now,
            caller.user,
            caller.bot,
            NotificationId(id),
            request,
        )
        .await?;
    }
    batch.commit().await?;
    Ok(Ok((NotificationId(id), marker_id)))
}

/// Gives the routine run whose turn this is the notifications already
/// recorded for it without one: those the model sent after Codex started
/// the turn but before the run was recorded running in it. Returns how
/// many.
pub async fn attribute_to_run(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    bot: BotId,
    thread_id: &str,
    turn_id: &str,
    run: RoutineRunId,
) -> Result<u64> {
    let done = sqlx::query(
        "UPDATE notifications SET routine_run_id = ?
         WHERE user_id = ? AND bot_id = ? AND thread_id = ? AND turn_id = ?
           AND routine_run_id IS NULL",
    )
    .bind(run.0)
    .bind(user.0)
    .bind(bot.0)
    .bind(bounded(thread_id, ID_CHARS))
    .bind(bounded(turn_id, ID_CHARS))
    .execute(db)
    .await?;
    Ok(done.rows_affected())
}

const COLUMNS: &str = "id, bot_id, message_id, routine_run_id, kind, title, body, created_at";

fn notification_from(row: &sqlx::sqlite::SqliteRow) -> Notification {
    Notification {
        id: NotificationId(row.get("id")),
        bot_id: BotId(row.get("bot_id")),
        message_id: MessageId(row.get("message_id")),
        routine_run_id: row
            .get::<Option<i64>, _>("routine_run_id")
            .map(RoutineRunId),
        kind: notification_kind_from(row.get::<&str, _>("kind")),
        title: row.get("title"),
        body: row.get("body"),
        created_at: Timestamp(row.get("created_at")),
    }
}

/// A page of the user's notifications from every Bot, newest first, older
/// than `before`.
pub async fn page(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    before: Option<NotificationId>,
    limit: u32,
) -> Result<(Vec<Notification>, Option<NotificationId>)> {
    let limit = limit.clamp(1, 200);
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM notifications
         WHERE user_id = ? AND (? IS NULL OR id < ?)
         ORDER BY id DESC LIMIT ?"
    ))
    .bind(user.0)
    .bind(before.map(|n| n.0))
    .bind(before.map(|n| n.0))
    .bind(i64::from(limit) + 1)
    .fetch_all(db)
    .await?;
    let mut notifications: Vec<Notification> = rows.iter().map(notification_from).collect();
    let older = if notifications.len() > limit as usize {
        notifications.truncate(limit as usize);
        notifications.last().map(|n| n.id)
    } else {
        None
    };
    Ok((notifications, older))
}

/// Deletes notifications older than [`RETENTION_MS`]. Their markers stay.
/// Returns how many.
pub async fn prune(conn: &mut SqliteConnection, now: Timestamp) -> Result<u64> {
    let done = sqlx::query("DELETE FROM notifications WHERE created_at <= ?")
        .bind(now.0 - RETENTION_MS)
        .execute(conn)
        .await?;
    Ok(done.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_needs_a_one_line_title_a_bounded_body_and_a_kind() {
        let request = parse(&json!({
            "title": "  Dinner plan ready ", "body": "Pasta.\n\tThen fruit.\n", "kind": "result",
        }))
        .unwrap();
        assert_eq!(request.title, "Dinner plan ready");
        assert_eq!(request.body, "Pasta.\n\tThen fruit.");
        assert_eq!(request.kind, NotificationKind::Result);
        let question = parse(&json!({"title": "Which day?", "body": "", "kind": "question"}));
        assert_eq!(question.unwrap().kind, NotificationKind::Question);
        // A body left out is empty.
        assert_eq!(
            parse(&json!({"title": "Done", "kind": "result"}))
                .unwrap()
                .body,
            ""
        );
        let longest = "t".repeat(NOTIFICATION_TITLE_CHARS);
        assert!(parse(&json!({"title": longest, "body": "", "kind": "result"})).is_ok());
        let body = "é".repeat(NOTIFICATION_BODY_CHARS);
        assert!(parse(&json!({"title": "x", "body": body, "kind": "result"})).is_ok());
        // Joiners stay: emoji sequences and some scripts need them.
        assert!(parse(&json!({"title": "Family 👨\u{200d}👩\u{200d}👧", "body": "می\u{200c}خواهم", "kind": "result"})).is_ok());
        for refused in [
            json!({"title": "", "body": "", "kind": "result"}),
            json!({"title": "   ", "body": "", "kind": "result"}),
            json!({"title": "t".repeat(NOTIFICATION_TITLE_CHARS + 1), "body": "", "kind": "result"}),
            json!({"title": "two\nlines", "body": "", "kind": "result"}),
            json!({"title": "bell\u{7}", "body": "", "kind": "result"}),
            json!({"title": "x", "body": "b".repeat(NOTIFICATION_BODY_CHARS + 1), "kind": "result"}),
            json!({"title": "x", "body": "escape\u{1b}[31m", "kind": "result"}),
            json!({"title": "line\u{2028}separator", "body": "", "kind": "result"}),
            json!({"title": "x", "body": "para\u{2029}graph", "kind": "result"}),
            json!({"title": "evil\u{202e}txt.exe", "body": "", "kind": "result"}),
            json!({"title": "x", "body": "isolate\u{2067}", "kind": "result"}),
            json!({"title": "mark\u{200f}", "body": "", "kind": "result"}),
            json!({"title": "x", "body": "arabic mark\u{061c}", "kind": "result"}),
            json!({"title": "\u{feff}bom", "body": "", "kind": "result"}),
            json!({"title": "x", "body": "annotation\u{fff9}", "kind": "result"}),
            json!({"title": "x", "body": "", "kind": "alert"}),
            json!({"title": "x", "body": ""}),
            json!({"title": 3, "body": "", "kind": "result"}),
            json!({"title": "x", "body": 3, "kind": "result"}),
            json!("notify"),
        ] {
            assert!(parse(&refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn the_log_summary_is_the_kind_and_title_bounded() {
        assert_eq!(
            summary(&json!({"title": "Dinner plan ready", "body": "secret", "kind": "result"})),
            "result: Dinner plan ready"
        );
        let long = summary(&json!({"title": "t".repeat(1000), "kind": "question"}));
        assert_eq!(long.chars().count(), audit::SUMMARY_CHARS);
        assert_eq!(
            summary(&json!({"title": "a\nb", "kind": "result"})),
            "result: ab"
        );
        assert_eq!(summary(&json!({})), "invalid arguments");
    }
}
