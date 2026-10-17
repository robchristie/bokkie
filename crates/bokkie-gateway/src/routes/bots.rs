//! Bots and their conversations. Every route acts only on the signed-in
//! user's own Bots; another user's Bot reads as not found.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use bokkie_protocol::api::{
    Bot, BotList, CreateBotRequest, MessagePage, SendMessageRequest, SendMessageResponse,
    StopResponse, UpdateBotRequest,
};
use bokkie_protocol::{BotId, MessageId};
use serde::Deserialize;

use crate::AppState;
use crate::auth::SignedIn;
use crate::bots::store::{self, BotChanges, BotRow};
use crate::error::{Body, Failure, Result};

async fn to_api(state: &AppState, bot: &BotRow) -> Result<Bot> {
    let previews = store::previews(&state.db, bot.user).await?;
    Ok(bot.to_api(
        state.conversations.is_working(bot.id),
        previews.get(&bot.id).cloned(),
    ))
}

/// `GET /api/v1/bots`.
pub async fn list(State(state): State<AppState>, signed_in: SignedIn) -> Result<Json<BotList>> {
    let mut previews = store::previews(&state.db, signed_in.id()).await?;
    let bots = store::list_bots(&state.db, signed_in.id())
        .await?
        .into_iter()
        .map(|bot| {
            let preview = previews.remove(&bot.id);
            bot.to_api(state.conversations.is_working(bot.id), preview)
        })
        .collect();
    Ok(Json(BotList { bots }))
}

/// `POST /api/v1/bots`.
pub async fn create(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Body(request): Body<CreateBotRequest>,
) -> Result<(StatusCode, Json<Bot>)> {
    let bot = store::create_bot(
        &state.db,
        state.clock.now(),
        signed_in.id(),
        &request.name,
        &request.label,
        &request.description,
        request.avatar.as_deref(),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(to_api(&state, &bot).await?)))
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
    Ok(Json(to_api(&state, &bot).await?))
}

/// `PATCH /api/v1/bots/{id}`.
pub async fn update(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Path(id): Path<i64>,
    Body(request): Body<UpdateBotRequest>,
) -> Result<Json<Bot>> {
    let bot = store::update_bot(
        &state.db,
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
    Ok(Json(to_api(&state, &bot).await?))
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
    let (messages, older) = store::messages(
        &state.db,
        signed_in.id(),
        BotId(id),
        query.before.map(MessageId),
        query.limit.unwrap_or(50),
    )
    .await?;
    Ok(Json(MessagePage { messages, older }))
}

/// `POST /api/v1/bots/{id}/messages`.
pub async fn send(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Path(id): Path<i64>,
    Body(request): Body<SendMessageRequest>,
) -> Result<Json<SendMessageResponse>> {
    Ok(Json(
        state
            .conversations
            .send(&signed_in.account, BotId(id), &request.text)
            .await?,
    ))
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
