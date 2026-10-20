//! Web search against the real pinned `codex app-server`, with a mock model
//! and a synthetic household sign-in. Runs only when `BOKKIE_CODEX_BIN`
//! names the binary. Nothing reaches the web: the mock answers both of
//! Codex's search paths, and each test shows that nothing tried to leave
//! the machine.
//!
//! A thread granted `web.search` sets `web_search = "live"` for itself
//! alone. What Codex 0.163.0 then offers depends on the model:
//!
//! - on a Responses-Lite model, which is every bundled model but
//!   `gpt-5.5` and so the default, Codex's own `web.run` tool, which the
//!   app server runs by calling the provider's search endpoint;
//! - on `gpt-5.5`, the provider's hosted `web_search` tool, which the
//!   provider runs within its response.
//!
//! Either way a search reaches the client as a `webSearch` item, which the
//! gateway allows only on a thread granted `web.search`.

mod common;

use std::time::Duration;

use bokkie_codex::protocol::{ServerNotification, ThreadItem, TurnStatus};
use bokkie_gateway::codex::{HouseholdSettings, RuntimeEventKind, Supervisor, catalog};
use bokkie_gateway::grants::GrantSet;
use bokkie_mock_model::{MockModel, SEARCH_OUTPUT};
use bokkie_protocol::UserId;
use bokkie_protocol::api::Grant;
use common::live::Live;
use serde_json::{Value, json};

type Events = tokio::sync::mpsc::UnboundedReceiver<bokkie_gateway::codex::RuntimeEvent>;

/// The one bundled model that is not Responses-Lite.
const HOSTED_MODEL: &str = "gpt-5.5";

/// A supervisor on `live`'s mocks, on the default model or on `model`.
async fn supervisor(
    live: &Live,
    data: &std::path::Path,
    model: Option<&str>,
) -> (std::sync::Arc<Supervisor>, Events) {
    let bundled = catalog::export_bundled(&live.program, &data.join("codex/catalog-home"))
        .await
        .unwrap();
    let catalog = catalog::write(&data.join("codex/models.json"), bundled).unwrap();
    live.sign_in_household(data, 86_400);
    let more = model
        .map(|m| vec![format!("model=\"{m}\"")])
        .unwrap_or_default();
    Supervisor::with_settings(
        data,
        catalog,
        live.launcher_with(more),
        HouseholdSettings::default(),
    )
}

/// Every tool a model request offered, top-level or in a Responses-Lite
/// `additional_tools` item.
fn offered(request: &Value) -> Vec<Value> {
    let mut tools = request["tools"].as_array().cloned().unwrap_or_default();
    for item in request["input"].as_array().into_iter().flatten() {
        if item["type"] == "additional_tools" {
            tools.extend(item["tools"].as_array().cloned().unwrap_or_default());
        }
    }
    tools
}

/// The search a request offered: `"web.run"`, `"hosted"` or nothing.
fn search_offered(request: &Value) -> Option<&'static str> {
    let tools = offered(request);
    let standalone = tools.iter().any(|tool| {
        tool["name"] == "web"
            && tool["tools"]
                .as_array()
                .is_some_and(|nested| nested.iter().any(|t| t["name"] == "run"))
    });
    let hosted = tools.iter().find(|tool| tool["type"] == "web_search");
    match (standalone, hosted) {
        (true, None) => Some("web.run"),
        (false, Some(hosted)) => {
            // Live mode: the provider may reach the live web.
            assert_eq!(hosted["external_web_access"], true, "{hosted}");
            Some("hosted")
        }
        (false, None) => None,
        (true, Some(_)) => panic!("both search paths offered"),
    }
}

/// What one turn did.
struct Turn {
    status: TurnStatus,
    items: Vec<ThreadItem>,
    refused: Option<String>,
    request: Value,
}

/// Runs one turn on a served thread, and returns its items, any refusal,
/// and the turn's first model request.
async fn turn(
    runtime: &bokkie_gateway::codex::Runtime,
    events: &mut Events,
    mock: &MockModel,
    thread: &str,
) -> Turn {
    let asked = mock.requests.lock().unwrap().len();
    let started = runtime.start_turn(thread, "hi", None).await.unwrap();
    let mut items = Vec::new();
    let mut refused = None;
    let status = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match events.recv().await.expect("events flow").kind {
                RuntimeEventKind::Notification(ServerNotification::ItemCompleted(item))
                    if item.thread_id == thread =>
                {
                    items.push(item.item)
                }
                RuntimeEventKind::Notification(ServerNotification::TurnCompleted(done))
                    if done.turn.id == started.id =>
                {
                    return done.turn.status;
                }
                RuntimeEventKind::Refused { reason, .. } => {
                    refused = Some(reason);
                    return TurnStatus::Failed;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("the turn ends");
    let request = mock.requests.lock().unwrap()[asked].clone();
    Turn {
        status,
        items,
        refused,
        request,
    }
}

fn searches(items: &[ThreadItem]) -> Vec<(String, Option<Value>)> {
    items
        .iter()
        .filter_map(|item| match item {
            ThreadItem::WebSearch { query, action, .. } => Some((query.clone(), action.clone())),
            _ => None,
        })
        .collect()
}

/// On each model, only a `web.search` thread offers search, by that
/// model's path, and a cold resume, which restores no configuration
/// override, still does because the gateway sends it again. The control: a
/// `web.search` thread resumed without the override has no search, so a
/// missed resend fails safe.
#[tokio::test]
async fn only_web_search_threads_search_after_start_and_cold_resume() {
    let Some(live) = Live::start(&["ok"]).await else {
        return;
    };
    let mock = &live.model;
    for (model, path) in [(None, "web.run"), (Some(HOSTED_MODEL), "hosted")] {
        let data = tempfile::tempdir().unwrap();
        let (supervisor, mut events) = supervisor(&live, data.path(), model).await;
        let user = UserId(1);
        let runtime = supervisor.runtime(user).await.unwrap();
        let searching: GrantSet = [Grant::WebSearch, Grant::Notes].into();
        let notes: GrantSet = [Grant::Notes].into();
        let scout = runtime
            .start_thread("Scout".into(), &searching)
            .await
            .unwrap();
        if let Some(model) = model {
            assert_eq!(scout.model, model);
        }
        let scout = scout.thread.id;
        let scribe = runtime
            .start_thread("Scribe".into(), &notes)
            .await
            .unwrap()
            .thread
            .id;
        let unsent = runtime
            .start_thread("Unsent".into(), &searching)
            .await
            .unwrap()
            .thread
            .id;
        for (thread, expected) in [(&scout, Some(path)), (&scribe, None), (&unsent, Some(path))] {
            let done = turn(&runtime, &mut events, mock, thread).await;
            assert_eq!(done.status, TurnStatus::Completed);
            assert_eq!(search_offered(&done.request), expected, "{model:?}");
        }

        supervisor.stop(user).await;
        let runtime = supervisor.runtime(user).await.unwrap();
        runtime
            .resume_thread(&scout, "Scout".into(), &searching)
            .await
            .unwrap();
        runtime
            .resume_thread(&scribe, "Scribe".into(), &notes)
            .await
            .unwrap();
        // Resumed as if it had no web.search, so without the override.
        runtime
            .resume_thread(&unsent, "Unsent".into(), &notes)
            .await
            .unwrap();
        for (thread, expected) in [(&scout, Some(path)), (&scribe, None), (&unsent, None)] {
            let done = turn(&runtime, &mut events, mock, thread).await;
            assert_eq!(done.status, TurnStatus::Completed);
            eprintln!("{model:?} resumed: {:?}", search_offered(&done.request));
            assert_eq!(search_offered(&done.request), expected, "{model:?} resumed");
        }
    }
    assert!(mock.searches.lock().unwrap().is_empty());
    live.assert_nothing_left_the_machine();
}

/// On the default model, the model calls `web.run`; the app server asks
/// the provider's search endpoint, here the mock's, and reports a
/// `webSearch` item, which the gateway allows on a `web.search` thread.
#[tokio::test]
async fn web_run_searches_through_the_provider_and_is_allowed() {
    let Some(live) = Live::start(&["Found it."]).await else {
        return;
    };
    let mock = &live.model;
    let data = tempfile::tempdir().unwrap();
    let (supervisor, mut events) = supervisor(&live, data.path(), None).await;
    let runtime = supervisor.runtime(UserId(1)).await.unwrap();
    let thread = runtime
        .start_thread("Scout".into(), &[Grant::WebSearch].into())
        .await
        .unwrap()
        .thread
        .id;
    mock.call_namespaced_tool_next(
        "web",
        "run",
        json!({"search_query": [{"q": "bokkie mock query"}]}),
    );
    let done = turn(&runtime, &mut events, mock, &thread).await;
    assert_eq!(done.refused, None);
    assert_eq!(done.status, TurnStatus::Completed);
    let found = searches(&done.items);
    eprintln!("web.run items: {found:?}");
    assert_eq!(found.len(), 1, "{:?}", done.items);
    assert!(found[0].0.contains("bokkie mock query"), "{found:?}");
    // The search went to the provider, never to the web, and its output
    // went back to the model.
    let asked = mock.searches.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert!(asked[0].to_string().contains("bokkie mock query"));
    eprintln!("search request settings: {}", asked[0]["settings"]);
    assert_eq!(asked[0]["settings"]["external_web_access"], true);
    let requests = mock.requests.lock().unwrap();
    assert!(
        requests.last().unwrap().to_string().contains(SEARCH_OUTPUT),
        "the search output reaches the model"
    );
    // Every item was one the gateway allows on this thread.
    for item in &done.items {
        bokkie_gateway::codex::guard::check_item(item, &[Grant::WebSearch].into()).unwrap();
    }
    assert!(supervisor.tainted().is_none());
    live.assert_nothing_left_the_machine();
}

/// On `gpt-5.5`, the provider's response carries a hosted search, which
/// Codex reports as a `webSearch` item, allowed on a `web.search` thread.
#[tokio::test]
async fn a_hosted_search_is_allowed_on_a_web_search_thread() {
    let Some(live) = Live::start(&["Found it."]).await else {
        return;
    };
    let mock = &live.model;
    let data = tempfile::tempdir().unwrap();
    let (supervisor, mut events) = supervisor(&live, data.path(), Some(HOSTED_MODEL)).await;
    let runtime = supervisor.runtime(UserId(1)).await.unwrap();
    let thread = runtime
        .start_thread("Scout".into(), &[Grant::WebSearch].into())
        .await
        .unwrap()
        .thread
        .id;
    mock.search_hosted_next("bokkie hosted query");
    let done = turn(&runtime, &mut events, mock, &thread).await;
    assert_eq!(done.refused, None);
    assert_eq!(done.status, TurnStatus::Completed);
    let found = searches(&done.items);
    eprintln!("hosted items: {found:?}");
    assert_eq!(found.len(), 1, "{:?}", done.items);
    assert_eq!(found[0].0, "bokkie hosted query");
    assert!(supervisor.tainted().is_none());
    live.assert_nothing_left_the_machine();
}

/// A search on a thread without `web.search`, which only a misbehaving
/// provider or app server could produce, is a violation: the gateway
/// refuses the thread and stops Codex, and never repeats the query.
#[tokio::test]
async fn a_search_on_a_thread_without_web_search_stops_codex() {
    let Some(live) = Live::start(&["Found it."]).await else {
        return;
    };
    let mock = &live.model;
    let data = tempfile::tempdir().unwrap();
    let (supervisor, mut events) = supervisor(&live, data.path(), None).await;
    let runtime = supervisor.runtime(UserId(1)).await.unwrap();
    let thread = runtime
        .start_thread("Scribe".into(), &[Grant::Notes].into())
        .await
        .unwrap()
        .thread
        .id;
    mock.search_hosted_next("private matter");
    let done = turn(&runtime, &mut events, mock, &thread).await;
    let reason = done.refused.expect("the thread is refused");
    assert!(reason.contains("web.search"), "{reason}");
    assert!(!reason.contains("private"), "{reason}");
    assert!(supervisor.tainted().is_some());
    live.assert_nothing_left_the_machine();
}
