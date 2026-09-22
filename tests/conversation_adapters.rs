//! Real adapter boundaries around managed tasks and their reviewed conversation.
use std::{
    path::Path,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use bokkie::{
    ConversationAction, ConversationConfirmRequest, ConversationList, ConversationReview,
    ConversationSelectRequest, ConversationTurnRequest, ConversationView, DbExecutor,
    ManagedCapabilityProfile, ManagedCataloguePage, ManagedTaskDefinition, ManagedTaskStatus,
    ManualClock, NewObligation, ObligationState, SessionBootstrap, Store, SystemClock, UnixClock,
    conversation_http::ConversationConfig,
    http::{ApiState, router_with_state},
    http_security::{ApiRuntime, MUTATION_TOKEN_HEADER},
    service::{Scheduler, SchedulerConfig, ServiceFakeOutcome},
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

const AUTHORITY: &str = "127.0.0.1:7744";

fn profiles() -> Vec<ManagedCapabilityProfile> {
    vec![ManagedCapabilityProfile::local_note()]
}
fn scheduler_config(database: &Path) -> SchedulerConfig {
    SchedulerConfig {
        database: database.to_owned(),
        poll_interval: Duration::from_millis(10),
        lease_seconds: 30,
        ordinary_concurrency: 1,
        fake_delay: Duration::ZERO,
        fake_outcome: ServiceFakeOutcome::Succeed,
    }
}
fn create_sentinel(store: &mut Store, id: &str, now: i64) {
    store
        .create(
            NewObligation {
                id: id.into(),
                description: "Scheduler progress sentinel".into(),
                scheduled_at: now,
                recurrence: None,
                approval_required: false,
                retry: Default::default(),
            },
            now,
        )
        .unwrap();
}
fn wait_for(store: &Store, description: &str, mut condition: impl FnMut(&Store) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition(store) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn production_scheduler_requires_note_opt_in_and_restart_preserves_one_result() {
    let temporary = TempDir::new().unwrap();
    let database = temporary.path().join("scheduler.sqlite");
    let mut store = Store::open(&database).unwrap();
    let now = SystemClock.now();
    let definition =
        ManagedTaskDefinition::local_note("Adapter note", "This is the real local result.");
    let receipt = store
        .managed_create("draft-note", &definition, now)
        .unwrap();
    let preview = store
        .managed_preview(&receipt.task_id, "scheduler-session", &profiles(), now)
        .unwrap();
    store
        .managed_activate(
            "activate-note",
            &preview,
            "scheduler-session",
            &profiles(),
            now,
        )
        .unwrap();
    let obligation = store.managed_detail(&receipt.task_id).unwrap().runs[0]
        .obligation_id
        .clone();
    create_sentinel(&mut store, "ordinary-progress", now);
    let disabled = Scheduler::start(scheduler_config(&database)).unwrap();
    wait_for(&store, "ordinary scheduler progress", |s| {
        s.get("ordinary-progress").unwrap().unwrap().state == ObligationState::Completed
    });
    disabled.shutdown().unwrap();
    assert_eq!(
        store.get(&obligation).unwrap().unwrap().state,
        ObligationState::Pending
    );
    assert!(store.attempts(&obligation).unwrap().is_empty());

    let enabled = Scheduler::start_with_notes(scheduler_config(&database), None, true).unwrap();
    wait_for(&store, "local note completion", |s| {
        s.managed_detail(&receipt.task_id).unwrap().status == ManagedTaskStatus::Completed
    });
    enabled.shutdown().unwrap();
    let completed = store.managed_detail(&receipt.task_id).unwrap();
    assert_eq!(completed.runs.len(), 1);
    assert_eq!(completed.runs[0].state, "completed");
    assert_eq!(completed.runs[0].definition_revision, 1);
    assert_eq!(completed.runs[0].profile_revision, "local-note-v1");
    assert!(completed.runs[0].admitted_at.is_some());
    assert_eq!(
        completed.runs[0].result.as_deref(),
        Some("This is the real local result.")
    );
    assert_eq!(store.attempts(&obligation).unwrap().len(), 1);
    drop(store);

    let mut store = Store::open(&database).unwrap();
    create_sentinel(&mut store, "restart-progress", SystemClock.now());
    let restarted = Scheduler::start_with_notes(scheduler_config(&database), None, true).unwrap();
    wait_for(&store, "restarted scheduler progress", |s| {
        s.get("restart-progress").unwrap().unwrap().state == ObligationState::Completed
    });
    restarted.shutdown().unwrap();
    assert_eq!(store.managed_detail(&receipt.task_id).unwrap(), completed);
    assert_eq!(store.attempts(&obligation).unwrap().len(), 1);
}

fn runtime() -> ApiRuntime {
    ApiRuntime::new(AUTHORITY.parse().unwrap(), bokkie::SUPPORTED_SCHEMA_VERSION).unwrap()
}
fn application(executor: &DbExecutor, runtime: ApiRuntime, notes: bool) -> Router {
    router_with_state(
        ApiState {
            executor: executor.clone(),
            runtime,
            engineering_intake: None,
            conversation: Some(ConversationConfig {
                profile: None,
                notes_enabled: notes,
                clock: Some(Arc::new(ManualClock::new(100))),
            }),
        },
        None,
    )
}
async fn request(
    app: &Router,
    method: Method,
    path: &str,
    token: Option<&str>,
    payload: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", AUTHORITY)
        .header("origin", format!("http://{AUTHORITY}"));
    if let Some(token) = token {
        request = request.header(MUTATION_TOKEN_HEADER, token);
    }
    let body = if let Some(payload) = payload {
        request = request.header("content-type", "application/json");
        Body::from(serde_json::to_vec(&payload).unwrap())
    } else {
        Body::empty()
    };
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"body":String::from_utf8_lossy(&bytes)})),
    )
}
fn decoded<T: DeserializeOwned>((status, value): (StatusCode, Value)) -> T {
    assert_eq!(status, StatusCode::OK, "{value}");
    serde_json::from_value(value).unwrap()
}
async fn bootstrap(app: &Router) -> SessionBootstrap {
    decoded(request(app, Method::GET, "/bootstrap", None, None).await)
}
fn review(store: &mut Store, conversation: &str, task: &str, session: &str, id: &str) {
    let preview = store
        .managed_preview(task, session, &profiles(), 100)
        .unwrap();
    store
        .conversation_review(
            conversation,
            &ConversationReview {
                id: id.into(),
                action: ConversationAction::Activate,
                task_id: task.into(),
                configuration_revision: preview.configuration_revision,
                session_id: session.into(),
                explanation: "Activate the exact saved local note".into(),
                blockers: preview.blockers.clone(),
                preview: Some(preview),
            },
            &profiles(),
            100,
        )
        .unwrap();
}

#[tokio::test]
async fn conversation_http_select_review_and_confirm_obey_bootstrap_and_restart_fences() {
    let temporary = TempDir::new().unwrap();
    let database = temporary.path().join("http.sqlite");
    let mut store = Store::open(&database).unwrap();
    let draft = store
        .managed_create(
            "draft",
            &ManagedTaskDefinition::local_note("HTTP note", "saved result"),
            100,
        )
        .unwrap();
    let executor = DbExecutor::start(database).unwrap();
    let app = application(&executor, runtime(), true);
    let first = bootstrap(&app).await;
    let select = ConversationSelectRequest {
        command_id: "select".into(),
        conversation_id: "conversation".into(),
        expected_revision: 0,
        task_id: draft.task_id.clone(),
    };
    let payload = serde_json::to_value(&select).unwrap();
    let denied = request(
        &app,
        Method::POST,
        "/conversations/select",
        None,
        Some(payload.clone()),
    )
    .await;
    assert_eq!(denied.0, StatusCode::FORBIDDEN);
    assert!(store.conversation_list().unwrap().is_empty());
    let selected: ConversationView = decoded(
        request(
            &app,
            Method::POST,
            "/conversations/select",
            Some(&first.mutation_token),
            Some(payload.clone()),
        )
        .await,
    );
    assert_eq!(
        selected.selected_task_id.as_deref(),
        Some(draft.task_id.as_str())
    );
    assert_eq!(selected.task.unwrap().status, ManagedTaskStatus::Draft);
    let replay: ConversationView = decoded(
        request(
            &app,
            Method::POST,
            "/conversations/select",
            Some(&first.mutation_token),
            Some(payload),
        )
        .await,
    );
    assert_eq!(replay.revision, selected.revision);
    let list: ConversationList =
        decoded(request(&app, Method::GET, "/conversations", None, None).await);
    assert_eq!(list.items.len(), 1);
    assert_eq!(list.items[0].id, "conversation");
    let view: ConversationView =
        decoded(request(&app, Method::GET, "/conversations/conversation", None, None).await);
    assert_eq!(view.service, first.service);
    assert_eq!(view.revision, selected.revision);

    review(
        &mut store,
        "conversation",
        &draft.task_id,
        &first.service.session_id,
        "old-review",
    );
    let old_confirm = ConversationConfirmRequest {
        command_id: "old-confirm".into(),
        conversation_id: "conversation".into(),
        proposal_id: "old-review".into(),
        session_id: first.service.session_id.clone(),
    };
    let restarted = application(&executor, runtime(), true);
    let second = bootstrap(&restarted).await;
    assert_ne!(first.service.session_id, second.service.session_id);
    let rotated = request(
        &restarted,
        Method::POST,
        "/conversations/confirm",
        Some(&first.mutation_token),
        Some(serde_json::to_value(&old_confirm).unwrap()),
    )
    .await;
    assert_eq!(rotated.0, StatusCode::FORBIDDEN);
    let old_session = request(
        &restarted,
        Method::POST,
        "/conversations/confirm",
        Some(&second.mutation_token),
        Some(serde_json::to_value(&old_confirm).unwrap()),
    )
    .await;
    assert_eq!(old_session.0, StatusCode::CONFLICT);
    let forged_session = ConversationConfirmRequest {
        session_id: second.service.session_id.clone(),
        ..old_confirm
    };
    let stale_card = request(
        &restarted,
        Method::POST,
        "/conversations/confirm",
        Some(&second.mutation_token),
        Some(serde_json::to_value(&forged_session).unwrap()),
    )
    .await;
    assert_eq!(stale_card.0, StatusCode::CONFLICT);
    assert_eq!(
        store.managed_detail(&draft.task_id).unwrap().status,
        ManagedTaskStatus::Draft
    );

    review(
        &mut store,
        "conversation",
        &draft.task_id,
        &second.service.session_id,
        "current-review",
    );
    let confirm = ConversationConfirmRequest {
        command_id: "confirm".into(),
        conversation_id: "conversation".into(),
        proposal_id: "current-review".into(),
        session_id: second.service.session_id.clone(),
    };
    let payload = serde_json::to_value(&confirm).unwrap();
    let confirmed: ConversationView = decoded(
        request(
            &restarted,
            Method::POST,
            "/conversations/confirm",
            Some(&second.mutation_token),
            Some(payload.clone()),
        )
        .await,
    );
    assert_eq!(
        confirmed.task.as_ref().unwrap().status,
        ManagedTaskStatus::Active
    );
    assert!(confirmed.receipt.is_some());
    assert_eq!(confirmed.task.as_ref().unwrap().runs.len(), 1);
    let watermark = store.change_page(0, None, 1000).unwrap().through;
    let replay: ConversationView = decoded(
        request(
            &restarted,
            Method::POST,
            "/conversations/confirm",
            Some(&second.mutation_token),
            Some(payload),
        )
        .await,
    );
    assert_eq!(replay.revision, confirmed.revision);
    assert_eq!(replay.receipt, confirmed.receipt);
    assert_eq!(store.change_page(0, None, 1000).unwrap().through, watermark);
    executor.shutdown().unwrap();
}

#[tokio::test]
async fn unavailable_runtime_does_not_dispatch_and_http_catalogue_searches_beyond_loaded_page() {
    let temporary = TempDir::new().unwrap();
    let database = temporary.path().join("catalogue.sqlite");
    let mut store = Store::open(&database).unwrap();
    let mut ids = Vec::new();
    for index in 0..110 {
        ids.push(
            store
                .managed_create(
                    &format!("draft-{index}"),
                    &ManagedTaskDefinition::local_note(format!("Task {index}"), "note"),
                    100,
                )
                .unwrap()
                .task_id,
        );
    }
    let executor = DbExecutor::start(database).unwrap();
    let app = application(&executor, runtime(), false);
    let bootstrap = bootstrap(&app).await;
    let first: ManagedCataloguePage =
        decoded(request(&app, Method::GET, "/tasks/catalogue?limit=50", None, None).await);
    assert_eq!(first.items.len(), 50);
    assert!(first.next_after.is_some());
    let last = ids.iter().max().unwrap();
    assert!(!first.items.iter().any(|entry| &entry.id == last));
    let found: ManagedCataloguePage = decoded(
        request(
            &app,
            Method::GET,
            &format!("/tasks/catalogue?q={last}"),
            None,
            None,
        )
        .await,
    );
    assert_eq!(found.items.len(), 1);
    assert_eq!(&found.items[0].id, last);
    let second: ManagedCataloguePage = decoded(
        request(
            &app,
            Method::GET,
            &format!(
                "/tasks/catalogue?limit=50&after={}",
                first.next_after.unwrap()
            ),
            None,
            None,
        )
        .await,
    );
    assert_eq!(second.items.len(), 50);
    assert!(
        !first
            .items
            .iter()
            .any(|entry| second.items.iter().any(|other| entry.id == other.id))
    );
    let before = store.change_page(0, None, 1000).unwrap().through;
    let turn = ConversationTurnRequest {
        command_id: "unavailable-turn".into(),
        conversation_id: "not-dispatched".into(),
        expected_revision: 0,
        text: "Make a note".into(),
    };
    let error = request(
        &app,
        Method::POST,
        "/conversations/turn",
        Some(&bootstrap.mutation_token),
        Some(serde_json::to_value(turn).unwrap()),
    )
    .await;
    assert_eq!(error.0, StatusCode::BAD_REQUEST);
    assert!(
        error.1["error"]["message"]
            .as_str()
            .unwrap()
            .contains("runtime unavailable")
    );
    assert!(store.conversation_list().unwrap().is_empty());
    assert_eq!(store.change_page(0, None, 1000).unwrap().through, before);
    let view: ConversationView = decoded(
        request(
            &app,
            Method::GET,
            "/conversations/not-dispatched",
            None,
            None,
        )
        .await,
    );
    assert!(!view.busy);
    assert!(!view.runtime_available);
    assert!(!view.notes_available);
    assert!(view.messages.is_empty());
    assert_eq!(view.revision, 0);
    // The body cannot manufacture an enabled runtime or wider effect profile.
    let unknown=request(&app,Method::POST,"/conversations/turn",Some(&bootstrap.mutation_token),Some(json!({"command_id":"forged","conversation_id":"not-dispatched","expected_revision":0,"text":"note","notes_enabled":true}))).await;
    assert_eq!(unknown.0, StatusCode::UNPROCESSABLE_ENTITY);
    executor.shutdown().unwrap();
}
