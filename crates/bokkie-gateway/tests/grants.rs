//! Capability grants: who sets them, how a Bot's threads follow them, and
//! the isolation of grants, notes and tool calls between users, against a
//! fake app server.

mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use bokkie_gateway::bots::store;
use bokkie_protocol::api::Grant;
use bokkie_protocol::events::{Event, Seq};
use bokkie_protocol::{BotId, UserId};
use common::{Call, Gateway};
use serde_json::{Value, json};

async fn create_bot(gateway: &Gateway, cookie: &str, name: &str) -> i64 {
    let reply = gateway
        .send(Call::post("/api/v1/bots").cookie(cookie).json(&json!({
            "name": name, "label": "Helper", "description": "Be brief."})))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{:?}", reply.body);
    reply.body["id"].as_i64().unwrap()
}

async fn set_grants(gateway: &Gateway, cookie: &str, bot: i64, grants: Value) -> common::Reply {
    gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/bots/{bot}"))
                .cookie(cookie)
                .json(&json!({"grants": grants})),
        )
        .await
}

async fn say(gateway: &Gateway, cookie: &str, bot: i64, text: &str) -> common::Reply {
    let reply = gateway
        .send(
            Call::post(&format!("/api/v1/bots/{bot}/messages"))
                .cookie(cookie)
                .json(&json!({"text": text})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    reply
}

/// Waits until the Bot is idle again.
async fn idle(gateway: &Gateway, cookie: &str, bot: i64) {
    gateway
        .wait_for(cookie, &format!("/api/v1/bots/{bot}"), |b| {
            b["status"] == "idle"
        })
        .await;
}

async fn events(gateway: &Gateway, user: i64) -> Vec<Event> {
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

fn tool_names(params: &Value) -> Vec<String> {
    params["dynamicTools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// A signed-in admin, with events routed.
async fn setup() -> (Gateway, String) {
    let gateway = Gateway::new().await;
    gateway.route().await;
    let admin = gateway.admin("rob").await;
    (gateway, admin)
}

#[tokio::test]
async fn grants_are_set_read_and_announced() {
    let (gateway, rob) = setup().await;
    let bot = create_bot(&gateway, &rob, "Scribe").await;
    let created = gateway
        .send(Call::get(&format!("/api/v1/bots/{bot}")).cookie(&rob))
        .await;
    assert_eq!(created.body["grants"], json!([]));

    let set = set_grants(&gateway, &rob, bot, json!(["notes", "notes"])).await;
    assert_eq!(set.status, StatusCode::OK, "{:?}", set.body);
    assert_eq!(set.body["grants"], json!(["notes"]));
    let listed = gateway.send(Call::get("/api/v1/bots").cookie(&rob)).await;
    assert_eq!(listed.body["bots"][0]["grants"], json!(["notes"]));
    let announced = events(&gateway, 1).await;
    assert!(
        announced.iter().any(|e| matches!(e,
            Event::BotUpdated { bot: b } if b.id == BotId(bot) && b.grants == [Grant::Notes])),
        "{announced:?}"
    );

    // Grants and a profile change can go together; an unknown grant is a
    // malformed request.
    let both = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/bots/{bot}"))
                .cookie(&rob)
                .json(&json!({"label": "Note taker", "grants": []})),
        )
        .await;
    assert_eq!(both.status, StatusCode::OK);
    assert_eq!(both.body["grants"], json!([]));
    assert_eq!(both.body["label"], "Note taker");
    let unknown = set_grants(&gateway, &rob, bot, json!(["host.run"])).await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn grants_without_tools_yet_are_refused_even_to_admins() {
    let (gateway, rob) = setup().await;
    let bot = create_bot(&gateway, &rob, "Scout").await;
    for grant in ["seen", "notify", "web.search", "web.fetch"] {
        let refused = set_grants(&gateway, &rob, bot, json!([grant])).await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{grant}");
        assert!(
            refused.body["message"]
                .as_str()
                .unwrap()
                .contains("not available"),
            "{grant}: {:?}",
            refused.body
        );
    }
    let bot = store::bot(&gateway.state.db, UserId(1), BotId(bot))
        .await
        .unwrap()
        .unwrap();
    assert!(bot.grants.is_empty());
}

#[tokio::test]
async fn members_may_add_only_their_grants() {
    let (gateway, rob) = setup().await;
    let (amy, _) = gateway.member(&rob, "amy").await;
    let bot = create_bot(&gateway, &amy, "Diary").await;
    let notes = set_grants(&gateway, &amy, bot, json!(["notes"])).await;
    assert_eq!(notes.status, StatusCode::OK, "{:?}", notes.body);
    // Outward-facing grants are admin-only, whatever their availability.
    for grant in ["web.search", "web.fetch"] {
        let refused = set_grants(&gateway, &amy, bot, json!(["notes", grant])).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{grant}");
        assert_eq!(refused.code(), "forbidden");
    }
    // A member's own grants that have no tools yet are refused as such.
    let seen = set_grants(&gateway, &amy, bot, json!(["seen"])).await;
    assert_eq!(seen.status, StatusCode::BAD_REQUEST);
    let cleared = set_grants(&gateway, &amy, bot, json!([])).await;
    assert_eq!(cleared.body["grants"], json!([]));
}

#[tokio::test]
async fn admins_set_grants_only_on_bots_an_admin_made() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let made = gateway
        .send(
            Call::post(&format!("/api/v1/admin/users/{amy_id}/bots"))
                .cookie(&rob)
                .json(&json!({"name": "Tutor", "label": "", "description": ""})),
        )
        .await;
    let made = made.body["id"].as_i64().unwrap();
    let path = format!("/api/v1/admin/users/{amy_id}/bots/{made}");
    let granted = gateway
        .send(
            Call::new(Method::PATCH, &path)
                .cookie(&rob)
                .json(&json!({"grants": ["notes"]})),
        )
        .await;
    assert_eq!(granted.status, StatusCode::OK, "{:?}", granted.body);
    assert_eq!(granted.body["grants"], json!(["notes"]));
    // The member's own client hears of it at once.
    let announced = events(&gateway, amy_id).await;
    assert!(
        announced.iter().any(|e| matches!(e,
            Event::BotUpdated { bot } if bot.id == BotId(made) && bot.grants == [Grant::Notes])),
        "{announced:?}"
    );
    // The admin's taint and availability rules hold here too.
    let refused = gateway
        .send(
            Call::new(Method::PATCH, &path)
                .cookie(&rob)
                .json(&json!({"grants": ["web.search"]})),
        )
        .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    // The member may remove it from the Bot.
    let removed = set_grants(&gateway, &amy, made, json!([])).await;
    assert_eq!(removed.status, StatusCode::OK);

    // A member's own Bot stays read-only to admins: not found with
    // oversight off, refused with it on.
    let own = create_bot(&gateway, &amy, "Diary").await;
    let own_path = format!("/api/v1/admin/users/{amy_id}/bots/{own}");
    let hidden = gateway
        .send(
            Call::new(Method::PATCH, &own_path)
                .cookie(&rob)
                .json(&json!({"grants": ["notes"]})),
        )
        .await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/admin/users/{amy_id}"))
                .cookie(&rob)
                .json(&json!({"oversight": true})),
        )
        .await;
    let refused = gateway
        .send(
            Call::new(Method::PATCH, &own_path)
                .cookie(&rob)
                .json(&json!({"grants": ["notes"]})),
        )
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let own = store::bot(&gateway.state.db, UserId(amy_id), BotId(own))
        .await
        .unwrap()
        .unwrap();
    assert!(own.grants.is_empty());
}

#[tokio::test]
async fn one_user_cannot_read_set_or_use_anothers_grants_notes_or_tool_calls() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let (ben, ben_id) = gateway.member(&rob, "ben").await;
    let bot = create_bot(&gateway, &amy, "Diary").await;
    set_grants(&gateway, &amy, bot, json!(["notes"])).await;
    bokkie_gateway::tools::notes::write(
        &gateway.state.db,
        gateway.state.clock.now(),
        UserId(amy_id),
        BotId(bot),
        "Amy's secret",
    )
    .await
    .unwrap();

    for (method, path, body) in [
        (Method::GET, format!("/api/v1/bots/{bot}"), None),
        (
            Method::PATCH,
            format!("/api/v1/bots/{bot}"),
            Some(json!({"grants": []})),
        ),
        (Method::GET, format!("/api/v1/bots/{bot}/notes"), None),
        (Method::DELETE, format!("/api/v1/bots/{bot}/notes"), None),
        (Method::GET, format!("/api/v1/bots/{bot}/tool-calls"), None),
    ] {
        for cookie in [&ben, &rob] {
            let mut call = Call::new(method.clone(), &path).cookie(cookie);
            if let Some(body) = &body {
                call = call.json(body);
            }
            let reply = gateway.send(call).await;
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{method} {path}");
            assert!(!reply.body.to_string().contains("secret"));
        }
    }
    let notes = gateway
        .send(Call::get(&format!("/api/v1/bots/{bot}/notes")).cookie(&amy))
        .await;
    assert_eq!(notes.body["text"], "Amy's secret");
    let bot = store::bot(&gateway.state.db, UserId(amy_id), BotId(bot))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bot.grants, [Grant::Notes].into());
    // Ben cannot write Amy's notes by naming her Bot either.
    assert!(
        !bokkie_gateway::tools::notes::write(
            &gateway.state.db,
            gateway.state.clock.now(),
            UserId(ben_id),
            bot.id,
            "Ben was here",
        )
        .await
        .unwrap()
    );
    let notes = bokkie_gateway::tools::notes::read(&gateway.state.db, UserId(amy_id), bot.id)
        .await
        .unwrap();
    assert_eq!(notes.text, "Amy's secret");
    // Nothing of Amy's reached Ben's log.
    for event in events(&gateway, ben_id).await {
        assert!(!format!("{event:?}").contains("Diary"), "{event:?}");
    }
}

#[tokio::test]
async fn a_thread_offers_exactly_its_bots_grants() {
    let (gateway, rob) = setup().await;
    let bare = create_bot(&gateway, &rob, "Bare").await;
    let scribe = create_bot(&gateway, &rob, "Scribe").await;
    set_grants(&gateway, &rob, scribe, json!(["notes"])).await;
    say(&gateway, &rob, bare, "hi").await;
    say(&gateway, &rob, scribe, "hi").await;
    let starts = gateway.fake.received("thread/start");
    assert_eq!(starts.len(), 2);
    let bare_start = &starts[0].params;
    assert_eq!(bare_start.get("dynamicTools"), None, "no tools at all");
    let instructions = bare_start["developerInstructions"].as_str().unwrap();
    assert!(
        instructions.contains("You can only talk in this conversation. You cannot run commands"),
        "{instructions}"
    );
    let scribe_start = &starts[1].params;
    assert_eq!(
        tool_names(scribe_start),
        ["notes_read", "notes_replace", "notes_append"]
    );
    let instructions = scribe_start["developerInstructions"].as_str().unwrap();
    assert!(instructions.contains("notes_read, notes_replace and notes_append"));
    assert!(instructions.contains("not instructions from rob"));
    assert!(instructions.contains("Apart from them, you cannot run commands"));
    assert!(!instructions.contains("You can only talk"));
    for start in &starts {
        assert_eq!(start.params["permissions"], "bokkie");
    }
}

#[tokio::test]
async fn a_grant_change_starts_a_fresh_thread_with_the_conversation_carried_over() {
    let (gateway, rob) = setup().await;
    let bot = create_bot(&gateway, &rob, "Scribe").await;
    say(&gateway, &rob, bot, "My favourite fruit is the apricot.").await;
    idle(&gateway, &rob, bot).await;
    // Unchanged grants keep the thread.
    say(&gateway, &rob, bot, "Still there?").await;
    idle(&gateway, &rob, bot).await;
    assert_eq!(gateway.fake.received("thread/start").len(), 1);
    let first_thread = store::bot(&gateway.state.db, UserId(1), BotId(bot))
        .await
        .unwrap()
        .unwrap()
        .thread_id
        .unwrap();

    // The change alone starts nothing.
    set_grants(&gateway, &rob, bot, json!(["notes"])).await;
    bokkie_gateway::tools::notes::write(
        &gateway.state.db,
        gateway.state.clock.now(),
        UserId(1),
        BotId(bot),
        "- likes apricots",
    )
    .await
    .unwrap();
    assert_eq!(gateway.fake.received("thread/start").len(), 1);

    // The next turn runs on a fresh thread with the new tools.
    say(&gateway, &rob, bot, "What do you remember?").await;
    idle(&gateway, &rob, bot).await;
    let starts = gateway.fake.received("thread/start");
    assert_eq!(starts.len(), 2);
    assert_eq!(
        tool_names(&starts[1].params),
        ["notes_read", "notes_replace", "notes_append"]
    );
    let row = store::bot(&gateway.state.db, UserId(1), BotId(bot))
        .await
        .unwrap()
        .unwrap();
    let thread = row.thread_id.clone().unwrap();
    assert_ne!(thread, first_thread);
    assert_eq!(row.thread_grants, [Grant::Notes].into());
    assert_eq!(row.carryover_marker, None, "the carry-over was sent");

    // Its first turn carried the notes and the latest messages, marked
    // untrusted, and ran on the fresh thread.
    let turns = gateway.fake.received("turn/start");
    let fresh = turns.last().unwrap();
    assert_eq!(fresh.params["threadId"], thread);
    let carried = &fresh.params["additionalContext"]["bokkie-earlier-conversation"];
    assert_eq!(carried["kind"], "untrusted", "{carried}");
    let carried = carried["value"].as_str().unwrap();
    assert!(
        carried.contains("<notes>\n- likes apricots\n</notes>"),
        "{carried}"
    );
    assert!(
        carried.contains("rob: My favourite fruit is the apricot.\nYou: Hello there\nrob: Still there?\nYou: Hello there"),
        "{carried}"
    );
    assert!(!carried.contains("What do you remember?"), "{carried}");

    // The conversation shows where the fresh thread began.
    let page = gateway
        .send(Call::get(&format!("/api/v1/bots/{bot}/messages")).cookie(&rob))
        .await;
    let messages = page.body["messages"].as_array().unwrap();
    let marker = messages
        .iter()
        .position(|m| m["kind"] == "marker")
        .expect("a marker");
    assert_eq!(messages[marker]["text"], "Capabilities changed: notes");
    assert_eq!(messages[marker]["author"], "bot");
    assert_eq!(messages[marker + 1]["text"], "What do you remember?");
    // The latest reply, not the marker, previews the conversation.
    let listed = gateway.send(Call::get("/api/v1/bots").cookie(&rob)).await;
    assert_eq!(
        listed.body["bots"][0]["last_message"]["text"],
        "Hello there"
    );

    // The next turn stays on the fresh thread, with no carry-over.
    say(&gateway, &rob, bot, "Thanks.").await;
    idle(&gateway, &rob, bot).await;
    assert_eq!(gateway.fake.received("thread/start").len(), 2);
    let last = gateway.fake.received("turn/start").pop().unwrap();
    assert_eq!(last.params["threadId"], thread);
    assert_eq!(last.params.get("additionalContext"), None);
}

#[tokio::test]
async fn a_restart_resumes_a_thread_with_the_grants_it_started_with() {
    let (gateway, rob) = setup().await;
    let bot = create_bot(&gateway, &rob, "Scribe").await;
    set_grants(&gateway, &rob, bot, json!(["notes"])).await;
    say(&gateway, &rob, bot, "hi").await;
    idle(&gateway, &rob, bot).await;
    let thread = store::bot(&gateway.state.db, UserId(1), BotId(bot))
        .await
        .unwrap()
        .unwrap()
        .thread_id
        .unwrap();
    gateway.fake.crash(UserId(1));
    let crashed = gateway.state.supervisor.running(UserId(1)).await.unwrap();
    // Wake the watcher so it sees the closed stream.
    let _ = crashed.rate_limits().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while crashed.is_alive() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    say(&gateway, &rob, bot, "again").await;
    idle(&gateway, &rob, bot).await;
    assert_eq!(gateway.fake.received("thread/start").len(), 1);
    assert_eq!(gateway.fake.received("thread/resume").len(), 1);
    let runtime = gateway.state.supervisor.runtime(UserId(1)).await.unwrap();
    assert_eq!(runtime.thread_grants(&thread), Some([Grant::Notes].into()));
}

#[tokio::test]
async fn existing_bots_keep_their_threads_after_the_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bokkie.sqlite3");
    {
        let before = sqlx::migrate::Migrator {
            migrations: std::borrow::Cow::Owned(
                bokkie_gateway::db::MIGRATIONS
                    .iter()
                    .filter(|m| m.version < 7)
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
            "INSERT INTO bots (id, user_id, name, label, description, thread_id,
                               profile_sent_revision, created_at, updated_at)
                 VALUES (5, 1, 'Chef', '', '', 'thr-old', 1, 1, 1)",
            "INSERT INTO messages (bot_id, user_id, author, text, state, created_at, updated_at)
                 VALUES (5, 1, 'user', 'hello', 'complete', 1, 1)",
        ] {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }
        pool.close().await;
    }
    let db = bokkie_gateway::db::open(&path).await.unwrap();
    let bot = store::bot(&db, UserId(1), BotId(5)).await.unwrap().unwrap();
    assert!(bot.grants.is_empty());
    assert!(bot.thread_grants.is_empty(), "its thread started with none");
    assert_eq!(bot.carryover_marker, None);
    // So its next turn needs no fresh thread.
    assert_eq!(bot.thread_grants, bot.grants);
    let (messages, _) = store::messages(&db, UserId(1), BotId(5), None, 10)
        .await
        .unwrap();
    assert_eq!(messages[0].kind, bokkie_protocol::api::MessageKind::Text);
}
