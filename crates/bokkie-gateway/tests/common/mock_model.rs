//! A stand-in for the OpenAI Responses API, so a real `codex app-server`
//! can run turns without signing in or calling a model. It records every
//! request body, which shows exactly which tools Codex offered the model.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use axum::routing::post;
use serde_json::{Value, json};

#[derive(Clone, Default)]
pub struct MockModel {
    pub requests: Arc<Mutex<Vec<Value>>>,
    /// The reply, streamed in these pieces.
    pub reply: Arc<Mutex<Vec<String>>>,
    /// When set, the next request waits for this before it is answered.
    pub gate: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
}

impl MockModel {
    /// Holds the next request until the returned sender fires or drops.
    pub fn hold_next(&self) -> tokio::sync::oneshot::Sender<()> {
        let (release, gate) = tokio::sync::oneshot::channel();
        *self.gate.lock().unwrap() = Some(gate);
        release
    }

    /// Serves on a free local port and returns the base URL (`.../v1`).
    pub async fn start(reply: &[&str]) -> (Self, String) {
        let mock = Self::default();
        *mock.reply.lock().unwrap() = reply.iter().map(|s| s.to_string()).collect();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/responses", post(respond))
            .with_state(mock.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (mock, format!("http://{address}/v1"))
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

async fn respond(State(mock): State<MockModel>, body: axum::body::Bytes) -> impl IntoResponse {
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    mock.requests.lock().unwrap().push(request);
    let gate = mock.gate.lock().unwrap().take();
    if let Some(gate) = gate {
        let _ = gate.await;
    }
    let reply = mock.reply.lock().unwrap().clone();
    let text: String = reply.concat();
    let mut events = vec![
        json!({"type": "response.created", "response": {"id": "resp-1"}}),
        json!({"type": "response.output_item.added", "item": {
            "type": "message", "role": "assistant", "id": "msg-1", "content": []}}),
    ];
    for piece in &reply {
        events.push(
            json!({"type": "response.output_text.delta", "item_id": "msg-1", "delta": piece}),
        );
    }
    events.push(json!({"type": "response.output_item.done", "item": {
        "type": "message", "role": "assistant", "id": "msg-1",
        "content": [{"type": "output_text", "text": text}]}}));
    events.push(
        json!({"type": "response.completed", "response": {"id": "resp-1", "usage": {
        "input_tokens": 100, "input_tokens_details": null, "output_tokens": 20,
        "output_tokens_details": null, "total_tokens": 120}}}),
    );
    let body: String = events
        .iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect();
    ([(CONTENT_TYPE, "text/event-stream")], body)
}
