//! Model-free adapters using the existing same-origin and mutation-token boundary.
use crate::{
    StoreError, SystemClock, UnixClock,
    http::{ApiError, ApiState},
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use bokkie_operator_api::*;
use serde::Deserialize;

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/projects", get(projects).post(save_project))
        .route("/handoffs", get(list))
        .route("/handoffs/{id}", get(view))
        .route("/handoffs/save", post(save))
        .route("/handoffs/activity", post(activity))
}
fn now(state: &ApiState) -> i64 {
    state
        .conversation
        .as_ref()
        .map_or_else(|| SystemClock.now(), |c| c.now())
}
async fn projects(State(state): State<ApiState>) -> Result<Json<ProjectList>, ApiError> {
    let items = state.executor.execute(|s| s.workspace_projects()).await?;
    Ok(Json(ProjectList {
        service: state.runtime.identity(),
        items,
    }))
}
async fn save_project(
    State(state): State<ApiState>,
    Json(request): Json<ProjectSaveRequest>,
) -> Result<Json<ProjectList>, ApiError> {
    let clock = now(&state);
    state
        .executor
        .execute(move |s| s.workspace_project_save(&request, clock))
        .await?;
    projects(State(state)).await
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    after: Option<String>,
}
async fn list(
    State(state): State<ApiState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<HandoffList>, ApiError> {
    let service = state.runtime.identity();
    Ok(Json(
        state
            .executor
            .execute(move |s| s.handoff_list(query.after.as_deref(), service))
            .await?,
    ))
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewQuery {
    revision: Option<i64>,
}
async fn view(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(query): Query<ViewQuery>,
) -> Result<Json<HandoffView>, ApiError> {
    if query.revision.is_some_and(|r| r < 1) {
        return Err(StoreError::Invalid("Hand-off revision must be positive".into()).into());
    }
    let service = state.runtime.identity();
    Ok(Json(
        state
            .executor
            .execute(move |s| s.handoff_view(&id, query.revision, service))
            .await?,
    ))
}
async fn save(
    State(state): State<ApiState>,
    Json(request): Json<HandoffSaveRequest>,
) -> Result<Json<HandoffView>, ApiError> {
    let origin = state.runtime.origin().to_owned();
    let clock = now(&state);
    let saved = state
        .executor
        .execute(move |s| s.handoff_save(&request, &origin, clock))
        .await?;
    let service = state.runtime.identity();
    Ok(Json(
        state
            .executor
            .execute(move |s| s.handoff_view(&saved.id, Some(saved.revision), service))
            .await?,
    ))
}
async fn activity(
    State(state): State<ApiState>,
    Json(request): Json<HandoffActivityRequest>,
) -> Result<Json<HandoffView>, ApiError> {
    let clock = now(&state);
    let id = request.handoff_id.clone();
    let revision = request.revision;
    state
        .executor
        .execute(move |s| s.handoff_activity(&request, clock))
        .await?;
    let service = state.runtime.identity();
    Ok(Json(
        state
            .executor
            .execute(move |s| s.handoff_view(&id, Some(revision), service))
            .await?,
    ))
}
