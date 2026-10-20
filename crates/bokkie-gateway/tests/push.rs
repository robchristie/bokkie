//! Web Push: the key, each session's subscription, what pushes and when,
//! and delivery to a mock push service that decrypts each push with the
//! keys of the device it was meant for. Each keeps one user's pushes from
//! every other user.
//!
//! The gateway's clock starts at 2027-01-15T08:00Z, which is 18:30 in
//! Adelaide, every account's default time zone.

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bokkie_gateway::clock::{HOUR_MS, MINUTE_MS};
use bokkie_gateway::push::{HOURLY_LIMIT, Retry};
use bokkie_gateway::tools::notify;
use bokkie_protocol::events::ClientFrame;
use bokkie_protocol::{BotId, UserId};
use common::push_service::{Answer, Device, PushService};
use common::{Call, Gateway, HOST, ORIGIN, PASSWORD, PUSH_CONTACT};
use futures_util::SinkExt;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// 2027-01-15T08:00Z, the manual clock's start: 18:30 in Adelaide.
const START: i64 = 1_800_000_000_000;

/// A gateway whose pushes go to a mock push service, retried quickly.
async fn with_service(gateway: Gateway) -> (Gateway, PushService) {
    gateway.route().await;
    let service = PushService::start().await;
    gateway.state.push.use_network(service.network().network());
    gateway.state.push.use_retry(Retry {
        retries: 3,
        backoff: Duration::from_millis(5),
        longest: Duration::from_millis(20),
    });
    (gateway, service)
}

/// A signed-in admin, `rob`, on a gateway whose pushes go to a mock push
/// service.
async fn setup() -> (Gateway, PushService, String) {
    let (gateway, service) = with_service(Gateway::new().await).await;
    let rob = gateway.admin("rob").await;
    (gateway, service, rob)
}

async fn subscribe(gateway: &Gateway, cookie: &str, subscription: &Value) -> common::Reply {
    gateway
        .send(
            Call::new(Method::PUT, "/api/v1/push/subscription")
                .cookie(cookie)
                .json(subscription),
        )
        .await
}

/// Subscribes `device` for the session `cookie`, which must succeed.
async fn subscribed(gateway: &Gateway, cookie: &str, device: &Device) {
    let reply = subscribe(gateway, cookie, &device.subscription()).await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    // The endpoint is never sent back.
    assert_eq!(reply.body, json!({"subscribed": true}));
}

/// Another session of `username`, as a second device signs in.
async fn sign_in(gateway: &Gateway, username: &str) -> String {
    let reply = gateway.login(username, PASSWORD).await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    reply.cookie()
}

/// Runs the sender once, and waits for its deliveries to finish.
async fn deliver(gateway: &Gateway) {
    gateway.state.push.deliver_pending().await.unwrap();
    gateway.state.push.settle().await;
}

/// Every push recorded, oldest first: its user, cause and state.
async fn pushes(gateway: &Gateway) -> Vec<(i64, String, String)> {
    sqlx::query_as("SELECT user_id, cause, state FROM pushes ORDER BY id")
        .fetch_all(&gateway.state.db)
        .await
        .unwrap()
}

/// The endpoints of a user's stored subscriptions, oldest first.
async fn endpoints(gateway: &Gateway, user: i64) -> Vec<String> {
    sqlx::query_scalar("SELECT endpoint FROM push_subscriptions WHERE user_id = ? ORDER BY id")
        .bind(user)
        .fetch_all(&gateway.state.db)
        .await
        .unwrap()
}

async fn subscriptions(gateway: &Gateway, user: i64) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM push_subscriptions WHERE user_id = ?")
        .bind(user)
        .fetch_one(&gateway.state.db)
        .await
        .unwrap()
}

async fn create_bot(gateway: &Gateway, cookie: &str, name: &str, grants: Value) -> i64 {
    let reply = gateway
        .send(Call::post("/api/v1/bots").cookie(cookie).json(&json!({
            "name": name, "label": "", "description": ""})))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{:?}", reply.body);
    let bot = reply.body["id"].as_i64().unwrap();
    let set = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/bots/{bot}"))
                .cookie(cookie)
                .json(&json!({"grants": grants})),
        )
        .await;
    assert_eq!(set.status, StatusCode::OK, "{:?}", set.body);
    bot
}

/// Starts the Bot's thread with one exchange and returns its id.
async fn thread(gateway: &Gateway, cookie: &str, user: i64, bot: i64) -> String {
    let sent = gateway
        .send(
            Call::post(&format!("/api/v1/bots/{bot}/messages"))
                .cookie(cookie)
                .json(&json!({"text": "hi"})),
        )
        .await;
    assert_eq!(sent.status, StatusCode::OK, "{:?}", sent.body);
    gateway
        .wait_for(cookie, &format!("/api/v1/bots/{bot}/messages"), |b| {
            b["messages"].as_array().is_some_and(|m| {
                m.iter().any(|m| {
                    m["author"] == "bot" && m["kind"] == "text" && m["state"] == "complete"
                })
            })
        })
        .await;
    gateway
        .wait_for(cookie, &format!("/api/v1/bots/{bot}"), |b| {
            b["status"] == "idle"
        })
        .await;
    bokkie_gateway::bots::store::bot(&gateway.state.db, UserId(user), BotId(bot))
        .await
        .unwrap()
        .unwrap()
        .thread_id
        .unwrap()
}

/// The model, through `user`'s app server, calls `notify` in `turn` of
/// `thread`. Returns whether it succeeded, and the tool's answer.
async fn notify_in(
    gateway: &Gateway,
    user: i64,
    id: i64,
    thread: &str,
    turn: &str,
    arguments: Value,
) -> (bool, String) {
    gateway.fake.inject(
        UserId(user),
        json!({"id": id, "method": "item/tool/call", "params": {
            "threadId": thread, "turnId": turn, "callId": format!("call-{id}"),
            "tool": "notify", "namespace": null, "arguments": arguments}}),
    );
    let answer = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(answer) = gateway
                .fake
                .received("<response>")
                .into_iter()
                .find(|r| r.params["id"] == id)
            {
                return answer.params;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the call is answered");
    let result = &answer["result"];
    (
        result["success"].as_bool().expect("a tool answer"),
        result["contentItems"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned(),
    )
}

/// The same, in a turn of its own, which must succeed.
async fn notify(gateway: &Gateway, user: i64, id: i64, thread: &str, title: &str, kind: &str) {
    let (ok, text) = notify_in(
        gateway,
        user,
        id,
        thread,
        &format!("turn-{id}"),
        json!({"title": title, "kind": kind}),
    )
    .await;
    assert!(ok, "{text}");
}

async fn waited(condition: impl AsyncFn() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition().await {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the condition holds in time");
}

// ---------------------------------------------------------------------------
// The key and subscriptions

#[tokio::test]
async fn the_key_is_made_privately_on_first_use_and_only_its_public_half_is_served() {
    let (gateway, _service, rob) = setup().await;
    let unsigned = gateway.send(Call::get("/api/v1/push/key")).await;
    assert_eq!(unsigned.status, StatusCode::UNAUTHORIZED);

    let reply = gateway
        .send(Call::get("/api/v1/push/key").cookie(&rob))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    assert_eq!(reply.body["sending"], true);
    let public = reply.body["public_key"].as_str().unwrap();
    let point = URL_SAFE_NO_PAD.decode(public).unwrap();
    assert_eq!(point.len(), 65);
    assert_eq!(point[0], 4);
    assert_eq!(reply.body.as_object().unwrap().len(), 2, "{:?}", reply.body);

    // The private key is in the data directory, readable by the gateway
    // alone, and appears in no answer.
    use std::os::unix::fs::PermissionsExt;
    let dir = gateway.dir.path().join("push");
    let file = dir.join("vapid.pem");
    let mode = |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode();
    assert_eq!(mode(&file) & 0o777, 0o600);
    assert_eq!(mode(&dir) & 0o777, 0o700);
    let pem = std::fs::read_to_string(&file).unwrap();
    assert!(pem.starts_with("-----BEGIN PRIVATE KEY-----"), "PKCS#8");
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    assert!(!reply.body.to_string().contains(&body[..40]));

    // The same key is served again.
    let again = gateway
        .send(Call::get("/api/v1/push/key").cookie(&rob))
        .await;
    assert_eq!(again.body["public_key"], public);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), pem);
}

#[tokio::test]
async fn a_session_subscribes_refreshes_and_unsubscribes_its_device() {
    let (gateway, _service, rob) = setup().await;
    let state = |cookie: String| {
        let gateway = &gateway;
        async move {
            let reply = gateway
                .send(Call::get("/api/v1/push/subscription").cookie(&cookie))
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
            assert!(reply.body.get("endpoint").is_none(), "{:?}", reply.body);
            reply.body["subscribed"].as_bool().unwrap()
        }
    };
    assert!(!state(rob.clone()).await);

    let phone = Device::new("phone");
    subscribed(&gateway, &rob, &phone).await;
    assert!(state(rob.clone()).await);
    assert_eq!(endpoints(&gateway, 1).await, [phone.endpoint.as_str()]);
    // A refresh with new keys replaces the session's subscription.
    let refreshed = Device::new("phone-renewed");
    subscribed(&gateway, &rob, &refreshed).await;
    assert!(state(rob.clone()).await);
    assert_eq!(endpoints(&gateway, 1).await, [refreshed.endpoint.as_str()]);

    // An endpoint belongs to the session that sent it last.
    let laptop = sign_in(&gateway, "rob").await;
    subscribed(&gateway, &laptop, &refreshed).await;
    assert!(!state(rob.clone()).await);
    assert!(state(laptop.clone()).await);
    assert_eq!(endpoints(&gateway, 1).await, [refreshed.endpoint.as_str()]);

    let deleted = gateway
        .send(Call::new(Method::DELETE, "/api/v1/push/subscription").cookie(&laptop))
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert!(!state(laptop.clone()).await);
    assert_eq!(subscriptions(&gateway, 1).await, 0);
    // Deleting again is fine.
    let again = gateway
        .send(Call::new(Method::DELETE, "/api/v1/push/subscription").cookie(&laptop))
        .await;
    assert_eq!(again.status, StatusCode::NO_CONTENT);

    // Without a session, nothing.
    for call in [
        Call::get("/api/v1/push/subscription"),
        Call::new(Method::PUT, "/api/v1/push/subscription").json(&phone.subscription()),
        Call::new(Method::DELETE, "/api/v1/push/subscription"),
    ] {
        assert_eq!(gateway.send(call).await.status, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn a_subscription_needs_an_https_endpoint_and_keys_of_the_right_lengths() {
    let (gateway, _service, rob) = setup().await;
    let good = Device::new("phone").subscription();
    let with = |pointer: &str, value: Value| {
        let mut changed = good.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        changed
    };
    let short = URL_SAFE_NO_PAD.encode([1u8; 15]);
    let mut compressed = URL_SAFE_NO_PAD
        .decode(good["keys"]["p256dh"].as_str().unwrap())
        .unwrap();
    compressed.truncate(33);
    compressed[0] = 2;
    let long = format!(
        "https://push.example/{}",
        "a".repeat(bokkie_protocol::api::PUSH_ENDPOINT_BYTES)
    );
    for bad in [
        with("/endpoint", json!("http://push.example/phone")),
        with("/endpoint", json!("https://127.0.0.1/phone")),
        with("/endpoint", json!("https://user@push.example/phone")),
        with("/endpoint", json!(long)),
        with("/endpoint", json!("not a url")),
        with("/keys/p256dh", json!(URL_SAFE_NO_PAD.encode(compressed))),
        with("/keys/p256dh", json!("")),
        with("/keys/auth", json!(short)),
        json!({"endpoint": "https://push.example/phone"}),
    ] {
        let reply = subscribe(&gateway, &rob, &bad).await;
        assert!(
            reply.status == StatusCode::BAD_REQUEST
                || reply.status == StatusCode::UNPROCESSABLE_ENTITY,
            "{bad}: {} {:?}",
            reply.status,
            reply.body
        );
    }
    assert_eq!(subscriptions(&gateway, 1).await, 0);
}

#[tokio::test]
async fn a_subscription_ends_with_its_session() {
    let (gateway, _service, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;

    // Signing out.
    let phone = sign_in(&gateway, "rob").await;
    subscribed(&gateway, &phone, &Device::new("rob-phone")).await;
    assert_eq!(subscriptions(&gateway, 1).await, 1);
    let out = gateway
        .send(Call::new(Method::DELETE, "/api/v1/session").cookie(&phone))
        .await;
    assert!(out.status.is_success(), "{:?}", out.body);
    assert_eq!(subscriptions(&gateway, 1).await, 0);

    // Revoked from another session.
    let tablet = sign_in(&gateway, "rob").await;
    subscribed(&gateway, &tablet, &Device::new("rob-tablet")).await;
    let sessions = gateway
        .send(Call::get("/api/v1/sessions").cookie(&tablet))
        .await;
    let tablet_session = sessions.body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["current"] == true)
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let revoked = gateway
        .send(
            Call::new(
                Method::DELETE,
                &format!("/api/v1/sessions/{tablet_session}"),
            )
            .cookie(&rob),
        )
        .await;
    assert!(revoked.status.is_success(), "{:?}", revoked.body);
    assert_eq!(subscriptions(&gateway, 1).await, 0);

    // A password change ends every other session's.
    let laptop = sign_in(&gateway, "rob").await;
    subscribed(&gateway, &laptop, &Device::new("rob-laptop")).await;
    subscribed(&gateway, &rob, &Device::new("rob-desk")).await;
    let changed = gateway
        .send(
            Call::new(Method::PUT, "/api/v1/session/password")
                .cookie(&rob)
                .json(&json!({"current_password": PASSWORD,
                              "new_password": "another long password"})),
        )
        .await;
    assert!(changed.status.is_success(), "{:?}", changed.body);
    let left: Vec<String> =
        sqlx::query_scalar("SELECT endpoint FROM push_subscriptions WHERE user_id = 1")
            .fetch_all(&gateway.state.db)
            .await
            .unwrap();
    assert_eq!(left, vec!["https://push.example/rob-desk".to_owned()]);

    // Disabling an account ends all of its.
    subscribed(&gateway, &amy, &Device::new("amy-phone")).await;
    assert_eq!(subscriptions(&gateway, amy_id).await, 1);
    let disabled = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/admin/users/{amy_id}"))
                .cookie(&rob)
                .json(&json!({"disabled": true})),
        )
        .await;
    assert_eq!(disabled.status, StatusCode::OK, "{:?}", disabled.body);
    assert_eq!(subscriptions(&gateway, amy_id).await, 0);
    assert_eq!(subscriptions(&gateway, 1).await, 1, "Rob's are his own");
}

// ---------------------------------------------------------------------------
// Notify

#[tokio::test]
async fn notify_pushes_to_each_of_its_users_devices_and_no_one_elses() {
    let (gateway, service, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let phone = Device::new("rob-phone");
    let laptop = Device::new("rob-laptop");
    let amys = Device::new("amy-phone");
    subscribed(&gateway, &rob, &phone).await;
    let rob_laptop = sign_in(&gateway, "rob").await;
    subscribed(&gateway, &rob_laptop, &laptop).await;
    subscribed(&gateway, &amy, &amys).await;
    let chef = create_bot(&gateway, &rob, "Chef", json!(["notify"])).await;
    let tutor = create_bot(&gateway, &rob, "Tutor", json!([])).await;
    let thread = thread(&gateway, &rob, 1, chef).await;
    // Tutor has written too: two unread Bots.
    let _ = crate::thread(&gateway, &rob, 1, tutor).await;

    let (ok, answer) = notify_in(
        &gateway,
        1,
        901,
        &thread,
        "turn-x",
        json!({"title": "Dinner plan ready", "body": "Pasta on Monday.", "kind": "result"}),
    )
    .await;
    assert!(ok, "{answer}");
    assert_eq!(answer, notify::ANSWER);
    deliver(&gateway).await;

    let message = json!({"web_push": 8030, "mutable": true, "notification": {
        "title": "Chef",
        "body": "Dinner plan ready\nPasta on Monday.",
        "navigate": format!("{ORIGIN}/?bot={chef}"),
        "tag": format!("bot-{chef}"),
        "app_badge": 2}});
    assert_eq!(service.opened(&phone), vec![message.clone()]);
    assert_eq!(service.opened(&laptop), vec![message]);
    // Nothing for Amy, and her keys open none of Rob's.
    assert!(service.sent_to(&amys).is_empty());
    for push in service.received() {
        assert!(amys.open(&push).is_none());
    }
    assert_eq!(
        pushes(&gateway).await,
        vec![(1, "notify".into(), "sent".into())]
    );

    // Each request is a push service's: encrypted, with a VAPID token for
    // the service's origin from the gateway's key and contact.
    let key = gateway
        .send(Call::get("/api/v1/push/key").cookie(&rob))
        .await
        .body["public_key"]
        .clone();
    let now = gateway.state.clock.now_ms() / 1000;
    for push in service.received() {
        assert_eq!(push.host, "push.example");
        assert_eq!(push.header("content-encoding"), Some("aes128gcm"));
        assert_eq!(push.header("ttl"), Some("86400"));
        assert_eq!(push.header("urgency"), Some("normal"));
        assert!(push.body.len() < 4096);
        assert_eq!(push.key.as_deref(), key.as_str());
        let claims = push.claims.clone().expect("the token's signature verifies");
        assert_eq!(claims["aud"], "https://push.example");
        assert_eq!(claims["sub"], PUSH_CONTACT);
        let exp = claims["exp"].as_i64().unwrap();
        assert!(exp > now && exp <= now + 24 * 3600, "{exp} {now}");
    }

    // A question is urgent. Amy's own Bot pushes to her alone.
    service.clear();
    notify(
        &gateway,
        1,
        902,
        &thread,
        "Which night for fish?",
        "question",
    )
    .await;
    let diary = create_bot(&gateway, &amy, "Diary", json!(["notify"])).await;
    let amys_thread = crate::thread(&gateway, &amy, amy_id, diary).await;
    notify(&gateway, amy_id, 903, &amys_thread, "Entry saved", "result").await;
    deliver(&gateway).await;
    let robs = service.opened(&phone);
    assert_eq!(robs.len(), 1);
    assert_eq!(robs[0]["notification"]["body"], "Which night for fish?");
    assert_eq!(service.sent_to(&phone)[0].header("urgency"), Some("high"));
    let hers = service.opened(&amys);
    assert_eq!(hers.len(), 1);
    assert_eq!(hers[0]["notification"]["title"], "Diary");
    assert_eq!(
        hers[0]["notification"]["navigate"],
        format!("{ORIGIN}/?bot={diary}")
    );
    assert_eq!(hers[0]["notification"]["app_badge"], 1);
    for push in service.sent_to(&amys) {
        assert!(phone.open(&push).is_none() && laptop.open(&push).is_none());
    }
}

#[tokio::test]
async fn a_device_signed_in_as_someone_else_gets_only_their_pushes() {
    let (gateway, service, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let chef = create_bot(&gateway, &rob, "Chef", json!(["notify"])).await;
    let robs_thread = thread(&gateway, &rob, 1, chef).await;
    let diary = create_bot(&gateway, &amy, "Diary", json!(["notify"])).await;
    let amys_thread = thread(&gateway, &amy, amy_id, diary).await;
    // The family tablet: Rob subscribed it, then Amy signed in on it.
    let tablet = Device::new("tablet");
    subscribed(&gateway, &rob, &tablet).await;
    let amys_keys = Device::new("tablet");
    subscribed(&gateway, &amy, &amys_keys).await;
    assert_eq!(subscriptions(&gateway, 1).await, 0);

    notify(&gateway, 1, 901, &robs_thread, "For Rob", "result").await;
    notify(&gateway, amy_id, 902, &amys_thread, "For Amy", "result").await;
    deliver(&gateway).await;
    let opened = service.opened(&amys_keys);
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0]["notification"]["body"], "For Amy");
    assert_eq!(
        pushes(&gateway).await,
        vec![
            (1, "notify".into(), "unsubscribed".into()),
            (amy_id, "notify".into(), "sent".into())
        ]
    );
}

#[tokio::test]
async fn notify_is_limited_per_turn_and_per_hour() {
    let (gateway, _service, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef", json!(["notify"])).await;
    let tutor = create_bot(&gateway, &rob, "Tutor", json!(["notify"])).await;
    let chefs = thread(&gateway, &rob, 1, chef).await;
    let tutors = thread(&gateway, &rob, 1, tutor).await;
    let count = async || -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM notifications")
            .fetch_one(&gateway.state.db)
            .await
            .unwrap()
    };
    let call = async |id: i64, which: &str, turn: &str| {
        let thread = if which == "chef" { &chefs } else { &tutors };
        notify_in(
            &gateway,
            1,
            id,
            thread,
            turn,
            json!({"title": format!("Note {id}"), "kind": "result"}),
        )
        .await
    };
    for id in 0..notify::PER_TURN {
        assert!(call(100 + id, "chef", "turn-a").await.0);
    }
    let (ok, text) = call(150, "chef", "turn-a").await;
    assert!(!ok);
    assert!(text.contains("in this turn"), "{text}");
    assert_eq!(count().await, notify::PER_TURN);
    // Another turn, or another Bot's, has its own.
    assert!(call(151, "chef", "turn-b").await.0);
    assert!(call(152, "tutor", "turn-a").await.0);

    // The hour's limit is the user's, across their Bots.
    let mut id = 200;
    while count().await < notify::PER_HOUR {
        id += 1;
        let thread = if id % 2 == 0 { "chef" } else { "tutor" };
        assert!(call(id, thread, &format!("turn-{id}")).await.0);
    }
    let (ok, text) = call(300, "tutor", "turn-300").await;
    assert!(!ok);
    assert!(text.contains("past hour"), "{text}");
    assert_eq!(count().await, notify::PER_HOUR);
    gateway.state.clock.advance(HOUR_MS);
    assert!(call(301, "chef", "turn-301").await.0);
}

#[tokio::test]
async fn notify_refuses_layout_characters() {
    let (gateway, _service, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef", json!(["notify"])).await;
    let thread = thread(&gateway, &rob, 1, chef).await;
    for (id, arguments) in [
        json!({"title": "invoice\u{202e}fdp.exe", "kind": "result"}),
        json!({"title": "Two\u{2028}lines", "kind": "result"}),
        json!({"title": "Ok", "body": "one\u{2029}two", "kind": "result"}),
        json!({"title": "Ok", "body": "\u{2066}isolated\u{2069}", "kind": "result"}),
    ]
    .into_iter()
    .enumerate()
    {
        let id = 400 + id as i64;
        let (ok, text) = notify_in(&gateway, 1, id, &thread, &format!("t{id}"), arguments).await;
        assert!(!ok, "{text}");
    }
    assert!(pushes(&gateway).await.is_empty());
}

// ---------------------------------------------------------------------------
// When not to push

#[tokio::test]
async fn no_push_while_the_bot_is_on_one_of_the_users_screens() {
    let (gateway, service, rob) = setup().await;
    let address = gateway.serve().await;
    let phone = Device::new("phone");
    subscribed(&gateway, &rob, &phone).await;
    let chef = create_bot(&gateway, &rob, "Chef", json!(["notify"])).await;
    let tutor = create_bot(&gateway, &rob, "Tutor", json!([])).await;
    let thread = thread(&gateway, &rob, 1, chef).await;

    // Chef is open on the laptop.
    let mut laptop = socket(address, &rob).await;
    view(&mut laptop, Some(chef)).await;
    let presence = gateway.state.presence.clone();
    waited(async || presence.is_shown(UserId(1), BotId(chef))).await;
    notify(&gateway, 1, 901, &thread, "Seen already", "result").await;
    deliver(&gateway).await;
    assert!(service.received().is_empty());

    // Another Bot open does not count.
    view(&mut laptop, Some(tutor)).await;
    waited(async || !presence.is_shown(UserId(1), BotId(chef))).await;
    notify(&gateway, 1, 902, &thread, "Not seen", "result").await;
    deliver(&gateway).await;
    let opened = service.opened(&phone);
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0]["notification"]["body"], "Not seen");
    let states: Vec<String> = pushes(&gateway).await.into_iter().map(|p| p.2).collect();
    assert_eq!(states, ["shown", "sent"]);
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn socket(address: SocketAddr, cookie: &str) -> Socket {
    let mut request = format!("ws://{address}/api/v1/events")
        .into_client_request()
        .unwrap();
    let headers = request.headers_mut();
    headers.insert("host", HOST.parse().unwrap());
    headers.insert("origin", ORIGIN.parse().unwrap());
    headers.insert("cookie", cookie.parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let resume = serde_json::to_string(&ClientFrame::Resume { after: None }).unwrap();
    socket.send(Message::text(resume)).await.unwrap();
    socket
}

async fn view(socket: &mut Socket, bot: Option<i64>) {
    let frame = serde_json::to_string(&ClientFrame::Viewing {
        bot_id: bot.map(BotId),
    })
    .unwrap();
    socket.send(Message::text(frame)).await.unwrap();
}

#[tokio::test]
async fn at_most_twenty_pushes_an_hour_and_the_rest_are_dropped() {
    let (gateway, service, rob) = setup().await;
    let phone = Device::new("phone");
    subscribed(&gateway, &rob, &phone).await;
    let chef = create_bot(&gateway, &rob, "Chef", json!(["notify"])).await;
    let thread = thread(&gateway, &rob, 1, chef).await;
    let limit = HOURLY_LIMIT;
    for id in 0..limit + 2 {
        notify(
            &gateway,
            1,
            500 + id,
            &thread,
            &format!("Note {id}"),
            "result",
        )
        .await;
    }
    deliver(&gateway).await;
    let opened = service.opened(&phone);
    assert_eq!(opened.len() as i64, limit);
    assert_eq!(opened[0]["notification"]["body"], "Note 0");
    assert!(
        opened
            .iter()
            .all(|m| !m["notification"]["body"].as_str().unwrap().is_empty())
    );
    let states: Vec<String> = pushes(&gateway).await.into_iter().map(|p| p.2).collect();
    assert_eq!(states.iter().filter(|s| *s == "sent").count() as i64, limit);
    assert_eq!(&states[limit as usize..], ["limited", "limited"]);

    // An hour on, pushes go again.
    gateway.state.clock.advance(HOUR_MS + 1);
    notify(&gateway, 1, 600, &thread, "Next hour", "result").await;
    deliver(&gateway).await;
    assert_eq!(service.opened(&phone).len() as i64, limit + 1);
}

#[tokio::test]
async fn without_a_contact_the_key_is_served_but_nothing_is_sent() {
    let (gateway, service) =
        with_service(Gateway::configured(|config| config.push_contact = None).await).await;
    let rob = gateway.admin("rob").await;
    let key = gateway
        .send(Call::get("/api/v1/push/key").cookie(&rob))
        .await;
    assert_eq!(key.status, StatusCode::OK);
    assert_eq!(key.body["sending"], false);
    subscribed(&gateway, &rob, &Device::new("phone")).await;
    let chef = create_bot(&gateway, &rob, "Chef", json!(["notify"])).await;
    let thread = thread(&gateway, &rob, 1, chef).await;
    notify(&gateway, 1, 901, &thread, "Ready", "result").await;
    deliver(&gateway).await;
    assert!(service.received().is_empty());
    assert_eq!(
        pushes(&gateway).await,
        vec![(1, "notify".into(), "unconfigured".into())]
    );
}

#[tokio::test]
async fn a_disabled_account_is_never_pushed() {
    let (gateway, service, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    subscribed(&gateway, &amy, &Device::new("amy-phone")).await;
    let diary = create_bot(&gateway, &amy, "Diary", json!(["notify"])).await;
    let thread = thread(&gateway, &amy, amy_id, diary).await;
    // Queued while she was enabled, then she is disabled before it goes.
    notify(&gateway, amy_id, 901, &thread, "Saved", "result").await;
    let disabled = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/admin/users/{amy_id}"))
                .cookie(&rob)
                .json(&json!({"disabled": true})),
        )
        .await;
    assert_eq!(disabled.status, StatusCode::OK);
    deliver(&gateway).await;
    assert!(service.received().is_empty());
    assert_eq!(
        pushes(&gateway).await,
        vec![(amy_id, "notify".into(), "disabled".into())]
    );
}

#[tokio::test]
async fn pushes_waiting_too_long_expire() {
    let (gateway, service, rob) = setup().await;
    subscribed(&gateway, &rob, &Device::new("phone")).await;
    let chef = create_bot(&gateway, &rob, "Chef", json!(["notify"])).await;
    let thread = thread(&gateway, &rob, 1, chef).await;
    notify(&gateway, 1, 901, &thread, "Stale", "result").await;
    gateway.state.clock.advance(HOUR_MS + 1);
    deliver(&gateway).await;
    assert!(service.received().is_empty());
    assert_eq!(
        pushes(&gateway).await,
        vec![(1, "notify".into(), "expired".into())]
    );
}

// ---------------------------------------------------------------------------
// Delivery

#[tokio::test]
async fn gone_subscriptions_are_removed_and_busy_services_are_retried() {
    let (gateway, service, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef", json!(["notify"])).await;
    let thread = thread(&gateway, &rob, 1, chef).await;
    let devices: Vec<(&str, Vec<Answer>)> = vec![
        ("gone", vec![Answer::status(410)]),
        ("unknown", vec![Answer::status(404)]),
        ("other-key", vec![Answer::key_mismatch()]),
        (
            "busy",
            vec![Answer::status(429).retry_after(1), Answer::status(201)],
        ),
        (
            "flaky",
            vec![
                Answer::status(500),
                Answer::status(503),
                Answer::status(201),
            ],
        ),
        ("down", vec![Answer::status(502); 4]),
        ("refusing", vec![Answer::status(400)]),
    ];
    let mut subscribed_devices = Vec::new();
    for (name, answers) in devices {
        let device = Device::new(name);
        let session = sign_in(&gateway, "rob").await;
        subscribed(&gateway, &session, &device).await;
        service.script(&device, answers);
        subscribed_devices.push((name, device));
    }
    notify(&gateway, 1, 901, &thread, "Ready", "result").await;
    deliver(&gateway).await;
    let attempts = |name: &str| {
        let device = &subscribed_devices.iter().find(|d| d.0 == name).unwrap().1;
        service.sent_to(device).len()
    };
    assert_eq!(attempts("gone"), 1);
    assert_eq!(attempts("unknown"), 1);
    assert_eq!(attempts("other-key"), 1);
    assert_eq!(attempts("busy"), 2);
    assert_eq!(attempts("flaky"), 3);
    assert_eq!(attempts("down"), 4, "three retries");
    assert_eq!(attempts("refusing"), 1);
    // Every attempt carried the same message, each device's to open.
    for (_, device) in &subscribed_devices {
        for message in service.opened(device) {
            assert_eq!(message["notification"]["body"], "Ready");
        }
    }
    let mut left: Vec<String> =
        sqlx::query_scalar("SELECT endpoint FROM push_subscriptions WHERE user_id = 1")
            .fetch_all(&gateway.state.db)
            .await
            .unwrap();
    left.sort();
    assert_eq!(
        left,
        ["busy", "down", "flaky", "refusing"].map(|n| format!("https://push.example/{n}"))
    );
    assert_eq!(
        pushes(&gateway).await,
        vec![(1, "notify".into(), "sent".into())]
    );

    // A push no device took is failed.
    service.clear();
    for name in ["busy", "down", "flaky", "refusing"] {
        let device = &subscribed_devices.iter().find(|d| d.0 == name).unwrap().1;
        service.script(device, vec![Answer::status(400)]);
    }
    notify(&gateway, 1, 902, &thread, "Again", "result").await;
    deliver(&gateway).await;
    assert_eq!(service.received().len(), 4);
    assert_eq!(pushes(&gateway).await[1].2, "failed");
}

#[tokio::test]
async fn pushes_left_being_delivered_by_a_stopped_gateway_are_failed() {
    let (gateway, _service, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef", json!([])).await;
    sqlx::query(
        "INSERT INTO pushes (user_id, bot_id, cause, urgency, title, body, state, created_at,
                             sent_at)
         VALUES (1, ?, 'notify', 'normal', 'Chef', 'x', 'sending', ?, ?)",
    )
    .bind(chef)
    .bind(START)
    .bind(START)
    .execute(&gateway.state.db)
    .await
    .unwrap();
    gateway.state.push.recover().await.unwrap();
    assert_eq!(
        pushes(&gateway).await,
        vec![(1, "notify".into(), "failed".into())]
    );
}

// ---------------------------------------------------------------------------
// Routines

async fn create_routine(gateway: &Gateway, cookie: &str, bot: i64, body: Value) -> i64 {
    let reply = gateway
        .send(
            Call::post(&format!("/api/v1/bots/{bot}/routines"))
                .cookie(cookie)
                .json(&body),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{:?}", reply.body);
    reply.body["id"].as_i64().unwrap()
}

async fn digest(gateway: &Gateway, cookie: &str, bot: i64, push: &str) -> i64 {
    create_routine(
        gateway,
        cookie,
        bot,
        json!({"name": "Digest", "prompt": "Go.", "push": push,
               "schedule": {"repeat": "daily", "at": "07:00"}}),
    )
    .await
}

async fn run_now(gateway: &Gateway, cookie: &str, bot: i64, routine: i64) -> Value {
    let reply = gateway
        .send(Call::post(&format!("/api/v1/bots/{bot}/routines/{routine}/run")).cookie(cookie))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    reply.body
}

/// Waits until the routine's newest run is no longer waiting or running.
async fn ended(gateway: &Gateway, cookie: &str, bot: i64, routine: i64) -> Value {
    let body = gateway
        .wait_for(
            cookie,
            &format!("/api/v1/bots/{bot}/routines/{routine}/runs"),
            |b| {
                b["runs"][0]["state"]
                    .as_str()
                    .is_some_and(|s| s != "waiting" && s != "running")
            },
        )
        .await;
    body["runs"][0].clone()
}

/// The routine pushes `device` opened, as (title, body).
fn bodies(service: &PushService, device: &Device) -> Vec<(String, String)> {
    service
        .opened(device)
        .iter()
        .map(|m| {
            (
                m["notification"]["title"].as_str().unwrap().to_owned(),
                m["notification"]["body"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[tokio::test]
async fn every_finished_run_pushes_its_replys_first_line() {
    let (gateway, service, rob) = setup().await;
    let phone = Device::new("phone");
    subscribed(&gateway, &rob, &phone).await;
    let chef = create_bot(&gateway, &rob, "Chef", json!([])).await;
    let routine = digest(&gateway, &rob, chef, "every_run").await;
    gateway.fake.with(|b| {
        b.reply = vec!["## **Rain** today\n".into(), "Take a coat.".into()];
    });
    run_now(&gateway, &rob, chef, routine).await;
    assert_eq!(
        ended(&gateway, &rob, chef, routine).await["state"],
        "succeeded"
    );
    deliver(&gateway).await;
    assert_eq!(
        bodies(&service, &phone),
        vec![("Chef · Digest".into(), "Rain today".into())]
    );
    let message = &service.opened(&phone)[0]["notification"];
    assert_eq!(message["navigate"], format!("{ORIGIN}/?bot={chef}"));
    assert_eq!(message["app_badge"], 1);

    // A run with no reply still says it finished.
    service.clear();
    gateway.fake.with(|b| b.reply = Vec::new());
    run_now(&gateway, &rob, chef, routine).await;
    ended(&gateway, &rob, chef, routine).await;
    deliver(&gateway).await;
    assert_eq!(
        bodies(&service, &phone),
        vec![("Chef · Digest".into(), "Finished, with no reply.".into())]
    );
}

#[tokio::test]
async fn a_run_that_notifies_pushes_once_with_its_notification() {
    let (gateway, service, rob) = setup().await;
    let phone = Device::new("phone");
    subscribed(&gateway, &rob, &phone).await;
    let chef = create_bot(&gateway, &rob, "Chef", json!(["notify"])).await;
    let tutor = create_bot(&gateway, &rob, "Tutor", json!(["notify"])).await;
    let every = digest(&gateway, &rob, chef, "every_run").await;
    let only = digest(&gateway, &rob, tutor, "notify_only").await;

    for (bot, routine, id) in [(chef, every, 901), (tutor, only, 902)] {
        gateway.fake.with(|b| b.finish_turns = false);
        let before = gateway.fake.with(|b| b.turns.len());
        run_now(&gateway, &rob, bot, routine).await;
        let (thread, turn) = {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                let found = gateway.fake.with(|b| b.turns.get(before).cloned());
                if let Some(found) = found {
                    break found;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the run's turn starts"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        let (ok, text) = notify_in(
            &gateway,
            1,
            id,
            &thread,
            &turn,
            json!({"title": "Digest ready", "body": "Three items.", "kind": "result"}),
        )
        .await;
        assert!(ok, "{text}");
        // Within the run, the notify does not push on its own.
        assert!(pushes(&gateway).await.iter().all(|p| p.1 != "notify"));
        gateway.fake.with(|b| b.finish_turns = true);
        gateway.fake.inject(
            UserId(1),
            json!({"method": "turn/completed", "params": {"threadId": thread,
                "turn": {"id": turn, "items": [], "status": "completed", "error": null}}}),
        );
        assert_eq!(
            ended(&gateway, &rob, bot, routine).await["state"],
            "succeeded"
        );
    }
    deliver(&gateway).await;
    assert_eq!(
        bodies(&service, &phone),
        vec![
            ("Chef · Digest".into(), "Digest ready\nThree items.".into()),
            ("Tutor · Digest".into(), "Digest ready\nThree items.".into()),
        ]
    );
    let causes: Vec<String> = pushes(&gateway).await.into_iter().map(|p| p.1).collect();
    assert_eq!(causes, ["run", "run"]);

    // A notify-only run that does not notify pushes nothing.
    service.clear();
    run_now(&gateway, &rob, tutor, only).await;
    assert_eq!(
        ended(&gateway, &rob, tutor, only).await["state"],
        "succeeded"
    );
    deliver(&gateway).await;
    assert!(service.received().is_empty());
}

#[tokio::test]
async fn failed_runs_push_and_the_third_says_the_routine_paused() {
    let (gateway, service, rob) = setup().await;
    let phone = Device::new("phone");
    subscribed(&gateway, &rob, &phone).await;
    let chef = create_bot(&gateway, &rob, "Chef", json!([])).await;
    // Notify-only routines still say when they fail.
    let routine = digest(&gateway, &rob, chef, "notify_only").await;
    gateway.fake.with(|b| b.reject_turns = true);
    for _ in 0..3 {
        assert_eq!(
            run_now(&gateway, &rob, chef, routine).await["state"],
            "failed"
        );
    }
    deliver(&gateway).await;
    let failed = "The Bots' runtime is unavailable.".to_owned();
    assert_eq!(
        bodies(&service, &phone),
        vec![
            ("Chef · Digest".into(), failed.clone()),
            ("Chef · Digest".into(), failed.clone()),
            (
                "Chef · Digest".into(),
                format!("{failed} Paused after 3 failed runs in a row.")
            ),
        ]
    );
    let causes: Vec<String> = pushes(&gateway).await.into_iter().map(|p| p.1).collect();
    assert_eq!(causes, ["failure", "failure", "pause"]);
}

#[tokio::test]
async fn a_run_its_owner_stopped_does_not_push() {
    let (gateway, service, rob) = setup().await;
    subscribed(&gateway, &rob, &Device::new("phone")).await;
    let chef = create_bot(&gateway, &rob, "Chef", json!([])).await;
    let routine = digest(&gateway, &rob, chef, "every_run").await;
    gateway.fake.with(|b| b.finish_turns = false);
    run_now(&gateway, &rob, chef, routine).await;
    gateway
        .wait_for(&rob, &format!("/api/v1/bots/{chef}"), |b| {
            b["status"] == "working"
        })
        .await;
    let stopped = gateway
        .send(Call::post(&format!("/api/v1/bots/{chef}/stop")).cookie(&rob))
        .await;
    assert_eq!(stopped.body["stopped"], true);
    let run = ended(&gateway, &rob, chef, routine).await;
    assert_eq!(run["state"], "interrupted");
    deliver(&gateway).await;
    assert!(service.received().is_empty());
    assert!(pushes(&gateway).await.is_empty());
}

/// Starts a run of `routine` whose turn stays open, and has its Bot
/// notify in it. Returns the turn's thread and id.
async fn run_that_notifies(
    gateway: &Gateway,
    cookie: &str,
    bot: i64,
    routine: i64,
    call: i64,
) -> (String, String) {
    gateway.fake.with(|b| b.finish_turns = false);
    let before = gateway.fake.with(|b| b.turns.len());
    run_now(gateway, cookie, bot, routine).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let (thread, turn) = loop {
        let found = gateway.fake.with(|b| b.turns.get(before).cloned());
        if let Some(found) = found {
            break found;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the run's turn starts"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    let (ok, text) = notify_in(
        gateway,
        1,
        call,
        &thread,
        &turn,
        json!({"title": "Need a decision", "body": "Lamb or fish?", "kind": "question"}),
    )
    .await;
    assert!(ok, "{text}");
    (thread, turn)
}

#[tokio::test]
async fn a_run_that_notified_and_then_failed_or_was_stopped_still_pushes_its_notification() {
    let (gateway, service, rob) = setup().await;
    let phone = Device::new("phone");
    subscribed(&gateway, &rob, &phone).await;
    let chef = create_bot(&gateway, &rob, "Chef", json!(["notify"])).await;
    let routine = digest(&gateway, &rob, chef, "notify_only").await;

    // Its turn fails after the notification: the push says both.
    let (thread, turn) = run_that_notifies(&gateway, &rob, chef, routine, 911).await;
    gateway.fake.with(|b| b.finish_turns = true);
    gateway.fake.inject(
        UserId(1),
        json!({"method": "turn/completed", "params": {"threadId": thread,
            "turn": {"id": turn, "items": [], "status": "failed",
                     "error": {"message": "boom"}}}}),
    );
    let run = ended(&gateway, &rob, chef, routine).await;
    assert_eq!(run["state"], "failed");
    deliver(&gateway).await;
    let sent = bodies(&service, &phone);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].0, "Chef · Digest");
    assert!(
        sent[0].1.starts_with("Need a decision\nLamb or fish?\n"),
        "{sent:?}"
    );
    assert!(sent[0].1.len() > "Need a decision\nLamb or fish?\n".len());
    assert_eq!(service.sent_to(&phone)[0].header("urgency"), Some("high"));

    // Its owner stops it after the notification: the notification alone.
    service.clear();
    run_that_notifies(&gateway, &rob, chef, routine, 912).await;
    let stopped = gateway
        .send(Call::post(&format!("/api/v1/bots/{chef}/stop")).cookie(&rob))
        .await;
    assert_eq!(stopped.body["stopped"], true);
    gateway.fake.with(|b| b.finish_turns = true);
    let run = ended(&gateway, &rob, chef, routine).await;
    assert_eq!(run["state"], "interrupted");
    deliver(&gateway).await;
    assert_eq!(
        bodies(&service, &phone),
        vec![(
            "Chef · Digest".into(),
            "Need a decision\nLamb or fish?".into()
        )]
    );
    let causes: Vec<String> = pushes(&gateway).await.into_iter().map(|p| p.1).collect();
    assert_eq!(causes, ["failure", "run"]);
}

#[tokio::test]
async fn a_missed_run_pushes_but_not_for_a_disabled_account() {
    let (gateway, service, rob) = setup().await;
    let phone = Device::new("phone");
    subscribed(&gateway, &rob, &phone).await;
    let chef = create_bot(&gateway, &rob, "Chef", json!([])).await;
    let routine = create_routine(
        &gateway,
        &rob,
        chef,
        json!({"name": "Digest", "prompt": "Go.",
               "schedule": {"repeat": "daily", "at": "19:00"}}),
    )
    .await;
    let (_amy, amy_id) = gateway.member(&rob, "amy").await;
    let amy_cookie = sign_in(&gateway, "amy").await;
    let diary = create_bot(&gateway, &amy_cookie, "Diary", json!([])).await;
    create_routine(
        &gateway,
        &amy_cookie,
        diary,
        json!({"name": "Journal", "prompt": "Go.",
               "schedule": {"repeat": "daily", "at": "19:00"}}),
    )
    .await;
    let disabled = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/admin/users/{amy_id}"))
                .cookie(&rob)
                .json(&json!({"disabled": true})),
        )
        .await;
    assert_eq!(disabled.status, StatusCode::OK);

    // Down from before 19:00 until after 01:30: more than six hours late.
    gateway.state.clock.advance(7 * HOUR_MS + 30 * MINUTE_MS);
    gateway
        .state
        .routines
        .pass(gateway.state.clock.now())
        .await
        .unwrap();
    gateway.state.routines.settle().await;
    let run = ended(&gateway, &rob, chef, routine).await;
    assert_eq!(run["state"], "missed");
    deliver(&gateway).await;
    assert_eq!(
        bodies(&service, &phone),
        vec![(
            "Chef · Digest".into(),
            "Missed a run. Bokkie was not running when it was due.".into()
        )]
    );
    // Amy's run was missed too, and nothing was queued for her.
    let users: Vec<i64> = pushes(&gateway).await.into_iter().map(|p| p.0).collect();
    assert_eq!(users, [1]);
}

#[tokio::test]
async fn a_notification_sent_before_its_run_was_running_is_carried_by_the_runs_push() {
    let (gateway, service, rob) = setup().await;
    let phone = Device::new("phone");
    subscribed(&gateway, &rob, &phone).await;
    gateway.fake.with(|b| {
        b.finish_turns = false;
        b.hold_turn_answers = true;
    });
    let chef = create_bot(&gateway, &rob, "Chef", json!(["notify"])).await;
    let routine = create_routine(
        &gateway,
        &rob,
        chef,
        json!({"name": "Digest", "prompt": "Go.", "push": "notify_only",
               "schedule": {"repeat": "daily", "at": "19:00"}}),
    )
    .await;
    gateway.state.clock.advance(30 * MINUTE_MS);
    let routines = &gateway.state.routines;
    routines.pass(gateway.state.clock.now()).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while gateway.fake.with(|b| b.turns.len()) == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the run's turn starts"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let (thread, turn) = gateway.fake.with(|b| b.turns[0].clone());
    // Before `turn/start` answers, the run is not yet running: the
    // notification queues a push of its own.
    let (ok, text) = notify_in(
        &gateway,
        1,
        901,
        &thread,
        &turn,
        json!({"title": "Early news", "kind": "question"}),
    )
    .await;
    assert!(ok, "{text}");
    assert_eq!(
        pushes(&gateway).await,
        vec![(1, "notify".into(), "queued".into())]
    );
    // The run starts and takes the notification: its push is merged.
    gateway.fake.turn_answers.add_permits(1);
    routines.settle().await;
    deliver(&gateway).await;
    assert!(service.received().is_empty());
    assert_eq!(
        pushes(&gateway).await,
        vec![(1, "notify".into(), "merged".into())]
    );

    gateway.fake.inject(
        UserId(1),
        json!({"method": "turn/completed", "params": {"threadId": thread,
            "turn": {"id": turn, "items": [], "status": "completed", "error": null}}}),
    );
    assert_eq!(
        ended(&gateway, &rob, chef, routine).await["state"],
        "succeeded"
    );
    deliver(&gateway).await;
    assert_eq!(
        bodies(&service, &phone),
        vec![("Chef · Digest".into(), "Early news".into())]
    );
    assert_eq!(service.received()[0].header("urgency"), Some("high"));
    let causes: Vec<String> = pushes(&gateway).await.into_iter().map(|p| p.1).collect();
    assert_eq!(causes, ["notify", "run"]);
}
