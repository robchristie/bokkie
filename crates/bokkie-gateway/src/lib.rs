//! The Bokkie gateway: an axum service that owns accounts, sessions, Bots
//! and routing, and supervises one Codex app server per user.
//!
//! Every route sits behind [`auth::guard`], which checks `Host` on every
//! request and `Origin` on every state-changing request. Routes that act
//! for a user take the [`auth::SignedIn`] extractor and pass its user id to
//! every query, so one user can never name another user's data.

pub mod accounts;
pub mod auth;
pub mod clock;
pub mod config;
pub mod db;
pub mod error;
pub mod routes;
pub mod secrets;

use std::sync::Arc;

use axum::Router;
use axum::routing::{delete, get, patch, post, put};
use bokkie_protocol::api::VersionResponse;
use sqlx::SqlitePool;
use tokio::sync::Semaphore;

use crate::auth::LoginLimiter;
use crate::clock::Clock;
use crate::config::Config;

/// State shared by every request.
#[derive(Debug, Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: SqlitePool,
    pub clock: Clock,
    pub limiter: Arc<LoginLimiter>,
    /// Bounds concurrent password hashing, which is slow by design.
    pub hashing: Arc<Semaphore>,
}

impl AppState {
    pub fn new(config: Config, db: SqlitePool, clock: Clock) -> Self {
        Self {
            config: Arc::new(config),
            db,
            clock,
            limiter: Arc::default(),
            hashing: Arc::new(Semaphore::new(2)),
        }
    }
}

/// The gateway's routes, behind the request guard.
pub fn router(state: AppState) -> Router {
    use routes::{accounts, admin};
    let api = Router::new()
        .route("/version", get(version))
        .route(
            "/session",
            post(accounts::login)
                .get(accounts::current)
                .delete(accounts::logout),
        )
        .route("/session/password", put(accounts::change_password))
        .route("/sessions", get(accounts::list_sessions))
        .route("/sessions/{id}", delete(accounts::revoke_session))
        .route("/invitations/accept", post(accounts::accept_invitation))
        .route("/admin/users", get(admin::list_users))
        .route("/admin/users/{id}", patch(admin::update_user))
        .route("/admin/users/{id}/password", put(admin::reset_password))
        .route(
            "/admin/invitations",
            get(admin::list_invitations).post(admin::create_invitation),
        )
        .route(
            "/admin/invitations/{id}",
            delete(admin::withdraw_invitation),
        );
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest(bokkie_protocol::API_PREFIX, api)
        .fallback(|| async { error::Failure::not_found() })
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::guard,
        ))
        .with_state(state)
}

async fn version() -> axum::Json<VersionResponse> {
    axum::Json(VersionResponse {
        protocol: bokkie_protocol::PROTOCOL_VERSION,
        gateway: env!("CARGO_PKG_VERSION").into(),
    })
}
