//! The signed-in user's own Codex account.
//!
//! Each user's app server holds its own ChatGPT credential in its own
//! `CODEX_HOME`. Whether household members sign in with their own ChatGPT
//! accounts is still an open decision; these routes only let a
//! user see and start the sign-in of their own app server.

use axum::Json;
use axum::extract::State;
use bokkie_codex::protocol::{Account, LoginAccountResponse};
use bokkie_protocol::api::{CodexAccount, DeviceCodeLogin};

use crate::AppState;
use crate::auth::SignedIn;
use crate::error::{Failure, Result};

pub(crate) fn unavailable(error: crate::codex::CodexError) -> Failure {
    tracing::error!(%error, "Codex is unavailable");
    Failure::unavailable("The Bots' runtime is unavailable")
}

/// `GET /api/v1/codex/account`.
pub async fn account(
    State(state): State<AppState>,
    signed_in: SignedIn,
) -> Result<Json<CodexAccount>> {
    let runtime = state
        .supervisor
        .runtime(signed_in.id())
        .await
        .map_err(unavailable)?;
    let account = runtime.account().await.map_err(unavailable)?;
    Ok(Json(match account.account {
        Some(Account::Chatgpt { plan_type, .. }) => CodexAccount {
            signed_in: true,
            plan: Some(plan_type),
        },
        Some(_) => CodexAccount {
            signed_in: true,
            plan: None,
        },
        None => CodexAccount {
            signed_in: false,
            plan: None,
        },
    }))
}

/// `POST /api/v1/codex/login`.
pub async fn login(
    State(state): State<AppState>,
    signed_in: SignedIn,
) -> Result<Json<DeviceCodeLogin>> {
    let runtime = state
        .supervisor
        .runtime(signed_in.id())
        .await
        .map_err(unavailable)?;
    match runtime.login_start().await.map_err(unavailable)? {
        LoginAccountResponse::ChatgptDeviceCode {
            user_code,
            verification_url,
            ..
        } => {
            tracing::info!(user = %signed_in.id(), "started a Codex device-code sign-in");
            Ok(Json(DeviceCodeLogin {
                verification_url,
                user_code,
            }))
        }
        LoginAccountResponse::Unknown => Err(Failure::unavailable(
            "Codex did not offer a device-code sign-in",
        )),
    }
}
