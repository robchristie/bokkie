//! Sign-in, sessions, password changes and invitation acceptance.

use axum::Json;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::header::{SET_COOKIE, USER_AGENT};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bokkie_protocol::api::{
    AcceptInvitationRequest, ChangePasswordRequest, LoginRequest, SessionList, SessionResponse,
};

use crate::AppState;
use crate::accounts;
use crate::auth::{self, Attempting, SignedIn};
use crate::error::{Body, Failure, Result};

/// The client's address, for rate limiting.
#[derive(Debug)]
pub struct ClientAddress(pub String);

impl FromRequestParts<AppState> for ClientAddress {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(auth::client_address(
            parts,
            state.config.trust_forwarded_for,
        )))
    }
}

fn user_agent(headers: &HeaderMap) -> Option<&str> {
    headers.get(USER_AGENT).and_then(|v| v.to_str().ok())
}

fn signed_in_response(token: &str, body: SessionResponse) -> Response {
    let mut response = Json(body).into_response();
    response
        .headers_mut()
        .insert(SET_COOKIE, auth::session_cookie(token));
    response
}

/// `POST /api/v1/session`.
pub async fn login(
    State(state): State<AppState>,
    ClientAddress(address): ClientAddress,
    headers: HeaderMap,
    Body(request): Body<LoginRequest>,
) -> Result<Response> {
    let now = state.clock.now();
    let attempt = state
        .limiter
        .begin(
            Attempting::SignIn {
                username: &request.username,
                address: &address,
            },
            now.0,
        )
        .ok_or_else(auth::rate_limited)?;
    let account = {
        // Bound concurrent hashing work.
        let _permit = state.hashing.acquire().await.map_err(Failure::internal)?;
        accounts::check_credentials(&state.db, &request.username, &request.password).await?
    };
    let Some(account) = account else {
        // The attempt stays counted.
        tracing::info!(%address, "failed sign-in");
        return Err(Failure::new(
            bokkie_protocol::api::ErrorCode::Unauthenticated,
            "The username or password is wrong",
        ));
    };
    state.limiter.succeeded(attempt);
    let token =
        accounts::create_session(&state.db, now, account.user.id, user_agent(&headers)).await?;
    tracing::info!(user = %account.user.id, "signed in");
    Ok(signed_in_response(
        &token,
        SessionResponse { user: account.user },
    ))
}

/// `GET /api/v1/session`. Also refreshes the cookie's lifetime in the
/// browser.
pub async fn current(signed_in: SignedIn, headers: HeaderMap) -> Response {
    let body = SessionResponse {
        user: signed_in.account.user,
    };
    match auth::session_token(&headers) {
        Some(token) => signed_in_response(&token, body),
        None => Json(body).into_response(),
    }
}

/// `DELETE /api/v1/session`: sign out.
pub async fn logout(State(state): State<AppState>, signed_in: SignedIn) -> Result<Response> {
    accounts::revoke_session(
        &state.db,
        state.clock.now(),
        signed_in.id(),
        signed_in.session_id,
    )
    .await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(SET_COOKIE, auth::clear_session_cookie());
    Ok(response)
}

/// `GET /api/v1/sessions`.
pub async fn list_sessions(
    State(state): State<AppState>,
    signed_in: SignedIn,
) -> Result<Json<SessionList>> {
    let sessions = accounts::list_sessions(
        &state.db,
        state.clock.now(),
        signed_in.id(),
        signed_in.session_id,
    )
    .await?;
    Ok(Json(SessionList { sessions }))
}

/// `DELETE /api/v1/sessions/{id}`.
pub async fn revoke_session(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Path(id): Path<i64>,
) -> Result<StatusCode> {
    accounts::revoke_session(&state.db, state.clock.now(), signed_in.id(), id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /api/v1/session/password`.
pub async fn change_password(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Body(request): Body<ChangePasswordRequest>,
) -> Result<StatusCode> {
    let now = state.clock.now();
    let attempt = state
        .limiter
        .begin(
            Attempting::PasswordChange {
                user: signed_in.id(),
            },
            now.0,
        )
        .ok_or_else(auth::rate_limited)?;
    let _permit = state.hashing.acquire().await.map_err(Failure::internal)?;
    let result = accounts::change_own_password(
        &state.db,
        now,
        signed_in.id(),
        signed_in.session_id,
        &request.current_password,
        &request.new_password,
    )
    .await;
    // Only a wrong current password counts against the limit.
    if !matches!(&result, Err(f) if f.code == bokkie_protocol::api::ErrorCode::Forbidden) {
        state.limiter.succeeded(attempt);
    }
    result?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/invitations/accept`. Refused codes count against the
/// client address's sign-in limit, so codes cannot be guessed quickly.
pub async fn accept_invitation(
    State(state): State<AppState>,
    ClientAddress(address): ClientAddress,
    headers: HeaderMap,
    Body(request): Body<AcceptInvitationRequest>,
) -> Result<Response> {
    let now = state.clock.now();
    let attempt = state
        .limiter
        .begin(Attempting::Invitation { address: &address }, now.0)
        .ok_or_else(auth::rate_limited)?;
    let result = {
        let _permit = state.hashing.acquire().await.map_err(Failure::internal)?;
        accounts::accept_invitation(
            &state.db,
            now,
            &request.code,
            &request.username,
            &request.display_name,
            &request.password,
        )
        .await
    };
    // Only a refused code counts against the limit.
    if !matches!(&result, Err(f) if f.code == bokkie_protocol::api::ErrorCode::Forbidden) {
        state.limiter.succeeded(attempt);
    }
    let user = result?;
    let account = accounts::account(&state.db, user)
        .await?
        .ok_or_else(Failure::not_found)?;
    let token = accounts::create_session(&state.db, now, user, user_agent(&headers)).await?;
    tracing::info!(user = %user, "accepted an invitation");
    Ok(signed_in_response(
        &token,
        SessionResponse { user: account.user },
    ))
}
