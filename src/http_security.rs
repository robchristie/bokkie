//! Per-process HTTP session and loopback request boundary.

use std::{fmt, net::SocketAddr, sync::Arc};

use axum::{
    Json,
    body::Body,
    extract::State,
    http::{
        HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode,
        header::{ALLOW, CACHE_CONTROL, CONTENT_TYPE, HOST},
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use bokkie_operator_api::{
    API_CONTRACT_VERSION, BOKKIE_BUILD_ID, ServiceIdentity, SessionBootstrap,
};
use serde::Serialize;
use subtle::ConstantTimeEq;
use uuid::Uuid;

pub const MUTATION_TOKEN_HEADER: &str = "x-bokkie-mutation-token";
const TOKEN_BYTES: usize = 32;

#[derive(Clone)]
pub struct ApiRuntime {
    authority: Arc<str>,
    origin: Arc<str>,
    identity: ServiceIdentity,
    secret: Arc<MutationSecret>,
}

impl fmt::Debug for ApiRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApiRuntime")
            .field("authority", &self.authority)
            .field("origin", &self.origin)
            .field("identity", &self.identity)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

struct MutationSecret(String);

impl fmt::Debug for MutationSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MutationSecret([REDACTED])")
    }
}

impl ApiRuntime {
    pub fn new(address: SocketAddr, schema_version: i64) -> Result<Self, getrandom::Error> {
        let mut bytes = [0_u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes)?;
        Ok(Self::from_parts(
            address,
            schema_version,
            Uuid::new_v4().to_string(),
            hex(&bytes),
        ))
    }

    fn from_parts(
        address: SocketAddr,
        schema_version: i64,
        session_id: String,
        mutation_token: String,
    ) -> Self {
        let authority = address.to_string();
        Self {
            origin: format!("http://{authority}").into(),
            authority: authority.into(),
            identity: ServiceIdentity {
                build: BOKKIE_BUILD_ID.to_owned(),
                api_contract_version: API_CONTRACT_VERSION,
                schema_version,
                process_id: std::process::id(),
                session_id,
            },
            secret: Arc::new(MutationSecret(mutation_token)),
        }
    }

    pub fn identity(&self) -> ServiceIdentity {
        self.identity.clone()
    }

    pub fn bootstrap(&self) -> SessionBootstrap {
        SessionBootstrap {
            service: self.identity(),
            mutation_token: self.secret.0.clone(),
        }
    }

    fn token_matches(&self, supplied: &str) -> bool {
        bool::from(self.secret.0.as_bytes().ct_eq(supplied.as_bytes()))
    }

    #[cfg(test)]
    pub(crate) fn deterministic(address: SocketAddr, token_byte: u8, session: &str) -> Self {
        Self::from_parts(
            address,
            bokkie_operator_api::SUPPORTED_SCHEMA_VERSION,
            session.to_owned(),
            hex(&[token_byte; TOKEN_BYTES]),
        )
    }
}

pub async fn enforce(
    State(runtime): State<ApiRuntime>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let headers = request.headers();

    if !single_header_equals(headers, HOST, &runtime.authority) {
        return rejection(
            StatusCode::MISDIRECTED_REQUEST,
            "invalid_host",
            "request Host must match the configured literal loopback authority",
        );
    }

    if let Some(origin) = single_optional_header(headers, &axum::http::header::ORIGIN) {
        let Ok(origin) = origin else {
            return rejection(
                StatusCode::FORBIDDEN,
                "invalid_origin",
                "request Origin is invalid",
            );
        };
        if origin != runtime.origin.as_ref() {
            return rejection(
                StatusCode::FORBIDDEN,
                "invalid_origin",
                "browser Origin must match the configured loopback origin",
            );
        }
    }

    let fetch_site = HeaderName::from_static("sec-fetch-site");
    if let Some(site) = single_optional_header(headers, &fetch_site) {
        let Ok(site) = site else {
            return rejection(
                StatusCode::FORBIDDEN,
                "invalid_browser_context",
                "browser request context is invalid",
            );
        };
        let allowed = site == "same-origin"
            || (site == "none" && matches!(method, Method::GET | Method::HEAD));
        if !allowed {
            return rejection(
                StatusCode::FORBIDDEN,
                "invalid_browser_context",
                "browser request must be same-origin",
            );
        }
    }

    if !matches!(method, Method::GET | Method::HEAD | Method::POST) {
        let mut response = rejection(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "HTTP method is not allowed",
        );
        response
            .headers_mut()
            .insert(ALLOW, HeaderValue::from_static("GET, HEAD, POST"));
        return response;
    }

    if method == Method::POST {
        if !has_json_content_type(headers) {
            return rejection(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "json_content_type_required",
                "HTTP mutations require Content-Type: application/json",
            );
        }
        let token_header = HeaderName::from_static(MUTATION_TOKEN_HEADER);
        let supplied = match single_optional_header(headers, &token_header) {
            Some(Ok(value)) => value,
            Some(Err(())) => {
                return rejection(
                    StatusCode::FORBIDDEN,
                    "mutation_token_invalid",
                    "mutation token is invalid; acquire a current bootstrap session",
                );
            }
            None => {
                return rejection(
                    StatusCode::FORBIDDEN,
                    "mutation_token_required",
                    "HTTP mutations require the current bootstrap session token",
                );
            }
        };
        if !runtime.token_matches(supplied) {
            return rejection(
                StatusCode::FORBIDDEN,
                "mutation_token_invalid",
                "mutation token is invalid; acquire a current bootstrap session",
            );
        }
    }

    let mut response = next.run(request).await;
    response.headers_mut().insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    response
}

fn single_header_equals(headers: &HeaderMap, name: HeaderName, expected: &str) -> bool {
    matches!(single_optional_header(headers, &name), Some(Ok(value)) if value == expected)
}

fn single_optional_header<'a>(
    headers: &'a HeaderMap,
    name: &HeaderName,
) -> Option<Result<&'a str, ()>> {
    let mut values = headers.get_all(name).iter();
    let first = values.next()?;
    if values.next().is_some() {
        return Some(Err(()));
    }
    Some(first.to_str().map_err(|_| ()))
}

fn has_json_content_type(headers: &HeaderMap) -> bool {
    matches!(
        single_optional_header(headers, &CONTENT_TYPE),
        Some(Ok(value)) if value
            .split(';')
            .next()
            .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
    )
}

#[derive(Serialize)]
struct RejectionEnvelope {
    error: RejectionBody,
}

#[derive(Serialize)]
struct RejectionBody {
    code: &'static str,
    message: &'static str,
}

fn rejection(status: StatusCode, code: &'static str, message: &'static str) -> Response {
    let mut response = (
        status,
        Json(RejectionEnvelope {
            error: RejectionBody { code, message },
        }),
    )
        .into_response();
    response.headers_mut().insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    response
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[usize::from(byte >> 4)] as char);
        encoded.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

pub fn bootstrap_response(runtime: &ApiRuntime) -> Response {
    let mut response = (StatusCode::OK, Json(runtime.bootstrap())).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_debug_and_identity_serialisation_do_not_expose_secret() {
        let runtime =
            ApiRuntime::deterministic("127.0.0.1:7744".parse().unwrap(), 0xab, "session-one");
        let token = runtime.bootstrap().mutation_token;
        assert_eq!(token.len(), 64);
        assert!(!format!("{runtime:?}").contains(&token));
        assert!(!format!("{:?}", runtime.bootstrap()).contains(&token));
        assert!(
            !serde_json::to_string(&runtime.identity())
                .unwrap()
                .contains(&token)
        );
    }

    #[test]
    fn different_runtime_instances_rotate_tokens_and_sessions() {
        let address = "127.0.0.1:7744".parse().unwrap();
        let first = ApiRuntime::new(address, 7).unwrap().bootstrap();
        let second = ApiRuntime::new(address, 7).unwrap().bootstrap();
        assert_ne!(first.mutation_token, second.mutation_token);
        assert_ne!(first.service.session_id, second.service.session_id);
    }
}
