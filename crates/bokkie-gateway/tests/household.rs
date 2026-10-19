//! The household's shared ChatGPT sign-in, against fake app servers: who may
//! sign in and out, the pending sign-in's state, the links, cold starts,
//! refreshes and recovery. `live_household.rs` proves the same mechanism
//! with the real pinned app server.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use bokkie_gateway::codex::household::LINK_TARGET;
use bokkie_gateway::codex::{CodexError, Owner};
use bokkie_protocol::UserId;
use common::{Call, Gateway, fake_codex};
use serde_json::{Value, json};

const FIFTEEN_MINUTES_MS: i64 = 15 * 60 * 1000;

fn household_auth(gateway: &Gateway) -> std::path::PathBuf {
    gateway.state.supervisor.household_paths().auth_json()
}

fn user_auth(gateway: &Gateway, user: UserId) -> std::path::PathBuf {
    gateway.state.supervisor.paths(user).auth_json()
}

fn methods_to(gateway: &Gateway, method: &str) -> Vec<Owner> {
    gateway
        .fake
        .received(method)
        .into_iter()
        .map(|r| r.owner)
        .collect()
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

async fn create_bot(gateway: &Gateway, cookie: &str) -> i64 {
    let bot = gateway
        .send(Call::post("/api/v1/bots").cookie(cookie).json(&json!({
            "name": "Chef", "label": "", "description": ""})))
        .await;
    assert_eq!(bot.status, StatusCode::CREATED, "{:?}", bot.body);
    bot.body["id"].as_i64().unwrap()
}

async fn say(gateway: &Gateway, cookie: &str, bot: i64, text: &str) -> common::Reply {
    gateway
        .send(
            Call::post(&format!("/api/v1/bots/{bot}/messages"))
                .cookie(cookie)
                .json(&json!({ "text": text })),
        )
        .await
}

#[tokio::test]
async fn members_see_only_whether_the_household_is_signed_in() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    let (amy, _) = gateway.member(&admin, "amy").await;
    let member = gateway
        .send(Call::get("/api/v1/codex/account").cookie(&amy))
        .await;
    assert_eq!(member.status, StatusCode::OK);
    assert_eq!(member.body, json!({"signed_in": true}), "no email or plan");
    let admin_view = gateway
        .send(Call::get("/api/v1/codex/account").cookie(&admin))
        .await;
    assert_eq!(
        admin_view.body,
        json!({"signed_in": true, "plan": "pro", "email": "owner@example.test"})
    );
    // The household's app server answered, never a user's.
    assert_eq!(methods_to(&gateway, "account/read"), [Owner::Household]);
    assert_eq!(gateway.fake.starts(UserId(2)), 0);

    // Signed out, everyone sees signed out.
    std::fs::remove_file(household_auth(&gateway)).unwrap();
    for cookie in [&amy, &admin] {
        let reply = gateway
            .send(Call::get("/api/v1/codex/account").cookie(cookie))
            .await;
        assert_eq!(reply.body, json!({"signed_in": false}));
    }
    let anonymous = gateway.send(Call::get("/api/v1/codex/account")).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn members_cannot_sign_the_household_in_or_out() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    let (amy, _) = gateway.member(&admin, "amy").await;
    for call in [
        Call::post("/api/v1/codex/login"),
        Call::get("/api/v1/codex/login"),
        Call::new(Method::DELETE, "/api/v1/codex/login"),
        Call::post("/api/v1/codex/logout"),
    ] {
        let reply = gateway.send(call.cookie(&amy)).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{:?}", reply.body);
        assert_eq!(reply.code(), "forbidden");
    }
    assert!(gateway.fake.received("account/login/start").is_empty());
    assert!(gateway.fake.received("account/logout").is_empty());
    assert!(household_auth(&gateway).exists());
}

#[tokio::test]
async fn an_admin_signs_the_household_in_by_device_code() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    let now = gateway.state.clock.now().0;
    let none = gateway
        .send(Call::get("/api/v1/codex/login").cookie(&admin))
        .await;
    assert_eq!(none.status, StatusCode::NOT_FOUND);

    let started = gateway
        .send(Call::post("/api/v1/codex/login").cookie(&admin))
        .await;
    assert_eq!(started.status, StatusCode::OK, "{:?}", started.body);
    assert_eq!(
        started.body,
        json!({"verification_url": "https://auth.example.test/device",
               "user_code": "ABCD-EFGH", "state": "pending",
               "expires_at": now + FIFTEEN_MINUTES_MS})
    );
    let starts = gateway.fake.received("account/login/start");
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].owner, Owner::Household);
    assert_eq!(starts[0].params, json!({"type": "chatgptDeviceCode"}));
    let login_id = gateway
        .state
        .supervisor
        .login(gateway.state.clock.now())
        .unwrap()
        .login_id;

    // Codex reports the sign-in complete.
    gateway.fake.inject_into(
        Owner::Household,
        json!({"method": "account/login/completed", "params": {
            "loginId": login_id, "success": true, "error": null}}),
    );
    let done = wait_for_state(&gateway, &admin, "completed").await;
    assert!(done.get("error").is_none(), "{done}");
}

async fn wait_for_state(gateway: &Gateway, admin: &str, state: &str) -> Value {
    gateway
        .wait_for(admin, "/api/v1/codex/login", |b| b["state"] == state)
        .await
}

#[tokio::test]
async fn a_pending_sign_in_expires_fails_or_is_cancelled() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;

    // Unused for 15 minutes, the code has expired.
    gateway
        .send(Call::post("/api/v1/codex/login").cookie(&admin))
        .await;
    gateway.state.clock.advance(FIFTEEN_MINUTES_MS - 1);
    wait_for_state(&gateway, &admin, "pending").await;
    gateway.state.clock.advance(1);
    wait_for_state(&gateway, &admin, "expired").await;
    // So does one that Codex gives up on.
    gateway
        .send(Call::post("/api/v1/codex/login").cookie(&admin))
        .await;
    let login_id = gateway
        .state
        .supervisor
        .login(gateway.state.clock.now())
        .unwrap()
        .login_id;
    gateway.fake.inject_into(
        Owner::Household,
        json!({"method": "account/login/completed", "params": {
            "loginId": login_id, "success": false,
            "error": "device auth timed out after 15 minutes"}}),
    );
    wait_for_state(&gateway, &admin, "expired").await;

    // A failure says why.
    gateway
        .send(Call::post("/api/v1/codex/login").cookie(&admin))
        .await;
    let login_id = gateway
        .state
        .supervisor
        .login(gateway.state.clock.now())
        .unwrap()
        .login_id;
    gateway.fake.inject_into(
        Owner::Household,
        json!({"method": "account/login/completed", "params": {
            "loginId": login_id, "success": false, "error": "the code was refused"}}),
    );
    let failed = wait_for_state(&gateway, &admin, "failed").await;
    assert_eq!(failed["error"], "the code was refused");

    // Cancelling a pending sign-in cancels it in Codex too, and its later
    // report changes nothing.
    gateway
        .send(Call::post("/api/v1/codex/login").cookie(&admin))
        .await;
    let login_id = gateway
        .state
        .supervisor
        .login(gateway.state.clock.now())
        .unwrap()
        .login_id;
    let cancelled = gateway
        .send(Call::new(Method::DELETE, "/api/v1/codex/login").cookie(&admin))
        .await;
    assert_eq!(cancelled.status, StatusCode::OK);
    assert_eq!(cancelled.body["state"], "cancelled");
    let cancels = gateway.fake.received("account/login/cancel");
    assert_eq!(cancels.len(), 1);
    assert_eq!(cancels[0].owner, Owner::Household);
    assert_eq!(cancels[0].params["loginId"], login_id);
    tokio::time::sleep(Duration::from_millis(50)).await;
    wait_for_state(&gateway, &admin, "cancelled").await;

    // Starting a new one cancels the pending one.
    gateway
        .send(Call::post("/api/v1/codex/login").cookie(&admin))
        .await;
    gateway
        .send(Call::post("/api/v1/codex/login").cookie(&admin))
        .await;
    assert_eq!(gateway.fake.received("account/login/cancel").len(), 2);
    wait_for_state(&gateway, &admin, "pending").await;

    // If the household's app server stops, the pending sign-in has failed.
    gateway.fake.crash_owner(Owner::Household);
    let household = gateway.state.supervisor.household().await.unwrap();
    let _ = household.rate_limits().await;
    let failed = wait_for_state(&gateway, &admin, "failed").await;
    assert_eq!(failed["error"], "The household's app server stopped");
}

#[tokio::test]
async fn an_admin_signs_the_household_out_for_everyone() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    let (amy, amy_id) = gateway.member(&admin, "amy").await;
    let bot = create_bot(&gateway, &amy).await;
    assert_eq!(say(&gateway, &amy, bot, "Hi").await.status, StatusCode::OK);
    let amys = gateway
        .state
        .supervisor
        .running(UserId(amy_id))
        .await
        .unwrap();

    let reply = gateway
        .send(Call::post("/api/v1/codex/logout").cookie(&admin))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{:?}", reply.body);
    assert_eq!(methods_to(&gateway, "account/logout"), [Owner::Household]);
    assert!(!household_auth(&gateway).exists());
    // Every user's app server held the revoked sign-in: all stopped.
    assert!(!amys.is_alive());

    // Signed out, nothing runs: no member app server starts, and a message
    // is refused with a clear error, not stored.
    let starts = gateway.fake.starts(UserId(amy_id));
    let path = format!("/api/v1/bots/{bot}/messages");
    let stored = gateway.send(Call::get(&path).cookie(&amy)).await.body["messages"]
        .as_array()
        .unwrap()
        .len();
    let sent = say(&gateway, &amy, bot, "Anyone there?").await;
    assert_eq!(sent.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(sent.code(), "unavailable");
    assert!(
        sent.body["message"]
            .as_str()
            .unwrap()
            .starts_with("ChatGPT is signed out for the household"),
        "{:?}",
        sent.body
    );
    assert_eq!(gateway.fake.starts(UserId(amy_id)), starts);
    let after = gateway.send(Call::get(&path).cookie(&amy)).await;
    assert_eq!(
        after.body["messages"].as_array().unwrap().len(),
        stored,
        "nothing stored"
    );
    let account = gateway
        .send(Call::get("/api/v1/codex/account").cookie(&amy))
        .await;
    assert_eq!(account.body, json!({"signed_in": false}));
    // The meter still answers, without the subscription's windows.
    let usage = gateway.send(Call::get("/api/v1/usage").cookie(&amy)).await;
    assert_eq!(usage.status, StatusCode::OK);
    assert_eq!(usage.body["subscription"], json!([]));
}

#[tokio::test]
async fn each_users_auth_json_is_a_link_to_the_households() {
    let gateway = Gateway::new().await;
    for user in [UserId(1), UserId(2)] {
        gateway.state.supervisor.runtime(user).await.unwrap();
        let auth = user_auth(&gateway, user);
        assert!(std::fs::symlink_metadata(&auth).unwrap().is_symlink());
        assert_eq!(
            std::fs::read_link(&auth).unwrap(),
            std::path::PathBuf::from(LINK_TARGET)
        );
        assert_eq!(
            std::fs::canonicalize(&auth).unwrap(),
            std::fs::canonicalize(household_auth(&gateway)).unwrap()
        );
    }
    // The household's file stays private, as Codex writes it.
    let mode = std::fs::metadata(household_auth(&gateway))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    for dir in [
        gateway.state.supervisor.household_paths().root,
        gateway.state.supervisor.household_paths().codex_home,
    ] {
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{}", dir.display());
    }
    // The household app server started first, once, and runs no threads.
    assert_eq!(gateway.fake.owner_starts(Owner::Household), 1);
    let household = gateway.state.supervisor.household().await.unwrap();
    assert!(matches!(
        household.start_thread("x".into(), None).await,
        Err(CodexError::Refused(_))
    ));
    assert!(gateway.fake.received("thread/start").is_empty());
}

#[tokio::test]
async fn an_app_server_whose_auth_json_is_not_the_link_is_refused() {
    // A link to somewhere else, and a directory, are refused, and no app
    // server starts.
    let gateway = Gateway::new().await;
    let elsewhere = gateway.dir.path().join("elsewhere.json");
    std::fs::write(&elsewhere, fake_codex::household_auth(3600)).unwrap();
    let amy = UserId(1);
    std::fs::create_dir_all(gateway.state.supervisor.paths(amy).codex_home).unwrap();
    std::os::unix::fs::symlink(&elsewhere, user_auth(&gateway, amy)).unwrap();
    let ben = UserId(2);
    std::fs::create_dir_all(user_auth(&gateway, ben)).unwrap();
    for user in [amy, ben] {
        match gateway.state.supervisor.runtime(user).await {
            Err(CodexError::Refused(refusal)) => {
                assert!(
                    refusal.0.contains("not the household's sign-in"),
                    "{refusal}"
                )
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert_eq!(gateway.fake.starts(user), 0);
    }
    assert!(gateway.fake.received("account/logout").is_empty());

    // A link replaced while the app server runs is refused at the next use.
    let cara = UserId(3);
    let runtime = gateway.state.supervisor.runtime(cara).await.unwrap();
    let auth = user_auth(&gateway, cara);
    std::fs::remove_file(&auth).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &auth).unwrap();
    assert!(matches!(
        gateway.state.supervisor.runtime(cara).await,
        Err(CodexError::Refused(_))
    ));
    assert!(!runtime.is_alive());
}

#[tokio::test]
async fn a_users_own_earlier_sign_in_is_signed_out_before_the_link_replaces_it() {
    let gateway = Gateway::new().await;
    let amy = UserId(1);
    let paths = gateway.state.supervisor.paths(amy);
    std::fs::create_dir_all(&paths.codex_home).unwrap();
    std::fs::write(paths.auth_json(), fake_codex::household_auth(3600)).unwrap();

    gateway.state.supervisor.runtime(amy).await.unwrap();
    // Signed out through Amy's own app server, which revokes it, and only
    // then linked; her next app server runs on the link.
    let logouts = gateway.fake.received("account/logout");
    assert_eq!(logouts.len(), 1);
    assert_eq!(logouts[0].owner, Owner::User(amy));
    assert_eq!(gateway.fake.starts(amy), 2);
    assert_eq!(
        std::fs::read_link(paths.auth_json()).unwrap(),
        std::path::PathBuf::from(LINK_TARGET)
    );
    assert!(
        household_auth(&gateway).exists(),
        "the household's is untouched"
    );

    // At startup it happens whether or not the household is signed in.
    let gateway = Gateway::new().await;
    std::fs::remove_file(household_auth(&gateway)).unwrap();
    let ben = UserId(2);
    let paths = gateway.state.supervisor.paths(ben);
    std::fs::create_dir_all(&paths.codex_home).unwrap();
    std::fs::write(paths.auth_json(), fake_codex::household_auth(3600)).unwrap();
    gateway.state.supervisor.sign_out_own_sign_ins().await;
    assert_eq!(methods_to(&gateway, "account/logout"), [Owner::User(ben)]);
    assert!(
        std::fs::symlink_metadata(paths.auth_json())
            .unwrap()
            .is_symlink()
    );
    // Its app server ran only to sign out.
    assert!(gateway.state.supervisor.running(ben).await.is_none());
    assert_eq!(gateway.fake.owner_starts(Owner::Household), 0);
}

#[tokio::test]
async fn the_gateway_never_signs_a_users_app_server_in_or_out() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    let (amy, _) = gateway.member(&admin, "amy").await;
    let bot = create_bot(&gateway, &amy).await;
    say(&gateway, &amy, bot, "Hi").await;
    for cookie in [&amy, &admin] {
        gateway
            .send(Call::get("/api/v1/codex/account").cookie(cookie))
            .await;
    }
    gateway
        .send(Call::post("/api/v1/codex/login").cookie(&admin))
        .await;
    gateway
        .send(Call::new(Method::DELETE, "/api/v1/codex/login").cookie(&admin))
        .await;
    gateway
        .send(Call::post("/api/v1/codex/logout").cookie(&admin))
        .await;
    for method in [
        "account/login/start",
        "account/login/cancel",
        "account/logout",
        "account/read",
    ] {
        let owners = methods_to(&gateway, method);
        assert!(!owners.is_empty(), "{method}");
        assert!(
            owners.iter().all(|o| *o == Owner::Household),
            "{method}: {owners:?}"
        );
    }
}

#[tokio::test]
async fn a_cold_start_refreshes_the_household_first() {
    // The household's token is within the refresh window when the gateway
    // starts: the household refreshes before any member's app server.
    let gateway = Gateway::new().await;
    fake_codex::sign_in_household(gateway.dir.path(), 60);
    gateway.state.supervisor.runtime(UserId(1)).await.unwrap();
    let reads = gateway.fake.received("account/read");
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].owner, Owner::Household);
    assert_eq!(reads[0].params["refreshToken"], true);
    // The member's app server started only after the refresh.
    let order: Vec<(Owner, String)> = gateway.fake.with(|b| {
        b.received
            .iter()
            .filter(|r| r.method == "initialize" || r.method == "account/read")
            .map(|r| (r.owner, r.method.clone()))
            .collect()
    });
    assert_eq!(
        order,
        [
            (Owner::Household, "initialize".into()),
            (Owner::Household, "account/read".into()),
            (Owner::User(UserId(1)), "initialize".into()),
        ]
    );
    // Fresh now, it is not refreshed again.
    gateway.state.supervisor.runtime(UserId(2)).await.unwrap();
    assert_eq!(gateway.fake.received("account/read").len(), 1);
}

#[tokio::test]
async fn a_refresh_that_fails_signs_the_household_out_until_one_succeeds() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    let bot = create_bot(&gateway, &admin).await;
    fake_codex::sign_in_household(gateway.dir.path(), 60);
    gateway.fake.with(|b| b.refresh_fails = true);
    let sent = say(&gateway, &admin, bot, "Hi").await;
    assert_eq!(
        sent.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{:?}",
        sent.body
    );
    assert_eq!(gateway.fake.starts(UserId(1)), 0, "no member app server");
    let account = gateway
        .send(Call::get("/api/v1/codex/account").cookie(&admin))
        .await;
    assert_eq!(account.body, json!({"signed_in": false}));
    // Not retried on every request.
    assert_eq!(gateway.fake.received("account/read").len(), 1);
    // A forced refresh that works restores it.
    gateway.fake.with(|b| b.refresh_fails = false);
    gateway.state.supervisor.refresh_household().await.unwrap();
    assert_eq!(
        say(&gateway, &admin, bot, "Hi").await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_member_failing_on_its_credential_is_restarted_once() {
    let gateway = Gateway::new().await;
    let amy = UserId(1);
    let runtime = gateway.state.supervisor.runtime(amy).await.unwrap();
    let thread = runtime
        .start_thread("x".into(), None)
        .await
        .unwrap()
        .thread
        .id;
    let fail = |turn: &str| {
        json!({"method": "turn/completed", "params": {"threadId": thread, "turn": {
            "id": turn, "items": [], "status": "failed", "error": {
                "message": "Your access token could not be refreshed.",
                "codexErrorInfo": "unauthorized", "additionalDetails": null}}}})
    };
    gateway.fake.inject(amy, fail("t1"));
    wait_until(|| gateway.fake.starts(amy) == 2).await;
    assert_eq!(gateway.state.supervisor.recoveries(), 1);
    assert!(!runtime.is_alive());

    // Failing again before any turn completes, it is not restarted again.
    gateway.fake.inject(amy, fail("t2"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(gateway.fake.starts(amy), 2);
    assert_eq!(gateway.state.supervisor.recoveries(), 1);

    // Once a turn completes, a later failure gets its one restart again.
    gateway.fake.inject(
        amy,
        json!({"method": "turn/completed", "params": {"threadId": thread, "turn": {
            "id": "t3", "items": [], "status": "completed", "error": null}}}),
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    gateway.fake.inject(amy, fail("t4"));
    wait_until(|| gateway.fake.starts(amy) == 3).await;

    // A 401 from the model counts as a credential failure too; other
    // failures do not.
    gateway.fake.inject(
        amy,
        json!({"method": "turn/completed", "params": {"threadId": thread, "turn": {
            "id": "t5", "items": [], "status": "completed", "error": null}}}),
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    gateway.fake.inject(
        amy,
        json!({"method": "turn/completed", "params": {"threadId": thread, "turn": {
            "id": "t6", "items": [], "status": "failed", "error": {
                "message": "stream failed", "additionalDetails": null,
                "codexErrorInfo": {"responseStreamDisconnected": {"httpStatusCode": 500}}}}}}),
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(gateway.fake.starts(amy), 3);
    gateway.fake.inject(
        amy,
        json!({"method": "turn/completed", "params": {"threadId": thread, "turn": {
            "id": "t7", "items": [], "status": "failed", "error": {
                "message": "stream failed", "additionalDetails": null,
                "codexErrorInfo": {"responseStreamDisconnected": {"httpStatusCode": 401}}}}}}),
    );
    wait_until(|| gateway.fake.starts(amy) == 4).await;
}

#[tokio::test]
async fn a_member_is_not_restarted_while_the_household_is_signed_out() {
    let gateway = Gateway::new().await;
    let amy = UserId(1);
    let runtime = gateway.state.supervisor.runtime(amy).await.unwrap();
    let thread = runtime
        .start_thread("x".into(), None)
        .await
        .unwrap()
        .thread
        .id;
    std::fs::remove_file(household_auth(&gateway)).unwrap();
    gateway.fake.inject(
        amy,
        json!({"method": "turn/completed", "params": {"threadId": thread, "turn": {
            "id": "t1", "items": [], "status": "failed", "error": {
                "message": "refresh failed", "codexErrorInfo": "unauthorized",
                "additionalDetails": null}}}}),
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(gateway.fake.starts(amy), 1);
    assert_eq!(gateway.state.supervisor.recoveries(), 0);
}
