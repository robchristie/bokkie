//! A gateway on a temporary database, driven in-process, with a fake app
//! server per user.

#![allow(dead_code)]

pub mod fake_codex;
pub mod mock_model;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use bokkie_gateway::clock::Clock;
use bokkie_gateway::codex::{RuntimeEvent, Supervisor, catalog};
use bokkie_gateway::config::{Config, PublicOrigin};
use bokkie_gateway::{AppState, accounts, db, router};
use bokkie_protocol::api::{CreateInvitationRequest, Role, SessionResponse};
use http_body_util::BodyExt;
use serde::Serialize;
use serde_json::Value;
use tower::ServiceExt;

pub const ORIGIN: &str = "https://bokkie.test";
pub const HOST: &str = "bokkie.test";
pub const PASSWORD: &str = "a long test password";

pub struct Gateway {
    pub app: Router,
    pub state: AppState,
    pub fake: fake_codex::FakeCodex,
    /// What the app servers reported, for tests that route it themselves.
    pub events: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<RuntimeEvent>>,
    pub dir: tempfile::TempDir,
}

/// A response, decoded.
#[derive(Debug)]
pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Value,
}

impl Reply {
    /// The session cookie set by this response, as a `Cookie` header value.
    pub fn cookie(&self) -> String {
        let set = self
            .headers
            .get(header::SET_COOKIE)
            .expect("a Set-Cookie header")
            .to_str()
            .unwrap();
        set.split(';').next().unwrap().to_owned()
    }

    pub fn code(&self) -> &str {
        self.body["code"].as_str().unwrap_or_default()
    }
}

/// A request builder with the right `Host` and `Origin` by default.
pub struct Call {
    method: Method,
    path: String,
    cookie: Option<String>,
    host: Option<String>,
    origin: Option<String>,
    body: Option<String>,
    headers: Vec<(String, String)>,
}

impl Call {
    pub fn new(method: Method, path: &str) -> Self {
        Self {
            method,
            path: path.to_owned(),
            cookie: None,
            host: Some(HOST.into()),
            origin: Some(ORIGIN.into()),
            body: None,
            headers: Vec::new(),
        }
    }

    pub fn get(path: &str) -> Self {
        Self::new(Method::GET, path)
    }

    pub fn post(path: &str) -> Self {
        Self::new(Method::POST, path)
    }

    pub fn cookie(mut self, cookie: &str) -> Self {
        self.cookie = Some(cookie.to_owned());
        self
    }

    pub fn host(mut self, host: Option<&str>) -> Self {
        self.host = host.map(str::to_owned);
        self
    }

    pub fn origin(mut self, origin: Option<&str>) -> Self {
        self.origin = origin.map(str::to_owned);
        self
    }

    pub fn json(mut self, body: &impl Serialize) -> Self {
        self.body = Some(serde_json::to_string(body).unwrap());
        self
    }

    pub fn raw(mut self, body: &str) -> Self {
        self.body = Some(body.to_owned());
        self
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

impl Gateway {
    pub async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            data_dir: dir.path().to_owned(),
            listen: "127.0.0.1:0".parse().unwrap(),
            origin: PublicOrigin::parse(ORIGIN).unwrap(),
            extra_hosts: vec!["bokkie.tailnet.test".into()],
            trust_forwarded_for: false,
            codex_program: "/nonexistent/codex".into(),
        };
        let pool = db::open(&config.database_path()).await.unwrap();
        let catalog =
            catalog::write(&config.catalog_path(), fake_codex::bundled_catalog()).unwrap();
        let fake = fake_codex::FakeCodex::default();
        let (supervisor, events) = Supervisor::new(
            &config.data_dir,
            catalog.clone(),
            fake.launcher(&config.data_dir, &catalog.path),
        );
        let state = AppState::new(
            config,
            pool,
            Clock::manual(1_800_000_000_000),
            std::sync::Arc::new(supervisor),
        );
        Self {
            app: router(state.clone()),
            state,
            fake,
            events: tokio::sync::Mutex::new(events),
            dir,
        }
    }

    /// A gateway whose app servers are the real `codex` binary, pointed at
    /// a mock model by `overrides`.
    pub async fn live(program: std::path::PathBuf, overrides: Vec<String>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            data_dir: dir.path().to_owned(),
            listen: "127.0.0.1:0".parse().unwrap(),
            origin: PublicOrigin::parse(ORIGIN).unwrap(),
            extra_hosts: Vec::new(),
            trust_forwarded_for: false,
            codex_program: program.clone(),
        };
        let pool = db::open(&config.database_path()).await.unwrap();
        let bundled =
            catalog::export_bundled(&program, &config.data_dir.join("codex/catalog-home"))
                .await
                .unwrap();
        let catalog = catalog::write(&config.catalog_path(), bundled).unwrap();
        let (supervisor, events) = Supervisor::new(
            &config.data_dir,
            catalog,
            bokkie_gateway::codex::Launcher::Process {
                program,
                overrides,
                env: Vec::new(),
            },
        );
        let state = AppState::new(
            config,
            pool,
            Clock::system(),
            std::sync::Arc::new(supervisor),
        );
        Self {
            app: router(state.clone()),
            state,
            fake: fake_codex::FakeCodex::default(),
            events: tokio::sync::Mutex::new(events),
            dir,
        }
    }

    pub async fn send(&self, call: Call) -> Reply {
        send(self.app.clone(), call).await
    }

    /// Sends every call at once and returns the replies in call order.
    pub async fn send_all(&self, calls: Vec<Call>) -> Vec<Reply> {
        let tasks: Vec<_> = calls
            .into_iter()
            .map(|call| tokio::spawn(send(self.app.clone(), call)))
            .collect();
        let mut replies = Vec::new();
        for task in tasks {
            replies.push(task.await.unwrap());
        }
        replies
    }

    /// Serves the gateway on a local port, as `bokkie-gateway serve` does.
    pub async fn serve(&self) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = self.app.clone();
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        address
    }

    /// Routes app-server events into conversations, as the gateway does.
    pub async fn route(&self) {
        let (_, empty) = tokio::sync::mpsc::unbounded_channel();
        let events = std::mem::replace(&mut *self.events.lock().await, empty);
        tokio::spawn(self.state.conversations.clone().run(events));
    }

    /// Polls `path` until `done` accepts the reply.
    pub async fn wait_for(&self, cookie: &str, path: &str, done: impl Fn(&Value) -> bool) -> Value {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let reply = self.send(Call::get(path).cookie(cookie)).await;
            if done(&reply.body) {
                return reply.body;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting on {path}: {}",
                reply.body
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// Creates the first admin and signs in. Returns the cookie.
    pub async fn admin(&self, username: &str) -> String {
        accounts::bootstrap_admin(
            &self.state.db,
            self.state.clock.now(),
            username,
            username,
            PASSWORD,
        )
        .await
        .unwrap();
        self.login(username, PASSWORD).await.cookie()
    }

    pub async fn login(&self, username: &str, password: &str) -> Reply {
        self.send(
            Call::post("/api/v1/session")
                .json(&serde_json::json!({"username": username, "password": password})),
        )
        .await
    }

    /// Invites and signs up a member through the admin. Returns its cookie
    /// and user id.
    pub async fn member(&self, admin: &str, username: &str) -> (String, i64) {
        let invitation = self
            .send(Call::post("/api/v1/admin/invitations").cookie(admin).json(
                &CreateInvitationRequest {
                    role: Role::Member,
                    display_name: None,
                    expires_in_hours: None,
                },
            ))
            .await;
        assert_eq!(
            invitation.status,
            StatusCode::CREATED,
            "{:?}",
            invitation.body
        );
        let code = invitation.body["code"].as_str().unwrap();
        let accepted = self
            .send(
                Call::post("/api/v1/invitations/accept").json(&serde_json::json!({
                    "code": code, "username": username, "password": PASSWORD,
                    "display_name": username,
                })),
            )
            .await;
        assert_eq!(accepted.status, StatusCode::OK, "{:?}", accepted.body);
        let session: SessionResponse = serde_json::from_value(accepted.body.clone()).unwrap();
        (accepted.cookie(), session.user.id.0)
    }
}

async fn send(app: Router, call: Call) -> Reply {
    let mut request = Request::builder().method(call.method).uri(&call.path);
    if let Some(host) = &call.host {
        request = request.header(header::HOST, host);
    }
    if let Some(origin) = &call.origin {
        request = request.header(header::ORIGIN, origin);
    }
    if let Some(cookie) = &call.cookie {
        request = request.header(header::COOKIE, cookie);
    }
    for (name, value) in &call.headers {
        request = request.header(name, value);
    }
    let body = match call.body {
        Some(body) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(body)
        }
        None => Body::empty(),
    };
    let response = app.oneshot(request.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    Reply {
        status,
        headers,
        body,
    }
}
