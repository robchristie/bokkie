//! Wire-level JSON-RPC messages and errors.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON-RPC request identifier. The app server uses integers for its own
/// requests but the protocol also allows strings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Integer(i64),
    String(String),
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Integer(id) => write!(f, "{id}"),
            Self::String(id) => f.write_str(id),
        }
    }
}

/// The error object of a JSON-RPC error response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("JSON-RPC error {code}: {message}")]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// The standard code for a method the receiver does not implement.
    pub const METHOD_NOT_FOUND: i64 = -32601;

    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: Self::METHOD_NOT_FOUND,
            message: format!("method not supported by this client: {method}"),
            data: None,
        }
    }
}

/// Errors from the client.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("app-server I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("app-server message was not valid JSON for its type: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("the app-server connection closed")]
    Closed,
}

/// One decoded line from the app server.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Request {
        id: RequestId,
        method: String,
        params: Option<Value>,
    },
    Notification {
        method: String,
        params: Option<Value>,
    },
    Response {
        id: RequestId,
        result: Value,
    },
    Error {
        id: Option<RequestId>,
        error: RpcError,
    },
}

#[derive(Deserialize)]
struct RawMessage {
    id: Option<RequestId>,
    method: Option<String>,
    params: Option<Value>,
    result: Option<Value>,
    error: Option<RpcError>,
}

impl Message {
    /// Decodes one line of the stream.
    pub fn decode(line: &str) -> Result<Self, serde_json::Error> {
        let raw: RawMessage = serde_json::from_str(line)?;
        Ok(match (raw.id, raw.method, raw.error) {
            (id, _, Some(error)) => Self::Error { id, error },
            (Some(id), Some(method), None) => Self::Request {
                id,
                method,
                params: raw.params,
            },
            (None, Some(method), None) => Self::Notification {
                method,
                params: raw.params,
            },
            (Some(id), None, None) => Self::Response {
                id,
                // A response must carry `result`; treat its absence as null so
                // the typed decode reports the mismatch.
                result: raw.result.unwrap_or(Value::Null),
            },
            (None, None, None) => {
                return Err(serde::de::Error::custom(
                    "message has neither an id nor a method",
                ));
            }
        })
    }

    /// Encodes the message as one line, without the trailing newline.
    pub fn encode(&self) -> String {
        let value = match self {
            Self::Request { id, method, params } => {
                let mut object = serde_json::json!({ "id": id, "method": method });
                if let Some(params) = params {
                    object["params"] = params.clone();
                }
                object
            }
            Self::Notification { method, params } => {
                let mut object = serde_json::json!({ "method": method });
                if let Some(params) = params {
                    object["params"] = params.clone();
                }
                object
            }
            Self::Response { id, result } => serde_json::json!({ "id": id, "result": result }),
            Self::Error { id, error } => serde_json::json!({ "id": id, "error": error }),
        };
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_each_message_kind() {
        assert_eq!(
            Message::decode(r#"{"id":1,"result":{"ok":true}}"#).unwrap(),
            Message::Response {
                id: RequestId::Integer(1),
                result: serde_json::json!({"ok": true}),
            }
        );
        assert_eq!(
            Message::decode(r#"{"id":"a","method":"item/tool/call","params":{}}"#).unwrap(),
            Message::Request {
                id: RequestId::String("a".into()),
                method: "item/tool/call".into(),
                params: Some(serde_json::json!({})),
            }
        );
        assert_eq!(
            Message::decode(r#"{"method":"turn/started"}"#).unwrap(),
            Message::Notification {
                method: "turn/started".into(),
                params: None,
            }
        );
        assert!(matches!(
            Message::decode(r#"{"id":2,"error":{"code":-32600,"message":"bad"}}"#).unwrap(),
            Message::Error { id: Some(RequestId::Integer(2)), error } if error.code == -32600
        ));
        assert!(Message::decode(r#"{"params":{}}"#).is_err());
    }

    #[test]
    fn encoding_round_trips() {
        let messages = [
            Message::Request {
                id: RequestId::Integer(7),
                method: "thread/start".into(),
                params: Some(serde_json::json!({"ephemeral": true})),
            },
            Message::Request {
                id: RequestId::Integer(8),
                method: "account/rateLimits/read".into(),
                params: None,
            },
            Message::Notification {
                method: "initialized".into(),
                params: None,
            },
            Message::Response {
                id: RequestId::String("x".into()),
                result: serde_json::json!({}),
            },
        ];
        for message in messages {
            assert_eq!(Message::decode(&message.encode()).unwrap(), message);
        }
    }
}
