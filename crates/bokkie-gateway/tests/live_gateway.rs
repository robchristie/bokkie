//! The whole gateway against the real pinned `codex app-server` and a mock
//! model: sign in, create a Bot, and watch its reply stream over the event
//! WebSocket. Runs only when `BOKKIE_CODEX_BIN` names the binary.

mod common;

use std::time::Duration;

use bokkie_protocol::api::{Author, BotStatus, MessageState};
use bokkie_protocol::events::{ClientFrame, Event, ServerFrame};
use common::mock_model::MockModel;
use common::{Call, Gateway, HOST, ORIGIN};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

#[tokio::test]
async fn a_bot_reply_streams_over_the_websocket_from_a_real_app_server() {
    let Some(program) = std::env::var_os("BOKKIE_CODEX_BIN") else {
        eprintln!("BOKKIE_CODEX_BIN is not set; skipping");
        return;
    };
    let (mock, base_url) = MockModel::start(&["Pasta", " with", " pesto."]).await;
    let gateway = Gateway::live(program.into(), MockModel::overrides(&base_url)).await;
    gateway.route().await;
    let address = gateway.serve().await;
    let rob = gateway.admin("rob").await;

    let mut request = format!("ws://{address}/api/v1/events")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert("host", HOST.parse().unwrap());
    request
        .headers_mut()
        .insert("origin", ORIGIN.parse().unwrap());
    request.headers_mut().insert("cookie", rob.parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    socket
        .send(Message::text(
            serde_json::to_string(&ClientFrame::Resume { after: None }).unwrap(),
        ))
        .await
        .unwrap();

    let bot = gateway
        .send(Call::post("/api/v1/bots").cookie(&rob).json(&json!({
            "name": "Chef", "label": "Meal planner", "description": "Suggest one dinner."})))
        .await;
    let bot_id = bot.body["id"].as_i64().unwrap();
    let sent = gateway
        .send(
            Call::post(&format!("/api/v1/bots/{bot_id}/messages"))
                .cookie(&rob)
                .json(&json!({"text": "What's for dinner?"})),
        )
        .await;
    assert_eq!(sent.status, 200, "{:?}", sent.body);

    let mut streamed = String::new();
    let mut working = false;
    let reply = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let Some(Ok(Message::Text(text))) = socket.next().await else {
                panic!("the socket closed");
            };
            let ServerFrame::Event(envelope) = serde_json::from_str(&text).unwrap() else {
                continue;
            };
            match envelope.event {
                Event::BotStatusChanged {
                    status: BotStatus::Working,
                    ..
                } => working = true,
                Event::MessageDelta { offset, delta, .. } => {
                    assert_eq!(offset, streamed.chars().count() as u64);
                    streamed.push_str(&delta);
                }
                Event::MessageCompleted { message } if message.author == Author::Bot => {
                    return message;
                }
                Event::BotError { message, .. } => panic!("Bot error: {message}"),
                _ => {}
            }
        }
    })
    .await
    .expect("the reply arrives");
    assert!(working);
    assert_eq!(reply.state, MessageState::Complete);
    assert_eq!(reply.text, "Pasta with pesto.");
    assert!(streamed.is_empty() || streamed == reply.text, "{streamed}");
    // The model saw the Bot's profile.
    let request = mock.requests.lock().unwrap()[0].to_string();
    assert!(request.contains("Suggest one dinner."));
}
