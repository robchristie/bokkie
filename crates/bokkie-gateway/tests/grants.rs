//! Capability grants: who sets them, how a Bot's threads follow them, and
//! the isolation of grants, notes and tool calls between users, against a
//! fake app server.

mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use bokkie_gateway::bots::store;
use bokkie_gateway::tools::seen;
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
    let refused = set_grants(&gateway, &rob, bot, json!(["web.search"])).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert!(
        refused.body["message"]
            .as_str()
            .unwrap()
            .contains("not available"),
        "{:?}",
        refused.body
    );
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
    let seen = set_grants(&gateway, &amy, bot, json!(["seen", "notes"])).await;
    assert_eq!(seen.status, StatusCode::OK, "{:?}", seen.body);
    assert_eq!(seen.body["grants"], json!(["notes", "seen"]));
    let notify = set_grants(&gateway, &amy, bot, json!(["notify", "seen"])).await;
    assert_eq!(notify.status, StatusCode::OK, "{:?}", notify.body);
    assert_eq!(notify.body["grants"], json!(["seen", "notify"]));
    let cleared = set_grants(&gateway, &amy, bot, json!([])).await;
    assert_eq!(cleared.body["grants"], json!([]));
}

#[tokio::test]
async fn web_fetch_is_granted_by_admins_only() {
    let (gateway, rob) = setup().await;
    // An admin grants it on their own Bot.
    let own = create_bot(&gateway, &rob, "Reader").await;
    let granted = set_grants(&gateway, &rob, own, json!(["web.fetch", "notes"])).await;
    assert_eq!(granted.status, StatusCode::OK, "{:?}", granted.body);
    assert_eq!(granted.body["grants"], json!(["notes", "web.fetch"]));

    // And on a Bot an admin made for a member, whose member may not add it
    // back once it is removed, nor add it to their own Bot.
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
                .json(&json!({"grants": ["web.fetch"]})),
        )
        .await;
    assert_eq!(granted.status, StatusCode::OK, "{:?}", granted.body);
    assert_eq!(granted.body["grants"], json!(["web.fetch"]));
    // Keeping it while adding notes is not adding it.
    let kept = set_grants(&gateway, &amy, made, json!(["web.fetch", "notes"])).await;
    assert_eq!(kept.status, StatusCode::OK, "{:?}", kept.body);
    let removed = set_grants(&gateway, &amy, made, json!(["notes"])).await;
    assert_eq!(removed.status, StatusCode::OK, "{:?}", removed.body);
    let again = set_grants(&gateway, &amy, made, json!(["notes", "web.fetch"])).await;
    assert_eq!(again.status, StatusCode::FORBIDDEN, "{:?}", again.body);
    let diary = create_bot(&gateway, &amy, "Diary").await;
    let refused = set_grants(&gateway, &amy, diary, json!(["web.fetch"])).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{:?}", refused.body);
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
    set_grants(&gateway, &amy, bot, json!(["notes", "seen"])).await;
    let shown = || seen::Item {
        id: "arxiv:2401.12345".into(),
        title: "Amy's secret paper".into(),
        url: "https://arxiv.org/abs/2401.12345".into(),
    };
    let mut tx = gateway.state.db.begin().await.unwrap();
    let now = gateway.state.clock.now();
    assert!(
        seen::mark(&mut tx, now, UserId(amy_id), BotId(bot), &[shown()])
            .await
            .unwrap()
    );
    // Ben cannot mark items on Amy's Bot by naming it.
    assert!(
        !seen::mark(&mut tx, now, UserId(ben_id), BotId(bot), &[shown()])
            .await
            .unwrap()
    );
    tx.commit().await.unwrap();
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
    assert_eq!(bot.grants, [Grant::Notes, Grant::Seen].into());
    // Nor read them.
    let ids = ["arxiv:2401.12345".to_owned()];
    let now = gateway.state.clock.now();
    assert!(
        seen::shown(&gateway.state.db, now, UserId(ben_id), bot.id, &ids)
            .await
            .unwrap()
            .is_empty()
    );
    let amys = seen::shown(&gateway.state.db, now, UserId(amy_id), bot.id, &ids)
        .await
        .unwrap();
    assert_eq!(amys[&ids[0]].title, "Amy's secret paper");
    assert_eq!(
        seen::count(&gateway.state.db, UserId(amy_id), bot.id)
            .await
            .unwrap(),
        1
    );
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
    let scout = create_bot(&gateway, &rob, "Scout").await;
    set_grants(&gateway, &rob, scout, json!(["seen", "notes"])).await;
    say(&gateway, &rob, bare, "hi").await;
    say(&gateway, &rob, scribe, "hi").await;
    say(&gateway, &rob, scout, "hi").await;
    let starts = gateway.fake.received("thread/start");
    assert_eq!(starts.len(), 3);
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
    assert!(!instructions.contains("seen_check"));
    let scout_start = &starts[2].params;
    assert_eq!(
        tool_names(scout_start),
        [
            "notes_read",
            "notes_replace",
            "notes_append",
            "seen_check",
            "seen_mark"
        ]
    );
    let instructions = scout_start["developerInstructions"].as_str().unwrap();
    assert!(
        instructions.contains("seen_check and seen_mark"),
        "{instructions}"
    );
    for start in &starts {
        assert_eq!(start.params["permissions"], "bokkie");
    }
}

/// Gives rob's Bot these grants in the database, past the API's rules.
async fn hold(gateway: &Gateway, bot: i64, grants: &[Grant]) {
    store::set_grants(
        &gateway.state.db,
        gateway.state.clock.now(),
        UserId(1),
        BotId(bot),
        &grants.iter().copied().collect(),
    )
    .await
    .unwrap();
}

/// Crashes the user's app server and waits until the gateway sees it.
async fn crash(gateway: &Gateway, user: i64) {
    gateway.fake.crash(UserId(user));
    let crashed = gateway
        .state
        .supervisor
        .running(UserId(user))
        .await
        .unwrap();
    // Wake the watcher so it sees the closed stream.
    let _ = crashed.rate_limits().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while crashed.is_alive() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The request among `requests` that names `thread`.
fn thread_of(requests: &[common::fake_codex::Received], thread: &str) -> Value {
    requests
        .iter()
        .find(|r| r.params["threadId"] == thread)
        .unwrap_or_else(|| panic!("no request for {thread}"))
        .params
        .clone()
}

#[tokio::test]
async fn only_a_web_search_bots_threads_turn_web_search_on_at_every_start_and_resume() {
    let (gateway, rob) = setup().await;
    let bare = create_bot(&gateway, &rob, "Bare").await;
    let scribe = create_bot(&gateway, &rob, "Scribe").await;
    set_grants(&gateway, &rob, scribe, json!(["notes", "seen"])).await;
    // web.search cannot be granted through the API yet; a Bot holding it
    // is made in the database.
    let scout = create_bot(&gateway, &rob, "Scout").await;
    hold(&gateway, scout, &[Grant::WebSearch]).await;
    let digest = create_bot(&gateway, &rob, "Digest").await;
    hold(
        &gateway,
        digest,
        &[Grant::Seen, Grant::WebSearch, Grant::Notes],
    )
    .await;
    let bots = [bare, scribe, scout, digest];
    for bot in bots {
        say(&gateway, &rob, bot, "hi").await;
        idle(&gateway, &rob, bot).await;
    }
    let live = json!({"web_search": "live"});
    let starts = gateway.fake.received("thread/start");
    assert_eq!(starts.len(), 4);
    assert_eq!(starts[0].params.get("config"), None, "no grants");
    assert_eq!(starts[1].params.get("config"), None, "notes and seen");
    assert_eq!(starts[2].params["config"], live);
    assert_eq!(starts[3].params["config"], live);
    // Web search is Codex's own, not a Bokkie tool, and the instructions
    // describe it.
    assert_eq!(starts[2].params.get("dynamicTools"), None);
    assert_eq!(tool_names(&starts[3].params).len(), 5);
    for (start, searches) in starts.iter().zip([false, false, true, true]) {
        let instructions = start.params["developerInstructions"].as_str().unwrap();
        assert_eq!(
            instructions.contains("Web search: you can search the live web"),
            searches,
            "{instructions}"
        );
        assert_eq!(start.params["permissions"], "bokkie");
    }
    let threads: Vec<String> = {
        let mut threads = Vec::new();
        for bot in bots {
            let row = store::bot(&gateway.state.db, UserId(1), BotId(bot))
                .await
                .unwrap()
                .unwrap();
            threads.push(row.thread_id.unwrap());
        }
        threads
    };

    // Codex keeps the override only while a thread is loaded, so a cold
    // resume sends it again, and only for the web.search Bots.
    crash(&gateway, 1).await;
    for bot in bots {
        say(&gateway, &rob, bot, "again").await;
        idle(&gateway, &rob, bot).await;
    }
    let resumes = gateway.fake.received("thread/resume");
    assert_eq!(resumes.len(), 4);
    assert_eq!(gateway.fake.received("thread/start").len(), 4);
    assert_eq!(thread_of(&resumes, &threads[0]).get("config"), None);
    assert_eq!(thread_of(&resumes, &threads[1]).get("config"), None);
    assert_eq!(thread_of(&resumes, &threads[2])["config"], live);
    assert_eq!(thread_of(&resumes, &threads[3])["config"], live);

    // Losing the grant starts a fresh thread without it; gaining it, one
    // with it.
    set_grants(&gateway, &rob, scout, json!([])).await;
    hold(&gateway, bare, &[Grant::WebSearch]).await;
    say(&gateway, &rob, scout, "and now?").await;
    idle(&gateway, &rob, scout).await;
    say(&gateway, &rob, bare, "and now?").await;
    idle(&gateway, &rob, bare).await;
    let starts = gateway.fake.received("thread/start");
    assert_eq!(starts.len(), 6);
    assert_eq!(starts[4].params.get("config"), None);
    assert_eq!(starts[5].params["config"], live);
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

/// The user's tokens this week, as the usage meter shows them.
async fn tokens(gateway: &Gateway, cookie: &str) -> i64 {
    gateway
        .send(Call::get("/api/v1/usage").cookie(cookie))
        .await
        .body["allowance"]["tokens_this_week"]
        .as_i64()
        .unwrap()
}

async fn tokens_reach(gateway: &Gateway, cookie: &str, expected: i64) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let now = tokens(gateway, cookie).await;
        if now == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "tokens stayed at {now}, not {expected}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// A message meant to steer a turn that ends just as the Bot's grants
/// changed starts a fresh thread. The replaced thread's turn can then
/// report its tokens and end only after the fresh thread took over: both
/// still reach the Bot, each thread's tokens count from its own baseline,
/// the old turn's end leaves the fresh turn running, and the message is
/// the fresh turn's input without being carried over as well.
#[tokio::test]
async fn a_replaced_threads_late_turn_end_reaches_its_bot_and_the_message_goes_once() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| {
        b.finish_turns = false;
        b.usage_mid_turn = true;
    });
    let bot = create_bot(&gateway, &rob, "Scribe").await;
    say(&gateway, &rob, bot, "First, the apricots.").await;
    tokens_reach(&gateway, &rob, 120).await;
    let old_thread = store::bot(&gateway.state.db, UserId(1), BotId(bot))
        .await
        .unwrap()
        .unwrap()
        .thread_id
        .unwrap();
    let old_turn: String =
        sqlx::query_scalar("SELECT turn_id FROM messages WHERE author = 'bot' ORDER BY id LIMIT 1")
            .fetch_one(&gateway.state.db)
            .await
            .unwrap();

    // The grants change mid-turn, and the turn ends just as a message
    // arrives to steer it.
    set_grants(&gateway, &rob, bot, json!(["notes"])).await;
    gateway.fake.with(|b| b.steer_fails = true);
    let sent = say(&gateway, &rob, bot, "Second, the figs.").await;
    assert_eq!(sent.body["delivery"], "new_turn", "{:?}", sent.body);
    let fresh_thread = store::bot(&gateway.state.db, UserId(1), BotId(bot))
        .await
        .unwrap()
        .unwrap()
        .thread_id
        .unwrap();
    assert_ne!(fresh_thread, old_thread);
    let turn = gateway.fake.received("turn/start").pop().unwrap();
    assert_eq!(turn.params["threadId"], fresh_thread);
    assert!(
        turn.params["input"]
            .to_string()
            .contains("Second, the figs."),
        "{}",
        turn.params
    );
    let carried = turn.params["additionalContext"]["bokkie-earlier-conversation"]["value"]
        .as_str()
        .unwrap();
    assert!(carried.contains("rob: First, the apricots."), "{carried}");
    assert!(!carried.contains("figs"), "{carried}");
    // The fresh thread's tokens count from zero.
    tokens_reach(&gateway, &rob, 240).await;

    // The replaced thread's turn now reports its last tokens and ends.
    gateway.fake.inject(
        UserId(1),
        json!({"method": "thread/tokenUsage/updated", "params": {
            "threadId": old_thread, "turnId": old_turn, "tokenUsage": {
                "total": {"totalTokens": 300, "inputTokens": 280, "cachedInputTokens": 0,
                          "outputTokens": 20, "reasoningOutputTokens": 0},
                "last": {"totalTokens": 180, "inputTokens": 160, "cachedInputTokens": 0,
                         "outputTokens": 20, "reasoningOutputTokens": 0},
                "modelContextWindow": 272000}}}),
    );
    gateway.fake.inject(
        UserId(1),
        json!({"method": "turn/completed", "params": {"threadId": old_thread,
            "turn": {"id": old_turn, "items": [], "status": "completed", "error": null}}}),
    );
    tokens_reach(&gateway, &rob, 420).await;
    let page = gateway
        .wait_for(&rob, &format!("/api/v1/bots/{bot}/messages"), |page| {
            page["messages"][1]["state"] == "complete"
        })
        .await;
    let messages = page["messages"].as_array().unwrap();
    let said: Vec<(&str, &str, &str)> = messages
        .iter()
        .map(|m| {
            (
                m["kind"].as_str().unwrap(),
                m["text"].as_str().unwrap(),
                m["state"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        said[..4],
        [
            ("text", "First, the apricots.", "complete"),
            ("text", "Hello there", "complete"),
            ("text", "Second, the figs.", "complete"),
            ("marker", "Capabilities changed: notes", "complete"),
        ],
        "{said:?}"
    );
    // The fresh turn still runs, and its reply still streams.
    assert_eq!(said[4].0, "text");
    assert_eq!(said[4].2, "streaming", "{said:?}");
    let listed = gateway
        .send(Call::get(&format!("/api/v1/bots/{bot}")).cookie(&rob))
        .await;
    assert_eq!(listed.body["status"], "working");

    // Stopping it ends the fresh turn.
    let stopped = gateway
        .send(Call::post(&format!("/api/v1/bots/{bot}/stop")).cookie(&rob))
        .await;
    assert_eq!(stopped.status, StatusCode::OK, "{:?}", stopped.body);
    idle(&gateway, &rob, bot).await;
    assert!(gateway.state.supervisor.tainted().is_none());
}

/// A message refused for want of a turn changes nothing in the
/// conversation: the turn is counted before a fresh thread is started, so
/// a send that a concurrent one beat to the last turn leaves no marker
/// behind.
#[tokio::test]
async fn the_turn_is_counted_before_a_fresh_thread_and_its_marker() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let diary = create_bot(&gateway, &amy, "Diary").await;
    let chef = create_bot(&gateway, &amy, "Chef").await;
    say(&gateway, &amy, diary, "hi").await;
    idle(&gateway, &amy, diary).await;
    say(&gateway, &amy, chef, "hi").await;
    idle(&gateway, &amy, chef).await;
    set_grants(&gateway, &amy, diary, json!(["notes"])).await;
    let limited = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/admin/users/{amy_id}"))
                .cookie(&rob)
                .json(&json!({"limits": {"daily_turns": 3, "weekly_tokens": null}})),
        )
        .await;
    assert_eq!(limited.status, StatusCode::OK, "{:?}", limited.body);

    // The Diary's fresh thread is slow to start, and meanwhile the Chef is
    // sent a message too: one turn is left for the two.
    gateway.fake.with(|b| b.hold_thread_starts = true);
    let held = gateway.spawn(
        Call::post(&format!("/api/v1/bots/{diary}/messages"))
            .cookie(&amy)
            .json(&json!({"text": "Remember this."})),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while gateway.fake.received("thread/start").len() < 3 {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let other = gateway
        .send(
            Call::post(&format!("/api/v1/bots/{chef}/messages"))
                .cookie(&amy)
                .json(&json!({"text": "And this?"})),
        )
        .await;
    gateway.fake.thread_starts.add_permits(1);
    let held = held.await.unwrap();

    // The Diary held the last turn from before its thread started.
    assert_eq!(held.status, StatusCode::OK, "{:?}", held.body);
    assert_eq!(held.body["delivery"], "new_turn");
    assert_eq!(other.code(), "limit_reached", "{:?}", other.body);
    let page = gateway
        .send(Call::get(&format!("/api/v1/bots/{diary}/messages")).cookie(&amy))
        .await;
    let messages = page.body["messages"].as_array().unwrap();
    let marker = messages
        .iter()
        .position(|m| m["kind"] == "marker")
        .expect("a marker");
    assert_eq!(messages[marker + 1]["text"], "Remember this.");
    let page = gateway
        .send(Call::get(&format!("/api/v1/bots/{chef}/messages")).cookie(&amy))
        .await;
    assert!(!page.body.to_string().contains("And this?"));
}

#[tokio::test]
async fn existing_threads_keep_their_bots_and_token_totals_after_the_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bokkie.sqlite3");
    {
        let before = sqlx::migrate::Migrator {
            migrations: std::borrow::Cow::Owned(
                bokkie_gateway::db::MIGRATIONS
                    .iter()
                    .filter(|m| m.version < 8)
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
                               profile_sent_revision, created_at, updated_at, tokens_reported)
                 VALUES (5, 1, 'Chef', '', '', 'thr-old', 1, 1, 1, 500)",
            "INSERT INTO bots (id, user_id, name, label, description, created_at, updated_at)
                 VALUES (6, 1, 'New', '', '', 1, 1)",
        ] {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }
        pool.close().await;
    }
    let db = bokkie_gateway::db::open(&path).await.unwrap();
    let bot = store::bot_by_thread(&db, UserId(1), "thr-old")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bot.id, BotId(5));
    assert!(bot.is_current("thr-old"));
    // Its next report counts from the total already reported.
    let mut tx = bokkie_gateway::db::write(&db).await.unwrap();
    let growth = bokkie_gateway::usage::record_tokens(
        &mut tx,
        bokkie_protocol::Timestamp(2),
        UserId(1),
        BotId(5),
        "thr-old",
        620,
    )
    .await
    .unwrap();
    assert_eq!(growth, 120);
    tx.commit().await.unwrap();
    // A Bot without a thread gains one, and keeps the one it replaced.
    store::set_thread(&db, UserId(1), BotId(6), "thr-a", 1, &Default::default())
        .await
        .unwrap();
    let mut tx = db.begin().await.unwrap();
    let marker = store::insert_message(
        &mut *tx,
        bokkie_protocol::Timestamp(3),
        UserId(1),
        store::NewMessage {
            bot: BotId(6),
            author: bokkie_protocol::api::Author::Bot,
            text: "Capabilities changed: notes",
            state: bokkie_protocol::api::MessageState::Complete,
            turn: None,
            kind: bokkie_protocol::api::MessageKind::Marker,
            routine: None,
            notification: None,
        },
    )
    .await
    .unwrap();
    assert!(
        store::replace_thread(
            &mut *tx,
            UserId(1),
            BotId(6),
            "thr-a",
            "thr-b",
            1,
            &[Grant::Notes].into(),
            marker.id,
        )
        .await
        .unwrap()
    );
    tx.commit().await.unwrap();
    for (thread, current) in [("thr-a", false), ("thr-b", true)] {
        let bot = store::bot_by_thread(&db, UserId(1), thread)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bot.id, BotId(6), "{thread}");
        assert_eq!(bot.is_current(thread), current, "{thread}");
    }
    // Another user's app server finds none of them.
    assert!(
        store::bot_by_thread(&db, UserId(2), "thr-b")
            .await
            .unwrap()
            .is_none()
    );
}
