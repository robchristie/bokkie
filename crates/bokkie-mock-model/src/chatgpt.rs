//! A stand-in for ChatGPT's sign-in authority and backend, so a real
//! `codex app-server` can hold a synthetic ChatGPT sign-in without any
//! request reaching OpenAI.
//!
//! [`MockChatgpt`] serves, over HTTPS with a certificate authority made for
//! it alone:
//!
//! - the token endpoint (`/oauth/token`), which rotates synthetic tokens and
//!   refuses a refresh token that was already used, as the real one does;
//! - the revoke endpoint (`/oauth/revoke`), which Codex derives from the
//!   token endpoint's URL;
//! - the backend's account check (`/api/codex/accounts/check`), which Codex
//!   needs before it reports a ChatGPT account. Codex talks only to an
//!   HTTPS backend, hence the certificate authority.
//!
//! Every other request is answered 404 and recorded. [`DenyProxy`] is the
//! proxy every app server in a test is given: whatever Codex tries to send
//! to any other host goes to it instead, is refused and is recorded, so a
//! test can show that nothing left the machine.
//!
//! Point an app server at both with [`MockChatgpt::overrides`] and
//! [`MockChatgpt::env`], and give it a sign-in with
//! [`MockChatgpt::sign_in`]. Nothing here is reachable from a production
//! configuration: no production crate depends on this one.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};

/// The ChatGPT account every synthetic sign-in belongs to.
pub const ACCOUNT_ID: &str = "acct-household-test";
/// The email the synthetic sign-in carries.
pub const EMAIL: &str = "household@example.test";
/// The plan the synthetic sign-in carries.
pub const PLAN: &str = "pro";

/// One refresh request the token endpoint answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Refresh {
    /// The `client_id` the app server sent. Tests give each app server its
    /// own (`CODEX_APP_SERVER_LOGIN_CLIENT_ID`) so that callers can be told
    /// apart.
    pub client_id: String,
    /// The generation of the refresh token presented, if this authority
    /// issued it.
    pub presented: Option<u64>,
    pub outcome: RefreshOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshOutcome {
    /// New tokens of this generation were issued.
    Rotated(u64),
    /// The refresh token had already been used: `refresh_token_reused`.
    Reused,
    /// The authority refuses every refresh: `refresh_token_invalidated`.
    Refused,
}

#[derive(Debug, Default)]
struct Authority {
    /// The current generation; each sign-in or refresh adds one.
    generation: u64,
    /// The refresh token of the current generation, until it is used or
    /// revoked.
    refresh_token: Option<String>,
    /// Every access token issued, with its generation and expiry (Unix s).
    access: Vec<(String, u64, i64)>,
    /// How long refreshed access tokens last.
    lifetime: Duration,
    refreshes: Vec<Refresh>,
    revoked: Vec<String>,
    requests: Vec<(String, String)>,
    /// When set, refreshes wait until this many have arrived (or a few
    /// seconds pass) and are then answered in arrival order.
    hold_until: Option<usize>,
    arrived: usize,
    /// Refuse every refresh, as for a sign-in revoked elsewhere.
    refuse: bool,
}

/// The mock authority and backend. Clones share their state.
#[derive(Debug, Clone)]
pub struct MockChatgpt {
    authority: Arc<Mutex<Authority>>,
    arrivals: Arc<tokio::sync::Notify>,
    origin: String,
    ca: PathBuf,
    proxy: DenyProxy,
}

impl MockChatgpt {
    /// Serves on a free local port, writing its certificate authority into
    /// `dir`, with a deny proxy beside it.
    pub async fn start(dir: &Path) -> Self {
        Self::serve(dir, "127.0.0.1:0").await
    }

    /// Serves on `listen`.
    pub async fn serve(dir: &Path, listen: &str) -> Self {
        let (ca_pem, config) = tls_config();
        let ca = dir.join("mock-chatgpt-ca.pem");
        std::fs::write(&ca, ca_pem).unwrap();
        let listener = TcpListener::bind(listen).await.unwrap();
        let address = listener.local_addr().unwrap();
        let mock = Self {
            authority: Arc::new(Mutex::new(Authority {
                lifetime: Duration::from_secs(3600),
                ..Authority::default()
            })),
            arrivals: Arc::default(),
            origin: format!("https://127.0.0.1:{}", address.port()),
            ca,
            proxy: DenyProxy::start().await,
        };
        let app = Router::new().fallback(handle).with_state(mock.clone());
        let tls = TlsListener {
            listener,
            acceptor: tokio_rustls::TlsAcceptor::from(Arc::new(config)),
        };
        tokio::spawn(async move {
            axum::serve(tls, app).await.unwrap();
        });
        mock
    }

    /// The backend's origin, `https://127.0.0.1:<port>`.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// The certificate authority's PEM file.
    pub fn ca_path(&self) -> &Path {
        &self.ca
    }

    pub fn proxy(&self) -> &DenyProxy {
        &self.proxy
    }

    /// The `-c` overrides that point an app server's ChatGPT backend here.
    pub fn overrides(&self) -> Vec<String> {
        vec![format!("chatgpt_base_url=\"{}\"", self.origin)]
    }

    /// The environment that points an app server's token and revoke
    /// endpoints here, trusts this certificate authority, and sends every
    /// other host to the deny proxy.
    pub fn env(&self) -> Vec<(OsString, OsString)> {
        let proxy = self.proxy.url();
        let mut env: Vec<(OsString, OsString)> = vec![
            (
                "CODEX_REFRESH_TOKEN_URL_OVERRIDE".into(),
                format!("{}/oauth/token", self.origin).into(),
            ),
            ("CODEX_CA_CERTIFICATE".into(), self.ca.clone().into()),
        ];
        for name in ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY"] {
            env.push((name.into(), proxy.clone().into()));
            env.push((name.to_lowercase().into(), proxy.clone().into()));
        }
        for name in ["NO_PROXY", "no_proxy"] {
            env.push((name.into(), "127.0.0.1,localhost".into()));
        }
        env
    }

    /// Issues a new generation of tokens whose access token expires in
    /// `expires_in` seconds (negative: already expired) and writes them as
    /// `auth.json` in `codex_home`, as a device-code sign-in would. The
    /// previous generation's refresh token stops working.
    pub fn sign_in(&self, codex_home: &Path, expires_in: i64) {
        write_private(&codex_home.join("auth.json"), &self.auth_json(expires_in));
    }

    /// The `auth.json` text of a new generation of tokens.
    pub fn auth_json(&self, expires_in: i64) -> String {
        let (id_token, access_token, refresh_token) = {
            let mut authority = self.authority.lock().unwrap();
            authority.generation += 1;
            let generation = authority.generation;
            let (id_token, access_token) = mint(generation, unix_now() + expires_in);
            let refresh_token = format!("synthetic-refresh-{generation}");
            authority.refresh_token = Some(refresh_token.clone());
            authority
                .access
                .push((access_token.clone(), generation, unix_now() + expires_in));
            (id_token, access_token, refresh_token)
        };
        serde_json::to_string_pretty(&json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": id_token,
                "access_token": access_token,
                "refresh_token": refresh_token,
                "account_id": ACCOUNT_ID,
            },
            "last_refresh": rfc3339(unix_now()),
        }))
        .unwrap()
    }

    /// How long access tokens issued by a refresh last.
    pub fn set_lifetime(&self, lifetime: Duration) {
        self.authority.lock().unwrap().lifetime = lifetime;
    }

    /// Makes refreshes wait until `count` have arrived (at most ten
    /// seconds), so that simultaneous refreshes really are simultaneous.
    pub fn hold_refreshes_until(&self, count: usize) {
        let mut authority = self.authority.lock().unwrap();
        authority.hold_until = Some(count);
        authority.arrived = 0;
    }

    /// Refuses every refresh from now on.
    pub fn refuse_refreshes(&self) {
        self.authority.lock().unwrap().refuse = true;
    }

    /// The current generation.
    pub fn generation(&self) -> u64 {
        self.authority.lock().unwrap().generation
    }

    /// The generation of an access token this authority issued.
    pub fn generation_of(&self, access_token: &str) -> Option<u64> {
        let authority = self.authority.lock().unwrap();
        authority
            .access
            .iter()
            .find(|(token, _, _)| token == access_token)
            .map(|(_, generation, _)| *generation)
    }

    /// Whether `access_token` was issued here and has not expired. With
    /// `latest_only`, it must also be of the current generation.
    pub fn accepts(&self, access_token: &str, latest_only: bool) -> bool {
        let authority = self.authority.lock().unwrap();
        authority.access.iter().any(|(token, generation, exp)| {
            token == access_token
                && *exp > unix_now()
                && (!latest_only || *generation == authority.generation)
        })
    }

    /// Every refresh answered so far.
    pub fn refreshes(&self) -> Vec<Refresh> {
        self.authority.lock().unwrap().refreshes.clone()
    }

    /// Every token revoked so far.
    pub fn revoked(&self) -> Vec<String> {
        self.authority.lock().unwrap().revoked.clone()
    }

    /// Every request, as method and path with query.
    pub fn requests(&self) -> Vec<(String, String)> {
        self.authority.lock().unwrap().requests.clone()
    }
}

/// `secs` since the Unix epoch in the RFC 3339 form `auth.json` uses.
fn rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    // The civil date of a day count (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn segment(value: &Value) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
}

/// An unsigned JWT-shaped token. Codex reads its claims without checking a
/// signature.
fn jwt(claims: Value) -> String {
    format!(
        "{}.{}.synthetic",
        segment(&json!({"alg": "none", "typ": "JWT"})),
        segment(&claims)
    )
}

/// An ID token and an access token of one generation.
fn mint(generation: u64, exp: i64) -> (String, String) {
    let auth = json!({
        "chatgpt_plan_type": PLAN,
        "chatgpt_user_id": "user-household-test",
        "chatgpt_account_id": ACCOUNT_ID,
    });
    let id_token = jwt(json!({
        "email": EMAIL, "exp": exp, "https://api.openai.com/auth": auth,
    }));
    let access_token = jwt(json!({
        "exp": exp, "generation": generation, "https://api.openai.com/auth": auth,
    }));
    (id_token, access_token)
}

fn write_private(path: &Path, contents: &str) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(contents.as_bytes()).unwrap();
}

fn json_response(status: StatusCode, body: Value) -> Response {
    (status, axum::Json(body)).into_response()
}

async fn handle(State(mock): State<MockChatgpt>, request: Request) -> Response {
    let method = request.method().to_string();
    let path = request.uri().path().to_owned();
    mock.authority
        .lock()
        .unwrap()
        .requests
        .push((method.clone(), request.uri().to_string()));
    let body = axum::body::to_bytes(request.into_body(), 1 << 20)
        .await
        .unwrap_or_default();
    match (method.as_str(), path.as_str()) {
        ("POST", "/oauth/token") => refresh(&mock, body).await,
        ("POST", "/oauth/revoke") => {
            let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let token = request["token"].as_str().unwrap_or_default().to_owned();
            let mut authority = mock.authority.lock().unwrap();
            if authority.refresh_token.as_deref() == Some(token.as_str()) {
                authority.refresh_token = None;
            }
            authority.revoked.push(token);
            json_response(StatusCode::OK, json!({}))
        }
        ("GET", "/api/codex/accounts/check" | "/backend-api/wham/accounts/check") => json_response(
            StatusCode::OK,
            json!({"accounts": [{
                "id": ACCOUNT_ID,
                "workspace_backend_origin": "NO_CONSTRAINT",
                "account_routing_override": "NO_CONSTRAINT",
            }]}),
        ),
        _ => json_response(StatusCode::NOT_FOUND, json!({"error": "not mocked"})),
    }
}

async fn refresh(mock: &MockChatgpt, body: Bytes) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let presented = request["refresh_token"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let client_id = request["client_id"].as_str().unwrap_or_default().to_owned();
    // Hold until enough refreshes have arrived, so that they race for real.
    let wanted = {
        let mut authority = mock.authority.lock().unwrap();
        authority.arrived += 1;
        authority.hold_until
    };
    mock.arrivals.notify_waiters();
    if let Some(wanted) = wanted {
        let _ = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let notified = mock.arrivals.notified();
                if mock.authority.lock().unwrap().arrived >= wanted {
                    break;
                }
                notified.await;
            }
        })
        .await;
    }
    let mut authority = mock.authority.lock().unwrap();
    let presented_generation = presented
        .strip_prefix("synthetic-refresh-")
        .and_then(|g| g.parse().ok());
    let (outcome, response) = if authority.refuse {
        (
            RefreshOutcome::Refused,
            json_response(
                StatusCode::UNAUTHORIZED,
                json!({"error": {"code": "refresh_token_invalidated",
                                 "message": "The refresh token was revoked."}}),
            ),
        )
    } else if authority.refresh_token.as_deref() == Some(presented.as_str()) {
        authority.generation += 1;
        let generation = authority.generation;
        let exp = unix_now() + authority.lifetime.as_secs() as i64;
        let (id_token, access_token) = mint(generation, exp);
        let refresh_token = format!("synthetic-refresh-{generation}");
        authority.refresh_token = Some(refresh_token.clone());
        authority
            .access
            .push((access_token.clone(), generation, exp));
        (
            RefreshOutcome::Rotated(generation),
            json_response(
                StatusCode::OK,
                json!({"id_token": id_token, "access_token": access_token,
                       "refresh_token": refresh_token}),
            ),
        )
    } else {
        (
            RefreshOutcome::Reused,
            json_response(
                StatusCode::UNAUTHORIZED,
                json!({"error": {"code": "refresh_token_reused",
                                 "message": "The refresh token was already used."}}),
            ),
        )
    };
    authority.refreshes.push(Refresh {
        client_id,
        presented: presented_generation,
        outcome,
    });
    response
}

/// Accepts TLS connections for `axum::serve`.
struct TlsListener {
    listener: TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
}

impl axum::serve::Listener for TlsListener {
    type Io = tokio_rustls::server::TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let Ok((stream, address)) = self.listener.accept().await else {
                continue;
            };
            let handshake = self.acceptor.accept(stream);
            if let Ok(Ok(stream)) = tokio::time::timeout(Duration::from_secs(5), handshake).await {
                return (stream, address);
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

/// A certificate authority made for this mock alone, and a server
/// configuration for `127.0.0.1` and `localhost` signed by it. Returns the
/// authority's PEM too.
fn tls_config() -> (String, tokio_rustls::rustls::ServerConfig) {
    use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
    use tokio_rustls::rustls;
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Bokkie test authority");
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let leaf_key = KeyPair::generate().unwrap();
    let leaf_params =
        CertificateParams::new(vec!["127.0.0.1".to_owned(), "localhost".to_owned()]).unwrap();
    let leaf = leaf_params.signed_by(&leaf_key, &issuer).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![leaf.der().clone(), ca_cert.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into()),
        )
        .unwrap();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    (ca_cert.pem(), config)
}

/// A proxy that refuses everything and records what was asked of it.
#[derive(Debug, Clone)]
pub struct DenyProxy {
    attempts: Arc<Mutex<Vec<String>>>,
    address: SocketAddr,
}

impl DenyProxy {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = Self {
            attempts: Arc::default(),
            address: listener.local_addr().unwrap(),
        };
        let attempts = proxy.attempts.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    continue;
                };
                let attempts = attempts.clone();
                tokio::spawn(async move {
                    let mut buffer = vec![0; 4096];
                    let read =
                        tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buffer))
                            .await;
                    let line = match read {
                        Ok(Ok(n)) => String::from_utf8_lossy(&buffer[..n])
                            .lines()
                            .next()
                            .unwrap_or_default()
                            .to_owned(),
                        _ => "<nothing read>".to_owned(),
                    };
                    attempts.lock().unwrap().push(line);
                    let _ = stream
                        .write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n")
                        .await;
                });
            }
        });
        proxy
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// The first line of every request the proxy refused, such as
    /// `CONNECT chatgpt.com:443 HTTP/1.1`.
    pub fn attempts(&self) -> Vec<String> {
        self.attempts.lock().unwrap().clone()
    }

    /// The hosts that were asked for.
    pub fn hosts(&self) -> BTreeSet<String> {
        self.attempts()
            .iter()
            .filter_map(|line| line.split_whitespace().nth(1))
            .map(str::to_owned)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_rfc3339() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_800_000_000), "2027-01-15T08:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
    }
}
