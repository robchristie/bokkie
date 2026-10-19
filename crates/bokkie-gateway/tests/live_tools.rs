//! Grants and the tool broker through the whole gateway, against the real
//! pinned `codex app-server`, a mock model that can call a tool, and a
//! synthetic household sign-in. Runs only when `BOKKIE_CODEX_BIN` names the
//! binary. Nothing leaves the machine.
//!
//! The model is offered exactly the tools of the Bot's grants; a grant
//! change starts a fresh thread at the next turn, carrying the conversation
//! over; and a tool call reaches the broker, which answers it from the
//! calling Bot's own notes or seen items.

mod common;

use std::collections::BTreeSet;
use std::time::Duration;

use bokkie_mock_model::MockModel;
use common::live::Live;
use common::{Call, Gateway};
use serde_json::{Value, json};

fn names(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

/// Sends `text` to the Bot and waits until it has `replies` complete text
/// replies. Returns the conversation.
async fn exchange(gateway: &Gateway, cookie: &str, bot: i64, text: &str, replies: usize) -> Value {
    let sent = gateway
        .send(
            Call::post(&format!("/api/v1/bots/{bot}/messages"))
                .cookie(cookie)
                .json(&json!({"text": text})),
        )
        .await;
    assert_eq!(sent.status, 200, "{:?}", sent.body);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let page = gateway
            .send(Call::get(&format!("/api/v1/bots/{bot}/messages")).cookie(cookie))
            .await;
        let done = page.body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["author"] == "bot" && m["kind"] == "text" && m["state"] == "complete")
            .count();
        if done >= replies {
            return page.body;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no reply: {}",
            page.body
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn request(mock: &MockModel, index: usize) -> Value {
    mock.requests.lock().unwrap()[index].clone()
}

fn request_count(mock: &MockModel) -> usize {
    mock.requests.lock().unwrap().len()
}

/// The output the model got back for its latest tool call, from the
/// request that followed it.
fn tool_output(request: &Value) -> String {
    request["input"]
        .as_array()
        .unwrap()
        .iter()
        .rfind(|item| item["type"] == "function_call_output")
        .map(|item| item["output"].to_string())
        .unwrap_or_else(|| panic!("no tool output in {request}"))
}

#[tokio::test]
async fn grants_set_the_tools_and_notes_work_end_to_end() {
    let Some(live) = Live::start(&["Noted."]).await else {
        return;
    };
    let mock = &live.model;
    let gateway = Gateway::live(&live).await;
    gateway.route().await;
    let rob = gateway.admin("rob").await;
    let bot = gateway
        .send(Call::post("/api/v1/bots").cookie(&rob).json(&json!({
            "name": "Scribe", "label": "Note taker", "description": "Keep notes."})))
        .await;
    let bot = bot.body["id"].as_i64().unwrap();
    let only_questions = names(&["request_user_input"]);
    let with_notes = names(&[
        "notes_append",
        "notes_read",
        "notes_replace",
        "request_user_input",
    ]);

    // No grants: no Bokkie tool, and the instructions say so.
    exchange(&gateway, &rob, bot, "First, about APRICOTS.", 1).await;
    let first = request(mock, 0);
    assert_eq!(MockModel::tools_in(&first), only_questions);
    assert!(
        first
            .to_string()
            .contains("You can only talk in this conversation.")
    );

    // Granting notes takes effect at the next turn, on a fresh thread
    // whose first turn carries the conversation over.
    let granted = gateway
        .send(
            Call::new(axum::http::Method::PATCH, &format!("/api/v1/bots/{bot}"))
                .cookie(&rob)
                .json(&json!({"grants": ["notes"]})),
        )
        .await;
    assert_eq!(granted.status, 200, "{:?}", granted.body);
    assert_eq!(granted.body["grants"], json!(["notes"]));
    mock.call_tool_next("notes_append", json!({"text": "The owner likes apricots."}));
    let conversation = exchange(&gateway, &rob, bot, "Remember that.", 2).await;
    assert_eq!(request_count(mock), 3, "a tool call and its follow-up");
    let fresh = request(mock, 1);
    assert_eq!(MockModel::tools_in(&fresh), with_notes);
    let fresh_text = fresh.to_string();
    assert!(fresh_text.contains("notes_read, notes_replace and notes_append"));
    assert!(!fresh_text.contains("You can only talk in this conversation."));
    // Codex 0.163.0 gives an untrusted fragment to the model as a user
    // message, tagged as external.
    let carried = fresh["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item.to_string().contains("bokkie-earlier-conversation"))
        .expect("the carry-over reaches the model");
    assert_eq!(carried["role"], "user", "{carried}");
    let carried = carried.to_string();
    assert!(carried.contains("<external_bokkie-earlier-conversation>"));
    assert!(carried.contains("rob: First, about APRICOTS.\\nYou: Noted."));
    assert!(tool_output(&request(mock, 2)).contains("Added to your notes"));
    // The marker sits between the threads.
    let texts: Vec<(String, String)> = conversation["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["kind"].as_str().unwrap().to_owned(),
                m["text"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let marker = texts
        .iter()
        .position(|(kind, text)| kind == "marker" && text == "Capabilities changed: notes")
        .expect("a marker");
    assert_eq!(texts[marker + 1].1, "Remember that.");

    // The broker wrote the calling Bot's notes, and logged the call.
    let notes = gateway
        .send(Call::get(&format!("/api/v1/bots/{bot}/notes")).cookie(&rob))
        .await;
    assert_eq!(notes.body["text"], "The owner likes apricots.");
    let calls = gateway
        .send(Call::get(&format!("/api/v1/bots/{bot}/tool-calls")).cookie(&rob))
        .await;
    assert_eq!(calls.body["calls"][0]["tool"], "notes_append");
    assert_eq!(calls.body["calls"][0]["outcome"], "ok");
    assert_eq!(calls.body["calls"][0]["summary"], "25 bytes");

    // Unchanged grants keep the thread, and its tools read the notes back.
    mock.call_tool_next("notes_read", json!({}));
    exchange(&gateway, &rob, bot, "What do you know?", 3).await;
    assert_eq!(request_count(mock), 5);
    let read = request(mock, 4);
    assert_eq!(MockModel::tools_in(&read), with_notes);
    assert!(
        tool_output(&read).contains("The owner likes apricots."),
        "{}",
        tool_output(&read)
    );
    // The carry-over stays in the thread's history, sent once.
    assert_eq!(
        read.to_string()
            .matches("<external_bokkie-earlier-conversation>")
            .count(),
        1
    );

    // Removing the grant starts another fresh thread with no tools, which
    // is not reminded of notes it can no longer use.
    let removed = gateway
        .send(
            Call::new(axum::http::Method::PATCH, &format!("/api/v1/bots/{bot}"))
                .cookie(&rob)
                .json(&json!({"grants": []})),
        )
        .await;
    assert_eq!(removed.status, 200, "{:?}", removed.body);
    exchange(&gateway, &rob, bot, "And now?", 4).await;
    let last = request(mock, 5);
    assert_eq!(MockModel::tools_in(&last), only_questions);
    let last_text = last.to_string();
    assert!(last_text.contains("You can only talk in this conversation."));
    assert!(last_text.contains("What do you know?"));
    assert!(!last_text.contains("likes apricots"));
    live.assert_nothing_left_the_machine();
}

#[tokio::test]
async fn seen_items_work_end_to_end_beside_notes() {
    let Some(live) = Live::start(&["Done."]).await else {
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
                .json(&json!({"grants": ["seen", "notes"]})),
        )
        .await;
    assert_eq!(granted.status, 200, "{:?}", granted.body);

    // The Bot marks what it showed...
    mock.call_tool_next(
        "seen_mark",
        json!({"items": [{"id": "https://arxiv.org/abs/2401.12345v2", "title": "A paper"}]}),
    );
    exchange(&gateway, &rob, bot, "Show me something.", 1).await;
    let first = request(mock, 0);
    assert_eq!(
        MockModel::tools_in(&first),
        names(&[
            "notes_append",
            "notes_read",
            "notes_replace",
            "request_user_input",
            "seen_check",
            "seen_mark",
        ])
    );
    assert!(first.to_string().contains("seen_check and seen_mark"));
    let marked = tool_output(&request(mock, 1));
    assert!(
        marked.contains("Marked 1 item as shown: arxiv:2401.12345."),
        "{marked}"
    );

    // ...and later finds it seen, under any of its ids, beside a new one.
    mock.call_tool_next(
        "seen_check",
        json!({"items": ["arXiv:2401.12345", "2401.54321"]}),
    );
    exchange(&gateway, &rob, bot, "Anything new?", 2).await;
    let checked = tool_output(&request(mock, 3));
    assert!(checked.contains("A paper"), "{checked}");
    assert!(checked.contains("arxiv:2401.54321"), "{checked}");

    let calls = gateway
        .send(Call::get(&format!("/api/v1/bots/{bot}/tool-calls")).cookie(&rob))
        .await;
    assert_eq!(calls.body["calls"][0]["tool"], "seen_check");
    assert_eq!(calls.body["calls"][0]["summary"], "2 items");
    assert_eq!(calls.body["calls"][1]["tool"], "seen_mark");
    assert_eq!(
        calls.body["calls"][1]["summary"],
        "1 item: arxiv:2401.12345"
    );
    assert_eq!(calls.body["calls"][1]["outcome"], "ok");
    live.assert_nothing_left_the_machine();
}
