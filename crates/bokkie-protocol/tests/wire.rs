//! Pins the JSON shape of the wire types. The gateway and the WASM client
//! both depend on these shapes, so a change here is a protocol change.

use bokkie_protocol::api::*;
use bokkie_protocol::events::*;
use bokkie_protocol::{BotId, MessageId, Timestamp, UserId};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Asserts that `value` encodes to exactly `expected` and decodes back.
fn pin<T>(value: &T, expected: Value)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let encoded = serde_json::to_value(value).unwrap();
    assert_eq!(encoded, expected, "encoding of {value:?}");
    let decoded: T = serde_json::from_value(expected).unwrap();
    assert_eq!(&decoded, value, "decoding back");
}

fn bot() -> Bot {
    Bot {
        id: BotId(7),
        name: "Chef".into(),
        label: "Meal planner".into(),
        description: "Plans dinners.".into(),
        avatar: Some("🍳".into()),
        pinned: true,
        hidden: false,
        status: BotStatus::Idle,
        created_at: Timestamp(1_000),
        updated_at: Timestamp(2_000),
        last_message: Some(MessagePreview {
            author: Author::Bot,
            text: "Pasta tonight?".into(),
            at: Timestamp(2_000),
        }),
    }
}

fn message() -> Message {
    Message {
        id: MessageId(42),
        bot_id: BotId(7),
        author: Author::Bot,
        text: "Hello".into(),
        state: MessageState::Streaming,
        created_at: Timestamp(3_000),
        updated_at: Timestamp(3_500),
    }
}

#[test]
fn ids_and_timestamps_are_bare_numbers() {
    pin(&UserId(1), json!(1));
    pin(&BotId(2), json!(2));
    pin(&MessageId(3), json!(3));
    pin(&Timestamp(4), json!(4));
    pin(&Seq(5), json!(5));
    assert_eq!(Seq::ZERO.next(), Seq(1));
}

#[test]
fn errors_name_their_code_and_status() {
    pin(
        &ApiError {
            code: ErrorCode::LimitReached,
            message: "Daily turns used up".into(),
        },
        json!({"code": "limit_reached", "message": "Daily turns used up"}),
    );
    let statuses = [
        (ErrorCode::InvalidRequest, 400),
        (ErrorCode::Unauthenticated, 401),
        (ErrorCode::Forbidden, 403),
        (ErrorCode::NotFound, 404),
        (ErrorCode::Conflict, 409),
        (ErrorCode::RateLimited, 429),
        (ErrorCode::LimitReached, 429),
        (ErrorCode::Unavailable, 503),
        (ErrorCode::Internal, 500),
    ];
    for (code, status) in statuses {
        assert_eq!(code.status(), status, "{code:?}");
    }
}

#[test]
fn account_types() {
    let user = User {
        id: UserId(1),
        username: "rob".into(),
        display_name: "Rob".into(),
        role: Role::Admin,
        disabled: false,
        created_at: Timestamp(10),
    };
    pin(
        &SessionResponse { user: user.clone() },
        json!({"user": {
            "id": 1, "username": "rob", "display_name": "Rob", "role": "admin",
            "disabled": false, "created_at": 10,
        }}),
    );
    pin(
        &LoginRequest {
            username: "rob".into(),
            password: "pw".into(),
        },
        json!({"username": "rob", "password": "pw"}),
    );
    pin(
        &AdminUser {
            user,
            limits: Limits {
                daily_turns: Some(20),
                weekly_tokens: None,
            },
        },
        json!({
            "user": {"id": 1, "username": "rob", "display_name": "Rob", "role": "admin",
                     "disabled": false, "created_at": 10},
            "limits": {"daily_turns": 20, "weekly_tokens": null},
        }),
    );
    // Optional request fields may be omitted.
    let request: CreateInvitationRequest =
        serde_json::from_value(json!({"role": "member"})).unwrap();
    assert_eq!(
        request,
        CreateInvitationRequest {
            role: Role::Member,
            display_name: None,
            expires_in_hours: None,
        }
    );
    pin(
        &UpdateUserRequest {
            disabled: Some(true),
            ..UpdateUserRequest::default()
        },
        json!({"disabled": true}),
    );
}

#[test]
fn bot_types() {
    pin(
        &bot(),
        json!({
            "id": 7, "name": "Chef", "label": "Meal planner", "description": "Plans dinners.",
            "avatar": "🍳", "pinned": true, "hidden": false, "status": "idle",
            "created_at": 1000, "updated_at": 2000,
            "last_message": {"author": "bot", "text": "Pasta tonight?", "at": 2000},
        }),
    );
    pin(
        &SendMessageResponse {
            message: message(),
            delivery: Delivery::Steered,
        },
        json!({
            "message": {"id": 42, "bot_id": 7, "author": "bot", "text": "Hello",
                        "state": "streaming", "created_at": 3000, "updated_at": 3500},
            "delivery": "steered",
        }),
    );
}

#[test]
fn bot_updates_distinguish_unchanged_from_cleared() {
    let unchanged: UpdateBotRequest = serde_json::from_value(json!({"pinned": true})).unwrap();
    assert_eq!(unchanged.avatar, None);
    assert_eq!(unchanged.pinned, Some(true));

    let cleared: UpdateBotRequest = serde_json::from_value(json!({"avatar": null})).unwrap();
    assert_eq!(cleared.avatar, Some(None));

    let set: UpdateBotRequest = serde_json::from_value(json!({"avatar": "🤖"})).unwrap();
    assert_eq!(set.avatar, Some(Some("🤖".into())));

    pin(&cleared, json!({"avatar": null}));
    pin(&set, json!({"avatar": "🤖"}));
    pin(&UpdateBotRequest::default(), json!({}));
}

#[test]
fn usage_types() {
    pin(
        &Usage {
            subscription: vec![RateLimitBucket {
                id: "codex".into(),
                name: None,
                primary: Some(RateLimitWindow {
                    used_percent: 12,
                    window_minutes: Some(10_080),
                    resets_at: Some(Timestamp(99_000)),
                }),
                secondary: None,
                reached: false,
            }],
            allowance: Allowance {
                limits: Limits::default(),
                turns_today: 3,
                tokens_this_week: 4_000,
                exhausted: false,
            },
        },
        json!({
            "subscription": [{
                "id": "codex", "name": null,
                "primary": {"used_percent": 12, "window_minutes": 10080, "resets_at": 99000},
                "secondary": null, "reached": false,
            }],
            "allowance": {
                "limits": {"daily_turns": null, "weekly_tokens": null},
                "turns_today": 3, "tokens_this_week": 4000, "exhausted": false,
            },
        }),
    );
}

#[test]
fn events_are_tagged_by_type() {
    pin(
        &EventEnvelope {
            seq: Seq(9),
            at: Timestamp(5),
            event: Event::MessageDelta {
                bot_id: BotId(7),
                message_id: MessageId(42),
                delta: "lo".into(),
            },
        },
        json!({"seq": 9, "at": 5, "event": {
            "type": "message_delta", "bot_id": 7, "message_id": 42, "delta": "lo",
        }}),
    );
    pin(
        &Event::BotStatusChanged {
            bot_id: BotId(7),
            status: BotStatus::Refused,
        },
        json!({"type": "bot_status_changed", "bot_id": 7, "status": "refused"}),
    );
    // Every variant round-trips.
    let usage = Usage {
        subscription: vec![],
        allowance: Allowance {
            limits: Limits::default(),
            turns_today: 0,
            tokens_this_week: 0,
            exhausted: false,
        },
    };
    let events = [
        Event::BotCreated { bot: bot() },
        Event::BotUpdated { bot: bot() },
        Event::MessageCreated { message: message() },
        Event::MessageCompleted { message: message() },
        Event::UsageUpdated { usage },
        Event::BotError {
            bot_id: BotId(7),
            message: "The Bot is unavailable".into(),
        },
    ];
    for event in events {
        let encoded = serde_json::to_value(&event).unwrap();
        assert!(encoded["type"].is_string(), "{encoded}");
        assert_eq!(serde_json::from_value::<Event>(encoded).unwrap(), event);
    }
}

#[test]
fn websocket_frames() {
    pin(
        &ClientFrame::Resume {
            after: Some(Seq(3)),
        },
        json!({"type": "resume", "after": 3}),
    );
    pin(
        &ClientFrame::Resume { after: None },
        json!({"type": "resume", "after": null}),
    );
    // A first start may omit `after`.
    assert_eq!(
        serde_json::from_value::<ClientFrame>(json!({"type": "resume"})).unwrap(),
        ClientFrame::Resume { after: None }
    );
    pin(&ClientFrame::Ping, json!({"type": "ping"}));
    pin(&ServerFrame::Pong, json!({"type": "pong"}));
    pin(
        &ServerFrame::Reset { after: Seq(100) },
        json!({"type": "reset", "after": 100}),
    );
    pin(
        &ServerFrame::Live { latest: Seq(12) },
        json!({"type": "live", "latest": 12}),
    );
    pin(
        &ServerFrame::Event(EventEnvelope {
            seq: Seq(12),
            at: Timestamp(6),
            event: Event::BotStatusChanged {
                bot_id: BotId(1),
                status: BotStatus::Working,
            },
        }),
        json!({"type": "event", "seq": 12, "at": 6, "event": {
            "type": "bot_status_changed", "bot_id": 1, "status": "working",
        }}),
    );
}

#[test]
fn unknown_frames_are_rejected() {
    assert!(serde_json::from_value::<ClientFrame>(json!({"type": "subscribe_all"})).is_err());
    assert!(serde_json::from_value::<Event>(json!({"type": "approval_requested"})).is_err());
}
