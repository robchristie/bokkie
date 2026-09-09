//! Loopback HTTP adapter. Lifecycle decisions remain owned by [`crate::Store`].

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    extract::{Path as AxumPath, Query, State, rejection::JsonRejection},
    http::{HeaderName, HeaderValue, StatusCode},
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
    ApprovalDecision, CANONICAL_DEFAULT_BRANCH, CANONICAL_REPOSITORY, DEFAULT_PAGE_SIZE,
    DbExecutor, DbExecutorError, GardenerImplementationRun, GardenerInspection,
    MAX_CHANGE_PAGE_SIZE, NewObligation, NewRepositoryRegistration, Obligation, Proposal,
    Recurrence, RepositoryRegistration, RetryPolicy, Store, StoreError, SystemClock, UnixClock,
    gardener::ProposalInstance,
    http_security::{ApiRuntime, bootstrap_response, enforce},
};
use bokkie_operator_api::ActionPrecondition;

#[derive(Debug, Clone)]
pub struct EngineeringIntakeConfig {
    /// Trusted, explicitly enabled service profile. Never accepted from HTTP JSON.
    pub contract_template: crate::engineering::EngineeringContract,
}

#[derive(Debug, Clone)]
pub struct ApiState {
    pub executor: DbExecutor,
    pub runtime: ApiRuntime,
    pub engineering_intake: Option<Arc<EngineeringIntakeConfig>>,
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

#[derive(Debug, Default, Deserialize)]
struct PageQuery {
    cursor: Option<String>,
    watermark: Option<i64>,
    limit: Option<usize>,
}

const NEXT_CURSOR_HEADER: HeaderName = HeaderName::from_static("x-bokkie-next-cursor");
const WATERMARK_HEADER: HeaderName = HeaderName::from_static("x-bokkie-watermark");

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
            StoreError::ProjectionGap(_) => (StatusCode::CONFLICT, "projection_gap"),
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
    router_state_core(ApiState {
        executor,
        runtime,
        engineering_intake: None,
    })
}

/// Service-owned engineering configuration shares the existing loopback security boundary.
pub fn router_with_state(state: ApiState, ui_dir: Option<PathBuf>) -> Router {
    let security = state.runtime.clone();
    let mut router = router_state_core(state);
    if let Some(ui_dir) = ui_dir {
        router = router.nest_service(
            "/ui",
            ServeDir::new(ui_dir).append_index_html_on_directories(true),
        );
    }
    router.layer(middleware::from_fn_with_state(security, enforce))
}

fn router_state_core(state: ApiState) -> Router {
    Router::new()
        .route(
            "/engineering/outcomes",
            post(engineering_intake).get(engineering_list),
        )
        .route("/engineering/outcomes/{id}", get(engineering_detail))
        .route(
            "/engineering/outcomes/{id}/cancel",
            post(engineering_cancel),
        )
        .route(
            "/engineering/outcomes/{id}/messages",
            post(engineering_follow_up),
        )
        .route("/bootstrap", get(bootstrap))
        .route("/health", get(health))
        .route(
            "/operator/tasks/{id}/configuration",
            post(operator_task_configuration),
        )
        .route("/operator/snapshot", get(operator_snapshot))
        .route("/operator/changes", get(operator_changes))
        .route("/operator/obligations/{id}", get(operator_obligation))
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
        store.check_readable()?;
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

async fn list(
    State(state): State<ApiState>,
    Query(query): Query<PageQuery>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| {
        store.obligation_page(query.cursor.as_deref(), query.watermark, query.limit)
    })
    .await
    .and_then(page_response)
}

async fn operator_snapshot(
    State(state): State<ApiState>,
    Query(query): Query<PageQuery>,
) -> Result<Response, ApiError> {
    let identity = state.runtime.identity();
    with_store(&state, move |store, now| {
        let mut snapshot = store.operator_snapshot_page(
            now,
            query.cursor.as_deref(),
            query.watermark,
            query.limit,
        )?;
        snapshot.service = Some(identity);
        Ok(snapshot)
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn operator_obligation(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    let identity = state.runtime.identity();
    with_store(&state, move |store, now| {
        let (watermark, obligation) = store.operator_obligation_with_watermark(&id, now)?;
        Ok(bokkie_operator_api::OperatorObligationProjection {
            service: identity,
            watermark,
            obligation,
        })
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

#[derive(Debug, Deserialize)]
struct ChangeQuery {
    #[serde(default)]
    after: i64,
    through: Option<i64>,
    limit: Option<usize>,
}

async fn operator_changes(
    State(state): State<ApiState>,
    Query(query): Query<ChangeQuery>,
) -> Result<Response, ApiError> {
    let identity = state.runtime.identity();
    let limit = change_page_limit(query.limit)?;
    with_store(&state, move |store, _| {
        let page = store.change_page(query.after, query.through, limit)?;
        let changes = page
            .items
            .into_iter()
            .map(|item| {
                let source = match item.envelope.source {
                    crate::EventSource::AuditEvent { sequence } => {
                        bokkie_operator_api::ProjectionEventSource::AuditEvent { sequence }
                    }
                    crate::EventSource::GardenerEvent { sequence } => {
                        bokkie_operator_api::ProjectionEventSource::GardenerEvent { sequence }
                    }
                    crate::EventSource::GardenerRunEvent { sequence } => {
                        bokkie_operator_api::ProjectionEventSource::GardenerRunEvent { sequence }
                    }
                };
                bokkie_operator_api::ProjectionChange {
                    revision: item.envelope.sequence,
                    provenance: match item.envelope.provenance {
                        crate::EventProvenance::LegacyNonCausal => {
                            bokkie_operator_api::ProjectionEventProvenance::LegacyNonCausal
                        }
                        crate::EventProvenance::LiveAppend => {
                            bokkie_operator_api::ProjectionEventProvenance::LiveAppend
                        }
                    },
                    source,
                    event_type: item.event_type,
                    occurred_at: item.occurred_at,
                    obligation_id: item.obligation_id,
                    occurrence: item.occurrence,
                    repository: item.repository,
                    inspection_id: item.inspection_id,
                    proposal_fingerprint: item.proposal_fingerprint,
                    proposal_instance_id: item.proposal_instance_id,
                    run_id: item.run_id,
                }
            })
            .collect();
        Ok(bokkie_operator_api::ProjectionChangePage {
            service: identity,
            requested_after: page.after,
            requested_through: query.through,
            next_after: page.next_after,
            watermark: page.through,
            changes,
        })
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

fn change_page_limit(requested: Option<usize>) -> Result<usize, ApiError> {
    let limit = requested.unwrap_or(DEFAULT_PAGE_SIZE);
    if (1..=MAX_CHANGE_PAGE_SIZE).contains(&limit) {
        Ok(limit)
    } else {
        Err(StoreError::Invalid(format!(
            "change page limit {limit} is outside 1..={MAX_CHANGE_PAGE_SIZE}"
        ))
        .into())
    }
}

async fn operator_topic(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<PageQuery>,
) -> Result<Response, ApiError> {
    let identity = state.runtime.identity();
    with_store(&state, move |store, now| {
        let mut topic = store.operator_topic_page(
            &id,
            now,
            query.cursor.as_deref(),
            query.watermark,
            query.limit,
        )?;
        topic.service = Some(identity);
        Ok(topic)
    })
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

async fn operator_task_configuration(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
    request: Result<Json<bokkie_operator_api::TaskConfigurationUpdate>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    with_store(&state, move |store, now| {
        store.update_gardener_task_configuration(&id, &request, now)
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
    Query(query): Query<PageQuery>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| {
        require_obligation(store, &id)?;
        store.audit_event_page(&id, query.cursor.as_deref(), query.watermark, query.limit)
    })
    .await
    .and_then(page_response)
}

async fn attempts(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<PageQuery>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| {
        require_obligation(store, &id)?;
        store.attempt_page(&id, query.cursor.as_deref(), query.watermark, query.limit)
    })
    .await
    .and_then(page_response)
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

async fn list_gardener_inspections(
    State(state): State<ApiState>,
    Query(query): Query<PageQuery>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| {
        store.gardener_inspection_page(query.cursor.as_deref(), query.watermark, query.limit)
    })
    .await
    .and_then(page_response)
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

async fn list_gardener_proposals(
    State(state): State<ApiState>,
    Query(query): Query<PageQuery>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| {
        store.gardener_proposal_page(query.cursor.as_deref(), query.watermark, query.limit)
    })
    .await
    .and_then(page_response)
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
    Query(query): Query<PageQuery>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| {
        require_gardener_proposal(store, &fingerprint)?;
        store.proposal_observation_page(
            &fingerprint,
            query.cursor.as_deref(),
            query.watermark,
            query.limit,
        )
    })
    .await
    .and_then(page_response)
}

async fn list_gardener_proposal_instances(
    State(state): State<ApiState>,
    Query(query): Query<PageQuery>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| {
        store.gardener_proposal_instance_page(
            None,
            query.cursor.as_deref(),
            query.watermark,
            query.limit,
        )
    })
    .await
    .and_then(page_response)
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
    Query(query): Query<PageQuery>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| {
        require_gardener_proposal_instance(store, &instance_id)?;
        store.proposal_instance_observation_page(
            &instance_id,
            query.cursor.as_deref(),
            query.watermark,
            query.limit,
        )
    })
    .await
    .and_then(page_response)
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

async fn list_gardener_runs(
    State(state): State<ApiState>,
    Query(query): Query<PageQuery>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| {
        store.gardener_implementation_run_page(
            query.cursor.as_deref(),
            query.watermark,
            query.limit,
        )
    })
    .await
    .and_then(page_response)
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
    Query(query): Query<PageQuery>,
) -> Result<Response, ApiError> {
    with_store(&state, move |store, _| {
        require_gardener_run(store, &id)?;
        store.gardener_run_event_page(&id, query.cursor.as_deref(), query.watermark, query.limit)
    })
    .await
    .and_then(page_response)
}

fn page_response<T: Serialize>(page: crate::ReadPage<T>) -> Result<Response, ApiError> {
    let mut response = (StatusCode::OK, Json(page.items)).into_response();
    response.headers_mut().insert(
        WATERMARK_HEADER,
        HeaderValue::from_str(&page.watermark.to_string()).expect("numeric watermark header"),
    );
    if let Some(cursor) = page.next_cursor {
        response.headers_mut().insert(
            NEXT_CURSOR_HEADER,
            HeaderValue::from_str(&cursor).map_err(|_| ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "cursor_encoding_error",
                message: "generated cursor is not a valid HTTP header".to_owned(),
            })?,
        );
    }
    Ok(response)
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

async fn engineering_intake(
    State(state): State<ApiState>,
    request: Result<Json<bokkie_operator_api::EngineeringIntakeRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    let mut contract = state
        .engineering_intake
        .as_ref()
        .ok_or_else(|| ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "engineering_not_configured",
            message: "Engineering supervision requires an explicitly configured runtime profile"
                .to_owned(),
        })?
        .contract_template
        .clone();
    contract.intent = request.intent;
    engineering_mutation(
        &state,
        crate::engineering::EngineeringCommandEnvelope {
            command_id: request.command_id,
            expected: None,
            command: crate::engineering::EngineeringCommand::CreateOutcome { contract },
        },
    )
    .await
}

async fn engineering_follow_up(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
    request: Result<Json<bokkie_operator_api::EngineeringFollowUpRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    if request.expected.outcome_id != id {
        return Err(
            StoreError::Invalid("follow-up identity does not match the route".into()).into(),
        );
    }
    let command = match request.question_id {
        Some(question_id) => crate::engineering::EngineeringCommand::ResolveQuestion {
            question_id,
            answer: request.text,
            authority_grants: Vec::new(),
        },
        None => crate::engineering::EngineeringCommand::FollowUp { text: request.text },
    };
    engineering_mutation(
        &state,
        crate::engineering::EngineeringCommandEnvelope {
            command_id: request.command_id,
            expected: Some(crate::engineering::EngineeringPrecondition {
                outcome_id: id,
                contract_revision: request.expected.contract_revision,
                state_revision: request.expected.state_revision,
            }),
            command,
        },
    )
    .await
}

async fn engineering_cancel(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
    request: Result<Json<bokkie_operator_api::EngineeringCancellationRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(invalid_json)?;
    if request.expected.outcome_id != id {
        return Err(
            StoreError::Invalid("cancellation identity does not match the route".into()).into(),
        );
    }
    engineering_mutation(
        &state,
        crate::engineering::EngineeringCommandEnvelope {
            command_id: request.command_id,
            expected: Some(crate::engineering::EngineeringPrecondition {
                outcome_id: id,
                contract_revision: request.expected.contract_revision,
                state_revision: request.expected.state_revision,
            }),
            command: crate::engineering::EngineeringCommand::RequestCancellation {
                package_id: None,
            },
        },
    )
    .await
}

async fn engineering_mutation(
    state: &ApiState,
    envelope: crate::engineering::EngineeringCommandEnvelope,
) -> Result<Response, ApiError> {
    let service = state.runtime.identity();
    with_store(state, move |store, now| {
        let replay = if let crate::engineering::EngineeringCommand::CreateOutcome { contract } =
            &envelope.command
        {
            store.engineering_operator_intake_receipt(
                &envelope.command_id,
                &contract.intent,
                "local operator",
            )?
        } else {
            None
        };
        let receipt = match replay {
            Some(receipt) => receipt,
            None => store.engineering_command(
                crate::engineering::EngineeringActor::Operator {
                    name: "local operator".to_owned(),
                },
                envelope,
                now,
            )?,
        };
        let outcome = store
            .engineering_outcome(&receipt.outcome_id)?
            .ok_or_else(|| StoreError::NotFound(receipt.outcome_id.clone()))?;
        Ok(bokkie_operator_api::EngineeringSaved {
            service,
            command_id: receipt.command_id,
            outcome_id: receipt.outcome_id,
            root_obligation_id: outcome.root.id,
        })
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

async fn engineering_detail(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    let service = state.runtime.identity();
    with_store(&state, move |store, now| {
        let outcome = store
            .engineering_outcome(&id)?
            .ok_or_else(|| StoreError::NotFound(id.clone()))?;
        let (watermark, obligation) =
            store.operator_obligation_with_watermark(&outcome.root.id, now)?;
        Ok(bokkie_operator_api::OperatorObligationProjection {
            service,
            watermark,
            obligation,
        })
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
}

/// Uses the existing keyset/watermark walk; callers must follow cursors even for filtered empty pages.
async fn engineering_list(
    State(state): State<ApiState>,
    Query(query): Query<PageQuery>,
) -> Result<Response, ApiError> {
    let service = state.runtime.identity();
    with_store(&state, move |store, now| {
        let mut page = store.operator_snapshot_page(
            now,
            query.cursor.as_deref(),
            query.watermark,
            query.limit,
        )?;
        page.service = Some(service);
        page.obligations.retain(|obligation| {
            obligation.task.as_ref().is_some_and(|task| {
                task.kind == bokkie_operator_api::OperatorTaskKind::EngineeringSupervisor
            })
        });
        Ok(page)
    })
    .await
    .map(|body| (StatusCode::OK, Json(body)).into_response())
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

    fn engineering_test_contract() -> crate::engineering::EngineeringContract {
        use crate::engineering::*;
        use sha2::{Digest, Sha256};
        let instructions = EngineeringInstructions {
            text: "test instructions".into(),
            digest: format!("{:x}", Sha256::digest(b"test instructions")),
            context_digests: vec![],
            profile_digest: "a".repeat(64),
            adapter_id: "test-adapter".into(),
        };
        EngineeringContract {
            intent: "template".into(),
            criteria: vec![],
            permitted_scope: vec!["isolated test workspace".into()],
            prohibited_effects: vec!["network publication".into()],
            authority: vec![],
            supervisor: instructions.clone(),
            worker: instructions,
            budget: EngineeringBudget {
                max_turns: 5,
                max_packages: 3,
                max_repairs: 2,
                max_recoveries: 2,
                max_checkpoints: 20,
                max_questions: 10,
                max_concurrent_workers: 1,
                turn_seconds: 30,
                deadline: SystemClock.now() + 3600,
            },
        }
    }

    async fn engineering_post(
        application: Router,
        uri: &str,
        body: Value,
        token: bool,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method("POST")
            .uri(uri)
            .header("host", TEST_AUTHORITY)
            .header("content-type", "application/json");
        if token {
            request = request.header("X-Bokkie-Mutation-Token", TEST_TOKEN);
        }
        response_json(
            application
                .oneshot(request.body(Body::from(body.to_string())).unwrap())
                .await
                .unwrap(),
        )
        .await
    }

    #[tokio::test]
    async fn engineering_plain_intent_is_durable_replay_safe_and_fenced() {
        let temp = TempDir::new().unwrap();
        let database = temp.path().join("engineering.sqlite");
        drop(Store::open(&database).unwrap());
        let executor = DbExecutor::start(database.clone()).unwrap();
        let application = router_with_state(
            ApiState {
                executor,
                runtime: test_runtime(),
                engineering_intake: Some(Arc::new(EngineeringIntakeConfig {
                    contract_template: engineering_test_contract(),
                })),
            },
            None,
        );
        let body = json!({"command_id":"ui-create", "intent":"Build a local reading workspace"});
        assert_eq!(
            engineering_post(
                application.clone(),
                "/engineering/outcomes",
                body.clone(),
                false
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        let (status, saved) = engineering_post(
            application.clone(),
            "/engineering/outcomes",
            body.clone(),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        let replay =
            engineering_post(application.clone(), "/engineering/outcomes", body, true).await;
        assert_eq!(replay.1, saved);
        let mut changed_template = engineering_test_contract();
        changed_template.budget.deadline += 300;
        changed_template.permitted_scope = vec!["a different configured workspace".into()];
        let restarted = router_with_state(
            ApiState {
                executor: DbExecutor::start(database.clone()).unwrap(),
                runtime: test_runtime(),
                engineering_intake: Some(Arc::new(EngineeringIntakeConfig {
                    contract_template: changed_template,
                })),
            },
            None,
        );
        let after_restart = engineering_post(
            restarted.clone(),
            "/engineering/outcomes",
            json!({"command_id":"ui-create", "intent":"Build a local reading workspace"}),
            true,
        )
        .await;
        assert_eq!(after_restart.0, StatusCode::OK, "{}", after_restart.1);
        assert_eq!(after_restart.1, saved);
        assert_eq!(
            engineering_post(
                restarted,
                "/engineering/outcomes",
                json!({"command_id":"ui-create", "intent":"A different request"}),
                true
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        let store = Store::open(&database).unwrap();
        let outcome = store
            .engineering_outcome(saved["outcome_id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(outcome.contract().intent, "Build a local reading workspace");
        assert_eq!(
            outcome.contract().permitted_scope,
            ["isolated test workspace"]
        );
        assert!(outcome.contract().criteria.is_empty());
        let projected = store
            .operator_obligation(&outcome.root.id, SystemClock.now())
            .unwrap();
        assert_eq!(
            projected.task.as_ref().unwrap().kind,
            bokkie_operator_api::OperatorTaskKind::EngineeringSupervisor
        );
        assert!(!projected.capabilities.cancel.available);
        assert_eq!(
            projected
                .task
                .as_ref()
                .unwrap()
                .engineering
                .as_ref()
                .unwrap()
                .responsibility,
            "Bokkie supervisor"
        );
        let uri = format!("/engineering/outcomes/{}/messages", outcome.id);
        let message = json!({"command_id":"ui-followup", "expected":outcome.precondition(), "text":"Preserve original files", "question_id":null});
        let (status, response) =
            engineering_post(application.clone(), &uri, message.clone(), true).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert_eq!(
            engineering_post(application.clone(), &uri, message.clone(), true)
                .await
                .0,
            StatusCode::OK
        );
        let mut stale = message;
        stale["command_id"] = "new-stale-command".into();
        assert_eq!(
            engineering_post(application.clone(), &uri, stale, true)
                .await
                .0,
            StatusCode::CONFLICT
        );
        let current = store.engineering_outcome(&outcome.id).unwrap().unwrap();
        assert_eq!(
            current
                .messages
                .iter()
                .filter(|m| m.text == "Preserve original files")
                .count(),
            1
        );
        let detail = application
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/engineering/outcomes/{}", outcome.id))
                    .header("host", TEST_AUTHORITY)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response_json(detail).await.0, StatusCode::OK);
        let listing = application
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/engineering/outcomes?limit=1")
                    .header("host", TEST_AUTHORITY)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let (status, listing) = response_json(listing).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listing["obligations"].as_array().unwrap().len(), 1);
        assert_eq!(
            listing["obligations"][0]["task"]["kind"],
            "engineering_supervisor"
        );
        let mut malicious = json!({"command_id":"authority-injection", "intent":"test"});
        malicious["contract"] = json!({"authority":["publish"]});
        assert_eq!(
            engineering_post(application, "/engineering/outcomes", malicious, true)
                .await
                .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[tokio::test]
    async fn engineering_cancellation_is_fenced_replay_safe_and_waits_for_cessation() {
        use crate::engineering::*;
        let temp = TempDir::new().unwrap();
        let database = temp.path().join("engineering-cancel.sqlite");
        let now = SystemClock.now();
        let mut store = Store::open(&database).unwrap();
        let created = store
            .engineering_command(
                EngineeringActor::Operator {
                    name: "local operator".into(),
                },
                EngineeringCommandEnvelope {
                    command_id: "create-cancellation-test".into(),
                    expected: None,
                    command: EngineeringCommand::CreateOutcome {
                        contract: engineering_test_contract(),
                    },
                },
                now,
            )
            .unwrap();
        let before_claim = store
            .engineering_outcome(&created.outcome_id)
            .unwrap()
            .unwrap()
            .precondition();
        let claim = store
            .claim_due_engineering(EngineeringRole::Supervisor, now, 600, 1)
            .unwrap()
            .remove(0);
        let expected = store
            .engineering_outcome(&created.outcome_id)
            .unwrap()
            .unwrap()
            .precondition();
        let application = test_router(database);
        let uri = format!("/engineering/outcomes/{}/cancel", created.outcome_id);
        let body = json!({"command_id":"cancel-outcome", "expected":expected});
        assert_eq!(
            engineering_post(application.clone(), &uri, body.clone(), false)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            engineering_post(
                application.clone(),
                &uri,
                json!({"command_id":"stale-cancel", "expected":before_claim}),
                true
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        let (status, saved) = engineering_post(application.clone(), &uri, body.clone(), true).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(
            engineering_post(application.clone(), &uri, body, true)
                .await
                .1,
            saved
        );
        assert_eq!(
            engineering_post(
                application,
                &uri,
                json!({"command_id":"second-stale-cancel", "expected":expected}),
                true
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        let cancelling = store
            .engineering_outcome(&created.outcome_id)
            .unwrap()
            .unwrap();
        assert!(cancelling.cancellation_requested);
        assert_eq!(cancelling.root.state, crate::ObligationState::Attention);
        assert!(cancelling.executions[0].fenced);
        assert!(!cancelling.executions[0].cessation_verified);
        for (command_id, boundary) in [
            ("observe-cancel", None),
            ("reap-cancel", Some("test-contained-boundary".to_owned())),
        ] {
            let expected = store
                .engineering_outcome(&created.outcome_id)
                .unwrap()
                .unwrap()
                .precondition();
            store
                .engineering_command(
                    EngineeringActor::Reconciler {
                        adapter_id: "test-adapter".into(),
                    },
                    EngineeringCommandEnvelope {
                        command_id: command_id.into(),
                        expected: Some(expected),
                        command: EngineeringCommand::RecordReconciliation(
                            EngineeringReconciliationInput {
                                execution_id: claim.execution_id.clone(),
                                runtime_identity: "test-runtime".into(),
                                observation: "cancellation reconciliation observation".into(),
                                evidence_digest: "b".repeat(64),
                                reaped_boundary: boundary.clone(),
                                recovered_submission: None,
                            },
                        ),
                    },
                    now,
                )
                .unwrap();
            let current = store
                .engineering_outcome(&created.outcome_id)
                .unwrap()
                .unwrap();
            assert_eq!(
                current.root.state,
                if boundary.is_some() {
                    crate::ObligationState::Cancelled
                } else {
                    crate::ObligationState::Attention
                }
            );
        }
    }

    #[tokio::test]
    async fn engineering_intake_requires_trusted_configuration() {
        let temp = TempDir::new().unwrap();
        let database = temp.path().join("engineering.sqlite");
        drop(Store::open(&database).unwrap());
        let (status, response) = engineering_post(
            test_router(database),
            "/engineering/outcomes",
            json!({"command_id":"create", "intent":"Build something"}),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response["error"]["code"], "engineering_not_configured");
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
        assert_eq!(
            bootstrap["service"]["schema_version"],
            bokkie_operator_api::SUPPORTED_SCHEMA_VERSION
        );
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
            "/operator/tasks/id/configuration",
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
    async fn task_configuration_route_saves_revision_and_rejects_stale_or_extra_fields() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("task-configuration-http.sqlite");
        let mut store = Store::open(&database).unwrap();
        let registration = store
            .register_gardener_repository(
                NewRepositoryRegistration {
                    repository: crate::CANONICAL_REPOSITORY.to_owned(),
                    default_branch: crate::CANONICAL_DEFAULT_BRANCH.to_owned(),
                    checkout_path: "/srv/bokkie".to_owned(),
                    inspection_recurrence: Recurrence::new("0 0 * * *", "UTC").unwrap(),
                    first_inspection_at: 2_000_000_000,
                },
                100,
            )
            .unwrap();
        drop(store);
        let application = test_router(database.clone());
        let body = serde_json::json!({
            "expected_revision": 1, "instruction_mode": "replace",
            "instructions": "Investigate recovery tests", "actor": "operator", "note": null
        });
        for (request_body, expected_status) in [
            (body.clone(), StatusCode::OK),
            (body.clone(), StatusCode::CONFLICT),
            (
                {
                    let mut extra = body.clone();
                    extra["inspection_cron"] = "* * * * *".into();
                    extra
                },
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
        ] {
            let response = application
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!(
                            "/operator/tasks/{}/configuration",
                            registration.inspection_obligation_id.replace('/', "%2F")
                        ))
                        .header("host", TEST_AUTHORITY)
                        .header("content-type", "application/json")
                        .header("X-Bokkie-Mutation-Token", TEST_TOKEN)
                        .body(Body::from(request_body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let (status, response) = response_json(response).await;
            assert_eq!(status, expected_status, "{response}");
            if status == StatusCode::OK {
                assert_eq!(response["revision"], 2);
                assert_eq!(response["effective_instructions"], body["instructions"]);
            }
        }
        let store = Store::open(&database).unwrap();
        assert_eq!(
            store
                .gardener_task_configuration(&registration.inspection_obligation_id)
                .unwrap()
                .revision,
            2
        );
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

    #[tokio::test]
    async fn read_routes_are_bounded_and_publish_durable_continuation_headers() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("page-http.sqlite");
        let mut store = Store::open(&database).unwrap();
        for id in ["a", "b", "c"] {
            store
                .create(
                    NewObligation {
                        id: id.to_owned(),
                        description: id.to_owned(),
                        scheduled_at: 100,
                        recurrence: None,
                        approval_required: false,
                        retry: RetryPolicy::default(),
                    },
                    100,
                )
                .unwrap();
        }
        drop(store);
        let router = test_router(database);
        let first = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/obligations?limit=2")
                    .header("host", TEST_AUTHORITY)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let cursor = first.headers()[&NEXT_CURSOR_HEADER]
            .to_str()
            .unwrap()
            .to_owned();
        let watermark = first.headers()[&WATERMARK_HEADER]
            .to_str()
            .unwrap()
            .to_owned();
        let body = to_bytes(first.into_body(), usize::MAX).await.unwrap();
        let items: Vec<Obligation> = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );

        let second = router
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/obligations?limit=2&watermark={watermark}&cursor={cursor}"
                    ))
                    .header("host", TEST_AUTHORITY)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        assert!(second.headers().get(&NEXT_CURSOR_HEADER).is_none());
        let body = to_bytes(second.into_body(), usize::MAX).await.unwrap();
        let items: Vec<Obligation> = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["c"]
        );
    }

    #[tokio::test]
    async fn incremental_changes_are_typed_bounded_and_do_not_require_mutation_token() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("changes-http.sqlite");
        let mut store = Store::open(&database).unwrap();
        for id in ["a", "b"] {
            store
                .create(
                    NewObligation {
                        id: id.to_owned(),
                        description: id.to_owned(),
                        scheduled_at: 100,
                        recurrence: None,
                        approval_required: false,
                        retry: RetryPolicy::default(),
                    },
                    100,
                )
                .unwrap();
        }
        drop(store);
        let response = test_router(database)
            .oneshot(
                Request::builder()
                    .uri("/operator/changes?after=0&limit=1")
                    .header("host", TEST_AUTHORITY)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let page: bokkie_operator_api::ProjectionChangePage =
            serde_json::from_slice(&body).unwrap();
        assert_eq!(page.changes.len(), 1);
        assert_eq!(page.requested_after, 0);
        assert!(page.next_after.is_some());
        assert_eq!(
            page.service.schema_version,
            bokkie_operator_api::SUPPORTED_SCHEMA_VERSION
        );
    }

    #[tokio::test]
    async fn incremental_changes_accept_the_dedicated_one_thousand_item_limit() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("change-limit-http.sqlite");
        drop(Store::open(&database).unwrap());
        let application = test_router(database);

        for limit in [500, 501, 1_000] {
            let response = application
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/operator/changes?limit={limit}"))
                        .header("host", TEST_AUTHORITY)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "limit {limit}");
        }

        let response = application
            .oneshot(
                Request::builder()
                    .uri("/operator/changes?limit=1001")
                    .header("host", TEST_AUTHORITY)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let (status, body) = response_json(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_request");
    }

    #[tokio::test]
    async fn operator_obligation_and_topic_reads_expose_identity_and_watermark() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("operator-read-identity-http.sqlite");
        let mut store = Store::open(&database).unwrap();
        store
            .create(
                NewObligation {
                    id: "affected".to_owned(),
                    description: "Affected obligation".to_owned(),
                    scheduled_at: 100,
                    recurrence: None,
                    approval_required: false,
                    retry: RetryPolicy::default(),
                },
                100,
            )
            .unwrap();
        drop(store);
        let application = test_router(database);

        let affected = application
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/operator/obligations/affected")
                    .header("host", TEST_AUTHORITY)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(affected.status(), StatusCode::OK);
        let body = to_bytes(affected.into_body(), usize::MAX).await.unwrap();
        let affected: bokkie_operator_api::OperatorObligationProjection =
            serde_json::from_slice(&body).unwrap();
        assert_eq!(affected.service.session_id, "test-session");
        assert_eq!(
            affected.service.schema_version,
            bokkie_operator_api::SUPPORTED_SCHEMA_VERSION
        );
        assert!(affected.watermark > 0);
        assert_eq!(affected.obligation.id, "affected");

        let topic = application
            .oneshot(
                Request::builder()
                    .uri("/operator/obligations/affected/topic")
                    .header("host", TEST_AUTHORITY)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(topic.status(), StatusCode::OK);
        let body = to_bytes(topic.into_body(), usize::MAX).await.unwrap();
        let topic: bokkie_operator_api::ObligationTopic = serde_json::from_slice(&body).unwrap();
        assert_eq!(topic.service.as_ref(), Some(&affected.service));
        assert_eq!(topic.watermark, affected.watermark);
        assert!(!topic.items.is_empty());
        assert!(
            topic
                .items
                .iter()
                .all(|item| item.evidence.get("envelope_sequence").is_none())
        );
    }
}
