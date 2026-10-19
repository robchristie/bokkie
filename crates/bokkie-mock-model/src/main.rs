//! Serves the mocks for the browser smoke:
//! `bokkie-mock-model [ADDRESS [DIR]]` (default `127.0.0.1:0` and a new
//! directory under the system's temporary directory). The model listens on
//! ADDRESS; the ChatGPT authority and backend, and the deny proxy, on free
//! local ports, with the authority's certificate in DIR. Prints one JSON
//! line, then serves until stopped:
//!
//! - `base_url`: the Responses API's base URL;
//! - `overrides`: the `-c` overrides that point an app server at the model
//!   and the backend;
//! - `env`: the environment that points its token and revoke endpoints at
//!   the authority, trusts the authority's certificate, and sends any other
//!   host to the deny proxy;
//! - `household_auth`: an `auth.json` holding a synthetic sign-in that lasts
//!   a day, for `<data>/household/codex/auth.json`;
//! - `control`: the control URL.
//!
//! Control, as JSON:
//! - `PUT /control/reply` `{"pieces": [...], "piece_delay_ms": N}`;
//! - `PUT /control/rate-limits` with [`RateLimits`] or `null`;
//! - `GET /control/requests` `{"count": N, "refreshes": N, "refused": [...]}`:
//!   the requests the model answered, the refreshes the authority answered,
//!   and the hosts the deny proxy refused.

use std::path::PathBuf;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::routing::{get, put};
use bokkie_mock_model::chatgpt::MockChatgpt;
use bokkie_mock_model::{MockModel, RateLimits};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Reply {
    pieces: Vec<String>,
    piece_delay_ms: u64,
}

#[derive(Clone)]
struct Mocks {
    model: MockModel,
    chatgpt: MockChatgpt,
}

async fn set_reply(State(mocks): State<Mocks>, Json(reply): Json<Reply>) -> Json<Value> {
    mocks.model.set_reply(reply.pieces);
    *mocks.model.piece_delay.lock().unwrap() = Duration::from_millis(reply.piece_delay_ms);
    Json(json!({}))
}

async fn set_rate_limits(
    State(mocks): State<Mocks>,
    Json(limits): Json<Option<RateLimits>>,
) -> Json<Value> {
    *mocks.model.rate_limits.lock().unwrap() = limits;
    Json(json!({}))
}

async fn requests(State(mocks): State<Mocks>) -> Json<Value> {
    Json(json!({
        "count": mocks.model.requests.lock().unwrap().len(),
        "refreshes": mocks.chatgpt.refreshes().len(),
        "refused": mocks.chatgpt.proxy().hosts(),
    }))
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let listen = args.next().unwrap_or_else(|| "127.0.0.1:0".into());
    let dir = match args.next() {
        Some(dir) => PathBuf::from(dir),
        None => {
            let dir = std::env::temp_dir().join(format!("bokkie-mock-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }
    };
    let model = MockModel::new(&["Hello", " from", " a mock"]);
    let chatgpt = MockChatgpt::start(&dir).await;
    let mocks = Mocks {
        model: model.clone(),
        chatgpt: chatgpt.clone(),
    };
    let control = axum::Router::new()
        .route("/control/reply", put(set_reply))
        .route("/control/rate-limits", put(set_rate_limits))
        .route("/control/requests", get(requests))
        .with_state(mocks);
    let base_url = bokkie_mock_model::serve(model.router().merge(control), &listen).await;
    let origin = base_url.trim_end_matches("/v1");
    let mut overrides = MockModel::signed_in_overrides(&base_url);
    overrides.extend(chatgpt.overrides());
    let env: serde_json::Map<String, Value> = chatgpt
        .env()
        .into_iter()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                Value::String(value.to_string_lossy().into_owned()),
            )
        })
        .collect();
    // One write, so a reader gets the line whole.
    let line = format!(
        "{}\n",
        json!({
            "base_url": base_url,
            "overrides": overrides,
            "env": env,
            "household_auth": chatgpt.auth_json(86_400),
            "control": format!("{origin}/control"),
        })
    );
    {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(line.as_bytes()).unwrap();
        stdout.flush().unwrap();
    }
    let _ = tokio::signal::ctrl_c().await;
}
