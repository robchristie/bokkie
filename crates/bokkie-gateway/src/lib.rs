//! The Bokkie gateway: an axum service that owns accounts, sessions, Bots
//! and routing, and supervises one Codex app server per user, all on the
//! household's one ChatGPT sign-in.
//!
//! Every route sits behind [`auth::guard`], which checks `Host` on every
//! request and `Origin` on every state-changing request. Routes that act
//! for a user take the [`auth::SignedIn`] extractor and pass its user id to
//! every query, so one user can never name another user's data. The
//! exception is an admin's view of a member's account and Bots, in
//! [`routes::admin`], which the member's permissions and oversight switch
//! bound.
//!
//! With `BOKKIE_WEB_ROOT` set, [`web::serve`] is the fallback and serves the
//! production web package at the same origin; otherwise every unrouted path
//! is the API's 404.

pub mod accounts;
pub mod auth;
pub mod bots;
pub mod clock;
pub mod codex;
pub mod config;
pub mod db;
pub mod error;
pub mod events;
pub mod grants;
pub mod http;
pub mod routes;
pub mod routines;
pub mod secrets;
pub mod tools;
pub mod usage;
pub mod web;

use std::sync::Arc;

use axum::Router;
use axum::routing::{delete, get, patch, post, put};
use bokkie_protocol::api::VersionResponse;
use sqlx::SqlitePool;
use tokio::sync::Semaphore;

use crate::auth::LoginLimiter;
use crate::bots::Conversations;
use crate::clock::Clock;
use crate::codex::Supervisor;
use crate::config::Config;
use crate::events::EventLog;
use crate::routines::Routines;

/// State shared by every request.
#[derive(Debug, Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: SqlitePool,
    pub clock: Clock,
    pub limiter: Arc<LoginLimiter>,
    /// Bounds concurrent password hashing, which is slow by design.
    pub hashing: Arc<Semaphore>,
    pub supervisor: Arc<Supervisor>,
    pub conversations: Arc<Conversations>,
    pub routines: Arc<Routines>,
    pub events: Arc<EventLog>,
    /// How often an open event socket rechecks its session.
    pub session_check: std::time::Duration,
}

impl AppState {
    pub fn new(config: Config, db: SqlitePool, clock: Clock, supervisor: Arc<Supervisor>) -> Self {
        Self::with_events(
            config,
            db.clone(),
            clock.clone(),
            supervisor,
            Arc::new(EventLog::new(db, clock)),
        )
    }

    /// Like [`AppState::new`], with a given event log.
    pub fn with_events(
        config: Config,
        db: SqlitePool,
        clock: Clock,
        supervisor: Arc<Supervisor>,
        events: Arc<EventLog>,
    ) -> Self {
        let conversations = Arc::new(Conversations::new(
            db.clone(),
            clock.clone(),
            supervisor.clone(),
            events.clone(),
        ));
        let routines = Arc::new(Routines::new(
            db.clone(),
            clock.clone(),
            events.clone(),
            conversations.clone(),
        ));
        Self {
            conversations,
            routines,
            events,
            session_check: std::time::Duration::from_secs(30),
            config: Arc::new(config),
            db,
            clock,
            limiter: Arc::default(),
            hashing: Arc::new(Semaphore::new(2)),
            supervisor,
        }
    }
}

/// The gateway's routes, behind the request guard.
pub fn router(state: AppState) -> Router {
    use routes::{accounts, admin, bots, codex, events, routines, usage};
    let api = Router::new()
        .route("/version", get(version))
        .route(
            "/session",
            post(accounts::login)
                .get(accounts::current)
                .delete(accounts::logout),
        )
        .route("/session/password", put(accounts::change_password))
        .route("/session/time-zone", put(accounts::set_time_zone))
        .route(
            "/session/password-reset",
            delete(accounts::acknowledge_password_reset),
        )
        .route("/sessions", get(accounts::list_sessions))
        .route("/sessions/{id}", delete(accounts::revoke_session))
        .route("/invitations/accept", post(accounts::accept_invitation))
        .route("/bots", get(bots::list).post(bots::create))
        .route("/bots/{id}", get(bots::get).patch(bots::update))
        .route("/bots/{id}/messages", get(bots::messages).post(bots::send))
        .route("/bots/{id}/stop", post(bots::stop))
        .route(
            "/bots/{id}/notes",
            get(bots::notes).delete(bots::clear_notes),
        )
        .route("/bots/{id}/tool-calls", get(bots::tool_calls))
        .route("/routines", get(routines::list_all))
        .route(
            "/bots/{id}/routines",
            get(routines::list).post(routines::create),
        )
        .route(
            "/bots/{id}/routines/{routine}",
            get(routines::get)
                .patch(routines::update)
                .delete(routines::delete),
        )
        .route("/bots/{id}/routines/{routine}/run", post(routines::run_now))
        .route("/bots/{id}/routines/{routine}/runs", get(routines::runs))
        .route("/events", get(events::events))
        .route("/usage", get(usage::usage))
        .route("/admin/users/{id}/usage", get(usage::admin_usage))
        .route("/codex/account", get(codex::account))
        .route(
            "/codex/login",
            post(codex::login)
                .get(codex::current_login)
                .delete(codex::cancel_login),
        )
        .route("/codex/logout", post(codex::logout))
        .route("/admin/users", get(admin::list_users))
        .route("/admin/users/{id}", patch(admin::update_user))
        .route("/admin/users/{id}/password", put(admin::reset_password))
        .route(
            "/admin/users/{id}/bots",
            get(admin::member_bots).post(admin::create_member_bot),
        )
        .route(
            "/admin/users/{id}/bots/{bot}",
            patch(admin::update_member_bot),
        )
        .route(
            "/admin/users/{id}/bots/{bot}/messages",
            get(admin::member_messages),
        )
        .route(
            "/admin/invitations",
            get(admin::list_invitations).post(admin::create_invitation),
        )
        .route(
            "/admin/invitations/{id}",
            delete(admin::withdraw_invitation),
        );
    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest(bokkie_protocol::API_PREFIX, api);
    let app = match state.config.web_root.clone() {
        Some(root) => {
            let policy = web::ContentSecurityPolicy::for_origin(&state.config.origin);
            app.fallback(move |request: axum::extract::Request| {
                web::serve(root.clone(), policy.clone(), request)
            })
        }
        None => app.fallback(|| async { error::Failure::not_found() }),
    };
    app.layer(axum::middleware::from_fn_with_state(
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
