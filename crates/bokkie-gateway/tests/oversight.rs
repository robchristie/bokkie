//! Members' permissions, admins' oversight of members, admin-made Bots and
//! admins' password resets: the isolation each must keep.

mod common;

use axum::http::{Method, StatusCode};
use bokkie_gateway::accounts;
use bokkie_gateway::bots::store;
use bokkie_protocol::api::{Permissions, Role};
use bokkie_protocol::events::{Event, Seq};
use bokkie_protocol::{BotId, UserId};
use common::{Call, Gateway, PASSWORD};
use serde_json::{Value, json};

const NO_PERMISSIONS: Permissions = Permissions {
    may_create_bots: false,
    may_edit_bot_profiles: false,
};

async fn setup() -> (Gateway, String) {
    let gateway = Gateway::new().await;
    gateway.route().await;
    let admin = gateway.admin("rob").await;
    (gateway, admin)
}

async fn create_bot(gateway: &Gateway, cookie: &str, name: &str) -> common::Reply {
    gateway
        .send(Call::post("/api/v1/bots").cookie(cookie).json(&json!({
            "name": name, "label": "Diarist", "description": "Keep my secrets.",
        })))
        .await
}

/// An admin makes a Bot for a member. Returns its id.
async fn make_for(gateway: &Gateway, admin: &str, member: i64, name: &str) -> i64 {
    let reply = gateway
        .send(
            Call::post(&format!("/api/v1/admin/users/{member}/bots"))
                .cookie(admin)
                .json(
                    &json!({"name": name, "label": "Tutor", "description": "Help with maths.",
                              "avatar": "📐"}),
                ),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{:?}", reply.body);
    assert_eq!(reply.body["made_by_admin"], true);
    reply.body["id"].as_i64().unwrap()
}

async fn patch_user(gateway: &Gateway, admin: &str, user: i64, body: Value) -> common::Reply {
    gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/admin/users/{user}"))
                .cookie(admin)
                .json(&body),
        )
        .await
}

async fn oversight(gateway: &Gateway, admin: &str, user: i64, on: bool) -> common::Reply {
    patch_user(gateway, admin, user, json!({"oversight": on})).await
}

/// Says something to a Bot and waits for the reply to complete.
async fn chat(gateway: &Gateway, cookie: &str, bot: i64, text: &str) {
    let reply = gateway
        .send(
            Call::post(&format!("/api/v1/bots/{bot}/messages"))
                .cookie(cookie)
                .json(&json!({"text": text})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    gateway
        .wait_for(cookie, &format!("/api/v1/bots/{bot}/messages"), |body| {
            body["messages"].as_array().is_some_and(|m| {
                m.iter()
                    .any(|m| m["author"] == "bot" && m["state"] == "complete")
            })
        })
        .await;
}

fn bot_names(list: &Value) -> Vec<String> {
    list["bots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap().to_owned())
        .collect()
}

/// Every event in a user's log.
async fn log(gateway: &Gateway, user: i64) -> Vec<Event> {
    gateway
        .state
        .events
        .since(UserId(user), Seq::ZERO, 10_000)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect()
}

async fn session(gateway: &Gateway, cookie: &str) -> common::Reply {
    gateway
        .send(Call::get("/api/v1/session").cookie(cookie))
        .await
}

#[tokio::test]
async fn existing_accounts_keep_todays_behaviour_after_the_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bokkie.sqlite3");
    // A database as the gateway left it before this migration.
    {
        let before = sqlx::migrate::Migrator {
            migrations: std::borrow::Cow::Owned(
                bokkie_gateway::db::MIGRATIONS
                    .iter()
                    .filter(|m| m.version < 6)
                    .cloned()
                    .collect(),
            ),
            ..sqlx::migrate::Migrator::DEFAULT
        };
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let pool = sqlx::SqlitePool::connect_with(options).await.unwrap();
        before.run(&pool).await.unwrap();
        for statement in [
            "INSERT INTO users (id, username, display_name, role, password_hash, created_at,
                                updated_at) VALUES (1, 'rob', 'Rob', 'admin', 'x', 1, 1)",
            "INSERT INTO users (id, username, display_name, role, password_hash, created_at,
                                updated_at) VALUES (2, 'amy', 'Amy', 'member', 'x', 1, 1)",
            "INSERT INTO invitations (code_hash, role, created_by, created_at, expires_at)
                 VALUES (x'00', 'member', 1, 1, 9999999999999)",
            "INSERT INTO bots (id, user_id, name, label, description, created_at, updated_at)
                 VALUES (5, 2, 'Diary', '', '', 1, 1)",
        ] {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }
        pool.close().await;
    }
    let db = bokkie_gateway::db::open(&path).await.unwrap();
    for id in [1, 2] {
        let account = accounts::account(&db, UserId(id)).await.unwrap().unwrap();
        assert_eq!(account.user.permissions, Permissions::default(), "{id}");
        assert!(!account.user.oversight);
        assert_eq!(account.user.oversight_changed_at, None);
        assert_eq!(account.password_reset_by_admin, None);
    }
    let invitations = accounts::admin_list_invitations(&db, bokkie_protocol::Timestamp(2))
        .await
        .unwrap();
    assert_eq!(invitations[0].permissions, Permissions::default());
    let bot = store::bot(&db, UserId(2), BotId(5)).await.unwrap().unwrap();
    assert_eq!(bot.made_by, None, "a member's existing Bot is their own");
}

#[tokio::test]
async fn invitations_carry_permissions_and_admins_always_have_both() {
    let (gateway, rob) = setup().await;
    let invitation =
        gateway
            .send(Call::post("/api/v1/admin/invitations").cookie(&rob).json(
                &json!({"role": "member",
                              "permissions": {"may_create_bots": false,
                                              "may_edit_bot_profiles": true}}),
            ))
            .await;
    assert_eq!(invitation.status, StatusCode::CREATED);
    let listed = gateway
        .send(Call::get("/api/v1/admin/invitations").cookie(&rob))
        .await;
    assert_eq!(
        listed.body["invitations"][0]["permissions"],
        json!({"may_create_bots": false, "may_edit_bot_profiles": true})
    );
    let accepted = gateway
        .send(Call::post("/api/v1/invitations/accept").json(&json!({
            "code": invitation.body["code"], "username": "amy", "password": PASSWORD,
            "display_name": "Amy",
        })))
        .await;
    assert_eq!(accepted.status, StatusCode::OK);
    assert_eq!(
        accepted.body["user"]["permissions"],
        json!({"may_create_bots": false, "may_edit_bot_profiles": true})
    );
    assert_eq!(accepted.body["user"]["oversight"], false);
    let amy = accepted.cookie();
    assert_eq!(
        create_bot(&gateway, &amy, "Diary").await.status,
        StatusCode::FORBIDDEN
    );

    // An admin's invitation, or an admin, cannot lose a permission.
    let refused = gateway
        .send(
            Call::post("/api/v1/admin/invitations")
                .cookie(&rob)
                .json(&json!({"role": "admin", "permissions": NO_PERMISSIONS})),
        )
        .await;
    assert_eq!(
        refused.status,
        StatusCode::BAD_REQUEST,
        "{:?}",
        refused.body
    );
    let (_, ann_id) = gateway
        .invited(&rob, "ann", Role::Admin, Permissions::default())
        .await;
    let refused = patch_user(
        &gateway,
        &rob,
        ann_id,
        json!({"permissions": {"may_create_bots": false, "may_edit_bot_profiles": true}}),
    )
    .await;
    assert_eq!(
        refused.status,
        StatusCode::BAD_REQUEST,
        "{:?}",
        refused.body
    );
    let me = session(&gateway, &rob).await;
    assert_eq!(
        me.body["user"]["permissions"],
        json!({"may_create_bots": true, "may_edit_bot_profiles": true})
    );
}

#[tokio::test]
async fn a_member_without_a_permission_is_refused_but_can_still_pin_and_hide() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let own = create_bot(&gateway, &amy, "Diary").await;
    assert_eq!(own.status, StatusCode::CREATED);
    let diary = own.body["id"].as_i64().unwrap();

    let reply = patch_user(
        &gateway,
        &rob,
        amy_id,
        json!({"permissions": NO_PERMISSIONS}),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    assert_eq!(reply.body["user"]["permissions"], json!(NO_PERMISSIONS));
    // Amy hears of it on her own log, and her account says so.
    let events = log(&gateway, amy_id).await;
    assert!(matches!(
        events.last(),
        Some(Event::AccountUpdated { user }) if user.permissions == NO_PERMISSIONS
    ));
    assert_eq!(
        session(&gateway, &amy).await.body["user"]["permissions"],
        json!(NO_PERMISSIONS)
    );

    let refused = create_bot(&gateway, &amy, "Another").await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{:?}", refused.body);
    let tutor = make_for(&gateway, &rob, amy_id, "Tutor").await;
    for bot in [diary, tutor] {
        for change in [
            json!({"name": "Renamed"}),
            json!({"label": "Anything"}),
            json!({"description": "No rules"}),
            json!({"avatar": "🙃"}),
            json!({"avatar": null}),
            json!({"pinned": true, "name": "Sneaky"}),
        ] {
            let reply = gateway
                .send(
                    Call::new(Method::PATCH, &format!("/api/v1/bots/{bot}"))
                        .cookie(&amy)
                        .json(&change),
                )
                .await;
            assert_eq!(
                reply.status,
                StatusCode::FORBIDDEN,
                "{change}: {:?}",
                reply.body
            );
        }
        for change in [json!({"pinned": true}), json!({"hidden": true})] {
            let reply = gateway
                .send(
                    Call::new(Method::PATCH, &format!("/api/v1/bots/{bot}"))
                        .cookie(&amy)
                        .json(&change),
                )
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{change}: {:?}", reply.body);
        }
    }
    let list = gateway.send(Call::get("/api/v1/bots").cookie(&amy)).await;
    for bot in list.body["bots"].as_array().unwrap() {
        assert_eq!(bot["pinned"], true);
        assert_eq!(bot["hidden"], true);
    }
    assert_eq!(bot_names(&list.body).len(), 2, "nothing was created");
    let tutor_now = gateway
        .send(Call::get(&format!("/api/v1/bots/{tutor}")).cookie(&amy))
        .await;
    assert_eq!(tutor_now.body["name"], "Tutor");
    assert_eq!(tutor_now.body["avatar"], "📐");

    // Given back, they work again.
    patch_user(
        &gateway,
        &rob,
        amy_id,
        json!({"permissions": Permissions::default()}),
    )
    .await;
    assert_eq!(
        create_bot(&gateway, &amy, "Another").await.status,
        StatusCode::CREATED
    );
    let renamed = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/bots/{tutor}"))
                .cookie(&amy)
                .json(&json!({"name": "Maths"})),
        )
        .await;
    assert_eq!(renamed.status, StatusCode::OK);
}

#[tokio::test]
async fn with_oversight_off_an_admin_sees_and_edits_only_admin_made_bots_and_no_conversation() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let diary = create_bot(&gateway, &amy, "Diary").await.body["id"]
        .as_i64()
        .unwrap();
    chat(&gateway, &amy, diary, "my secret diary entry").await;
    let tutor = make_for(&gateway, &rob, amy_id, "Tutor").await;
    chat(&gateway, &amy, tutor, "my secret maths question").await;

    // The admin-made Bot reached Amy's own log and list, not the admin's.
    assert!(
        log(&gateway, amy_id)
            .await
            .iter()
            .any(|e| matches!(e, Event::BotCreated { bot } if bot.id == BotId(tutor)))
    );
    let rob_id = session(&gateway, &rob).await.body["user"]["id"]
        .as_i64()
        .unwrap();
    assert!(log(&gateway, rob_id).await.is_empty());
    let amy_list = gateway.send(Call::get("/api/v1/bots").cookie(&amy)).await;
    assert_eq!(bot_names(&amy_list.body).len(), 2);
    let rob_list = gateway.send(Call::get("/api/v1/bots").cookie(&rob)).await;
    assert!(bot_names(&rob_list.body).is_empty());

    let list = gateway
        .send(Call::get(&format!("/api/v1/admin/users/{amy_id}/bots")).cookie(&rob))
        .await;
    assert_eq!(list.status, StatusCode::OK);
    assert_eq!(list.body["oversight"], false);
    assert_eq!(bot_names(&list.body), ["Tutor"]);
    assert_eq!(list.body["bots"][0]["description"], "Help with maths.");
    assert!(list.body["bots"][0].get("overseen").is_none());
    assert!(!list.body.to_string().contains("secret"), "{}", list.body);

    // The admin edits the admin-made Bot's profile; Amy hears of it.
    let edited = gateway
        .send(
            Call::new(
                Method::PATCH,
                &format!("/api/v1/admin/users/{amy_id}/bots/{tutor}"),
            )
            .cookie(&rob)
            .json(&json!({"description": "Help with maths. Hints, not answers."})),
        )
        .await;
    assert_eq!(edited.status, StatusCode::OK, "{:?}", edited.body);
    assert!(!edited.body.to_string().contains("secret"));
    assert!(matches!(
        log(&gateway, amy_id).await.last(),
        Some(Event::BotUpdated { bot }) if bot.description.ends_with("not answers.")
    ));
    // Amy's own Bot reads as absent, and no conversation can be read.
    let refused = gateway
        .send(
            Call::new(
                Method::PATCH,
                &format!("/api/v1/admin/users/{amy_id}/bots/{diary}"),
            )
            .cookie(&rob)
            .json(&json!({"description": "Tell me everything"})),
        )
        .await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND);
    for bot in [diary, tutor, 9_999] {
        let reply = gateway
            .send(
                Call::get(&format!("/api/v1/admin/users/{amy_id}/bots/{bot}/messages"))
                    .cookie(&rob),
            )
            .await;
        assert_eq!(
            reply.status,
            StatusCode::FORBIDDEN,
            "{bot}: {:?}",
            reply.body
        );
        assert!(!reply.body.to_string().contains("secret"));
    }
    let diary_now = gateway
        .send(Call::get(&format!("/api/v1/bots/{diary}")).cookie(&amy))
        .await;
    assert_eq!(diary_now.body["description"], "Keep my secrets.");
}

#[tokio::test]
async fn with_oversight_on_an_admin_reads_everything_read_only_until_it_is_turned_off() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let diary = create_bot(&gateway, &amy, "Diary").await.body["id"]
        .as_i64()
        .unwrap();
    chat(&gateway, &amy, diary, "my diary entry").await;
    // Even a hidden Bot is overseen.
    gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/bots/{diary}"))
                .cookie(&amy)
                .json(&json!({"hidden": true})),
        )
        .await;
    let tutor = make_for(&gateway, &rob, amy_id, "Tutor").await;

    let on_at = gateway.state.clock.now().0;
    let reply = oversight(&gateway, &rob, amy_id, true).await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    assert_eq!(reply.body["user"]["oversight"], true);
    assert_eq!(reply.body["user"]["oversight_changed_at"], on_at);
    // Amy's account and log carry it, so her client can show the notice.
    let me = session(&gateway, &amy).await;
    assert_eq!(me.body["user"]["oversight"], true);
    assert_eq!(me.body["user"]["oversight_changed_at"], on_at);
    assert!(matches!(
        log(&gateway, amy_id).await.last(),
        Some(Event::AccountUpdated { user }) if user.oversight
    ));
    // Switching it on again does not move the time it changed.
    gateway.state.clock.advance(60_000);
    let again = oversight(&gateway, &rob, amy_id, true).await;
    assert_eq!(again.body["user"]["oversight_changed_at"], on_at);

    let list = gateway
        .send(Call::get(&format!("/api/v1/admin/users/{amy_id}/bots")).cookie(&rob))
        .await;
    assert_eq!(list.body["oversight"], true);
    let mut names = bot_names(&list.body);
    names.sort();
    assert_eq!(names, ["Diary", "Tutor"]);
    let overseen = list.body["bots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "Diary")
        .unwrap();
    assert_eq!(overseen["made_by_admin"], false);
    assert_eq!(overseen["overseen"]["hidden"], true);
    assert!(overseen["overseen"]["last_message"]["text"].is_string());
    let page = gateway
        .send(
            Call::get(&format!(
                "/api/v1/admin/users/{amy_id}/bots/{diary}/messages"
            ))
            .cookie(&rob),
        )
        .await;
    assert_eq!(page.status, StatusCode::OK, "{:?}", page.body);
    assert!(page.body.to_string().contains("my diary entry"));
    let tutor_page = gateway
        .send(
            Call::get(&format!(
                "/api/v1/admin/users/{amy_id}/bots/{tutor}/messages"
            ))
            .cookie(&rob),
        )
        .await;
    assert_eq!(tutor_page.status, StatusCode::OK);
    // A Bot that is not Amy's reads as absent.
    let (_, ben_id) = gateway.member(&rob, "ben").await;
    let bens = make_for(&gateway, &rob, ben_id, "Bens").await;
    let reply = gateway
        .send(
            Call::get(&format!(
                "/api/v1/admin/users/{amy_id}/bots/{bens}/messages"
            ))
            .cookie(&rob),
        )
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    // While on, the admin may edit any of Amy's Bots' profiles.
    let edited = gateway
        .send(
            Call::new(
                Method::PATCH,
                &format!("/api/v1/admin/users/{amy_id}/bots/{diary}"),
            )
            .cookie(&rob)
            .json(&json!({"label": "Journal"})),
        )
        .await;
    assert_eq!(edited.status, StatusCode::OK, "{:?}", edited.body);
    assert_eq!(edited.body["overseen"]["hidden"], true, "pins stay Amy's");

    // Read-only: the admin cannot send, steer or stop.
    let starts_before = gateway.fake.received("turn/start").len();
    let calls = [
        Call::post(&format!("/api/v1/bots/{diary}/messages")).json(&json!({"text": "hi"})),
        Call::post(&format!("/api/v1/bots/{diary}/stop")),
        Call::post(&format!(
            "/api/v1/admin/users/{amy_id}/bots/{diary}/messages"
        ))
        .json(&json!({"text": "hi"})),
        Call::post(&format!("/api/v1/admin/users/{amy_id}/bots/{diary}/stop")),
    ];
    for call in calls {
        let reply = gateway.send(call.cookie(&rob)).await;
        assert!(
            [StatusCode::NOT_FOUND, StatusCode::METHOD_NOT_ALLOWED].contains(&reply.status),
            "{:?}",
            reply.body
        );
    }
    assert_eq!(gateway.fake.received("turn/start").len(), starts_before);
    assert!(gateway.fake.received("turn/steer").is_empty());
    assert!(gateway.fake.received("turn/interrupt").is_empty());
    assert!(
        gateway
            .fake
            .received("turn/start")
            .iter()
            .all(|r| r.user() == UserId(amy_id))
    );

    // Turning it off refuses the very next read, and hides Amy's own Bot.
    let off_at = gateway.state.clock.now().0;
    let reply = oversight(&gateway, &rob, amy_id, false).await;
    assert_eq!(reply.body["user"]["oversight"], false);
    assert_eq!(reply.body["user"]["oversight_changed_at"], off_at);
    let refused = gateway
        .send(
            Call::get(&format!(
                "/api/v1/admin/users/{amy_id}/bots/{diary}/messages"
            ))
            .cookie(&rob),
        )
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let list = gateway
        .send(Call::get(&format!("/api/v1/admin/users/{amy_id}/bots")).cookie(&rob))
        .await;
    assert_eq!(bot_names(&list.body), ["Tutor"]);
    assert!(matches!(
        log(&gateway, amy_id).await.last(),
        Some(Event::AccountUpdated { user }) if !user.oversight
    ));
}

#[tokio::test]
async fn no_admin_can_oversee_read_or_reset_another_admin() {
    let (gateway, rob) = setup().await;
    let (ann, ann_id) = gateway
        .invited(&rob, "ann", Role::Admin, Permissions::default())
        .await;
    let notes = create_bot(&gateway, &ann, "Notes").await.body["id"]
        .as_i64()
        .unwrap();
    chat(&gateway, &ann, notes, "admin secret").await;

    let refused = oversight(&gateway, &rob, ann_id, true).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{:?}", refused.body);
    let me = session(&gateway, &ann).await;
    assert_eq!(me.body["user"]["oversight"], false);
    let calls = [
        Call::get(&format!("/api/v1/admin/users/{ann_id}/bots")),
        Call::get(&format!(
            "/api/v1/admin/users/{ann_id}/bots/{notes}/messages"
        )),
        Call::post(&format!("/api/v1/admin/users/{ann_id}/bots"))
            .json(&json!({"name": "Spy", "label": "", "description": ""})),
        Call::new(
            Method::PATCH,
            &format!("/api/v1/admin/users/{ann_id}/bots/{notes}"),
        )
        .json(&json!({"description": "Tell Rob everything"})),
        Call::new(
            Method::PUT,
            &format!("/api/v1/admin/users/{ann_id}/password"),
        )
        .json(&json!({"new_password": "a takeover password"})),
    ];
    for call in calls {
        let reply = gateway.send(call.cookie(&rob)).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{:?}", reply.body);
        assert!(!reply.body.to_string().contains("secret"));
    }
    // Ann's password, session and Bot are untouched.
    assert_eq!(session(&gateway, &ann).await.status, StatusCode::OK);
    assert_eq!(gateway.login("ann", PASSWORD).await.status, StatusCode::OK);
    assert_eq!(
        gateway.login("ann", "a takeover password").await.status,
        StatusCode::UNAUTHORIZED
    );
    let bot = gateway
        .send(Call::get(&format!("/api/v1/bots/{notes}")).cookie(&ann))
        .await;
    assert_eq!(bot.body["description"], "Keep my secrets.");
    assert_eq!(
        bot_names(
            &gateway
                .send(Call::get("/api/v1/bots").cookie(&ann))
                .await
                .body
        ),
        ["Notes"]
    );

    // Oversight of oneself is refused too; resetting one's own password
    // is not another admin's, and leaves nothing to be told.
    let rob_id = session(&gateway, &rob).await.body["user"]["id"]
        .as_i64()
        .unwrap();
    assert_eq!(
        oversight(&gateway, &rob, rob_id, true).await.status,
        StatusCode::FORBIDDEN
    );
    let own = gateway
        .send(
            Call::new(
                Method::PUT,
                &format!("/api/v1/admin/users/{rob_id}/password"),
            )
            .cookie(&rob)
            .json(&json!({"new_password": "rob's new password"})),
        )
        .await;
    assert_eq!(own.status, StatusCode::NO_CONTENT);
    let signed_in = gateway.login("rob", "rob's new password").await;
    assert_eq!(signed_in.status, StatusCode::OK);
    assert!(signed_in.body.get("password_reset_by_admin").is_none());
}

#[tokio::test]
async fn a_member_is_told_of_an_admins_reset_at_the_next_sign_in_until_acknowledged() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    // A member's own password change is nothing to be told about.
    let changed = gateway
        .send(
            Call::new(Method::PUT, "/api/v1/session/password")
                .cookie(&amy)
                .json(&json!({"current_password": PASSWORD,
                              "new_password": "amy's own new password"})),
        )
        .await;
    assert_eq!(changed.status, StatusCode::NO_CONTENT);
    assert!(
        session(&gateway, &amy)
            .await
            .body
            .get("password_reset_by_admin")
            .is_none()
    );

    let sessions = gateway
        .send(Call::get("/api/v1/sessions").cookie(&amy))
        .await;
    let old_session = sessions.body["sessions"][0]["id"].as_i64().unwrap();
    gateway.state.clock.advance(3_600_000);
    let reset_at = gateway.state.clock.now().0;
    let reset = gateway
        .send(
            Call::new(
                Method::PUT,
                &format!("/api/v1/admin/users/{amy_id}/password"),
            )
            .cookie(&rob)
            .json(&json!({"new_password": "a password rob chose"})),
        )
        .await;
    assert_eq!(reset.status, StatusCode::NO_CONTENT);
    assert_eq!(
        session(&gateway, &amy).await.status,
        StatusCode::UNAUTHORIZED
    );
    // An acknowledgement racing the reset, from a session it revoked,
    // clears nothing.
    accounts::acknowledge_password_reset(&gateway.state.db, UserId(amy_id), old_session)
        .await
        .unwrap();

    gateway.state.clock.advance(60_000);
    let signed_in = gateway.login("amy", "a password rob chose").await;
    assert_eq!(signed_in.status, StatusCode::OK);
    assert_eq!(signed_in.body["password_reset_by_admin"], reset_at);
    let amy = signed_in.cookie();
    // Until acknowledged, the account keeps saying so.
    assert_eq!(
        session(&gateway, &amy).await.body["password_reset_by_admin"],
        reset_at
    );
    // The admin's view never carries it, and no other member sees it.
    let (ben, _) = gateway.member(&rob, "ben").await;
    assert!(
        session(&gateway, &ben)
            .await
            .body
            .get("password_reset_by_admin")
            .is_none()
    );
    assert!(
        session(&gateway, &rob)
            .await
            .body
            .get("password_reset_by_admin")
            .is_none()
    );
    // Ben acknowledging changes nothing of Amy's.
    let acknowledge = Call::new(Method::DELETE, "/api/v1/session/password-reset");
    assert_eq!(
        gateway.send(acknowledge.cookie(&ben)).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        session(&gateway, &amy).await.body["password_reset_by_admin"],
        reset_at
    );

    let acknowledge = Call::new(Method::DELETE, "/api/v1/session/password-reset");
    assert_eq!(
        gateway.send(acknowledge.cookie(&amy)).await.status,
        StatusCode::NO_CONTENT
    );
    assert!(
        session(&gateway, &amy)
            .await
            .body
            .get("password_reset_by_admin")
            .is_none()
    );
    let again = gateway.login("amy", "a password rob chose").await;
    assert_eq!(again.status, StatusCode::OK);
    assert!(again.body.get("password_reset_by_admin").is_none());
    // Acknowledging needs a session.
    let anonymous = gateway
        .send(Call::new(Method::DELETE, "/api/v1/session/password-reset"))
        .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn no_member_can_read_another_members_data_whatever_the_switches_say() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let (ben, ben_id) = gateway.member(&rob, "ben").await;
    let diary = create_bot(&gateway, &amy, "Diary").await.body["id"]
        .as_i64()
        .unwrap();
    chat(&gateway, &amy, diary, "amy's secret").await;
    let tutor = make_for(&gateway, &rob, amy_id, "Tutor").await;
    // Every switch on for both, and Ben with every permission.
    for user in [amy_id, ben_id] {
        let reply = patch_user(
            &gateway,
            &rob,
            user,
            json!({"oversight": true, "permissions": Permissions::default()}),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK);
    }

    let mut calls = vec![
        Call::get(&format!("/api/v1/admin/users/{amy_id}/bots")),
        Call::post(&format!("/api/v1/admin/users/{amy_id}/bots"))
            .json(&json!({"name": "Spy", "label": "", "description": ""})),
        Call::new(Method::PATCH, &format!("/api/v1/admin/users/{amy_id}"))
            .json(&json!({"oversight": false})),
        Call::new(
            Method::PUT,
            &format!("/api/v1/admin/users/{amy_id}/password"),
        )
        .json(&json!({"new_password": "a takeover password"})),
    ];
    for bot in [diary, tutor] {
        calls.push(Call::get(&format!(
            "/api/v1/admin/users/{amy_id}/bots/{bot}/messages"
        )));
        calls.push(
            Call::new(
                Method::PATCH,
                &format!("/api/v1/admin/users/{amy_id}/bots/{bot}"),
            )
            .json(&json!({"name": "Mine now"})),
        );
    }
    for call in calls {
        let reply = gateway.send(call.cookie(&ben)).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{:?}", reply.body);
        assert!(!reply.body.to_string().contains("secret"));
    }
    for bot in [diary, tutor] {
        for call in [
            Call::get(&format!("/api/v1/bots/{bot}")),
            Call::get(&format!("/api/v1/bots/{bot}/messages")),
            Call::new(Method::PATCH, &format!("/api/v1/bots/{bot}"))
                .json(&json!({"name": "Mine now"})),
        ] {
            let reply = gateway.send(call.cookie(&ben)).await;
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{:?}", reply.body);
        }
    }
    // Amy's account, Bots and log are as they were; Ben's log holds only
    // his own account's changes.
    assert_eq!(
        session(&gateway, &amy).await.body["user"]["oversight"],
        true
    );
    assert_eq!(gateway.login("amy", PASSWORD).await.status, StatusCode::OK);
    let list = gateway.send(Call::get("/api/v1/bots").cookie(&amy)).await;
    let mut names = bot_names(&list.body);
    names.sort();
    assert_eq!(names, ["Diary", "Tutor"]);
    assert!(
        log(&gateway, ben_id)
            .await
            .iter()
            .all(|e| matches!(e, Event::AccountUpdated { user } if user.id == UserId(ben_id)))
    );
    assert!(
        bot_names(
            &gateway
                .send(Call::get("/api/v1/bots").cookie(&ben))
                .await
                .body
        )
        .is_empty()
    );
}
