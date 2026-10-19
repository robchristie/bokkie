//! Routines and their run history. Every route acts only on the signed-in
//! user's own Bots and routines; another user's read as not found, even for
//! an admin. The admin routes give no view of a member's routines.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use bokkie_protocol::api::{
    CreateRoutineRequest, Routine, RoutineList, RoutineRun, RoutineRunPage, UpdateRoutineRequest,
};
use bokkie_protocol::{BotId, RoutineId, RoutineRunId};
use serde::Deserialize;

use crate::AppState;
use crate::auth::SignedIn;
use crate::bots::store as bots;
use crate::error::{Body, Failure, Path, Query, Result};
use crate::routines::store;

/// The Bot, if it is the signed-in user's.
async fn own_bot(state: &AppState, signed_in: &SignedIn, id: i64) -> Result<BotId> {
    bots::bot(&state.db, signed_in.id(), BotId(id))
        .await?
        .map(|bot| bot.id)
        .ok_or_else(Failure::not_found)
}

/// `GET /api/v1/routines`: every routine of the user's.
pub async fn list_all(
    State(state): State<AppState>,
    signed_in: SignedIn,
) -> Result<Json<RoutineList>> {
    let seq = state.events.latest(signed_in.id()).await?;
    let routines = store::list_for_user(&state.db, signed_in.id())
        .await?
        .iter()
        .map(store::RoutineRow::to_api)
        .collect();
    Ok(Json(RoutineList { routines, seq }))
}

/// `GET /api/v1/bots/{id}/routines`.
pub async fn list(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Path(id): Path<i64>,
) -> Result<Json<RoutineList>> {
    let bot = own_bot(&state, &signed_in, id).await?;
    let seq = state.events.latest(signed_in.id()).await?;
    let routines = store::list_for_bot(&state.db, signed_in.id(), bot)
        .await?
        .iter()
        .map(store::RoutineRow::to_api)
        .collect();
    Ok(Json(RoutineList { routines, seq }))
}

/// `POST /api/v1/bots/{id}/routines`.
pub async fn create(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Path(id): Path<i64>,
    Body(request): Body<CreateRoutineRequest>,
) -> Result<(StatusCode, Json<Routine>)> {
    let routine = state
        .routines
        .create(signed_in.id(), BotId(id), request)
        .await?;
    Ok((StatusCode::CREATED, Json(routine)))
}

/// `GET /api/v1/bots/{id}/routines/{routine}`.
pub async fn get(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Path((id, routine)): Path<(i64, i64)>,
) -> Result<Json<Routine>> {
    let routine = store::routine(&state.db, signed_in.id(), BotId(id), RoutineId(routine))
        .await?
        .ok_or_else(Failure::not_found)?;
    Ok(Json(routine.to_api()))
}

/// `PATCH /api/v1/bots/{id}/routines/{routine}`.
pub async fn update(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Path((id, routine)): Path<(i64, i64)>,
    Body(request): Body<UpdateRoutineRequest>,
) -> Result<Json<Routine>> {
    Ok(Json(
        state
            .routines
            .update(signed_in.id(), BotId(id), RoutineId(routine), request)
            .await?,
    ))
}

/// `DELETE /api/v1/bots/{id}/routines/{routine}`.
pub async fn delete(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Path((id, routine)): Path<(i64, i64)>,
) -> Result<StatusCode> {
    state
        .routines
        .delete(signed_in.id(), BotId(id), RoutineId(routine))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/bots/{id}/routines/{routine}/run`: "run now".
pub async fn run_now(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Path((id, routine)): Path<(i64, i64)>,
) -> Result<Json<RoutineRun>> {
    // Starting the run's turn runs to completion in its own task, so a
    // client that goes away midway cannot leave it half done.
    let routines = state.routines.clone();
    let user = signed_in.id();
    let run =
        tokio::spawn(async move { routines.run_now(user, BotId(id), RoutineId(routine)).await });
    Ok(Json(run.await.map_err(Failure::internal)??))
}

#[derive(Debug, Deserialize)]
pub struct RunQuery {
    pub before: Option<i64>,
    pub limit: Option<u32>,
}

/// `GET /api/v1/bots/{id}/routines/{routine}/runs?before=&limit=`.
pub async fn runs(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Path((id, routine)): Path<(i64, i64)>,
    Query(query): Query<RunQuery>,
) -> Result<Json<RoutineRunPage>> {
    let routine = store::routine(&state.db, signed_in.id(), BotId(id), RoutineId(routine))
        .await?
        .ok_or_else(Failure::not_found)?;
    let mut conn = state.db.acquire().await?;
    let (runs, older) = store::runs_page(
        &mut conn,
        signed_in.id(),
        routine.id,
        query.before.map(RoutineRunId),
        query.limit.unwrap_or(50),
    )
    .await?;
    Ok(Json(RoutineRunPage { runs, older }))
}
