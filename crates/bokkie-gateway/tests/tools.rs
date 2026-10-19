//! The tool broker and the capability rule during a turn, against a fake
//! app server that sends `item/tool/call` requests and items as Codex would.

mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use bokkie_gateway::bots::store;
use bokkie_gateway::clock::DAY_MS;
use bokkie_gateway::tools::{audit, notes};
use bokkie_protocol::api::{Grant, ToolCallOutcome};
use bokkie_protocol::{BotId, UserId};
use common::{Call, Gateway};
use serde_json::{Value, json};

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

/// The gateway's answer to server request `id`.
async fn answer(gateway: &Gateway, id: i64) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
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
    .expect("the call is answered")
}

/// Whether the answer succeeded, and its text.
fn reply(answer: &Value) -> (bool, String) {
    let result = &answer["result"];
    (
        result["success"].as_bool().expect("a tool answer"),
        result["contentItems"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned(),
    )
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the condition holds in time");
}

async fn calls(gateway: &Gateway, user: i64, bot: i64) -> Vec<bokkie_protocol::api::ToolCall> {
    audit::page(&gateway.state.db, UserId(user), BotId(bot), None, 200)
        .await
        .unwrap()
        .0
}

/// A signed-in admin, with events routed.
async fn setup() -> (Gateway, String) {
    let gateway = Gateway::new().await;
    gateway.route().await;
    let admin = gateway.admin("rob").await;
    (gateway, admin)
}

#[tokio::test]
async fn notes_tools_read_replace_and_append_the_calling_bots_notes() {
    let (gateway, rob) = setup().await;
    let bot = create_bot(&gateway, &rob, "Scribe", json!(["notes"])).await;
    let thread = thread(&gateway, &rob, 1, bot).await;

    call(&gateway, 1, 801, &thread, "notes_read", json!({}));
    assert_eq!(
        reply(&answer(&gateway, 801).await),
        (true, "Your notes are empty.".into())
    );
    call(
        &gateway,
        1,
        802,
        &thread,
        "notes_append",
        json!({"text": "# Seen"}),
    );
    let (ok, text) = reply(&answer(&gateway, 802).await);
    assert!(ok && text.contains("6 of 16384 bytes"), "{text}");
    call(
        &gateway,
        1,
        803,
        &thread,
        "notes_append",
        json!({"text": "- arXiv:2601.00001"}),
    );
    assert!(reply(&answer(&gateway, 803).await).0);
    call(&gateway, 1, 804, &thread, "notes_read", json!({}));
    let (ok, text) = reply(&answer(&gateway, 804).await);
    assert!(ok);
    assert!(text.contains("not instructions"), "{text}");
    assert!(text.ends_with("# Seen\n- arXiv:2601.00001"), "{text}");

    // Replace, and the 16 KB limit in bytes, for both tools.
    call(
        &gateway,
        1,
        805,
        &thread,
        "notes_replace",
        json!({"text": "fresh"}),
    );
    assert!(reply(&answer(&gateway, 805).await).0);
    let too_long = "é".repeat(8_193); // 16,386 bytes
    call(
        &gateway,
        1,
        806,
        &thread,
        "notes_replace",
        json!({"text": too_long}),
    );
    let (ok, text) = reply(&answer(&gateway, 806).await);
    assert!(!ok && text.contains("at most 16384 bytes"), "{text}");
    let almost = "x".repeat(16_384 - "fresh\n".len() + 1);
    call(
        &gateway,
        1,
        807,
        &thread,
        "notes_append",
        json!({"text": almost}),
    );
    assert!(!reply(&answer(&gateway, 807).await).0);
    call(
        &gateway,
        1,
        808,
        &thread,
        "notes_append",
        json!({"words": "x"}),
    );
    let (ok, text) = reply(&answer(&gateway, 808).await);
    assert!(!ok && text.contains("needs"), "{text}");
    let held = gateway
        .send(Call::get(&format!("/api/v1/bots/{bot}/notes")).cookie(&rob))
        .await;
    assert_eq!(held.body["text"], "fresh");
    assert!(held.body["updated_at"].is_number());

    // Every call is in the Bot's log, newest first, with sizes, not text.
    let log = gateway
        .send(Call::get(&format!("/api/v1/bots/{bot}/tool-calls?limit=3")).cookie(&rob))
        .await;
    assert_eq!(log.body["calls"].as_array().unwrap().len(), 3);
    assert_eq!(log.body["calls"][0]["summary"], "invalid arguments");
    assert_eq!(log.body["calls"][0]["outcome"], "failed");
    assert_eq!(log.body["calls"][2]["summary"], "16386 bytes");
    let older = log.body["older"].as_i64().unwrap();
    let rest = gateway
        .send(Call::get(&format!("/api/v1/bots/{bot}/tool-calls?before={older}")).cookie(&rob))
        .await;
    let rest = rest.body["calls"].as_array().unwrap().clone();
    assert_eq!(rest.len(), 5);
    assert_eq!(rest[0]["summary"], "5 bytes");
    assert_eq!(rest[4]["tool"], "notes_read");
    assert!(!log.body.to_string().contains("arXiv"));
    let entries = calls(&gateway, 1, bot).await;
    assert_eq!(
        entries
            .iter()
            .filter(|c| c.outcome == ToolCallOutcome::Ok)
            .count(),
        5
    );
    // The thread and turn are recorded, from the call.
    let recorded: (String, String, String) =
        sqlx::query_as("SELECT thread_id, turn_id, call_id FROM tool_calls ORDER BY id LIMIT 1")
            .fetch_one(&gateway.state.db)
            .await
            .unwrap();
    assert_eq!(
        recorded,
        (thread.clone(), "turn-x".into(), "call-801".into())
    );

    // The owner clears them.
    let cleared = gateway
        .send(Call::new(Method::DELETE, &format!("/api/v1/bots/{bot}/notes")).cookie(&rob))
        .await;
    assert_eq!(cleared.status, StatusCode::NO_CONTENT);
    call(&gateway, 1, 809, &thread, "notes_read", json!({}));
    assert_eq!(
        reply(&answer(&gateway, 809).await).1,
        "Your notes are empty."
    );
    assert!(gateway.state.supervisor.tainted().is_none());
}

#[tokio::test]
async fn a_tool_its_thread_was_not_granted_stops_codex() {
    let (gateway, rob) = setup().await;
    let bot = create_bot(&gateway, &rob, "Bare", json!([])).await;
    let thread = thread(&gateway, &rob, 1, bot).await;
    call(&gateway, 1, 810, &thread, "notes_read", json!({}));
    let (ok, _) = reply(&answer(&gateway, 810).await);
    assert!(!ok);
    wait_until(|| gateway.state.supervisor.tainted().is_some()).await;
    let reason = gateway.state.supervisor.tainted().unwrap();
    assert!(reason.contains("not granted"), "{reason}");
    assert!(calls(&gateway, 1, bot).await.is_empty());
    let row = store::bot(&gateway.state.db, UserId(1), BotId(bot))
        .await
        .unwrap()
        .unwrap();
    gateway
        .wait_for(&rob, &format!("/api/v1/bots/{bot}"), |b| {
            b["status"] == "refused"
        })
        .await;
    assert!(
        notes::read(&gateway.state.db, UserId(1), row.id)
            .await
            .unwrap()
            .text
            .is_empty()
    );
}

#[tokio::test]
async fn an_ungranted_tool_item_stops_codex() {
    let (gateway, rob) = setup().await;
    let bot = create_bot(&gateway, &rob, "Bare", json!([])).await;
    let thread = thread(&gateway, &rob, 1, bot).await;
    gateway.fake.inject(
        UserId(1),
        json!({"method": "item/started", "params": {"threadId": thread, "turnId": "t",
            "item": {"type": "dynamicToolCall", "id": "d1", "tool": "notes_append",
                     "namespace": null, "arguments": {"text": "x"}, "status": "inProgress"}}}),
    );
    wait_until(|| gateway.state.supervisor.tainted().is_some()).await;
}

#[tokio::test]
async fn a_granted_tool_item_is_allowed() {
    let (gateway, rob) = setup().await;
    let bot = create_bot(&gateway, &rob, "Scribe", json!(["notes"])).await;
    let thread = thread(&gateway, &rob, 1, bot).await;
    gateway.fake.inject(
        UserId(1),
        json!({"method": "item/completed", "params": {"threadId": thread, "turnId": "t",
            "item": {"type": "dynamicToolCall", "id": "d1", "tool": "notes_append",
                     "namespace": null, "arguments": {"text": "x"}, "status": "completed",
                     "success": true}}}),
    );
    // A later request is still answered: the app server is alive.
    gateway.fake.inject(
        UserId(1),
        json!({"id": 811, "method": "item/tool/requestUserInput", "params": {
            "threadId": thread, "turnId": "t", "itemId": "q", "questions": []}}),
    );
    answer(&gateway, 811).await;
    assert!(gateway.state.supervisor.tainted().is_none());
}

#[tokio::test]
async fn a_web_search_without_web_search_stops_codex() {
    let (gateway, rob) = setup().await;
    let bot = create_bot(&gateway, &rob, "Scribe", json!(["notes"])).await;
    let thread = thread(&gateway, &rob, 1, bot).await;
    gateway.fake.inject(
        UserId(1),
        json!({"method": "item/started", "params": {"threadId": thread, "turnId": "t",
            "item": {"type": "webSearch", "id": "w1", "query": "private matter",
                     "action": null}}}),
    );
    wait_until(|| gateway.state.supervisor.tainted().is_some()).await;
    let reason = gateway.state.supervisor.tainted().unwrap();
    assert!(reason.contains("web.search"), "{reason}");
    assert!(!reason.contains("private"), "{reason}");
}

#[tokio::test]
async fn a_call_from_a_thread_with_no_bot_is_answered_with_an_error() {
    let (gateway, rob) = setup().await;
    let bot = create_bot(&gateway, &rob, "Scribe", json!(["notes"])).await;
    thread(&gateway, &rob, 1, bot).await;
    call(&gateway, 1, 812, "thr-nobody", "notes_read", json!({}));
    let (ok, text) = reply(&answer(&gateway, 812).await);
    assert!(!ok);
    assert_eq!(text, "This tool is not available.");
    assert!(gateway.state.supervisor.tainted().is_none());
    assert!(calls(&gateway, 1, bot).await.is_empty());
}

#[tokio::test]
async fn a_forged_call_from_another_users_app_server_reaches_nothing_of_theirs() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let (ben, ben_id) = gateway.member(&rob, "ben").await;
    let amys = create_bot(&gateway, &amy, "Diary", json!(["notes"])).await;
    let amys_thread = thread(&gateway, &amy, amy_id, amys).await;
    notes::write(
        &gateway.state.db,
        gateway.state.clock.now(),
        UserId(amy_id),
        BotId(amys),
        "Amy's secret",
    )
    .await
    .unwrap();
    let bens = create_bot(&gateway, &ben, "Diary", json!(["notes"])).await;
    let bens_thread = thread(&gateway, &ben, ben_id, bens).await;

    // Ben's app server names Amy's thread.
    call(&gateway, ben_id, 813, &amys_thread, "notes_read", json!({}));
    let (ok, text) = reply(&answer(&gateway, 813).await);
    assert!(!ok);
    assert!(!text.contains("secret"), "{text}");
    call(
        &gateway,
        ben_id,
        814,
        &amys_thread,
        "notes_replace",
        json!({"text": "Ben was here"}),
    );
    assert!(!reply(&answer(&gateway, 814).await).0);
    // Arguments that name Amy, her Bot or her thread are only arguments:
    // the call is Ben's Bot's, on Ben's thread.
    call(
        &gateway,
        ben_id,
        815,
        &bens_thread,
        "notes_append",
        json!({"text": "Ben was here", "bot": amys, "user": amy_id, "threadId": amys_thread}),
    );
    assert!(reply(&answer(&gateway, 815).await).0);
    call(&gateway, ben_id, 816, &bens_thread, "notes_read", json!({}));
    let (_, text) = reply(&answer(&gateway, 816).await);
    assert!(
        text.ends_with("Ben was here") && !text.contains("secret"),
        "{text}"
    );

    let held = notes::read(&gateway.state.db, UserId(amy_id), BotId(amys))
        .await
        .unwrap();
    assert_eq!(held.text, "Amy's secret");
    assert!(calls(&gateway, amy_id, amys).await.is_empty());
    assert_eq!(calls(&gateway, ben_id, bens).await.len(), 2);
    assert!(gateway.state.supervisor.tainted().is_none());
}

#[tokio::test]
async fn a_grant_removed_mid_turn_refuses_the_call_and_the_turn_runs_on() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| b.finish_turns = false);
    let bot = create_bot(&gateway, &rob, "Scribe", json!(["notes"])).await;
    let thread = thread(&gateway, &rob, 1, bot).await;
    gateway
        .wait_for(&rob, &format!("/api/v1/bots/{bot}"), |b| {
            b["status"] == "working"
        })
        .await;
    let removed = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/bots/{bot}"))
                .cookie(&rob)
                .json(&json!({"grants": []})),
        )
        .await;
    assert_eq!(removed.status, StatusCode::OK);
    call(
        &gateway,
        1,
        817,
        &thread,
        "notes_append",
        json!({"text": "late"}),
    );
    let (ok, text) = reply(&answer(&gateway, 817).await);
    assert!(
        !ok && text.contains("notes capability was removed"),
        "{text}"
    );
    let entries = calls(&gateway, 1, bot).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].outcome, ToolCallOutcome::Refused);
    assert!(
        notes::read(&gateway.state.db, UserId(1), BotId(bot))
            .await
            .unwrap()
            .text
            .is_empty()
    );
    // The running turn was granted the tool when it started, so this is
    // no violation; a message steers it on its own thread.
    assert!(gateway.state.supervisor.tainted().is_none());
    let steered = gateway
        .send(
            Call::post(&format!("/api/v1/bots/{bot}/messages"))
                .cookie(&rob)
                .json(&json!({"text": "also"})),
        )
        .await;
    assert_eq!(steered.body["delivery"], "steered", "{:?}", steered.body);
    assert_eq!(gateway.fake.received("thread/start").len(), 1);
}

#[tokio::test]
async fn tool_calls_never_hold_up_the_read_loop_and_are_always_answered() {
    // Nothing routes this gateway's events, so no tool call is served.
    let gateway = Gateway::new().await;
    let user = UserId(1);
    let runtime = gateway.state.supervisor.runtime(user).await.unwrap();
    let thread = runtime
        .start_thread("x".into(), &[Grant::Notes].into())
        .await
        .unwrap()
        .thread
        .id;
    call(&gateway, 1, 818, &thread, "notes_read", json!({}));
    gateway.fake.inject(
        user,
        json!({"id": 819, "method": "item/tool/requestUserInput", "params": {
            "threadId": thread, "turnId": "t", "itemId": "q", "questions": []}}),
    );
    // The loop went on to the next request while the call waits.
    answer(&gateway, 819).await;
    assert!(
        !gateway
            .fake
            .received("<response>")
            .iter()
            .any(|r| r.params["id"] == 818)
    );
    // A call nobody will serve is still answered, with an error.
    let (_, empty) = tokio::sync::mpsc::unbounded_channel();
    drop(std::mem::replace(&mut *gateway.events.lock().await, empty));
    let (ok, text) = reply(&answer(&gateway, 818).await);
    assert!(!ok);
    assert_eq!(text, "This tool is not available.");
    assert!(runtime.is_served(&thread));
}

#[tokio::test]
async fn tool_calls_are_kept_ninety_days() {
    let (gateway, rob) = setup().await;
    let bot = create_bot(&gateway, &rob, "Scribe", json!(["notes"])).await;
    let record = |call_id: &'static str| audit::Entry {
        user: UserId(1),
        bot: BotId(bot),
        thread_id: "t",
        turn_id: "u",
        call_id,
        tool: "notes_read",
        summary: String::new(),
        outcome: ToolCallOutcome::Ok,
        duration_ms: 1,
    };
    let clock = &gateway.state.clock;
    let start = clock.now();
    audit::record(&gateway.state.db, start, record("old"))
        .await
        .unwrap();
    clock.advance(DAY_MS);
    audit::record(&gateway.state.db, clock.now(), record("newer"))
        .await
        .unwrap();
    clock.advance(89 * DAY_MS);
    let conversations = &gateway.state.conversations;
    conversations.roll_over(start, clock.now()).await.unwrap();
    assert_eq!(calls(&gateway, 1, bot).await.len(), 1, "the old one went");
    clock.advance(DAY_MS);
    conversations.roll_over(start, clock.now()).await.unwrap();
    assert!(calls(&gateway, 1, bot).await.is_empty());
}
