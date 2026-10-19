//! The household's ChatGPT sign-in, which every user's app server shares.
//!
//! Everyone can see whether the household is signed in; a member sees only
//! that. Only an admin sees the account's email and plan, signs the
//! household in by device code, and signs it out, which signs out every
//! app server. Each acts on the household's app server, never a user's.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use bokkie_protocol::api::{CodexAccount, DeviceCodeLogin, Role};

use crate::AppState;
use crate::auth::{SignedIn, SignedInAdmin};
use crate::codex::CodexError;
use crate::error::{Failure, Result};

pub(crate) fn unavailable(error: CodexError) -> Failure {
    match error {
        CodexError::SignedOut(reason) => {
            tracing::warn!(%reason, "the household is signed out");
            signed_out()
        }
        error => {
            tracing::error!(%error, "Codex is unavailable");
            Failure::unavailable("The Bots' runtime is unavailable")
        }
    }
}

/// What a request that needs ChatGPT gets while the household is signed
/// out.
pub(crate) fn signed_out() -> Failure {
    Failure::unavailable(
        "ChatGPT is signed out for the household, so the Bots cannot reply. Ask an admin to sign it in",
    )
}

/// `GET /api/v1/codex/account`: whether the household is signed in. An
/// admin also sees the account's email and plan.
pub async fn account(
    State(state): State<AppState>,
    signed_in: SignedIn,
) -> Result<Json<CodexAccount>> {
    let admin = signed_in.account.user.role == Role::Admin;
    let status = state.supervisor.household_status(admin).await;
    Ok(Json(CodexAccount {
        signed_in: status.signed_in,
        plan: status.plan.filter(|_| admin),
        email: status.email.filter(|_| admin),
    }))
}

/// `POST /api/v1/codex/login`: starts a device-code sign-in of the
/// household, replacing any pending one.
pub async fn login(
    State(state): State<AppState>,
    SignedInAdmin(admin): SignedInAdmin,
) -> Result<Json<DeviceCodeLogin>> {
    let login = state
        .supervisor
        .start_login(state.clock.now())
        .await
        .map_err(unavailable)?;
    tracing::info!(admin = %admin.id(), "started a device-code sign-in of the household");
    Ok(Json(login.to_api()))
}

/// `GET /api/v1/codex/login`: the latest device-code sign-in since the
/// gateway started.
pub async fn current_login(
    State(state): State<AppState>,
    _admin: SignedInAdmin,
) -> Result<Json<DeviceCodeLogin>> {
    let login = state
        .supervisor
        .login(state.clock.now())
        .ok_or_else(Failure::not_found)?;
    Ok(Json(login.to_api()))
}

/// `DELETE /api/v1/codex/login`: cancels a pending device-code sign-in.
pub async fn cancel_login(
    State(state): State<AppState>,
    SignedInAdmin(admin): SignedInAdmin,
) -> Result<Json<DeviceCodeLogin>> {
    let login = state
        .supervisor
        .cancel_login(state.clock.now())
        .await
        .map_err(unavailable)?
        .ok_or_else(Failure::not_found)?;
    tracing::info!(admin = %admin.id(), state = ?login.state, "cancelled the household's sign-in");
    Ok(Json(login.to_api()))
}

/// `POST /api/v1/codex/logout`: signs the household out of ChatGPT. That
/// revokes the one sign-in every app server shares, so it signs out
/// everyone, and every user's app server stops.
pub async fn logout(
    State(state): State<AppState>,
    SignedInAdmin(admin): SignedInAdmin,
) -> Result<StatusCode> {
    state
        .supervisor
        .sign_out_household(state.clock.now())
        .await
        .map_err(unavailable)?;
    tracing::info!(admin = %admin.id(), "signed the household out of ChatGPT");
    Ok(StatusCode::NO_CONTENT)
}
