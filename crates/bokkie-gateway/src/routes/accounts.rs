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
use crate::auth::{self, SignedIn};
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
    if !state.limiter.allows(&request.username, &address, now.0) {
        return Err(auth::rate_limited());
    }
    let account = {
        // Bound concurrent hashing work.
        let _permit = state.hashing.acquire().await.map_err(Failure::internal)?;
        accounts::check_credentials(&state.db, &request.username, &request.password).await?
    };
    let Some(account) = account else {
        state
            .limiter
            .record_failure(&request.username, &address, now.0);
        tracing::info!(%address, "failed sign-in");
        return Err(Failure::new(
            bokkie_protocol::api::ErrorCode::Unauthenticated,
            "The username or password is wrong",
        ));
    };
    state.limiter.record_success(&request.username);
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
    let _permit = state.hashing.acquire().await.map_err(Failure::internal)?;
    accounts::change_own_password(
        &state.db,
        state.clock.now(),
        signed_in.id(),
        signed_in.session_id,
        &request.current_password,
        &request.new_password,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/invitations/accept`. Rate-limited like sign-in, by client
/// address, so codes cannot be guessed quickly.
pub async fn accept_invitation(
    State(state): State<AppState>,
    ClientAddress(address): ClientAddress,
    headers: HeaderMap,
    Body(request): Body<AcceptInvitationRequest>,
) -> Result<Response> {
    let now = state.clock.now();
    // Invitation attempts share the address limit under a key no username
    // can take.
    let key = "#invitation";
    if !state.limiter.allows(key, &address, now.0) {
        return Err(auth::rate_limited());
    }
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
    let user = match result {
        Ok(user) => user,
        Err(failure) => {
            if failure.code == bokkie_protocol::api::ErrorCode::Forbidden {
                state.limiter.record_failure(key, &address, now.0);
            }
            return Err(failure);
        }
    };
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
