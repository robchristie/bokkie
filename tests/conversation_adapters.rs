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
                push: None,
                notifications: None,
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
        consult_adviser: false,
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

// Serialise only tests that exercise the two-slot production model admission.
// Their subprocess peers are deterministic; clocks never advance while polling.
static MODEL_PEER_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct ModelApplication {
    _temporary: TempDir,
    store: Store,
    executor: DbExecutor,
    app: Router,
    token: String,
    record: std::path::PathBuf,
    clock: Arc<ManualClock>,
}
impl ModelApplication {
    async fn new(scenario: &str) -> Self {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("model-http.sqlite");
        let store = Store::open(&database).unwrap();
        let executor = DbExecutor::start(database).unwrap();
        let profile = bokkie::conversation_runtime::ConversationProfile {
            broker: Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/conversation_broker.py"),
            codex: "/usr/bin/true".into(),
            bwrap: "/usr/bin/true".into(),
            model: scenario.into(),
            effort: "medium".into(),
            timezone: "Australia/Adelaide".into(),
            timeout_seconds: 5,
            max_context_bytes: 65536,
            max_output_bytes: 16384,
        };
        profile.validate().unwrap();
        let clock = Arc::new(ManualClock::new(100));
        let app = router_with_state(
            ApiState {
                executor: executor.clone(),
                runtime: runtime(),
                engineering_intake: None,
                conversation: Some(ConversationConfig {
                    profile: Some(Arc::new(profile)),
                    notes_enabled: true,
                    push: None,
                    notifications: None,
                    clock: Some(clock.clone()),
                }),
            },
            None,
        );
        let token = bootstrap(&app).await.mutation_token;
        let record = temporary.path().join("broker-calls.jsonl");
        Self {
            _temporary: temporary,
            store,
            executor,
            app,
            token,
            record,
            clock,
        }
    }

    fn turn_request(&self, revision: i64) -> ConversationTurnRequest {
        ConversationTurnRequest {
            command_id: "model-turn".into(),
            conversation_id: "model-conversation".into(),
            expected_revision: revision,
            consult_adviser: false,
            text: json!({"record":self.record,"intent":"Find or prepare the requested reminder"})
                .to_string(),
        }
    }

    async fn post_turn(&self, turn: &ConversationTurnRequest) -> ConversationView {
        decoded(
            request(
                &self.app,
                Method::POST,
                "/conversations/turn",
                Some(&self.token),
                Some(serde_json::to_value(turn).unwrap()),
            )
            .await,
        )
    }

    async fn finished(&self) -> ConversationView {
        self.finished_within(Duration::from_secs(5)).await
    }

    async fn finished_within(&self, timeout: Duration) -> ConversationView {
        let deadline = Instant::now() + timeout;
        loop {
            let view: ConversationView = decoded(
                request(
                    &self.app,
                    Method::GET,
                    "/conversations/model-conversation",
                    None,
                    None,
                )
                .await,
            );
            if !view.busy {
                return view;
            }
            assert!(Instant::now() < deadline, "model peer did not finish");
            tokio::task::yield_now().await;
        }
    }

    fn calls(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.record)
            .unwrap_or_default()
            .split_inclusive('\n')
            .filter(|line| line.ends_with('\n'))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    async fn configure_adviser(
        &self,
        automatic: bool,
        calls: u8,
        model: &str,
    ) -> bokkie::AgentProfileRevision {
        let initial: bokkie::AgentSettingsView =
            decoded(request(&self.app, Method::GET, "/agent-settings", None, None).await);
        let initial = initial.profile.unwrap();
        let mut main = initial.main.clone();
        main.max_model_calls = calls;
        let mut adviser = initial.main;
        adviser.model = model.into();
        adviser.effort = "high".into();
        adviser.additional_instructions = "Explain the trade-off briefly".into();
        adviser.max_model_calls = 1;
        let save = bokkie::AgentSettingsSaveRequest {
            command_id: format!("configure-adviser-{}", initial.revision),
            expected_revision: initial.revision,
            main,
            adviser: Some(bokkie::AdviserRoleSettings {
                role: adviser,
                automatic_consultation: automatic,
            }),
        };
        let saved: bokkie::AgentSettingsView = decoded(
            request(
                &self.app,
                Method::POST,
                "/agent-settings",
                Some(&self.token),
                Some(json!(save)),
            )
            .await,
        );
        assert!(saved.effective);
        assert_eq!(saved.ceilings.unwrap().max_model_calls, 4);
        assert!(
            self.calls().is_empty(),
            "Settings reads and saves must not invoke models"
        );
        saved.profile.unwrap()
    }

    fn invocations(&self) -> Vec<Value> {
        let connection = rusqlite::Connection::open_with_flags(
            self._temporary.path().join("model-http.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut query = connection.prepare("SELECT ordinal,purpose,profile_revision,status,outcome_json FROM conversation_invocations ORDER BY ordinal").unwrap();
        query.query_map([], |r| {
            let outcome = r.get::<_,Option<String>>(4)?.map(|raw| serde_json::from_str::<Value>(&raw).unwrap());
            Ok(json!({"ordinal":r.get::<_,u8>(0)?,"purpose":r.get::<_,String>(1)?,"revision":r.get::<_,i64>(2)?,"status":r.get::<_,String>(3)?,"outcome":outcome}))
        }).unwrap().collect::<Result<Vec<_>,_>>().unwrap()
    }

    async fn replay_is_free(&self, turn: &ConversationTurnRequest, previous: &ConversationView) {
        let calls = self.calls().len();
        let dispatches = self.store.conversation_model_dispatch_count().unwrap();
        let watermark = self.store.change_page(0, None, 1000).unwrap().through;
        let replay = self.post_turn(turn).await;
        assert_eq!(replay, *previous);
        assert_eq!(self.calls().len(), calls);
        assert_eq!(
            self.store.conversation_model_dispatch_count().unwrap(),
            dispatches
        );
        assert_eq!(
            self.store.change_page(0, None, 1000).unwrap().through,
            watermark
        );
    }
}
impl Drop for ModelApplication {
    fn drop(&mut self) {
        self.executor.clone().shutdown().unwrap();
    }
}

#[tokio::test]
async fn model_http_empty_lookup_continues_once_saves_only_a_draft_and_replay_is_free() {
    let _serial = MODEL_PEER_TESTS.lock().await;
    let fixture = ModelApplication::new("fixture-empty").await;
    let turn = fixture.turn_request(0);
    fixture.post_turn(&turn).await;
    let view = fixture.finished().await;
    assert_eq!(view.request_error, None, "{:?}", view.messages);
    let task = view
        .task
        .as_ref()
        .expect("continuation saved the requested draft");
    assert_eq!(task.status, ManagedTaskStatus::Draft);
    assert!(task.active.is_none());
    assert!(task.runs.is_empty());
    assert_eq!(
        task.candidate.as_ref().unwrap().definition.instructions,
        "Keep this supplied reminder text."
    );
    assert_eq!(view.selected_task_id.as_ref(), Some(&task.id));
    assert!(view.review.is_some());
    assert_eq!(
        fixture
            .store
            .managed_catalogue("", None, 100)
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        fixture.store.conversation_model_dispatch_count().unwrap(),
        2
    );
    let calls = fixture.calls();
    assert_eq!(calls.len(), 2);
    assert!(calls[0]["context"].get("lookup_result").is_none());
    assert_eq!(calls[1]["context"]["lookup_result"]["successful"], true);
    assert_eq!(calls[1]["context"]["lookup_result"]["items"], json!([]));
    assert_eq!(
        calls[1]["context"]["messages"],
        calls[0]["context"]["messages"]
    );
    assert!(
        calls[0]["instructions"]
            .as_str()
            .unwrap()
            .contains("You are Bokkie")
    );
    assert!(calls[0]["context"].get("instruction").is_none());
    assert!(calls[1]["context"].get("instruction").is_none());
    assert!(
        calls[1]["context"]["lookup_result"]
            .get("instruction")
            .is_none()
    );
    assert!(
        calls[1]["instructions"].as_str().unwrap().contains(
            "Continue the original user request: save a draft if they asked to create one"
        )
    );
    assert!(
        calls[1]["instructions"]
            .as_str()
            .unwrap()
            .starts_with(calls[0]["instructions"].as_str().unwrap())
    );
    fixture.replay_is_free(&turn, &view).await;
}

#[tokio::test]
async fn model_http_nonempty_lookup_stops_at_candidates_until_operator_selects() {
    let _serial = MODEL_PEER_TESTS.lock().await;
    let mut fixture = ModelApplication::new("fixture-matches").await;
    let mut before = Vec::new();
    for index in 0..2 {
        let receipt = fixture
            .store
            .managed_create(
                &format!("seed-{index}"),
                &ManagedTaskDefinition::local_note(
                    format!("Adapter needle {index}"),
                    "Original reminder",
                ),
                100,
            )
            .unwrap();
        before.push(fixture.store.managed_detail(&receipt.task_id).unwrap());
    }
    let turn = fixture.turn_request(0);
    fixture.post_turn(&turn).await;
    let view = fixture.finished().await;
    assert!(view.request_error.is_none());
    assert_eq!(view.candidates.len(), 2);
    assert!(view.selected_task_id.is_none());
    assert!(view.task.is_none());
    assert!(view.review.is_none());
    assert_eq!(fixture.calls().len(), 1);
    assert_eq!(
        fixture.store.conversation_model_dispatch_count().unwrap(),
        1
    );
    for original in &before {
        assert_eq!(
            fixture.store.managed_detail(&original.id).unwrap(),
            *original
        );
    }
    fixture.replay_is_free(&turn, &view).await;
    let selection = ConversationSelectRequest {
        command_id: "choose-match".into(),
        conversation_id: turn.conversation_id.clone(),
        expected_revision: view.revision,
        task_id: before[1].id.clone(),
    };
    let selected: ConversationView = decoded(
        request(
            &fixture.app,
            Method::POST,
            "/conversations/select",
            Some(&fixture.token),
            Some(serde_json::to_value(selection).unwrap()),
        )
        .await,
    );
    assert_eq!(selected.task.as_ref(), Some(&before[1]));
    assert_eq!(fixture.calls().len(), 1);
    assert_eq!(
        fixture
            .store
            .managed_catalogue("", None, 100)
            .unwrap()
            .items
            .len(),
        2
    );
}

#[tokio::test]
async fn model_http_failed_or_malformed_peer_preserves_selected_draft_and_replay() {
    let _serial = MODEL_PEER_TESTS.lock().await;
    for scenario in [
        "fixture-fail",
        "fixture-malformed",
        "fixture-invalid-json",
        "fixture-read-fail",
    ] {
        let mut fixture = ModelApplication::new(scenario).await;
        let receipt = fixture
            .store
            .managed_create(
                "seed",
                &ManagedTaskDefinition::local_note("Existing draft", "Preserve this exact text"),
                100,
            )
            .unwrap();
        let original = fixture.store.managed_detail(&receipt.task_id).unwrap();
        let selected: ConversationView = decoded(
            request(
                &fixture.app,
                Method::POST,
                "/conversations/select",
                Some(&fixture.token),
                Some(
                    json!({"command_id":"select-existing","conversation_id":"model-conversation",
                        "expected_revision":0,"task_id":receipt.task_id}),
                ),
            )
            .await,
        );
        let turn = fixture.turn_request(selected.revision);
        fixture.post_turn(&turn).await;
        let view = fixture.finished().await;
        assert!(view.request_error.is_some(), "{scenario}");
        assert_eq!(view.task.as_ref(), Some(&original), "{scenario}");
        assert_eq!(
            fixture.store.managed_detail(&original.id).unwrap(),
            original
        );
        assert_eq!(
            fixture
                .store
                .managed_catalogue("", None, 100)
                .unwrap()
                .items
                .len(),
            1
        );
        assert_eq!(fixture.calls().len(), 1, "{scenario}");
        assert_eq!(
            fixture.store.conversation_model_dispatch_count().unwrap(),
            1
        );
        fixture.replay_is_free(&turn, &view).await;
    }
}

#[tokio::test]
async fn model_http_repeated_lookup_exhausts_two_calls_without_creating_a_task() {
    let _serial = MODEL_PEER_TESTS.lock().await;
    let fixture = ModelApplication::new("fixture-repeat").await;
    let turn = fixture.turn_request(0);
    fixture.post_turn(&turn).await;
    let view = fixture.finished().await;
    assert!(
        view.request_error
            .as_deref()
            .unwrap()
            .contains("unavailable tool"),
        "{:?}",
        view.request_error
    );
    assert!(view.task.is_none());
    assert!(view.review.is_none());
    assert!(
        fixture
            .store
            .managed_catalogue("", None, 100)
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(fixture.calls().len(), 2);
    assert_eq!(
        fixture.store.conversation_model_dispatch_count().unwrap(),
        2
    );
    fixture.replay_is_free(&turn, &view).await;
}

#[tokio::test]
async fn managed_attention_retry_uses_existing_fenced_operator_http_and_preserves_occurrence() {
    let temporary = TempDir::new().unwrap();
    let database = temporary.path().join("retry.sqlite");
    let mut store = Store::open(&database).unwrap();
    let mut definition = ManagedTaskDefinition::local_note("Recover note", "Original result");
    definition.max_attempts = 1;
    let task = store.managed_create("draft", &definition, 0).unwrap();
    let preview = store
        .managed_preview(&task.task_id, "session", &profiles(), 0)
        .unwrap();
    store
        .managed_activate("activate", &preview, "session", &profiles(), 0)
        .unwrap();
    let claim = store.claim_due_notes(0, 1, 1).unwrap().pop().unwrap();
    assert!(store.claim_due_notes(1, 1, 1).unwrap().is_empty());
    let snapshot = store.operator_snapshot(1).unwrap();
    let item = snapshot
        .obligations
        .iter()
        .find(|o| o.id == claim.obligation_id)
        .unwrap();
    let payload = json!({"actor":"operator", "note":null, "precondition":item.capabilities.retry.precondition.clone().unwrap()});
    let executor = DbExecutor::start(database).unwrap();
    let app = application(&executor, runtime(), true);
    let token = bootstrap(&app).await.mutation_token;
    let route = format!("/operator/obligations/{}/retry", claim.obligation_id);
    assert_eq!(
        request(&app, Method::POST, &route, None, Some(payload.clone()))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &app,
            Method::POST,
            &format!("/obligations/{}/retry", claim.obligation_id),
            Some(&token),
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &app,
            Method::POST,
            &route,
            Some(&token),
            Some(payload.clone())
        )
        .await
        .0,
        StatusCode::OK
    );
    // A lost response cannot authorise a second retry of a different state.
    assert_eq!(
        request(&app, Method::POST, &route, Some(&token), Some(payload))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let due = store
        .get(&claim.obligation_id)
        .unwrap()
        .unwrap()
        .next_wake_at
        .unwrap();
    let recovered = store.claim_due_notes(due, 30, 1).unwrap().pop().unwrap();
    assert_eq!(recovered.obligation_id, claim.obligation_id);
    store
        .complete_managed_note(&recovered, "Original result", due)
        .unwrap();
    assert_eq!(
        store
            .managed_detail(&task.task_id)
            .unwrap()
            .runs
            .iter()
            .filter(|r| r.result.is_some())
            .count(),
        1
    );
}

#[tokio::test]
async fn settings_http_are_validated_atomic_and_consumed_by_new_requests() {
    use bokkie::{AgentSettingsSaveRequest, AgentSettingsView};
    let _serial = MODEL_PEER_TESTS.lock().await;
    let mut f = ModelApplication::new("fixture-empty").await;
    let response = request(&f.app, Method::GET, "/agent-settings", None, None).await;
    let initial: AgentSettingsView = decoded(response);
    assert!(initial.effective);
    let original = initial.profile.unwrap();
    assert_eq!(original.main.model, "fixture-empty");
    assert!(!f.record.exists());
    assert_eq!(f.store.conversation_model_dispatch_count().unwrap(), 0);
    let mut role = original.main;
    role.model = "fixture-settings".into();
    role.effort = "high".into();
    role.additional_instructions = "Use concise Australian English.".into();
    role.timeout_seconds = 3;
    role.max_context_bytes = 16384;
    role.max_output_bytes = 4096;
    role.max_model_calls = 1;
    let save = AgentSettingsSaveRequest {
        command_id: "settings-save".into(),
        expected_revision: 1,
        adviser: None,
        main: role.clone(),
    };
    let mut bad = save.clone();
    bad.command_id = "bad-pair".into();
    bad.main.effort = "unsupported".into();
    let rejected = request(
        &f.app,
        Method::POST,
        "/agent-settings",
        Some(&f.token),
        Some(json!(bad)),
    )
    .await;
    assert_eq!(rejected.0, StatusCode::BAD_REQUEST);
    assert!(f.store.agent_settings(None, 100).unwrap().unwrap().revision == 1);
    let response = request(
        &f.app,
        Method::POST,
        "/agent-settings",
        Some(&f.token),
        Some(json!(save)),
    )
    .await;
    let saved: AgentSettingsView = decoded(response);
    assert_eq!(saved.profile.unwrap().main, role);
    assert!(!f.record.exists());
    let turn = f.turn_request(0);
    f.post_turn(&turn).await;
    let view = f.finished().await;
    assert!(!view.busy);
    assert!(view.request_error.is_none());
    let calls: Vec<Value> = std::fs::read_to_string(&f.record)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["profile"]["model"], "fixture-settings");
    assert_eq!(calls[0]["profile"]["effort"], "high");
    assert_eq!(
        calls[0]["context"]["additional_instructions"],
        role.additional_instructions
    );
    assert_eq!(calls[0]["profile"]["timeout_seconds"], 3);
    assert_eq!(calls[0]["profile"]["max_context_bytes"], 16384);
    assert_eq!(calls[0]["profile"]["max_output_bytes"], 4096);
    f.replay_is_free(&turn, &view).await;
}

#[tokio::test]
async fn settings_edit_during_execution_pins_every_call_and_replay_bypasses_unavailable_runtime() {
    use bokkie::{AgentSettingsSaveRequest, AgentSettingsView};
    let _serial = MODEL_PEER_TESTS.lock().await;
    let f = ModelApplication::new("fixture-empty").await;
    let initial: AgentSettingsView =
        decoded(request(&f.app, Method::GET, "/agent-settings", None, None).await);
    let release = f._temporary.path().join("release");
    let mut turn = f.turn_request(0);
    turn.text = json!({"record":f.record,"release":release,"intent":"Find or prepare a reminder"})
        .to_string();
    f.post_turn(&turn).await;
    let deadline = Instant::now() + Duration::from_secs(3);
    while f.calls().is_empty() {
        assert!(Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
    let mut main = initial.profile.unwrap().main;
    main.model = "fixture-settings".into();
    main.effort = "high".into();
    main.additional_instructions = "Changed while running".into();
    main.max_model_calls = 1;
    let save = AgentSettingsSaveRequest {
        command_id: "during-execution".into(),
        expected_revision: 1,
        adviser: None,
        main,
    };
    let _: AgentSettingsView = decoded(
        request(
            &f.app,
            Method::POST,
            "/agent-settings",
            Some(&f.token),
            Some(json!(save)),
        )
        .await,
    );
    std::fs::write(release, "release").unwrap();
    let completed = f.finished().await;
    assert!(
        completed.request_error.is_none(),
        "{:?}",
        completed.request_error
    );
    let calls = f.calls();
    assert_eq!(calls.len(), 2);
    for call in calls {
        assert_eq!(call["profile"]["model"], "fixture-empty");
        assert_eq!(call["profile"]["effort"], "medium");
        assert_eq!(call["context"]["additional_instructions"], "");
    }
    let disabled = application(&f.executor, runtime(), true);
    let replay: ConversationView = decoded(
        request(
            &disabled,
            Method::POST,
            "/conversations/turn",
            Some(&bootstrap(&disabled).await.mutation_token),
            Some(json!(turn)),
        )
        .await,
    );
    assert_eq!(replay.messages, completed.messages);
    assert_eq!(f.calls().len(), 2);
}

#[tokio::test]
async fn aggregate_deadline_between_calls_leaves_no_unfinished_dispatch_on_failure_or_restart() {
    let _serial = MODEL_PEER_TESTS.lock().await;
    let mut f = ModelApplication::new("fixture-empty").await;
    let release = f._temporary.path().join("release-deadline");
    let mut turn = f.turn_request(0);
    turn.text = json!({"record":f.record,"release":release,"intent":"Find a reminder"}).to_string();
    f.post_turn(&turn).await;
    let wait_limit = Instant::now() + Duration::from_secs(3);
    while f.calls().is_empty() {
        assert!(Instant::now() < wait_limit);
        tokio::task::yield_now().await;
    }
    f.clock.set(111);
    std::fs::write(release, "release").unwrap();
    let completed = f.finished().await;
    assert!(
        completed
            .request_error
            .as_deref()
            .unwrap()
            .contains("time limit")
    );
    assert_eq!(f.calls().len(), 1);
    assert_eq!(f.store.conversation_model_dispatch_count().unwrap(), 1);
    let connection = rusqlite::Connection::open_with_flags(
        f._temporary.path().join("model-http.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let unfinished = || {
        connection
            .query_row(
                "SELECT COUNT(*) FROM conversation_invocations WHERE status='dispatched'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
    };
    assert_eq!(unfinished(), 0);
    f.store.conversation_interrupt("restart", 112).unwrap();
    assert_eq!(unfinished(), 0);
    f.replay_is_free(&turn, &completed).await;
}

#[tokio::test]
async fn adviser_explicit_consultation_is_schema_only_then_bokkie_returns_and_replay_is_free() {
    let _serial = MODEL_PEER_TESTS.lock().await;
    let f = ModelApplication::new("fixture-adviser-main").await;
    let saved = f.configure_adviser(false, 2, "fixture-adviser").await;
    let mut turn = f.turn_request(0);
    turn.consult_adviser = true;
    turn.text = json!({"record":f.record,"adviser_route":"manual"}).to_string();
    let accepted = f.post_turn(&turn).await;
    assert!(accepted.adviser_available);
    let completed = f.finished().await;
    assert!(
        completed.request_error.is_none(),
        "{:?}",
        completed.request_error
    );
    assert!(
        completed
            .messages
            .last()
            .unwrap()
            .text
            .contains("Bokkie considered Astra")
    );
    assert!(completed.activity.is_none());
    let advice = completed.adviser_outcome.as_ref().unwrap();
    assert_eq!(advice.profile_revision, saved.revision);
    assert_eq!(advice.status, "completed");
    assert!(
        advice
            .advice
            .as_deref()
            .unwrap()
            .contains("requirements conflict")
    );
    let calls = f.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["profile"]["model"], "fixture-adviser");
    assert_eq!(calls[0]["profile"]["effort"], "high");
    assert!(calls[0].get("tools").is_none());
    assert!(calls[0]["context"].get("messages").is_none());
    assert!(calls[0]["context"].get("available_capabilities").is_none());
    assert_eq!(calls[1]["profile"]["model"], "fixture-adviser-main");
    assert_eq!(calls[1]["context"]["adviser_result"]["status"], "completed");
    assert!(
        calls[1]["tools"][0]["inputSchema"]["properties"]
            .get("difficulty")
            .is_none()
    );
    let ledger = f.invocations();
    assert_eq!(
        ledger
            .iter()
            .map(|r| r["purpose"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["adviser_manual", "after_advice"]
    );
    assert!(
        ledger
            .iter()
            .all(|r| r["revision"] == saved.revision && r["status"] == "completed")
    );
    f.replay_is_free(&turn, &completed).await;
}

#[tokio::test]
async fn adviser_four_call_orderings_keep_both_profiles_pinned_during_settings_edit() {
    let _serial = MODEL_PEER_TESTS.lock().await;
    for route in ["lookup_first", "advice_first"] {
        let f = ModelApplication::new("fixture-adviser-main").await;
        let saved = f.configure_adviser(true, 4, "fixture-adviser").await;
        let release = f._temporary.path().join("release-main");
        let mut turn = f.turn_request(0);
        turn.text = json!({"record":f.record,"release":release,"adviser_route":route,"requirements":["Only at 9 am","Only at 10 am"]}).to_string();
        f.post_turn(&turn).await;
        let deadline = Instant::now() + Duration::from_secs(3);
        while f.calls().is_empty() {
            assert!(Instant::now() < deadline);
            tokio::task::yield_now().await;
        }
        let mut revised = saved.main.clone();
        revised.model = "fixture-settings".into();
        revised.effort = "high".into();
        revised.additional_instructions =
            "This revision must not reach the accepted request".into();
        revised.max_model_calls = 1;
        let change = bokkie::AgentSettingsSaveRequest {
            command_id: "edit-during-held-turn".into(),
            expected_revision: saved.revision,
            main: revised,
            adviser: None,
        };
        let _: bokkie::AgentSettingsView = decoded(
            request(
                &f.app,
                Method::POST,
                "/agent-settings",
                Some(&f.token),
                Some(json!(change)),
            )
            .await,
        );
        std::fs::write(release, "release").unwrap();
        let completed = f.finished().await;
        assert!(
            completed.request_error.is_none(),
            "{route}: {:?}",
            completed.request_error
        );
        let calls = f.calls();
        assert_eq!(calls.len(), 4, "{route}");
        for call in &calls {
            let advisory = call.get("output_schema").is_some();
            assert_eq!(
                call["profile"]["model"],
                if advisory {
                    "fixture-adviser"
                } else {
                    "fixture-adviser-main"
                }
            );
            assert_eq!(
                call["profile"]["effort"],
                if advisory { "high" } else { "medium" }
            );
            assert_eq!(
                call["context"]["additional_instructions"],
                if advisory {
                    "Explain the trade-off briefly"
                } else {
                    ""
                }
            );
        }
        let ledger = f.invocations();
        let expected = if route == "lookup_first" {
            vec![
                "main",
                "empty_lookup_continuation",
                "adviser_conflicting_requirements",
                "after_advice",
            ]
        } else {
            vec![
                "main",
                "adviser_conflicting_requirements",
                "after_advice",
                "empty_lookup_continuation",
            ]
        };
        assert_eq!(
            ledger
                .iter()
                .map(|r| r["purpose"].as_str().unwrap())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(
            ledger
                .iter()
                .all(|r| r["revision"] == saved.revision && r["status"] == "completed")
        );
        f.replay_is_free(&turn, &completed).await;
    }
}

#[tokio::test]
async fn adviser_automatic_consultation_requires_enabled_grounded_supported_difficulty() {
    let _serial = MODEL_PEER_TESTS.lock().await;
    for (enabled, route) in [
        (false, "advice_first"),
        (true, "invalid_quotes"),
        (true, "unsupported_condition"),
    ] {
        let f = ModelApplication::new("fixture-adviser-main").await;
        f.configure_adviser(enabled, 4, "fixture-adviser").await;
        let mut turn = f.turn_request(0);
        turn.text = json!({"record":f.record,"adviser_route":route,"requirements":["Only at 9 am","Only at 10 am"]}).to_string();
        f.post_turn(&turn).await;
        let completed = f.finished().await;
        assert!(completed.request_error.is_some(), "{enabled}/{route}");
        assert!(completed.adviser_outcome.is_none());
        assert_eq!(f.calls().len(), 1);
        assert_eq!(f.invocations().len(), 1);
        assert!(completed.task.is_none());
        f.replay_is_free(&turn, &completed).await;
    }
    let f = ModelApplication::new("fixture-adviser-main").await;
    f.configure_adviser(true, 4, "fixture-adviser").await;
    let mut turn = f.turn_request(0);
    turn.text = json!({"record":f.record,"adviser_route":"manual"}).to_string();
    f.post_turn(&turn).await;
    let completed = f.finished().await;
    assert!(completed.request_error.is_none());
    assert_eq!(f.calls().len(), 1);
    assert!(completed.adviser_outcome.is_none());
}

#[tokio::test]
async fn adviser_failures_timeouts_and_malformed_advice_are_saved_then_bokkie_continues() {
    let _serial = MODEL_PEER_TESTS.lock().await;
    for (model, status) in [
        ("fixture-adviser-fail", "failed"),
        ("fixture-adviser-timeout", "timeout"),
        ("fixture-adviser-malformed", "failed"),
    ] {
        let f = ModelApplication::new("fixture-adviser-main").await;
        f.configure_adviser(false, 2, model).await;
        let mut turn = f.turn_request(0);
        turn.consult_adviser = true;
        turn.text = json!({"record":f.record,"adviser_route":"manual"}).to_string();
        f.post_turn(&turn).await;
        let completed = f.finished().await;
        assert!(
            completed.request_error.is_none(),
            "{model}: {:?}",
            completed.request_error
        );
        assert!(
            completed
                .messages
                .last()
                .unwrap()
                .text
                .contains("consultation failed")
        );
        let outcome = completed.adviser_outcome.as_ref().unwrap();
        assert_eq!(outcome.status, status);
        assert!(outcome.advice.is_none());
        assert!(outcome.error.is_some());
        assert_eq!(f.calls().len(), 2);
        let ledger = f.invocations();
        assert_eq!(ledger[0]["status"], "failed");
        assert_eq!(ledger[1]["status"], "completed");
        f.replay_is_free(&turn, &completed).await;
    }
}

#[tokio::test]
async fn adviser_return_budget_is_reserved_and_a_recursive_consult_cannot_dispatch_again() {
    let _serial = MODEL_PEER_TESTS.lock().await;
    for (calls, route, expected_calls) in [(2, "advice_first", 1), (4, "repeat_advice", 3)] {
        let f = ModelApplication::new("fixture-adviser-main").await;
        f.configure_adviser(true, calls, "fixture-adviser").await;
        let mut turn = f.turn_request(0);
        turn.text = json!({"record":f.record,"adviser_route":route,"requirements":["Only at 9 am","Only at 10 am"]}).to_string();
        f.post_turn(&turn).await;
        let completed = f.finished().await;
        assert!(completed.request_error.is_some());
        assert_eq!(f.calls().len(), expected_calls);
        assert!(f.invocations().iter().all(|r| r["status"] != "dispatched"));
        assert!(completed.task.is_none());
        f.replay_is_free(&turn, &completed).await;
    }
}

#[tokio::test]
async fn adviser_activity_and_restart_preserve_uncertain_dispatch_without_relaunching() {
    use bokkie::conversation::InvocationPurpose;
    let _serial = MODEL_PEER_TESTS.lock().await;
    let mut f = ModelApplication::new("fixture-adviser-main").await;
    f.configure_adviser(false, 2, "fixture-adviser").await;
    let release = f._temporary.path().join("release-adviser");
    let mut turn = f.turn_request(0);
    turn.consult_adviser = true;
    turn.text =
        json!({"record":f.record,"adviser_route":"manual","adviser_release":release}).to_string();
    f.post_turn(&turn).await;
    let deadline = Instant::now() + Duration::from_secs(3);
    while f.calls().is_empty() {
        assert!(Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
    let running: ConversationView = decoded(
        request(
            &f.app,
            Method::GET,
            "/conversations/model-conversation",
            None,
            None,
        )
        .await,
    );
    assert_eq!(running.activity.as_deref(), Some("Consulting Astra"));
    assert_eq!(
        running.adviser_outcome.as_ref().unwrap().status,
        "dispatched"
    );
    f.store
        .conversation_interrupt("different-restart-session", 101)
        .unwrap();
    let interrupted: ConversationView = decoded(
        request(
            &f.app,
            Method::GET,
            "/conversations/model-conversation",
            None,
            None,
        )
        .await,
    );
    assert_eq!(
        interrupted.adviser_outcome.as_ref().unwrap().status,
        "interrupted"
    );
    assert!(
        interrupted
            .adviser_outcome
            .as_ref()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("no automatic replay")
    );
    assert!(
        f.store
            .conversation_dispatch(&turn, 1, InvocationPurpose::AfterAdvice, 101)
            .is_err()
    );
    f.replay_is_free(&turn, &interrupted).await;
    std::fs::write(release, "release").unwrap();
    // Restart fences the old owner: a later completion cannot replace its saved
    // interrupted outcome or admit a return call.
    assert_eq!(f.calls().len(), 1);
    assert_eq!(f.invocations()[0]["status"], "interrupted");
}

#[tokio::test]
async fn adviser_deadline_exhaustion_after_dispatch_leaves_saved_outcome_and_no_return_dispatch() {
    let _serial = MODEL_PEER_TESTS.lock().await;
    let f = ModelApplication::new("fixture-adviser-main").await;
    f.configure_adviser(false, 2, "fixture-adviser").await;
    let release = f._temporary.path().join("release-deadline-adviser");
    let mut turn = f.turn_request(0);
    turn.consult_adviser = true;
    turn.text =
        json!({"record":f.record,"adviser_route":"manual","adviser_release":release}).to_string();
    f.post_turn(&turn).await;
    let deadline = Instant::now() + Duration::from_secs(3);
    while f.calls().is_empty() {
        assert!(Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
    f.clock.set(111);
    std::fs::write(release, "release").unwrap();
    let completed = f.finished().await;
    assert!(
        completed
            .request_error
            .as_deref()
            .unwrap()
            .contains("time limit")
    );
    assert_eq!(f.calls().len(), 1);
    assert_eq!(
        completed.adviser_outcome.as_ref().unwrap().status,
        "completed"
    );
    assert!(f.invocations().iter().all(|r| r["status"] != "dispatched"));
    f.replay_is_free(&turn, &completed).await;
}

#[tokio::test]
async fn adviser_time_reservation_refuses_consultation_before_consuming_a_slot() {
    let _serial = MODEL_PEER_TESTS.lock().await;
    let f = ModelApplication::new("fixture-adviser-main").await;
    f.configure_adviser(true, 4, "fixture-adviser").await;
    let release = f._temporary.path().join("release-low-time");
    let mut turn = f.turn_request(0);
    turn.text = json!({"record":f.record,"release":release,"adviser_route":"advice_first","requirements":["Only at 9 am","Only at 10 am"]}).to_string();
    f.post_turn(&turn).await;
    let deadline = Instant::now() + Duration::from_secs(3);
    while f.calls().is_empty() {
        assert!(Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
    f.clock.set(116);
    std::fs::write(release, "release").unwrap();
    let completed = f.finished().await;
    assert!(
        completed
            .request_error
            .as_deref()
            .unwrap()
            .contains("insufficient saved budget")
    );
    assert!(completed.adviser_outcome.is_none());
    assert_eq!(f.calls().len(), 1);
    assert_eq!(f.invocations().len(), 1);
    assert_eq!(f.invocations()[0]["status"], "completed");
    f.replay_is_free(&turn, &completed).await;
}

#[tokio::test]
async fn adviser_context_rejection_settles_dispatch_and_returns_the_bounded_failure_to_bokkie() {
    let _serial = MODEL_PEER_TESTS.lock().await;
    let f = ModelApplication::new("fixture-adviser-main").await;
    let mut saved = f.configure_adviser(false, 2, "fixture-adviser").await;
    saved.adviser.as_mut().unwrap().role.max_context_bytes = 1024;
    let save = bokkie::AgentSettingsSaveRequest {
        command_id: "small-adviser-context".into(),
        expected_revision: saved.revision,
        main: saved.main,
        adviser: saved.adviser,
    };
    let _: bokkie::AgentSettingsView = decoded(
        request(
            &f.app,
            Method::POST,
            "/agent-settings",
            Some(&f.token),
            Some(json!(save)),
        )
        .await,
    );
    let mut turn = f.turn_request(0);
    turn.consult_adviser = true;
    turn.text =
        json!({"record":f.record,"adviser_route":"manual","intent":"x".repeat(2048)}).to_string();
    f.post_turn(&turn).await;
    let completed = f.finished().await;
    assert!(completed.request_error.is_none());
    assert!(
        completed
            .adviser_outcome
            .as_ref()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("context or schema exceeded")
    );
    assert_eq!(
        f.calls().len(),
        1,
        "The adviser must fail before launching its broker"
    );
    let ledger = f.invocations();
    assert_eq!(ledger.len(), 2);
    assert_eq!(ledger[0]["status"], "failed");
    assert_eq!(ledger[1]["status"], "completed");
    f.replay_is_free(&turn, &completed).await;
}

#[tokio::test]
async fn adviser_hanging_peer_is_stopped_by_process_deadline_then_bokkie_returns_without_replay() {
    let _serial = MODEL_PEER_TESTS.lock().await;
    let f = ModelApplication::new("fixture-adviser-main").await;
    let mut saved = f.configure_adviser(false, 2, "fixture-adviser-hang").await;
    saved.adviser.as_mut().unwrap().role.timeout_seconds = 1;
    let save = bokkie::AgentSettingsSaveRequest {
        command_id: "one-second-adviser".into(),
        expected_revision: saved.revision,
        main: saved.main,
        adviser: saved.adviser,
    };
    let _: bokkie::AgentSettingsView = decoded(
        request(
            &f.app,
            Method::POST,
            "/agent-settings",
            Some(&f.token),
            Some(json!(save)),
        )
        .await,
    );
    let pid_file = f._temporary.path().join("hanging-adviser.pid");
    let mut turn = f.turn_request(0);
    turn.consult_adviser = true;
    turn.text =
        json!({"record":f.record,"adviser_route":"manual","adviser_pid":pid_file}).to_string();
    let started = Instant::now();
    f.post_turn(&turn).await;
    let completed = f.finished_within(Duration::from_secs(10)).await;
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(6),
        "The hang must reach its one-second deadline plus the five-second teardown allowance"
    );
    assert!(elapsed < Duration::from_secs(10));
    assert!(
        completed.request_error.is_none(),
        "{:?}",
        completed.request_error
    );
    let outcome = completed.adviser_outcome.as_ref().unwrap();
    assert_eq!(outcome.status, "timeout");
    assert!(
        outcome
            .error
            .as_deref()
            .unwrap()
            .contains("conversation broker timed out")
    );
    assert!(outcome.advice.is_none());
    assert!(
        completed
            .messages
            .last()
            .unwrap()
            .text
            .contains("Astra consultation failed")
    );
    let ledger = f.invocations();
    assert_eq!(ledger.len(), 2);
    assert_eq!(ledger[0]["purpose"], "adviser_manual");
    assert_eq!(ledger[0]["status"], "failed");
    assert!(
        ledger[0]["outcome"]["error"]
            .as_str()
            .unwrap()
            .contains("timed out")
    );
    assert_eq!(ledger[1]["purpose"], "after_advice");
    assert_eq!(ledger[1]["status"], "completed");
    assert_eq!(f.calls().len(), 2);
    assert_eq!(f.calls()[0]["profile"]["timeout_seconds"], 1);
    let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
    // A zero signal performs a read-only existence check; the supervisor must
    // already have reaped the exact peer before returning its timeout outcome.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    f.replay_is_free(&turn, &completed).await;
    eprintln!(
        "hang-peer evidence: elapsed={elapsed:?}, pid={pid}, peer_absent=ESRCH, adviser_ledger=failed, adviser_view=timeout, bokkie_return=completed, calls=2 before_and_after_replay"
    );
}
