//! Routines through the whole gateway, against the real pinned `codex
//! app-server`, a mock model and a synthetic household sign-in. Runs only
//! when `BOKKIE_CODEX_BIN` names the binary. Nothing leaves the machine.
//!
//! A routine's run is a turn in its Bot's own thread: the model is given
//! the routine's prompt and is told it is a routine's run, its reply lands
//! in the conversation behind the run's marker, a tool it calls is logged
//! against the run, and a later message from the user follows on in the
//! same thread.

mod common;

use std::time::Duration;

use bokkie_mock_model::MockModel;
use common::live::Live;
use common::{Call, Gateway};
use serde_json::{Value, json};

fn request(mock: &MockModel, index: usize) -> Value {
    mock.requests.lock().unwrap()[index].clone()
}

fn request_count(mock: &MockModel) -> usize {
    mock.requests.lock().unwrap().len()
}

/// Waits until the routine's newest run has ended, and returns it.
async fn ended_run(gateway: &Gateway, cookie: &str, bot: i64, routine: i64, runs: usize) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let page = gateway
            .send(Call::get(&format!("/api/v1/bots/{bot}/routines/{routine}/runs")).cookie(cookie))
            .await;
        let all = page.body["runs"].as_array().unwrap();
        if all.len() == runs && !matches!(all[0]["state"].as_str(), Some("waiting" | "running")) {
            return all[0].clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no end: {}",
            page.body
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn thread_of(gateway: &Gateway, bot: i64) -> String {
    sqlx::query_scalar("SELECT thread_id FROM bots WHERE id = ?")
        .bind(bot)
        .fetch_one(&gateway.state.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_routine_run_gets_a_reply_in_the_bots_own_thread() {
    let Some(live) = Live::start(&["Digest ready."]).await else {
        return;
    };
    let mock = &live.model;
    let gateway = Gateway::live(&live).await;
    gateway.route().await;
    let rob = gateway.admin("rob").await;
    let bot = gateway
        .send(Call::post("/api/v1/bots").cookie(&rob).json(&json!({
            "name": "Scout", "label": "Research", "description": "Find papers."})))
        .await;
    let bot = bot.body["id"].as_i64().unwrap();
    let granted = gateway
        .send(
            Call::new(axum::http::Method::PATCH, &format!("/api/v1/bots/{bot}"))
                .cookie(&rob)
                .json(&json!({"grants": ["notes"]})),
        )
        .await;
    assert_eq!(granted.status, 200, "{:?}", granted.body);
    let routine = gateway
        .send(
            Call::post(&format!("/api/v1/bots/{bot}/routines"))
                .cookie(&rob)
                .json(&json!({"name": "Morning digest",
                              "prompt": "Write today's digest about TANGERINES.",
                              "schedule": {"repeat": "daily", "at": "07:00"}})),
        )
        .await;
    assert_eq!(routine.status, 201, "{:?}", routine.body);
    let next_run_at = routine.body["next_run_at"].as_i64().unwrap();
    let routine = routine.body["id"].as_i64().unwrap();

    // Its time comes: the scheduler's pass at that time runs it, and the
    // model calls a tool.
    mock.call_tool_next("notes_append", json!({"text": "Digest sent."}));
    gateway
        .state
        .routines
        .pass(bokkie_protocol::Timestamp(next_run_at))
        .await
        .unwrap();
    let run = ended_run(&gateway, &rob, bot, routine, 1).await;
    assert_eq!(run["state"], "succeeded", "{run}");
    assert_eq!(run["trigger"], "schedule");
    assert!(run["tokens"].as_u64().is_some_and(|t| t > 0), "{run}");
    assert_eq!(request_count(mock), 2, "a tool call and its follow-up");
    let first = request(mock, 0).to_string();
    assert!(first.contains("Write today's digest about TANGERINES."));
    assert!(
        first.contains("This turn is a run of your routine \\\"Morning digest\\\""),
        "{first}"
    );
    assert!(first.contains("07:00 on"), "{first}");

    // The conversation: the marker, with the prompt folded into it, then
    // the reply.
    let page = gateway
        .send(Call::get(&format!("/api/v1/bots/{bot}/messages")).cookie(&rob))
        .await;
    let messages = page.body["messages"].as_array().unwrap();
    let marker = messages
        .iter()
        .find(|m| m["kind"] == "marker" && m["routine"].is_object())
        .expect("the run's marker");
    assert_eq!(marker["text"], "Morning digest · 7:00");
    assert_eq!(
        marker["routine"]["prompt"],
        "Write today's digest about TANGERINES."
    );
    let reply = messages.last().unwrap();
    assert_eq!(reply["text"], "Digest ready.");
    assert_eq!(reply["state"], "complete");
    assert_eq!(run["messages"], json!([marker["id"], reply["id"]]));

    // The tool call is logged against the run.
    let calls = gateway
        .send(Call::get(&format!("/api/v1/bots/{bot}/tool-calls")).cookie(&rob))
        .await;
    assert_eq!(calls.body["calls"][0]["tool"], "notes_append");
    assert_eq!(calls.body["calls"][0]["routine_run_id"], run["id"]);
    let thread = thread_of(&gateway, bot).await;

    // "Run now" takes the same path, in the same thread.
    let manual = gateway
        .send(Call::post(&format!("/api/v1/bots/{bot}/routines/{routine}/run")).cookie(&rob))
        .await;
    assert_eq!(manual.status, 200, "{:?}", manual.body);
    let manual = ended_run(&gateway, &rob, bot, routine, 2).await;
    assert_eq!(manual["state"], "succeeded", "{manual}");
    assert_eq!(manual["trigger"], "manual");
    assert_eq!(thread_of(&gateway, bot).await, thread);
    assert!(
        request(mock, 2)
            .to_string()
            .contains("test run of your routine")
    );

    // A follow-up from the user continues the same thread, with the runs in
    // its history.
    let sent = gateway
        .send(
            Call::post(&format!("/api/v1/bots/{bot}/messages"))
                .cookie(&rob)
                .json(&json!({"text": "Tell me more."})),
        )
        .await;
    assert_eq!(sent.status, 200, "{:?}", sent.body);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while request_count(mock) < 4 {
        assert!(tokio::time::Instant::now() < deadline, "no follow-up turn");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let follow_up = request(mock, 3).to_string();
    assert!(follow_up.contains("Tell me more."));
    assert!(follow_up.contains("Write today's digest about TANGERINES."));
    assert!(follow_up.contains("Digest ready."));
    assert_eq!(thread_of(&gateway, bot).await, thread);
    live.assert_nothing_left_the_machine();
}
