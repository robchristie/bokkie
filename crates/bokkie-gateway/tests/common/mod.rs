//! A gateway on a temporary database, driven in-process.

#![allow(dead_code)]

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use bokkie_gateway::clock::Clock;
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
        };
        let pool = db::open(&config.database_path()).await.unwrap();
        let state = AppState::new(config, pool, Clock::manual(1_800_000_000_000));
        Self {
            app: router(state.clone()),
            state,
            dir,
        }
    }

    pub async fn send(&self, call: Call) -> Reply {
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
        let response = self
            .app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
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
