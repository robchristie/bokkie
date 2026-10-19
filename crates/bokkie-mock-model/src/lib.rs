//! A stand-in for the OpenAI Responses API, so a real `codex app-server`
//! can run turns without calling a model. It records every request body,
//! which shows exactly which tools Codex offered the model, and the bearer
//! token each request carried.
//!
//! The gateway's tests use it in-process; the `bokkie-mock-model` binary
//! serves it for the browser smoke. Point an app server at it with
//! [`MockModel::overrides`], or with [`MockModel::signed_in_overrides`]
//! when the app server holds a synthetic ChatGPT sign-in from
//! [`chatgpt::MockChatgpt`], which stands in for ChatGPT's sign-in
//! authority and backend. Nothing here is reachable from a production
//! configuration: no production crate depends on this one.
//!
//! A reply streams in pieces, optionally with a pause before each, and the
//! response can report the subscription's rate-limit windows in the
//! `x-codex-*` headers that Codex reads from the real API.

pub mod chatgpt;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::HeaderValue;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The mock and what it has seen. Clones share their state.
#[derive(Clone, Default)]
pub struct MockModel {
    pub requests: Arc<Mutex<Vec<Value>>>,
    /// The reply, streamed in these pieces.
    pub reply: Arc<Mutex<Vec<String>>>,
    /// The pause before each piece.
    pub piece_delay: Arc<Mutex<Duration>>,
    /// The windows each response reports, if any.
    pub rate_limits: Arc<Mutex<Option<RateLimits>>>,
    /// When set, the next request waits for this before it is answered.
    pub gate: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
    /// The bearer token of each request, in order, if it carried one.
    pub bearers: Arc<Mutex<Vec<Option<String>>>>,
    /// Answers this many requests with 401 before answering normally.
    pub unauthorized: Arc<Mutex<u32>>,
    /// When set, a request whose bearer token this refuses is answered 401.
    pub authorizer: Arc<Mutex<Option<Authorizer>>>,
}

/// Decides whether a bearer token may call the model.
pub type Authorizer = Arc<dyn Fn(Option<&str>) -> bool + Send + Sync>;

impl std::fmt::Debug for MockModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockModel")
            .field("requests", &self.requests.lock().unwrap().len())
            .finish_non_exhaustive()
    }
}

/// The subscription's windows, as the real API reports them in headers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimits {
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimitWindow {
    pub used_percent: u32,
    pub window_minutes: u32,
    /// When the window resets, in Unix seconds.
    pub resets_at: u64,
}

impl RateLimits {
    /// The response headers that carry these windows.
    pub fn headers(&self) -> Vec<(String, String)> {
        let mut headers = Vec::new();
        for (name, window) in [("primary", &self.primary), ("secondary", &self.secondary)] {
            if let Some(window) = window {
                headers.push((
                    format!("x-codex-{name}-used-percent"),
                    window.used_percent.to_string(),
                ));
                headers.push((
                    format!("x-codex-{name}-window-minutes"),
                    window.window_minutes.to_string(),
                ));
                headers.push((
                    format!("x-codex-{name}-reset-at"),
                    window.resets_at.to_string(),
                ));
            }
        }
        headers
    }
}

impl MockModel {
    /// Holds the next request until the returned sender fires or drops.
    pub fn hold_next(&self) -> tokio::sync::oneshot::Sender<()> {
        let (release, gate) = tokio::sync::oneshot::channel();
        *self.gate.lock().unwrap() = Some(gate);
        release
    }

    /// A mock that streams `reply` with no pauses.
    pub fn new(reply: &[&str]) -> Self {
        let mock = Self::default();
        mock.set_reply(reply.iter().map(|s| s.to_string()).collect());
        mock
    }

    pub fn set_reply(&self, pieces: Vec<String>) {
        *self.reply.lock().unwrap() = pieces;
    }

    /// The Responses API route, for serving alongside other routes.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/v1/responses", post(respond))
            .with_state(self.clone())
    }

    /// Serves on a free local port and returns the base URL (`.../v1`).
    pub async fn start(reply: &[&str]) -> (Self, String) {
        let mock = Self::new(reply);
        let base_url = serve(mock.router(), "127.0.0.1:0").await;
        (mock, base_url)
    }

    /// The `-c` overrides that point an app server at this mock.
    pub fn overrides(base_url: &str) -> Vec<String> {
        vec![
            "model_provider=\"bokkie_mock\"".into(),
            "model=\"gpt-6.1-sol\"".into(),
            "model_providers.bokkie_mock.name=\"bokkie_mock\"".into(),
            format!("model_providers.bokkie_mock.base_url=\"{base_url}\""),
            "model_providers.bokkie_mock.wire_api=\"responses\"".into(),
            "model_providers.bokkie_mock.requires_openai_auth=false".into(),
            "model_providers.bokkie_mock.supports_websockets=false".into(),
        ]
    }

    /// The same, for an app server that holds a ChatGPT sign-in: Codex then
    /// sends the sign-in's access token to the model as a bearer token, so
    /// tests can see which token each turn used. Add
    /// [`chatgpt::MockChatgpt::overrides`] so that the sign-in's backend is
    /// the mock's too.
    pub fn signed_in_overrides(base_url: &str) -> Vec<String> {
        Self::overrides(base_url)
            .into_iter()
            .map(|o| o.replace("requires_openai_auth=false", "requires_openai_auth=true"))
            .collect()
    }

    /// Every tool offered in any request, by name. Namespaces are opened,
    /// tools in the `additional_tools` input item are included, and so are
    /// the nested tools that a code-mode `exec` tool describes.
    pub fn offered_tools(&self) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        for request in self.requests.lock().unwrap().iter() {
            collect(&request["tools"], &mut names);
            for item in request["input"].as_array().into_iter().flatten() {
                if item["type"] == "additional_tools" {
                    collect(&item["tools"], &mut names);
                }
            }
        }
        names
    }
}

/// Serves `app` on `listen` in the background and returns the base URL of
/// its Responses API (`http://<address>/v1`).
pub async fn serve(app: Router, listen: &str) -> String {
    let listener = tokio::net::TcpListener::bind(listen).await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{address}/v1")
}

fn collect(tools: &Value, names: &mut BTreeSet<String>) {
    for tool in tools.as_array().into_iter().flatten() {
        if tool["type"] == "namespace" {
            collect(&tool["tools"], names);
            continue;
        }
        let name = tool["name"]
            .as_str()
            .or_else(|| tool["type"].as_str())
            .unwrap_or("?");
        names.insert(name.to_owned());
        // A code-mode `exec` tool lists its nested tools as headings.
        if let Some(description) = tool["description"].as_str() {
            for line in description.lines() {
                if let Some(nested) = line
                    .strip_prefix("### `")
                    .and_then(|rest| rest.strip_suffix('`'))
                {
                    names.insert(format!("{name}>{nested}"));
                }
            }
        }
    }
}

fn event(event: &Value) -> String {
    format!(
        "event: {}\ndata: {event}\n\n",
        event["type"].as_str().unwrap()
    )
}

async fn respond(
    State(mock): State<MockModel>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned);
    mock.bearers.lock().unwrap().push(bearer.clone());
    let refused = {
        let mut unauthorized = mock.unauthorized.lock().unwrap();
        if *unauthorized > 0 {
            *unauthorized -= 1;
            true
        } else {
            let authorizer = mock.authorizer.lock().unwrap().clone();
            authorizer.is_some_and(|accepts| !accepts(bearer.as_deref()))
        }
    };
    if refused {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({"error": {
                "message": "The access token is not valid.", "code": "invalid_token"}})),
        )
            .into_response();
    }
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    mock.requests.lock().unwrap().push(request);
    let gate = mock.gate.lock().unwrap().take();
    if let Some(gate) = gate {
        let _ = gate.await;
    }
    let reply = mock.reply.lock().unwrap().clone();
    let delay = *mock.piece_delay.lock().unwrap();
    let rate_limits = mock.rate_limits.lock().unwrap().clone();
    let text: String = reply.concat();
    let opening = [
        json!({"type": "response.created", "response": {"id": "resp-1"}}),
        json!({"type": "response.output_item.added", "item": {
            "type": "message", "role": "assistant", "id": "msg-1", "content": []}}),
    ];
    let closing = [
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "role": "assistant", "id": "msg-1",
            "content": [{"type": "output_text", "text": text}]}}),
        json!({"type": "response.completed", "response": {"id": "resp-1", "usage": {
            "input_tokens": 100, "input_tokens_details": null, "output_tokens": 20,
            "output_tokens_details": null, "total_tokens": 120}}}),
    ];
    // The stream: the opening events at once, then each piece after its
    // pause, then the closing events.
    let mut chunks: Vec<(Duration, String)> =
        vec![(Duration::ZERO, opening.iter().map(event).collect())];
    chunks.extend(reply.iter().map(|piece| {
        (
            delay,
            event(
                &json!({"type": "response.output_text.delta", "item_id": "msg-1", "delta": piece}),
            ),
        )
    }));
    chunks.push((Duration::ZERO, closing.iter().map(event).collect()));
    let stream = futures_util::stream::unfold(chunks.into_iter(), |mut chunks| async move {
        let (pause, chunk) = chunks.next()?;
        if !pause.is_zero() {
            tokio::time::sleep(pause).await;
        }
        Some((Ok::<_, std::convert::Infallible>(chunk), chunks))
    });
    let mut response = Body::from_stream(stream).into_response();
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    for (name, value) in rate_limits.iter().flat_map(RateLimits::headers) {
        headers.insert(
            axum::http::HeaderName::try_from(name).unwrap(),
            HeaderValue::try_from(value).unwrap(),
        );
    }
    response
}
