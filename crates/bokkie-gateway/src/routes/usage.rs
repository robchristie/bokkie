//! The usage meter.

use axum::Json;
use axum::extract::{Path, State};
use bokkie_protocol::UserId;
use bokkie_protocol::api::{Allowance, Usage};

use crate::AppState;
use crate::accounts;
use crate::auth::{SignedIn, SignedInAdmin};
use crate::error::{Failure, Result};

/// `GET /api/v1/usage`: the shared subscription's windows and the signed-in
/// user's own allowance.
pub async fn usage(State(state): State<AppState>, signed_in: SignedIn) -> Result<Json<Usage>> {
    Ok(Json(state.conversations.usage(signed_in.id()).await?))
}

/// `GET /api/v1/admin/users/{id}/usage`: another account's allowance, for an
/// admin. It shows counts only, never conversations.
pub async fn admin_usage(
    State(state): State<AppState>,
    _admin: SignedInAdmin,
    Path(id): Path<i64>,
) -> Result<Json<Allowance>> {
    accounts::account(&state.db, UserId(id))
        .await?
        .ok_or_else(Failure::not_found)?;
    Ok(Json(state.conversations.allowance(UserId(id)).await?))
}
