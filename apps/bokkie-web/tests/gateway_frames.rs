//! The client core against what the gateway itself produces: events stored
//! in and replayed from the gateway's own event log, sent as the event route
//! sends them, and error responses rendered by the gateway's own error type.
//!
//! The gateway is used as a library, on a temporary database with a
//! synthetic account. No app server runs and no model is called.

use axum::response::IntoResponse;
use bokkie_gateway::clock::Clock;
use bokkie_gateway::error::Failure;
use bokkie_gateway::events::EventLog;
use bokkie_gateway::{accounts, auth, db};
use bokkie_protocol::api::{
    Allowance, Author, Bot, BotList, BotStatus, ErrorCode, Limits, Message, MessagePage,
    MessageState, SessionResponse, Usage,
};
use bokkie_protocol::events::{Event, EventEnvelope, Seq, ServerFrame};
use bokkie_protocol::{BotId, MessageId, Timestamp, UserId};
use bokkie_web::connection::{Command, Connection, Input, State};
use bokkie_web::store::{Snapshot, Store};
use bokkie_web::wire;
use http_body_util::BodyExt;

const BOT: BotId = BotId(1);
const REPLY: MessageId = MessageId(2);
const DELTAS: [&str; 4] = [
    "Kia ora! ",
    "Here is a plan: ",
    "rest, then 🥾 walk",
    " — and tea.",
];

struct Gateway {
    log: EventLog,
    user: UserId,
    _dir: tempfile::TempDir,
}

impl Gateway {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::open(&dir.path().join("bokkie.sqlite3")).await.unwrap();
        let clock = Clock::manual(1_800_000_000_000);
        let admin = accounts::bootstrap_admin(
            &pool,
            clock.now(),
            "synthetic",
            "Synthetic Admin",
            "a long test password",
        )
        .await
        .unwrap();
        Self {
            log: EventLog::new(pool, clock),
            user: admin.id,
            _dir: dir,
        }
    }

    async fn append(&self, event: Event) -> EventEnvelope {
        self.log.append(self.user, event).await.unwrap()
    }

    /// What the gateway replays after `after`, read back from its database.
    async fn since(&self, after: Seq) -> Vec<EventEnvelope> {
        self.log.since(self.user, after, 500).await.unwrap()
    }
}

/// A frame as the event route sends it: `serde_json` of the protocol type.
fn text(frame: &ServerFrame) -> String {
    serde_json::to_string(frame).unwrap()
}

fn bot(status: BotStatus) -> Bot {
    Bot {
        id: BOT,
        name: "Ada".into(),
        label: "Trip planner".into(),
        description: "Plans walks".into(),
        avatar: Some("🥾".into()),
        pinned: false,
        hidden: false,
        status,
        created_at: Timestamp(1_800_000_000_000),
        updated_at: Timestamp(1_800_000_000_000),
        last_message: None,
    }
}

fn message(id: MessageId, author: Author, text: &str, state: MessageState) -> Message {
    Message {
        id,
        bot_id: BOT,
        author,
        text: text.into(),
        state,
        created_at: Timestamp(1_800_000_000_100),
        updated_at: Timestamp(1_800_000_000_200),
    }
}

/// The events of one exchange: the user's message, then a streamed reply.
async fn exchange(gateway: &Gateway) -> Vec<EventEnvelope> {
    let mut sent = vec![
        gateway
            .append(Event::BotCreated {
                bot: bot(BotStatus::Idle),
            })
            .await,
        gateway
            .append(Event::MessageCreated {
                message: message(
                    MessageId(1),
                    Author::User,
                    "Plan a walk",
                    MessageState::Complete,
                ),
            })
            .await,
        gateway
            .append(Event::BotStatusChanged {
                bot_id: BOT,
                status: BotStatus::Working,
            })
            .await,
        gateway
            .append(Event::MessageCreated {
                message: message(REPLY, Author::Bot, "", MessageState::Streaming),
            })
            .await,
    ];
    let mut offset = 0;
    for delta in DELTAS {
        sent.push(
            gateway
                .append(Event::MessageDelta {
                    bot_id: BOT,
                    message_id: REPLY,
                    offset,
                    delta: delta.into(),
                })
                .await,
        );
        offset += delta.chars().count() as u64;
    }
    sent.push(
        gateway
            .append(Event::MessageCompleted {
                message: message(REPLY, Author::Bot, &full(), MessageState::Complete),
            })
            .await,
    );
    sent.push(
        gateway
            .append(Event::BotStatusChanged {
                bot_id: BOT,
                status: BotStatus::Idle,
            })
            .await,
    );
    sent.push(
        gateway
            .append(Event::UsageUpdated {
                usage: Usage {
                    subscription: Vec::new(),
                    allowance: Allowance {
                        limits: Limits {
                            daily_turns: Some(20),
                            weekly_tokens: None,
                        },
                        turns_today: 1,
                        tokens_this_week: 812,
                        exhausted: false,
                    },
                },
            })
            .await,
    );
    sent
}

fn full() -> String {
    DELTAS.concat()
}

/// The client: decoding, the connection policy and the store, wired as the
/// browser application wires them. It records what the transport was asked
/// to do.
struct Client {
    connection: Connection,
    store: Store,
    commands: Vec<Command>,
}

impl Client {
    fn new() -> Self {
        Self {
            connection: Connection::new(),
            store: Store::new(),
            commands: Vec::new(),
        }
    }

    fn input(&mut self, input: Input) {
        for command in self.connection.handle(input) {
            match command {
                Command::Apply(envelope) => {
                    let _ = self.store.apply(&envelope);
                }
                Command::Reload => {
                    for snapshot in self.store.reload_all() {
                        assert!(self.store.is_loading(snapshot));
                    }
                    self.commands.push(Command::Reload);
                }
                other => self.commands.push(other),
            }
        }
    }

    fn receive(&mut self, text: &str) {
        match wire::decode_frame(text) {
            Ok(frame) => self.input(Input::Frame(frame)),
            Err(_) => self.input(Input::Unreadable),
        }
    }

    fn connect(&mut self) {
        self.input(Input::Start);
        self.input(Input::Opened);
    }

    fn reply(&self) -> Option<&Message> {
        self.store.conversation(BOT)?.message(REPLY)
    }
}

#[tokio::test]
async fn the_client_follows_a_streamed_reply_from_the_gateways_log() {
    let gateway = Gateway::new().await;
    let mut client = Client::new();
    client.connect();
    // The gateway's first frame for a client starting from the end.
    let latest = gateway.log.latest(gateway.user).await.unwrap();
    client.receive(&text(&ServerFrame::Live { latest }));
    assert_eq!(client.connection.state(), State::Live);
    assert_eq!(latest, Seq(0));
    // Once live, the client loads its snapshots: an empty account.
    let _ = client.store.load_bots(BotList {
        bots: Vec::new(),
        seq: latest,
    });
    let _ = client.store.load_messages(
        BOT,
        MessagePage {
            messages: Vec::new(),
            older: None,
            seq: latest,
        },
    );

    // Each event as the gateway broadcasts it live.
    let sent = exchange(&gateway).await;
    for envelope in &sent {
        client.receive(&text(&ServerFrame::Event(envelope.clone())));
    }
    let reply = client.reply().unwrap();
    assert_eq!(
        (reply.text.as_str(), reply.state),
        (full().as_str(), MessageState::Complete)
    );
    let listed = client.store.bot(BOT).unwrap();
    assert_eq!(listed.status, BotStatus::Idle);
    assert_eq!(listed.last_message.as_ref().unwrap().text, full());
    assert_eq!(
        client.store.usage().unwrap().allowance.tokens_this_week,
        812
    );
    assert_eq!(client.connection.last(), sent.last().map(|e| e.seq));
    assert!(
        !client
            .commands
            .iter()
            .any(|c| matches!(c, Command::Close | Command::RetryAfter(_))),
        "{:?}",
        client.commands
    );

    // The replayed log, read back from the database, is what was broadcast.
    assert_eq!(gateway.since(Seq(0)).await, sent);
}

#[tokio::test]
async fn a_socket_dropped_mid_reply_catches_up_without_losing_or_repeating_text() {
    let gateway = Gateway::new().await;
    let sent = exchange(&gateway).await;
    // Through the second delta.
    let cut = sent
        .iter()
        .position(|e| matches!(&e.event, Event::MessageDelta { delta, .. } if delta == DELTAS[1]))
        .unwrap();

    let mut client = Client::new();
    client.connect();
    client.receive(&text(&ServerFrame::Live { latest: Seq(0) }));
    let _ = client.store.load_bots(BotList {
        bots: Vec::new(),
        seq: Seq(0),
    });
    let _ = client.store.load_messages(
        BOT,
        MessagePage {
            messages: Vec::new(),
            older: None,
            seq: Seq(0),
        },
    );
    for envelope in &sent[..=cut] {
        client.receive(&text(&ServerFrame::Event(envelope.clone())));
    }
    assert_eq!(client.reply().unwrap().text, DELTAS[..2].concat());

    // The phone sleeps; the socket drops without a close frame.
    client.input(Input::Closed { code: Some(1006) });
    assert!(matches!(
        client.connection.state(),
        State::BackingOff { .. }
    ));
    client.input(Input::Visible);
    assert_eq!(client.connection.state(), State::Connecting);
    client.input(Input::Opened);
    let after = client.connection.last().unwrap();
    assert_eq!(after, sent[cut].seq);
    assert!(client.commands.contains(&Command::Send(
        bokkie_protocol::events::ClientFrame::Resume { after: Some(after) }
    )));

    // The gateway replays from its database after `after`. Send one replayed
    // delta twice, as an overlap would, and the live tail after `Live`.
    let replay = gateway.since(after).await;
    let latest = gateway.log.latest(gateway.user).await.unwrap();
    client.receive(&text(&ServerFrame::Event(replay[0].clone())));
    client.receive(&text(&ServerFrame::Event(replay[0].clone())));
    assert_eq!(client.reply().unwrap().text, DELTAS[..3].concat());
    for envelope in &replay[1..] {
        client.receive(&text(&ServerFrame::Event(envelope.clone())));
        if let Some(reply) = client.reply()
            && reply.state == MessageState::Streaming
        {
            assert!(full().starts_with(&reply.text), "never more than was sent");
        }
    }
    client.receive(&text(&ServerFrame::Live { latest }));
    assert_eq!(client.connection.state(), State::Live);
    let reply = client.reply().unwrap();
    assert_eq!(
        (reply.text.as_str(), reply.state),
        (full().as_str(), MessageState::Complete)
    );
}

#[tokio::test]
async fn a_snapshot_taken_mid_reply_merges_with_the_replay() {
    let gateway = Gateway::new().await;
    let sent = exchange(&gateway).await;
    let created = sent
        .iter()
        .position(|e| matches!(&e.event, Event::MessageCreated { message } if message.id == REPLY))
        .unwrap();
    // The page claims the creation as its sequence number but already holds
    // the first two deltas, as a page read mid-stream can.
    let page = MessagePage {
        messages: vec![
            message(
                MessageId(1),
                Author::User,
                "Plan a walk",
                MessageState::Complete,
            ),
            message(
                REPLY,
                Author::Bot,
                &DELTAS[..2].concat(),
                MessageState::Streaming,
            ),
        ],
        older: None,
        seq: sent[created].seq,
    };
    let mut store = Store::new();
    assert!(store.request(Snapshot::Messages(BOT)));
    // The socket replays the whole log while the page loads.
    for envelope in gateway.since(Seq(0)).await {
        let frame = wire::decode_frame(&text(&ServerFrame::Event(envelope))).unwrap();
        let ServerFrame::Event(envelope) = frame else {
            panic!("an event frame");
        };
        let _ = store.apply(&envelope);
    }
    let applied = store.load_messages(BOT, page);
    assert!(applied.reload.is_empty());
    let reply = store.conversation(BOT).unwrap().message(REPLY).unwrap();
    assert_eq!(
        (reply.text.as_str(), reply.state),
        (full().as_str(), MessageState::Complete)
    );
}

#[tokio::test]
async fn every_server_frame_the_gateway_sends_decodes() {
    let gateway = Gateway::new().await;
    let sent = exchange(&gateway).await;
    let mut frames: Vec<ServerFrame> = gateway
        .since(Seq(0))
        .await
        .into_iter()
        .map(ServerFrame::Event)
        .collect();
    frames.extend([
        ServerFrame::Reset { after: Seq(3) },
        ServerFrame::Live {
            latest: sent.last().unwrap().seq,
        },
        ServerFrame::Pong,
    ]);
    for frame in frames {
        assert_eq!(wire::decode_frame(&text(&frame)), Ok(frame));
    }
    // The event route's text for one event, on the wire.
    let first = text(&ServerFrame::Event(sent[0].clone()));
    assert!(
        first.starts_with(r#"{"type":"event","seq":1,"at":"#),
        "{first}"
    );
    assert!(
        first.contains(r#""event":{"type":"bot_created","bot":{"id":1,"#),
        "{first}"
    );
}

/// A response as the gateway renders it.
async fn render(response: axum::response::Response) -> (u16, String) {
    let status = response.status().as_u16();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn the_gateways_error_responses_decode_with_their_messages() {
    let cases = [
        (auth::rate_limited(), ErrorCode::RateLimited, 429),
        (Failure::unauthenticated(), ErrorCode::Unauthenticated, 401),
        (Failure::not_found(), ErrorCode::NotFound, 404),
        (
            Failure::invalid("The name is too long"),
            ErrorCode::InvalidRequest,
            400,
        ),
        (
            Failure::unavailable("The Bot's runtime is starting"),
            ErrorCode::Unavailable,
            503,
        ),
    ];
    for (failure, code, status) in cases {
        let message = failure.message.clone();
        let (got_status, body) = render(failure.into_response()).await;
        assert_eq!(got_status, status);
        let error = wire::decode_response::<SessionResponse>(got_status, &body).unwrap_err();
        assert_eq!((error.status, error.code), (status, code));
        assert_eq!(error.message, message);
        assert_eq!(
            error.is_unauthenticated(),
            code == ErrorCode::Unauthenticated
        );
    }
}
