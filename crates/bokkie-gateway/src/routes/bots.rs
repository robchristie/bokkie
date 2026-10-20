//! Bots and their conversations, grants, notes and tool calls. Every route
//! acts only on the signed-in user's own Bots; another user's Bot reads as
//! not found, even for an admin, whose view of a member's Bots is in
//! [`super::admin`]. A member without a permission is refused creating Bots
//! or changing their profiles, and a member may add only the grants
//! [`grants::member_may_grant`] allows.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use bokkie_protocol::api::{
    Bot, BotList, BotNotes, CreateBotRequest, MarkReadRequest, MessagePage, NotificationPage,
    ReadState, SendMessageRequest, SendMessageResponse, StopResponse, ToolCallPage,
    UpdateBotRequest,
};
use bokkie_protocol::events::Event;
use bokkie_protocol::{BotId, MessageId, NotificationId};
use serde::Deserialize;

use crate::AppState;
use crate::accounts::{self, Account};
use crate::auth::SignedIn;
use crate::bots::store::{self, BotChanges, BotRow};
use crate::error::{Body, Failure, Path, Query, Result};
use crate::{grants, tools};
use sqlx::sqlite::SqliteExecutor;

/// The Bot as its owner sees it.
pub(crate) async fn to_api(
    state: &AppState,
    db: impl SqliteExecutor<'_>,
    bot: &BotRow,
) -> Result<Bot> {
    let mut activity = store::activity(db, bot.user, Some(bot.id)).await?;
    Ok(bot.to_api(
        state.conversations.is_working(bot.id),
        activity.remove(&bot.id).unwrap_or_default(),
    ))
}

/// `GET /api/v1/bots`.
pub async fn list(State(state): State<AppState>, signed_in: SignedIn) -> Result<Json<BotList>> {
    // Read the watermark first: the list then reflects at least every event
    // up to it.
    let seq = state.events.latest(signed_in.id()).await?;
    // One read transaction, so the Bots and their activity agree.
    let mut tx = state.db.begin().await?;
    let mut activity = store::activity(&mut *tx, signed_in.id(), None).await?;
    let bots: Vec<Bot> = store::list_bots(&mut *tx, signed_in.id())
        .await?
        .into_iter()
        .map(|bot| {
            let activity = activity.remove(&bot.id).unwrap_or_default();
            bot.to_api(state.conversations.is_working(bot.id), activity)
        })
        .collect();
    drop(tx);
    let unread_bots = bots.iter().filter(|bot| bot.unread).count();
    Ok(Json(BotList {
        bots,
        unread_bots: u32::try_from(unread_bots).unwrap_or(u32::MAX),
        seq,
    }))
}

/// `POST /api/v1/bots`.
pub async fn create(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Body(request): Body<CreateBotRequest>,
) -> Result<(StatusCode, Json<Bot>)> {
    // The Bot and its event commit together, or neither does. The
    // permission is read in the same transaction, so a Bot is never made
    // after an admin withdrew it.
    let mut batch = state.events.begin(signed_in.id()).await?;
    if !own_account(&mut *batch, signed_in.id())
        .await?
        .user
        .permissions
        .may_create_bots
    {
        return Err(Failure::forbidden("Only an admin can create your Bots"));
    }
    let bot = store::create_bot(
        &mut *batch,
        state.clock.now(),
        signed_in.id(),
        None,
        &request.name,
        &request.label,
        &request.description,
        request.avatar.as_deref(),
    )
    .await?;
    let bot = to_api(&state, &mut *batch, &bot).await?;
    batch.emit(Event::BotCreated { bot: bot.clone() }).await?;
    batch.commit().await?;
    Ok((StatusCode::CREATED, Json(bot)))
}

/// `GET /api/v1/bots/{id}`.
pub async fn get(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Path(id): Path<i64>,
) -> Result<Json<Bot>> {
    let bot = store::bot(&state.db, signed_in.id(), BotId(id))
        .await?
        .ok_or_else(Failure::not_found)?;
    Ok(Json(to_api(&state, &state.db, &bot).await?))
}

/// `PATCH /api/v1/bots/{id}`.
pub async fn update(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Path(id): Path<i64>,
    Body(request): Body<UpdateBotRequest>,
) -> Result<Json<Bot>> {
    // A grant change waits for any send to the Bot, so that every turn runs
    // on a thread whose grants are the Bot's.
    let _sends = match request.grants {
        Some(_) => Some(
            state
                .conversations
                .lock_bot(signed_in.id(), BotId(id))
                .await,
        ),
        None => None,
    };
    // The edit and its event commit together under the user's log lock, so
    // concurrent edits announce the Bot in the order they took effect.
    let mut batch = state.events.begin(signed_in.id()).await?;
    let profile = request.name.is_some()
        || request.label.is_some()
        || request.description.is_some()
        || request.avatar.is_some();
    if profile
        && !own_account(&mut *batch, signed_in.id())
            .await?
            .user
            .permissions
            .may_edit_bot_profiles
    {
        return Err(Failure::forbidden(
            "Only an admin can change your Bots' profiles",
        ));
    }
    if let Some(requested) = &request.grants {
        // The role is read in the same transaction as the change.
        let role = own_account(&mut *batch, signed_in.id()).await?.user.role;
        let existing = store::bot(&mut *batch, signed_in.id(), BotId(id))
            .await?
            .ok_or_else(Failure::not_found)?;
        let grants = grants::check_change(role, &existing.grants, requested)?;
        if grants != existing.grants {
            store::set_grants(
                &mut *batch,
                state.clock.now(),
                signed_in.id(),
                existing.id,
                &grants,
            )
            .await?;
            tracing::info!(
                user = %signed_in.id(),
                bot = id,
                grants = %grants::to_stored(&grants),
                "set a Bot's grants"
            );
        }
    }
    let bot = store::update_bot(
        &mut *batch,
        state.clock.now(),
        signed_in.id(),
        BotId(id),
        BotChanges {
            name: request.name,
            label: request.label,
            description: request.description,
            avatar: request.avatar,
            pinned: request.pinned,
            hidden: request.hidden,
        },
    )
    .await?;
    let bot = to_api(&state, &mut *batch, &bot).await?;
    batch.emit(Event::BotUpdated { bot: bot.clone() }).await?;
    batch.commit().await?;
    Ok(Json(bot))
}

/// The signed-in user's account as it is now, for its permissions.
async fn own_account(
    db: impl SqliteExecutor<'_>,
    user: bokkie_protocol::UserId,
) -> Result<Account> {
    accounts::account(db, user)
        .await?
        .ok_or_else(Failure::unauthenticated)
}

#[derive(Debug, Deserialize)]
pub struct PageQuery {
    pub before: Option<i64>,
    pub limit: Option<u32>,
}

/// `GET /api/v1/bots/{id}/messages?before=&limit=`.
pub async fn messages(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Path(id): Path<i64>,
    Query(query): Query<PageQuery>,
) -> Result<Json<MessagePage>> {
    store::bot(&state.db, signed_in.id(), BotId(id))
        .await?
        .ok_or_else(Failure::not_found)?;
    let seq = state.events.latest(signed_in.id()).await?;
    let (messages, older) = store::messages(
        &state.db,
        signed_in.id(),
        BotId(id),
        query.before.map(MessageId),
        query.limit.unwrap_or(50),
    )
    .await?;
    Ok(Json(MessagePage {
        messages,
        older,
        seq,
    }))
}

/// `POST /api/v1/bots/{id}/messages`.
pub async fn send(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Path(id): Path<i64>,
    Body(request): Body<SendMessageRequest>,
) -> Result<Json<SendMessageResponse>> {
    // A send runs to completion in its own task, so a client that goes away
    // midway cannot leave it half done.
    let conversations = state.conversations.clone();
    let send = tokio::spawn(async move {
        conversations
            .send(&signed_in.account, BotId(id), &request.text)
            .await
    });
    Ok(Json(send.await.map_err(Failure::internal)??))
}

/// `POST /api/v1/bots/{id}/stop`.
pub async fn stop(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Path(id): Path<i64>,
) -> Result<Json<StopResponse>> {
    let stopped = state.conversations.stop(signed_in.id(), BotId(id)).await?;
    Ok(Json(StopResponse { stopped }))
}

/// The Bot, if it is the signed-in user's.
async fn own_bot(state: &AppState, signed_in: &SignedIn, id: i64) -> Result<BotRow> {
    store::bot(&state.db, signed_in.id(), BotId(id))
        .await?
        .ok_or_else(Failure::not_found)
}

/// `GET /api/v1/bots/{id}/notes`.
pub async fn notes(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Path(id): Path<i64>,
) -> Result<Json<BotNotes>> {
    let bot = own_bot(&state, &signed_in, id).await?;
    Ok(Json(
        tools::notes::read(&state.db, signed_in.id(), bot.id).await?,
    ))
}

/// `DELETE /api/v1/bots/{id}/notes`.
pub async fn clear_notes(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Path(id): Path<i64>,
) -> Result<StatusCode> {
    let bot = own_bot(&state, &signed_in, id).await?;
    tools::notes::clear(&state.db, signed_in.id(), bot.id).await?;
    tracing::info!(user = %signed_in.id(), bot = id, "cleared a Bot's notes");
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct CallQuery {
    pub before: Option<i64>,
    pub limit: Option<u32>,
}

/// `GET /api/v1/bots/{id}/tool-calls?before=&limit=`.
pub async fn tool_calls(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Path(id): Path<i64>,
    Query(query): Query<CallQuery>,
) -> Result<Json<ToolCallPage>> {
    let bot = own_bot(&state, &signed_in, id).await?;
    let (calls, older) = tools::audit::page(
        &state.db,
        signed_in.id(),
        bot.id,
        query.before,
        query.limit.unwrap_or(50),
    )
    .await?;
    Ok(Json(ToolCallPage { calls, older }))
}

/// `PUT /api/v1/bots/{id}/read`: moves the user's read marker on their Bot
/// forward, and tells their other devices.
pub async fn mark_read(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Path(id): Path<i64>,
    Body(request): Body<MarkReadRequest>,
) -> Result<Json<ReadState>> {
    let user = signed_in.id();
    // The marker and its event commit together.
    let mut batch = state.events.begin(user).await?;
    let bot = store::bot(&mut *batch, user, BotId(id))
        .await?
        .ok_or_else(Failure::not_found)?;
    let message = store::message(&mut *batch, user, request.message_id).await?;
    if message.is_none_or(|message| message.bot_id != bot.id) {
        return Err(Failure::invalid(
            "message_id names none of the Bot's messages",
        ));
    }
    let moved = store::mark_read(
        &mut *batch,
        state.clock.now(),
        user,
        bot.id,
        request.message_id,
    )
    .await?;
    let read = store::read_state(&mut *batch, user, bot.id)
        .await?
        .ok_or_else(Failure::not_found)?;
    if moved {
        batch
            .emit(Event::ReadUpdated { read: read.clone() })
            .await?;
        batch.commit().await?;
    }
    Ok(Json(read))
}

/// `GET /api/v1/notifications?before=&limit=`: the user's notifications
/// from every Bot of theirs, newest first.
pub async fn notifications(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Query(query): Query<CallQuery>,
) -> Result<Json<NotificationPage>> {
    let (notifications, older) = tools::notify::page(
        &state.db,
        signed_in.id(),
        query.before.map(NotificationId),
        query.limit.unwrap_or(50),
    )
    .await?;
    Ok(Json(NotificationPage {
        notifications,
        older,
    }))
}
