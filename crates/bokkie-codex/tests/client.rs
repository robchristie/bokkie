//! Exercises the transport against an in-memory fake app server.

use std::time::Duration;

use bokkie_codex::protocol::*;
use bokkie_codex::{Client, Error, Incoming, RequestId, RpcError};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};

struct FakeServer {
    lines: Lines<BufReader<tokio::io::ReadHalf<DuplexStream>>>,
    writer: tokio::io::WriteHalf<DuplexStream>,
}

impl FakeServer {
    async fn read(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(5), self.lines.next_line())
            .await
            .expect("client wrote in time")
            .unwrap()
            .expect("client stream open");
        serde_json::from_str(&line).unwrap()
    }

    async fn write(&mut self, value: Value) {
        let mut line = value.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await.unwrap();
    }
}

fn connect() -> (
    Client,
    tokio::sync::mpsc::UnboundedReceiver<Incoming>,
    FakeServer,
) {
    let (client_side, server_side) = tokio::io::duplex(1 << 16);
    let (client_read, client_write) = tokio::io::split(client_side);
    let (server_read, server_write) = tokio::io::split(server_side);
    let (client, incoming) = Client::new(client_read, client_write);
    let server = FakeServer {
        lines: BufReader::new(server_read).lines(),
        writer: server_write,
    };
    (client, incoming, server)
}

#[tokio::test]
async fn initialize_sends_the_handshake_and_initialized() {
    let (client, _incoming, mut server) = connect();
    let call = tokio::spawn(async move {
        client
            .initialize(&InitializeParams {
                client_info: ClientInfo {
                    name: "bokkie".into(),
                    version: "0".into(),
                    title: None,
                },
                capabilities: Some(InitializeCapabilities {
                    experimental_api: true,
                    opt_out_notification_methods: None,
                }),
            })
            .await
    });
    let request = server.read().await;
    assert_eq!(request["method"], "initialize");
    assert_eq!(request["params"]["capabilities"]["experimentalApi"], true);
    assert!(request.get("jsonrpc").is_none());
    server
        .write(json!({
            "id": request["id"],
            "result": {
                "codexHome": "/data/users/u/codex",
                "platformFamily": "unix",
                "platformOs": "linux",
                "userAgent": "codex/0.163.0",
            }
        }))
        .await;
    let response = call.await.unwrap().unwrap();
    assert_eq!(response.codex_home, "/data/users/u/codex");
    assert_eq!(server.read().await, json!({"method": "initialized"}));
}

#[tokio::test]
async fn responses_are_matched_by_id_in_any_order() {
    let (client, _incoming, mut server) = connect();
    let first = tokio::spawn({
        let client = client.clone();
        async move { client.request_value("a", None).await }
    });
    let first_request = server.read().await;
    let second = tokio::spawn({
        let client = client.clone();
        async move { client.request_value("b", Some(json!({"x": 1}))).await }
    });
    let second_request = server.read().await;
    assert_ne!(first_request["id"], second_request["id"]);
    server
        .write(json!({"id": second_request["id"], "result": "second"}))
        .await;
    server
        .write(json!({"id": first_request["id"], "result": "first"}))
        .await;
    assert_eq!(first.await.unwrap().unwrap(), "first");
    assert_eq!(second.await.unwrap().unwrap(), "second");
}

#[tokio::test]
async fn error_responses_become_rpc_errors() {
    let (client, _incoming, mut server) = connect();
    let call = tokio::spawn(async move { client.account_rate_limits_read().await });
    let request = server.read().await;
    assert_eq!(request["method"], "account/rateLimits/read");
    assert!(request.get("params").is_none());
    server
        .write(json!({
            "id": request["id"],
            "error": {"code": -32000, "message": "not signed in"},
        }))
        .await;
    match call.await.unwrap() {
        Err(Error::Rpc(error)) => assert_eq!(error.message, "not signed in"),
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn notifications_and_server_requests_are_delivered() {
    let (client, mut incoming, mut server) = connect();
    server
        .write(json!({
            "method": "item/agentMessage/delta",
            "params": {"threadId": "t", "turnId": "u", "itemId": "i", "delta": "Hel"},
        }))
        .await;
    server
        .write(json!({"method": "thread/started", "params": {"anything": 1}}))
        .await;
    server
        .write(json!({
            "id": 99,
            "method": "item/tool/call",
            "params": {
                "threadId": "t", "turnId": "u", "callId": "c",
                "tool": "message_bot", "arguments": {"to": "Chef"},
            },
        }))
        .await;
    server
        .write(json!({"id": 100, "method": "currentTime/read"}))
        .await;
    server.writer.write_all(b"not json\n").await.unwrap();

    let Some(Incoming::Notification(ServerNotification::AgentMessageDelta(delta))) =
        incoming.recv().await
    else {
        panic!("expected a delta");
    };
    assert_eq!(delta.delta, "Hel");
    assert!(matches!(
        incoming.recv().await,
        Some(Incoming::Notification(ServerNotification::Unknown { method, .. }))
            if method == "thread/started"
    ));
    let Some(Incoming::Request(request)) = incoming.recv().await else {
        panic!("expected a tool call");
    };
    let ServerRequestKind::DynamicToolCall(call) = request.kind else {
        panic!("expected a tool call");
    };
    assert_eq!(call.arguments["to"], "Chef");
    client
        .respond(request.id, &DynamicToolCallResponse::text(true, "sent"))
        .await
        .unwrap();
    assert_eq!(
        server.read().await,
        json!({"id": 99, "result": {
            "success": true,
            "contentItems": [{"type": "inputText", "text": "sent"}],
        }})
    );

    let Some(Incoming::Request(unknown)) = incoming.recv().await else {
        panic!("expected an unknown request");
    };
    assert_eq!(unknown.id, RequestId::Integer(100));
    client
        .respond_error(unknown.id, RpcError::method_not_found("currentTime/read"))
        .await
        .unwrap();
    assert_eq!(server.read().await["error"]["code"], -32601);
    assert!(matches!(
        incoming.recv().await,
        Some(Incoming::Malformed { .. })
    ));
}

#[tokio::test]
async fn closing_the_stream_fails_pending_and_later_requests() {
    let (client, mut incoming, mut server) = connect();
    let call = tokio::spawn({
        let client = client.clone();
        async move { client.request_value("thread/start", None).await }
    });
    server.read().await;
    drop(server);
    assert!(matches!(call.await.unwrap(), Err(Error::Closed)));
    assert!(incoming.recv().await.is_none());
    assert!(matches!(
        client.request_value("model/list", None).await,
        Err(Error::Closed) | Err(Error::Io(_))
    ));
}

#[tokio::test]
async fn a_cancelled_request_leaves_the_connection_usable() {
    let (client, _incoming, mut server) = connect();
    // Cancel a request before its answer arrives.
    let cancelled = tokio::time::timeout(
        Duration::from_millis(50),
        client.request_value("thread/start", None),
    )
    .await;
    assert!(cancelled.is_err(), "the request timed out");
    let abandoned = server.read().await;
    // A late answer to the abandoned request is ignored.
    server
        .write(json!({"id": abandoned["id"], "result": "late"}))
        .await;
    let next = tokio::spawn({
        let client = client.clone();
        async move { client.request_value("model/list", None).await }
    });
    let request = server.read().await;
    assert_eq!(request["method"], "model/list");
    server
        .write(json!({"id": request["id"], "result": "fresh"}))
        .await;
    assert_eq!(next.await.unwrap().unwrap(), "fresh");
}

#[tokio::test]
async fn an_undecodable_server_request_keeps_its_id() {
    let (_client, mut incoming, mut server) = connect();
    server
        .write(json!({"id": 5, "method": "item/tool/call", "params": {"tool": 1}}))
        .await;
    let Some(Incoming::MalformedRequest { id, method, .. }) = incoming.recv().await else {
        panic!("expected a malformed request");
    };
    assert_eq!(id, RequestId::Integer(5));
    assert_eq!(method, "item/tool/call");
}

#[tokio::test]
async fn an_undecodable_response_fails_only_its_request() {
    let (client, mut incoming, mut server) = connect();
    let call = tokio::spawn({
        let client = client.clone();
        async move { client.request_value("config/read", None).await }
    });
    let request = server.read().await;
    server
        .write(json!({"id": request["id"], "error": {"code": "not a number"}}))
        .await;
    match call.await.unwrap() {
        Err(Error::Rpc(error)) => assert_eq!(error.code, RpcError::PARSE_ERROR),
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(
        incoming.recv().await,
        Some(Incoming::Malformed { .. })
    ));
    // Invalid UTF-8 is reported without closing the connection.
    server.writer.write_all(b"\xff\xfe\n").await.unwrap();
    assert!(matches!(
        incoming.recv().await,
        Some(Incoming::Malformed { .. })
    ));
    let next = tokio::spawn(async move { client.request_value("model/list", None).await });
    let request = server.read().await;
    server
        .write(json!({"id": request["id"], "result": 1}))
        .await;
    assert_eq!(next.await.unwrap().unwrap(), 1);
}

#[tokio::test]
async fn an_over_long_line_closes_the_connection() {
    let (client_side, server_side) = tokio::io::duplex(1 << 16);
    let (client_read, client_write) = tokio::io::split(client_side);
    let (server_read, mut server_write) = tokio::io::split(server_side);
    let (client, mut incoming) = Client::with_max_line(client_read, client_write, 1024);
    let mut lines = BufReader::new(server_read).lines();
    let call = tokio::spawn({
        let client = client.clone();
        async move { client.request_value("thread/resume", None).await }
    });
    lines.next_line().await.unwrap().unwrap();
    // A line exactly at the bound still decodes.
    let fits = format!(
        r#"{{"method":"warning","params":{{"pad":"{}"}}}}"#,
        "x".repeat(960)
    );
    assert!(fits.len() <= 1024);
    server_write
        .write_all(format!("{fits}\n").as_bytes())
        .await
        .unwrap();
    match incoming.recv().await.unwrap() {
        Incoming::Notification(ServerNotification::Unknown { method, .. }) => {
            assert_eq!(method, "warning")
        }
        other => panic!("unexpected {other:?}"),
    }
    // A runaway line is cut off without buffering it whole.
    let runaway = "y".repeat(64 * 1024);
    server_write.write_all(runaway.as_bytes()).await.unwrap();
    match incoming.recv().await.unwrap() {
        Incoming::Malformed { line, error } => {
            assert!(line.len() <= 256);
            assert!(error.contains("1024"), "{error}");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(call.await.unwrap(), Err(Error::Closed)));
    assert!(incoming.recv().await.is_none());
}

#[tokio::test]
async fn turn_steer_names_the_expected_turn() {
    let (client, _incoming, mut server) = connect();
    let call = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .turn_steer(&TurnSteerParams {
                    thread_id: "t".into(),
                    expected_turn_id: "u".into(),
                    input: vec![UserInput::Text {
                        text: "also".into(),
                    }],
                    additional_context: None,
                })
                .await
        }
    });
    let request = server.read().await;
    assert_eq!(request["method"], "turn/steer");
    assert_eq!(request["params"]["expectedTurnId"], "u");
    server
        .write(json!({"id": request["id"], "result": {"turnId": "u"}}))
        .await;
    assert_eq!(call.await.unwrap().unwrap().turn_id, "u");
}
