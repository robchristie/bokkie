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
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax"
    ))
    .expect("a valid header value")
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

/// Counts password and invitation-code attempts per key over a sliding
/// window, and refuses further attempts once a key is at its limit. Refused
/// attempts are not checked at all, so they cost no hashing work.
///
/// Checking and counting happen together under one lock: [`LoginLimiter::begin`]
/// counts the attempt as a failure up front, and [`LoginLimiter::succeeded`]
/// takes it back. Concurrent attempts therefore cannot all slip past a limit
/// before any of them fails.
#[derive(Debug, Default)]
pub struct LoginLimiter {
    failures: Mutex<HashMap<String, VecDeque<i64>>>,
}

/// What an attempt is limited by.
#[derive(Debug, Clone, Copy)]
pub enum Attempting<'a> {
    /// A sign-in: limited per username and per client address.
    SignIn { username: &'a str, address: &'a str },
    /// An invitation code: limited per client address, sharing the sign-in
    /// address limit.
    Invitation { address: &'a str },
    /// The current-password check of a password change: limited per user.
    PasswordChange { user: bokkie_protocol::UserId },
}

/// One counted attempt. Pass it to [`LoginLimiter::succeeded`] if it
/// succeeds; dropping it leaves it counted as a failure.
#[derive(Debug)]
#[must_use]
pub struct Attempt {
    /// Each key, and whether success clears the key entirely (an account's
    /// own key) or only takes back this attempt (a shared address key).
    keys: Vec<(String, bool)>,
    at: i64,
}

impl LoginLimiter {
    pub const WINDOW_MS: i64 = 15 * MINUTE_MS;
    pub const PER_ACCOUNT: usize = 5;
    pub const PER_ADDRESS: usize = 20;
    /// Bounds memory: beyond this many tracked keys, expired keys are
    /// dropped, and if none are expired the oldest key goes.
    const MAX_KEYS: usize = 10_000;

    /// Each key with its limit, and whether success clears it.
    fn keys(attempting: Attempting<'_>) -> Vec<(String, usize, bool)> {
        let address = |a: &str| (format!("a:{a}"), Self::PER_ADDRESS, false);
        match attempting {
            Attempting::SignIn {
                username,
                address: a,
            } => vec![
                (
                    format!("u:{}", username.trim().to_ascii_lowercase()),
                    Self::PER_ACCOUNT,
                    true,
                ),
                address(a),
            ],
            Attempting::Invitation { address: a } => vec![address(a)],
            Attempting::PasswordChange { user } => {
                vec![(format!("p:{user}"), Self::PER_ACCOUNT, true)]
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, VecDeque<i64>>> {
        self.failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Counts an attempt, or refuses it when any of its keys is at its limit.
    pub fn begin(&self, attempting: Attempting<'_>, now: i64) -> Option<Attempt> {
        let keys = Self::keys(attempting);
        let mut failures = self.lock();
        for (key, limit, _) in &keys {
            if let Some(times) = failures.get_mut(key) {
                while times.front().is_some_and(|t| now - t >= Self::WINDOW_MS) {
                    times.pop_front();
                }
                if times.len() >= *limit {
                    return None;
                }
            }
        }
        if failures.len() + keys.len() > Self::MAX_KEYS {
            failures.retain(|_, times| times.back().is_some_and(|t| now - t < Self::WINDOW_MS));
            while failures.len() + keys.len() > Self::MAX_KEYS {
                let Some(oldest) = failures
                    .iter()
                    .min_by_key(|(_, times)| times.back().copied())
                    .map(|(key, _)| key.clone())
                else {
                    break;
                };
                failures.remove(&oldest);
            }
        }
        for (key, _, _) in &keys {
            failures.entry(key.clone()).or_default().push_back(now);
        }
        Some(Attempt {
            keys: keys
                .into_iter()
                .map(|(key, _, clear)| (key, clear))
                .collect(),
            at: now,
        })
    }

    /// Takes back a successful attempt. An account's own key forgets all
    /// its failures; a shared address key forgets only this attempt, so one
    /// good sign-in cannot reset guessing against other accounts.
    pub fn succeeded(&self, attempt: Attempt) {
        let mut failures = self.lock();
        for (key, clear) in attempt.keys {
            if clear {
                failures.remove(&key);
            } else if let Some(times) = failures.get_mut(&key)
                && let Some(index) = times.iter().position(|t| *t == attempt.at)
            {
                times.remove(index);
            }
        }
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

    fn sign_in<'a>(username: &'a str, address: &'a str) -> Attempting<'a> {
        Attempting::SignIn { username, address }
    }

    #[test]
    fn the_limiter_counts_per_username_and_per_address() {
        let limiter = LoginLimiter::default();
        let now = 1_000_000;
        for _ in 0..LoginLimiter::PER_ACCOUNT {
            drop(limiter.begin(sign_in("Rob", "10.0.0.1"), now).unwrap());
        }
        // The username is locked, from any address, case-insensitively.
        assert!(limiter.begin(sign_in("rob", "10.0.0.2"), now).is_none());
        // Other usernames from the same address still work for now.
        assert!(limiter.begin(sign_in("amy", "10.0.0.1"), now).is_some());
        // The window slides.
        let later = now + LoginLimiter::WINDOW_MS;
        assert!(limiter.begin(sign_in("rob", "10.0.0.1"), later).is_some());
    }

    #[test]
    fn an_address_is_limited_across_usernames_and_invitations() {
        let limiter = LoginLimiter::default();
        for i in 0..LoginLimiter::PER_ADDRESS - 1 {
            drop(limiter.begin(sign_in(&format!("user{i}"), "10.0.0.9"), 0));
        }
        drop(limiter.begin(
            Attempting::Invitation {
                address: "10.0.0.9",
            },
            0,
        ));
        assert!(
            limiter
                .begin(sign_in("someone-else", "10.0.0.9"), 1)
                .is_none()
        );
        assert!(
            limiter
                .begin(
                    Attempting::Invitation {
                        address: "10.0.0.9"
                    },
                    1
                )
                .is_none()
        );
        assert!(
            limiter
                .begin(sign_in("someone-else", "10.0.0.10"), 1)
                .is_some()
        );
    }

    #[test]
    fn attempts_count_before_they_finish() {
        // Concurrent attempts each hold a counted attempt while they hash,
        // so the limit holds even if none has failed yet.
        let limiter = LoginLimiter::default();
        let held: Vec<_> = (0..LoginLimiter::PER_ACCOUNT)
            .map(|_| limiter.begin(sign_in("rob", "10.0.0.1"), 0).unwrap())
            .collect();
        assert!(limiter.begin(sign_in("rob", "10.0.0.1"), 0).is_none());
        drop(held);
    }

    #[test]
    fn success_clears_the_account_but_only_its_own_address_attempt() {
        let limiter = LoginLimiter::default();
        for _ in 0..LoginLimiter::PER_ACCOUNT - 1 {
            drop(limiter.begin(sign_in("rob", "10.0.0.1"), 0));
        }
        let good = limiter.begin(sign_in("rob", "10.0.0.1"), 1).unwrap();
        limiter.succeeded(good);
        for _ in 0..LoginLimiter::PER_ACCOUNT {
            limiter.succeeded(limiter.begin(sign_in("rob", "10.0.0.1"), 2).unwrap());
        }
        // The four failures still count against the address.
        let failures = limiter.lock();
        assert_eq!(failures.get("a:10.0.0.1").map(VecDeque::len), Some(4));
        assert!(!failures.contains_key("u:rob"));
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
        headers.append(
            COOKIE,
            HeaderValue::from_static("bokkie_session=old; __Host-bokkie_session=tok; b=2"),
        );
        assert_eq!(session_token(&headers).as_deref(), Some("tok"));
        let cookie = session_cookie("tok");
        let cookie = cookie.to_str().unwrap();
        for attribute in ["HttpOnly", "Secure", "SameSite=Lax", "Path=/", "Max-Age="] {
            assert!(cookie.contains(attribute), "{cookie} lacks {attribute}");
        }
    }
}
