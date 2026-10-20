//! Unread state, presence and the notify tool: read markers through the
//! API and the event log, what each user's event sockets say they show,
//! and notifications from a fake app server's tool calls. Each keeps one
//! user's state from every other user.

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use bokkie_gateway::bots::store;
use bokkie_gateway::clock::DAY_MS;
use bokkie_gateway::tools::{audit, notify};
use bokkie_protocol::api::{NotificationKind, ToolCallOutcome};
use bokkie_protocol::events::{ClientFrame, Event, ServerFrame};
use bokkie_protocol::{BotId, MessageId, UserId};
use common::{Call, Gateway, HOST, ORIGIN};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A signed-in admin, with events routed.
async fn setup() -> (Gateway, String) {
    let gateway = Gateway::new().await;
    gateway.route().await;
    let admin = gateway.admin("rob").await;
    (gateway, admin)
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

/// Sends `text` and waits for the Bot's reply. Returns the reply's id.
async fn exchange(gateway: &Gateway, cookie: &str, bot: i64, text: &str) -> i64 {
    let before = replies(gateway, cookie, bot).await.len();
    let sent = gateway
        .send(
            Call::post(&format!("/api/v1/bots/{bot}/messages"))
                .cookie(cookie)
                .json(&json!({"text": text})),
        )
        .await;
    assert_eq!(sent.status, StatusCode::OK, "{:?}", sent.body);
    let path = format!("/api/v1/bots/{bot}");
    gateway
        .wait_for(cookie, &path, |b| b["status"] == "idle")
        .await;
    let replies = replies(gateway, cookie, bot).await;
    assert_eq!(replies.len(), before + 1, "one reply");
    replies.last().unwrap()["id"].as_i64().unwrap()
}

/// The Bot's complete text replies, oldest first.
async fn replies(gateway: &Gateway, cookie: &str, bot: i64) -> Vec<Value> {
    let page = gateway
        .send(Call::get(&format!("/api/v1/bots/{bot}/messages")).cookie(cookie))
        .await;
    assert_eq!(page.status, StatusCode::OK, "{:?}", page.body);
    page.body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["author"] == "bot" && m["kind"] == "text" && m["state"] == "complete")
        .cloned()
        .collect()
}

async fn mark_read(gateway: &Gateway, cookie: &str, bot: i64, message: i64) -> common::Reply {
    gateway
        .send(
            Call::new(Method::PUT, &format!("/api/v1/bots/{bot}/read"))
                .cookie(cookie)
                .json(&json!({"message_id": message})),
        )
        .await
}

async fn bot_list(gateway: &Gateway, cookie: &str) -> Value {
    let list = gateway.send(Call::get("/api/v1/bots").cookie(cookie)).await;
    assert_eq!(list.status, StatusCode::OK, "{:?}", list.body);
    list.body
}

fn listed(list: &Value, bot: i64) -> Value {
    list["bots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == bot)
        .cloned()
        .unwrap_or_else(|| panic!("Bot {bot} is not in {list}"))
}

/// The user's events with the given type, from the event log.
async fn read_updates(gateway: &Gateway, user: i64) -> Vec<Value> {
    gateway
        .state
        .events
        .since(UserId(user), bokkie_protocol::events::Seq(0), 10_000)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::ReadUpdated { read } => Some(serde_json::to_value(read).unwrap()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_bot_is_unread_after_it_writes_until_its_user_reads_it() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef", json!([])).await;
    let tutor = create_bot(&gateway, &rob, "Tutor", json!([])).await;

    // A new Bot that has written nothing is read.
    let list = bot_list(&gateway, &rob).await;
    assert_eq!(list["unread_bots"], 0);
    assert_eq!(listed(&list, chef)["unread"], false);
    assert_eq!(listed(&list, chef)["read_up_to"], Value::Null);

    // Its reply makes it unread; the user's own message never does.
    let first = exchange(&gateway, &rob, chef, "Plan dinner.").await;
    let list = bot_list(&gateway, &rob).await;
    assert_eq!(listed(&list, chef)["unread"], true);
    assert_eq!(listed(&list, tutor)["unread"], false);
    assert_eq!(list["unread_bots"], 1);
    let one = gateway
        .send(Call::get(&format!("/api/v1/bots/{chef}")).cookie(&rob))
        .await;
    assert_eq!(one.body["unread"], true);

    // Reading up to the reply clears it, and says so to every device.
    let read = mark_read(&gateway, &rob, chef, first).await;
    assert_eq!(read.status, StatusCode::OK, "{:?}", read.body);
    assert_eq!(
        read.body,
        json!({"bot_id": chef, "read_up_to": first, "unread": false})
    );
    assert_eq!(read_updates(&gateway, 1).await, vec![read.body.clone()]);
    let list = bot_list(&gateway, &rob).await;
    assert_eq!(listed(&list, chef)["unread"], false);
    assert_eq!(listed(&list, chef)["read_up_to"], first);
    assert_eq!(list["unread_bots"], 0);

    // The marker only moves forward: reading an older message, or the
    // same one again, changes nothing and says nothing.
    let second = exchange(&gateway, &rob, chef, "And dessert?").await;
    assert_eq!(bot_list(&gateway, &rob).await["unread_bots"], 1);
    let user_message = gateway
        .send(Call::get(&format!("/api/v1/bots/{chef}/messages")).cookie(&rob))
        .await
        .body["messages"][0]["id"]
        .as_i64()
        .unwrap();
    for older in [first, user_message] {
        let again = mark_read(&gateway, &rob, chef, older).await;
        assert_eq!(again.status, StatusCode::OK, "{:?}", again.body);
        assert_eq!(again.body["read_up_to"], first);
        assert_eq!(again.body["unread"], true);
    }
    assert_eq!(read_updates(&gateway, 1).await.len(), 1);
    let read = mark_read(&gateway, &rob, chef, second).await;
    assert_eq!(read.body["unread"], false);
    assert_eq!(read_updates(&gateway, 1).await.len(), 2);

    // Only the Bot's own messages can be read on it.
    let tutors = exchange(&gateway, &rob, tutor, "Hello.").await;
    for (bot, message) in [(chef, tutors), (chef, 999_999), (tutor, second)] {
        let refused = mark_read(&gateway, &rob, bot, message).await;
        assert_eq!(
            refused.status,
            StatusCode::BAD_REQUEST,
            "{:?}",
            refused.body
        );
    }
    let malformed = gateway
        .send(
            Call::new(Method::PUT, &format!("/api/v1/bots/{chef}/read"))
                .cookie(&rob)
                .json(&json!({"message": second})),
        )
        .await;
    assert!(malformed.status.is_client_error());
    let list = bot_list(&gateway, &rob).await;
    assert_eq!(listed(&list, chef)["read_up_to"], second);
    assert_eq!(listed(&list, tutor)["unread"], true);
    assert_eq!(list["unread_bots"], 1);
}

#[tokio::test]
async fn a_hidden_bot_still_counts_as_unread() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef", json!([])).await;
    exchange(&gateway, &rob, chef, "Plan dinner.").await;
    let hidden = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/bots/{chef}"))
                .cookie(&rob)
                .json(&json!({"hidden": true})),
        )
        .await;
    assert_eq!(hidden.status, StatusCode::OK, "{:?}", hidden.body);
    assert_eq!(bot_list(&gateway, &rob).await["unread_bots"], 1);
}

#[tokio::test]
async fn one_user_cannot_read_or_see_anothers_unread_state() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let (ben, ben_id) = gateway.member(&rob, "ben").await;
    let amys = create_bot(&gateway, &amy, "Diary", json!([])).await;
    let amys_reply = exchange(&gateway, &amy, amys, "Dear diary.").await;
    let bens = create_bot(&gateway, &ben, "Diary", json!([])).await;
    exchange(&gateway, &ben, bens, "Dear diary.").await;

    // Ben can neither read Amy's Bot nor move his marker with her message.
    let theirs = mark_read(&gateway, &ben, amys, amys_reply).await;
    assert_eq!(theirs.status, StatusCode::NOT_FOUND, "{:?}", theirs.body);
    let hers = mark_read(&gateway, &ben, bens, amys_reply).await;
    assert_eq!(hers.status, StatusCode::BAD_REQUEST, "{:?}", hers.body);
    assert_eq!(
        store::read_state(&gateway.state.db, UserId(amy_id), BotId(amys))
            .await
            .unwrap()
            .unwrap()
            .read_up_to,
        None
    );
    let bens_state = store::read_state(&gateway.state.db, UserId(ben_id), BotId(bens))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bens_state.read_up_to, None);
    assert!(bens_state.unread);
    // Nor reach it through the store, naming the wrong user.
    assert!(
        !store::mark_read(
            &gateway.state.db,
            gateway.state.clock.now(),
            UserId(ben_id),
            BotId(amys),
            MessageId(amys_reply),
        )
        .await
        .unwrap()
    );
    assert_eq!(
        store::read_state(&gateway.state.db, UserId(ben_id), BotId(amys))
            .await
            .unwrap(),
        None
    );

    // Amy's reading reaches only Amy's events, and her count only her.
    let read = mark_read(&gateway, &amy, amys, amys_reply).await;
    assert_eq!(read.status, StatusCode::OK, "{:?}", read.body);
    assert_eq!(read_updates(&gateway, amy_id).await.len(), 1);
    assert!(read_updates(&gateway, ben_id).await.is_empty());
    assert_eq!(bot_list(&gateway, &amy).await["unread_bots"], 0);
    assert_eq!(bot_list(&gateway, &ben).await["unread_bots"], 1);

    // An admin overseeing Amy sees her conversation, not her reading.
    let oversight = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/admin/users/{amy_id}"))
                .cookie(&rob)
                .json(&json!({"oversight": true})),
        )
        .await;
    assert_eq!(oversight.status, StatusCode::OK, "{:?}", oversight.body);
    let overseen = gateway
        .send(Call::get(&format!("/api/v1/admin/users/{amy_id}/bots")).cookie(&rob))
        .await;
    assert_eq!(overseen.status, StatusCode::OK, "{:?}", overseen.body);
    let bot = &overseen.body["bots"][0]["overseen"];
    assert!(bot["last_message"]["text"].is_string(), "{bot}");
    assert_eq!(bot["read_up_to"], Value::Null);
    assert_eq!(bot["unread"], false);
    // And the admin's own count is the admin's.
    assert_eq!(bot_list(&gateway, &rob).await["unread_bots"], 0);
}

#[tokio::test]
async fn upgrading_leaves_nothing_unread() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bokkie.sqlite3");
    {
        let before = sqlx::migrate::Migrator {
            migrations: std::borrow::Cow::Owned(
                bokkie_gateway::db::MIGRATIONS
                    .iter()
                    .filter(|m| m.version < 11)
                    .cloned()
                    .collect(),
            ),
            ..sqlx::migrate::Migrator::DEFAULT
        };
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let pool = sqlx::SqlitePool::connect_with(options).await.unwrap();
        before.run(&pool).await.unwrap();
        for statement in [
            "INSERT INTO users (id, username, display_name, role, password_hash, created_at,
                                updated_at) VALUES (1, 'rob', 'Rob', 'admin', 'x', 1, 1)",
            "INSERT INTO bots (id, user_id, name, label, description, created_at, updated_at)
                 VALUES (5, 1, 'Chef', '', '', 1, 1)",
            "INSERT INTO bots (id, user_id, name, label, description, created_at, updated_at)
                 VALUES (6, 1, 'Tutor', '', '', 1, 1)",
            "INSERT INTO messages (id, bot_id, user_id, author, text, state, created_at,
                                   updated_at)
                 VALUES (10, 5, 1, 'user', 'hello', 'complete', 1, 1),
                        (11, 5, 1, 'bot', 'hi', 'complete', 2, 2),
                        (12, 5, 1, 'user', 'thanks', 'complete', 3, 3)",
        ] {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }
        pool.close().await;
    }
    let db = bokkie_gateway::db::open(&path).await.unwrap();
    let chef = store::read_state(&db, UserId(1), BotId(5))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(chef.read_up_to, Some(MessageId(12)));
    assert!(!chef.unread);
    let tutor = store::read_state(&db, UserId(1), BotId(6))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tutor.read_up_to, None);
    assert!(!tutor.unread);
    assert_eq!(store::unread_bots(&db, UserId(1)).await.unwrap(), 0);
}

async fn socket(address: SocketAddr, cookie: &str) -> Socket {
    let mut request = format!("ws://{address}/api/v1/events")
        .into_client_request()
        .unwrap();
    let headers = request.headers_mut();
    headers.insert("host", HOST.parse().unwrap());
    headers.insert("origin", ORIGIN.parse().unwrap());
    headers.insert("cookie", cookie.parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    send(&mut socket, &ClientFrame::Resume { after: None }).await;
    socket
}

async fn send(socket: &mut Socket, frame: &ClientFrame) {
    let frame = serde_json::to_string(frame).unwrap();
    socket.send(Message::text(frame)).await.unwrap();
}

/// Sends a ping and waits for its pong, so every frame sent before it
/// has been handled.
async fn settle(socket: &mut Socket) {
    send(socket, &ClientFrame::Ping).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let Some(Ok(Message::Text(text))) = socket.next().await else {
                panic!("the socket closed");
            };
            if matches!(serde_json::from_str(&text), Ok(ServerFrame::Pong)) {
                return;
            }
        }
    })
    .await
    .expect("a pong arrives in time");
}

async fn eventually(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the condition holds in time");
}

#[tokio::test]
async fn each_socket_says_which_of_its_users_bots_it_shows_until_it_closes() {
    let (gateway, rob) = setup().await;
    let address = gateway.serve().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let robs = create_bot(&gateway, &rob, "Chef", json!([])).await;
    let amys = create_bot(&gateway, &amy, "Diary", json!([])).await;
    let presence = &gateway.state.presence;
    let shown = |user: i64| presence.shown(UserId(user));

    let mut phone = socket(address, &rob).await;
    let mut laptop = socket(address, &rob).await;
    send(
        &mut phone,
        &ClientFrame::Viewing {
            bot_id: Some(BotId(robs)),
        },
    )
    .await;
    settle(&mut phone).await;
    assert!(presence.is_shown(UserId(1), BotId(robs)));
    // Naming another user's Bot shows nothing, for either user.
    send(
        &mut laptop,
        &ClientFrame::Viewing {
            bot_id: Some(BotId(amys)),
        },
    )
    .await;
    settle(&mut laptop).await;
    assert_eq!(shown(1), vec![BotId(robs)]);
    assert!(shown(amy_id).is_empty());
    assert!(!presence.is_shown(UserId(amy_id), BotId(amys)));
    assert!(!presence.is_shown(UserId(1), BotId(amys)));
    // Nor does a Bot that does not exist.
    send(
        &mut laptop,
        &ClientFrame::Viewing {
            bot_id: Some(BotId(9_999)),
        },
    )
    .await;
    settle(&mut laptop).await;
    assert_eq!(shown(1), vec![BotId(robs)]);

    // Amy's own socket shows her Bot, to her only.
    let mut amys_socket = socket(address, &amy).await;
    send(
        &mut amys_socket,
        &ClientFrame::Viewing {
            bot_id: Some(BotId(amys)),
        },
    )
    .await;
    settle(&mut amys_socket).await;
    assert_eq!(shown(amy_id), vec![BotId(amys)]);
    assert_eq!(shown(1), vec![BotId(robs)]);

    // The page hidden, then the socket closed: nothing is shown.
    send(&mut phone, &ClientFrame::Viewing { bot_id: None }).await;
    settle(&mut phone).await;
    assert!(shown(1).is_empty());
    send(
        &mut phone,
        &ClientFrame::Viewing {
            bot_id: Some(BotId(robs)),
        },
    )
    .await;
    settle(&mut phone).await;
    assert!(presence.is_shown(UserId(1), BotId(robs)));
    phone.close(None).await.unwrap();
    drop(phone);
    eventually(|| shown(1).is_empty()).await;
    drop(amys_socket);
    eventually(|| shown(amy_id).is_empty()).await;
    drop(laptop);

    // What a socket said lapses unless it says it again.
    let mut tablet = socket(address, &rob).await;
    send(
        &mut tablet,
        &ClientFrame::Viewing {
            bot_id: Some(BotId(robs)),
        },
    )
    .await;
    settle(&mut tablet).await;
    assert!(presence.is_shown(UserId(1), BotId(robs)));
    gateway
        .state
        .clock
        .advance(bokkie_protocol::events::VIEWING_EXPIRES_MS as i64);
    assert!(!presence.is_shown(UserId(1), BotId(robs)));
}

/// Starts the Bot's thread with one exchange and returns its id.
async fn thread(gateway: &Gateway, cookie: &str, user: i64, bot: i64) -> String {
    exchange(gateway, cookie, bot, "hi").await;
    store::bot(&gateway.state.db, UserId(user), BotId(bot))
        .await
        .unwrap()
        .unwrap()
        .thread_id
        .unwrap()
}

/// The model, through `user`'s app server, calls `tool` on `thread`.
fn call(gateway: &Gateway, user: i64, id: i64, thread: &str, tool: &str, arguments: Value) {
    gateway.fake.inject(
        UserId(user),
        json!({"id": id, "method": "item/tool/call", "params": {
            "threadId": thread, "turnId": "turn-x", "callId": format!("call-{id}"),
            "tool": tool, "namespace": null, "arguments": arguments}}),
    );
}

/// Whether the gateway's answer to server request `id` succeeded, and its
/// text.
async fn answer(gateway: &Gateway, id: i64) -> (bool, String) {
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

async fn notifications(gateway: &Gateway, cookie: &str) -> Value {
    let page = gateway
        .send(Call::get("/api/v1/notifications").cookie(cookie))
        .await;
    assert_eq!(page.status, StatusCode::OK, "{:?}", page.body);
    page.body
}

async fn messages(gateway: &Gateway, cookie: &str, bot: i64) -> Vec<Value> {
    gateway
        .send(Call::get(&format!("/api/v1/bots/{bot}/messages")).cookie(cookie))
        .await
        .body["messages"]
        .as_array()
        .unwrap()
        .clone()
}

#[tokio::test]
async fn notify_records_a_notification_its_marker_and_its_log_entry() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef", json!(["notify"])).await;
    let thread = thread(&gateway, &rob, 1, chef).await;
    let reply = replies(&gateway, &rob, chef).await[0]["id"]
        .as_i64()
        .unwrap();
    assert_eq!(
        mark_read(&gateway, &rob, chef, reply).await.status,
        StatusCode::OK
    );

    call(
        &gateway,
        1,
        901,
        &thread,
        "notify",
        json!({"title": "  Dinner plan ready ", "body": "Pasta on Monday.\nCurry on Tuesday.",
               "kind": "result"}),
    );
    let (ok, text) = answer(&gateway, 901).await;
    assert!(ok, "{text}");
    assert_eq!(text, notify::ANSWER);

    // The marker is the Bot's, in the conversation, with the call's turn.
    let messages = messages(&gateway, &rob, chef).await;
    let marker = messages.last().unwrap();
    assert_eq!(marker["author"], "bot");
    assert_eq!(marker["kind"], "marker");
    assert_eq!(marker["state"], "complete");
    assert_eq!(marker["text"], "Dinner plan ready");
    assert_eq!(
        marker["notification"],
        json!({"kind": "result", "body": "Pasta on Monday.\nCurry on Tuesday."})
    );
    // Other messages carry no notification.
    assert!(messages[0].get("notification").is_none());
    // It makes the Bot unread.
    assert_eq!(
        listed(&bot_list(&gateway, &rob).await, chef)["unread"],
        true
    );

    let page = notifications(&gateway, &rob).await;
    assert_eq!(page["older"], Value::Null);
    let recorded = &page["notifications"][0];
    assert_eq!(recorded["bot_id"], chef);
    assert_eq!(recorded["message_id"], marker["id"]);
    assert_eq!(recorded["kind"], "result");
    assert_eq!(recorded["title"], "Dinner plan ready");
    assert_eq!(recorded["body"], "Pasta on Monday.\nCurry on Tuesday.");
    assert!(recorded.get("routine_run_id").is_none(), "{recorded}");
    let stored: (String, String, String) =
        sqlx::query_as("SELECT thread_id, turn_id, call_id FROM notifications WHERE id = ?")
            .bind(recorded["id"].as_i64().unwrap())
            .fetch_one(&gateway.state.db)
            .await
            .unwrap();
    assert_eq!(stored, (thread.clone(), "turn-x".into(), "call-901".into()));

    // The event reached the user's log with the marker.
    let created = gateway
        .state
        .events
        .since(UserId(1), bokkie_protocol::events::Seq(0), 10_000)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::MessageCreated { message } if message.notification.is_some() => Some(message),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(created.len(), 1);
    assert_eq!(
        created[0].notification.as_ref().unwrap().kind,
        NotificationKind::Result
    );

    // The log names the kind and title.
    let calls = audit::page(&gateway.state.db, UserId(1), BotId(chef), None, 10)
        .await
        .unwrap()
        .0;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].tool, "notify");
    assert_eq!(calls[0].outcome, ToolCallOutcome::Ok);
    assert_eq!(calls[0].summary, "result: Dinner plan ready");
    assert!(gateway.state.supervisor.tainted().is_none());

    // A question, with no body, is the newest.
    call(
        &gateway,
        1,
        902,
        &thread,
        "notify",
        json!({"title": "Which night for fish?", "kind": "question"}),
    );
    assert!(answer(&gateway, 902).await.0);
    let page = notifications(&gateway, &rob).await;
    assert_eq!(page["notifications"][0]["kind"], "question");
    assert_eq!(page["notifications"][0]["body"], "");
    assert_eq!(page["notifications"][1]["title"], "Dinner plan ready");
    let first = gateway
        .send(Call::get("/api/v1/notifications?limit=1").cookie(&rob))
        .await;
    assert_eq!(first.body["notifications"].as_array().unwrap().len(), 1);
    let older = first.body["older"].as_i64().unwrap();
    let rest = gateway
        .send(Call::get(&format!("/api/v1/notifications?before={older}")).cookie(&rob))
        .await;
    assert_eq!(rest.body["notifications"][0]["title"], "Dinner plan ready");
}

#[tokio::test]
async fn notify_refuses_bad_arguments_and_bots_without_the_grant() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef", json!(["notify"])).await;
    let chefs = thread(&gateway, &rob, 1, chef).await;
    let long_title = "x".repeat(81);
    let long_body = "y".repeat(501);
    let bad = [
        json!({"title": "", "kind": "result"}),
        json!({"title": long_title, "kind": "result"}),
        json!({"title": "Two\nlines", "kind": "result"}),
        json!({"title": "Ok", "body": long_body, "kind": "result"}),
        json!({"title": "Ok", "body": "bell\u{7}", "kind": "result"}),
        json!({"title": "Ok", "kind": "urgent"}),
        json!({"title": "Ok"}),
        json!({"title": 5, "kind": "result"}),
    ];
    for (n, arguments) in bad.iter().enumerate() {
        let id = 910 + n as i64;
        call(&gateway, 1, id, &chefs, "notify", arguments.clone());
        let (ok, text) = answer(&gateway, id).await;
        assert!(!ok, "{arguments} was accepted: {text}");
    }
    // The limits are characters, not bytes.
    call(
        &gateway,
        1,
        930,
        &chefs,
        "notify",
        json!({"title": "é".repeat(80), "body": "é".repeat(500), "kind": "question"}),
    );
    assert!(answer(&gateway, 930).await.0);
    assert_eq!(
        notifications(&gateway, &rob).await["notifications"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let calls = audit::page(&gateway.state.db, UserId(1), BotId(chef), None, 50)
        .await
        .unwrap()
        .0;
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.outcome == ToolCallOutcome::Failed)
            .count(),
        bad.len()
    );

    // Without the grant, the call is refused and records nothing.
    let plain = create_bot(&gateway, &rob, "Tutor", json!([])).await;
    let plain_thread = thread(&gateway, &rob, 1, plain).await;
    call(
        &gateway,
        1,
        931,
        &plain_thread,
        "notify",
        json!({"title": "Hi", "kind": "result"}),
    );
    assert!(!answer(&gateway, 931).await.0);
    assert_eq!(
        notifications(&gateway, &rob).await["notifications"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        messages(&gateway, &rob, plain)
            .await
            .iter()
            .all(|m| m.get("notification").is_none())
    );
}

#[tokio::test]
async fn a_forged_notify_from_another_users_app_server_reaches_nothing_of_theirs() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let (ben, ben_id) = gateway.member(&rob, "ben").await;
    let amys = create_bot(&gateway, &amy, "Diary", json!(["notify"])).await;
    let amys_thread = thread(&gateway, &amy, amy_id, amys).await;
    let bens = create_bot(&gateway, &ben, "Diary", json!(["notify"])).await;
    let bens_thread = thread(&gateway, &ben, ben_id, bens).await;
    let amys_messages = messages(&gateway, &amy, amys).await.len();

    // Ben's app server names Amy's thread.
    call(
        &gateway,
        ben_id,
        940,
        &amys_thread,
        "notify",
        json!({"title": "Ben was here", "kind": "result"}),
    );
    assert!(!answer(&gateway, 940).await.0);
    // Arguments naming Amy, her Bot or her thread are only arguments: the
    // call is Ben's Bot's, on Ben's thread.
    call(
        &gateway,
        ben_id,
        941,
        &bens_thread,
        "notify",
        json!({"title": "Ben's", "kind": "result", "bot": amys, "user": amy_id,
               "threadId": amys_thread}),
    );
    assert!(answer(&gateway, 941).await.0);

    assert_eq!(
        notifications(&gateway, &amy).await["notifications"],
        json!([])
    );
    assert_eq!(messages(&gateway, &amy, amys).await.len(), amys_messages);
    let bens_page = notifications(&gateway, &ben).await;
    assert_eq!(bens_page["notifications"].as_array().unwrap().len(), 1);
    assert_eq!(bens_page["notifications"][0]["bot_id"], bens);
    assert_eq!(
        notifications(&gateway, &rob).await["notifications"],
        json!([])
    );
    assert!(
        notify::page(&gateway.state.db, UserId(amy_id), None, 50)
            .await
            .unwrap()
            .0
            .is_empty()
    );
    assert!(
        gateway
            .state
            .events
            .since(UserId(amy_id), bokkie_protocol::events::Seq(0), 10_000)
            .await
            .unwrap()
            .iter()
            .all(|e| !matches!(&e.event, Event::MessageCreated { message }
                if message.notification.is_some()))
    );
    assert!(gateway.state.supervisor.tainted().is_none());
}

#[tokio::test]
async fn notifications_are_kept_ninety_days_and_their_markers_stay() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef", json!(["notify"])).await;
    let thread = thread(&gateway, &rob, 1, chef).await;
    let clock = &gateway.state.clock;
    let start = clock.now();
    call(
        &gateway,
        1,
        950,
        &thread,
        "notify",
        json!({"title": "Old", "kind": "result"}),
    );
    assert!(answer(&gateway, 950).await.0);
    clock.advance(DAY_MS);
    call(
        &gateway,
        1,
        951,
        &thread,
        "notify",
        json!({"title": "Newer", "kind": "result"}),
    );
    assert!(answer(&gateway, 951).await.0);
    clock.advance(89 * DAY_MS);
    let conversations = &gateway.state.conversations;
    conversations.roll_over(start, clock.now()).await.unwrap();
    let page = notifications(&gateway, &rob).await;
    assert_eq!(page["notifications"].as_array().unwrap().len(), 1);
    assert_eq!(page["notifications"][0]["title"], "Newer");
    clock.advance(DAY_MS);
    conversations.roll_over(start, clock.now()).await.unwrap();
    assert_eq!(
        notifications(&gateway, &rob).await["notifications"],
        json!([])
    );
    let markers: Vec<String> = messages(&gateway, &rob, chef)
        .await
        .iter()
        .filter(|m| m["notification"].is_object())
        .map(|m| m["text"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(markers, ["Old", "Newer"]);
}

#[tokio::test]
async fn notifications_need_a_session() {
    let (gateway, _rob) = setup().await;
    let page = gateway.send(Call::get("/api/v1/notifications")).await;
    assert!(
        [401, 403].contains(&page.status.as_u16()),
        "{}",
        page.status
    );
    let read = gateway
        .send(Call::new(Method::PUT, "/api/v1/bots/1/read").json(&json!({"message_id": 1})))
        .await;
    assert!(
        [401, 403].contains(&read.status.as_u16()),
        "{}",
        read.status
    );
}
