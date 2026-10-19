//! The event WebSocket over a real socket.

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use bokkie_protocol::events::{ClientFrame, Event, EventEnvelope, Seq, ServerFrame};
use common::{Call, Gateway, HOST, ORIGIN};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(
    address: SocketAddr,
    cookie: Option<&str>,
    origin: Option<&str>,
) -> Result<Socket, Box<tungstenite::Error>> {
    let mut request = format!("ws://{address}/api/v1/events")
        .into_client_request()
        .unwrap();
    let headers = request.headers_mut();
    headers.insert("host", HOST.parse().unwrap());
    if let Some(origin) = origin {
        headers.insert("origin", origin.parse().unwrap());
    }
    if let Some(cookie) = cookie {
        headers.insert("cookie", cookie.parse().unwrap());
    }
    tokio_tungstenite::connect_async(request)
        .await
        .map(|(socket, _)| socket)
        .map_err(Box::new)
}

async fn open(address: SocketAddr, cookie: &str, after: Option<Seq>) -> Socket {
    let mut socket = connect(address, Some(cookie), Some(ORIGIN)).await.unwrap();
    let frame = serde_json::to_string(&ClientFrame::Resume { after }).unwrap();
    socket.send(Message::text(frame)).await.unwrap();
    socket
}

/// The next frame, or `None` if the socket closed.
async fn next(socket: &mut Socket) -> Option<ServerFrame> {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("a frame arrives in time")?;
        match message.ok()? {
            Message::Text(text) => return Some(serde_json::from_str(&text).unwrap()),
            Message::Close(_) => return None,
            _ => {}
        }
    }
}

async fn event(socket: &mut Socket) -> EventEnvelope {
    match next(socket).await {
        Some(ServerFrame::Event(envelope)) => envelope,
        other => panic!("expected an event, got {other:?}"),
    }
}

/// Reads events until one matches.
async fn until(socket: &mut Socket, want: impl Fn(&Event) -> bool) -> Vec<EventEnvelope> {
    let mut seen = Vec::new();
    loop {
        let envelope = event(socket).await;
        let done = want(&envelope.event);
        seen.push(envelope);
        if done {
            return seen;
        }
    }
}

async fn create_bot(gateway: &Gateway, cookie: &str, name: &str) -> i64 {
    let reply = gateway
        .send(Call::post("/api/v1/bots").cookie(cookie).json(&json!({
            "name": name, "label": "", "description": ""})))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    reply.body["id"].as_i64().unwrap()
}

async fn say(gateway: &Gateway, cookie: &str, bot: i64, text: &str) {
    let reply = gateway
        .send(
            Call::post(&format!("/api/v1/bots/{bot}/messages"))
                .cookie(cookie)
                .json(&json!({"text": text})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
}

async fn setup() -> (Gateway, SocketAddr, String) {
    let gateway = Gateway::new().await;
    gateway.route().await;
    let admin = gateway.admin("rob").await;
    let address = gateway.serve().await;
    (gateway, address, admin)
}

#[tokio::test]
async fn the_socket_needs_a_session_and_the_gateways_origin() {
    let (_gateway, address, rob) = setup().await;
    for (cookie, origin) in [
        (None, Some(ORIGIN)),
        (Some("__Host-bokkie_session=forged"), Some(ORIGIN)),
        (Some(rob.as_str()), None),
        (Some(rob.as_str()), Some("https://evil.test")),
    ] {
        match connect(address, cookie, origin).await {
            Err(error) if matches!(*error, tungstenite::Error::Http(_)) => {
                let tungstenite::Error::Http(response) = *error else {
                    unreachable!()
                };
                assert!(
                    [401, 403].contains(&response.status().as_u16()),
                    "{cookie:?} {origin:?}: {}",
                    response.status()
                );
            }
            other => panic!("{cookie:?} {origin:?} connected: {:?}", other.map(|_| ())),
        }
    }
    // A socket that never sends its resume frame is closed.
    let mut silent = connect(address, Some(&rob), Some(ORIGIN)).await.unwrap();
    silent
        .send(Message::text(r#"{"type":"subscribe_all"}"#))
        .await
        .unwrap();
    assert_eq!(next(&mut silent).await, None);
}

#[tokio::test]
async fn a_conversation_streams_over_the_socket_in_order() {
    let (gateway, address, rob) = setup().await;
    let mut socket = open(address, &rob, None).await;
    assert_eq!(
        next(&mut socket).await,
        Some(ServerFrame::Live { latest: Seq(0) })
    );

    let chef = create_bot(&gateway, &rob, "Chef").await;
    let created = event(&mut socket).await;
    assert_eq!(created.seq, Seq(1));
    assert!(matches!(created.event, Event::BotCreated { ref bot } if bot.id.0 == chef));

    say(&gateway, &rob, chef, "Hello?").await;
    let events = until(&mut socket, |e| {
        matches!(e, Event::BotStatusChanged { status, .. } if *status == bokkie_protocol::api::BotStatus::Idle)
    })
    .await;
    // Sequence numbers increase by one with no gaps.
    for (i, envelope) in events.iter().enumerate() {
        assert_eq!(envelope.seq, Seq(2 + i as u64));
    }
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| match &e.event {
            Event::MessageCreated { message }
                if message.author == bokkie_protocol::api::Author::User =>
            {
                "user"
            }
            Event::MessageCreated { .. } => "reply",
            Event::MessageDelta { .. } => "delta",
            Event::MessageCompleted { .. } => "completed",
            Event::BotStatusChanged { .. } => "status",
            Event::UsageUpdated { .. } => "usage",
            _ => "other",
        })
        .collect();
    // The turn is counted before the message that starts it is stored.
    assert_eq!(kinds[..2], ["usage", "user"], "{kinds:?}");
    assert!(
        kinds.contains(&"reply") && kinds.contains(&"completed"),
        "{kinds:?}"
    );
    assert_eq!(
        kinds.iter().filter(|k| **k == "status").count(),
        2,
        "{kinds:?}"
    );

    // The deltas, applied at their offsets, give the final text.
    let mut text = String::new();
    for envelope in &events {
        if let Event::MessageDelta { offset, delta, .. } = &envelope.event {
            assert_eq!(*offset, text.chars().count() as u64);
            text.push_str(delta);
        }
    }
    let completed = events
        .iter()
        .find_map(|e| match &e.event {
            Event::MessageCompleted { message } => Some(message.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(completed.text, "Hello there");
    // Deltas may stop short: the final text replaces what streamed.
    assert!(completed.text.starts_with(&text), "{text}");
}

#[tokio::test]
async fn streamed_deltas_are_coalesced() {
    let (gateway, address, rob) = setup().await;
    let pieces: Vec<String> = (0..200).map(|i| format!("{} ", i % 10)).collect();
    let expected: String = pieces.concat();
    gateway.fake.with(|b| {
        b.reply = pieces.clone();
        b.finish_turns = false;
    });
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let mut socket = open(address, &rob, None).await;
    next(&mut socket).await;
    say(&gateway, &rob, chef, "Count").await;
    let mut text = String::new();
    let mut deltas = 0;
    while text.len() < expected.len() {
        if let Event::MessageDelta { offset, delta, .. } = event(&mut socket).await.event {
            assert_eq!(offset, text.chars().count() as u64);
            text.push_str(&delta);
            deltas += 1;
        }
    }
    assert_eq!(text, expected);
    assert!(
        deltas < 20,
        "{deltas} delta events for 200 app-server deltas"
    );
}

#[tokio::test]
async fn a_reconnecting_client_replays_what_it_missed() {
    let (gateway, address, rob) = setup().await;
    let mut socket = open(address, &rob, None).await;
    next(&mut socket).await;
    create_bot(&gateway, &rob, "One").await;
    let first = event(&mut socket).await;
    drop(socket);

    // While the client is away.
    create_bot(&gateway, &rob, "Two").await;
    create_bot(&gateway, &rob, "Three").await;

    let mut socket = open(address, &rob, Some(first.seq)).await;
    let replayed: Vec<u64> = vec![
        event(&mut socket).await.seq.0,
        event(&mut socket).await.seq.0,
    ];
    assert_eq!(replayed, [2, 3]);
    assert_eq!(
        next(&mut socket).await,
        Some(ServerFrame::Live { latest: Seq(3) })
    );
    create_bot(&gateway, &rob, "Four").await;
    assert_eq!(event(&mut socket).await.seq, Seq(4));
}

#[tokio::test]
async fn resuming_outside_the_retained_log_resets() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    // Keep only a few events, so older ones drop out.
    let state = gateway.state.clone();
    let events = std::sync::Arc::new(bokkie_gateway::events::EventLog::with_limits(
        state.db.clone(),
        state.clock.clone(),
        2,
        16,
    ));
    let user = bokkie_protocol::UserId(1);
    for _ in 0..300 {
        events
            .append(
                user,
                Event::BotError {
                    bot_id: bokkie_protocol::BotId(1),
                    message: "x".into(),
                },
            )
            .await
            .unwrap();
    }
    assert!(events.earliest(user).await.unwrap().unwrap() > Seq(2));
    let app = bokkie_gateway::router(bokkie_gateway::AppState::with_events(
        (*state.config).clone(),
        state.db.clone(),
        state.clock.clone(),
        state.supervisor.clone(),
        events,
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    // Too old: the events after 1 were dropped.
    let mut socket = open(address, &admin, Some(Seq(1))).await;
    assert_eq!(
        next(&mut socket).await,
        Some(ServerFrame::Reset { after: Seq(300) })
    );
    assert_eq!(
        next(&mut socket).await,
        Some(ServerFrame::Live { latest: Seq(300) })
    );
    // Beyond the end, for example after the database was replaced.
    let mut socket = open(address, &admin, Some(Seq(900))).await;
    assert_eq!(
        next(&mut socket).await,
        Some(ServerFrame::Reset { after: Seq(300) })
    );
    // Up to date: nothing to replay.
    let mut socket = open(address, &admin, Some(Seq(300))).await;
    assert_eq!(
        next(&mut socket).await,
        Some(ServerFrame::Live { latest: Seq(300) })
    );
}

#[tokio::test]
async fn sequence_numbers_survive_a_restart() {
    let gateway = Gateway::new().await;
    let user = bokkie_protocol::UserId(1);
    gateway.admin("rob").await;
    let state = &gateway.state;
    state
        .events
        .append(
            user,
            Event::BotError {
                bot_id: bokkie_protocol::BotId(1),
                message: "a".into(),
            },
        )
        .await
        .unwrap();
    // A fresh log on the same database carries on from the stored end.
    let restarted = bokkie_gateway::events::EventLog::new(state.db.clone(), state.clock.clone());
    assert_eq!(restarted.latest(user).await.unwrap(), Seq(1));
    let next = restarted
        .append(
            user,
            Event::BotError {
                bot_id: bokkie_protocol::BotId(1),
                message: "b".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(next.seq, Seq(2));
}

#[tokio::test]
async fn snapshots_carry_their_watermark() {
    let (gateway, _address, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let list = gateway.send(Call::get("/api/v1/bots").cookie(&rob)).await;
    assert_eq!(list.body["seq"], 1);
    say(&gateway, &rob, chef, "Hi").await;
    let path = format!("/api/v1/bots/{chef}/messages");
    let page = gateway
        .wait_for(&rob, &path, |b| {
            b["messages"].as_array().is_some_and(|m| {
                m.iter()
                    .any(|m| m["state"] == "complete" && m["author"] == "bot")
            })
        })
        .await;
    let latest = gateway
        .state
        .events
        .latest(bokkie_protocol::UserId(1))
        .await
        .unwrap();
    assert!(page["seq"].as_u64().unwrap() <= latest.0);
    assert!(page["seq"].as_u64().unwrap() >= 2);
}

#[tokio::test]
async fn one_users_socket_never_carries_anothers_events() {
    let (gateway, address, rob) = setup().await;
    let (amy, _) = gateway.member(&rob, "amy").await;
    let (ben, _) = gateway.member(&rob, "ben").await;
    let mut bens = open(address, &ben, None).await;
    assert_eq!(
        next(&mut bens).await,
        Some(ServerFrame::Live { latest: Seq(0) })
    );
    let mut amys = open(address, &amy, None).await;
    next(&mut amys).await;

    let chef = create_bot(&gateway, &amy, "Chef").await;
    say(&gateway, &amy, chef, "Amy's secret").await;
    until(&mut amys, |e| matches!(e, Event::MessageCompleted { message } if message.author == bokkie_protocol::api::Author::Bot)).await;

    // Ben's own first event is his seq 1, and nothing of Amy's came before.
    create_bot(&gateway, &ben, "Mine").await;
    let first = event(&mut bens).await;
    assert_eq!(first.seq, Seq(1));
    assert!(matches!(first.event, Event::BotCreated { ref bot } if bot.name == "Mine"));
    // Replaying Ben's log from the start holds only his event.
    let mut replay = open(address, &ben, Some(Seq(0))).await;
    assert_eq!(event(&mut replay).await.seq, Seq(1));
    assert_eq!(
        next(&mut replay).await,
        Some(ServerFrame::Live { latest: Seq(1) })
    );
    // Amy's log is untouched by Ben's.
    let amy_latest = gateway
        .state
        .events
        .latest(bokkie_protocol::UserId(2))
        .await
        .unwrap();
    let amy_events = gateway
        .state
        .events
        .since(bokkie_protocol::UserId(2), Seq(0), 1000)
        .await
        .unwrap();
    assert_eq!(amy_events.len() as u64, amy_latest.0);
    assert!(
        amy_events
            .iter()
            .all(|e| !matches!(&e.event, Event::BotCreated { bot } if bot.name == "Mine"))
    );
}

#[tokio::test]
async fn signing_out_closes_the_socket() {
    let gateway = Gateway::new().await;
    let rob = gateway.admin("rob").await;
    let mut state = gateway.state.clone();
    state.session_check = Duration::from_millis(50);
    let app = bokkie_gateway::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    let mut socket = open(address, &rob, None).await;
    next(&mut socket).await;
    gateway
        .send(Call::new(Method::DELETE, "/api/v1/session").cookie(&rob))
        .await;
    assert_eq!(next(&mut socket).await, None);
}

#[tokio::test]
async fn the_client_can_ping() {
    let (_gateway, address, rob) = setup().await;
    let mut socket = open(address, &rob, None).await;
    next(&mut socket).await;
    socket
        .send(Message::text(
            serde_json::to_string(&ClientFrame::Ping).unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(next(&mut socket).await, Some(ServerFrame::Pong));
}

/// Serves a gateway whose sockets recheck their session quickly.
async fn serve_quick(gateway: &Gateway) -> SocketAddr {
    let mut state = gateway.state.clone();
    state.session_check = Duration::from_millis(50);
    let app = bokkie_gateway::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    address
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_append_neither_jams_the_log_nor_goes_unheard() {
    let (gateway, address, rob) = setup().await;
    let user = bokkie_protocol::UserId(1);
    let mut socket = open(address, &rob, None).await;
    assert_eq!(
        next(&mut socket).await,
        Some(ServerFrame::Live { latest: Seq(0) })
    );
    let noise = |message: &str| Event::BotError {
        bot_id: bokkie_protocol::BotId(1),
        message: message.into(),
    };

    // Prime the cached sequence number.
    gateway
        .state
        .events
        .append(user, noise("zero"))
        .await
        .unwrap();
    assert_eq!(event(&mut socket).await.seq, Seq(1));

    // Hold the write lock, so the append's insert waits.
    let mut blocker = gateway.state.db.acquire().await.unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let events = gateway.state.events.clone();
    let first = noise("first");
    let append = tokio::spawn(async move { events.append(user, first).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    // The caller goes away, as a disconnecting client's request does.
    append.abort();
    sqlx::query("COMMIT").execute(&mut *blocker).await.unwrap();
    drop(blocker);

    let second = gateway
        .state
        .events
        .append(user, noise("second"))
        .await
        .expect("the log still takes events");
    assert_eq!(second.seq, Seq(3));
    let seqs = [event(&mut socket).await.seq, event(&mut socket).await.seq];
    assert_eq!(seqs, [Seq(2), Seq(3)]);
}

#[tokio::test]
async fn multibyte_deltas_carry_character_offsets() {
    let (gateway, address, rob) = setup().await;
    let pieces = ["héllo ", "🌍🌏", " wörld", " ✓"];
    gateway.fake.with(|b| {
        b.reply = pieces.iter().map(|p| p.to_string()).collect();
        b.finish_turns = false;
    });
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let mut socket = open(address, &rob, None).await;
    next(&mut socket).await;
    say(&gateway, &rob, chef, "Greet").await;
    let expected: String = pieces.concat();
    let mut text = String::new();
    while text != expected {
        if let Event::MessageDelta { offset, delta, .. } = event(&mut socket).await.event {
            assert_eq!(offset, text.chars().count() as u64, "{text:?}");
            text.push_str(&delta);
        }
    }
    let page = gateway
        .send(Call::get(&format!("/api/v1/bots/{chef}/messages")).cookie(&rob))
        .await;
    let stored = page.body["messages"][1]["text"].as_str().unwrap();
    assert_eq!(stored, expected);
}

#[tokio::test]
async fn disabling_an_account_closes_its_socket() {
    let gateway = Gateway::new().await;
    let rob = gateway.admin("rob").await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let address = serve_quick(&gateway).await;
    let mut socket = open(address, &amy, None).await;
    next(&mut socket).await;
    let reply = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/admin/users/{amy_id}"))
                .cookie(&rob)
                .json(&json!({"disabled": true})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(next(&mut socket).await, None);
}

#[tokio::test]
async fn a_change_and_its_event_commit_together_or_not_at_all() {
    let (gateway, address, rob) = setup().await;
    let user = bokkie_protocol::UserId(1);
    // The event cannot be stored, so the Bot is not created either.
    sqlx::query(
        "CREATE TRIGGER broken_events BEFORE INSERT ON events
         BEGIN SELECT RAISE(ABORT, 'disk full'); END",
    )
    .execute(&gateway.state.db)
    .await
    .unwrap();
    let failed = gateway
        .send(Call::post("/api/v1/bots").cookie(&rob).json(&json!({
            "name": "Chef", "label": "", "description": ""})))
        .await;
    assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR);
    sqlx::query("DROP TRIGGER broken_events")
        .execute(&gateway.state.db)
        .await
        .unwrap();
    let list = gateway.send(Call::get("/api/v1/bots").cookie(&rob)).await;
    assert_eq!(list.body["bots"], json!([]), "no Bot without its event");

    // A request dropped between its change and its commit leaves neither.
    let mut batch = gateway.state.events.begin(user).await.unwrap();
    let bot = bokkie_gateway::bots::store::create_bot(
        &mut *batch,
        gateway.state.clock.now(),
        user,
        None,
        "Tutor",
        "",
        "",
        None,
    )
    .await
    .unwrap();
    batch
        .emit(Event::BotCreated {
            bot: bot.to_api(false, None),
        })
        .await
        .unwrap();
    drop(batch);
    let list = gateway.send(Call::get("/api/v1/bots").cookie(&rob)).await;
    assert_eq!(list.body["bots"], json!([]));
    assert_eq!(list.body["seq"], 0);

    // The log carries on from where it was.
    let mut socket = open(address, &rob, None).await;
    assert_eq!(
        next(&mut socket).await,
        Some(ServerFrame::Live { latest: Seq(0) })
    );
    create_bot(&gateway, &rob, "Tutor").await;
    let created = event(&mut socket).await;
    assert_eq!(created.seq, Seq(1));
    assert!(matches!(created.event, Event::BotCreated { .. }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_edits_to_one_bot_are_announced_in_the_order_they_took_effect() {
    let (gateway, _, rob) = setup().await;
    let user = bokkie_protocol::UserId(1);
    let chef = create_bot(&gateway, &rob, "Chef").await;
    for round in 0..20 {
        let edits = (0..8)
            .map(|edit| {
                Call::new(Method::PATCH, &format!("/api/v1/bots/{chef}"))
                    .cookie(&rob)
                    .json(&json!({"label": format!("round {round} edit {edit}")}))
            })
            .collect();
        for reply in gateway.send_all(edits).await {
            assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
        }
        let stored = gateway
            .send(Call::get(&format!("/api/v1/bots/{chef}")).cookie(&rob))
            .await;
        let log = gateway
            .state
            .events
            .since(user, Seq(0), 10_000)
            .await
            .unwrap();
        let announced = log
            .iter()
            .rev()
            .find_map(|e| match &e.event {
                Event::BotUpdated { bot } => Some(bot.label.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            stored.body["label"], announced,
            "the last event must show the Bot as stored"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edit_waits_for_the_change_before_it_and_is_announced_after_it() {
    let (gateway, _, rob) = setup().await;
    let user = bokkie_protocol::UserId(1);
    let chef = create_bot(&gateway, &rob, "Chef").await;
    // An earlier change to the Bot, still in flight.
    let mut earlier = gateway.state.events.begin(user).await.unwrap();
    let bot = bokkie_gateway::bots::store::update_bot(
        &mut *earlier,
        gateway.state.clock.now(),
        user,
        bokkie_protocol::BotId(chef),
        bokkie_gateway::bots::store::BotChanges {
            label: Some("first".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    earlier
        .emit(Event::BotUpdated {
            bot: bot.to_api(false, None),
        })
        .await
        .unwrap();

    let later = gateway.spawn(
        Call::new(Method::PATCH, &format!("/api/v1/bots/{chef}"))
            .cookie(&rob)
            .json(&json!({"label": "second"})),
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !later.is_finished(),
        "the edit waits for the earlier change"
    );
    earlier.commit().await.unwrap();
    assert_eq!(later.await.unwrap().status, StatusCode::OK);

    let labels: Vec<String> = gateway
        .state
        .events
        .since(user, Seq(0), 100)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::BotUpdated { bot } => Some(bot.label),
            _ => None,
        })
        .collect();
    assert_eq!(labels, ["first", "second"]);
}

/// Reads frames until the socket closes. Returns the events seen, any
/// other frames, and the close code.
async fn drain(socket: &mut Socket) -> (Vec<EventEnvelope>, Vec<ServerFrame>, Option<u16>) {
    let (mut events, mut others) = (Vec::new(), Vec::new());
    loop {
        let message = tokio::time::timeout(Duration::from_secs(30), socket.next())
            .await
            .expect("a frame arrives in time");
        match message {
            Some(Ok(Message::Text(text))) => match serde_json::from_str(&text).unwrap() {
                ServerFrame::Event(envelope) => events.push(envelope),
                other => others.push(other),
            },
            Some(Ok(Message::Close(frame))) => {
                return (events, others, frame.map(|f| u16::from(f.code)));
            }
            Some(Ok(_)) => {}
            Some(Err(_)) | None => return (events, others, None),
        }
    }
}

/// Serves the gateway with its own event log, so a test can set the log's
/// limits. Events go to that log through the returned state.
async fn serve_with_log(
    gateway: &Gateway,
    retention: u64,
    capacity: usize,
) -> (bokkie_gateway::AppState, SocketAddr) {
    let mut state = gateway.state.clone();
    state.events = std::sync::Arc::new(bokkie_gateway::events::EventLog::with_limits(
        state.db.clone(),
        state.clock.clone(),
        retention,
        capacity,
    ));
    let app = bokkie_gateway::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    (state, address)
}

/// An event big enough that a few hundred fill the socket's buffers.
fn bulky(size: usize) -> Event {
    Event::BotError {
        bot_id: bokkie_protocol::BotId(1),
        message: "x".repeat(size),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_that_falls_behind_is_closed_and_resumes_from_the_log() {
    let gateway = Gateway::new().await;
    let rob = gateway.admin("rob").await;
    let user = bokkie_protocol::UserId(1);
    let (state, address) = serve_with_log(&gateway, 5_000, 4).await;
    let mut socket = open(address, &rob, None).await;
    assert_eq!(
        next(&mut socket).await,
        Some(ServerFrame::Live { latest: Seq(0) })
    );
    // The client stops reading; the socket's buffers fill, and the log
    // runs far more than four events ahead of the connection.
    for _ in 0..100 {
        state.events.append(user, bulky(256 * 1024)).await.unwrap();
    }
    let (seen, _, code) = drain(&mut socket).await;
    assert_eq!(code, Some(4001), "closed as lagged");
    let last = seen.last().map_or(Seq(0), |e| e.seq);
    assert!(last < Seq(100), "it fell behind at {last:?}");
    for (i, envelope) in seen.iter().enumerate() {
        assert_eq!(envelope.seq, Seq(1 + i as u64), "no gaps before the close");
    }

    // It resumes from its last event and gets the rest from the log.
    let mut socket = open(address, &rob, Some(last)).await;
    let mut seqs = Vec::new();
    loop {
        match next(&mut socket).await.unwrap() {
            ServerFrame::Event(envelope) => seqs.push(envelope.seq.0),
            ServerFrame::Live { latest } => {
                assert_eq!(latest, Seq(100));
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(seqs, ((last.0 + 1)..=100).collect::<Vec<_>>());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_prune_during_replay_resets_the_client() {
    let gateway = Gateway::new().await;
    let rob = gateway.admin("rob").await;
    let user = bokkie_protocol::UserId(1);
    let (state, address) = serve_with_log(&gateway, 600, 2_048).await;
    // More than one replay page (500 events), too much to buffer.
    for _ in 0..600 {
        state.events.append(user, bulky(100 * 1024)).await.unwrap();
    }
    // The client resumes from the start, then stops reading: the replay
    // stalls in its first page.
    let mut socket = open(address, &rob, Some(Seq(0))).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Meanwhile the log grows and prunes everything up to 680, including
    // events the replay has not read yet.
    for _ in 600..1_280 {
        state.events.append(user, bulky(16)).await.unwrap();
    }
    assert_eq!(state.events.earliest(user).await.unwrap(), Some(Seq(681)));
    let mut replayed = Vec::new();
    let reset = loop {
        match next(&mut socket).await.unwrap() {
            ServerFrame::Event(envelope) => replayed.push(envelope.seq.0),
            ServerFrame::Reset { after } => break after,
            other => panic!("unexpected {other:?}"),
        }
    };
    // It replayed the page it had read, with no hole, and then reset
    // rather than skip the pruned events.
    assert_eq!(replayed, (1..=500).collect::<Vec<_>>());
    assert_eq!(reset, Seq(600));
}

#[tokio::test]
async fn an_admin_revoking_a_members_sessions_closes_their_socket() {
    let gateway = Gateway::new().await;
    gateway.route().await;
    let rob = gateway.admin("rob").await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let address = serve_quick(&gateway).await;
    let mut socket = open(address, &amy, None).await;
    assert!(matches!(
        next(&mut socket).await,
        Some(ServerFrame::Live { .. })
    ));
    let mut admin = open(address, &rob, None).await;
    assert!(matches!(
        next(&mut admin).await,
        Some(ServerFrame::Live { .. })
    ));
    // Resetting a password revokes every session of the account.
    let reset = gateway
        .send(
            Call::new(
                Method::PUT,
                &format!("/api/v1/admin/users/{amy_id}/password"),
            )
            .cookie(&rob)
            .json(&json!({"new_password": "another long password"})),
        )
        .await;
    assert_eq!(reset.status, StatusCode::NO_CONTENT);
    let (_, _, code) = drain(&mut socket).await;
    assert_eq!(code, Some(4002), "closed as signed out");
    // Rob's socket, open throughout, survives several rechecks after the
    // reset and still answers; a fresh one opens too.
    tokio::time::sleep(Duration::from_millis(200)).await;
    admin
        .send(Message::text(
            serde_json::to_string(&ClientFrame::Ping).unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(next(&mut admin).await, Some(ServerFrame::Pong));
    let mut fresh = open(address, &rob, None).await;
    assert!(matches!(
        next(&mut fresh).await,
        Some(ServerFrame::Live { .. })
    ));
}
