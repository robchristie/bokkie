//! Session cookies, the request guard (`Host` and `Origin` checks), the
//! signed-in-user extractors and sign-in rate limiting.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Mutex;

use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::header::{COOKIE, HOST, ORIGIN};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use bokkie_protocol::SESSION_COOKIE;
use bokkie_protocol::api::{ErrorCode, Role};

use crate::AppState;
use crate::accounts::{self, Account, SESSION_IDLE_MS};
use crate::clock::MINUTE_MS;
use crate::error::Failure;

// ---------------------------------------------------------------------------
// Cookies

/// The `Set-Cookie` value that stores a session token. The cookie is
/// HttpOnly, Secure and SameSite=Lax, so an iOS home-screen launch carries
/// it but scripts and cross-site subrequests do not.
pub fn session_cookie(token: &str) -> HeaderValue {
    let max_age = SESSION_IDLE_MS / 1000;
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}={token}; Path=/; Max-Age={max_age}; HttpOnly; Secure; SameSite=Lax"
    ))
    .expect("tokens are URL-safe base64")
}

/// The `Set-Cookie` value that removes the session cookie.
pub fn clear_session_cookie() -> HeaderValue {
    HeaderValue::from_static("bokkie_session=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax")
}

/// The session token from the `Cookie` headers, if any.
pub fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value.to_owned())
        .filter(|value| !value.is_empty())
}

// ---------------------------------------------------------------------------
// The request guard

/// Rejects requests whose `Host` does not name this gateway, and
/// state-changing requests (anything but `GET`, `HEAD` and `OPTIONS`) and
/// WebSocket upgrades whose `Origin` is not the gateway's own origin.
///
/// `/healthz` is exempt from the `Host` check so that a container health
/// check can reach it by address.
pub async fn guard(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if request.uri().path() != "/healthz" {
        let host = request
            .headers()
            .get(HOST)
            .and_then(|h| h.to_str().ok())
            .or_else(|| request.uri().authority().map(|a| a.as_str()));
        if !host.is_some_and(|h| state.config.accepts_host(h)) {
            return Failure::forbidden("Unknown host").into_response();
        }
    }
    let safe = matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    let upgrade = request.headers().contains_key(axum::http::header::UPGRADE);
    if !safe || upgrade {
        let origin = request.headers().get(ORIGIN).and_then(|o| o.to_str().ok());
        if origin.map(str::to_ascii_lowercase).as_deref() != Some(state.config.origin.as_str()) {
            return Failure::forbidden("Cross-origin requests are not allowed").into_response();
        }
    }
    next.run(request).await
}

// ---------------------------------------------------------------------------
// Extractors

/// The signed-in user. Rejects the request with 401 when there is no live
/// session.
#[derive(Debug, Clone)]
pub struct SignedIn {
    pub session_id: i64,
    pub account: Account,
}

impl SignedIn {
    pub fn id(&self) -> bokkie_protocol::UserId {
        self.account.user.id
    }
}

impl FromRequestParts<AppState> for SignedIn {
    type Rejection = Failure;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Failure> {
        let token = session_token(&parts.headers).ok_or_else(Failure::unauthenticated)?;
        let session = accounts::find_session(&state.db, state.clock.now(), &token)
            .await?
            .ok_or_else(Failure::unauthenticated)?;
        Ok(Self {
            session_id: session.id,
            account: session.account,
        })
    }
}

/// A signed-in admin. Rejects members with 403.
#[derive(Debug, Clone)]
pub struct SignedInAdmin(pub SignedIn);

impl FromRequestParts<AppState> for SignedInAdmin {
    type Rejection = Failure;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Failure> {
        let signed_in = SignedIn::from_request_parts(parts, state).await?;
        if signed_in.account.user.role != Role::Admin {
            return Err(Failure::forbidden("Only an admin can do that"));
        }
        Ok(Self(signed_in))
    }
}

/// The client's address for rate limiting: the last `X-Forwarded-For` entry
/// when the proxy is trusted, else the peer address.
pub fn client_address(parts: &Parts, trust_forwarded_for: bool) -> String {
    if trust_forwarded_for
        && let Some(forwarded) = parts
            .headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .map(str::trim)
            .rfind(|v| !v.is_empty())
    {
        return forwarded.to_owned();
    }
    parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip().to_string())
        .unwrap_or_else(|| "unknown".into())
}

// ---------------------------------------------------------------------------
// Sign-in rate limiting

/// Counts failed sign-ins per username and per client address over a
/// sliding window, and refuses further attempts once either is over its
/// limit. Refused attempts are not checked against the password at all, so
/// they cost no hashing work.
#[derive(Debug, Default)]
pub struct LoginLimiter {
    failures: Mutex<HashMap<String, VecDeque<i64>>>,
}

impl LoginLimiter {
    pub const WINDOW_MS: i64 = 15 * MINUTE_MS;
    pub const PER_USERNAME: usize = 5;
    pub const PER_ADDRESS: usize = 20;
    /// Bounds memory: beyond this many tracked keys, expired keys are
    /// dropped, and if none are expired the oldest key goes.
    const MAX_KEYS: usize = 10_000;

    fn keys(username: &str, address: &str) -> [(String, usize); 2] {
        [
            (
                format!("u:{}", username.trim().to_ascii_lowercase()),
                Self::PER_USERNAME,
            ),
            (format!("a:{address}"), Self::PER_ADDRESS),
        ]
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, VecDeque<i64>>> {
        self.failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether another attempt is allowed now.
    pub fn allows(&self, username: &str, address: &str, now: i64) -> bool {
        let mut failures = self.lock();
        Self::keys(username, address).iter().all(|(key, limit)| {
            let Some(times) = failures.get_mut(key) else {
                return true;
            };
            while times.front().is_some_and(|t| now - t >= Self::WINDOW_MS) {
                times.pop_front();
            }
            times.len() < *limit
        })
    }

    pub fn record_failure(&self, username: &str, address: &str, now: i64) {
        let mut failures = self.lock();
        if failures.len() >= Self::MAX_KEYS {
            failures.retain(|_, times| times.back().is_some_and(|t| now - t < Self::WINDOW_MS));
            if failures.len() >= Self::MAX_KEYS
                && let Some(oldest) = failures
                    .iter()
                    .min_by_key(|(_, times)| times.back().copied())
                    .map(|(key, _)| key.clone())
            {
                failures.remove(&oldest);
            }
        }
        for (key, _) in Self::keys(username, address) {
            failures.entry(key).or_default().push_back(now);
        }
    }

    /// Forgets the username's failures after a successful sign-in. The
    /// address's failures stay, so one good account cannot reset guessing
    /// against others.
    pub fn record_success(&self, username: &str) {
        let [(key, _), _] = Self::keys(username, "");
        self.lock().remove(&key);
    }
}

/// The rejection for a rate-limited sign-in.
pub fn rate_limited() -> Failure {
    Failure::new(
        ErrorCode::RateLimited,
        "Too many sign-in attempts. Try again in a few minutes",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limiter_counts_per_username_and_per_address() {
        let limiter = LoginLimiter::default();
        let now = 1_000_000;
        for _ in 0..LoginLimiter::PER_USERNAME {
            assert!(limiter.allows("Rob", "10.0.0.1", now));
            limiter.record_failure("Rob", "10.0.0.1", now);
        }
        // The username is locked, from any address, case-insensitively.
        assert!(!limiter.allows("rob", "10.0.0.2", now));
        // Other usernames from the same address still work for now.
        assert!(limiter.allows("amy", "10.0.0.1", now));
        // The window slides.
        assert!(limiter.allows("rob", "10.0.0.1", now + LoginLimiter::WINDOW_MS));
    }

    #[test]
    fn an_address_is_limited_across_usernames() {
        let limiter = LoginLimiter::default();
        for i in 0..LoginLimiter::PER_ADDRESS {
            limiter.record_failure(&format!("user{i}"), "10.0.0.9", 0);
        }
        assert!(!limiter.allows("someone-else", "10.0.0.9", 1));
        assert!(limiter.allows("someone-else", "10.0.0.10", 1));
    }

    #[test]
    fn success_clears_only_the_username() {
        let limiter = LoginLimiter::default();
        for _ in 0..LoginLimiter::PER_USERNAME {
            limiter.record_failure("rob", "10.0.0.1", 0);
        }
        limiter.record_success("rob");
        assert!(limiter.allows("rob", "10.0.0.1", 1));
    }

    #[test]
    fn the_client_address_comes_from_the_proxy_only_when_trusted() {
        let request = axum::http::Request::builder()
            .header("x-forwarded-for", "203.0.113.9, 192.168.1.20")
            .extension(ConnectInfo(SocketAddr::from(([172, 30, 52, 97], 41000))))
            .body(())
            .unwrap();
        let (parts, ()) = request.into_parts();
        // The proxy appends the address it saw, so the last entry counts;
        // earlier entries are whatever the client claimed.
        assert_eq!(client_address(&parts, true), "192.168.1.20");
        assert_eq!(client_address(&parts, false), "172.30.52.97");
    }

    #[test]
    fn cookies_parse_from_any_cookie_header() {
        let mut headers = HeaderMap::new();
        headers.append(COOKIE, HeaderValue::from_static("a=1; theme=dark"));
        headers.append(COOKIE, HeaderValue::from_static("bokkie_session=tok; b=2"));
        assert_eq!(session_token(&headers).as_deref(), Some("tok"));
        let cookie = session_cookie("tok");
        let cookie = cookie.to_str().unwrap();
        for attribute in ["HttpOnly", "Secure", "SameSite=Lax", "Path=/", "Max-Age="] {
            assert!(cookie.contains(attribute), "{cookie} lacks {attribute}");
        }
    }
}
