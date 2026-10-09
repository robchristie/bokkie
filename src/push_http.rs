//! Push setup shares the existing authenticated same-origin mutation boundary.
use crate::{
    StoreError, SystemClock, UnixClock,
    http::{ApiError, ApiState},
    notifications::push::PushConfig,
};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use bokkie_operator_api::{PushDisableRequest, PushReceiptRequest, PushRegisterRequest, PushSetup};
use std::sync::Arc;

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/notifications/push", get(view))
        .route("/notifications/push/register", post(register))
        .route("/notifications/push/disable", post(disable))
        .route("/notifications/push/receipts", post(receipt))
}
fn config(state: &ApiState) -> Option<Arc<PushConfig>> {
    state.conversation.as_ref().and_then(|c| c.push.clone())
}
fn now(state: &ApiState) -> i64 {
    state
        .conversation
        .as_ref()
        .map_or_else(|| SystemClock.now(), |c| c.now())
}
async fn view(State(state): State<ApiState>) -> Result<Json<PushSetup>, ApiError> {
    let c = config(&state);
    let key = c
        .as_ref()
        .map(|c| c.public_key())
        .transpose()
        .map_err(StoreError::Invalid)?;
    let ttl = c.as_ref().map_or(0, |c| c.ttl_seconds);
    let service = state.runtime.identity();
    Ok(Json(
        state
            .executor
            .execute(move |s| s.push_setup(service, key, ttl))
            .await?,
    ))
}
async fn register(
    State(state): State<ApiState>,
    Json(request): Json<PushRegisterRequest>,
) -> Result<Json<PushSetup>, ApiError> {
    let c = config(&state).ok_or_else(|| {
        StoreError::Invalid("Bokkie push is not configured; saved reminders remain readable".into())
    })?;
    let key = c.public_key().map_err(StoreError::Invalid)?;
    let service = state.runtime.identity();
    let now = now(&state);
    Ok(Json(
        state
            .executor
            .execute(move |s| s.register_push(&request, service, &key, c.ttl_seconds, now))
            .await?,
    ))
}
async fn disable(
    State(state): State<ApiState>,
    Json(request): Json<PushDisableRequest>,
) -> Result<Json<PushSetup>, ApiError> {
    let c = config(&state);
    let key = c
        .as_ref()
        .map(|c| c.public_key())
        .transpose()
        .map_err(StoreError::Invalid)?;
    let ttl = c.as_ref().map_or(0, |c| c.ttl_seconds);
    let service = state.runtime.identity();
    Ok(Json(
        state
            .executor
            .execute(move |s| s.disable_push(&request, service, key, ttl))
            .await?,
    ))
}
async fn receipt(
    State(state): State<ApiState>,
    Json(request): Json<PushReceiptRequest>,
) -> Result<StatusCode, ApiError> {
    let now = now(&state);
    state
        .executor
        .execute(move |s| s.record_push_receipt(&request, now))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DbExecutor, Store, conversation_http::ConversationConfig, http::router_with_state,
        http_security::ApiRuntime,
    };
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use tower::ServiceExt;
    #[tokio::test]
    async fn registration_is_authenticated_fenced_idempotent_and_never_exposes_subscription_secrets()
     {
        let temp = tempfile::TempDir::new().unwrap();
        let db = temp.path().join("push-http.sqlite");
        drop(Store::open(&db).unwrap());
        let executor = DbExecutor::start(db.clone()).unwrap();
        let runtime = ApiRuntime::new("127.0.0.1:7744".parse().unwrap(), 15).unwrap();
        let token = runtime.bootstrap().mutation_token;
        let config = Arc::new(PushConfig {
            vapid_private_key: URL_SAFE_NO_PAD.encode([7; 32]),
            subject: "https://bokkie.example.org".into(),
            timeout_ms: 1000,
            ttl_seconds: 3600,
        });
        let key = config.public_key().unwrap();
        let app = router_with_state(
            ApiState {
                workspace: None,
                executor: executor.clone(),
                runtime: runtime.clone(),
                engineering_intake: None,
                conversation: Some(ConversationConfig {
                    profile: None,
                    notes_enabled: false,
                    notifications: None,
                    push: Some(config),
                    clock: Some(Arc::new(crate::ManualClock::new(100))),
                }),
            },
            None,
        );
        let body=serde_json::json!({"command_id":uuid::Uuid::new_v4().to_string(),"configuration_revision":0,"label":"Phone","endpoint":"https://fcm.googleapis.com/fcm/send/synthetic-http","keys":{"p256dh":key,"auth":URL_SAFE_NO_PAD.encode([9;16])}}).to_string();
        let request = |token: &str, origin: &str| {
            Request::builder()
                .method("POST")
                .uri("/notifications/push/register")
                .header("host", "127.0.0.1:7744")
                .header("origin", origin)
                .header("content-type", "application/json")
                .header("x-bokkie-mutation-token", token)
                .body(Body::from(body.clone()))
                .unwrap()
        };
        assert_eq!(
            app.clone()
                .oneshot(request("", "http://127.0.0.1:7744"))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.clone()
                .oneshot(request(&token, "https://hostile.example.org"))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let response = app
            .clone()
            .oneshot(request(&token, "http://127.0.0.1:7744"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 8192).await.unwrap();
        let setup: PushSetup = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(setup.device.unwrap().label, "Phone");
        let serialised = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(!serialised.contains("fcm.googleapis"));
        assert!(!serialised.contains("p256dh"));
        assert!(!serialised.contains("auth"));
        assert_eq!(
            app.clone()
                .oneshot(request(&token, "http://127.0.0.1:7744"))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let mut stale: serde_json::Value = serde_json::from_str(&body).unwrap();
        stale["command_id"] = serde_json::json!(uuid::Uuid::new_v4().to_string());
        let r = Request::builder()
            .method("POST")
            .uri("/notifications/push/register")
            .header("host", "127.0.0.1:7744")
            .header("origin", "http://127.0.0.1:7744")
            .header("content-type", "application/json")
            .header("x-bokkie-mutation-token", &token)
            .body(Body::from(stale.to_string()))
            .unwrap();
        assert_eq!(app.oneshot(r).await.unwrap().status(), StatusCode::CONFLICT);
        assert_eq!(
            Store::open_compatible(&db)
                .unwrap()
                .push_setup(runtime.identity(), Some(key), 3600)
                .unwrap()
                .configuration_revision,
            1
        );
        executor.shutdown().unwrap();
    }
}
