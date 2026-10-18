//! What the gateway sends and what the client sends back.
//!
//! The event WebSocket carries one JSON [`ServerFrame`] per text message, and
//! the client answers with [`ClientFrame`]s. Every HTTP command answers with
//! its JSON response body or, on failure, an [`ApiError`]. Decoding never
//! panics: anything the client cannot read becomes an error it can show or
//! act on.

use bokkie_protocol::api::{ApiError, ErrorCode};
use bokkie_protocol::events::{ClientFrame, ServerFrame};
use serde::de::DeserializeOwned;

/// A WebSocket message that is not a frame this client understands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameError(pub String);

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unreadable event frame: {}", self.0)
    }
}

impl std::error::Error for FrameError {}

/// Decode one text message from the event WebSocket.
pub fn decode_frame(text: &str) -> Result<ServerFrame, FrameError> {
    serde_json::from_str(text).map_err(|error| FrameError(error.to_string()))
}

/// Encode one frame for the event WebSocket.
pub fn encode_frame(frame: &ClientFrame) -> String {
    serde_json::to_string(frame).expect("client frames always serialise")
}

/// A command that failed, with a message that is safe to show the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandError {
    /// The HTTP status, or 0 when no response arrived.
    pub status: u16,
    pub code: ErrorCode,
    pub message: String,
}

impl CommandError {
    /// No response arrived: the network or the server is down.
    pub fn unreachable() -> Self {
        Self {
            status: 0,
            code: ErrorCode::Unavailable,
            message: "Bokkie's server could not be reached. Check the connection and try again"
                .into(),
        }
    }

    /// The session is missing, expired or revoked, or a sign-in failed.
    pub fn is_unauthenticated(&self) -> bool {
        self.code == ErrorCode::Unauthenticated
    }

    /// A response arrived but did not have the shape its status promised.
    fn unreadable(status: u16) -> Self {
        Self {
            status,
            code: ErrorCode::Internal,
            message: format!(
                "Bokkie's server sent a response this client cannot read (HTTP {status})"
            ),
        }
    }

    /// The error for a failed response whose body is not the gateway's
    /// error shape, as when a proxy in front of it answers.
    fn from_status(status: u16) -> Self {
        let (code, message) = match status {
            400 => (ErrorCode::InvalidRequest, "The request was not valid"),
            401 => (ErrorCode::Unauthenticated, "Sign in to continue"),
            403 => (ErrorCode::Forbidden, "That is not allowed"),
            404 => (ErrorCode::NotFound, "Not found"),
            409 => (
                ErrorCode::Conflict,
                "That conflicts with a change made elsewhere",
            ),
            429 => (ErrorCode::RateLimited, "Too many attempts. Try again later"),
            502..=504 => (
                ErrorCode::Unavailable,
                "Bokkie's server is not available. Try again shortly",
            ),
            _ => (ErrorCode::Internal, "Something went wrong"),
        };
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CommandError {}

fn success(status: u16) -> bool {
    (200..300).contains(&status)
}

/// Decode a failed response's body, which should be an [`ApiError`].
pub fn decode_failure(status: u16, body: &str) -> CommandError {
    match serde_json::from_str::<ApiError>(body) {
        Ok(error) => CommandError {
            status,
            code: error.code,
            message: error.message,
        },
        Err(_) => CommandError::from_status(status),
    }
}

/// Decode a response that carries a JSON body on success.
pub fn decode_response<T: DeserializeOwned>(status: u16, body: &str) -> Result<T, CommandError> {
    if !success(status) {
        return Err(decode_failure(status, body));
    }
    serde_json::from_str(body).map_err(|_| CommandError::unreadable(status))
}

/// Decode a response whose success carries no body worth reading, such as
/// signing out.
pub fn decode_empty(status: u16, body: &str) -> Result<(), CommandError> {
    if success(status) {
        Ok(())
    } else {
        Err(decode_failure(status, body))
    }
}

#[cfg(test)]
mod tests {
    use bokkie_protocol::api::{BotList, SessionResponse};
    use bokkie_protocol::events::Seq;

    use super::*;

    #[test]
    fn frames_decode_from_the_documented_shapes() {
        assert_eq!(
            decode_frame(r#"{"type":"reset","after":7}"#),
            Ok(ServerFrame::Reset { after: Seq(7) })
        );
        assert_eq!(
            decode_frame(r#"{"type":"live","latest":0}"#),
            Ok(ServerFrame::Live { latest: Seq(0) })
        );
        assert_eq!(decode_frame(r#"{"type":"pong"}"#), Ok(ServerFrame::Pong));
        for text in [
            "",
            "null",
            "{}",
            r#"{"type":"surprise"}"#,
            r#"{"type":"live"}"#,
        ] {
            assert!(decode_frame(text).is_err(), "{text}");
        }
    }

    #[test]
    fn client_frames_encode_as_the_gateway_reads_them() {
        assert_eq!(
            encode_frame(&ClientFrame::Resume { after: None }),
            r#"{"type":"resume","after":null}"#
        );
        assert_eq!(
            encode_frame(&ClientFrame::Resume {
                after: Some(Seq(42))
            }),
            r#"{"type":"resume","after":42}"#
        );
        assert_eq!(encode_frame(&ClientFrame::Ping), r#"{"type":"ping"}"#);
    }

    #[test]
    fn failures_carry_the_gateways_message() {
        let body = r#"{"code":"rate_limited","message":"Too many sign-in attempts. Try again in a few minutes"}"#;
        let error = decode_response::<SessionResponse>(429, body).unwrap_err();
        assert_eq!(error.code, ErrorCode::RateLimited);
        assert_eq!(
            error.message,
            "Too many sign-in attempts. Try again in a few minutes"
        );
        assert!(!error.is_unauthenticated());
        let error = decode_empty(401, r#"{"code":"unauthenticated","message":"No"}"#).unwrap_err();
        assert!(error.is_unauthenticated());
    }

    #[test]
    fn a_failure_without_the_error_shape_falls_back_to_its_status() {
        let error = decode_response::<BotList>(502, "<html>Bad Gateway</html>").unwrap_err();
        assert_eq!((error.status, error.code), (502, ErrorCode::Unavailable));
        let error = decode_response::<BotList>(401, "").unwrap_err();
        assert!(error.is_unauthenticated());
        let error = decode_response::<BotList>(418, "").unwrap_err();
        assert_eq!(error.code, ErrorCode::Internal);
    }

    #[test]
    fn an_unreadable_success_is_an_error_not_a_panic() {
        let error = decode_response::<BotList>(200, r#"{"bots":"many"}"#).unwrap_err();
        assert_eq!((error.status, error.code), (200, ErrorCode::Internal));
        assert_eq!(decode_empty(204, ""), Ok(()));
        assert_eq!(CommandError::unreachable().status, 0);
    }
}
