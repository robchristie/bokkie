//! Web Push: the gateway's public key, and the current session's
//! subscription. A session has at most one subscription, and signing out
//! or revoking the session ends it.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use bokkie_protocol::api::{PushKey, PushSubscriptionRequest, PushSubscriptionState};

use crate::AppState;
use crate::auth::SignedIn;
use crate::error::{Body, Failure, Result};
use crate::push::subscription;

/// `GET /api/v1/push/key`: the VAPID public key a device subscribes with,
/// and whether the gateway sends pushes.
pub async fn key(State(state): State<AppState>, _signed_in: SignedIn) -> Result<Json<PushKey>> {
    let key = state.push.key().map_err(Failure::internal)?;
    Ok(Json(PushKey {
        public_key: key.public_key().to_owned(),
        sending: state.push.sending(),
    }))
}

/// `GET /api/v1/push/subscription`: the current session's subscription.
pub async fn subscription(
    State(state): State<AppState>,
    signed_in: SignedIn,
) -> Result<Json<PushSubscriptionState>> {
    let endpoint = subscription::endpoint(&state.db, signed_in.id(), signed_in.session_id).await?;
    Ok(Json(PushSubscriptionState { endpoint }))
}

/// `PUT /api/v1/push/subscription`: makes a subscription the current
/// session's, replacing its earlier one.
pub async fn subscribe(
    State(state): State<AppState>,
    signed_in: SignedIn,
    Body(request): Body<PushSubscriptionRequest>,
) -> Result<Json<PushSubscriptionState>> {
    let checked = subscription::parse(&request).map_err(Failure::invalid)?;
    let mut tx = crate::db::write(&state.db).await?;
    let saved = subscription::put(
        &mut tx,
        state.clock.now(),
        signed_in.id(),
        signed_in.session_id,
        &checked,
    )
    .await?;
    if !saved {
        return Err(Failure::unauthenticated());
    }
    tx.commit().await?;
    tracing::info!(
        user = %signed_in.id(),
        session = signed_in.session_id,
        service = checked.endpoint.host_str().unwrap_or_default(),
        "a device subscribed to pushes"
    );
    Ok(Json(PushSubscriptionState {
        endpoint: Some(checked.endpoint.to_string()),
    }))
}

/// `DELETE /api/v1/push/subscription`: ends the current session's
/// subscription, if it has one.
pub async fn unsubscribe(State(state): State<AppState>, signed_in: SignedIn) -> Result<StatusCode> {
    if subscription::delete(&state.db, signed_in.id(), signed_in.session_id).await? {
        tracing::info!(user = %signed_in.id(), session = signed_in.session_id, "a device unsubscribed from pushes");
    }
    Ok(StatusCode::NO_CONTENT)
}
