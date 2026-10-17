//! The Codex supervisor and its safety checks, against a fake app server.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use bokkie_gateway::codex::{CodexError, RuntimeEventKind};
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
        Err(CodexError::Refused(refusal)) => refusal.0,
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
        .start_turn(&thread.thread.id, "hello")
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
    // config/read ran at startup and again for the thread.
    assert_eq!(gateway.fake.received("config/read").len(), 2);

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
    let reason = refused(ben.start_turn(&thread.thread.id, "hi").await);
    assert!(reason.contains("not passed"), "{reason}");
    refused(ben.steer_turn(&thread.thread.id, "turn-1", "hi").await);
    assert!(
        gateway
            .fake
            .received("turn/start")
            .iter()
            .all(|r| r.user != UserId(2))
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
    refused(runtime.start_turn("thr-old", "hi").await);
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
    let _ = first.start_turn(&thread, "hi").await;
    next_event(&gateway, |k| matches!(k, RuntimeEventKind::Exited)).await;
    assert!(!first.is_alive());

    let second = gateway.state.supervisor.runtime(user).await.unwrap();
    assert_eq!(gateway.fake.starts(user), 2);
    refused(second.start_turn(&thread, "hi").await);
    second.resume_thread(&thread, "x".into()).await.unwrap();
    second.start_turn(&thread, "hi").await.unwrap();
}

#[tokio::test]
async fn a_forbidden_item_mid_turn_interrupts_and_refuses_the_thread() {
    let gateway = Gateway::new().await;
    let user = UserId(1);
    gateway.fake.with(|b| b.finish_turns = false);
    let runtime = gateway.state.supervisor.runtime(user).await.unwrap();
    let thread = runtime
        .start_thread("x".into(), None)
        .await
        .unwrap()
        .thread
        .id;
    let turn = runtime.start_turn(&thread, "hi").await.unwrap().id;
    gateway.fake.inject(
        user,
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
    // The turn was interrupted and the thread is refused from now on, even
    // after a resume.
    tokio::time::timeout(Duration::from_secs(5), async {
        while gateway.fake.received("turn/interrupt").is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!runtime.is_served(&thread));
    refused(runtime.start_turn(&thread, "again").await);
    refused(runtime.resume_thread(&thread, "x".into()).await);
}

#[tokio::test]
async fn approval_requests_are_declined_and_refuse_the_thread() {
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
    let responses = gateway.fake.received("<response>");
    let answer = responses
        .iter()
        .find(|r| r.params["id"] == 900)
        .expect("the request was answered");
    assert_eq!(answer.params["result"]["decision"], "decline");
    refused(runtime.start_turn(&thread, "hi").await);

    // An unknown approval kind is refused too, and answered with an error.
    let other = runtime
        .start_thread("y".into(), None)
        .await
        .unwrap()
        .thread
        .id;
    gateway.fake.inject(
        user,
        json!({"id": 901, "method": "item/permissions/requestApproval", "params": {
            "threadId": other, "turnId": "turn-2", "itemId": "p1"}}),
    );
    next_event(
        &gateway,
        |k| matches!(k, RuntimeEventKind::Refused { thread_id, .. } if *thread_id == other),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let responses = gateway.fake.received("<response>");
            if let Some(answer) = responses.iter().find(|r| r.params["id"] == 901) {
                assert!(answer.params["error"].is_object());
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn users_see_and_start_only_their_own_codex_sign_in() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    let (amy, amy_id) = gateway.member(&admin, "amy").await;
    gateway.fake.with(|b| b.signed_in = false);
    let account = gateway
        .send(Call::get("/api/v1/codex/account").cookie(&amy))
        .await;
    assert_eq!(account.status, StatusCode::OK);
    assert_eq!(account.body, json!({"signed_in": false, "plan": null}));
    let login = gateway
        .send(Call::post("/api/v1/codex/login").cookie(&amy))
        .await;
    assert_eq!(login.status, StatusCode::OK);
    assert_eq!(login.body["user_code"], "ABCD-EFGH");
    // The sign-in went to Amy's own app server only.
    let starts = gateway.fake.received("account/login/start");
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].user, UserId(amy_id));
    // Without a session there is nothing to see.
    let anonymous = gateway.send(Call::get("/api/v1/codex/account")).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn refused_runtimes_answer_unavailable() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    gateway.fake.with(|b| {
        b.config_override = Some((vec!["features".into(), "shell_tool".into()], json!(true)))
    });
    let reply = gateway
        .send(Call::get("/api/v1/codex/account").cookie(&admin))
        .await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(reply.code(), "unavailable");
    assert!(!reply.body.to_string().contains("shell_tool"));
}

#[tokio::test]
async fn disabling_a_user_stops_their_app_server() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    let (amy, amy_id) = gateway.member(&admin, "amy").await;
    gateway
        .send(Call::get("/api/v1/codex/account").cookie(&amy))
        .await;
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
