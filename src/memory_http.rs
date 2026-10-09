//! Memory uses the service's existing CSRF and database command boundaries.
use crate::{
    SystemClock, UnixClock,
    http::{ApiError, ApiState},
};
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use bokkie_operator_api::*;
use serde::Deserialize;

pub fn routes() -> Router<ApiState> {
    Router::new().route("/memory", get(list).post(save))
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    after: Option<String>,
}
async fn list(
    State(state): State<ApiState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<MemoryList>, ApiError> {
    let service = state.runtime.identity();
    Ok(Json(
        state
            .executor
            .execute(move |s| s.memory_list(query.after.as_deref(), service))
            .await?,
    ))
}
async fn save(
    State(state): State<ApiState>,
    Json(request): Json<MemoryCommandRequest>,
) -> Result<Json<MemorySaved>, ApiError> {
    let now = state
        .conversation
        .as_ref()
        .map_or_else(|| SystemClock.now(), |c| c.now());
    let entry = state
        .executor
        .execute(move |s| s.memory_command(&request, now))
        .await?;
    Ok(Json(MemorySaved {
        service: state.runtime.identity(),
        entry,
    }))
}
