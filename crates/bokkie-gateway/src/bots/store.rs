//! Bots and messages in the database.
//!
//! Every function takes the owning user's id and puts it in the query's
//! `WHERE` clause, so a Bot or message id from another user reads as absent.
//! Each is one statement, so it runs on the pool or inside an event
//! [`Batch`](crate::events::Batch) alike.

use bokkie_protocol::api::{
    Author, Bot, BotStatus, Message, MessageKind, MessageNotification, MessagePreview,
    MessageRoutine, MessageState, NotificationKind, ReadState,
};
use bokkie_protocol::{BotId, MessageId, RoutineId, RoutineRunId, Timestamp, UserId};
use sqlx::Row;
use sqlx::sqlite::SqliteExecutor;

use crate::error::{Failure, Result};
use crate::grants::{self, GrantSet};

/// The longest message accepted from a user, in characters.
pub const MAX_MESSAGE_CHARS: usize = 20_000;
/// How many characters of the latest message the conversation list shows.
pub const PREVIEW_CHARS: usize = 140;

/// A Bot row with what only the gateway needs.
#[derive(Debug, Clone)]
pub struct BotRow {
    pub id: BotId,
    pub user: UserId,
    pub name: String,
    pub label: String,
    pub description: String,
    pub avatar: Option<String>,
    pub pinned: bool,
    pub hidden: bool,
    pub thread_id: Option<String>,
    pub profile_revision: i64,
    pub profile_sent_revision: i64,
    pub refused_reason: Option<String>,
    /// The admin who made the Bot for its owner, if an admin did.
    pub made_by: Option<UserId>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// The Bot's grants.
    pub grants: GrantSet,
    /// The grants its current thread started with.
    pub thread_grants: GrantSet,
    /// The marker that begins a fresh thread which has had no turn yet.
    pub carryover_marker: Option<MessageId>,
}

impl BotRow {
    /// Whether `thread_id` is the Bot's current thread, rather than one a
    /// fresh thread replaced.
    pub fn is_current(&self, thread_id: &str) -> bool {
        self.thread_id.as_deref() == Some(thread_id)
    }

    /// The Bot as its owner sees it.
    pub fn to_api(&self, working: bool, activity: Activity) -> Bot {
        Bot {
            id: self.id,
            name: self.name.clone(),
            label: self.label.clone(),
            description: self.description.clone(),
            avatar: self.avatar.clone(),
            pinned: self.pinned,
            hidden: self.hidden,
            status: if self.refused_reason.is_some() {
                BotStatus::Refused
            } else if working {
                BotStatus::Working
            } else {
                BotStatus::Idle
            },
            created_at: self.created_at,
            updated_at: self.updated_at,
            last_message: activity.last_message,
            grants: self.grants.iter().copied().collect(),
            read_up_to: activity.read_up_to,
            unread: activity.unread,
        }
    }
}

const BOT_COLUMNS: &str = "id, user_id, name, label, description, avatar, pinned, hidden, \
     thread_id, profile_revision, profile_sent_revision, refused_reason, made_by, created_at, \
     updated_at, grants, thread_grants, carryover_marker";

fn bot_from(row: &sqlx::sqlite::SqliteRow) -> BotRow {
    BotRow {
        id: BotId(row.get("id")),
        user: UserId(row.get("user_id")),
        name: row.get("name"),
        label: row.get("label"),
        description: row.get("description"),
        avatar: row.get("avatar"),
        pinned: row.get::<i64, _>("pinned") != 0,
        hidden: row.get::<i64, _>("hidden") != 0,
        thread_id: row.get("thread_id"),
        profile_revision: row.get("profile_revision"),
        profile_sent_revision: row.get("profile_sent_revision"),
        refused_reason: row.get("refused_reason"),
        made_by: row.get::<Option<i64>, _>("made_by").map(UserId),
        created_at: Timestamp(row.get("created_at")),
        updated_at: Timestamp(row.get("updated_at")),
        grants: grants::from_stored(row.get("grants")),
        thread_grants: grants::from_stored(row.get("thread_grants")),
        carryover_marker: row.get::<Option<i64>, _>("carryover_marker").map(MessageId),
    }
}

fn author_name(author: Author) -> &'static str {
    match author {
        Author::User => "user",
        Author::Bot => "bot",
    }
}

fn author_from(name: &str) -> Author {
    if name == "user" {
        Author::User
    } else {
        Author::Bot
    }
}

pub fn state_name(state: MessageState) -> &'static str {
    match state {
        MessageState::Streaming => "streaming",
        MessageState::Complete => "complete",
        MessageState::Interrupted => "interrupted",
        MessageState::Failed => "failed",
    }
}

fn state_from(name: &str) -> MessageState {
    match name {
        "streaming" => MessageState::Streaming,
        "complete" => MessageState::Complete,
        "interrupted" => MessageState::Interrupted,
        _ => MessageState::Failed,
    }
}

fn kind_name(kind: MessageKind) -> &'static str {
    match kind {
        MessageKind::Text => "text",
        MessageKind::Marker => "marker",
    }
}

fn kind_from(name: &str) -> MessageKind {
    match name {
        "marker" => MessageKind::Marker,
        _ => MessageKind::Text,
    }
}

const MESSAGE_COLUMNS: &str = "id, bot_id, author, text, state, created_at, updated_at, kind, \
     routine_id, routine_run_id, detail, notification_kind";

pub fn notification_kind_name(kind: NotificationKind) -> &'static str {
    match kind {
        NotificationKind::Result => "result",
        NotificationKind::Question => "question",
    }
}

pub fn notification_kind_from(name: &str) -> NotificationKind {
    if name == "question" {
        NotificationKind::Question
    } else {
        NotificationKind::Result
    }
}

fn message_from(row: &sqlx::sqlite::SqliteRow) -> Message {
    Message {
        id: MessageId(row.get("id")),
        bot_id: BotId(row.get("bot_id")),
        author: author_from(row.get::<&str, _>("author")),
        text: row.get("text"),
        state: state_from(row.get::<&str, _>("state")),
        created_at: Timestamp(row.get("created_at")),
        updated_at: Timestamp(row.get("updated_at")),
        kind: kind_from(row.get::<&str, _>("kind")),
        routine: match (
            row.get::<Option<i64>, _>("routine_id"),
            row.get::<Option<i64>, _>("routine_run_id"),
            row.get::<Option<String>, _>("detail"),
        ) {
            (Some(routine), Some(run), Some(prompt)) => Some(MessageRoutine {
                routine_id: RoutineId(routine),
                run_id: RoutineRunId(run),
                prompt,
            }),
            _ => None,
        },
        notification: row.get::<Option<&str>, _>("notification_kind").map(|kind| {
            MessageNotification {
                kind: notification_kind_from(kind),
                body: row.get::<Option<String>, _>("detail").unwrap_or_default(),
            }
        }),
    }
}

// ---------------------------------------------------------------------------
// Validation

fn check_text(field: &str, value: &str, min: usize, max: usize) -> Result<String> {
    let value = value.trim().to_owned();
    let length = value.chars().count();
    if length < min || length > max {
        return Err(Failure::invalid(format!(
            "{field} needs {min} to {max} characters"
        )));
    }
    if value
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(Failure::invalid(format!("{field} has control characters")));
    }
    Ok(value)
}

pub fn check_name(name: &str) -> Result<String> {
    let name = check_text("A name", name, 1, 40)?;
    if name.contains('\n') {
        return Err(Failure::invalid("A name is one line"));
    }
    Ok(name)
}

pub fn check_label(label: &str) -> Result<String> {
    let label = check_text("A label", label, 0, 60)?;
    if label.contains('\n') {
        return Err(Failure::invalid("A label is one line"));
    }
    Ok(label)
}

pub fn check_description(description: &str) -> Result<String> {
    check_text("A description", description, 0, 4_000)
}

pub fn check_avatar(avatar: &str) -> Result<String> {
    let avatar = check_text("An avatar", avatar, 1, 8)?;
    if avatar.contains('\n') {
        return Err(Failure::invalid("An avatar is one line"));
    }
    Ok(avatar)
}

pub fn check_message(text: &str) -> Result<String> {
    check_text("A message", text, 1, MAX_MESSAGE_CHARS)
}

// ---------------------------------------------------------------------------
// Bots

async fn unique_name<T>(result: std::result::Result<T, sqlx::Error>) -> Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            Err(Failure::conflict("You already have a Bot with that name"))
        }
        Err(e) => Err(e.into()),
    }
}

/// Creates one of `user`'s Bots; `made_by` names the admin who made it for
/// them, if an admin did.
#[allow(clippy::too_many_arguments)]
pub async fn create_bot(
    db: impl SqliteExecutor<'_>,
    now: Timestamp,
    user: UserId,
    made_by: Option<UserId>,
    name: &str,
    label: &str,
    description: &str,
    avatar: Option<&str>,
) -> Result<BotRow> {
    let name = check_name(name)?;
    let label = check_label(label)?;
    let description = check_description(description)?;
    let avatar = avatar.map(check_avatar).transpose()?;
    let row = unique_name(
        sqlx::query(&format!(
            "INSERT INTO bots (user_id, made_by, name, label, description, avatar, created_at,
                               updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) RETURNING {BOT_COLUMNS}"
        ))
        .bind(user.0)
        .bind(made_by.map(|admin| admin.0))
        .bind(&name)
        .bind(&label)
        .bind(&description)
        .bind(&avatar)
        .bind(now.0)
        .bind(now.0)
        .fetch_one(db)
        .await,
    )
    .await?;
    Ok(bot_from(&row))
}

pub async fn bot(db: impl SqliteExecutor<'_>, user: UserId, bot: BotId) -> Result<Option<BotRow>> {
    let row = sqlx::query(&format!(
        "SELECT {BOT_COLUMNS} FROM bots WHERE id = ? AND user_id = ?"
    ))
    .bind(bot.0)
    .bind(user.0)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(bot_from))
}

/// The user's Bot with this name, whatever its case: names are unique
/// per user that way.
pub async fn bot_by_name(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    name: &str,
) -> Result<Option<BotRow>> {
    let row = sqlx::query(&format!(
        "SELECT {BOT_COLUMNS} FROM bots WHERE user_id = ? AND name = ? COLLATE NOCASE"
    ))
    .bind(user.0)
    .bind(name)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(bot_from))
}

/// The user's Bot that a Codex thread belongs to, if any: the thread is
/// the Bot's current one or one a fresh thread replaced
/// ([`BotRow::is_current`] tells them apart). A replaced thread can still
/// report the end of its last turn.
pub async fn bot_by_thread(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    thread_id: &str,
) -> Result<Option<BotRow>> {
    let row = sqlx::query(&format!(
        "SELECT {BOT_COLUMNS} FROM bots
         WHERE user_id = ?1
           AND id = (SELECT bot_id FROM bot_threads WHERE user_id = ?1 AND thread_id = ?2)"
    ))
    .bind(user.0)
    .bind(thread_id)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(bot_from))
}

/// The user's Bots, pinned first, then by most recent activity.
pub async fn list_bots(db: impl SqliteExecutor<'_>, user: UserId) -> Result<Vec<BotRow>> {
    let rows = sqlx::query(&format!(
        "SELECT {BOT_COLUMNS} FROM bots b WHERE user_id = ?
         ORDER BY pinned DESC,
           coalesce((SELECT max(m.updated_at) FROM messages m WHERE m.bot_id = b.id), updated_at) DESC,
           id"
    ))
    .bind(user.0)
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(bot_from).collect())
}

/// The fields of a Bot an update may change. `None` leaves a field alone.
#[derive(Debug, Default)]
pub struct BotChanges {
    pub name: Option<String>,
    pub label: Option<String>,
    pub description: Option<String>,
    pub avatar: Option<Option<String>>,
    pub pinned: Option<bool>,
    pub hidden: Option<bool>,
}

pub async fn update_bot(
    db: impl SqliteExecutor<'_>,
    now: Timestamp,
    user: UserId,
    id: BotId,
    changes: BotChanges,
) -> Result<BotRow> {
    let name = changes.name.as_deref().map(check_name).transpose()?;
    let label = changes.label.as_deref().map(check_label).transpose()?;
    let description = changes
        .description
        .as_deref()
        .map(check_description)
        .transpose()?;
    let avatar = match changes.avatar {
        Some(Some(avatar)) => Some(Some(check_avatar(&avatar)?)),
        other => other,
    };
    // One statement, so concurrent edits cannot lose each other's fields.
    // SQLite evaluates every expression against the row as it was, so the
    // revision compares the old profile with the new one.
    let row = unique_name(
        sqlx::query(&format!(
            "UPDATE bots SET
                 profile_revision = profile_revision + (CASE
                     WHEN (?1 IS NOT NULL AND ?1 != name)
                       OR (?2 IS NOT NULL AND ?2 != label)
                       OR (?3 IS NOT NULL AND ?3 != description) THEN 1 ELSE 0 END),
                 name = coalesce(?1, name),
                 label = coalesce(?2, label),
                 description = coalesce(?3, description),
                 avatar = CASE WHEN ?4 THEN ?5 ELSE avatar END,
                 pinned = coalesce(?6, pinned),
                 hidden = coalesce(?7, hidden),
                 updated_at = ?8
             WHERE id = ?9 AND user_id = ?10 RETURNING {BOT_COLUMNS}"
        ))
        .bind(name)
        .bind(label)
        .bind(description)
        .bind(avatar.is_some())
        .bind(avatar.flatten())
        .bind(changes.pinned.map(i64::from))
        .bind(changes.hidden.map(i64::from))
        .bind(now.0)
        .bind(id.0)
        .bind(user.0)
        .fetch_optional(db)
        .await,
    )
    .await?;
    row.as_ref().map(bot_from).ok_or_else(Failure::not_found)
}

/// Records the Bot's first thread, with the profile revision and grants it
/// started with. The thread joins the Bot's threads in the same statement
/// (a trigger), as with [`replace_thread`].
pub async fn set_thread(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    bot: BotId,
    thread_id: &str,
    revision: i64,
    grants: &GrantSet,
) -> Result<()> {
    sqlx::query(
        "UPDATE bots SET thread_id = ?, profile_sent_revision = ?, thread_grants = ?
         WHERE id = ? AND user_id = ? AND thread_id IS NULL",
    )
    .bind(thread_id)
    .bind(revision)
    .bind(grants::to_stored(grants))
    .bind(bot.0)
    .bind(user.0)
    .execute(db)
    .await?;
    Ok(())
}

/// Replaces the Bot's thread `old` with a fresh one, started with the
/// profile revision and grants given, and begun by `marker`. Returns
/// whether `old` was still the Bot's thread.
#[allow(clippy::too_many_arguments)]
pub async fn replace_thread(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    bot: BotId,
    old: &str,
    new: &str,
    revision: i64,
    grants: &GrantSet,
    marker: MessageId,
) -> Result<bool> {
    let done = sqlx::query(
        "UPDATE bots SET thread_id = ?, profile_sent_revision = ?, thread_grants = ?,
             carryover_marker = ?
         WHERE id = ? AND user_id = ? AND thread_id = ?",
    )
    .bind(new)
    .bind(revision)
    .bind(grants::to_stored(grants))
    .bind(marker.0)
    .bind(bot.0)
    .bind(user.0)
    .bind(old)
    .execute(db)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Clears the carry-over once a turn on the fresh thread has started.
pub async fn clear_carryover(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    bot: BotId,
    marker: MessageId,
) -> Result<()> {
    sqlx::query(
        "UPDATE bots SET carryover_marker = NULL
         WHERE id = ? AND user_id = ? AND carryover_marker = ?",
    )
    .bind(bot.0)
    .bind(user.0)
    .bind(marker.0)
    .execute(db)
    .await?;
    Ok(())
}

/// Sets the Bot's grants. Its thread keeps the grants it started with
/// until the next turn replaces it.
pub async fn set_grants(
    db: impl SqliteExecutor<'_>,
    now: Timestamp,
    user: UserId,
    bot: BotId,
    grants: &GrantSet,
) -> Result<BotRow> {
    let row = sqlx::query(&format!(
        "UPDATE bots SET grants = ?, updated_at = ? WHERE id = ? AND user_id = ?
         RETURNING {BOT_COLUMNS}"
    ))
    .bind(grants::to_stored(grants))
    .bind(now.0)
    .bind(bot.0)
    .bind(user.0)
    .fetch_optional(db)
    .await?;
    row.as_ref().map(bot_from).ok_or_else(Failure::not_found)
}

pub async fn set_profile_sent(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    bot: BotId,
    revision: i64,
) -> Result<()> {
    sqlx::query(
        "UPDATE bots SET profile_sent_revision = max(profile_sent_revision, ?)
         WHERE id = ? AND user_id = ?",
    )
    .bind(revision)
    .bind(bot.0)
    .bind(user.0)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn set_refused(
    db: impl SqliteExecutor<'_>,
    now: Timestamp,
    user: UserId,
    bot: BotId,
    reason: &str,
) -> Result<()> {
    sqlx::query("UPDATE bots SET refused_reason = ?, updated_at = ? WHERE id = ? AND user_id = ?")
        .bind(reason)
        .bind(now.0)
        .bind(bot.0)
        .bind(user.0)
        .execute(db)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Messages

/// A message to add to a Bot's conversation.
#[derive(Debug, Clone, Copy)]
pub struct NewMessage<'a> {
    pub bot: BotId,
    pub author: Author,
    pub text: &'a str,
    pub state: MessageState,
    /// For Bot replies: the Codex turn and item.
    pub turn: Option<(&'a str, &'a str)>,
    pub kind: MessageKind,
    /// For a routine run's marker: the routine, the run and its prompt.
    pub routine: Option<(RoutineId, RoutineRunId, &'a str)>,
    /// For a notification's marker: its kind and body. The text is the
    /// title.
    pub notification: Option<(NotificationKind, &'a str)>,
}

pub async fn insert_message(
    db: impl SqliteExecutor<'_>,
    now: Timestamp,
    user: UserId,
    new: NewMessage<'_>,
) -> Result<Message> {
    let NewMessage {
        bot,
        author,
        text,
        state,
        turn,
        kind,
        routine,
        notification,
    } = new;
    let row = sqlx::query(&format!(
        "INSERT INTO messages (bot_id, user_id, author, text, state, turn_id, item_id,
                               created_at, updated_at, kind, routine_id, routine_run_id, detail,
                               notification_kind)
         SELECT id, user_id, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ? FROM bots
         WHERE id = ? AND user_id = ?
         RETURNING {MESSAGE_COLUMNS}"
    ))
    .bind(author_name(author))
    .bind(text)
    .bind(state_name(state))
    .bind(turn.map(|t| t.0))
    .bind(turn.map(|t| t.1))
    .bind(now.0)
    .bind(now.0)
    .bind(kind_name(kind))
    .bind(routine.map(|r| r.0.0))
    .bind(routine.map(|r| r.1.0))
    .bind(routine.map(|r| r.2).or(notification.map(|n| n.1)))
    .bind(notification.map(|n| notification_kind_name(n.0)))
    .bind(bot.0)
    .bind(user.0)
    .fetch_optional(db)
    .await?;
    row.as_ref()
        .map(message_from)
        .ok_or_else(Failure::not_found)
}

/// Up to `limit` of the Bot's latest text messages before `before`,
/// oldest first, leaving out `except` if given. Markers are left out.
pub async fn recent_text(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    bot: BotId,
    before: MessageId,
    except: Option<MessageId>,
    limit: u32,
) -> Result<Vec<Message>> {
    let rows = sqlx::query(&format!(
        "SELECT {MESSAGE_COLUMNS} FROM messages
         WHERE bot_id = ? AND user_id = ? AND id < ? AND kind = 'text'
           AND (? IS NULL OR id != ?)
         ORDER BY id DESC LIMIT ?"
    ))
    .bind(bot.0)
    .bind(user.0)
    .bind(before.0)
    .bind(except.map(|m| m.0))
    .bind(except.map(|m| m.0))
    .bind(i64::from(limit))
    .fetch_all(db)
    .await?;
    let mut messages: Vec<Message> = rows.iter().map(message_from).collect();
    messages.reverse();
    Ok(messages)
}

pub async fn message(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    id: MessageId,
) -> Result<Option<Message>> {
    let row = sqlx::query(&format!(
        "SELECT {MESSAGE_COLUMNS} FROM messages WHERE id = ? AND user_id = ?"
    ))
    .bind(id.0)
    .bind(user.0)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(message_from))
}

/// Appends streamed text to a Bot reply that is still streaming.
pub async fn append_text(
    db: impl SqliteExecutor<'_>,
    now: Timestamp,
    user: UserId,
    id: MessageId,
    text: &str,
) -> Result<()> {
    sqlx::query(
        "UPDATE messages SET text = text || ?, updated_at = ?
         WHERE id = ? AND user_id = ? AND state = 'streaming'",
    )
    .bind(text)
    .bind(now.0)
    .bind(id.0)
    .bind(user.0)
    .execute(db)
    .await?;
    Ok(())
}

/// Sets a streaming message's final text (when given) and state. Returns
/// the message if it was still streaming.
pub async fn finish_message(
    db: impl SqliteExecutor<'_>,
    now: Timestamp,
    user: UserId,
    id: MessageId,
    text: Option<&str>,
    state: MessageState,
) -> Result<Option<Message>> {
    let row = sqlx::query(&format!(
        "UPDATE messages SET text = coalesce(?, text), state = ?, updated_at = ?
         WHERE id = ? AND user_id = ? AND state = 'streaming' RETURNING {MESSAGE_COLUMNS}"
    ))
    .bind(text)
    .bind(state_name(state))
    .bind(now.0)
    .bind(id.0)
    .bind(user.0)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(message_from))
}

/// The user's replies still streaming: of every Bot, of one Bot, or of
/// one Bot's turn.
pub async fn streaming_messages(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    bot: Option<BotId>,
    turn: Option<&str>,
) -> Result<Vec<MessageId>> {
    let rows = sqlx::query(
        "SELECT id FROM messages WHERE user_id = ? AND state = 'streaming'
           AND (? IS NULL OR bot_id = ?) AND (? IS NULL OR turn_id = ?)",
    )
    .bind(user.0)
    .bind(bot.map(|b| b.0))
    .bind(bot.map(|b| b.0))
    .bind(turn)
    .bind(turn)
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(|r| MessageId(r.get("id"))).collect())
}

/// A page of a Bot's conversation, oldest first, ending before `before`.
pub async fn messages(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    bot: BotId,
    before: Option<MessageId>,
    limit: u32,
) -> Result<(Vec<Message>, Option<MessageId>)> {
    let limit = limit.clamp(1, 200);
    let rows = sqlx::query(&format!(
        "SELECT {MESSAGE_COLUMNS} FROM messages
         WHERE bot_id = ? AND user_id = ? AND (? IS NULL OR id < ?)
         ORDER BY id DESC LIMIT ?"
    ))
    .bind(bot.0)
    .bind(user.0)
    .bind(before.map(|m| m.0))
    .bind(before.map(|m| m.0))
    .bind(i64::from(limit) + 1)
    .fetch_all(db)
    .await?;
    let mut messages: Vec<Message> = rows.iter().map(message_from).collect();
    let older = if messages.len() > limit as usize {
        messages.truncate(limit as usize);
        messages.last().map(|m| m.id)
    } else {
        None
    };
    messages.reverse();
    Ok((messages, older))
}

/// What a Bot's conversation list entry shows of its conversation: the
/// start of its latest message, and whether its user has seen it all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activity {
    pub last_message: Option<MessagePreview>,
    pub read_up_to: Option<MessageId>,
    pub unread: bool,
}

/// The conversation activity of each of the user's Bots, or of `only`.
pub async fn activity(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    only: Option<BotId>,
) -> Result<std::collections::HashMap<BotId, Activity>> {
    let rows = sqlx::query(&format!(
        "SELECT b.id AS bot_id, {UNREAD_COLUMNS},
                    m.author, substr(m.text, 1, ?) AS text, m.updated_at
             FROM bots b
             {READ_MARKER_JOIN}
             LEFT JOIN messages m
               ON m.id = (SELECT max(id) FROM messages WHERE bot_id = b.id)
              AND m.user_id = b.user_id
             WHERE b.user_id = ? AND (? IS NULL OR b.id = ?)"
    ))
    .bind(PREVIEW_CHARS as i64)
    .bind(user.0)
    .bind(only.map(|b| b.0))
    .bind(only.map(|b| b.0))
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let read_up_to = r.get::<Option<i64>, _>("read_up_to").map(MessageId);
            let newest = r.get::<Option<i64>, _>("newest").map(MessageId);
            let last_message = r
                .get::<Option<&str>, _>("author")
                .map(|author| MessagePreview {
                    author: author_from(author),
                    text: r.get("text"),
                    at: Timestamp(r.get("updated_at")),
                });
            (
                BotId(r.get("bot_id")),
                Activity {
                    last_message,
                    read_up_to,
                    unread: is_unread(newest, read_up_to),
                },
            )
        })
        .collect())
}

/// The columns [`is_unread`] reads, for a query over Bots `b` joined to
/// their read markers `r` by [`READ_MARKER_JOIN`]: the user's read marker
/// and the Bot's newest message of its own. Every unread state and count
/// comes from these and [`is_unread`].
const UNREAD_COLUMNS: &str = "r.message_id AS read_up_to,
    (SELECT max(id) FROM messages WHERE bot_id = b.id AND author = 'bot') AS newest";

/// Joins Bots `b` to their users' read markers `r`.
const READ_MARKER_JOIN: &str =
    "LEFT JOIN read_markers r ON r.bot_id = b.id AND r.user_id = b.user_id";

/// How many of a list of Bots' unread flags are set: the count the app's
/// badge shows.
pub fn unread_count(unread: impl IntoIterator<Item = bool>) -> u32 {
    let count = unread.into_iter().filter(|unread| *unread).count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// Whether a Bot whose newest message of its own is `newest` is unread to
/// a user who has seen its conversation up to `read_up_to`, by the rule of
/// [`Message::is_unread`].
fn is_unread(newest: Option<MessageId>, read_up_to: Option<MessageId>) -> bool {
    newest.is_some_and(|newest| read_up_to.is_none_or(|read| newest > read))
}

// ---------------------------------------------------------------------------
// Read markers

/// The user's read marker on one of their Bots, or `None` if the Bot is not
/// theirs.
pub async fn read_state(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    bot: BotId,
) -> Result<Option<ReadState>> {
    let row = sqlx::query(&format!(
        "SELECT {UNREAD_COLUMNS} FROM bots b {READ_MARKER_JOIN} WHERE b.id = ? AND b.user_id = ?"
    ))
    .bind(bot.0)
    .bind(user.0)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| {
        let read_up_to = r.get::<Option<i64>, _>("read_up_to").map(MessageId);
        let newest = r.get::<Option<i64>, _>("newest").map(MessageId);
        ReadState {
            bot_id: bot,
            read_up_to,
            unread: is_unread(newest, read_up_to),
        }
    }))
}

/// Moves the user's read marker on their Bot forward to `message`, one of
/// that Bot's messages. Returns whether it moved: an older message, or one
/// that is not the user's Bot's, leaves it where it is.
pub async fn mark_read(
    db: impl SqliteExecutor<'_>,
    now: Timestamp,
    user: UserId,
    bot: BotId,
    message: MessageId,
) -> Result<bool> {
    let moved = sqlx::query(
        "INSERT INTO read_markers (bot_id, user_id, message_id, updated_at)
         SELECT m.bot_id, m.user_id, m.id, ?
         FROM messages m JOIN bots b ON b.id = m.bot_id AND b.user_id = m.user_id
         WHERE m.id = ? AND m.bot_id = ? AND m.user_id = ?
         ON CONFLICT (bot_id) DO UPDATE
           SET message_id = excluded.message_id, updated_at = excluded.updated_at
           WHERE excluded.message_id > read_markers.message_id
             AND read_markers.user_id = excluded.user_id
         RETURNING message_id",
    )
    .bind(now.0)
    .bind(message.0)
    .bind(bot.0)
    .bind(user.0)
    .fetch_optional(db)
    .await?;
    Ok(moved.is_some())
}

/// How many of the user's Bots are unread, hidden ones too: the count the
/// app's badge shows, as the Bot list counts it.
pub async fn unread_bots(db: impl SqliteExecutor<'_>, user: UserId) -> Result<u32> {
    let rows = sqlx::query(&format!(
        "SELECT {UNREAD_COLUMNS} FROM bots b {READ_MARKER_JOIN} WHERE b.user_id = ?"
    ))
    .bind(user.0)
    .fetch_all(db)
    .await?;
    Ok(unread_count(rows.iter().map(|r| {
        is_unread(
            r.get::<Option<i64>, _>("newest").map(MessageId),
            r.get::<Option<i64>, _>("read_up_to").map(MessageId),
        )
    })))
}
