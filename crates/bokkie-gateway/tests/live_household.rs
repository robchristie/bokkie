//! The household's shared ChatGPT sign-in with the real pinned
//! `codex app-server`: the household app server and two users' app servers
//! on one synthetic sign-in from a mock token endpoint. Runs only when
//! `BOKKIE_CODEX_BIN` names the binary.
//!
//! No real credential is involved and nothing leaves the machine: tokens
//! come from the mock authority, every endpoint the app servers reach is a
//! local mock, and each test shows that the deny proxy, which receives
//! anything else, received nothing. Each app server names itself in its
//! token requests (`household` or `user-<id>`), so the mock records who
//! refreshed.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use bokkie_codex::protocol::{ServerNotification, Turn, TurnStatus};
use bokkie_gateway::codex::household::LINK_TARGET;
use bokkie_gateway::codex::{
    CodexError, HouseholdSettings, Runtime, RuntimeEvent, RuntimeEventKind, Supervisor, catalog,
};
use bokkie_mock_model::chatgpt::{Refresh, RefreshOutcome};
use bokkie_protocol::UserId;
use common::live::Live;
use common::{Call, Gateway};
use serde_json::json;

/// Codex refreshes a token itself within 5 minutes of its expiry.
const CODEX_WINDOW: i64 = 5 * 60;

/// Turns as they complete, by id, collected from every user's app server.
#[derive(Clone, Default)]
struct Turns(Arc<Mutex<HashMap<String, Turn>>>);

impl Turns {
    fn collect(events: tokio::sync::mpsc::UnboundedReceiver<RuntimeEvent>) -> Self {
        let turns = Self::default();
        let collected = turns.clone();
        let mut events = events;
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if let RuntimeEventKind::Notification(ServerNotification::TurnCompleted(done)) =
                    event.kind
                {
                    collected
                        .0
                        .lock()
                        .unwrap()
                        .insert(done.turn.id.clone(), done.turn);
                }
            }
        });
        turns
    }

    async fn wait(&self, turn_id: &str) -> Turn {
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            if let Some(turn) = self.0.lock().unwrap().get(turn_id) {
                return turn.clone();
            }
            assert!(Instant::now() < deadline, "turn {turn_id} never completed");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// Starts a turn on `thread` and waits for it to end.
async fn turn(turns: &Turns, runtime: &Runtime, thread: &str) -> Turn {
    let started = runtime.start_turn(thread, "hi", None).await.unwrap();
    turns.wait(&started.id).await
}

/// A supervisor on `live`'s mocks with the household signed in, its access
/// token expiring `expires_in` seconds after the instant returned.
async fn supervisor(
    live: &Live,
    data: &std::path::Path,
    settings: HouseholdSettings,
    expires_in: i64,
) -> (Arc<Supervisor>, Turns, Instant) {
    let bundled = catalog::export_bundled(&live.program, &data.join("codex/catalog-home"))
        .await
        .unwrap();
    let catalog = catalog::write(&data.join("codex/models.json"), bundled).unwrap();
    // Expiries are whole seconds, so this is at most a second early.
    let issued = Instant::now();
    live.sign_in_household(data, expires_in);
    let (supervisor, events) = Supervisor::with_settings(data, catalog, live.launcher(), settings);
    (supervisor, Turns::collect(events), issued)
}

/// Refreshing only when a token has under a minute left, so that the
/// household leaves a token with more than Codex's 5 minutes alone.
fn late_refresh() -> HouseholdSettings {
    HouseholdSettings {
        refresh_ahead: Duration::from_secs(60),
        ..HouseholdSettings::default()
    }
}

/// Sleeps until a token issued at `issued` that expires `expires_in`
/// seconds later is inside Codex's own refresh window, with a margin.
async fn until_inside_codex_window(issued: Instant, expires_in: i64) {
    let inside = issued + Duration::from_secs((expires_in - CODEX_WINDOW + 3) as u64);
    tokio::time::sleep_until(inside.into()).await;
}

fn generations(live: &Live, from: usize) -> Vec<Option<u64>> {
    live.model_generations()[from..].to_vec()
}

fn summary(refreshes: &[Refresh]) -> Vec<(String, RefreshOutcome)> {
    refreshes
        .iter()
        .map(|r| (r.client_id.clone(), r.outcome))
        .collect()
}

/// Two users' app servers and the household's share one sign-in. A
/// household refresh rotates the token once; both users' next turns use the
/// new token, which they read from the shared file without calling the
/// token endpoint themselves.
#[tokio::test]
async fn a_household_refresh_reaches_both_users_without_their_refreshing() {
    let Some(live) = Live::start(&["ok"]).await else {
        return;
    };
    let data = tempfile::tempdir().unwrap();
    let expires_in = CODEX_WINDOW + 25;
    let (supervisor, turns, issued) =
        supervisor(&live, data.path(), late_refresh(), expires_in).await;
    let amy = supervisor.runtime(UserId(1)).await.unwrap();
    let ben = supervisor.runtime(UserId(2)).await.unwrap();
    // One file: both links resolve to the household's.
    let household_file = supervisor.household_paths().auth_json();
    for user in [UserId(1), UserId(2)] {
        let link = supervisor.paths(user).auth_json();
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            std::path::Path::new(LINK_TARGET)
        );
        assert_eq!(
            std::fs::canonicalize(&link).unwrap(),
            std::fs::canonicalize(&household_file).unwrap()
        );
    }
    let amys = amy
        .start_thread("Amy's Bot".into(), None)
        .await
        .unwrap()
        .thread
        .id;
    let bens = ben
        .start_thread("Ben's Bot".into(), None)
        .await
        .unwrap()
        .thread
        .id;
    for (runtime, thread) in [(&amy, &amys), (&ben, &bens)] {
        assert_eq!(
            turn(&turns, runtime, thread).await.status,
            TurnStatus::Completed
        );
    }
    assert_eq!(live.model_generations(), [Some(1), Some(1)]);
    assert!(live.refreshers().is_empty());

    // The household refreshes: one rotation, by the household.
    supervisor.refresh_household().await.unwrap();
    assert_eq!(
        summary(&live.chatgpt.refreshes()),
        [("household".to_owned(), RefreshOutcome::Rotated(2))]
    );
    // Codex rewrote the household's file in place: still a private file,
    // still what both links point at.
    let metadata = std::fs::symlink_metadata(&household_file).unwrap();
    assert!(metadata.is_file());
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o777,
        0o600
    );

    // Once their cached token is inside Codex's own window, each user's app
    // server reloads the file instead of refreshing.
    until_inside_codex_window(issued, expires_in).await;
    let before = live.model_generations().len();
    for (runtime, thread) in [(&amy, &amys), (&ben, &bens)] {
        assert_eq!(
            turn(&turns, runtime, thread).await.status,
            TurnStatus::Completed
        );
    }
    assert_eq!(generations(&live, before), [Some(2), Some(2)]);
    assert_eq!(
        live.refreshers(),
        ["household"],
        "only the household refreshed"
    );
    live.assert_nothing_left_the_machine();
}

/// Two users' app servers that both find their token stale and the file
/// unchanged refresh at once with the same refresh token: Codex's own race,
/// which the household's early refresh exists to prevent. The mock accepts
/// only the newest access token, as strictly as the real service could.
#[tokio::test]
async fn simultaneous_refreshes_from_two_app_servers_recover() {
    let Some(live) = Live::start(&["ok"]).await else {
        return;
    };
    let data = tempfile::tempdir().unwrap();
    let expires_in = CODEX_WINDOW + 20;
    let (supervisor, turns, issued) =
        supervisor(&live, data.path(), late_refresh(), expires_in).await;
    let chatgpt = live.chatgpt.clone();
    *live.model.authorizer.lock().unwrap() = Some(Arc::new(move |bearer| {
        bearer.is_some_and(|token| chatgpt.accepts(token, true))
    }));
    let amy = supervisor.runtime(UserId(1)).await.unwrap();
    let ben = supervisor.runtime(UserId(2)).await.unwrap();
    let amys = amy
        .start_thread("Amy's Bot".into(), None)
        .await
        .unwrap()
        .thread
        .id;
    let bens = ben
        .start_thread("Ben's Bot".into(), None)
        .await
        .unwrap()
        .thread
        .id;
    for (runtime, thread) in [(&amy, &amys), (&ben, &bens)] {
        assert_eq!(
            turn(&turns, runtime, thread).await.status,
            TurnStatus::Completed
        );
    }

    // Both now find their cached token inside the window and the file
    // unchanged; the mock answers neither until both have asked.
    until_inside_codex_window(issued, expires_in).await;
    live.chatgpt.hold_refreshes_until(2);
    let (first, second) = tokio::join!(turn(&turns, &amy, &amys), turn(&turns, &ben, &bens));
    let raced = summary(&live.chatgpt.refreshes());
    eprintln!(
        "simultaneous refresh: {raced:?}; turns {:?} and {:?}; recoveries {}",
        first.status,
        second.status,
        supervisor.recoveries()
    );
    // Both refreshed with the same token: one rotated it, the other was
    // refused as a reuse...
    let mut outcomes: Vec<_> = raced.iter().map(|(_, outcome)| *outcome).collect();
    outcomes.sort_by_key(|o| format!("{o:?}"));
    assert_eq!(
        outcomes,
        [RefreshOutcome::Reused, RefreshOutcome::Rotated(2)]
    );
    let mut callers: Vec<_> = raced.iter().map(|(who, _)| who.clone()).collect();
    callers.sort();
    assert_eq!(callers, ["user-1", "user-2"]);
    // ...and still both turns completed: the loser read the winner's token
    // from the shared file when the model refused its old one.
    assert_eq!(first.status, TurnStatus::Completed, "{first:?}");
    assert_eq!(second.status, TurnStatus::Completed, "{second:?}");
    assert_eq!(supervisor.recoveries(), 0, "no restart was needed");

    // Every app server is signed in afterwards: both users' next turns run
    // on the newest token, and the household is signed in and refreshes.
    let before = live.model_generations().len();
    for (runtime, thread) in [(&amy, &amys), (&ben, &bens)] {
        assert_eq!(
            turn(&turns, runtime, thread).await.status,
            TurnStatus::Completed
        );
    }
    assert_eq!(generations(&live, before), [Some(2), Some(2)]);
    assert!(supervisor.household_status(true).await.signed_in);
    supervisor.refresh_household().await.unwrap();
    let after = live.chatgpt.refreshes();
    let household: Vec<_> = after[raced.len()..].iter().map(|r| r.outcome).collect();
    // The household read the winner's tokens from the file and needed no
    // refresh of its own, or refreshed them; either way nothing was reused.
    assert!(
        household
            .iter()
            .all(|o| matches!(o, RefreshOutcome::Rotated(_))),
        "{household:?}"
    );
    live.assert_nothing_left_the_machine();
}

/// After downtime the household's token has expired. The household app
/// server starts first and refreshes once; the users' app servers started
/// after it find a fresh token and make no refresh request.
#[tokio::test]
async fn a_cold_start_with_an_expired_token_refreshes_once_in_the_household() {
    let Some(live) = Live::start(&["ok"]).await else {
        return;
    };
    let data = tempfile::tempdir().unwrap();
    let (supervisor, turns, _) =
        supervisor(&live, data.path(), HouseholdSettings::default(), -60).await;
    let amy = supervisor.runtime(UserId(1)).await.unwrap();
    let ben = supervisor.runtime(UserId(2)).await.unwrap();
    assert_eq!(
        summary(&live.chatgpt.refreshes()),
        [("household".to_owned(), RefreshOutcome::Rotated(2))]
    );
    for runtime in [&amy, &ben] {
        let thread = runtime
            .start_thread("Bot".into(), None)
            .await
            .unwrap()
            .thread
            .id;
        assert_eq!(
            turn(&turns, runtime, &thread).await.status,
            TurnStatus::Completed
        );
    }
    assert_eq!(live.model_generations(), [Some(2), Some(2)]);
    assert_eq!(live.refreshers(), ["household"]);
    live.assert_nothing_left_the_machine();
}

/// A user's app server whose model calls are refused with 401, again and
/// again. This records what the pinned Codex does, and that the gateway
/// restarts it once and no more.
#[tokio::test]
async fn a_member_given_repeated_401s() {
    let Some(live) = Live::start(&["ok"]).await else {
        return;
    };
    let data = tempfile::tempdir().unwrap();
    let (supervisor, turns, _) =
        supervisor(&live, data.path(), HouseholdSettings::default(), 86_400).await;
    let amy = supervisor.runtime(UserId(1)).await.unwrap();
    let thread = amy
        .start_thread("Bot".into(), None)
        .await
        .unwrap()
        .thread
        .id;
    *live.model.unauthorized.lock().unwrap() = u32::MAX;
    let failed = turn(&turns, &amy, &thread).await;
    let refreshes = summary(&live.chatgpt.refreshes());
    eprintln!(
        "repeated 401: turn {:?} with {:?}; model asked {} times; refreshes {refreshes:?}",
        failed.status,
        failed.error,
        live.model.bearers.lock().unwrap().len()
    );
    // What Codex 0.163.0 does: on each 401 it reloads the file, then, the
    // file unchanged, refreshes the shared token itself; every refresh it
    // makes rotates it, and the turn fails as unauthorised.
    assert_eq!(failed.status, TurnStatus::Failed);
    assert!(
        failed.error.as_ref().is_some_and(|e| e.is_unauthorized()),
        "{failed:?}"
    );
    assert!(!refreshes.is_empty());
    assert!(
        refreshes
            .iter()
            .all(|(who, outcome)| who == "user-1" && matches!(outcome, RefreshOutcome::Rotated(_))),
        "{refreshes:?}"
    );
    // Its rotations went to the shared file, so the household holds the
    // newest token and is still signed in.
    assert!(supervisor.household_status(true).await.signed_in);

    // The gateway restarts it once.
    let deadline = Instant::now() + Duration::from_secs(30);
    while supervisor.recoveries() == 0 {
        assert!(Instant::now() < deadline, "no restart");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!amy.is_alive());
    let restarted = supervisor.runtime(UserId(1)).await.unwrap();
    restarted
        .resume_thread(&thread, "Bot".into())
        .await
        .unwrap();
    // Still refused, it fails again and is not restarted again.
    let again = turn(&turns, &restarted, &thread).await;
    assert_eq!(again.status, TurnStatus::Failed);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(supervisor.recoveries(), 1);
    assert!(restarted.is_alive());
    // Once the model accepts it again, its turns run on the newest token,
    // and the household's next refresh finds nothing reused.
    *live.model.unauthorized.lock().unwrap() = 0;
    let before = live.model_generations().len();
    let ok = turn(&turns, &restarted, &thread).await;
    assert_eq!(ok.status, TurnStatus::Completed);
    assert_eq!(
        generations(&live, before),
        [Some(live.chatgpt.generation())]
    );
    let refreshed = live.chatgpt.refreshes().len();
    supervisor.refresh_household().await.unwrap();
    assert!(
        live.chatgpt.refreshes()[refreshed..]
            .iter()
            .all(|r| r.outcome != RefreshOutcome::Reused)
    );
    live.assert_nothing_left_the_machine();
}

/// A user's own earlier sign-in is signed out through the user's own app
/// server, which revokes it at the (mock) revoke endpoint, before the link
/// replaces it; a user's `auth.json` that is anything else is refused.
#[tokio::test]
async fn an_earlier_own_sign_in_is_revoked_and_other_files_are_refused() {
    let Some(live) = Live::start(&["ok"]).await else {
        return;
    };
    let data = tempfile::tempdir().unwrap();
    // Amy signed in her own app server before the household existed.
    let amy = UserId(1);
    let amys = data.path().join("users/1/codex");
    std::fs::create_dir_all(&amys).unwrap();
    live.chatgpt.sign_in(&amys, 86_400);
    let (supervisor, turns, _) =
        supervisor(&live, data.path(), HouseholdSettings::default(), 86_400).await;
    supervisor.sign_out_own_sign_ins().await;
    assert_eq!(live.chatgpt.revoked(), ["synthetic-refresh-1"]);
    let link = supervisor.paths(amy).auth_json();
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        std::path::Path::new(LINK_TARGET)
    );
    // The household's sign-in (the second generation) is untouched.
    let runtime = supervisor.runtime(amy).await.unwrap();
    let thread = runtime
        .start_thread("Bot".into(), None)
        .await
        .unwrap()
        .thread
        .id;
    assert_eq!(
        turn(&turns, &runtime, &thread).await.status,
        TurnStatus::Completed
    );
    assert_eq!(live.model_generations(), [Some(2)]);

    // Ben's auth.json is a link to some other file: refused, and never
    // started, so nothing of his is signed in or out.
    let ben = UserId(2);
    let bens = supervisor.paths(ben).codex_home;
    std::fs::create_dir_all(&bens).unwrap();
    let elsewhere = data.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    live.chatgpt.sign_in(&elsewhere, 86_400);
    std::os::unix::fs::symlink(elsewhere.join("auth.json"), bens.join("auth.json")).unwrap();
    match supervisor.runtime(ben).await {
        Err(CodexError::Refused(refusal)) => {
            assert!(
                refusal.0.contains("not the household's sign-in"),
                "{refusal}"
            )
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(
        !bens.join("config.toml").exists(),
        "Ben's app server never started"
    );
    assert_eq!(live.chatgpt.revoked().len(), 1);
    live.assert_nothing_left_the_machine();
}

/// Over the API: a member cannot sign the household in or out; an admin's
/// sign-out revokes the shared sign-in, stops every user's app server, and
/// leaves members refused with a clear error.
#[tokio::test]
async fn signing_the_household_out_signs_everyone_out() {
    let Some(live) = Live::start(&["ok"]).await else {
        return;
    };
    let gateway = Gateway::live(&live).await;
    gateway.route().await;
    let rob = gateway.admin("rob").await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    for call in [
        Call::post("/api/v1/codex/login"),
        Call::post("/api/v1/codex/logout"),
    ] {
        let reply = gateway.send(call.cookie(&amy)).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
    }
    let account = gateway
        .send(Call::get("/api/v1/codex/account").cookie(&amy))
        .await;
    assert_eq!(account.body, json!({"signed_in": true}));
    let admin_view = gateway
        .send(Call::get("/api/v1/codex/account").cookie(&rob))
        .await;
    assert_eq!(
        admin_view.body,
        json!({"signed_in": true, "plan": "pro", "email": "household@example.test"})
    );
    let bot = gateway
        .send(Call::post("/api/v1/bots").cookie(&amy).json(&json!({
            "name": "Chef", "label": "", "description": ""})))
        .await;
    let bot = bot.body["id"].as_i64().unwrap();
    let path = format!("/api/v1/bots/{bot}/messages");
    let sent = gateway
        .send(Call::post(&path).cookie(&amy).json(&json!({"text": "Hi"})))
        .await;
    assert_eq!(sent.status, StatusCode::OK, "{:?}", sent.body);
    let running = gateway
        .state
        .supervisor
        .running(UserId(amy_id))
        .await
        .unwrap();

    let out = gateway
        .send(Call::post("/api/v1/codex/logout").cookie(&rob))
        .await;
    assert_eq!(out.status, StatusCode::NO_CONTENT, "{:?}", out.body);
    assert_eq!(live.chatgpt.revoked(), ["synthetic-refresh-1"]);
    assert!(
        !gateway
            .state
            .supervisor
            .household_paths()
            .auth_json()
            .exists()
    );
    assert!(!running.is_alive());
    let refused = gateway
        .send(
            Call::post(&path)
                .cookie(&amy)
                .json(&json!({"text": "Again?"})),
        )
        .await;
    assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        refused.body["message"]
            .as_str()
            .unwrap()
            .starts_with("ChatGPT is signed out for the household")
    );
    let account = gateway
        .send(Call::get("/api/v1/codex/account").cookie(&rob))
        .await;
    assert_eq!(account.body, json!({"signed_in": false}));
    live.assert_nothing_left_the_machine();
}
