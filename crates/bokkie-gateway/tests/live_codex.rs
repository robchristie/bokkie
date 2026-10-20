//! The supervisor against the real pinned `codex app-server`, with a mock
//! model and a synthetic household sign-in from a mock authority. Runs only
//! when `BOKKIE_CODEX_BIN` names the binary, as `scripts/verify` arranges.
//! It never uses a real ChatGPT credential or calls a real model, and each
//! test shows that nothing tried to leave the machine.
//!
//! This is the proof of the capability rule: the gateway's configuration
//! leaves every Codex tool off, and a thread offers exactly the Bokkie
//! tools of its grants, with Codex's web search only on a thread granted
//! `web.search`. It captures the tools the app server actually offers a
//! model. `tests/live_search.rs` follows web search further.

mod common;

use std::sync::Arc;
use std::time::Duration;

use bokkie_codex::protocol::{ServerNotification, TurnStatus};
use bokkie_gateway::codex::{
    CodexError, HouseholdSettings, Launcher, RuntimeEventKind, Supervisor, catalog,
};
use bokkie_gateway::grants::GrantSet;
use bokkie_mock_model::MockModel;
use bokkie_protocol::UserId;
use bokkie_protocol::api::Grant;
use common::live::Live;

/// Codex's question to the user, which every thread offers and the gateway
/// answers with an error.
const ALWAYS_OFFERED: &str = "request_user_input";

/// Exactly what a thread with these grants may offer the model on the
/// default model. Its web search is Codex's `web.run` (see
/// `tests/live_search.rs`), which the capture names `run`.
fn expected_tools(grants: &GrantSet) -> std::collections::BTreeSet<String> {
    let search = grants.contains(&Grant::WebSearch).then(|| "run".to_owned());
    bokkie_gateway::tools::granted(grants)
        .map(|tool| tool.name.to_owned())
        .chain([ALWAYS_OFFERED.to_owned()])
        .chain(search)
        .collect()
}

/// Whether a model request offered Codex's `web.run` tool, and whether it
/// offered a hosted `web_search` tool. A Responses-Lite request carries its
/// tools in an `additional_tools` input item.
fn search_tools(request: &serde_json::Value) -> (bool, bool) {
    let mut tools = request["tools"].as_array().cloned().unwrap_or_default();
    for item in request["input"].as_array().into_iter().flatten() {
        if item["type"] == "additional_tools" {
            tools.extend(item["tools"].as_array().cloned().unwrap_or_default());
        }
    }
    let standalone = tools.iter().any(|tool| {
        tool["type"] == "namespace"
            && tool["name"] == "web"
            && tool["tools"]
                .as_array()
                .is_some_and(|nested| nested.iter().any(|t| t["name"] == "run"))
    });
    let hosted = tools.iter().any(|tool| tool["type"] == "web_search");
    (standalone, hosted)
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

/// Starts a thread with `grants`, runs one turn on it, and returns the
/// tools the model was offered in that turn and the reply.
async fn capture(
    runtime: &bokkie_gateway::codex::Runtime,
    events: &mut Events,
    mock: &MockModel,
    grants: &GrantSet,
) -> (std::collections::BTreeSet<String>, String) {
    let asked = mock.requests.lock().unwrap().len();
    let thread = runtime
        .start_thread("You are a test Bot.".into(), grants)
        .await
        .expect("the thread is served");
    assert_eq!(
        thread.active_permission_profile.as_ref().unwrap().id,
        "bokkie"
    );
    assert!(thread.cwd.ends_with("users/1/workspace"), "{}", thread.cwd);
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
    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), asked + 1, "one model request per turn");
    // Web search is Codex's web.run, on exactly the web.search threads.
    assert_eq!(
        search_tools(&requests[asked]),
        (grants.contains(&Grant::WebSearch), false),
        "grants {grants:?}"
    );
    (MockModel::tools_in(&requests[asked]), text)
}

#[tokio::test]
async fn the_real_app_server_offers_exactly_the_granted_tools() {
    let Some(live) = Live::start(&["Hello", " from", " a mock"]).await else {
        return;
    };
    let mock = &live.model;
    let data = tempfile::tempdir().unwrap();
    let (supervisor, mut events) = supervisor(&live, data.path()).await;
    let user = UserId(1);
    let runtime = supervisor.runtime(user).await.expect("the checks pass");
    // Every grant with tools today, alone and together, and none.
    let with_tools = [Grant::Notes, Grant::Seen, Grant::WebSearch];
    let combinations: Vec<GrantSet> = (0..(1u32 << with_tools.len()))
        .map(|bits| {
            with_tools
                .iter()
                .enumerate()
                .filter(|(i, _)| bits & (1 << i) != 0)
                .map(|(_, g)| *g)
                .collect()
        })
        .collect();
    for grants in &combinations {
        let (offered, text) = capture(&runtime, &mut events, mock, grants).await;
        eprintln!("grants {grants:?}: offered {offered:?}");
        assert_eq!(text, "Hello from a mock");
        // The capture sees the always-offered tool, so it works, and
        // nothing else but the grants' tools and, for web.search, Codex's
        // web search.
        assert_eq!(offered, expected_tools(grants), "grants {grants:?}");
    }
    // The grants whose tools do not exist yet offer nothing of Bokkie's;
    // web.search brings Codex's own search instead.
    for grant in [Grant::Notify, Grant::WebSearch, Grant::WebFetch] {
        assert_eq!(bokkie_gateway::tools::granted(&[grant].into()).count(), 0);
    }

    // The configuration and the catalog both matter: this app server's
    // config.toml is the gateway's, and its CODEX_HOME is the user's own.
    let paths = supervisor.paths(user);
    assert!(paths.codex_home.join("config.toml").exists());
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

/// Threads that run no command need no working sandbox, so a container
/// without one (Docker's default confinement, or a CI runner that forbids
/// user namespaces) still serves them. Here every Bubblewrap fails: a failing
/// `bwrap` comes first on `PATH`, and the bundled copy is replaced by it.
#[tokio::test]
async fn threads_that_run_no_command_run_without_a_working_sandbox() {
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
        .start_thread("You are a test Bot.".into(), &Default::default())
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
    let thread = amy
        .start_thread("Amy's Bot".into(), &Default::default())
        .await
        .unwrap();
    run_turn(&amy, &mut events, &thread.thread.id).await;

    let ben = supervisor.runtime(UserId(2)).await.unwrap();
    match ben
        .resume_thread(&thread.thread.id, "Ben's Bot".into(), &Default::default())
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
        .start_thread("INSTRUCTIONS-ALPHA".into(), &Default::default())
        .await
        .unwrap()
        .thread
        .id;
    run_turn(&runtime, &mut events, &thread).await;
    // Warm resume with new instructions.
    runtime
        .resume_thread(&thread, "INSTRUCTIONS-BETA".into(), &Default::default())
        .await
        .unwrap();
    run_turn(&runtime, &mut events, &thread).await;
    // Cold resume on a new app server.
    supervisor.stop(user).await;
    let runtime = supervisor.runtime(user).await.unwrap();
    runtime
        .resume_thread(&thread, "INSTRUCTIONS-GAMMA".into(), &Default::default())
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
        .start_thread("You are a test Bot.".into(), &Default::default())
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
        .start_thread("You are a test Bot.".into(), &Default::default())
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
