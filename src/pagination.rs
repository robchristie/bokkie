//! Stable, bounded keyset pagination for durable read projections.
//!
//! A page cursor is an opaque read identity. It binds the projection scope,
//! the first page's global event-envelope watermark, the projection-specific
//! upper key and the last returned key. It is deliberately separate from the
//! per-process [`bokkie_operator_api::ServiceIdentity`], execution-lane state,
//! failure disposition and mutation [`bokkie_operator_api::ActionPrecondition`].

use rusqlite::Connection;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::StoreError;

pub const DEFAULT_PAGE_SIZE: usize = 100;
pub const MAX_PAGE_SIZE: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadPage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    /// Exact global event-envelope sequence captured by the first page.
    pub watermark: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CursorPayload<K> {
    version: u8,
    scope: String,
    watermark: i64,
    after: K,
    upper: K,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CursorState<K> {
    pub watermark: i64,
    pub after: K,
    pub upper: K,
}

pub fn page_limit(requested: Option<usize>) -> Result<usize, StoreError> {
    let limit = requested.unwrap_or(DEFAULT_PAGE_SIZE);
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(StoreError::Invalid(format!(
            "page limit {limit} is outside 1..={MAX_PAGE_SIZE}"
        )));
    }
    Ok(limit)
}

pub(crate) fn watermark(connection: &Connection) -> Result<i64, StoreError> {
    connection
        .query_row(
            "SELECT coalesce(max(sequence), 0) FROM event_envelopes",
            [],
            |row| row.get(0),
        )
        .map_err(StoreError::from)
}

pub(crate) fn initial_watermark(current: i64, requested: Option<i64>) -> Result<i64, StoreError> {
    match requested {
        Some(requested) if requested != current => Err(StoreError::Invalid(format!(
            "requested watermark {requested} does not match current watermark {current} without a cursor"
        ))),
        Some(requested) => Ok(requested),
        None => Ok(current),
    }
}

pub(crate) fn decode_cursor<K: DeserializeOwned>(
    encoded: &str,
    expected_scope: &str,
    requested_watermark: Option<i64>,
    current_watermark: i64,
) -> Result<CursorState<K>, StoreError> {
    let bytes =
        decode_hex(encoded).ok_or_else(|| invalid_cursor("invalid hexadecimal encoding"))?;
    let separator = bytes
        .len()
        .checked_sub(33)
        .filter(|index| bytes.get(*index) == Some(&b'.'))
        .ok_or_else(|| invalid_cursor("missing integrity digest"))?;
    let (json, suffix) = bytes.split_at(separator);
    let digest = suffix
        .get(1..)
        .ok_or_else(|| invalid_cursor("missing integrity digest"))?;
    let expected = Sha256::digest(json);
    if digest != expected.as_slice() {
        return Err(invalid_cursor("integrity validation failed"));
    }
    let payload: CursorPayload<K> =
        serde_json::from_slice(json).map_err(|_| invalid_cursor("payload is not valid"))?;
    if payload.version != 1 {
        return Err(invalid_cursor("version is unsupported"));
    }
    if payload.scope != expected_scope {
        return Err(invalid_cursor("scope does not match this projection"));
    }
    if requested_watermark.is_some_and(|value| value != payload.watermark) {
        return Err(invalid_cursor("watermark does not match the cursor"));
    }
    if payload.watermark > current_watermark {
        return Err(StoreError::ProjectionGap(format!(
            "cursor watermark {} is ahead of retained watermark {current_watermark}",
            payload.watermark
        )));
    }
    Ok(CursorState {
        watermark: payload.watermark,
        after: payload.after,
        upper: payload.upper,
    })
}

/// Mutable projections cannot be reconstructed at an older event watermark;
/// continuing after any intervening write would mix projection revisions.
pub(crate) fn decode_cursor_exact<K: DeserializeOwned>(
    encoded: &str,
    expected_scope: &str,
    requested_watermark: Option<i64>,
    current_watermark: i64,
) -> Result<CursorState<K>, StoreError> {
    let state = decode_cursor(
        encoded,
        expected_scope,
        requested_watermark,
        current_watermark,
    )?;
    if state.watermark != current_watermark {
        return Err(StoreError::ProjectionGap(format!(
            "cursor watermark {} no longer matches current watermark {current_watermark}",
            state.watermark
        )));
    }
    Ok(state)
}

pub(crate) fn encode_cursor<K: Serialize + Clone>(
    scope: &str,
    watermark: i64,
    after: K,
    upper: K,
) -> Result<String, StoreError> {
    let json = serde_json::to_vec(&CursorPayload {
        version: 1,
        scope: scope.to_owned(),
        watermark,
        after,
        upper,
    })
    .map_err(|error| StoreError::Invalid(format!("cursor serialisation failed: {error}")))?;
    let digest = Sha256::digest(&json);
    let mut bytes = json;
    bytes.push(b'.');
    bytes.extend_from_slice(&digest);
    Ok(encode_hex(&bytes))
}

fn invalid_cursor(reason: &str) -> StoreError {
    StoreError::Invalid(format!("invalid page cursor: {reason}"))
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn decode_hex(encoded: &str) -> Option<Vec<u8>> {
    if encoded.len() % 2 != 0 {
        return None;
    }
    encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| Some((hex_digit(pair[0])? << 4) | hex_digit(pair[1])?))
        .collect()
}

fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_rejects_tampering_scope_and_watermark_mismatch() {
        let cursor = encode_cursor("obligations", 7, (1_i64, "a"), (9_i64, "z")).unwrap();
        let state = decode_cursor::<(i64, String)>(&cursor, "obligations", Some(7), 8).unwrap();
        assert_eq!(state.after, (1, "a".to_owned()));

        let mut tampered = cursor.clone();
        tampered.replace_range(4..5, if &cursor[4..5] == "0" { "1" } else { "0" });
        assert!(matches!(
            decode_cursor::<(i64, String)>(&tampered, "obligations", None, 8),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            decode_cursor::<(i64, String)>(&cursor, "attempts:x", None, 8),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            decode_cursor::<(i64, String)>(&cursor, "obligations", Some(6), 8),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            decode_cursor::<(i64, String)>(&cursor, "obligations", None, 6),
            Err(StoreError::ProjectionGap(_))
        ));
        assert!(matches!(
            decode_cursor_exact::<(i64, String)>(&cursor, "obligations", None, 8),
            Err(StoreError::ProjectionGap(_))
        ));
    }

    #[test]
    fn limits_have_a_bounded_default() {
        assert_eq!(page_limit(None).unwrap(), DEFAULT_PAGE_SIZE);
        assert!(page_limit(Some(0)).is_err());
        assert!(page_limit(Some(MAX_PAGE_SIZE + 1)).is_err());
    }
}
