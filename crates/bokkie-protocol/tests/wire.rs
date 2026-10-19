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
        grants: vec![Grant::Notes, Grant::WebSearch],
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
        kind: MessageKind::Text,
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

fn user() -> User {
    User {
        id: UserId(1),
        username: "rob".into(),
        display_name: "Rob".into(),
        role: Role::Admin,
        disabled: false,
        created_at: Timestamp(10),
        permissions: Permissions::default(),
        oversight: false,
        oversight_changed_at: None,
    }
}

fn user_json() -> Value {
    json!({
        "id": 1, "username": "rob", "display_name": "Rob", "role": "admin",
        "disabled": false, "created_at": 10,
        "permissions": {"may_create_bots": true, "may_edit_bot_profiles": true},
        "oversight": false, "oversight_changed_at": null,
    })
}

#[test]
fn account_types() {
    let user = user();
    pin(
        &SessionResponse {
            user: user.clone(),
            password_reset_by_admin: None,
        },
        json!({"user": user_json()}),
    );
    // An admin's reset that the owner has not acknowledged.
    let member = User {
        id: UserId(2),
        username: "amy".into(),
        display_name: "Amy".into(),
        role: Role::Member,
        permissions: Permissions {
            may_create_bots: false,
            may_edit_bot_profiles: true,
        },
        oversight: true,
        oversight_changed_at: Some(Timestamp(20)),
        ..user.clone()
    };
    pin(
        &SessionResponse {
            user: member.clone(),
            password_reset_by_admin: Some(Timestamp(30)),
        },
        json!({
            "user": {
                "id": 2, "username": "amy", "display_name": "Amy", "role": "member",
                "disabled": false, "created_at": 10,
                "permissions": {"may_create_bots": false, "may_edit_bot_profiles": true},
                "oversight": true, "oversight_changed_at": 20,
            },
            "password_reset_by_admin": 30,
        }),
    );
    // An account from a gateway before permissions decodes with today's
    // behaviour: both permissions on, oversight off.
    let older: User = serde_json::from_value(json!({
        "id": 1, "username": "rob", "display_name": "Rob", "role": "admin",
        "disabled": false, "created_at": 10,
    }))
    .unwrap();
    assert_eq!(older, user);
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
            "user": user_json(),
            "limits": {"daily_turns": 20, "weekly_tokens": null},
        }),
    );
    // Optional request fields may be omitted; permissions default to both.
    let request: CreateInvitationRequest =
        serde_json::from_value(json!({"role": "member"})).unwrap();
    assert_eq!(
        request,
        CreateInvitationRequest {
            role: Role::Member,
            permissions: Permissions::default(),
            display_name: None,
            expires_in_hours: None,
        }
    );
    pin(
        &CreateInvitationRequest {
            role: Role::Member,
            permissions: Permissions {
                may_create_bots: false,
                may_edit_bot_profiles: false,
            },
            display_name: None,
            expires_in_hours: None,
        },
        json!({"role": "member",
               "permissions": {"may_create_bots": false, "may_edit_bot_profiles": false}}),
    );
    pin(
        &UpdateUserRequest {
            disabled: Some(true),
            ..UpdateUserRequest::default()
        },
        json!({"disabled": true}),
    );
    pin(
        &UpdateUserRequest {
            permissions: Some(Permissions {
                may_create_bots: true,
                may_edit_bot_profiles: false,
            }),
            oversight: Some(true),
            ..UpdateUserRequest::default()
        },
        json!({"permissions": {"may_create_bots": true, "may_edit_bot_profiles": false},
               "oversight": true}),
    );
}

#[test]
fn an_admins_view_of_a_members_bots() {
    let profile_only = MemberBot {
        id: BotId(7),
        name: "Chef".into(),
        label: "Meal planner".into(),
        description: "Plans dinners.".into(),
        avatar: Some("🍳".into()),
        made_by_admin: true,
        created_at: Timestamp(1_000),
        grants: vec![Grant::Notes],
        overseen: None,
    };
    pin(
        &MemberBotList {
            oversight: false,
            bots: vec![profile_only.clone()],
        },
        json!({"oversight": false, "bots": [{
            "id": 7, "name": "Chef", "label": "Meal planner", "description": "Plans dinners.",
            "avatar": "🍳", "made_by_admin": true, "created_at": 1000, "grants": ["notes"],
        }]}),
    );
    let overseen = MemberBot {
        made_by_admin: false,
        overseen: Some(bot()),
        ..profile_only.clone()
    };
    let encoded = serde_json::to_value(&overseen).unwrap();
    assert_eq!(encoded["made_by_admin"], false);
    assert_eq!(encoded["overseen"]["status"], "idle");
    assert_eq!(
        encoded["overseen"]["last_message"]["text"],
        "Pasta tonight?"
    );
    assert_eq!(
        serde_json::from_value::<MemberBot>(encoded).unwrap(),
        overseen
    );
    // A profile edit names only profile fields; `null` clears the avatar.
    let edit: UpdateBotProfileRequest =
        serde_json::from_value(json!({"label": "Cook", "avatar": null})).unwrap();
    assert_eq!(edit.label.as_deref(), Some("Cook"));
    assert_eq!(edit.avatar, Some(None));
    pin(&edit, json!({"label": "Cook", "avatar": null}));
    let grants: UpdateBotProfileRequest =
        serde_json::from_value(json!({"grants": ["web.search", "notes"]})).unwrap();
    assert_eq!(grants.grants, Some(vec![Grant::WebSearch, Grant::Notes]));
    // A list from before grants existed decodes with none.
    let mut old = serde_json::to_value(&profile_only).unwrap();
    old.as_object_mut().unwrap().remove("grants");
    assert!(
        serde_json::from_value::<MemberBot>(old)
            .unwrap()
            .grants
            .is_empty()
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
            "grants": ["notes", "web.search"],
        }),
    );
    // A Bot stored in an event before grants existed decodes with none.
    let mut old = serde_json::to_value(bot()).unwrap();
    old.as_object_mut().unwrap().remove("grants");
    assert!(
        serde_json::from_value::<Bot>(old)
            .unwrap()
            .grants
            .is_empty()
    );
    pin(
        &SendMessageResponse {
            message: message(),
            delivery: Delivery::Steered,
        },
        json!({
            "message": {"id": 42, "bot_id": 7, "author": "bot", "text": "Hello",
                        "state": "streaming", "created_at": 3000, "updated_at": 3500,
                        "kind": "text"},
            "delivery": "steered",
        }),
    );
    // A message stored before kinds existed is text.
    let old = json!({"id": 42, "bot_id": 7, "author": "bot", "text": "Hello",
                     "state": "streaming", "created_at": 3000, "updated_at": 3500});
    assert_eq!(serde_json::from_value::<Message>(old).unwrap(), message());
    let marker = Message {
        kind: MessageKind::Marker,
        ..message()
    };
    assert_eq!(serde_json::to_value(&marker).unwrap()["kind"], "marker");
}

#[test]
fn grants_are_named_as_the_catalogue_names_them() {
    let names: Vec<&str> = Grant::ALL.iter().map(|g| g.name()).collect();
    assert_eq!(
        names,
        ["notes", "seen", "notify", "web.search", "web.fetch"]
    );
    for grant in Grant::ALL {
        pin(&grant, json!(grant.name()));
        assert_eq!(Grant::from_name(grant.name()), Some(grant));
        assert_eq!(grant.to_string(), grant.name());
    }
    assert_eq!(Grant::from_name("host.run"), None);
    assert!(serde_json::from_value::<Grant>(json!("host.run")).is_err());
    let mut sorted = Grant::ALL.to_vec();
    sorted.sort();
    assert_eq!(sorted, Grant::ALL, "the order is the catalogue's");
    let set: UpdateBotRequest =
        serde_json::from_value(json!({"grants": ["notes", "seen"]})).unwrap();
    assert_eq!(set.grants, Some(vec![Grant::Notes, Grant::Seen]));
    pin(&set, json!({"grants": ["notes", "seen"]}));
}

#[test]
fn notes_and_tool_calls() {
    pin(
        &BotNotes {
            text: "# Seen\n".into(),
            updated_at: Some(Timestamp(5)),
        },
        json!({"text": "# Seen\n", "updated_at": 5}),
    );
    pin(
        &ToolCallPage {
            calls: vec![ToolCall {
                id: 3,
                tool: "notes_append".into(),
                summary: "12 bytes".into(),
                outcome: ToolCallOutcome::TimedOut,
                duration_ms: 30_000,
                at: Timestamp(9),
            }],
            older: Some(3),
        },
        json!({"calls": [{"id": 3, "tool": "notes_append", "summary": "12 bytes",
                          "outcome": "timed_out", "duration_ms": 30000, "at": 9}],
               "older": 3}),
    );
    for (outcome, name) in [
        (ToolCallOutcome::Ok, "ok"),
        (ToolCallOutcome::Failed, "failed"),
        (ToolCallOutcome::Refused, "refused"),
        (ToolCallOutcome::TimedOut, "timed_out"),
    ] {
        pin(&outcome, json!(name));
    }
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
                offset: 3,
                delta: "lo".into(),
            },
        },
        json!({"seq": 9, "at": 5, "event": {
            "type": "message_delta", "bot_id": 7, "message_id": 42, "offset": 3, "delta": "lo",
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
        Event::AccountUpdated { user: user() },
    ];
    for event in events {
        let encoded = serde_json::to_value(&event).unwrap();
        assert!(encoded["type"].is_string(), "{encoded}");
        assert_eq!(serde_json::from_value::<Event>(encoded).unwrap(), event);
    }
    pin(
        &Event::AccountUpdated { user: user() },
        json!({"type": "account_updated", "user": user_json()}),
    );
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

#[test]
fn secrets_do_not_print() {
    let secret = "hunter2-secret-value";
    let printed = [
        format!(
            "{:?}",
            LoginRequest {
                username: "rob".into(),
                password: secret.into(),
            }
        ),
        format!(
            "{:?}",
            ChangePasswordRequest {
                current_password: secret.into(),
                new_password: secret.into(),
            }
        ),
        format!(
            "{:?}",
            AcceptInvitationRequest {
                code: secret.into(),
                username: "amy".into(),
                password: secret.into(),
                display_name: "Amy".into(),
            }
        ),
        format!(
            "{:?}",
            ResetPasswordRequest {
                new_password: secret.into(),
            }
        ),
        format!(
            "{:?}",
            CreatedInvitation {
                id: 1,
                code: secret.into(),
                role: Role::Member,
                expires_at: Timestamp(1),
            }
        ),
    ];
    for line in printed {
        assert!(!line.contains(secret), "{line}");
        assert!(line.contains("<redacted>"), "{line}");
    }
}

#[test]
fn more_request_and_response_shapes() {
    pin(
        &AcceptInvitationRequest {
            code: "c".into(),
            username: "amy".into(),
            password: "p".into(),
            display_name: "Amy".into(),
        },
        json!({"code": "c", "username": "amy", "password": "p", "display_name": "Amy"}),
    );
    pin(
        &ChangePasswordRequest {
            current_password: "a".into(),
            new_password: "b".into(),
        },
        json!({"current_password": "a", "new_password": "b"}),
    );
    pin(
        &ResetPasswordRequest {
            new_password: "b".into(),
        },
        json!({"new_password": "b"}),
    );
    pin(
        &CreatedInvitation {
            id: 3,
            code: "c".into(),
            role: Role::Member,
            expires_at: Timestamp(9),
        },
        json!({"id": 3, "code": "c", "role": "member", "expires_at": 9}),
    );
    pin(
        &InvitationList {
            invitations: vec![InvitationSummary {
                id: 3,
                role: Role::Admin,
                permissions: Permissions::default(),
                display_name: None,
                created_at: Timestamp(1),
                expires_at: Timestamp(9),
            }],
        },
        json!({"invitations": [{"id": 3, "role": "admin",
                                 "permissions": {"may_create_bots": true,
                                                 "may_edit_bot_profiles": true},
                                 "display_name": null, "created_at": 1, "expires_at": 9}]}),
    );
    pin(
        &SessionList {
            sessions: vec![SessionSummary {
                id: 4,
                current: true,
                created_at: Timestamp(1),
                last_seen_at: Timestamp(2),
                user_agent: Some("Safari".into()),
            }],
        },
        json!({"sessions": [{"id": 4, "current": true, "created_at": 1, "last_seen_at": 2,
                              "user_agent": "Safari"}]}),
    );
    pin(
        &VersionResponse {
            protocol: 1,
            gateway: "0.1.0".into(),
        },
        json!({"protocol": 1, "gateway": "0.1.0"}),
    );
    pin(
        &CreateBotRequest {
            name: "Chef".into(),
            label: "Cook".into(),
            description: "d".into(),
            avatar: None,
        },
        json!({"name": "Chef", "label": "Cook", "description": "d"}),
    );
    pin(
        &SendMessageRequest { text: "hi".into() },
        json!({"text": "hi"}),
    );
    pin(&StopResponse { stopped: true }, json!({"stopped": true}));
    pin(
        &BotList {
            bots: vec![],
            seq: Seq(4),
        },
        json!({"bots": [], "seq": 4}),
    );
    pin(
        &MessagePage {
            messages: vec![message()],
            older: Some(MessageId(40)),
            seq: Seq(5),
        },
        json!({"messages": [{"id": 42, "bot_id": 7, "author": "bot", "text": "Hello",
                             "state": "streaming", "created_at": 3000, "updated_at": 3500, "kind": "text"}],
               "older": 40, "seq": 5}),
    );
}
