//! The event WebSocket: the signed-in user's event log, replayed from where
//! the client left off and then live. See `bokkie_protocol::events` for the
//! protocol.
//!
//! The request guard has already checked `Host` and `Origin` on the
//! upgrade, and [`SignedIn`] the session cookie. While the socket is open the
//! session is rechecked periodically, so signing out, revocation or an
//! account being disabled closes it.

use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::response::Response;
use bokkie_protocol::events::{ClientFrame, ServerFrame};
use tokio::sync::broadcast::error::RecvError;

use crate::AppState;
use crate::accounts;
use crate::auth::{self, SignedIn};
use crate::bots::store;

/// How long the client has to send its first frame.
const RESUME_TIMEOUT: Duration = Duration::from_secs(10);
/// How long one frame may take to send before the client counts as stuck.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// How many events one replay query reads.
const REPLAY_PAGE: u32 = 500;

/// Close codes, in the range reserved for applications.
const CLOSE_PROTOCOL: u16 = 4000;
const CLOSE_LAGGED: u16 = 4001;
const CLOSE_SIGNED_OUT: u16 = 4002;

/// `GET /api/v1/events`.
pub async fn events(
    State(state): State<AppState>,
    signed_in: SignedIn,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    // The cookie may be gone by the time a recheck runs; keep the token.
    let token = auth::session_token(&headers).unwrap_or_default();
    upgrade
        .max_message_size(16 * 1024)
        .on_upgrade(move |socket| async move {
            let user = signed_in.id();
            if let Err(reason) = serve(&state, signed_in, &token, socket).await {
                tracing::debug!(user = %user, reason, "event socket closed");
            }
        })
}

async fn send(socket: &mut WebSocket, frame: &ServerFrame) -> Result<(), &'static str> {
    let text = serde_json::to_string(frame).map_err(|_| "encoding failed")?;
    send_raw(socket, Message::Text(text.into())).await
}

async fn send_raw(socket: &mut WebSocket, message: Message) -> Result<(), &'static str> {
    tokio::time::timeout(SEND_TIMEOUT, socket.send(message))
        .await
        .map_err(|_| "the client stopped reading")?
        .map_err(|_| "the connection failed")
}

async fn close(
    socket: &mut WebSocket,
    code: u16,
    reason: &'static str,
) -> Result<(), &'static str> {
    let _ = tokio::time::timeout(
        SEND_TIMEOUT,
        socket.send(Message::Close(Some(CloseFrame {
            code,
            reason: Utf8Bytes::from_static(reason),
        }))),
    )
    .await;
    Err(reason)
}

async fn serve(
    state: &AppState,
    signed_in: SignedIn,
    token: &str,
    mut socket: WebSocket,
) -> Result<(), &'static str> {
    let user = signed_in.id();
    let after = match tokio::time::timeout(RESUME_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str(&text) {
            Ok(ClientFrame::Resume { after }) => after,
            _ => return close(&mut socket, CLOSE_PROTOCOL, "expected resume").await,
        },
        _ => return close(&mut socket, CLOSE_PROTOCOL, "expected resume").await,
    };

    // Subscribe before reading the log, so nothing falls between the two.
    let mut live = state.events.subscribe(user);
    let latest = state
        .events
        .latest(user)
        .await
        .map_err(|_| "log unavailable")?;
    let mut sent = match after {
        None => latest,
        Some(after) => {
            let earliest = state
                .events
                .earliest(user)
                .await
                .map_err(|_| "log unavailable")?;
            let dropped = after < latest && earliest.is_none_or(|e| after.next() < e);
            if after > latest || dropped {
                send(&mut socket, &ServerFrame::Reset { after: latest }).await?;
                latest
            } else {
                let mut sent = after;
                while sent < latest {
                    let page = state
                        .events
                        .since(user, sent, REPLAY_PAGE)
                        .await
                        .map_err(|_| "log unavailable")?;
                    let Some(last) = page.last() else { break };
                    let last = last.seq;
                    // A prune between the earliest check and this read
                    // would leave a hole: start over from a snapshot.
                    if page.first().is_some_and(|first| first.seq != sent.next()) {
                        send(&mut socket, &ServerFrame::Reset { after: latest }).await?;
                        break;
                    }
                    for envelope in page {
                        if envelope.seq > latest {
                            break;
                        }
                        send(&mut socket, &ServerFrame::Event(envelope)).await?;
                    }
                    sent = last.min(latest);
                }
                latest
            }
        }
    };
    send(&mut socket, &ServerFrame::Live { latest: sent }).await?;

    // What this connection says it shows, forgotten when it closes.
    let viewer = state.presence.connect(user);

    let mut check = tokio::time::interval(state.session_check);
    check.tick().await;
    loop {
        tokio::select! {
            event = live.recv() => match event {
                Ok(envelope) => {
                    // Events up to `sent` were replayed already. Anything
                    // but the next one is a hole: let the client resume
                    // from the log, which has it.
                    if envelope.seq > sent {
                        if envelope.seq != sent.next() {
                            return close(&mut socket, CLOSE_LAGGED, "missed an event; resume").await;
                        }
                        sent = envelope.seq;
                        send(&mut socket, &ServerFrame::Event(envelope)).await?;
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    return close(&mut socket, CLOSE_LAGGED, "fell behind; resume").await;
                }
                Err(RecvError::Closed) => return Ok(()),
            },
            frame = socket.recv() => match frame {
                Some(Ok(Message::Text(text))) => match serde_json::from_str(&text) {
                    Ok(ClientFrame::Ping) => send(&mut socket, &ServerFrame::Pong).await?,
                    Ok(ClientFrame::Viewing { bot_id }) => {
                        let own = match bot_id {
                            Some(bot) => store::bot(&state.db, user, bot)
                                .await
                                .map_err(|_| "database unavailable")?
                                .map(|bot| bot.id),
                            None => None,
                        };
                        viewer.show(own);
                    }
                    _ => return close(&mut socket, CLOSE_PROTOCOL, "unexpected frame").await,
                },
                Some(Ok(Message::Ping(payload))) => {
                    send_raw(&mut socket, Message::Pong(payload)).await?;
                }
                Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Binary(_))) => {
                    return close(&mut socket, CLOSE_PROTOCOL, "unexpected frame").await;
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return Ok(()),
            },
            _ = check.tick() => {
                let alive = accounts::find_session(&state.db, state.clock.now(), token)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|s| s.id == signed_in.session_id);
                if !alive {
                    return close(&mut socket, CLOSE_SIGNED_OUT, "signed out").await;
                }
                send_raw(&mut socket, Message::Ping(Default::default())).await?;
            }
        }
    }
}
