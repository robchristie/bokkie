//! Loopback HTTP adapter. Lifecycle decisions remain owned by [`crate::Store`].

use std::{net::SocketAddr, path::PathBuf};

use axum::{
    Json, Router,
    extract::{Path as AxumPath, State, rejection::JsonRejection},
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tower_http::services::ServeDir;
use uuid::Uuid;

use crate::{
    ApprovalDecision, CANONICAL_DEFAULT_BRANCH, CANONICAL_REPOSITORY, DbExecutor, DbExecutorError,
    GardenerImplementationRun, GardenerInspection, NewObligation, NewRepositoryRegistration,
    Obligation, Proposal, Recurrence, RepositoryRegistration, RetryPolicy, Store, StoreError,
    SystemClock, UnixClock,
    gardener::ProposalInstance,
    http_security::{ApiRuntime, bootstrap_response, enforce},
};
use bokkie_operator_api::ActionPrecondition;

#[derive(Debug, Clone)]
pub struct ApiState {
    pub executor: DbExecutor,
    pub runtime: ApiRuntime,
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

#[derive(Debug, Deserialize, Serialize)]
pub struct OperatorActionRequest {
    pub precondition: ActionPrecondition,
    #[serde(default)]
    pub actor: String,
    pub note: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GardenerRegistrationRequest {
    #[serde(default = "canonical_repository")]
    pub repository: String,
    #[serde(default = "canonical_default_branch")]
    pub default_branch: String,
    pub checkout_path: String,
    pub first_inspection_at: Option<i64>,
    #[serde(default = "default_inspection_cron")]
    pub recurrence_cron: String,
    #[serde(default = "default_inspection_timezone")]
    pub recurrence_timezone: String,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    service: bokkie_operator_api::ServiceIdentity,
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

impl From<DbExecutorError> for ApiError {
    fn from(error: DbExecutorError) -> Self {
        match error {
            DbExecutorError::Store(error) | DbExecutorError::Open(error) => error.into(),
            DbExecutorError::QueueFull => Self {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "storage_queue_full",
                message: error.to_string(),
            },
            DbExecutorError::Shutdown
            | DbExecutorError::Panicked
            | DbExecutorError::Thread(_)
            | DbExecutorError::ShutdownTimedOut => Self {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "storage_executor_unavailable",
                message: error.to_string(),
            },
        }
    }
}

pub fn router(database: PathBuf, address: SocketAddr) -> Router {
    drop(
        Store::open(&database)
            .expect("HTTP database must be migratable before router construction"),
    );
    let executor = DbExecutor::start(database)
        .expect("HTTP database must be migrated and compatible before router construction");
    router_with_executor(
        executor,
        ApiRuntime::new(address, schema_version()).expect("OS randomness must be available"),
    )
}

pub fn router_with_executor(executor: DbExecutor, runtime: ApiRuntime) -> Router {
    let security = runtime.clone();
    router_core(executor, runtime).layer(middleware::from_fn_with_state(security, enforce))
}

fn router_core(executor: DbExecutor, runtime: ApiRuntime) -> Router {
    let state = ApiState { executor, runtime };
    Router::new()
        .route("/bootstrap", get(bootstrap))
        .route("/health", get(health))
        .route("/operator/snapshot", get(operator_snapshot))
        .route("/operator/obligations/{id}/topic", get(operator_topic))
        .route("/operator/obligations/{id}/approve", post(operator_approve))
        .route("/operator/obligations/{id}/reject", post(operator_reject))
        .route("/operator/obligations/{id}/retry", post(operator_retry))
        .route("/operator/obligations/{id}/cancel", post(operator_cancel))
        .route(
            "/operator/gardener/proposals/{fingerprint}/approve",
            post(operator_approve_gardener_proposal),
        )
        .route(
            "/operator/gardener/proposals/{fingerprint}/reject",
            post(operator_reject_gardener_proposal),
        )
        .route(
            "/operator/gardener/proposal-instances/{instance_id}/approve",
            post(operator_approve_gardener_proposal_instance),
        )
        .route(
            "/operator/gardener/proposal-instances/{instance_id}/reject",
            post(operator_reject_gardener_proposal_instance),
        )
        .route("/obligations", post(create).get(list))
        .route("/obligations/{id}", get(show))
        .route("/obligations/{id}/approve", post(approve))
        .route("/obligations/{id}/reject", post(reject))
        .route("/obligations/{id}/retry", post(retry))
        .route("/obligations/{id}/cancel", post(cancel))
        .route("/obligations/{id}/events", get(events))
        .route("/obligations/{id}/attempts", get(attempts))
        .route(
            "/gardener/repository",
            post(register_gardener_repository).get(show_gardener_repository),
        )
        .route("/gardener/inspections", get(list_gardener_inspections))
        .route("/gardener/inspections/{id}", get(show_gardener_inspection))
        .route("/gardener/proposals", get(list_gardener_proposals))
        .route(
            "/gardener/proposals/{fingerprint}",
            get(show_gardener_proposal),
        )
        .route(
            "/gardener/proposals/{fingerprint}/observations",
            get(gardener_proposal_observations),
        )
        .route(
            "/gardener/proposals/{fingerprint}/approve",
            post(approve_gardener_proposal),
        )
        .route(
            "/gardener/proposals/{fingerprint}/reject",
            post(reject_gardener_proposal),
        )
        .route(
            "/gardener/proposal-instances",
            get(list_gardener_proposal_instances),
        )
        .route(
            "/gardener/proposal-instances/{instance_id}",
            get(show_gardener_proposal_instance),
        )
        .route(
            "/gardener/proposal-instances/{instance_id}/observations",
            get(gardener_proposal_instance_observations),
        )
        .route(
            "/gardener/proposal-instances/{instance_id}/approve",
            post(approve_gardener_proposal_instance),
        )
        .route(
            "/gardener/proposal-instances/{instance_id}/reject",
            post(reject_gardener_proposal_instance),
        )
        .route("/gardener/runs", get(list_gardener_runs))
        .route("/gardener/runs/{id}", get(show_gardener_run))
        .route("/gardener/runs/{id}/events", get(gardener_run_events))
        .method_not_allowed_fallback(method_not_allowed)
        .fallback(not_found_route)
        .with_state(state)
}

/// Add an explicit static UI directory to the same loopback service as the API.
///
/// Loopback validation remains the listener owner's responsibility. Serving the
/// browser application from this router keeps its requests same-origin and does
/// not add CORS or another network listener.
pub fn router_with_ui(database: PathBuf, ui_dir: PathBuf, address: SocketAddr) -> Router {
    drop(
        Store::open(&database)
            .expect("HTTP database must be migratable before router construction"),
    );
    let executor = DbExecutor::start(database)
        .expect("HTTP database must be migrated and compatible before router construction");
    router_with_ui_executor(
        executor,
        ui_dir,
        ApiRuntime::new(address, schema_version()).expect("OS randomness must be available"),
    )
}

pub fn router_with_ui_executor(
    executor: DbExecutor,
    ui_dir: PathBuf,
    runtime: ApiRuntime,
) -> Router {
    let security = runtime.clone();
    router_core(executor, runtime)
        .nest_service(
            "/ui",
            ServeDir::new(ui_dir).append_index_html_on_directories(true),
        )
        .layer(middleware::from_fn_with_state(security, enforce))
}

fn schema_version() -> i64 {
    crate::migration_manifest()
        .last()
        .expect("migration manifest is not empty")
        .version
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
    let identity = state.runtime.identity();
    with_store(&state, move |store, _| {
        store.list()?;
        Ok(HealthResponse {
            status: "ok",
            service: identity,
        })
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn bootstrap(State(state): State<ApiState>) -> Response {
    bootstrap_response(&state.runtime)
}

async fn create(
    State(state): State<ApiState>,
    request: Result<Json<CreateRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    with_store(&state, move |store, now| {
        let obligation = new_obligation(request, now)?;
        store.create(obligation, now)
    })
    .await
    .map(|body| (StatusCode::CREATED, Json(body)).into_response())
}

async fn list(State(state): State<ApiState>) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| store.list())
        .await
        .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn operator_snapshot(State(state): State<ApiState>) -> Result<Response, ApiError> {
    let identity = state.runtime.identity();
    with_store(&state, move |store, now| {
        let mut snapshot = store.operator_snapshot(now)?;
        snapshot.service = Some(identity);
        Ok(snapshot)
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn operator_topic(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, now| store.operator_topic(&id, now))
        .await
        .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn show(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| require_obligation(store, &id))
        .await
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
    with_store(&state, move |store, now| {
        store.decide_approval(&id, decision, &request.actor, request.note.as_deref(), now)?;
        require_obligation(store, &id)
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn retry(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, now| {
        store.retry_attention(&id, now)?;
        require_obligation(store, &id)
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn cancel(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, now| {
        store.cancel(&id, now)?;
        require_obligation(store, &id)
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn operator_approve(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
    request: Result<Json<OperatorActionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    operator_decide(state, id, request, ApprovalDecision::Approved).await
}

async fn operator_reject(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
    request: Result<Json<OperatorActionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    operator_decide(state, id, request, ApprovalDecision::Rejected).await
}

async fn operator_decide(
    state: ApiState,
    id: String,
    request: OperatorActionRequest,
    decision: ApprovalDecision,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, now| {
        store.decide_approval_if_current(
            &id,
            decision,
            &request.actor,
            request.note.as_deref(),
            &request.precondition,
            now,
        )?;
        require_obligation(store, &id)
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn operator_retry(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
    request: Result<Json<OperatorActionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    with_store(&state, move |store, now| {
        store.retry_attention_if_current(&id, &request.precondition, now)?;
        require_obligation(store, &id)
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn operator_cancel(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
    request: Result<Json<OperatorActionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    with_store(&state, move |store, now| {
        store.cancel_if_current(&id, &request.precondition, now)?;
        require_obligation(store, &id)
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn events(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| {
        require_obligation(store, &id)?;
        store.events(&id)
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn attempts(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| {
        require_obligation(store, &id)?;
        store.attempts(&id)
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn register_gardener_repository(
    State(state): State<ApiState>,
    request: Result<Json<GardenerRegistrationRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    with_store(&state, move |store, now| {
        let recurrence = Recurrence::new(request.recurrence_cron, request.recurrence_timezone)?;
        store.register_gardener_repository(
            NewRepositoryRegistration {
                repository: request.repository,
                default_branch: request.default_branch,
                checkout_path: request.checkout_path,
                inspection_recurrence: recurrence,
                first_inspection_at: request.first_inspection_at.unwrap_or(now),
            },
            now,
        )
    })
    .await
    .map(|body| (StatusCode::CREATED, Json(body)).into_response())
}

async fn show_gardener_repository(State(state): State<ApiState>) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| require_gardener_repository(store))
        .await
        .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn list_gardener_inspections(State(state): State<ApiState>) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| store.gardener_inspections())
        .await
        .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn show_gardener_inspection(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| {
        require_gardener_inspection(store, &id)
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn list_gardener_proposals(State(state): State<ApiState>) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| store.gardener_proposals())
        .await
        .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn show_gardener_proposal(
    State(state): State<ApiState>,
    AxumPath(fingerprint): AxumPath<String>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| {
        require_gardener_proposal(store, &fingerprint)
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn gardener_proposal_observations(
    State(state): State<ApiState>,
    AxumPath(fingerprint): AxumPath<String>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| {
        require_gardener_proposal(store, &fingerprint)?;
        store.proposal_observations(&fingerprint)
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn list_gardener_proposal_instances(
    State(state): State<ApiState>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| {
        store.gardener_proposal_instances_all()
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn show_gardener_proposal_instance(
    State(state): State<ApiState>,
    AxumPath(instance_id): AxumPath<String>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| {
        require_gardener_proposal_instance(store, &instance_id)
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn gardener_proposal_instance_observations(
    State(state): State<ApiState>,
    AxumPath(instance_id): AxumPath<String>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| {
        require_gardener_proposal_instance(store, &instance_id)?;
        store.proposal_instance_observations(&instance_id)
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn approve_gardener_proposal(
    State(state): State<ApiState>,
    AxumPath(fingerprint): AxumPath<String>,
    request: Result<Json<DecisionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    decide_gardener_proposal(state, fingerprint, request, ApprovalDecision::Approved).await
}

async fn reject_gardener_proposal(
    State(state): State<ApiState>,
    AxumPath(fingerprint): AxumPath<String>,
    request: Result<Json<DecisionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    decide_gardener_proposal(state, fingerprint, request, ApprovalDecision::Rejected).await
}

async fn decide_gardener_proposal(
    state: ApiState,
    fingerprint: String,
    request: DecisionRequest,
    decision: ApprovalDecision,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, now| {
        store.decide_gardener_proposal(
            &fingerprint,
            decision,
            &request.actor,
            request.note.as_deref(),
            now,
        )
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn operator_approve_gardener_proposal(
    State(state): State<ApiState>,
    AxumPath(fingerprint): AxumPath<String>,
    request: Result<Json<OperatorActionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    operator_decide_gardener_proposal(state, fingerprint, request, ApprovalDecision::Approved).await
}

async fn operator_reject_gardener_proposal(
    State(state): State<ApiState>,
    AxumPath(fingerprint): AxumPath<String>,
    request: Result<Json<OperatorActionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    operator_decide_gardener_proposal(state, fingerprint, request, ApprovalDecision::Rejected).await
}

async fn operator_decide_gardener_proposal(
    state: ApiState,
    fingerprint: String,
    request: OperatorActionRequest,
    decision: ApprovalDecision,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, now| {
        store.decide_gardener_proposal_if_current(
            &fingerprint,
            decision,
            &request.actor,
            request.note.as_deref(),
            &request.precondition,
            now,
        )
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn approve_gardener_proposal_instance(
    State(state): State<ApiState>,
    AxumPath(instance_id): AxumPath<String>,
    request: Result<Json<DecisionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    decide_gardener_proposal_instance(state, instance_id, request, ApprovalDecision::Approved).await
}

async fn reject_gardener_proposal_instance(
    State(state): State<ApiState>,
    AxumPath(instance_id): AxumPath<String>,
    request: Result<Json<DecisionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    decide_gardener_proposal_instance(state, instance_id, request, ApprovalDecision::Rejected).await
}

async fn decide_gardener_proposal_instance(
    state: ApiState,
    instance_id: String,
    request: DecisionRequest,
    decision: ApprovalDecision,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, now| {
        store.decide_gardener_proposal_instance(
            &instance_id,
            decision,
            &request.actor,
            request.note.as_deref(),
            now,
        )
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn operator_approve_gardener_proposal_instance(
    State(state): State<ApiState>,
    AxumPath(instance_id): AxumPath<String>,
    request: Result<Json<OperatorActionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    operator_decide_gardener_proposal_instance(
        state,
        instance_id,
        request,
        ApprovalDecision::Approved,
    )
    .await
}

async fn operator_reject_gardener_proposal_instance(
    State(state): State<ApiState>,
    AxumPath(instance_id): AxumPath<String>,
    request: Result<Json<OperatorActionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    operator_decide_gardener_proposal_instance(
        state,
        instance_id,
        request,
        ApprovalDecision::Rejected,
    )
    .await
}

async fn operator_decide_gardener_proposal_instance(
    state: ApiState,
    instance_id: String,
    request: OperatorActionRequest,
    decision: ApprovalDecision,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, now| {
        store.decide_gardener_proposal_instance_if_current(
            &instance_id,
            decision,
            &request.actor,
            request.note.as_deref(),
            &request.precondition,
            now,
        )
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn list_gardener_runs(State(state): State<ApiState>) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| store.gardener_implementation_runs())
        .await
        .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn show_gardener_run(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| require_gardener_run(store, &id))
        .await
        .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn gardener_run_events(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| {
        require_gardener_run(store, &id)?;
        store.gardener_run_events(&id)
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn with_store<T>(
    state: &ApiState,
    operation: impl FnOnce(&mut Store, i64) -> Result<T, StoreError> + Send + 'static,
) -> Result<T, ApiError>
where
    T: Send + 'static,
{
    state
        .executor
        .execute(move |store| operation(store, SystemClock.now()))
        .await
        .map_err(ApiError::from)
}

fn require_obligation(store: &Store, id: &str) -> Result<Obligation, StoreError> {
    store
        .get(id)?
        .ok_or_else(|| StoreError::NotFound(id.to_owned()))
}

fn require_gardener_repository(store: &Store) -> Result<RepositoryRegistration, StoreError> {
    store
        .gardener_repository()?
        .ok_or_else(|| StoreError::NotFound(CANONICAL_REPOSITORY.to_owned()))
}

fn require_gardener_inspection(store: &Store, id: &str) -> Result<GardenerInspection, StoreError> {
    store
        .gardener_inspection(id)?
        .ok_or_else(|| StoreError::NotFound(id.to_owned()))
}

fn require_gardener_proposal(store: &Store, fingerprint: &str) -> Result<Proposal, StoreError> {
    store
        .gardener_proposal(fingerprint)?
        .ok_or_else(|| StoreError::NotFound(fingerprint.to_owned()))
}

fn require_gardener_proposal_instance(
    store: &Store,
    instance_id: &str,
) -> Result<ProposalInstance, StoreError> {
    store
        .gardener_proposal_instance(instance_id)?
        .ok_or_else(|| StoreError::NotFound(instance_id.to_owned()))
}

fn require_gardener_run(store: &Store, id: &str) -> Result<GardenerImplementationRun, StoreError> {
    store
        .gardener_implementation_run(id)?
        .ok_or_else(|| StoreError::NotFound(id.to_owned()))
}

fn canonical_repository() -> String {
    CANONICAL_REPOSITORY.to_owned()
}

fn canonical_default_branch() -> String {
    CANONICAL_DEFAULT_BRANCH.to_owned()
}

fn default_inspection_cron() -> String {
    "0 0 * * *".to_owned()
}

fn default_inspection_timezone() -> String {
    "UTC".to_owned()
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
    use crate::http_security::MUTATION_TOKEN_HEADER;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tempfile::TempDir;
    use tower::ServiceExt;

    const TEST_AUTHORITY: &str = "127.0.0.1:7744";
    const TEST_TOKEN: &str = "4242424242424242424242424242424242424242424242424242424242424242";

    fn test_runtime() -> ApiRuntime {
        ApiRuntime::deterministic(TEST_AUTHORITY.parse().unwrap(), 0x42, "test-session")
    }

    fn test_router(database: PathBuf) -> Router {
        router_with_executor(DbExecutor::start(database).unwrap(), test_runtime())
    }

    async fn response_json(response: Response) -> (StatusCode, Value) {
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[test]
    fn remote_bind_is_rejected() {
        let error = validate_loopback("0.0.0.0:7744".parse().unwrap()).unwrap_err();
        assert!(error.contains("authentication"));
        assert!(validate_loopback("127.0.0.1:7744".parse().unwrap()).is_ok());
        assert!(validate_loopback("[::1]:7744".parse().unwrap()).is_ok());
    }

    #[tokio::test]
    async fn bootstrap_health_and_snapshot_expose_identity_without_secret() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("identity-http.sqlite");
        drop(Store::open(&database).unwrap());
        let application = test_router(database);

        let bootstrap = application
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/bootstrap")
                    .header("host", TEST_AUTHORITY)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bootstrap.headers()["cache-control"], "no-store");
        let (_, bootstrap) = response_json(bootstrap).await;
        assert_eq!(bootstrap["mutation_token"], TEST_TOKEN);
        assert_eq!(bootstrap["service"]["api_contract_version"], 1);
        assert_eq!(bootstrap["service"]["schema_version"], 8);
        assert_eq!(bootstrap["service"]["session_id"], "test-session");

        for path in ["/health", "/operator/snapshot"] {
            let (_, body) = response_json(
                application
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri(path)
                            .header("host", TEST_AUTHORITY)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap(),
            )
            .await;
            let serialised = body.to_string();
            assert!(serialised.contains("test-session"));
            assert!(!serialised.contains(TEST_TOKEN));
        }
    }

    #[tokio::test]
    async fn every_mutation_route_requires_the_explicit_session_header() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("all-mutations-http.sqlite");
        drop(Store::open(&database).unwrap());
        let application = test_router(database);
        let paths = [
            "/obligations",
            "/obligations/id/approve",
            "/obligations/id/reject",
            "/obligations/id/retry",
            "/obligations/id/cancel",
            "/operator/obligations/id/approve",
            "/operator/obligations/id/reject",
            "/operator/obligations/id/retry",
            "/operator/obligations/id/cancel",
            "/gardener/repository",
            "/gardener/proposals/fingerprint/approve",
            "/gardener/proposals/fingerprint/reject",
            "/operator/gardener/proposals/fingerprint/approve",
            "/operator/gardener/proposals/fingerprint/reject",
            "/gardener/proposal-instances/instance/approve",
            "/gardener/proposal-instances/instance/reject",
            "/operator/gardener/proposal-instances/instance/approve",
            "/operator/gardener/proposal-instances/instance/reject",
        ];
        for path in paths {
            let response = application
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header("host", TEST_AUTHORITY)
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            let (status, body) = response_json(response).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
            assert_eq!(body["error"]["code"], "mutation_token_required", "{path}");
        }
    }

    #[tokio::test]
    async fn wrong_and_rotated_tokens_fail_without_leaking_supplied_values() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("rotated-token-http.sqlite");
        drop(Store::open(&database).unwrap());
        let executor = DbExecutor::start(database).unwrap();
        let first = router_with_executor(executor.clone(), test_runtime());
        let rotated = router_with_executor(
            executor,
            ApiRuntime::deterministic(TEST_AUTHORITY.parse().unwrap(), 0x43, "test-session-2"),
        );

        for (application, supplied) in [(first, "definitely-wrong"), (rotated, TEST_TOKEN)] {
            let response = application
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/obligations")
                        .header("host", TEST_AUTHORITY)
                        .header("content-type", "application/json")
                        .header(MUTATION_TOKEN_HEADER, supplied)
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            let (status, body) = response_json(response).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(body["error"]["code"], "mutation_token_invalid");
            assert!(!body.to_string().contains(supplied));
        }
    }

    #[tokio::test]
    async fn host_origin_and_browser_context_are_fail_closed() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("origin-http.sqlite");
        drop(Store::open(&database).unwrap());
        let application = test_router(database);

        for (host, origin, fetch_site, expected) in [
            (TEST_AUTHORITY, None, None, StatusCode::OK),
            (
                TEST_AUTHORITY,
                Some("http://127.0.0.1:7744"),
                Some("same-origin"),
                StatusCode::OK,
            ),
            (TEST_AUTHORITY, None, Some("none"), StatusCode::OK),
            (
                "attacker.invalid:7744",
                None,
                None,
                StatusCode::MISDIRECTED_REQUEST,
            ),
            (
                "127.0.0.1:9999",
                None,
                None,
                StatusCode::MISDIRECTED_REQUEST,
            ),
            (
                TEST_AUTHORITY,
                Some("http://evil.invalid"),
                None,
                StatusCode::FORBIDDEN,
            ),
            (TEST_AUTHORITY, Some("null"), None, StatusCode::FORBIDDEN),
            (TEST_AUTHORITY, Some("file://"), None, StatusCode::FORBIDDEN),
            (
                TEST_AUTHORITY,
                None,
                Some("cross-site"),
                StatusCode::FORBIDDEN,
            ),
        ] {
            let mut builder = Request::builder().uri("/health").header("host", host);
            if let Some(origin) = origin {
                builder = builder.header("origin", origin);
            }
            if let Some(fetch_site) = fetch_site {
                builder = builder.header("sec-fetch-site", fetch_site);
            }
            let response = application
                .clone()
                .oneshot(builder.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "host={host} origin={origin:?}");
            assert!(
                response
                    .headers()
                    .get("access-control-allow-origin")
                    .is_none()
            );
        }

        let missing_host = application
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_host.status(), StatusCode::MISDIRECTED_REQUEST);

        let duplicate_origin = application
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header("host", TEST_AUTHORITY)
                    .header("origin", "http://127.0.0.1:7744")
                    .header("origin", "http://127.0.0.1:7744")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(duplicate_origin.status(), StatusCode::FORBIDDEN);

        let ipv6_database = temporary.path().join("origin-ipv6-http.sqlite");
        drop(Store::open(&ipv6_database).unwrap());
        let ipv6 = router_with_executor(
            DbExecutor::start(ipv6_database).unwrap(),
            ApiRuntime::deterministic("[::1]:7744".parse().unwrap(), 0x42, "ipv6-session"),
        );
        let response = ipv6
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header("host", "[::1]:7744")
                    .header("origin", "http://[::1]:7744")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn static_ui_uses_the_same_host_and_origin_boundary() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("ui-security-http.sqlite");
        let ui = temporary.path().join("ui");
        std::fs::create_dir(&ui).unwrap();
        std::fs::write(
            ui.join("index.html"),
            "<!doctype html><title>Bokkie</title>",
        )
        .unwrap();
        drop(Store::open(&database).unwrap());
        let application =
            router_with_ui_executor(DbExecutor::start(database).unwrap(), ui, test_runtime());

        let accepted = application
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/ui/")
                    .header("host", TEST_AUTHORITY)
                    .header("sec-fetch-site", "none")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::OK);
        assert_eq!(accepted.headers()["referrer-policy"], "no-referrer");

        let rejected = application
            .oneshot(
                Request::builder()
                    .uri("/ui/")
                    .header("host", "rebound.invalid:7744")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::MISDIRECTED_REQUEST);
    }

    #[tokio::test]
    async fn legacy_bodyless_mutations_require_json_and_reject_browser_simple_requests() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("legacy-mutation-http.sqlite");
        let mut store = Store::open(&database).unwrap();
        store
            .create(
                NewObligation {
                    id: "legacy-cancel".to_owned(),
                    description: "cancel through the non-browser compatibility route".to_owned(),
                    scheduled_at: 2_000_000_000,
                    recurrence: None,
                    approval_required: false,
                    retry: RetryPolicy::default(),
                },
                100,
            )
            .unwrap();
        drop(store);
        let application = test_router(database);

        for content_type in ["application/x-www-form-urlencoded", "text/plain"] {
            let response = application
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/obligations/legacy-cancel/cancel")
                        .header("host", TEST_AUTHORITY)
                        .header("origin", "http://evil.invalid")
                        .header("content-type", content_type)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }

        let response = application
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/obligations/legacy-cancel/cancel")
                    .header("host", TEST_AUTHORITY)
                    .header("content-type", "text/plain")
                    .header(MUTATION_TOKEN_HEADER, TEST_TOKEN)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        let response = application
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/obligations/legacy-cancel/cancel")
                    .header("host", TEST_AUTHORITY)
                    .header("sec-fetch-site", "none")
                    .header("content-type", "application/json")
                    .header(MUTATION_TOKEN_HEADER, TEST_TOKEN)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response = application
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/obligations/legacy-cancel/cancel")
                    .header("host", TEST_AUTHORITY)
                    .header("content-type", "application/json; charset=utf-8")
                    .header(MUTATION_TOKEN_HEADER, TEST_TOKEN)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = application
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/obligations/legacy-cancel/cancel")
                    .header("host", TEST_AUTHORITY)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers()["allow"], "GET, HEAD, POST");

        let response = application
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/obligations/legacy-cancel/cancel")
                    .header("host", TEST_AUTHORITY)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn operator_endpoints_return_shared_projection_and_missing_topic_is_not_found() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("operator-http.sqlite");
        let mut store = Store::open(&database).unwrap();
        store
            .create(
                NewObligation {
                    id: "approval".to_owned(),
                    description: "Approve carefully".to_owned(),
                    scheduled_at: 2_000_000_000,
                    recurrence: None,
                    approval_required: true,
                    retry: RetryPolicy::default(),
                },
                100,
            )
            .unwrap();
        drop(store);

        let response = test_router(database.clone())
            .oneshot(
                Request::builder()
                    .uri("/operator/snapshot")
                    .header("host", TEST_AUTHORITY)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let snapshot: bokkie_operator_api::OperatorSnapshot =
            serde_json::from_slice(&body).unwrap();
        assert_eq!(snapshot.obligations[0].id, "approval");
        assert!(snapshot.obligations[0].capabilities.approve.available);

        let response = test_router(database.clone())
            .oneshot(
                Request::builder()
                    .uri("/operator/obligations/approval/topic")
                    .header("host", TEST_AUTHORITY)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let topic: bokkie_operator_api::ObligationTopic = serde_json::from_slice(&body).unwrap();
        assert_eq!(topic.obligation_id, "approval");
        assert_eq!(
            topic.items[0].source,
            bokkie_operator_api::TopicSource::AuditEvent
        );

        let response = test_router(database.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/operator/obligations/approval/approve")
                    .header("host", TEST_AUTHORITY)
                    .header("content-type", "application/json")
                    .header(MUTATION_TOKEN_HEADER, TEST_TOKEN)
                    .body(Body::from(r#"{"actor":"operator"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let response = test_router(database)
            .oneshot(
                Request::builder()
                    .uri("/operator/obligations/missing/topic")
                    .header("host", TEST_AUTHORITY)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn stale_confirmation_returns_transition_conflict_after_same_occurrence_cycle() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("stale-action-http.sqlite");
        let mut store = Store::open(&database).unwrap();
        store
            .create(
                NewObligation {
                    id: "cycled".to_owned(),
                    description: "Cycle back to approval".to_owned(),
                    scheduled_at: 2_000_000_000,
                    recurrence: None,
                    approval_required: true,
                    retry: RetryPolicy::default(),
                },
                100,
            )
            .unwrap();
        let stale = store
            .operator_snapshot(100)
            .unwrap()
            .obligations
            .pop()
            .unwrap()
            .capabilities
            .approve
            .precondition
            .unwrap();
        store
            .decide_approval(
                "cycled",
                ApprovalDecision::Rejected,
                "other operator",
                None,
                101,
            )
            .unwrap();
        store.retry_attention("cycled", 102).unwrap();
        drop(store);

        let request = Request::builder()
            .method("POST")
            .uri("/operator/obligations/cycled/approve")
            .header("host", TEST_AUTHORITY)
            .header("content-type", "application/json")
            .header(MUTATION_TOKEN_HEADER, TEST_TOKEN)
            .body(Body::from(
                serde_json::to_vec(&OperatorActionRequest {
                    precondition: stale,
                    actor: "operator".to_owned(),
                    note: Some("confirmed old state".to_owned()),
                })
                .unwrap(),
            ))
            .unwrap();
        let response = test_router(database).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let error: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(error["error"]["code"], "transition_conflict");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("revision")
        );
    }

    #[tokio::test]
    async fn conditional_operator_route_rejects_a_later_occurrence() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("later-occurrence-http.sqlite");
        let mut store = Store::open(&database).unwrap();
        store
            .create(
                NewObligation {
                    id: "recurring".to_owned(),
                    description: "Review every occurrence".to_owned(),
                    scheduled_at: 100,
                    recurrence: Some(Recurrence::new("* * * * *", "UTC").unwrap()),
                    approval_required: true,
                    retry: RetryPolicy::default(),
                },
                90,
            )
            .unwrap();
        let stale = store
            .operator_snapshot(90)
            .unwrap()
            .obligations
            .pop()
            .unwrap()
            .capabilities
            .approve
            .precondition
            .unwrap();
        store
            .decide_approval(
                "recurring",
                ApprovalDecision::Approved,
                "other operator",
                None,
                100,
            )
            .unwrap();
        let claim = store.claim_due(100, 60, 1).unwrap().pop().unwrap();
        store
            .complete(&claim, crate::Completion::Succeeded { evidence: None }, 101)
            .unwrap();
        drop(store);

        let request = Request::builder()
            .method("POST")
            .uri("/operator/obligations/recurring/approve")
            .header("host", TEST_AUTHORITY)
            .header("content-type", "application/json")
            .header(MUTATION_TOKEN_HEADER, TEST_TOKEN)
            .body(Body::from(
                serde_json::to_vec(&OperatorActionRequest {
                    precondition: stale,
                    actor: "operator".to_owned(),
                    note: None,
                })
                .unwrap(),
            ))
            .unwrap();
        let response = test_router(database).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let error: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(error["error"]["code"], "transition_conflict");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("occurrence")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_http_commands_share_one_database_owner() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("concurrent-http.sqlite");
        drop(Store::open(&database).unwrap());
        let executor = DbExecutor::start(database).unwrap();
        let application = router_with_executor(executor.clone(), test_runtime());
        let mut tasks = tokio::task::JoinSet::new();
        for index in 0..24 {
            let application = application.clone();
            tasks.spawn(async move {
                application
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri("/obligations")
                            .header("host", TEST_AUTHORITY)
                            .header("content-type", "application/json")
                            .header(MUTATION_TOKEN_HEADER, TEST_TOKEN)
                            .body(Body::from(
                                serde_json::to_vec(&CreateRequest {
                                    id: Some(format!("concurrent-{index:02}")),
                                    description: format!("concurrent command {index}"),
                                    scheduled_at: Some(2_000_000_000),
                                    recurrence_cron: None,
                                    recurrence_timezone: None,
                                    approval_required: false,
                                    max_attempts: None,
                                    retry_base_seconds: None,
                                    retry_max_seconds: None,
                                })
                                .unwrap(),
                            ))
                            .unwrap(),
                    )
                    .await
                    .unwrap()
                    .status()
            });
        }
        while let Some(result) = tasks.join_next().await {
            assert_eq!(result.unwrap(), StatusCode::CREATED);
        }
        let response = application
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/obligations")
                    .header("host", TEST_AUTHORITY)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let obligations: Vec<Obligation> = serde_json::from_slice(&body).unwrap();
        assert_eq!(obligations.len(), 24);
        drop(application);
        executor.shutdown().unwrap();
    }

    #[tokio::test]
    async fn stopped_database_owner_returns_a_typed_service_error() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("stopped-http.sqlite");
        drop(Store::open(&database).unwrap());
        let executor = DbExecutor::start(database).unwrap();
        executor.shutdown().unwrap();
        let response = router_with_executor(executor, test_runtime())
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header("host", TEST_AUTHORITY)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let error: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(error["error"]["code"], "storage_executor_unavailable");
    }
}
