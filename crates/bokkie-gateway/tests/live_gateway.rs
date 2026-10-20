//! The whole gateway against the real pinned `codex app-server`, a mock
//! model and a synthetic household sign-in: sign in, create a Bot, and
//! watch its reply stream over the event WebSocket. Runs only when
//! `BOKKIE_CODEX_BIN` names the binary.

mod common;

use std::time::Duration;

use bokkie_mock_model::{RateLimitWindow, RateLimits};
use bokkie_protocol::api::{Author, BotStatus, MessageState};
use bokkie_protocol::events::{ClientFrame, Event, ServerFrame};
use common::live::Live;
use common::{Call, Gateway, HOST, ORIGIN};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

#[tokio::test]
async fn a_bot_reply_streams_over_the_websocket_from_a_real_app_server() {
    let Some(live) = Live::start(&["Pasta", " with", " pesto."]).await else {
        return;
    };
    let mock = &live.model;
    let gateway = Gateway::live(&live).await;
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
    // Deltas may stop short: the final text replaces what streamed.
    assert!(reply.text.starts_with(&streamed), "{streamed}");
    // The model saw the Bot's profile.
    let request = mock.requests.lock().unwrap()[0].to_string();
    assert!(request.contains("Suggest one dinner."));
    // On the household's sign-in, which nobody refreshed.
    assert_eq!(live.model_generations(), [Some(1)]);
    assert!(live.refreshers().is_empty());
    live.assert_nothing_left_the_machine();
}

/// The mock reports the subscription's windows in the headers the real API
/// uses, and the real app server passes them to the gateway's usage meter.
/// The browser smoke relies on this to show the meter and the banner.
#[tokio::test]
async fn the_mock_models_rate_limit_windows_reach_the_usage_meter() {
    let Some(live) = Live::start(&["Fine."]).await else {
        return;
    };
    let mock = &live.model;
    let resets_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    let window = |used_percent, window_minutes| RateLimitWindow {
        used_percent,
        window_minutes,
        resets_at,
    };
    *mock.rate_limits.lock().unwrap() = Some(RateLimits {
        primary: Some(window(42, 300)),
        secondary: Some(window(7, 10080)),
    });
    let gateway = Gateway::live(&live).await;
    gateway.route().await;
    let rob = gateway.admin("rob").await;
    let bot = gateway
        .send(Call::post("/api/v1/bots").cookie(&rob).json(&json!({
            "name": "Chef", "label": "", "description": ""})))
        .await;
    let bot_id = bot.body["id"].as_i64().unwrap();
    let sent = gateway
        .send(
            Call::post(&format!("/api/v1/bots/{bot_id}/messages"))
                .cookie(&rob)
                .json(&json!({"text": "Hello"})),
        )
        .await;
    assert_eq!(sent.status, 200, "{:?}", sent.body);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let usage = loop {
        let usage = gateway.send(Call::get("/api/v1/usage").cookie(&rob)).await;
        let primary = &usage.body["subscription"][0]["primary"];
        if primary["used_percent"] == 42 {
            break usage.body;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no windows: {:?}",
            usage.body
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let bucket = &usage["subscription"][0];
    assert_eq!(bucket["primary"]["window_minutes"], 300, "{usage}");
    assert_eq!(bucket["primary"]["resets_at"], resets_at * 1000, "{usage}");
    assert_eq!(bucket["secondary"]["used_percent"], 7, "{usage}");
    assert_eq!(bucket["reached"], false, "{usage}");

    // A full window is reported as reached.
    *mock.rate_limits.lock().unwrap() = Some(RateLimits {
        primary: Some(window(100, 300)),
        secondary: None,
    });
    let sent = gateway
        .send(
            Call::post(&format!("/api/v1/bots/{bot_id}/messages"))
                .cookie(&rob)
                .json(&json!({"text": "Again"})),
        )
        .await;
    assert_eq!(sent.status, 200, "{:?}", sent.body);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let usage = gateway.send(Call::get("/api/v1/usage").cookie(&rob)).await;
        if usage.body["subscription"][0]["reached"] == true {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never reached: {:?}",
            usage.body
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    live.assert_nothing_left_the_machine();
}

/// A message sent the moment the Bot's reply completes arrives while its
/// turn is ending. The real app server may then refuse the steer, answer it
/// in the turn, or take it into the thread and end the turn without asking
/// the model; the gateway starts a turn for it in the last case. Every way,
/// each message gets one reply.
#[tokio::test]
async fn a_message_sent_as_the_turn_ends_gets_a_reply() {
    let Some(live) = Live::start(&["Noted."]).await else {
        return;
    };
    let mock = &live.model;
    let gateway = Gateway::live(&live).await;
    gateway.route().await;
    let rob = gateway.admin("rob").await;
    let mut events = gateway.state.events.subscribe(bokkie_protocol::UserId(1));
    let bot = gateway
        .send(Call::post("/api/v1/bots").cookie(&rob).json(&json!({
            "name": "Scribe", "label": "", "description": ""})))
        .await;
    let bot_id = bot.body["id"].as_i64().unwrap();
    let say = async |text: &str| {
        let sent = gateway
            .send(
                Call::post(&format!("/api/v1/bots/{bot_id}/messages"))
                    .cookie(&rob)
                    .json(&json!({"text": text})),
            )
            .await;
        assert_eq!(sent.status, 200, "{:?}", sent.body);
        sent.body["delivery"].as_str().unwrap().to_owned()
    };
    let mut deliveries = vec![say("Round 0").await];
    const ROUNDS: usize = 8;
    for round in 1..=ROUNDS {
        // Each message's reply completes, and the next goes at once.
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match events.recv().await.unwrap().event {
                    Event::MessageCompleted { message } if message.author == Author::Bot => {
                        return;
                    }
                    Event::BotError { message, .. } => panic!("Bot error: {message}"),
                    _ => {}
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no reply to round {}: {deliveries:?}", round - 1));
        if round < ROUNDS {
            deliveries.push(say(&format!("Round {round}")).await);
        }
    }
    // Each message has one reply, after it.
    let page = gateway
        .wait_for(&rob, &format!("/api/v1/bots/{bot_id}"), |b| {
            b["status"] == "idle"
        })
        .await;
    assert_eq!(page["status"], "idle");
    let page = gateway
        .send(Call::get(&format!("/api/v1/bots/{bot_id}/messages")).cookie(&rob))
        .await;
    let said: Vec<_> = page.body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["author"].as_str().unwrap().to_owned(),
                m["text"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let expected: Vec<_> = (0..ROUNDS)
        .flat_map(|round| {
            [
                ("user".to_owned(), format!("Round {round}")),
                ("bot".to_owned(), "Noted.".to_owned()),
            ]
        })
        .collect();
    assert_eq!(said, expected, "{deliveries:?}");
    // How often each message reached the model in the request that
    // answered it: twice when the gateway started a turn for a steered
    // message that the thread already held.
    let requests = mock.requests.lock().unwrap().clone();
    let seen: Vec<_> = (0..ROUNDS)
        .map(|round| {
            let text = format!("\"Round {round}\"");
            requests
                .iter()
                .find(|r| r["input"].to_string().contains(&text))
                .map(|r| r["input"].to_string().matches(&text).count())
                .unwrap_or(0)
        })
        .collect();
    eprintln!("deliveries {deliveries:?}, seen by the model {seen:?}");
    live.assert_nothing_left_the_machine();
}
