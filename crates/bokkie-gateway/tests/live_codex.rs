//! The supervisor against the real pinned `codex app-server`, with a mock
//! model. Runs only when `BOKKIE_CODEX_BIN` names the binary, as
//! `scripts/verify` arranges. It never signs in or calls a real model.
//!
//! This is the proof that the gateway's configuration makes threads
//! chat-only: it captures the tools the app server actually offers a model.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use bokkie_codex::protocol::{DynamicToolSpec, ServerNotification, TurnStatus};
use bokkie_gateway::codex::{CodexError, Launcher, RuntimeEventKind, Supervisor, catalog};
use bokkie_protocol::UserId;
use common::mock_model::MockModel;
use serde_json::json;

fn codex() -> Option<PathBuf> {
    std::env::var_os("BOKKIE_CODEX_BIN").map(PathBuf::from)
}

/// Tools that may be offered while threads are chat-only: Bokkie's own
/// dynamic tools, and Codex's question to the user, which the gateway
/// answers with an error.
const ALLOWED_TOOLS: &[&str] = &["message_bot", "request_user_input"];

fn message_bot() -> DynamicToolSpec {
    DynamicToolSpec::Function {
        name: "message_bot".into(),
        description: "Send a message to another of the user's Bots.".into(),
        input_schema: json!({"type": "object", "properties": {
            "bot": {"type": "string"}, "message": {"type": "string"}},
            "required": ["bot", "message"]}),
    }
}

async fn supervisor(
    program: PathBuf,
    data: &std::path::Path,
    overrides: Vec<String>,
) -> (
    Supervisor,
    tokio::sync::mpsc::UnboundedReceiver<bokkie_gateway::codex::RuntimeEvent>,
) {
    let bundled = catalog::export_bundled(&program, &data.join("codex/catalog-home"))
        .await
        .unwrap();
    let catalog = catalog::write(&data.join("codex/models.json"), bundled).unwrap();
    assert!(catalog.slugs.contains("gpt-6.1-sol"), "{:?}", catalog.slugs);
    Supervisor::new(data, catalog, Launcher::Process { program, overrides })
}

#[tokio::test]
async fn the_real_app_server_passes_the_checks_and_offers_no_unsafe_tools() {
    let Some(program) = codex() else {
        eprintln!("BOKKIE_CODEX_BIN is not set; skipping");
        return;
    };
    let data = tempfile::tempdir().unwrap();
    let (mock, base_url) = MockModel::start(&["Hello", " from", " a mock"]).await;
    let (supervisor, mut events) =
        supervisor(program, data.path(), MockModel::overrides(&base_url)).await;
    let user = UserId(1);
    let runtime = supervisor.runtime(user).await.expect("the checks pass");
    let thread = runtime
        .start_thread("You are a test Bot.".into(), Some(vec![message_bot()]))
        .await
        .expect("the thread is served");
    assert_eq!(
        thread.active_permission_profile.as_ref().unwrap().id,
        "bokkie"
    );
    let turn = runtime.start_turn(&thread.thread.id, "hi").await.unwrap();

    let mut text = String::new();
    let status = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let event = events.recv().await.expect("events flow");
            match event.kind {
                RuntimeEventKind::Notification(ServerNotification::AgentMessageDelta(delta)) => {
                    text.push_str(&delta.delta)
                }
                RuntimeEventKind::Notification(ServerNotification::TurnCompleted(done))
                    if done.turn.id == turn.id =>
                {
                    return done.turn.status;
                }
                RuntimeEventKind::Refused { reason, .. } => panic!("refused: {reason}"),
                _ => {}
            }
        }
    })
    .await
    .expect("the turn completes");
    assert_eq!(status, TurnStatus::Completed);
    assert_eq!(text, "Hello from a mock");

    let offered = mock.offered_tools();
    eprintln!("offered tools: {offered:?}");
    assert!(
        offered.contains("message_bot"),
        "capture works: {offered:?}"
    );
    let unsafe_tools: Vec<_> = offered
        .iter()
        .filter(|name| !ALLOWED_TOOLS.contains(&name.as_str()))
        .collect();
    assert!(
        unsafe_tools.is_empty(),
        "the app server offered {unsafe_tools:?}"
    );

    // The configuration and the catalog both matter: this app server's
    // config.toml is the gateway's, and its CODEX_HOME is the user's own.
    let paths = supervisor.paths(user);
    assert!(paths.codex_home.join("config.toml").exists());
    assert!(thread.cwd.ends_with("users/1/workspace"), "{}", thread.cwd);
}

#[tokio::test]
async fn the_real_app_server_is_refused_when_a_tool_is_enabled() {
    let Some(program) = codex() else {
        eprintln!("BOKKIE_CODEX_BIN is not set; skipping");
        return;
    };
    let data = tempfile::tempdir().unwrap();
    let (_mock, base_url) = MockModel::start(&["x"]).await;
    let mut overrides = MockModel::overrides(&base_url);
    overrides.push("features.shell_tool=true".into());
    let (supervisor, _events) = supervisor(program, data.path(), overrides).await;
    match supervisor.runtime(UserId(1)).await {
        Err(CodexError::Refused(refusal)) => {
            assert!(refusal.0.contains("shell_tool"), "{refusal}")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn the_real_app_server_is_refused_with_another_profile() {
    let Some(program) = codex() else {
        eprintln!("BOKKIE_CODEX_BIN is not set; skipping");
        return;
    };
    let data = tempfile::tempdir().unwrap();
    let (_mock, base_url) = MockModel::start(&["x"]).await;
    let mut overrides = MockModel::overrides(&base_url);
    overrides.push("default_permissions=\":workspace\"".into());
    let (supervisor, _events) = supervisor(program, data.path(), overrides).await;
    match supervisor.runtime(UserId(1)).await {
        Err(CodexError::Refused(refusal)) => {
            assert!(refusal.0.contains("default_permissions"), "{refusal}")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// The control: the same capture, on an app server without the gateway's
/// configuration and catalog, does see the unsafe tools. So the test above
/// would notice if they came back.
#[tokio::test]
async fn without_the_gateways_configuration_the_capture_sees_unsafe_tools() {
    use bokkie_codex::protocol::{
        ClientInfo, InitializeCapabilities, InitializeParams, ThreadStartParams, TurnStartParams,
    };
    use bokkie_codex::{AppServer, AppServerCommand, Incoming};
    let Some(program) = codex() else {
        eprintln!("BOKKIE_CODEX_BIN is not set; skipping");
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let (mock, base_url) = MockModel::start(&["ok"]).await;
    let mut command = AppServerCommand::new(&program, home.path());
    command.args = MockModel::overrides(&base_url)
        .into_iter()
        .flat_map(|o| ["-c".into(), o.into()])
        .collect();
    command.current_dir = Some(workspace.path().to_owned());
    command.stderr = Some(home.path().join("stderr"));
    let (server, mut incoming) = AppServer::spawn(&command).unwrap();
    server
        .client
        .initialize(&InitializeParams {
            client_info: ClientInfo {
                name: "control".into(),
                version: "0".into(),
                title: None,
            },
            capabilities: Some(InitializeCapabilities {
                experimental_api: true,
                opt_out_notification_methods: None,
            }),
        })
        .await
        .unwrap();
    let thread = server
        .client
        .thread_start(&ThreadStartParams {
            cwd: Some(workspace.path().display().to_string()),
            ..ThreadStartParams::default()
        })
        .await
        .unwrap();
    server
        .client
        .turn_start(&TurnStartParams::text(thread.thread.id, "hi"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(message) = incoming.recv().await {
            if let Incoming::Notification(ServerNotification::TurnCompleted(_)) = message {
                break;
            }
        }
    })
    .await
    .unwrap();
    let offered = mock.offered_tools();
    eprintln!("control offered tools: {offered:?}");
    for unsafe_tool in ["exec>apply_patch", "exec>exec_command"] {
        assert!(
            offered.contains(unsafe_tool),
            "{unsafe_tool} in {offered:?}"
        );
    }
}
