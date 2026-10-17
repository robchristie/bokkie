//! The scripted end-to-end check for `deploy/e2e/`.
//!
//! Against a running gateway it signs in, checks that the account's Codex
//! app server is signed in to ChatGPT and passes the gateway's checks,
//! creates a Bot, opens the event WebSocket, sends a message asking for a
//! token, and waits for the Bot's reply to stream in over the socket. It
//! prints one JSON result line and exits 0 only if every check passed.
//!
//! Settings come from the environment:
//!
//! | Variable | Meaning |
//! | --- | --- |
//! | `BOKKIE_E2E_ADDR` | The gateway's address, such as `127.0.0.1:8080` |
//! | `BOKKIE_ORIGIN` | The gateway's origin; its host is sent as `Host` |
//! | `BOKKIE_E2E_USER`, `BOKKIE_E2E_PASSWORD` | The account to sign in as |
//! | `BOKKIE_E2E_TIMEOUT_SECS` | How long to wait for the reply (default 180) |
//!
//! It speaks plain HTTP/1.1 itself, so it needs nothing in the container
//! beyond the binary.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use bokkie_protocol::api::{Author, BotStatus, MessageState};
use bokkie_protocol::events::{ClientFrame, Event, ServerFrame};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

type Failure = String;

struct Gateway {
    address: String,
    host: String,
    origin: String,
    cookie: Option<String>,
}

struct Reply {
    status: u16,
    set_cookie: Option<String>,
    body: Value,
}

impl Gateway {
    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply, Failure> {
        let mut stream = tokio::net::TcpStream::connect(&self.address)
            .await
            .map_err(|e| format!("connect {}: {e}", self.address))?;
        let body = body.map(Value::to_string).unwrap_or_default();
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nOrigin: {}\r\nConnection: close\r\n",
            self.host, self.origin
        );
        if let Some(cookie) = &self.cookie {
            request.push_str(&format!("Cookie: {cookie}\r\n"));
        }
        if !body.is_empty() {
            request.push_str(&format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                body.len()
            ));
        }
        request.push_str("\r\n");
        request.push_str(&body);
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        let mut raw = Vec::new();
        tokio::time::timeout(Duration::from_secs(120), stream.read_to_end(&mut raw))
            .await
            .map_err(|_| format!("{method} {path} timed out"))?
            .map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&raw);
        let (head, payload) = text
            .split_once("\r\n\r\n")
            .ok_or_else(|| format!("{method} {path}: malformed response"))?;
        let status = head
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("{method} {path}: no status"))?;
        let set_cookie = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("set-cookie")
                    .then(|| value.trim())
            })
            .and_then(|value| value.split(';').next())
            .map(str::to_owned);
        let chunked = head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked");
        let payload = if chunked {
            dechunk(payload)
        } else {
            payload.to_owned()
        };
        let body = serde_json::from_str(&payload).unwrap_or(Value::Null);
        Ok(Reply {
            status,
            set_cookie,
            body,
        })
    }
}

fn dechunk(mut payload: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = payload.split_once("\r\n") {
        let Ok(size) = usize::from_str_radix(size.trim(), 16) else {
            break;
        };
        if size == 0 || rest.len() < size {
            break;
        }
        out.push_str(&rest[..size]);
        payload = rest[size..].trim_start_matches("\r\n");
    }
    out
}

fn env(name: &str) -> Result<String, Failure> {
    std::env::var(name).map_err(|_| format!("{name} is required"))
}

fn expect(ok: bool, what: impl FnOnce() -> String) -> Result<(), Failure> {
    if ok { Ok(()) } else { Err(what()) }
}

async fn run(result: &mut serde_json::Map<String, Value>) -> Result<(), Failure> {
    let address = env("BOKKIE_E2E_ADDR")?;
    let origin = env("BOKKIE_ORIGIN")?.trim_end_matches('/').to_owned();
    let host = origin
        .split_once("://")
        .map(|(_, host)| host.to_owned())
        .ok_or("BOKKIE_ORIGIN must be scheme://host")?;
    let timeout = Duration::from_secs(
        std::env::var("BOKKIE_E2E_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(180),
    );
    let mut gateway = Gateway {
        address: address.clone(),
        host: host.clone(),
        origin: origin.clone(),
        cookie: None,
    };

    // The gateway may still be starting.
    let started = Instant::now();
    loop {
        match gateway.call("GET", "/healthz", None).await {
            Ok(reply) if reply.status == 200 => break,
            _ if started.elapsed() < Duration::from_secs(60) => {
                tokio::time::sleep(Duration::from_millis(500)).await
            }
            other => return Err(format!("the gateway did not come up: {:?}", other.err())),
        }
    }

    let login = gateway
        .call(
            "POST",
            "/api/v1/session",
            Some(&json!({"username": env("BOKKIE_E2E_USER")?, "password": env("BOKKIE_E2E_PASSWORD")?})),
        )
        .await?;
    expect(login.status == 200, || {
        format!("sign-in answered {}", login.status)
    })?;
    gateway.cookie = login.set_cookie;
    expect(gateway.cookie.is_some(), || "sign-in set no cookie".into())?;

    // This starts the user's app server, which must pass the gateway's
    // startup checks, and must be signed in to ChatGPT.
    let account = gateway.call("GET", "/api/v1/codex/account", None).await?;
    result.insert("codex_account".into(), account.body.clone());
    expect(account.status == 200, || {
        format!(
            "the Codex account answered {}: {}",
            account.status, account.body
        )
    })?;
    expect(account.body["signed_in"] == true, || {
        "the account's app server is not signed in to ChatGPT".into()
    })?;

    let usage = gateway.call("GET", "/api/v1/usage", None).await?;
    result.insert(
        "subscription_buckets".into(),
        json!(usage.body["subscription"].as_array().map(Vec::len)),
    );

    let nonce = format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    );
    let token = format!("bokkie-e2e-{nonce}");
    let bot = gateway
        .call(
            "POST",
            "/api/v1/bots",
            Some(&json!({
                "name": format!("E2E {}", &nonce[nonce.len().saturating_sub(6)..]),
                "label": "End-to-end check",
                "description": "Answer briefly. When asked for a token, reply with exactly that token.",
            })),
        )
        .await?;
    expect(bot.status == 201, || {
        format!("creating a Bot answered {}: {}", bot.status, bot.body)
    })?;
    let bot_id = bot.body["id"].as_i64().ok_or("the Bot has no id")?;
    result.insert("bot".into(), json!(bot_id));

    let mut request = format!("ws://{address}/api/v1/events")
        .into_client_request()
        .map_err(|e| e.to_string())?;
    let headers = request.headers_mut();
    headers.insert("host", host.parse().map_err(|_| "bad host")?);
    headers.insert("origin", origin.parse().map_err(|_| "bad origin")?);
    headers.insert(
        "cookie",
        gateway
            .cookie
            .as_deref()
            .unwrap_or_default()
            .parse()
            .map_err(|_| "bad cookie")?,
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("opening the event socket: {e}"))?;
    let resume = serde_json::to_string(&ClientFrame::Resume { after: None }).unwrap_or_default();
    socket
        .send(Message::text(resume))
        .await
        .map_err(|e| e.to_string())?;

    let sent = gateway
        .call(
            "POST",
            &format!("/api/v1/bots/{bot_id}/messages"),
            Some(&json!({"text": format!("Reply with exactly this token and nothing else: {token}")})),
        )
        .await?;
    expect(sent.status == 200, || {
        format!("sending answered {}: {}", sent.status, sent.body)
    })?;

    let mut deltas = 0u64;
    let mut streamed = String::new();
    let mut working = false;
    let mut first_seq = None;
    let started = Instant::now();
    let reply = loop {
        let remaining = timeout
            .checked_sub(started.elapsed())
            .ok_or("no reply in time")?;
        let frame = tokio::time::timeout(remaining, socket.next())
            .await
            .map_err(|_| "no reply in time")?
            .ok_or("the event socket closed")?
            .map_err(|e| format!("the event socket failed: {e}"))?;
        let Message::Text(text) = frame else { continue };
        let Ok(ServerFrame::Event(envelope)) = serde_json::from_str(&text) else {
            continue;
        };
        first_seq.get_or_insert(envelope.seq.0);
        match envelope.event {
            Event::BotStatusChanged {
                status: BotStatus::Working,
                ..
            } => working = true,
            Event::BotStatusChanged {
                status: BotStatus::Refused,
                ..
            } => return Err("the gateway refused the Bot's thread".into()),
            Event::MessageDelta { offset, delta, .. } => {
                expect(offset == streamed.chars().count() as u64, || {
                    format!(
                        "a delta started at {offset}, not {}",
                        streamed.chars().count()
                    )
                })?;
                streamed.push_str(&delta);
                deltas += 1;
            }
            Event::MessageCompleted { message } if message.author == Author::Bot => {
                result.insert("last_seq".into(), json!(envelope.seq.0));
                break message;
            }
            Event::BotError { message, .. } => return Err(format!("Bot error: {message}")),
            _ => {}
        }
    };
    result.insert("first_seq".into(), json!(first_seq));
    result.insert("deltas".into(), json!(deltas));
    result.insert("seconds".into(), json!(started.elapsed().as_secs_f64()));
    result.insert("reply_chars".into(), json!(reply.text.chars().count()));
    result.insert("token_echoed".into(), json!(reply.text.contains(&token)));
    expect(working, || "no working status arrived".into())?;
    expect(reply.state == MessageState::Complete, || {
        format!("the reply ended {:?}", reply.state)
    })?;
    expect(deltas > 0, || "the reply did not stream".into())?;
    expect(reply.text.starts_with(&streamed), || {
        "the streamed text is not a prefix of the reply".into()
    })?;
    expect(reply.text.contains(&token), || {
        "the reply lacks the token".into()
    })?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let mut result = serde_json::Map::new();
    let outcome = run(&mut result).await;
    result.insert("pass".into(), json!(outcome.is_ok()));
    if let Err(error) = &outcome {
        result.insert("error".into(), json!(error));
    }
    println!("{}", Value::Object(result));
    if outcome.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
