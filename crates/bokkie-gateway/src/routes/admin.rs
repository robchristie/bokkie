//! Admin user management and invitations. Every route needs an admin.
//!
//! Admins see accounts, limits and invitations. They do not see members'
//! conversations: whether they should is an open decision in the plan.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use bokkie_protocol::UserId;
use bokkie_protocol::api::{
    AdminUser, AdminUserList, CreateInvitationRequest, CreatedInvitation, InvitationList,
    ResetPasswordRequest, UpdateUserRequest,
};

use crate::AppState;
use crate::accounts;
use crate::auth::SignedInAdmin;
use crate::error::{Body, Failure, Result};

/// `GET /api/v1/admin/users`.
pub async fn list_users(
    State(state): State<AppState>,
    _admin: SignedInAdmin,
) -> Result<Json<AdminUserList>> {
    Ok(Json(AdminUserList {
        users: accounts::admin_list_users(&state.db).await?,
    }))
}

/// `PATCH /api/v1/admin/users/{id}`.
pub async fn update_user(
    State(state): State<AppState>,
    SignedInAdmin(admin): SignedInAdmin,
    Path(id): Path<i64>,
    Body(request): Body<UpdateUserRequest>,
) -> Result<Json<AdminUser>> {
    let updated = accounts::admin_update_user(
        &state.db,
        state.clock.now(),
        admin.id(),
        UserId(id),
        request.display_name.as_deref(),
        request.disabled,
        request.limits,
    )
    .await?;
    if request.disabled == Some(true) {
        // A disabled account's Bots stop at once.
        state.supervisor.stop(UserId(id)).await;
    }
    tracing::info!(admin = %admin.id(), user = id, "updated an account");
    Ok(Json(updated))
}

/// `PUT /api/v1/admin/users/{id}/password`.
pub async fn reset_password(
    State(state): State<AppState>,
    SignedInAdmin(admin): SignedInAdmin,
    Path(id): Path<i64>,
    Body(request): Body<ResetPasswordRequest>,
) -> Result<StatusCode> {
    let _permit = state.hashing.acquire().await.map_err(Failure::internal)?;
    accounts::admin_reset_password(
        &state.db,
        state.clock.now(),
        UserId(id),
        &request.new_password,
    )
    .await?;
    tracing::info!(admin = %admin.id(), user = id, "reset a password");
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/admin/invitations`.
pub async fn create_invitation(
    State(state): State<AppState>,
    SignedInAdmin(admin): SignedInAdmin,
    Body(request): Body<CreateInvitationRequest>,
) -> Result<(StatusCode, Json<CreatedInvitation>)> {
    let (id, code, expires_at) = accounts::admin_create_invitation(
        &state.db,
        state.clock.now(),
        admin.id(),
        request.role,
        request.display_name.as_deref(),
        request.expires_in_hours,
    )
    .await?;
    tracing::info!(admin = %admin.id(), invitation = id, "created an invitation");
    Ok((
        StatusCode::CREATED,
        Json(CreatedInvitation {
            id,
            code,
            role: request.role,
            expires_at,
        }),
    ))
}

/// `GET /api/v1/admin/invitations`.
pub async fn list_invitations(
    State(state): State<AppState>,
    _admin: SignedInAdmin,
) -> Result<Json<InvitationList>> {
    Ok(Json(InvitationList {
        invitations: accounts::admin_list_invitations(&state.db, state.clock.now()).await?,
    }))
}

/// `DELETE /api/v1/admin/invitations/{id}`.
pub async fn withdraw_invitation(
    State(state): State<AppState>,
    _admin: SignedInAdmin,
    Path(id): Path<i64>,
) -> Result<StatusCode> {
    accounts::admin_withdraw_invitation(&state.db, state.clock.now(), id).await?;
    Ok(StatusCode::NO_CONTENT)
}
