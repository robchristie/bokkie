//! Bots and messages in the database.
//!
//! Every function takes the owning user's id and puts it in the query's
//! `WHERE` clause, so a Bot or message id from another user reads as absent.

use bokkie_protocol::api::{Author, Bot, BotStatus, Message, MessagePreview, MessageState};
use bokkie_protocol::{BotId, MessageId, Timestamp, UserId};
use sqlx::{Row, SqlitePool};

use crate::error::{Failure, Result};

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
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl BotRow {
    /// The Bot as its owner sees it.
    pub fn to_api(&self, working: bool, last_message: Option<MessagePreview>) -> Bot {
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
            last_message,
        }
    }
}

const BOT_COLUMNS: &str = "id, user_id, name, label, description, avatar, pinned, hidden, \
     thread_id, profile_revision, profile_sent_revision, refused_reason, created_at, updated_at";

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
        created_at: Timestamp(row.get("created_at")),
        updated_at: Timestamp(row.get("updated_at")),
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

const MESSAGE_COLUMNS: &str = "id, bot_id, author, text, state, created_at, updated_at";

fn message_from(row: &sqlx::sqlite::SqliteRow) -> Message {
    Message {
        id: MessageId(row.get("id")),
        bot_id: BotId(row.get("bot_id")),
        author: author_from(row.get::<&str, _>("author")),
        text: row.get("text"),
        state: state_from(row.get::<&str, _>("state")),
        created_at: Timestamp(row.get("created_at")),
        updated_at: Timestamp(row.get("updated_at")),
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

pub async fn create_bot(
    db: &SqlitePool,
    now: Timestamp,
    user: UserId,
    name: &str,
    label: &str,
    description: &str,
    avatar: Option<&str>,
) -> Result<BotRow> {
    let name = check_name(name)?;
    let label = check_label(label)?;
    let description = check_description(description)?;
    let avatar = avatar.map(check_avatar).transpose()?;
    let done = unique_name(
        sqlx::query(
            "INSERT INTO bots (user_id, name, label, description, avatar, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(user.0)
        .bind(&name)
        .bind(&label)
        .bind(&description)
        .bind(&avatar)
        .bind(now.0)
        .bind(now.0)
        .execute(db)
        .await,
    )
    .await?;
    bot(db, user, BotId(done.last_insert_rowid()))
        .await?
        .ok_or_else(Failure::not_found)
}

pub async fn bot(db: &SqlitePool, user: UserId, bot: BotId) -> Result<Option<BotRow>> {
    let row = sqlx::query(&format!(
        "SELECT {BOT_COLUMNS} FROM bots WHERE id = ? AND user_id = ?"
    ))
    .bind(bot.0)
    .bind(user.0)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(bot_from))
}

/// The user's Bot on a Codex thread, if any.
pub async fn bot_by_thread(
    db: &SqlitePool,
    user: UserId,
    thread_id: &str,
) -> Result<Option<BotRow>> {
    let row = sqlx::query(&format!(
        "SELECT {BOT_COLUMNS} FROM bots WHERE thread_id = ? AND user_id = ?"
    ))
    .bind(thread_id)
    .bind(user.0)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(bot_from))
}

/// The user's Bots, pinned first, then by most recent activity.
pub async fn list_bots(db: &SqlitePool, user: UserId) -> Result<Vec<BotRow>> {
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
    db: &SqlitePool,
    now: Timestamp,
    user: UserId,
    id: BotId,
    changes: BotChanges,
) -> Result<BotRow> {
    let current = bot(db, user, id).await?.ok_or_else(Failure::not_found)?;
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
    let profile_changed = name.as_ref().is_some_and(|n| *n != current.name)
        || label.as_ref().is_some_and(|l| *l != current.label)
        || description
            .as_ref()
            .is_some_and(|d| *d != current.description);
    unique_name(
        sqlx::query(
            "UPDATE bots SET name = ?, label = ?, description = ?, avatar = ?, pinned = ?,
                 hidden = ?, profile_revision = profile_revision + ?, updated_at = ?
             WHERE id = ? AND user_id = ?",
        )
        .bind(name.unwrap_or(current.name))
        .bind(label.unwrap_or(current.label))
        .bind(description.unwrap_or(current.description))
        .bind(avatar.unwrap_or(current.avatar))
        .bind(changes.pinned.unwrap_or(current.pinned) as i64)
        .bind(changes.hidden.unwrap_or(current.hidden) as i64)
        .bind(profile_changed as i64)
        .bind(now.0)
        .bind(id.0)
        .bind(user.0)
        .execute(db)
        .await,
    )
    .await?;
    bot(db, user, id).await?.ok_or_else(Failure::not_found)
}

pub async fn set_thread(
    db: &SqlitePool,
    user: UserId,
    bot: BotId,
    thread_id: &str,
    revision: i64,
) -> Result<()> {
    sqlx::query(
        "UPDATE bots SET thread_id = ?, profile_sent_revision = ?
         WHERE id = ? AND user_id = ? AND thread_id IS NULL",
    )
    .bind(thread_id)
    .bind(revision)
    .bind(bot.0)
    .bind(user.0)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn set_profile_sent(
    db: &SqlitePool,
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
    db: &SqlitePool,
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
}

pub async fn insert_message(
    db: &SqlitePool,
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
    } = new;
    let done = sqlx::query(
        "INSERT INTO messages (bot_id, user_id, author, text, state, turn_id, item_id,
                               created_at, updated_at)
         SELECT id, user_id, ?, ?, ?, ?, ?, ?, ? FROM bots WHERE id = ? AND user_id = ?",
    )
    .bind(author_name(author))
    .bind(text)
    .bind(state_name(state))
    .bind(turn.map(|t| t.0))
    .bind(turn.map(|t| t.1))
    .bind(now.0)
    .bind(now.0)
    .bind(bot.0)
    .bind(user.0)
    .execute(db)
    .await?;
    if done.rows_affected() != 1 {
        return Err(Failure::not_found());
    }
    message(db, user, MessageId(done.last_insert_rowid()))
        .await?
        .ok_or_else(Failure::not_found)
}

pub async fn message(db: &SqlitePool, user: UserId, id: MessageId) -> Result<Option<Message>> {
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
    db: &SqlitePool,
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

/// Sets a message's final text (when given) and state.
pub async fn finish_message(
    db: &SqlitePool,
    now: Timestamp,
    user: UserId,
    id: MessageId,
    text: Option<&str>,
    state: MessageState,
) -> Result<Option<Message>> {
    sqlx::query(
        "UPDATE messages SET text = coalesce(?, text), state = ?, updated_at = ?
         WHERE id = ? AND user_id = ? AND state = 'streaming'",
    )
    .bind(text)
    .bind(state_name(state))
    .bind(now.0)
    .bind(id.0)
    .bind(user.0)
    .execute(db)
    .await?;
    message(db, user, id).await
}

/// The user's replies still streaming, for one Bot or for every Bot.
pub async fn streaming_messages(
    db: &SqlitePool,
    user: UserId,
    bot: Option<BotId>,
) -> Result<Vec<MessageId>> {
    let rows = sqlx::query(
        "SELECT id FROM messages WHERE user_id = ? AND state = 'streaming'
           AND (? IS NULL OR bot_id = ?)",
    )
    .bind(user.0)
    .bind(bot.map(|b| b.0))
    .bind(bot.map(|b| b.0))
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(|r| MessageId(r.get("id"))).collect())
}

/// A page of a Bot's conversation, oldest first, ending before `before`.
pub async fn messages(
    db: &SqlitePool,
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

/// The start of each of the user's Bots' latest message.
pub async fn previews(
    db: &SqlitePool,
    user: UserId,
) -> Result<std::collections::HashMap<BotId, MessagePreview>> {
    let rows = sqlx::query(
        "SELECT m.bot_id, m.author, substr(m.text, 1, ?) AS text, m.updated_at
         FROM messages m
         WHERE m.user_id = ?
           AND m.id = (SELECT max(id) FROM messages WHERE bot_id = m.bot_id)",
    )
    .bind(PREVIEW_CHARS as i64)
    .bind(user.0)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            (
                BotId(r.get("bot_id")),
                MessagePreview {
                    author: author_from(r.get::<&str, _>("author")),
                    text: r.get("text"),
                    at: Timestamp(r.get("updated_at")),
                },
            )
        })
        .collect())
}
