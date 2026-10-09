//! Authenticated outward host exchange; browser changes retain their own fences.
use crate::{
    StoreError, SystemClock, UnixClock, WorkspaceExchangeRequest, WorkspaceExchangeResponse,
    WorkspaceRunActionRequest, WorkspaceTaskEditRequest,
    http::{ApiError, ApiState},
    workspace::WorkspaceHost,
};
use axum::{
    Extension, Json, Router,
    extract::{Path, Request, State},
    http::{StatusCode, header::AUTHORIZATION},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::post,
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

#[derive(Clone)]
pub(crate) struct AuthenticatedWorkspaceHost(pub WorkspaceHost);

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/workspace-hosts/{id}/exchange", post(exchange))
        .route("/tasks/managed/{id}/definition", post(edit))
        .route("/tasks/workspace/action", post(action))
}

fn host_route(path: &str) -> Option<&str> {
    let id = path
        .strip_prefix("/workspace-hosts/")?
        .strip_suffix("/exchange")?;
    (!id.is_empty() && !id.contains('/')).then_some(id)
}

pub async fn authenticate(
    State(state): State<ApiState>,
    mut request: Request,
    next: Next,
) -> Response {
    if let Some(id) = host_route(request.uri().path()) {
        let host = state
            .workspace
            .as_ref()
            .and_then(|c| c.hosts.iter().find(|h| h.id == id));
        let mut authorisations = request.headers().get_all(AUTHORIZATION).iter();
        let authorisation = authorisations.next().map(|h| h.to_str()).transpose();
        let mut host_tokens = request.headers().get_all("x-bokkie-host-token").iter();
        let host_token = host_tokens.next().map(|h| h.to_str()).transpose();
        // The dedicated header can traverse the existing Basic-auth edge.
        // Direct Bearer clients remain compatible; two token mechanisms conflict.
        let supplied = match (host_token, authorisation) {
            (Ok(Some(token)), Ok(None)) => Some(token),
            (Ok(Some(token)), Ok(Some(auth))) if auth.starts_with("Basic ") => Some(token),
            (Ok(None), Ok(Some(auth))) => auth.strip_prefix("Bearer "),
            _ => None,
        };
        let authenticated = match (host, supplied, authorisations.next(), host_tokens.next()) {
            (Some(host), Some(token), None, None)
                if token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                let digest = format!("{:x}", Sha256::digest(token.as_bytes()));
                bool::from(digest.as_bytes().ct_eq(host.token_sha256.as_bytes()))
            }
            _ => false,
        };
        if !authenticated {
            return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":{"code":"workspace_host_unauthorised","message":"This execution host is not authenticated"}}))).into_response();
        }
        request.extensions_mut().insert(AuthenticatedWorkspaceHost(
            host.expect("authenticated host").clone(),
        ));
    }
    next.run(request).await
}

async fn exchange(
    State(state): State<ApiState>,
    Extension(host): Extension<AuthenticatedWorkspaceHost>,
    Json(request): Json<WorkspaceExchangeRequest>,
) -> Result<Json<WorkspaceExchangeResponse>, ApiError> {
    let now = SystemClock.now();
    Ok(Json(
        state
            .executor
            .execute(move |s| s.workspace_exchange(&host.0, &request, now))
            .await?,
    ))
}

async fn edit(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(request): Json<WorkspaceTaskEditRequest>,
) -> Result<Json<crate::ConversationView>, ApiError> {
    let now = SystemClock.now();
    let conversation_id = request.conversation_id.clone();
    let task_id = id.clone();
    state
        .executor
        .execute(move |s| {
            s.managed_revise(
                &request.command_id,
                &id,
                request.configuration_revision,
                &request.definition,
                now,
            )?;
            Ok(())
        })
        .await?;
    crate::conversation_http::make_review(
        &state,
        &conversation_id,
        &task_id,
        crate::ConversationAction::Activate,
    )
    .await?;
    Ok(Json(
        crate::conversation_http::get_view(&state, conversation_id).await?,
    ))
}

async fn action(
    State(state): State<ApiState>,
    Json(request): Json<WorkspaceRunActionRequest>,
) -> Result<Json<crate::ConversationView>, ApiError> {
    if request.answer.is_some() == request.cancel {
        return Err(StoreError::Invalid("Choose one answer or stop request".into()).into());
    }
    let conversation_id = request.conversation_id.clone();
    state
        .executor
        .execute(move |s| s.workspace_run_action(&request, SystemClock.now()))
        .await?;
    Ok(Json(
        crate::conversation_http::get_view(&state, conversation_id).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DbExecutor, Store, WorkspaceLimits,
        http::router_with_state,
        http_security::ApiRuntime,
        workspace::{WorkspaceHostConfig, WorkspaceHostProject},
    };
    use axum::{body::Body, http::Request};
    use std::sync::Arc;
    use tempfile::TempDir;
    use tower::ServiceExt;

    #[tokio::test]
    async fn host_authentication_is_separate_and_scoped_to_exchange() {
        let root = TempDir::new().unwrap();
        let database = root.path().join("controller.sqlite");
        drop(Store::open(&database).unwrap());
        let executor = DbExecutor::start(database).unwrap();
        let token = "ab".repeat(32);
        let app = router_with_state(
            ApiState {
                executor: executor.clone(),
                runtime: ApiRuntime::new(
                    "127.0.0.1:7744".parse().unwrap(),
                    crate::SUPPORTED_SCHEMA_VERSION,
                )
                .unwrap(),
                engineering_intake: None,
                conversation: None,
                workspace: Some(Arc::new(WorkspaceHostConfig {
                    hosts: vec![WorkspaceHost {
                        id: "development".into(),
                        name: "Development".into(),
                        token_sha256: format!("{:x}", Sha256::digest(token.as_bytes())),
                        projects: vec![WorkspaceHostProject {
                            project_id: "project".into(),
                            profile_revision: "workspace-v1/project".into(),
                            permitted_actions: vec!["inspect".into()],
                            limits: WorkspaceLimits {
                                max_seconds: 60,
                                max_turns: 1,
                                max_tokens: 1000,
                            },
                        }],
                    }],
                })),
            },
            None,
        );
        for (path, bearer, host, expected) in [
            (
                "/workspace-hosts/development/exchange",
                None,
                "127.0.0.1:7744",
                StatusCode::UNAUTHORIZED,
            ),
            (
                "/workspace-hosts/development/exchange",
                Some("invalid"),
                "127.0.0.1:7744",
                StatusCode::UNAUTHORIZED,
            ),
            (
                "/workspace-hosts/development/exchange",
                Some(token.as_str()),
                "127.0.0.1:7744",
                StatusCode::OK,
            ),
            (
                "/workspace-hosts/development/exchange",
                Some(token.as_str()),
                "wrong.example",
                StatusCode::MISDIRECTED_REQUEST,
            ),
            (
                "/workspace-hosts/other/exchange",
                Some(token.as_str()),
                "127.0.0.1:7744",
                StatusCode::UNAUTHORIZED,
            ),
            (
                "/conversations/turn",
                Some(token.as_str()),
                "127.0.0.1:7744",
                StatusCode::FORBIDDEN,
            ),
        ] {
            let mut request = Request::builder()
                .method("POST")
                .uri(path)
                .header("Host", host)
                .header("Content-Type", "application/json");
            if let Some(bearer) = bearer {
                request = request.header("Authorization", format!("Bearer {bearer}"));
            }
            let response = app
                .clone()
                .oneshot(
                    request
                        .body(Body::from(r#"{"events":[],"heartbeats":[]}"#))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "{path}");
            if expected == StatusCode::OK {
                assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            }
        }
        for (dedicated, authorisation, expected) in [
            (Some(token.as_str()), None, StatusCode::OK),
            (
                Some(token.as_str()),
                Some("Basic dXNlcjpwYXNz"),
                StatusCode::OK,
            ),
            (
                Some(token.as_str()),
                Some("Bearer invalid"),
                StatusCode::UNAUTHORIZED,
            ),
            (None, Some("Basic dXNlcjpwYXNz"), StatusCode::UNAUTHORIZED),
        ] {
            let mut request = Request::builder()
                .method("POST")
                .uri("/workspace-hosts/development/exchange")
                .header("Host", "127.0.0.1:7744")
                .header("Content-Type", "application/json");
            if let Some(dedicated) = dedicated {
                request = request.header("X-Bokkie-Host-Token", dedicated);
            }
            if let Some(authorisation) = authorisation {
                request = request.header("Authorization", authorisation);
            }
            let response = app
                .clone()
                .oneshot(
                    request
                        .body(Body::from(r#"{"events":[],"heartbeats":[]}"#))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/workspace-hosts/development/exchange")
                    .header("Host", "127.0.0.1:7744")
                    .header("Content-Type", "application/json")
                    .header("X-Bokkie-Host-Token", token.as_str())
                    .header("X-Bokkie-Host-Token", token.as_str())
                    .body(Body::from(r#"{"events":[],"heartbeats":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        executor.shutdown().unwrap();
    }
}
