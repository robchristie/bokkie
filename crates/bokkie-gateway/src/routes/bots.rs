//! Bots and their conversations. Every route acts only on the signed-in
//! user's own Bots; another user's Bot reads as not found.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use bokkie_protocol::api::{
    Bot, BotList, CreateBotRequest, MessagePage, SendMessageRequest, SendMessageResponse,
    StopResponse, UpdateBotRequest,
};
use bokkie_protocol::events::Event;
use bokkie_protocol::{BotId, MessageId};
use serde::Deserialize;

use crate::AppState;
use crate::auth::SignedIn;
use crate::bots::store::{self, BotChanges, BotRow};
use crate::error::{Body, Failure, Result};
use sqlx::sqlite::SqliteExecutor;

async fn to_api(state: &AppState, db: impl SqliteExecutor<'_>, bot: &BotRow) -> Result<Bot> {
    let previews = store::previews(db, bot.user).await?;
    Ok(bot.to_api(
        state.conversations.is_working(bot.id),
        previews.get(&bot.id).cloned(),
    ))
}

/// `GET /api/v1/bots`.
pub async fn list(State(state): State<AppState>, signed_in: SignedIn) -> Result<Json<BotList>> {
    // Read the watermark first: the list then reflects at least every event
    // up to it.
    let seq = state.events.latest(signed_in.id()).await?;
    let mut previews = store::previews(&state.db, signed_in.id()).await?;
    let bots = store::list_bots(&state.db, signed_in.id())
        .await?
        .into_iter()
        .map(|bot| {
            let preview = previews.remove(&bot.id);
            bot.to_api(state.conversations.is_working(bot.id), preview)
        })
        .collect();
    Ok(Json(BotList { bots, seq }))
}

/// `POST /api/v1/bots`.
pub async fn create(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Body(request): Body<CreateBotRequest>,
) -> Result<(StatusCode, Json<Bot>)> {
    // The Bot and its event commit together, or neither does.
    let mut batch = state.events.begin(signed_in.id()).await?;
    let bot = store::create_bot(
        &mut *batch,
        state.clock.now(),
        signed_in.id(),
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
    // The edit and its event commit together under the user's log lock, so
    // concurrent edits announce the Bot in the order they took effect.
    let mut batch = state.events.begin(signed_in.id()).await?;
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

#[derive(Debug, Deserialize)]
pub struct PageQuery {
    before: Option<i64>,
    limit: Option<u32>,
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
