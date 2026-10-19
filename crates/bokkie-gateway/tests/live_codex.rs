//! The supervisor against the real pinned `codex app-server`, with a mock
//! model and a synthetic household sign-in from a mock authority. Runs only
//! when `BOKKIE_CODEX_BIN` names the binary, as `scripts/verify` arranges.
//! It never uses a real ChatGPT credential or calls a real model, and each
//! test shows that nothing tried to leave the machine.
//!
//! This is the proof that the gateway's configuration makes threads
//! chat-only: it captures the tools the app server actually offers a model.

mod common;

use std::sync::Arc;
use std::time::Duration;

use bokkie_codex::protocol::{DynamicToolSpec, ServerNotification, TurnStatus};
use bokkie_gateway::codex::{
    CodexError, HouseholdSettings, Launcher, RuntimeEventKind, Supervisor, catalog,
};
use bokkie_mock_model::MockModel;
use bokkie_protocol::UserId;
use common::live::Live;
use serde_json::json;

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

type Events = tokio::sync::mpsc::UnboundedReceiver<bokkie_gateway::codex::RuntimeEvent>;

/// A supervisor on `live`'s mocks, with the household signed in for a day.
async fn supervisor(live: &Live, data: &std::path::Path) -> (Arc<Supervisor>, Events) {
    supervisor_with(live, data, live.launcher()).await
}

async fn supervisor_with(
    live: &Live,
    data: &std::path::Path,
    launcher: Launcher,
) -> (Arc<Supervisor>, Events) {
    let bundled = catalog::export_bundled(&live.program, &data.join("codex/catalog-home"))
        .await
        .unwrap();
    let catalog = catalog::write(&data.join("codex/models.json"), bundled).unwrap();
    assert!(catalog.slugs.contains("gpt-6.1-sol"), "{:?}", catalog.slugs);
    live.sign_in_household(data, 86_400);
    Supervisor::with_settings(data, catalog, launcher, HouseholdSettings::default())
}

#[tokio::test]
async fn the_real_app_server_passes_the_checks_and_offers_no_unsafe_tools() {
    let Some(live) = Live::start(&["Hello", " from", " a mock"]).await else {
        return;
    };
    let mock = &live.model;
    let data = tempfile::tempdir().unwrap();
    let (supervisor, mut events) = supervisor(&live, data.path()).await;
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
    let turn = runtime
        .start_turn(&thread.thread.id, "hi", None)
        .await
        .unwrap();

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
    live.assert_nothing_left_the_machine();
}

#[tokio::test]
async fn the_real_app_server_is_refused_when_a_tool_is_enabled() {
    let Some(live) = Live::start(&["x"]).await else {
        return;
    };
    let data = tempfile::tempdir().unwrap();
    let launcher = live.launcher_with(vec!["features.shell_tool=true".into()]);
    let (supervisor, _events) = supervisor_with(&live, data.path(), launcher).await;
    match supervisor.runtime(UserId(1)).await {
        Err(CodexError::Refused(refusal)) => {
            assert!(refusal.0.contains("shell_tool"), "{refusal}")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    live.assert_nothing_left_the_machine();
}

#[tokio::test]
async fn the_real_app_server_is_refused_with_another_profile() {
    let Some(live) = Live::start(&["x"]).await else {
        return;
    };
    let data = tempfile::tempdir().unwrap();
    let launcher = live.launcher_with(vec!["default_permissions=\":workspace\"".into()]);
    let (supervisor, _events) = supervisor_with(&live, data.path(), launcher).await;
    match supervisor.runtime(UserId(1)).await {
        Err(CodexError::Refused(refusal)) => {
            assert!(refusal.0.contains("default_permissions"), "{refusal}")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    live.assert_nothing_left_the_machine();
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
    let Some(live) = Live::start(&["ok"]).await else {
        return;
    };
    let mock = &live.model;
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    // Signed out, so the model needs no sign-in. Without the gateway's
    // configuration, Codex asks other hosts too (its plugins feature asks
    // github.com and chatgpt.com); the deny proxy refuses them all.
    let mut command = AppServerCommand::new(&live.program, home.path());
    command.args = MockModel::overrides(&live.base_url)
        .into_iter()
        .chain(live.chatgpt.overrides())
        .flat_map(|o| ["-c".into(), o.into()])
        .collect();
    command.env = live.env();
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
    eprintln!(
        "control: the deny proxy refused {:?}",
        live.chatgpt.proxy().hosts()
    );
}

async fn run_turn(
    runtime: &bokkie_gateway::codex::Runtime,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<bokkie_gateway::codex::RuntimeEvent>,
    thread: &str,
) -> TurnStatus {
    let turn = runtime.start_turn(thread, "hi", None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match events.recv().await.expect("events flow").kind {
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
    .expect("the turn completes")
}

/// Chat-only threads need no working sandbox, so a container without one
/// (Docker's default confinement, or a CI runner that forbids user
/// namespaces) still serves them. Here every Bubblewrap fails: a failing
/// `bwrap` comes first on `PATH`, and the bundled copy is replaced by it.
#[tokio::test]
async fn chat_only_threads_run_without_a_working_sandbox() {
    use std::os::unix::fs::PermissionsExt;
    let Some(live) = Live::start(&["still here"]).await else {
        return;
    };
    let program = std::path::PathBuf::from(std::env::var_os("BOKKIE_CODEX_BIN").unwrap());
    // Lay out a package beside the real one, so the binary can be linked
    // rather than copied, with a broken bundled bwrap.
    let package = program.parent().unwrap().parent().unwrap();
    let broken = tempfile::tempdir_in(package.parent().unwrap()).unwrap();
    for dir in ["bin", "codex-resources", "stub"] {
        std::fs::create_dir(broken.path().join(dir)).unwrap();
    }
    let linked = broken.path().join("bin/codex");
    if std::fs::hard_link(&program, &linked).is_err() {
        std::fs::copy(&program, &linked).unwrap();
    }
    std::fs::copy(
        package.join("codex-package.json"),
        broken.path().join("codex-package.json"),
    )
    .unwrap();
    std::os::unix::fs::symlink(package.join("codex-path"), broken.path().join("codex-path"))
        .unwrap();
    let stub = "#!/bin/sh\necho 'bwrap: no user namespaces here' >&2\nexit 1\n";
    for path in ["codex-resources/bwrap", "stub/bwrap"] {
        let path = broken.path().join(path);
        std::fs::write(&path, stub).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!(
        "{}:{}",
        broken.path().join("stub").display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let data = tempfile::tempdir().unwrap();
    let mut env = live.env();
    env.push(("PATH".into(), path.into()));
    let launcher = Launcher::Process {
        program: linked,
        overrides: live.overrides(),
        env,
    };
    let (supervisor, mut events) = supervisor_with(&live, data.path(), launcher).await;
    let runtime = supervisor
        .runtime(UserId(1))
        .await
        .expect("the checks pass");
    let thread = runtime
        .start_thread("You are a test Bot.".into(), None)
        .await
        .expect("a thread starts without a sandbox");
    assert_eq!(
        run_turn(&runtime, &mut events, &thread.thread.id).await,
        TurnStatus::Completed
    );
    live.assert_nothing_left_the_machine();
}

/// One user's app server cannot load another user's thread: each has its
/// own `CODEX_HOME`, which holds the threads.
#[tokio::test]
async fn one_users_app_server_cannot_resume_anothers_thread() {
    let Some(live) = Live::start(&["private"]).await else {
        return;
    };
    let data = tempfile::tempdir().unwrap();
    let (supervisor, mut events) = supervisor(&live, data.path()).await;
    let amy = supervisor.runtime(UserId(1)).await.unwrap();
    let thread = amy.start_thread("Amy's Bot".into(), None).await.unwrap();
    run_turn(&amy, &mut events, &thread.thread.id).await;

    let ben = supervisor.runtime(UserId(2)).await.unwrap();
    match ben
        .resume_thread(&thread.thread.id, "Ben's Bot".into())
        .await
    {
        Err(CodexError::Rejected(error)) => eprintln!("refused as expected: {error}"),
        other => panic!("Ben's app server loaded Amy's thread: {other:?}"),
    }
    assert!(!ben.is_served(&thread.thread.id));
    live.assert_nothing_left_the_machine();
}

fn requests_text(mock: &MockModel) -> Vec<String> {
    // Only requests the model answered: every one carried the sign-in.
    mock.requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| r["input"].to_string())
        .collect()
}

/// Bokkie relies on this Codex behaviour: developer instructions are fixed
/// when a thread starts, and `thread/resume` ignores new ones, warm or
/// cold. A changed Bot profile therefore travels as turn context instead.
#[tokio::test]
async fn developer_instructions_are_fixed_at_thread_start() {
    let Some(live) = Live::start(&["ok"]).await else {
        return;
    };
    let mock = &live.model;
    let data = tempfile::tempdir().unwrap();
    let (supervisor, mut events) = supervisor(&live, data.path()).await;
    let user = UserId(1);
    let runtime = supervisor.runtime(user).await.unwrap();
    let thread = runtime
        .start_thread("INSTRUCTIONS-ALPHA".into(), None)
        .await
        .unwrap()
        .thread
        .id;
    run_turn(&runtime, &mut events, &thread).await;
    // Warm resume with new instructions.
    runtime
        .resume_thread(&thread, "INSTRUCTIONS-BETA".into())
        .await
        .unwrap();
    run_turn(&runtime, &mut events, &thread).await;
    // Cold resume on a new app server.
    supervisor.stop(user).await;
    let runtime = supervisor.runtime(user).await.unwrap();
    runtime
        .resume_thread(&thread, "INSTRUCTIONS-GAMMA".into())
        .await
        .unwrap();
    run_turn(&runtime, &mut events, &thread).await;
    let requests = requests_text(mock);
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert!(request.contains("INSTRUCTIONS-ALPHA"));
        assert!(!request.contains("INSTRUCTIONS-BETA"));
        assert!(!request.contains("INSTRUCTIONS-GAMMA"));
    }
    live.assert_nothing_left_the_machine();
}

/// And this: an application context fragment reaches the model as a
/// tagged developer message and stays in the thread's history.
#[tokio::test]
async fn profile_context_reaches_the_model_and_stays_in_history() {
    use bokkie_codex::protocol::{AdditionalContextEntry, AdditionalContextKind};
    let Some(live) = Live::start(&["ok"]).await else {
        return;
    };
    let mock = &live.model;
    let data = tempfile::tempdir().unwrap();
    let (supervisor, mut events) = supervisor(&live, data.path()).await;
    let user = UserId(1);
    let runtime = supervisor.runtime(user).await.unwrap();
    let thread = runtime
        .start_thread("You are a test Bot.".into(), None)
        .await
        .unwrap()
        .thread
        .id;
    let context = [(
        "bokkie-profile".to_owned(),
        AdditionalContextEntry {
            kind: AdditionalContextKind::Application,
            value: "PROFILE-DELTA".into(),
        },
    )]
    .into();
    let turn = runtime
        .start_turn(&thread, "first", Some(context))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let RuntimeEventKind::Notification(ServerNotification::TurnCompleted(done)) =
                events.recv().await.unwrap().kind
                && done.turn.id == turn.id
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    run_turn(&runtime, &mut events, &thread).await;
    let requests = requests_text(mock);
    for request in &requests {
        assert!(
            request.contains("<bokkie-profile>PROFILE-DELTA</bokkie-profile>"),
            "{request}"
        );
    }
    assert_eq!(requests.len(), 2);
    live.assert_nothing_left_the_machine();
}

/// And this: a context fragment sent with `turn/steer` reaches the model
/// within the running turn, which is how a profile change made mid-turn
/// travels.
#[tokio::test]
async fn profile_context_on_a_steer_reaches_the_model() {
    use bokkie_codex::protocol::{AdditionalContextEntry, AdditionalContextKind};
    let Some(live) = Live::start(&["ok"]).await else {
        return;
    };
    let mock = &live.model;
    let data = tempfile::tempdir().unwrap();
    let (supervisor, mut events) = supervisor(&live, data.path()).await;
    let runtime = supervisor.runtime(UserId(1)).await.unwrap();
    let thread = runtime
        .start_thread("You are a test Bot.".into(), None)
        .await
        .unwrap()
        .thread
        .id;
    // The model holds its first answer, so the turn is still running.
    let release = mock.hold_next();
    let turn = runtime.start_turn(&thread, "first", None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        while mock.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the model is asked");
    let context = [(
        "bokkie-profile".to_owned(),
        AdditionalContextEntry {
            kind: AdditionalContextKind::Application,
            value: "PROFILE-STEERED".into(),
        },
    )]
    .into();
    let steered = runtime
        .steer_turn(&thread, &turn.id, "STEER-TEXT", Some(context))
        .await
        .unwrap();
    assert_eq!(steered, turn.id);
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let RuntimeEventKind::Notification(ServerNotification::TurnCompleted(done)) =
                events.recv().await.unwrap().kind
                && done.turn.id == turn.id
            {
                break;
            }
        }
    })
    .await
    .expect("the turn completes");
    let requests = requests_text(mock);
    assert!(!requests[0].contains("PROFILE-STEERED"), "{}", requests[0]);
    let later = &requests[1..];
    assert!(
        later.iter().any(|r| r.contains("STEER-TEXT")
            && r.contains("<bokkie-profile>PROFILE-STEERED</bokkie-profile>")),
        "{later:?}"
    );
    live.assert_nothing_left_the_machine();
}
