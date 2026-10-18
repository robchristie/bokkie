//! Serves the mock model for the browser smoke: `bokkie-mock-model [ADDRESS]`
//! (default `127.0.0.1:0`). Prints one JSON line with the Responses API's
//! base URL, the `-c` overrides that point an app server at it, and its
//! control URL, then serves until stopped.
//!
//! Control, as JSON:
//! - `PUT /control/reply` `{"pieces": [...], "piece_delay_ms": N}`;
//! - `PUT /control/rate-limits` with [`RateLimits`] or `null`;
//! - `GET /control/requests` `{"count": N}`, the requests answered so far.

use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::routing::{get, put};
use bokkie_mock_model::{MockModel, RateLimits};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Reply {
    pieces: Vec<String>,
    piece_delay_ms: u64,
}

async fn set_reply(State(mock): State<MockModel>, Json(reply): Json<Reply>) -> Json<Value> {
    mock.set_reply(reply.pieces);
    *mock.piece_delay.lock().unwrap() = Duration::from_millis(reply.piece_delay_ms);
    Json(json!({}))
}

async fn set_rate_limits(
    State(mock): State<MockModel>,
    Json(limits): Json<Option<RateLimits>>,
) -> Json<Value> {
    *mock.rate_limits.lock().unwrap() = limits;
    Json(json!({}))
}

async fn requests(State(mock): State<MockModel>) -> Json<Value> {
    Json(json!({"count": mock.requests.lock().unwrap().len()}))
}

#[tokio::main]
async fn main() {
    let listen = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:0".into());
    let mock = MockModel::new(&["Hello", " from", " a mock"]);
    let control = axum::Router::new()
        .route("/control/reply", put(set_reply))
        .route("/control/rate-limits", put(set_rate_limits))
        .route("/control/requests", get(requests))
        .with_state(mock.clone());
    let base_url = bokkie_mock_model::serve(mock.router().merge(control), &listen).await;
    let origin = base_url.trim_end_matches("/v1");
    println!(
        "{}",
        json!({
            "base_url": base_url,
            "overrides": MockModel::overrides(&base_url),
            "control": format!("{origin}/control"),
        })
    );
    let _ = tokio::signal::ctrl_c().await;
}
