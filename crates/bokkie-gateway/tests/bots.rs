//! Bots and conversations, against a fake app server.

mod common;

use axum::http::{Method, StatusCode};
use bokkie_protocol::{BotId, UserId};
use common::{Call, Gateway};
use serde_json::{Value, json};

async fn create_bot(gateway: &Gateway, cookie: &str, name: &str) -> i64 {
    let reply = gateway
        .send(Call::post("/api/v1/bots").cookie(cookie).json(&json!({
            "name": name, "label": "Meal planner", "description": "Plan dinners. No mushrooms.",
            "avatar": "🍳",
        })))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{:?}", reply.body);
    reply.body["id"].as_i64().unwrap()
}

async fn say(gateway: &Gateway, cookie: &str, bot: i64, text: &str) -> common::Reply {
    gateway
        .send(
            Call::post(&format!("/api/v1/bots/{bot}/messages"))
                .cookie(cookie)
                .json(&json!({"text": text})),
        )
        .await
}

fn messages(body: &Value) -> Vec<Value> {
    body["messages"].as_array().cloned().unwrap_or_default()
}

fn last_bot_reply(body: &Value) -> Option<Value> {
    messages(body)
        .into_iter()
        .rev()
        .find(|m| m["author"] == "bot")
}

/// A signed-in admin, with events routed.
async fn setup() -> (Gateway, String) {
    let gateway = Gateway::new().await;
    gateway.route().await;
    let admin = gateway.admin("rob").await;
    (gateway, admin)
}

#[tokio::test]
async fn bots_are_created_listed_edited_pinned_and_hidden() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let tutor = create_bot(&gateway, &rob, "Tutor").await;
    // Names are unique per user, ignoring case.
    let duplicate = gateway
        .send(Call::post("/api/v1/bots").cookie(&rob).json(&json!({
            "name": "chef", "label": "", "description": ""})))
        .await;
    assert_eq!(duplicate.status, StatusCode::CONFLICT);
    let invalid = gateway
        .send(Call::post("/api/v1/bots").cookie(&rob).json(&json!({
            "name": "", "label": "", "description": ""})))
        .await;
    assert_eq!(invalid.status, StatusCode::BAD_REQUEST);

    let pinned = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/bots/{tutor}"))
                .cookie(&rob)
                .json(&json!({"pinned": true, "avatar": null})),
        )
        .await;
    assert_eq!(pinned.status, StatusCode::OK);
    assert_eq!(pinned.body["pinned"], true);
    assert_eq!(pinned.body["avatar"], Value::Null);
    let hidden = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/bots/{chef}"))
                .cookie(&rob)
                .json(&json!({"hidden": true})),
        )
        .await;
    assert_eq!(hidden.body["hidden"], true);
    assert_eq!(hidden.body["avatar"], "🍳", "unchanged fields stay");

    let list = gateway.send(Call::get("/api/v1/bots").cookie(&rob)).await;
    let bots = list.body["bots"].as_array().unwrap();
    assert_eq!(bots.len(), 2);
    assert_eq!(bots[0]["name"], "Tutor", "pinned Bots come first");
    assert_eq!(bots[0]["status"], "idle");
    assert_eq!(bots[1]["last_message"], Value::Null);
}

#[tokio::test]
async fn a_message_starts_the_bots_thread_and_the_reply_streams_in() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let sent = say(&gateway, &rob, chef, "What's for dinner?").await;
    assert_eq!(sent.status, StatusCode::OK, "{:?}", sent.body);
    assert_eq!(sent.body["delivery"], "new_turn");
    assert_eq!(sent.body["message"]["author"], "user");

    let path = format!("/api/v1/bots/{chef}/messages");
    let page = gateway
        .wait_for(&rob, &path, |body| {
            last_bot_reply(body).is_some_and(|m| m["state"] == "complete")
        })
        .await;
    let reply = last_bot_reply(&page).unwrap();
    assert_eq!(reply["text"], "Hello there");
    assert_eq!(messages(&page)[0]["text"], "What's for dinner?");

    // The thread started with the Bot's profile as developer instructions,
    // under the bokkie profile.
    let start = &gateway.fake.received("thread/start")[0];
    let instructions = start.params["developerInstructions"].as_str().unwrap();
    for part in [
        "You are Chef",
        "Meal planner",
        "No mushrooms",
        "You work for rob",
    ] {
        assert!(instructions.contains(part), "{instructions}");
    }
    assert_eq!(start.params["permissions"], "bokkie");
    // The first turn needs no profile update: the thread already has it.
    let turn = &gateway.fake.received("turn/start")[0];
    assert!(
        turn.params.get("additionalContext").is_none(),
        "{}",
        turn.params
    );

    let bots = gateway
        .wait_for(&rob, "/api/v1/bots", |body| {
            body["bots"][0]["status"] == "idle"
        })
        .await;
    assert_eq!(bots["bots"][0]["last_message"]["text"], "Hello there");
    assert_eq!(bots["bots"][0]["last_message"]["author"], "bot");

    // A second message continues the same thread.
    let again = say(&gateway, &rob, chef, "And dessert?").await;
    assert_eq!(again.body["delivery"], "new_turn");
    assert_eq!(gateway.fake.received("thread/start").len(), 1);
    let turns = gateway.fake.received("turn/start");
    assert_eq!(turns[1].params["threadId"], turns[0].params["threadId"]);
}

#[tokio::test]
async fn a_message_mid_turn_steers_and_stop_interrupts() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| b.finish_turns = false);
    let chef = create_bot(&gateway, &rob, "Chef").await;
    say(&gateway, &rob, chef, "Plan the week").await;
    let path = format!("/api/v1/bots/{chef}/messages");
    gateway
        .wait_for(&rob, &path, |body| {
            last_bot_reply(body)
                .is_some_and(|m| m["state"] == "streaming" && m["text"] == "Hello there")
        })
        .await;
    let bot = gateway
        .send(Call::get(&format!("/api/v1/bots/{chef}")).cookie(&rob))
        .await;
    assert_eq!(bot.body["status"], "working");

    let steer = say(&gateway, &rob, chef, "Vegetarian, please").await;
    assert_eq!(steer.body["delivery"], "steered");
    let steers = gateway.fake.received("turn/steer");
    assert_eq!(steers.len(), 1);
    let turn_id = gateway.fake.received("turn/start")[0].params["threadId"].clone();
    assert_eq!(steers[0].params["threadId"], turn_id);
    assert!(
        steers[0].params["expectedTurnId"]
            .as_str()
            .unwrap()
            .starts_with("turn-")
    );

    let stop = gateway
        .send(Call::post(&format!("/api/v1/bots/{chef}/stop")).cookie(&rob))
        .await;
    assert_eq!(stop.body["stopped"], true);
    assert_eq!(gateway.fake.received("turn/interrupt").len(), 1);
    let page = gateway
        .wait_for(&rob, &path, |body| {
            last_bot_reply(body).is_some_and(|m| m["state"] == "interrupted")
        })
        .await;
    assert_eq!(last_bot_reply(&page).unwrap()["text"], "Hello there");
    gateway
        .wait_for(&rob, &format!("/api/v1/bots/{chef}"), |body| {
            body["status"] == "idle"
        })
        .await;
    // Nothing runs now, so stop does nothing.
    let stop = gateway
        .send(Call::post(&format!("/api/v1/bots/{chef}/stop")).cookie(&rob))
        .await;
    assert_eq!(stop.body["stopped"], false);
}

#[tokio::test]
async fn a_changed_profile_reaches_the_model_on_the_next_turn_once() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let path = format!("/api/v1/bots/{chef}/messages");
    say(&gateway, &rob, chef, "Hi").await;
    gateway
        .wait_for(&rob, &path, |b| {
            last_bot_reply(b).is_some_and(|m| m["state"] == "complete")
        })
        .await;
    gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/bots/{chef}"))
                .cookie(&rob)
                .json(&json!({"description": "Only plan Italian food."})),
        )
        .await;
    // Pinning is not a profile change.
    gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/bots/{chef}"))
                .cookie(&rob)
                .json(&json!({"pinned": true})),
        )
        .await;
    gateway
        .wait_for(&rob, &format!("/api/v1/bots/{chef}"), |b| {
            b["status"] == "idle"
        })
        .await;
    say(&gateway, &rob, chef, "Tonight?").await;
    gateway
        .wait_for(&rob, &path, |b| {
            last_bot_reply(b).is_some_and(|m| m["state"] == "complete") && messages(b).len() == 4
        })
        .await;
    gateway
        .wait_for(&rob, &format!("/api/v1/bots/{chef}"), |b| {
            b["status"] == "idle"
        })
        .await;
    say(&gateway, &rob, chef, "Tomorrow?").await;
    let turns = gateway.fake.received("turn/start");
    let context = &turns[1].params["additionalContext"]["bokkie-profile"];
    assert_eq!(context["kind"], "application");
    assert!(
        context["value"]
            .as_str()
            .unwrap()
            .contains("Only plan Italian food.")
    );
    assert!(turns[2].params.get("additionalContext").is_none());
}

#[tokio::test]
async fn threads_resume_on_a_new_app_server() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let path = format!("/api/v1/bots/{chef}/messages");
    say(&gateway, &rob, chef, "Hi").await;
    gateway
        .wait_for(&rob, &path, |b| {
            last_bot_reply(b).is_some_and(|m| m["state"] == "complete")
        })
        .await;
    let user = UserId(1);
    gateway.fake.crash(user);
    let runtime = gateway.state.supervisor.running(user).await.unwrap();
    // Wake the watcher so it sees the closed stream.
    let _ = runtime.rate_limits().await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while runtime.is_alive() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let sent = say(&gateway, &rob, chef, "Still there?").await;
    assert_eq!(sent.status, StatusCode::OK, "{:?}", sent.body);
    let resumes = gateway.fake.received("thread/resume");
    assert_eq!(resumes.len(), 1);
    let thread = gateway.fake.received("thread/start")[0].params.clone();
    assert_eq!(resumes[0].params["permissions"], "bokkie");
    assert_eq!(resumes[0].user(), user);
    assert_eq!(gateway.fake.starts(user), 2);
    assert!(thread.is_object());
}

#[tokio::test]
async fn a_bot_whose_thread_is_refused_stays_unavailable() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    gateway.fake.with(|b| b.profile = ":workspace".into());
    let sent = say(&gateway, &rob, chef, "Hi").await;
    assert_eq!(sent.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        !sent.body.to_string().contains(":workspace"),
        "{:?}",
        sent.body
    );
    let bot = gateway
        .send(Call::get(&format!("/api/v1/bots/{chef}")).cookie(&rob))
        .await;
    assert_eq!(bot.body["status"], "refused");
    // Even once the app server behaves, the Bot stays refused.
    gateway.fake.with(|b| b.profile = "bokkie".into());
    let again = say(&gateway, &rob, chef, "Hi").await;
    assert_eq!(again.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(gateway.fake.received("turn/start").is_empty());
    // Other Bots still work.
    let tutor = create_bot(&gateway, &rob, "Tutor").await;
    assert_eq!(
        say(&gateway, &rob, tutor, "Hi").await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn conversations_page_backwards() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let user = UserId(1);
    for i in 0..5 {
        bokkie_gateway::bots::store::insert_message(
            &gateway.state.db,
            gateway.state.clock.now(),
            user,
            bokkie_gateway::bots::store::NewMessage {
                bot: bokkie_protocol::BotId(chef),
                author: bokkie_protocol::api::Author::User,
                text: &format!("m{i}"),
                state: bokkie_protocol::api::MessageState::Complete,
                turn: None,
                kind: bokkie_protocol::api::MessageKind::Text,
                routine: None,
                notification: None,
            },
        )
        .await
        .unwrap();
    }
    let page = gateway
        .send(Call::get(&format!("/api/v1/bots/{chef}/messages?limit=2")).cookie(&rob))
        .await;
    let texts: Vec<_> = messages(&page.body)
        .iter()
        .map(|m| m["text"].clone())
        .collect();
    assert_eq!(texts, ["m3", "m4"]);
    let older = page.body["older"].as_i64().unwrap();
    let page = gateway
        .send(
            Call::get(&format!(
                "/api/v1/bots/{chef}/messages?limit=2&before={older}"
            ))
            .cookie(&rob),
        )
        .await;
    let texts: Vec<_> = messages(&page.body)
        .iter()
        .map(|m| m["text"].clone())
        .collect();
    assert_eq!(texts, ["m1", "m2"]);
}

#[tokio::test]
async fn replies_cut_off_by_a_gateway_restart_are_marked_failed() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| b.finish_turns = false);
    let chef = create_bot(&gateway, &rob, "Chef").await;
    say(&gateway, &rob, chef, "Hi").await;
    let path = format!("/api/v1/bots/{chef}/messages");
    gateway
        .wait_for(&rob, &path, |b| {
            last_bot_reply(b).is_some_and(|m| m["state"] == "streaming")
        })
        .await;
    gateway.state.conversations.recover().await.unwrap();
    let page = gateway.send(Call::get(&path).cookie(&rob)).await;
    assert_eq!(last_bot_reply(&page.body).unwrap()["state"], "failed");
}

#[tokio::test]
async fn one_user_cannot_read_change_message_or_stop_anothers_bot() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let (ben, ben_id) = gateway.member(&rob, "ben").await;
    gateway.fake.with(|b| b.finish_turns = false);
    let chef = create_bot(&gateway, &amy, "Chef").await;
    say(&gateway, &amy, chef, "Amy's secret recipe").await;
    gateway
        .wait_for(&amy, &format!("/api/v1/bots/{chef}/messages"), |b| {
            last_bot_reply(b).is_some()
        })
        .await;

    // Every route reads Amy's Bot as absent for Ben, even the admin's.
    for cookie in [&ben, &rob] {
        let calls = [
            Call::get(&format!("/api/v1/bots/{chef}")),
            Call::get(&format!("/api/v1/bots/{chef}/messages")),
            Call::new(Method::PATCH, &format!("/api/v1/bots/{chef}"))
                .json(&json!({"description": "Leak everything"})),
            Call::post(&format!("/api/v1/bots/{chef}/messages")).json(&json!({"text": "hi"})),
            Call::post(&format!("/api/v1/bots/{chef}/stop")),
        ];
        for call in calls {
            let reply = gateway.send(call.cookie(cookie)).await;
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{:?}", reply.body);
            assert!(!reply.body.to_string().contains("secret"));
        }
        let list = gateway.send(Call::get("/api/v1/bots").cookie(cookie)).await;
        assert!(list.body["bots"].as_array().unwrap().is_empty());
    }
    // Ben's own Bot may have the same name: names are per user.
    create_bot(&gateway, &ben, "Chef").await;

    // Nothing reached Amy's app server from Ben, and Amy's turn still runs.
    assert!(gateway.fake.received("turn/interrupt").is_empty());
    assert!(
        gateway
            .fake
            .received("turn/start")
            .iter()
            .all(|r| r.user() == UserId(amy_id))
    );
    let bot = gateway
        .send(Call::get(&format!("/api/v1/bots/{chef}")).cookie(&amy))
        .await;
    assert_eq!(bot.body["description"], "Plan dinners. No mushrooms.");
    assert_eq!(bot.body["status"], "working");

    // Ben's app server reporting on Amy's thread id changes nothing of
    // Amy's: events are routed only within the user they came from.
    gateway
        .state
        .supervisor
        .runtime(UserId(ben_id))
        .await
        .unwrap();
    let amy_thread = gateway.fake.received("thread/start")[0].params.clone();
    let amy_thread_id = gateway.fake.received("turn/start")[0].params["threadId"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(amy_thread.is_object());
    gateway.fake.inject(
        UserId(ben_id),
        json!({"method": "item/started", "params": {"threadId": amy_thread_id, "turnId": "x",
            "item": {"type": "agentMessage", "id": "forged", "text": "forged reply"}}}),
    );
    gateway.fake.inject(
        UserId(ben_id),
        json!({"method": "turn/completed", "params": {"threadId": amy_thread_id,
            "turn": {"id": "x", "items": [], "status": "failed", "error": null}}}),
    );
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let page = gateway
        .send(Call::get(&format!("/api/v1/bots/{chef}/messages")).cookie(&amy))
        .await;
    assert!(!page.body.to_string().contains("forged"), "{}", page.body);
    assert_eq!(last_bot_reply(&page.body).unwrap()["state"], "streaming");
}

#[tokio::test]
async fn a_turn_that_finishes_before_turn_start_answers_leaves_the_bot_idle() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| b.finish_before_answer = true);
    let chef = create_bot(&gateway, &rob, "Chef").await;
    say(&gateway, &rob, chef, "Quick one").await;
    let path = format!("/api/v1/bots/{chef}/messages");
    gateway
        .wait_for(&rob, &path, |b| {
            last_bot_reply(b).is_some_and(|m| m["state"] == "complete")
        })
        .await;
    gateway
        .wait_for(&rob, &format!("/api/v1/bots/{chef}"), |b| {
            b["status"] == "idle"
        })
        .await;
    // The next message starts a new turn rather than steering a dead one.
    let next = say(&gateway, &rob, chef, "Another").await;
    assert_eq!(next.body["delivery"], "new_turn");
    assert!(gateway.fake.received("turn/steer").is_empty());
}

async fn wait_status(gateway: &Gateway, cookie: &str, bot: i64, status: &str) {
    gateway
        .wait_for(cookie, &format!("/api/v1/bots/{bot}"), |b| {
            b["status"] == status
        })
        .await;
}

#[tokio::test]
async fn a_steer_that_misses_its_turn_starts_a_new_one_with_the_profile() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| b.finish_turns = false);
    let chef = create_bot(&gateway, &rob, "Chef").await;
    say(&gateway, &rob, chef, "Start").await;
    wait_status(&gateway, &rob, chef, "working").await;
    // The profile changes mid-turn; the next message carries it.
    gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/bots/{chef}"))
                .cookie(&rob)
                .json(&json!({"label": "Baker"})),
        )
        .await;
    let steered = say(&gateway, &rob, chef, "Also bread").await;
    assert_eq!(steered.body["delivery"], "steered");
    let steer = &gateway.fake.received("turn/steer")[0];
    assert!(
        steer.params["additionalContext"]["bokkie-profile"]["value"]
            .as_str()
            .unwrap()
            .contains("Baker")
    );
    // When the turn has already ended, the message starts a new turn.
    gateway.fake.with(|b| b.steer_fails = true);
    let next = say(&gateway, &rob, chef, "And cake").await;
    assert_eq!(next.status, StatusCode::OK, "{:?}", next.body);
    assert_eq!(next.body["delivery"], "new_turn");
    let turns = gateway.fake.received("turn/start");
    assert_eq!(turns.len(), 2);
    // The profile was already delivered by the steer.
    assert!(turns[1].params.get("additionalContext").is_none());
}

/// The texts of the conversation, each with its author.
fn said(body: &Value) -> Vec<(String, String)> {
    messages(body)
        .into_iter()
        .map(|m| {
            (
                m["author"].as_str().unwrap().to_owned(),
                m["text"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    expected
        .iter()
        .map(|(a, t)| ((*a).to_owned(), (*t).to_owned()))
        .collect()
}

/// A Bot mid-turn, whose reply has begun, and whose next turns finish on
/// their own.
async fn mid_reply(gateway: &Gateway, cookie: &str, bot: i64) {
    gateway.fake.with(|b| b.finish_turns = false);
    say(gateway, cookie, bot, "Start").await;
    gateway
        .wait_for(cookie, &format!("/api/v1/bots/{bot}/messages"), |b| {
            last_bot_reply(b).is_some_and(|m| m["text"] == "Hello there")
        })
        .await;
    gateway.fake.with(|b| b.finish_turns = true);
}

/// Codex can accept a steer as the turn ends, record the message in the
/// thread and end the turn without asking the model again. The message
/// then gets a turn of its own: on the same thread, counted, with the
/// message as its input.
#[tokio::test]
async fn a_message_steered_in_as_its_turn_ends_gets_a_turn_of_its_own() {
    use common::fake_codex::SteerEnding;
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    mid_reply(&gateway, &rob, chef).await;
    gateway
        .fake
        .with(|b| b.steer_ends_turn = Some(SteerEnding::Unanswered));
    let steered = say(&gateway, &rob, chef, "Also bread").await;
    assert_eq!(steered.body["delivery"], "steered");
    let page = gateway
        .wait_for(&rob, &format!("/api/v1/bots/{chef}/messages"), |b| {
            messages(b).len() == 4 && messages(b)[3]["state"] == "complete"
        })
        .await;
    assert_eq!(
        said(&page),
        pairs(&[
            ("user", "Start"),
            ("bot", "Hello there"),
            ("user", "Also bread"),
            ("bot", "Hello there"),
        ])
    );
    wait_status(&gateway, &rob, chef, "idle").await;
    let turns = gateway.fake.received("turn/start");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1].params["threadId"], turns[0].params["threadId"]);
    assert_eq!(turns[1].params["input"][0]["text"], "Also bread");
    assert_eq!(gateway.fake.received("thread/start").len(), 1);
    let usage = gateway.send(Call::get("/api/v1/usage").cookie(&rob)).await;
    assert_eq!(usage.body["allowance"]["turns_today"], 2);
    // The Bot never showed idle between the turns: its first idle status
    // comes after the second turn's reply began. Status events can arrive
    // a moment after the status reads idle, so wait for one.
    use bokkie_protocol::api::BotStatus;
    use bokkie_protocol::events::{Event, Seq};
    let answer = messages(&page)[3]["id"].as_i64().unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let (began, idle) = loop {
        let log = gateway
            .state
            .events
            .since(UserId(1), Seq::ZERO, 10_000)
            .await
            .unwrap();
        let began = log.iter().position(
            |e| matches!(&e.event, Event::MessageCreated { message } if message.id.0 == answer),
        );
        let idle = log.iter().position(|e| {
            matches!(
                e.event,
                Event::BotStatusChanged {
                    status: BotStatus::Idle,
                    ..
                }
            )
        });
        if let (Some(began), Some(idle)) = (began, idle) {
            break (began, idle);
        }
        assert!(tokio::time::Instant::now() < deadline, "no idle status");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    assert!(began < idle, "idle between the turns");
    // And it showed idle once, as its last status: the second turn's end
    // announces it, and nothing after.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let statuses: Vec<BotStatus> = gateway
        .state
        .events
        .since(UserId(1), Seq::ZERO, 10_000)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::BotStatusChanged { status, .. } => Some(status),
            _ => None,
        })
        .collect();
    assert_eq!(
        statuses.iter().filter(|s| **s == BotStatus::Idle).count(),
        1,
        "{statuses:?}"
    );
    assert_eq!(statuses.last(), Some(&BotStatus::Idle), "{statuses:?}");
    // The gateway knows the message by the client id it steered it with.
    let steer = &gateway.fake.received("turn/steer")[0];
    let message = steered.body["message"]["id"].as_i64().unwrap();
    assert_eq!(
        steer.params["clientUserMessageId"],
        format!("bokkie-message-{message}")
    );
}

/// A steered message that the model answers within its turn gets no
/// other turn.
#[tokio::test]
async fn a_message_steered_in_and_answered_starts_no_other_turn() {
    use common::fake_codex::SteerEnding;
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    mid_reply(&gateway, &rob, chef).await;
    gateway
        .fake
        .with(|b| b.steer_ends_turn = Some(SteerEnding::Answered));
    say(&gateway, &rob, chef, "Also bread").await;
    wait_status(&gateway, &rob, chef, "idle").await;
    let page = gateway
        .send(Call::get(&format!("/api/v1/bots/{chef}/messages")).cookie(&rob))
        .await;
    assert_eq!(
        said(&page.body),
        pairs(&[
            ("user", "Start"),
            ("bot", "Hello there"),
            ("user", "Also bread"),
            ("bot", "Hello there"),
        ])
    );
    // Give a follow-up turn the time to start, if one wrongly would.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(gateway.fake.received("turn/start").len(), 1);
}

/// Two messages steered into a turn that ends without answering them get
/// one turn, begun by the latest: the thread holds both.
#[tokio::test]
async fn two_messages_steered_in_as_their_turn_ends_get_one_turn() {
    use common::fake_codex::SteerEnding;
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    mid_reply(&gateway, &rob, chef).await;
    assert_eq!(
        say(&gateway, &rob, chef, "Also bread").await.body["delivery"],
        "steered"
    );
    assert_eq!(
        say(&gateway, &rob, chef, "And cake").await.body["delivery"],
        "steered"
    );
    gateway
        .fake
        .end_steered_turn(UserId(1), SteerEnding::Unanswered);
    let page = gateway
        .wait_for(&rob, &format!("/api/v1/bots/{chef}/messages"), |b| {
            messages(b).len() == 5 && messages(b)[4]["state"] == "complete"
        })
        .await;
    assert_eq!(
        said(&page),
        pairs(&[
            ("user", "Start"),
            ("bot", "Hello there"),
            ("user", "Also bread"),
            ("user", "And cake"),
            ("bot", "Hello there"),
        ])
    );
    wait_status(&gateway, &rob, chef, "idle").await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let turns = gateway.fake.received("turn/start");
    assert_eq!(turns.len(), 2, "one turn for both");
    assert_eq!(turns[1].params["input"][0]["text"], "And cake");
}

/// With no turn left in the user's allowance, the steered message stays
/// without a reply, as a steer that missed its turn does, and the user is
/// told why.
#[tokio::test]
async fn with_no_turn_left_a_message_steered_in_as_its_turn_ends_stays_unanswered() {
    use bokkie_protocol::events::{Event, Seq};
    use common::fake_codex::SteerEnding;
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let limited = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/admin/users/{amy_id}"))
                .cookie(&rob)
                .json(&json!({"limits": {"daily_turns": 1, "weekly_tokens": null}})),
        )
        .await;
    assert_eq!(limited.status, StatusCode::OK);
    let chef = create_bot(&gateway, &amy, "Chef").await;
    mid_reply(&gateway, &amy, chef).await;
    gateway
        .fake
        .with(|b| b.steer_ends_turn = Some(SteerEnding::Unanswered));
    // Steering needs no turn left.
    let steered = say(&gateway, &amy, chef, "Also bread").await;
    assert_eq!(steered.body["delivery"], "steered");
    let user = UserId(amy_id);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let told = loop {
        let log = gateway
            .state
            .events
            .since(user, Seq::ZERO, 10_000)
            .await
            .unwrap();
        let told = log.into_iter().find_map(|e| match e.event {
            Event::BotError { bot_id, message } if bot_id.0 == chef => Some(message),
            _ => None,
        });
        if let Some(told) = told {
            break told;
        }
        assert!(tokio::time::Instant::now() < deadline, "no error event");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    assert!(told.contains("used your allowance"), "{told}");
    wait_status(&gateway, &amy, chef, "idle").await;
    let page = gateway
        .send(Call::get(&format!("/api/v1/bots/{chef}/messages")).cookie(&amy))
        .await;
    assert_eq!(
        said(&page.body),
        pairs(&[
            ("user", "Start"),
            ("bot", "Hello there"),
            ("user", "Also bread"),
        ])
    );
    assert_eq!(gateway.fake.received("turn/start").len(), 1);
    let usage = gateway.send(Call::get("/api/v1/usage").cookie(&amy)).await;
    assert_eq!(usage.body["allowance"]["turns_today"], 1);
}

/// A Bot owes a turn to a steered message its turn left unanswered: it
/// stays working until that turn starts, which waits for the Bot's sends.
#[tokio::test]
async fn a_bot_owed_a_turn_stays_working_until_it_starts() {
    use common::fake_codex::SteerEnding;
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    mid_reply(&gateway, &rob, chef).await;
    // Hold the Bot's sends, as a message being sent does, while the turn
    // ends with the steered message unanswered.
    say(&gateway, &rob, chef, "Also bread").await;
    let held = gateway
        .state
        .conversations
        .lock_bot(UserId(1), BotId(chef))
        .await;
    gateway
        .fake
        .end_steered_turn(UserId(1), SteerEnding::Unanswered);
    // The Bot stays working: the message is owed a turn.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let status = gateway
        .send(Call::get(&format!("/api/v1/bots/{chef}")).cookie(&rob))
        .await;
    assert_eq!(status.body["status"], "working");
    drop(held);
    wait_status(&gateway, &rob, chef, "idle").await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(gateway.fake.received("turn/start").len(), 2);
}

#[tokio::test]
async fn an_app_server_exit_mid_reply_fails_the_reply() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| b.finish_turns = false);
    let chef = create_bot(&gateway, &rob, "Chef").await;
    say(&gateway, &rob, chef, "Hi").await;
    let path = format!("/api/v1/bots/{chef}/messages");
    gateway
        .wait_for(&rob, &path, |b| {
            last_bot_reply(b).is_some_and(|m| m["text"] == "Hello there")
        })
        .await;
    gateway.fake.crash(UserId(1));
    let runtime = gateway.state.supervisor.running(UserId(1)).await.unwrap();
    let _ = runtime.rate_limits().await;
    let page = gateway
        .wait_for(&rob, &path, |b| {
            last_bot_reply(b).is_some_and(|m| m["state"] == "failed")
        })
        .await;
    assert_eq!(last_bot_reply(&page).unwrap()["text"], "Hello there");
    wait_status(&gateway, &rob, chef, "idle").await;
}

#[tokio::test]
async fn a_violation_mid_reply_refuses_that_bot_but_not_its_siblings() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| b.finish_turns = false);
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let tutor = create_bot(&gateway, &rob, "Tutor").await;
    say(&gateway, &rob, chef, "Hi").await;
    let path = format!("/api/v1/bots/{chef}/messages");
    gateway
        .wait_for(&rob, &path, |b| last_bot_reply(b).is_some())
        .await;
    let thread = gateway.fake.received("thread/start")[0].clone();
    let turn = gateway.fake.received("turn/start")[0].params["threadId"].clone();
    assert!(thread.params.is_object());
    gateway.fake.inject(
        UserId(1),
        json!({"method": "item/started", "params": {"threadId": turn, "turnId": "t",
            "item": {"type": "webSearch", "id": "w1", "query": "x"}}}),
    );
    let page = gateway
        .wait_for(&rob, &path, |b| {
            last_bot_reply(b).is_some_and(|m| m["state"] == "failed")
        })
        .await;
    assert!(last_bot_reply(&page).is_some());
    wait_status(&gateway, &rob, chef, "refused").await;
    // Codex is stopped for the whole gateway, but the sibling Bot is not
    // marked refused: the refusal is not about its thread.
    let sibling = say(&gateway, &rob, tutor, "Hello").await;
    assert_eq!(sibling.status, StatusCode::SERVICE_UNAVAILABLE);
    let tutor_bot = gateway
        .send(Call::get(&format!("/api/v1/bots/{tutor}")).cookie(&rob))
        .await;
    assert_eq!(tutor_bot.body["status"], "idle");
}
