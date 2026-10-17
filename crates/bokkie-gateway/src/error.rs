//! The error type of every route.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bokkie_protocol::api::{ApiError, ErrorCode};

/// A failed request. Internal causes are logged, never returned.
#[derive(Debug)]
pub struct Failure {
    pub code: ErrorCode,
    pub message: String,
}

pub type Result<T, E = Failure> = std::result::Result<T, E>;

impl Failure {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidRequest, message)
    }

    pub fn unauthenticated() -> Self {
        Self::new(ErrorCode::Unauthenticated, "Sign in to continue")
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Forbidden, message)
    }

    /// Another user's resources are always reported this way, so their
    /// existence does not leak.
    pub fn not_found() -> Self {
        Self::new(ErrorCode::NotFound, "Not found")
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Conflict, message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unavailable, message)
    }

    /// Logs `cause` and returns a generic internal error.
    pub fn internal(cause: impl std::fmt::Display) -> Self {
        tracing::error!(%cause, "internal error");
        Self::new(ErrorCode::Internal, "Something went wrong")
    }
}

impl From<sqlx::Error> for Failure {
    fn from(error: sqlx::Error) -> Self {
        Self::internal(format!("database: {error}"))
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.code.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (
            status,
            Json(ApiError {
                code: self.code,
                message: self.message,
            }),
        )
            .into_response()
    }
}

/// A JSON request body. Unlike `axum::Json`, a malformed body is rejected
/// with the gateway's own error shape.
#[derive(Debug, Clone)]
pub struct Body<T>(pub T);

impl<S, T> axum::extract::FromRequest<S> for Body<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned,
{
    type Rejection = Failure;

    async fn from_request(request: axum::extract::Request, state: &S) -> Result<Self, Failure> {
        match <Json<T> as axum::extract::FromRequest<S>>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(Failure::invalid(rejection.body_text())),
        }
    }
}
