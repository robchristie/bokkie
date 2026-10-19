//! Accounts, sign-in, sessions, invitations and the request guard.

mod common;

use axum::http::{Method, StatusCode, header};
use bokkie_gateway::accounts;
use bokkie_gateway::auth::LoginLimiter;
use bokkie_gateway::clock::{DAY_MS, HOUR_MS};
use bokkie_protocol::api::{CreateInvitationRequest, Role};
use common::{Call, Gateway, PASSWORD};
use serde_json::json;

#[tokio::test]
async fn bootstrap_creates_one_admin_only() {
    let gateway = Gateway::new().await;
    gateway.admin("rob").await;
    let second = accounts::bootstrap_admin(
        &gateway.state.db,
        gateway.state.clock.now(),
        "eve",
        "Eve",
        PASSWORD,
    )
    .await
    .unwrap_err();
    assert_eq!(second.code, bokkie_protocol::api::ErrorCode::Conflict);
}

#[tokio::test]
async fn sign_in_sets_a_hardened_long_lived_cookie() {
    let gateway = Gateway::new().await;
    let cookie = gateway.admin("rob").await;
    let reply = gateway.login("ROB", PASSWORD).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["user"]["username"], "rob");
    assert_eq!(reply.body["user"]["role"], "admin");
    let set = reply.headers[header::SET_COOKIE].to_str().unwrap();
    for attribute in ["HttpOnly", "Secure", "SameSite=Lax", "Path=/"] {
        assert!(set.contains(attribute), "{set}");
    }
    let max_age: i64 = set
        .split("Max-Age=")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .unwrap()
        .parse()
        .unwrap();
    assert!(max_age >= 365 * 24 * 3600, "{max_age}");

    let me = gateway
        .send(Call::get("/api/v1/session").cookie(&cookie))
        .await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.body["user"]["username"], "rob");
    // The password hash never leaves the gateway.
    assert!(!me.body.to_string().contains("argon2"));
}

#[tokio::test]
async fn wrong_passwords_and_unknown_users_look_the_same() {
    let gateway = Gateway::new().await;
    gateway.admin("rob").await;
    let wrong = gateway.login("rob", "not the password").await;
    let unknown = gateway.login("nobody", "not the password").await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.body, unknown.body);
    assert!(wrong.headers.get(header::SET_COOKIE).is_none());
}

#[tokio::test]
async fn sign_in_is_rate_limited_per_username() {
    let gateway = Gateway::new().await;
    gateway.admin("rob").await;
    for _ in 0..LoginLimiter::PER_ACCOUNT {
        assert_eq!(
            gateway.login("rob", "guess").await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    // Even the right password is refused while the limit holds.
    let limited = gateway.login("rob", PASSWORD).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.code(), "rate_limited");
    gateway.state.clock.advance(LoginLimiter::WINDOW_MS);
    assert_eq!(gateway.login("rob", PASSWORD).await.status, StatusCode::OK);
}

#[tokio::test]
async fn the_host_header_must_name_the_gateway() {
    let gateway = Gateway::new().await;
    for host in [Some("evil.test"), Some("bokkie.test.evil.test"), None] {
        let reply = gateway.send(Call::get("/api/v1/version").host(host)).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{host:?}");
    }
    for host in ["bokkie.test", "BOKKIE.TEST", "bokkie.tailnet.test"] {
        let reply = gateway
            .send(Call::get("/api/v1/version").host(Some(host)))
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{host}");
        assert_eq!(reply.body["protocol"], bokkie_protocol::PROTOCOL_VERSION);
    }
    // The health check works by address.
    let health = gateway
        .send(Call::get("/healthz").host(Some("172.30.0.2:8080")))
        .await;
    assert_eq!(health.status, StatusCode::OK);
}

#[tokio::test]
async fn state_changing_requests_need_the_gateways_origin() {
    let gateway = Gateway::new().await;
    let cookie = gateway.admin("rob").await;
    let body = json!({"username": "rob", "password": PASSWORD});
    for origin in [None, Some("https://evil.test"), Some("http://bokkie.test")] {
        let reply = gateway
            .send(Call::post("/api/v1/session").origin(origin).json(&body))
            .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{origin:?}");
    }
    // Signing out across origins is refused too, and the session survives.
    let logout = gateway
        .send(
            Call::new(Method::DELETE, "/api/v1/session")
                .cookie(&cookie)
                .origin(Some("https://evil.test")),
        )
        .await;
    assert_eq!(logout.status, StatusCode::FORBIDDEN);
    // Reads need no Origin.
    let me = gateway
        .send(Call::get("/api/v1/session").cookie(&cookie).origin(None))
        .await;
    assert_eq!(me.status, StatusCode::OK);
}

#[tokio::test]
async fn malformed_bodies_get_the_api_error_shape() {
    let gateway = Gateway::new().await;
    let reply = gateway
        .send(Call::post("/api/v1/session").raw("{not json"))
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.code(), "invalid_request");
    let missing = gateway.send(Call::get("/api/v1/nothing-here")).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.code(), "not_found");
}

#[tokio::test]
async fn malformed_paths_and_queries_get_the_api_error_shape() {
    let gateway = Gateway::new().await;
    let cookie = gateway.admin("rob").await;
    for path in [
        "/api/v1/bots/chef",
        "/api/v1/bots/1.5/messages",
        "/api/v1/admin/users/rob/usage",
        "/api/v1/bots/1/messages?limit=lots",
    ] {
        let reply = gateway.send(Call::get(path).cookie(&cookie)).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(reply.code(), "invalid_request", "{path}: {:?}", reply.body);
    }
    let reply = gateway
        .send(Call::new(Method::DELETE, "/api/v1/sessions/latest").cookie(&cookie))
        .await;
    assert_eq!(reply.code(), "invalid_request", "{:?}", reply.body);
}

#[tokio::test]
async fn signing_out_revokes_the_session() {
    let gateway = Gateway::new().await;
    let cookie = gateway.admin("rob").await;
    let logout = gateway
        .send(Call::new(Method::DELETE, "/api/v1/session").cookie(&cookie))
        .await;
    assert_eq!(logout.status, StatusCode::NO_CONTENT);
    // The browser drops the very cookie that sign-in set.
    let cleared = logout.headers[header::SET_COOKIE].to_str().unwrap();
    let name = cookie.split('=').next().unwrap();
    assert_eq!(name, bokkie_protocol::SESSION_COOKIE);
    assert!(cleared.starts_with(&format!("{name}=;")), "{cleared}");
    assert!(cleared.contains("Max-Age=0"), "{cleared}");
    let me = gateway
        .send(Call::get("/api/v1/session").cookie(&cookie))
        .await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sessions_expire_only_after_long_idleness() {
    let gateway = Gateway::new().await;
    let cookie = gateway.admin("rob").await;
    let me = || gateway.send(Call::get("/api/v1/session").cookie(&cookie));
    // Regular use keeps the session alive indefinitely.
    for _ in 0..3 {
        gateway.state.clock.advance(300 * DAY_MS);
        assert_eq!(me().await.status, StatusCode::OK);
    }
    gateway
        .state
        .clock
        .advance(accounts::SESSION_IDLE_MS + HOUR_MS);
    assert_eq!(me().await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn invitations_create_accounts_once() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    let invitation = gateway
        .send(Call::post("/api/v1/admin/invitations").cookie(&admin).json(
            &CreateInvitationRequest {
                role: Role::Member,
                permissions: Default::default(),
                display_name: Some("Amy".into()),
                expires_in_hours: Some(2),
            },
        ))
        .await;
    assert_eq!(invitation.status, StatusCode::CREATED);
    let code = invitation.body["code"].as_str().unwrap().to_owned();
    let listed = gateway
        .send(Call::get("/api/v1/admin/invitations").cookie(&admin))
        .await;
    assert_eq!(listed.body["invitations"][0]["display_name"], "Amy");
    // The code itself is shown only once.
    assert!(!listed.body.to_string().contains(&code));

    let accept = |username: &str| {
        gateway.send(Call::post("/api/v1/invitations/accept").json(&json!({
            "code": code, "username": username, "password": PASSWORD, "display_name": "Amy",
        })))
    };
    // A taken username fails without consuming the code.
    let taken = accept("rob").await;
    assert_eq!(taken.status, StatusCode::CONFLICT);
    let accepted = accept("amy").await;
    assert_eq!(accepted.status, StatusCode::OK, "{:?}", accepted.body);
    assert_eq!(accepted.body["user"]["role"], "member");
    let me = gateway
        .send(Call::get("/api/v1/session").cookie(&accepted.cookie()))
        .await;
    assert_eq!(me.body["user"]["username"], "amy");
    // A used code is spent.
    assert_eq!(accept("amy2").await.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn expired_and_withdrawn_invitations_are_refused() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    let create = || {
        gateway.send(
            Call::post("/api/v1/admin/invitations")
                .cookie(&admin)
                .json(&json!({"role": "member", "expires_in_hours": 1})),
        )
    };
    let accept = |code: String| {
        gateway.send(Call::post("/api/v1/invitations/accept").json(&json!({
            "code": code, "username": "amy", "password": PASSWORD, "display_name": "Amy",
        })))
    };
    let expiring = create().await.body["code"].as_str().unwrap().to_owned();
    let withdrawn = create().await;
    let id = withdrawn.body["id"].as_i64().unwrap();
    let removed = gateway
        .send(Call::new(Method::DELETE, &format!("/api/v1/admin/invitations/{id}")).cookie(&admin))
        .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    let reply = accept(withdrawn.body["code"].as_str().unwrap().to_owned()).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    gateway.state.clock.advance(HOUR_MS);
    let reply = accept(expiring).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(reply.code(), "forbidden");
    // A guessed code reads the same.
    assert_eq!(accept("guess".into()).await.body, reply.body);
}

#[tokio::test]
async fn members_cannot_use_admin_routes() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    let (member, _) = gateway.member(&admin, "amy").await;
    let (_, other) = gateway.member(&admin, "ben").await;
    let calls = [
        Call::get("/api/v1/admin/users"),
        Call::get("/api/v1/admin/invitations"),
        Call::post("/api/v1/admin/invitations").json(&json!({"role": "admin"})),
        Call::new(Method::PATCH, &format!("/api/v1/admin/users/{other}"))
            .json(&json!({"disabled": true})),
        Call::new(
            Method::PUT,
            &format!("/api/v1/admin/users/{other}/password"),
        )
        .json(&json!({"new_password": "a takeover password"})),
    ];
    for call in calls {
        let reply = gateway.send(call.cookie(&member)).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{:?}", reply.body);
    }
    // Without a session, admin routes ask for one.
    let reply = gateway.send(Call::get("/api/v1/admin/users")).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    // Ben still signs in with his own password.
    assert_eq!(gateway.login("ben", PASSWORD).await.status, StatusCode::OK);
}

#[tokio::test]
async fn one_user_cannot_see_or_revoke_anothers_sessions() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    let (amy, _) = gateway.member(&admin, "amy").await;
    let (ben, _) = gateway.member(&admin, "ben").await;

    let amy_sessions = gateway
        .send(Call::get("/api/v1/sessions").cookie(&amy))
        .await;
    let sessions = amy_sessions.body["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["current"], true);
    let amy_session = sessions[0]["id"].as_i64().unwrap();

    let ben_sessions = gateway
        .send(Call::get("/api/v1/sessions").cookie(&ben))
        .await;
    let ben_ids: Vec<i64> = ben_sessions.body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_i64().unwrap())
        .collect();
    assert!(!ben_ids.contains(&amy_session));

    // Ben revoking Amy's session reads as not found, and changes nothing.
    let revoke = gateway
        .send(Call::new(Method::DELETE, &format!("/api/v1/sessions/{amy_session}")).cookie(&ben))
        .await;
    assert_eq!(revoke.status, StatusCode::NOT_FOUND);
    let me = gateway
        .send(Call::get("/api/v1/session").cookie(&amy))
        .await;
    assert_eq!(me.status, StatusCode::OK);

    // Amy revoking her own works.
    let revoke = gateway
        .send(Call::new(Method::DELETE, &format!("/api/v1/sessions/{amy_session}")).cookie(&amy))
        .await;
    assert_eq!(revoke.status, StatusCode::NO_CONTENT);
    let me = gateway
        .send(Call::get("/api/v1/session").cookie(&amy))
        .await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn disabling_an_account_ends_its_sessions_and_sign_ins() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    let (amy, amy_id) = gateway.member(&admin, "amy").await;
    let reply = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/admin/users/{amy_id}"))
                .cookie(&admin)
                .json(&json!({"disabled": true, "limits": {"daily_turns": 10, "weekly_tokens": null}})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    assert_eq!(reply.body["user"]["disabled"], true);
    assert_eq!(reply.body["limits"]["daily_turns"], 10);
    let me = gateway
        .send(Call::get("/api/v1/session").cookie(&amy))
        .await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        gateway.login("amy", PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
    // An admin cannot lock themselves out.
    let users = gateway
        .send(Call::get("/api/v1/admin/users").cookie(&admin))
        .await;
    let rob_id = users.body["users"][0]["user"]["id"].as_i64().unwrap();
    let reply = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/admin/users/{rob_id}"))
                .cookie(&admin)
                .json(&json!({"disabled": true})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
}

async fn status(gateway: &Gateway, cookie: &str) -> StatusCode {
    gateway
        .send(Call::get("/api/v1/session").cookie(cookie))
        .await
        .status
}

#[tokio::test]
async fn password_changes_revoke_other_sessions() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    let (amy_phone, amy_id) = gateway.member(&admin, "amy").await;
    let amy_laptop = gateway.login("amy", PASSWORD).await.cookie();

    let new_password = "another long password";
    let wrong = gateway
        .send(
            Call::new(Method::PUT, "/api/v1/session/password")
                .cookie(&amy_phone)
                .json(&json!({"current_password": "wrong", "new_password": new_password})),
        )
        .await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    let changed = gateway
        .send(
            Call::new(Method::PUT, "/api/v1/session/password")
                .cookie(&amy_phone)
                .json(&json!({"current_password": PASSWORD, "new_password": new_password})),
        )
        .await;
    assert_eq!(changed.status, StatusCode::NO_CONTENT);
    assert_eq!(status(&gateway, &amy_phone).await, StatusCode::OK);
    assert_eq!(
        status(&gateway, &amy_laptop).await,
        StatusCode::UNAUTHORIZED
    );

    // An admin reset revokes every session, including the current one.
    let reset = gateway
        .send(
            Call::new(
                Method::PUT,
                &format!("/api/v1/admin/users/{amy_id}/password"),
            )
            .cookie(&admin)
            .json(&json!({"new_password": "a reset long password"})),
        )
        .await;
    assert_eq!(reset.status, StatusCode::NO_CONTENT);
    assert_eq!(status(&gateway, &amy_phone).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        gateway.login("amy", "a reset long password").await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn weak_passwords_and_bad_usernames_are_refused() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    let code = gateway
        .send(
            Call::post("/api/v1/admin/invitations")
                .cookie(&admin)
                .json(&json!({"role": "member"})),
        )
        .await
        .body["code"]
        .as_str()
        .unwrap()
        .to_owned();
    for (username, password) in [
        ("amy", "short"),
        ("a", PASSWORD),
        ("amy smith", PASSWORD),
        ("-amy", PASSWORD),
    ] {
        let reply = gateway
            .send(Call::post("/api/v1/invitations/accept").json(&json!({
                "code": code, "username": username, "password": password, "display_name": "Amy",
            })))
            .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{username}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_sign_in_bursts_cannot_overrun_the_limit() {
    let gateway = Gateway::new().await;
    gateway.admin("rob").await;
    let mut calls: Vec<Call> = (0..30)
        .map(|_| {
            Call::post("/api/v1/session")
                .json(&json!({"username": "rob", "password": "a wrong guess"}))
        })
        .collect();
    calls.push(
        Call::post("/api/v1/session").json(&json!({"username": "rob", "password": PASSWORD})),
    );
    let replies = gateway.send_all(calls).await;
    let checked = replies
        .iter()
        .filter(|r| r.status != StatusCode::TOO_MANY_REQUESTS)
        .count();
    assert!(
        checked <= LoginLimiter::PER_ACCOUNT,
        "{checked} attempts were checked"
    );
    assert!(
        replies
            .iter()
            .all(|r| r.status == StatusCode::TOO_MANY_REQUESTS
                || r.status == StatusCode::UNAUTHORIZED
                || r.status == StatusCode::OK)
    );
}

#[tokio::test]
async fn an_invitation_accepted_twice_at_once_makes_one_account() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    let code = gateway
        .send(
            Call::post("/api/v1/admin/invitations")
                .cookie(&admin)
                .json(&json!({"role": "member"})),
        )
        .await
        .body["code"]
        .as_str()
        .unwrap()
        .to_owned();
    let calls = ["amy", "ben", "cat", "dan"]
        .iter()
        .map(|username| {
            Call::post("/api/v1/invitations/accept").json(&json!({
                "code": code, "username": username, "password": PASSWORD, "display_name": username,
            }))
        })
        .collect();
    let replies = gateway.send_all(calls).await;
    let accepted = replies
        .iter()
        .filter(|r| r.status == StatusCode::OK)
        .count();
    assert_eq!(accepted, 1, "{replies:?}");
    let users = gateway
        .send(Call::get("/api/v1/admin/users").cookie(&admin))
        .await;
    assert_eq!(users.body["users"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn websocket_upgrades_need_the_gateways_origin() {
    let gateway = Gateway::new().await;
    for origin in [None, Some("https://evil.test")] {
        let reply = gateway
            .send(
                Call::get("/api/v1/events")
                    .origin(origin)
                    .header("connection", "upgrade")
                    .header("upgrade", "websocket"),
            )
            .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{origin:?}");
    }
}

#[tokio::test]
async fn wrong_current_passwords_are_rate_limited() {
    let gateway = Gateway::new().await;
    let cookie = gateway.admin("rob").await;
    let change = |current: &str| {
        gateway.send(
            Call::new(Method::PUT, "/api/v1/session/password")
                .cookie(&cookie)
                .json(&json!({"current_password": current, "new_password": "a new long password"})),
        )
    };
    for _ in 0..LoginLimiter::PER_ACCOUNT {
        assert_eq!(change("a wrong guess").await.status, StatusCode::FORBIDDEN);
    }
    assert_eq!(change(PASSWORD).await.status, StatusCode::TOO_MANY_REQUESTS);
    gateway.state.clock.advance(LoginLimiter::WINDOW_MS);
    assert_eq!(change(PASSWORD).await.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn refused_invitation_codes_do_not_block_other_addresses() {
    let gateway = Gateway::new().await;
    let admin = gateway.admin("rob").await;
    let accept = |code: &str| {
        gateway.send(Call::post("/api/v1/invitations/accept").json(&json!({
            "code": code, "username": "amy", "password": PASSWORD, "display_name": "Amy",
        })))
    };
    // Refused codes count against the client address.
    for _ in 0..LoginLimiter::PER_ADDRESS {
        assert_eq!(accept("guess").await.status, StatusCode::FORBIDDEN);
    }
    assert_eq!(accept("guess").await.status, StatusCode::TOO_MANY_REQUESTS);
    // The limit is the address's sign-in limit, not a household-wide one,
    // and it slides.
    assert_eq!(
        gateway.login("rob", "a wrong guess").await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    gateway.state.clock.advance(LoginLimiter::WINDOW_MS);
    let code = gateway
        .send(
            Call::post("/api/v1/admin/invitations")
                .cookie(&admin)
                .json(&json!({"role": "member"})),
        )
        .await
        .body["code"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(accept(&code).await.status, StatusCode::OK);
}

/// How many sign-in failures are stored under `key`.
async fn stored_failures(gateway: &Gateway, key: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM login_failures WHERE key = ?")
        .bind(key)
        .fetch_one(&gateway.state.db)
        .await
        .unwrap()
}

/// Waits until `key` has `count` stored failures. The limiter writes them
/// in order, so later keys are written after earlier ones.
async fn until_stored(gateway: &Gateway, key: &str, count: i64) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while stored_failures(gateway, key).await != count {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{key} never reached {count} stored failures"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn sign_in_limits_survive_a_restart() {
    let gateway = Gateway::new().await;
    gateway.admin("rob").await;
    // A name that is no account, such as a password typed in the wrong
    // field, is limited in memory but never stored.
    assert_eq!(
        gateway.login("hunter2-secret", "guess").await.status,
        StatusCode::UNAUTHORIZED
    );
    for _ in 0..LoginLimiter::PER_ACCOUNT {
        assert_eq!(
            gateway.login("rob", "guess").await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    until_stored(&gateway, "u:rob", LoginLimiter::PER_ACCOUNT as i64).await;
    assert_eq!(stored_failures(&gateway, "u:hunter2-secret").await, 0);

    // A restarted gateway still refuses Rob, until the window passes.
    let db = gateway.state.db.clone();
    let now = gateway.state.clock.now_ms();
    let restarted = LoginLimiter::persistent(db.clone(), now).await.unwrap();
    let attempt = |limiter: &LoginLimiter, at: i64| {
        limiter.begin(
            bokkie_gateway::auth::Attempting::SignIn {
                username: "Rob",
                address: "10.0.0.7",
                account_exists: true,
            },
            at,
        )
    };
    assert!(attempt(&restarted, now).is_none());
    let later = now + LoginLimiter::WINDOW_MS;
    let restarted = LoginLimiter::persistent(db.clone(), later).await.unwrap();
    assert_eq!(
        stored_failures(&gateway, "u:rob").await,
        0,
        "expired rows go"
    );
    drop(attempt(&restarted, later).expect("the window has passed"));
    drop(attempt(&restarted, later).unwrap());
    until_stored(&gateway, "u:rob", 2).await;

    // A success clears the account's stored failures, so a restart after
    // it does not count them again.
    restarted.succeeded(attempt(&restarted, later).unwrap());
    until_stored(&gateway, "u:rob", 0).await;
    let again = LoginLimiter::persistent(db, later).await.unwrap();
    for _ in 0..LoginLimiter::PER_ACCOUNT - 1 {
        drop(attempt(&again, later).unwrap());
    }
    assert!(attempt(&again, later).is_some());
}
