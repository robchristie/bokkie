//! Loopback HTTP adapter. Lifecycle decisions remain owned by [`crate::Store`].

use std::{net::SocketAddr, path::PathBuf};

use axum::{
    Json, Router,
    extract::{Path as AxumPath, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    ApprovalDecision, NewObligation, Obligation, Recurrence, RetryPolicy, Store, StoreError,
    SystemClock, UnixClock,
};

#[derive(Debug, Clone)]
pub struct ApiState {
    pub database: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateRequest {
    pub id: Option<String>,
    pub description: String,
    pub scheduled_at: Option<i64>,
    pub recurrence_cron: Option<String>,
    pub recurrence_timezone: Option<String>,
    #[serde(default)]
    pub approval_required: bool,
    pub max_attempts: Option<u32>,
    pub retry_base_seconds: Option<i64>,
    pub retry_max_seconds: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DecisionRequest {
    pub actor: String,
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        let (status, code) = match &error {
            StoreError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            StoreError::Invalid(_) | StoreError::Recurrence(_) => {
                (StatusCode::BAD_REQUEST, "invalid_request")
            }
            StoreError::Conflict(_) | StoreError::Fenced => {
                (StatusCode::CONFLICT, "transition_conflict")
            }
            StoreError::Sql(rusqlite::Error::SqliteFailure(sqlite, _))
                if sqlite.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                (StatusCode::CONFLICT, "constraint_conflict")
            }
            StoreError::Sql(_) => (StatusCode::INTERNAL_SERVER_ERROR, "storage_error"),
        };
        Self {
            status,
            code,
            message: error.to_string(),
        }
    }
}

pub fn router(database: PathBuf) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/obligations", post(create).get(list))
        .route("/obligations/{id}", get(show))
        .route("/obligations/{id}/approve", post(approve))
        .route("/obligations/{id}/reject", post(reject))
        .route("/obligations/{id}/retry", post(retry))
        .route("/obligations/{id}/cancel", post(cancel))
        .route("/obligations/{id}/events", get(events))
        .route("/obligations/{id}/attempts", get(attempts))
        .method_not_allowed_fallback(method_not_allowed)
        .fallback(not_found_route)
        .with_state(ApiState { database })
}

pub fn validate_loopback(address: SocketAddr) -> Result<(), String> {
    if address.ip().is_loopback() {
        Ok(())
    } else {
        Err(format!(
            "refusing non-loopback bind {address}: authentication and remote exposure are out of scope"
        ))
    }
}

async fn health(State(state): State<ApiState>) -> Result<Response, ApiError> {
    with_store(&state, |store, _| {
        store.list()?;
        Ok(HealthResponse { status: "ok" })
    })
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn create(
    State(state): State<ApiState>,
    request: Result<Json<CreateRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    with_store(&state, |store, now| {
        let obligation = new_obligation(request, now)?;
        store.create(obligation, now)
    })
    .map(|body| (StatusCode::CREATED, Json(body)).into_response())
}

async fn list(State(state): State<ApiState>) -> Result<Response, ApiError> {
    with_store(&state, |store, _| store.list())
        .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn show(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    with_store(&state, |store, _| require_obligation(store, &id))
        .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn approve(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
    request: Result<Json<DecisionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    decide(state, id, request, ApprovalDecision::Approved).await
}

async fn reject(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
    request: Result<Json<DecisionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    decide(state, id, request, ApprovalDecision::Rejected).await
}

async fn decide(
    state: ApiState,
    id: String,
    request: DecisionRequest,
    decision: ApprovalDecision,
) -> Result<Response, ApiError> {
    with_store(&state, |store, now| {
        store.decide_approval(&id, decision, &request.actor, request.note.as_deref(), now)?;
        require_obligation(store, &id)
    })
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn retry(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    with_store(&state, |store, now| {
        store.retry_attention(&id, now)?;
        require_obligation(store, &id)
    })
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn cancel(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    with_store(&state, |store, now| {
        store.cancel(&id, now)?;
        require_obligation(store, &id)
    })
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn events(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    with_store(&state, |store, _| {
        require_obligation(store, &id)?;
        store.events(&id)
    })
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn attempts(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    with_store(&state, |store, _| {
        require_obligation(store, &id)?;
        store.attempts(&id)
    })
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

fn with_store<T>(
    state: &ApiState,
    operation: impl FnOnce(&mut Store, i64) -> Result<T, StoreError>,
) -> Result<T, ApiError> {
    let mut store = Store::open(&state.database)?;
    operation(&mut store, SystemClock.now()).map_err(ApiError::from)
}

fn require_obligation(store: &Store, id: &str) -> Result<Obligation, StoreError> {
    store
        .get(id)?
        .ok_or_else(|| StoreError::NotFound(id.to_owned()))
}

fn new_obligation(request: CreateRequest, now: i64) -> Result<NewObligation, StoreError> {
    let recurrence = match (request.recurrence_cron, request.recurrence_timezone) {
        (Some(expression), Some(timezone)) => Some(Recurrence::new(expression, timezone)?),
        (None, None) => None,
        _ => {
            return Err(StoreError::Invalid(
                "recurrence_cron and recurrence_timezone must be supplied together".to_owned(),
            ));
        }
    };
    let defaults = RetryPolicy::default();
    Ok(NewObligation {
        id: request.id.unwrap_or_else(|| Uuid::new_v4().to_string()),
        description: request.description,
        scheduled_at: request.scheduled_at.unwrap_or(now),
        recurrence,
        approval_required: request.approval_required,
        retry: RetryPolicy {
            max_attempts: request.max_attempts.unwrap_or(defaults.max_attempts),
            base_delay_seconds: request
                .retry_base_seconds
                .unwrap_or(defaults.base_delay_seconds),
            max_delay_seconds: request
                .retry_max_seconds
                .unwrap_or(defaults.max_delay_seconds),
        },
    })
}

async fn not_found_route() -> ApiError {
    ApiError {
        status: StatusCode::NOT_FOUND,
        code: "route_not_found",
        message: "HTTP route was not found".to_owned(),
    }
}

async fn method_not_allowed() -> ApiError {
    ApiError {
        status: StatusCode::METHOD_NOT_ALLOWED,
        code: "method_not_allowed",
        message: "HTTP method is not allowed for this route".to_owned(),
    }
}

fn invalid_json(error: JsonRejection) -> ApiError {
    ApiError {
        status: error.status(),
        code: "invalid_json",
        message: error.body_text(),
    }
}

pub fn error_json(code: &'static str, message: impl Into<String>) -> Value {
    json!({"error": {"code": code, "message": message.into()}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_bind_is_rejected() {
        let error = validate_loopback("0.0.0.0:7744".parse().unwrap()).unwrap_err();
        assert!(error.contains("authentication"));
        assert!(validate_loopback("127.0.0.1:7744".parse().unwrap()).is_ok());
        assert!(validate_loopback("[::1]:7744".parse().unwrap()).is_ok());
    }
}
