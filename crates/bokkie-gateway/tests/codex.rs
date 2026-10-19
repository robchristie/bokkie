//! The Codex supervisor and its safety checks, against a fake app server.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use bokkie_gateway::codex::{CodexError, Owner, RuntimeEventKind};
use bokkie_protocol::UserId;
use common::{Call, Gateway};
use serde_json::{Value, json};

async fn next_event(
    gateway: &Gateway,
    want: impl Fn(&RuntimeEventKind) -> bool,
) -> RuntimeEventKind {
    let mut events = gateway.events.lock().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = events.recv().await.expect("events flow");
            if want(&event.kind) {
                return event.kind;
            }
        }
    })
    .await
    .expect("the event arrives")
}

fn refused(result: Result<impl std::fmt::Debug, CodexError>) -> String {
    match result {
        Err(CodexError::Refused(refusal) | CodexError::ThreadRefused(refusal)) => refusal.0,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn a_checked_app_server_serves_bokkie_threads() {
    let gateway = Gateway::new().await;
    let user = UserId(1);
    let runtime = gateway.state.supervisor.runtime(user).await.unwrap();
    let thread = runtime
        .start_thread("You are Chef.".into(), None)
        .await
        .unwrap();
    assert_eq!(thread.active_permission_profile.unwrap().id, "bokkie");
    let turn = runtime
        .start_turn(&thread.thread.id, "hello", None)
        .await
        .unwrap();
    assert!(!turn.id.is_empty());

    // The thread was started in the user's workspace, under the profile,
    // with no approvals and the Bot's instructions.
    let start = &gateway.fake.received("thread/start")[0];
    assert_eq!(start.params["permissions"], "bokkie");
    assert_eq!(start.params["approvalPolicy"], "never");
    assert_eq!(start.params["developerInstructions"], "You are Chef.");
    assert_eq!(start.params["cwd"].as_str().unwrap(), runtime.workspace());
    // config/read ran at startup and again for the thread, on the user's
    // app server; the household's ran its own at startup.
    let reads = gateway.fake.received("config/read");
    assert_eq!(
        reads
            .iter()
            .filter(|r| r.owner == Owner::User(user))
            .count(),
        2
    );
    assert_eq!(
        reads.iter().filter(|r| r.owner == Owner::Household).count(),
        1
    );

    // The user's directories are private, and so is the configuration.
    let paths = gateway.state.supervisor.paths(user);
    for dir in [
        &paths.root,
        &paths.codex_home,
        &paths.home,
        &paths.workspace,
    ] {
        let mode = std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{}", dir.display());
    }
    let config = paths.codex_home.join("config.toml");
    assert_eq!(
        std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let text = std::fs::read_to_string(config).unwrap();
    assert!(text.contains("default_permissions = \"bokkie\""));
    assert!(text.contains("shell_tool = false"));

    // The same app server is reused.
    gateway.state.supervisor.runtime(user).await.unwrap();
    assert_eq!(gateway.fake.starts(user), 1);
}

#[tokio::test]
async fn each_user_has_their_own_app_server_and_codex_home() {
    let gateway = Gateway::new().await;
    let amy = gateway.state.supervisor.runtime(UserId(1)).await.unwrap();
    let ben = gateway.state.supervisor.runtime(UserId(2)).await.unwrap();
    assert_ne!(amy.workspace(), ben.workspace());
    let amy_paths = gateway.state.supervisor.paths(UserId(1));
    let ben_paths = gateway.state.supervisor.paths(UserId(2));
    assert_ne!(amy_paths.codex_home, ben_paths.codex_home);
    assert_eq!(gateway.fake.starts(UserId(1)), 1);
    assert_eq!(gateway.fake.starts(UserId(2)), 1);
}

#[tokio::test]
async fn one_users_app_server_will_not_act_on_anothers_thread() {
    let gateway = Gateway::new().await;
    let amy = gateway.state.supervisor.runtime(UserId(1)).await.unwrap();
    let ben = gateway.state.supervisor.runtime(UserId(2)).await.unwrap();
    let thread = amy.start_thread("Amy's Bot".into(), None).await.unwrap();
    // Ben's runtime has never checked Amy's thread, so it refuses to start
    // or steer a turn on it.
    let reason = refused(ben.start_turn(&thread.thread.id, "hi", None).await);
    assert!(reason.contains("not passed"), "{reason}");
    refused(
        ben.steer_turn(&thread.thread.id, "turn-1", "hi", None)
            .await,
    );
    assert!(
        gateway
            .fake
            .received("turn/start")
            .iter()
            .all(|r| r.owner != Owner::User(UserId(2)))
    );
}

#[tokio::test]
async fn an_app_server_with_unsafe_configuration_is_refused() {
    let cases: Vec<(&[&str], Value)> = vec![
        (&["default_permissions"], json!(":workspace")),
        (&["features", "shell_tool"], json!(true)),
        (&["features", "unified_exec"], Value::Null),
        (&["model_catalog_json"], json!("/elsewhere/models.json")),
        (&["web_search"], json!("live")),
        (&["mcp_servers"], json!({"files": {"command": "mcp-files"}})),
        (
            &["permissions", "bokkie", "network", "enabled"],
            json!(true),
        ),
    ];
    for (path, value) in cases {
        let gateway = Gateway::new().await;
        gateway.fake.with(|b| {
            b.config_override = Some((path.iter().map(|s| s.to_string()).collect(), value.clone()))
        });
        let reason = refused(gateway.state.supervisor.runtime(UserId(1)).await);
        assert!(!reason.is_empty(), "{path:?}");
        assert!(gateway.state.supervisor.running(UserId(1)).await.is_none());
    }
}

#[tokio::test]
async fn a_model_outside_bokkies_catalog_is_refused() {
    let gateway = Gateway::new().await;
    gateway
        .fake
        .with(|b| b.extra_model = Some("gpt-remote-tools".into()));
    let reason = refused(gateway.state.supervisor.runtime(UserId(1)).await);
    assert!(reason.contains("gpt-remote-tools"), "{reason}");
}

#[tokio::test]
async fn a_tampered_catalog_is_refused() {
    let gateway = Gateway::new().await;
    let path = gateway.state.config.catalog_path();
    let mut catalog: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    catalog["models"][0]["apply_patch_tool_type"] = json!("freeform");
    std::fs::write(&path, catalog.to_string()).unwrap();
    let reason = refused(gateway.state.supervisor.runtime(UserId(1)).await);
    assert!(reason.contains("apply_patch_tool_type"), "{reason}");
    assert_eq!(gateway.fake.starts(UserId(1)), 0, "no app server started");
}

#[tokio::test]
async fn a_thread_without_the_bokkie_profile_is_not_served() {
    let gateway = Gateway::new().await;
    let runtime = gateway.state.supervisor.runtime(UserId(1)).await.unwrap();
    gateway.fake.with(|b| b.profile = ":workspace".into());
    let reason = refused(runtime.start_thread("x".into(), None).await);
    assert!(reason.contains(":workspace"), "{reason}");
    // Resume is checked the same way.
    let reason = refused(runtime.resume_thread("thr-old", "x".into()).await);
    assert!(reason.contains("bokkie"), "{reason}");
    refused(runtime.start_turn("thr-old", "hi", None).await);
    assert!(gateway.fake.received("turn/start").is_empty());
}

#[tokio::test]
async fn threads_must_be_resumed_on_a_new_app_server() {
    let gateway = Gateway::new().await;
    let user = UserId(1);
    let first = gateway.state.supervisor.runtime(user).await.unwrap();
    let thread = first
        .start_thread("x".into(), None)
        .await
        .unwrap()
        .thread
        .id;
    gateway.fake.crash(user);
    // Wake the watcher so it sees the closed stream.
    let _ = first.start_turn(&thread, "hi", None).await;
    next_event(&gateway, |k| matches!(k, RuntimeEventKind::Exited)).await;
    assert!(!first.is_alive());

    let second = gateway.state.supervisor.runtime(user).await.unwrap();
    assert_eq!(gateway.fake.starts(user), 2);
    refused(second.start_turn(&thread, "hi", None).await);
    second.resume_thread(&thread, "x".into()).await.unwrap();
    second.start_turn(&thread, "hi", None).await.unwrap();
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

#[tokio::test]
async fn a_forbidden_item_mid_turn_stops_codex_for_the_whole_gateway() {
    let gateway = Gateway::new().await;
    let (amy, ben) = (UserId(1), UserId(2));
    gateway.fake.with(|b| b.finish_turns = false);
    let runtime = gateway.state.supervisor.runtime(amy).await.unwrap();
    let bens = gateway.state.supervisor.runtime(ben).await.unwrap();
    let thread = runtime
        .start_thread("x".into(), None)
        .await
        .unwrap()
        .thread
        .id;
    let turn = runtime.start_turn(&thread, "hi", None).await.unwrap().id;
    gateway.fake.inject(
        amy,
        json!({"method": "item/started", "params": {"threadId": thread, "turnId": turn,
            "item": {"type": "commandExecution", "id": "c1", "command": "cat /data/bokkie.sqlite3",
                     "cwd": "/", "status": "inProgress", "aggregatedOutput": null, "exitCode": null}}}),
    );
    let kind = next_event(&gateway, |k| matches!(k, RuntimeEventKind::Refused { .. })).await;
    let RuntimeEventKind::Refused { thread_id, reason } = kind else {
        unreachable!()
    };
    assert_eq!(thread_id, thread);
    assert!(
        !reason.contains("sqlite"),
        "the command is not repeated: {reason}"
    );
    // Every app server stops, and none is served until the gateway restarts.
    wait_until(|| !runtime.is_alive() && !bens.is_alive()).await;
    assert!(gateway.state.supervisor.tainted().is_some());
    refused(gateway.state.supervisor.runtime(amy).await);
    refused(gateway.state.supervisor.runtime(ben).await);
    refused(runtime.start_turn(&thread, "again", None).await);
    refused(runtime.resume_thread(&thread, "x".into()).await);
}

#[tokio::test]
async fn a_refused_thread_stays_refused_on_the_next_app_server() {
    let gateway = Gateway::new().await;
    let user = UserId(1);
    let first = gateway.state.supervisor.runtime(user).await.unwrap();
    let good = first
        .start_thread("x".into(), None)
        .await
        .unwrap()
        .thread
        .id;
    gateway.fake.with(|b| b.profile = ":workspace".into());
    refused(first.resume_thread("thr-bad", "x".into()).await);
    gateway.fake.crash(user);
    let _ = first.start_turn(&good, "hi", None).await;
    next_event(&gateway, |k| matches!(k, RuntimeEventKind::Exited)).await;

    // Even with the app server now reporting the right profile, the
    // refused thread is not served again; the other thread is.
    gateway.fake.with(|b| b.profile = "bokkie".into());
    let second = gateway.state.supervisor.runtime(user).await.unwrap();
    refused(second.resume_thread("thr-bad", "x".into()).await);
    refused(second.start_turn("thr-bad", "hi", None).await);
    second.resume_thread(&good, "x".into()).await.unwrap();
    second.start_turn(&good, "hi", None).await.unwrap();
}

#[tokio::test]
async fn approval_requests_are_declined_and_stop_codex() {
    let gateway = Gateway::new().await;
    let user = UserId(1);
    let runtime = gateway.state.supervisor.runtime(user).await.unwrap();
    let thread = runtime
        .start_thread("x".into(), None)
        .await
        .unwrap()
        .thread
        .id;
    gateway.fake.inject(
        user,
        json!({"id": 900, "method": "item/fileChange/requestApproval", "params": {
            "threadId": thread, "turnId": "turn-1", "itemId": "f1", "reason": null}}),
    );
    next_event(&gateway, |k| matches!(k, RuntimeEventKind::Refused { .. })).await;
    let answer = gateway
        .fake
        .received("<response>")
        .into_iter()
        .find(|r| r.params["id"] == 900)
        .expect("the request was answered first");
    assert_eq!(answer.params["result"]["decision"], "decline");
    assert!(gateway.state.supervisor.tainted().is_some());
    refused(runtime.start_turn(&thread, "hi", None).await);
}

#[tokio::test]
async fn unknown_approval_kinds_stop_codex_too() {
    let gateway = Gateway::new().await;
    let user = UserId(1);
    let runtime = gateway.state.supervisor.runtime(user).await.unwrap();
    let thread = runtime
        .start_thread("y".into(), None)
        .await
        .unwrap()
        .thread
        .id;
    gateway.fake.inject(
        user,
        json!({"id": 901, "method": "item/permissions/requestApproval", "params": {
            "threadId": thread, "turnId": "turn-2", "itemId": "p1"}}),
    );
    next_event(
        &gateway,
        |k| matches!(k, RuntimeEventKind::Refused { thread_id, .. } if *thread_id == thread),
    )
    .await;
    let answer = gateway
        .fake
        .received("<response>")
        .into_iter()
        .find(|r| r.params["id"] == 901)
        .expect("answered");
    assert!(answer.params["error"].is_object());
    assert!(gateway.state.supervisor.tainted().is_some());
}

#[tokio::test]
async fn unreadable_items_and_approvals_fail_closed() {
    // An item whose shape no longer decodes cannot be checked.
    let gateway = Gateway::new().await;
    let user = UserId(1);
    let runtime = gateway.state.supervisor.runtime(user).await.unwrap();
    gateway.fake.inject(
        user,
        json!({"method": "item/started", "params": {"threadId": "thr-1-1", "turnId": "t",
            "item": {"type": "commandExecution", "id": 7}}}),
    );
    wait_until(|| gateway.state.supervisor.tainted().is_some()).await;
    wait_until(|| !runtime.is_alive()).await;

    // So can an approval request whose parameters do not decode.
    let gateway = Gateway::new().await;
    gateway.state.supervisor.runtime(user).await.unwrap();
    gateway.fake.inject(
        user,
        json!({"id": 902, "method": "item/commandExecution/requestApproval", "params": {"x": 1}}),
    );
    wait_until(|| gateway.state.supervisor.tainted().is_some()).await;
}

#[tokio::test]
async fn questions_to_the_user_are_answered_with_an_error() {
    let gateway = Gateway::new().await;
    let user = UserId(1);
    let runtime = gateway.state.supervisor.runtime(user).await.unwrap();
    let thread = runtime
        .start_thread("x".into(), None)
        .await
        .unwrap()
        .thread
        .id;
    gateway.fake.inject(
        user,
        json!({"id": 903, "method": "item/tool/requestUserInput", "params": {
            "threadId": thread, "turnId": "t", "itemId": "q", "isBlocking": true,
            "questions": []}}),
    );
    wait_until(|| {
        gateway
            .fake
            .received("<response>")
            .iter()
            .any(|r| r.params["id"] == 903 && r.params["error"].is_object())
    })
    .await;
    // That is not a safety breach.
    assert!(gateway.state.supervisor.tainted().is_none());
    assert!(runtime.is_served(&thread));
}

#[tokio::test]
async fn refused_runtimes_answer_unavailable() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    gateway.fake.with(|b| {
        b.config_override = Some((vec!["features".into(), "shell_tool".into()], json!(true)))
    });
    let reply = gateway
        .send(Call::post("/api/v1/codex/login").cookie(&admin))
        .await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(reply.code(), "unavailable");
    assert!(!reply.body.to_string().contains("shell_tool"));
}

#[tokio::test]
async fn disabling_a_user_stops_their_app_server() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    let (_amy, amy_id) = gateway.member(&admin, "amy").await;
    gateway
        .state
        .supervisor
        .runtime(UserId(amy_id))
        .await
        .unwrap();
    let runtime = gateway
        .state
        .supervisor
        .running(UserId(amy_id))
        .await
        .expect("running");
    let reply = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/admin/users/{amy_id}"))
                .cookie(&admin)
                .json(&json!({"disabled": true})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(!runtime.is_alive());
    assert!(
        gateway
            .state
            .supervisor
            .running(UserId(amy_id))
            .await
            .is_none()
    );
}
