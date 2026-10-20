//! Web Push subscriptions: one per session, checked when a device sends
//! it, and ended with its session.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bokkie_protocol::api::{PUSH_ENDPOINT_BYTES, PushSubscriptionRequest};
use bokkie_protocol::{Timestamp, UserId};
use sqlx::Row;
use sqlx::sqlite::{SqliteConnection, SqliteExecutor};
use url::Url;

use crate::error::{Failure, Result};

/// A subscription, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub endpoint: Url,
    /// The device's public key, an uncompressed P-256 point.
    pub p256dh: Vec<u8>,
    /// The device's authentication secret.
    pub auth: Vec<u8>,
}

/// Decodes base64url, with or without padding.
fn decode(value: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(value.trim_end_matches('=')).ok()
}

/// Checks a subscription a device sent: an `https` endpoint on a named
/// host, without credentials or a fragment, of at most
/// [`PUSH_ENDPOINT_BYTES`]; a valid uncompressed P-256 public key; and a
/// 16-byte secret.
pub fn parse(request: &PushSubscriptionRequest) -> std::result::Result<Subscription, String> {
    if request.endpoint.len() > PUSH_ENDPOINT_BYTES {
        return Err(format!(
            "The endpoint must be at most {PUSH_ENDPOINT_BYTES} bytes."
        ));
    }
    let endpoint =
        Url::parse(&request.endpoint).map_err(|_| "The endpoint is not a URL.".to_owned())?;
    if endpoint.scheme() != "https" {
        return Err("The endpoint must be an https URL.".into());
    }
    if !matches!(endpoint.host(), Some(url::Host::Domain(_))) {
        return Err("The endpoint must name its host.".into());
    }
    if !endpoint.username().is_empty() || endpoint.password().is_some() {
        return Err("The endpoint must not carry a user name or password.".into());
    }
    if endpoint.fragment().is_some() {
        return Err("The endpoint must not have a fragment.".into());
    }
    let p256dh = decode(&request.keys.p256dh)
        .filter(|key| key.len() == 65 && key[0] == 4)
        .filter(|key| p256::PublicKey::from_sec1_bytes(key).is_ok())
        .ok_or_else(|| "keys.p256dh must be an uncompressed P-256 public key.".to_owned())?;
    let auth = decode(&request.keys.auth)
        .filter(|auth| auth.len() == 16)
        .ok_or_else(|| "keys.auth must be 16 bytes.".to_owned())?;
    Ok(Subscription {
        endpoint,
        p256dh,
        auth,
    })
}

/// Makes `subscription` the session's, replacing its earlier one and
/// ending any other session's with the same endpoint. Returns false if the
/// session is no longer live.
pub async fn put(
    conn: &mut SqliteConnection,
    now: Timestamp,
    user: UserId,
    session: i64,
    subscription: &Subscription,
) -> Result<bool> {
    sqlx::query("DELETE FROM push_subscriptions WHERE endpoint = ? AND session_id <> ?")
        .bind(subscription.endpoint.as_str())
        .bind(session)
        .execute(&mut *conn)
        .await?;
    let saved = sqlx::query(
        "INSERT INTO push_subscriptions
             (user_id, session_id, endpoint, p256dh, auth, created_at, updated_at)
         SELECT user_id, id, ?2, ?3, ?4, ?1, ?1 FROM sessions
         WHERE id = ?5 AND user_id = ?6 AND revoked_at IS NULL AND expires_at > ?1
         ON CONFLICT (session_id) DO UPDATE
           SET endpoint = excluded.endpoint, p256dh = excluded.p256dh,
               auth = excluded.auth, updated_at = excluded.updated_at",
    )
    .bind(now.0)
    .bind(subscription.endpoint.as_str())
    .bind(&subscription.p256dh)
    .bind(&subscription.auth)
    .bind(session)
    .bind(user.0)
    .execute(&mut *conn)
    .await?;
    Ok(saved.rows_affected() == 1)
}

/// The session's subscription's endpoint, if it has one.
pub async fn endpoint(
    db: impl SqliteExecutor<'_>,
    user: UserId,
    session: i64,
) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT endpoint FROM push_subscriptions WHERE session_id = ? AND user_id = ?",
    )
    .bind(session)
    .bind(user.0)
    .fetch_optional(db)
    .await?)
}

/// Removes the session's subscription. Returns whether it had one.
pub async fn delete(db: impl SqliteExecutor<'_>, user: UserId, session: i64) -> Result<bool> {
    let done = sqlx::query("DELETE FROM push_subscriptions WHERE session_id = ? AND user_id = ?")
        .bind(session)
        .bind(user.0)
        .execute(db)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// A stored subscription a push goes to.
#[derive(Debug, Clone)]
pub struct Target {
    pub id: i64,
    pub endpoint: Url,
    pub p256dh: Vec<u8>,
    pub auth: Vec<u8>,
}

/// The user's subscriptions whose sessions are live.
pub async fn live(
    db: impl SqliteExecutor<'_>,
    now: Timestamp,
    user: UserId,
) -> Result<Vec<Target>> {
    let rows = sqlx::query(
        "SELECT p.id, p.endpoint, p.p256dh, p.auth
         FROM push_subscriptions p
         JOIN sessions s ON s.id = p.session_id AND s.user_id = p.user_id
         JOIN users u ON u.id = p.user_id
         WHERE p.user_id = ? AND s.revoked_at IS NULL AND s.expires_at > ? AND u.disabled = 0
         ORDER BY p.id",
    )
    .bind(user.0)
    .bind(now.0)
    .fetch_all(db)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(Target {
                id: row.get("id"),
                endpoint: Url::parse(row.get("endpoint")).map_err(Failure::internal)?,
                p256dh: row.get("p256dh"),
                auth: row.get("auth"),
            })
        })
        .collect()
}

/// Removes a subscription the push service says is gone.
pub async fn remove(db: impl SqliteExecutor<'_>, target: &Target) -> Result<bool> {
    let done = sqlx::query("DELETE FROM push_subscriptions WHERE id = ? AND endpoint = ?")
        .bind(target.id)
        .bind(target.endpoint.as_str())
        .execute(db)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Removes the subscriptions of sessions that expired. Revoked sessions'
/// went as they were revoked. Returns how many.
pub async fn prune(conn: &mut SqliteConnection, now: Timestamp) -> Result<u64> {
    let done = sqlx::query(
        "DELETE FROM push_subscriptions WHERE session_id IN
           (SELECT id FROM sessions WHERE expires_at <= ? OR revoked_at IS NOT NULL)",
    )
    .bind(now.0)
    .execute(conn)
    .await?;
    Ok(done.rows_affected())
}

#[cfg(test)]
mod tests {
    use bokkie_protocol::api::PushSubscriptionKeys;

    use super::*;

    fn device_key() -> String {
        let secret = p256::SecretKey::from_slice(&[7u8; 32]).unwrap();
        let point = p256::elliptic_curve::sec1::ToEncodedPoint::to_encoded_point(
            &secret.public_key(),
            false,
        );
        URL_SAFE_NO_PAD.encode(point.as_bytes())
    }

    fn request(endpoint: &str, p256dh: &str, auth: &str) -> PushSubscriptionRequest {
        PushSubscriptionRequest {
            endpoint: endpoint.into(),
            keys: PushSubscriptionKeys {
                p256dh: p256dh.into(),
                auth: auth.into(),
            },
        }
    }

    #[test]
    fn a_subscription_needs_an_https_endpoint_and_keys_of_the_right_lengths() {
        let key = device_key();
        let auth = URL_SAFE_NO_PAD.encode([1u8; 16]);
        let good = parse(&request("https://web.push.apple.com/QGx", &key, &auth)).unwrap();
        assert_eq!(good.endpoint.as_str(), "https://web.push.apple.com/QGx");
        assert_eq!(good.p256dh.len(), 65);
        assert_eq!(good.auth, vec![1u8; 16]);
        // Padded base64url is accepted too.
        let padded = format!("{auth}==");
        assert!(parse(&request("https://push.example/x", &key, &padded)).is_ok());

        let long = format!("https://push.example/{}", "a".repeat(PUSH_ENDPOINT_BYTES));
        let mut off_curve = URL_SAFE_NO_PAD.decode(&key).unwrap();
        off_curve[64] ^= 1;
        let off_curve = URL_SAFE_NO_PAD.encode(off_curve);
        let mut compressed = URL_SAFE_NO_PAD.decode(&key).unwrap();
        compressed.truncate(33);
        compressed[0] = 2;
        let compressed = URL_SAFE_NO_PAD.encode(compressed);
        let short_auth = URL_SAFE_NO_PAD.encode([1u8; 15]);
        let long_auth = URL_SAFE_NO_PAD.encode([1u8; 17]);
        for (endpoint, p256dh, auth) in [
            ("http://push.example/x", key.as_str(), auth.as_str()),
            ("wss://push.example/x", &key, &auth),
            ("https://192.0.2.1/x", &key, &auth),
            ("https://[2001:db8::1]/x", &key, &auth),
            ("https://user:pw@push.example/x", &key, &auth),
            ("https://push.example/x#f", &key, &auth),
            ("push.example/x", &key, &auth),
            ("", &key, &auth),
            (&long, &key, &auth),
            ("https://push.example/x", &off_curve, &auth),
            ("https://push.example/x", &compressed, &auth),
            ("https://push.example/x", "not base64!", &auth),
            ("https://push.example/x", "", &auth),
            ("https://push.example/x", &key, &short_auth),
            ("https://push.example/x", &key, &long_auth),
            ("https://push.example/x", &key, ""),
        ] {
            assert!(
                parse(&request(endpoint, p256dh, auth)).is_err(),
                "{endpoint} {p256dh} {auth}"
            );
        }
    }
}
