//! Usage: the subscription's windows and per-user allowances.

mod common;

use axum::http::{Method, StatusCode};
use bokkie_gateway::clock::DAY_MS;
use bokkie_protocol::UserId;
use common::{Call, Gateway};
use serde_json::{Value, json};

async fn create_bot(gateway: &Gateway, cookie: &str, name: &str) -> i64 {
    let reply = gateway
        .send(Call::post("/api/v1/bots").cookie(cookie).json(&json!({
            "name": name, "label": "", "description": ""})))
        .await;
    reply.body["id"].as_i64().unwrap()
}

async fn say(gateway: &Gateway, cookie: &str, bot: i64) -> common::Reply {
    gateway
        .send(
            Call::post(&format!("/api/v1/bots/{bot}/messages"))
                .cookie(cookie)
                .json(&json!({"text": "hi"})),
        )
        .await
}

/// Sends a message and waits until the reply has finished.
async fn turn(gateway: &Gateway, cookie: &str, bot: i64) {
    let before = gateway
        .send(Call::get(&format!("/api/v1/bots/{bot}/messages")).cookie(cookie))
        .await
        .body["messages"]
        .as_array()
        .unwrap()
        .len();
    let reply = say(gateway, cookie, bot).await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    gateway
        .wait_for(cookie, &format!("/api/v1/bots/{bot}/messages"), |b| {
            let messages = b["messages"].as_array().unwrap();
            messages.len() == before + 2 && messages.last().unwrap()["state"] == "complete"
        })
        .await;
    gateway
        .wait_for(cookie, &format!("/api/v1/bots/{bot}"), |b| {
            b["status"] == "idle"
        })
        .await;
}

async fn set_limits(gateway: &Gateway, admin: &str, user: i64, limits: Value) {
    let reply = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/admin/users/{user}"))
                .cookie(admin)
                .json(&json!({"limits": limits})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
}

async fn usage(gateway: &Gateway, cookie: &str) -> Value {
    gateway
        .send(Call::get("/api/v1/usage").cookie(cookie))
        .await
        .body
}

async fn setup() -> (Gateway, String) {
    let gateway = Gateway::new().await;
    gateway.route().await;
    let admin = gateway.admin("rob").await;
    (gateway, admin)
}

#[tokio::test]
async fn a_members_daily_turns_are_enforced_and_refill() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    set_limits(
        &gateway,
        &rob,
        amy_id,
        json!({"daily_turns": 2, "weekly_tokens": null}),
    )
    .await;
    let chef = create_bot(&gateway, &amy, "Chef").await;
    turn(&gateway, &amy, chef).await;
    turn(&gateway, &amy, chef).await;
    let refused = say(&gateway, &amy, chef).await;
    assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(refused.code(), "limit_reached");
    // The refused message was not stored, and no turn started.
    assert_eq!(gateway.fake.received("turn/start").len(), 2);
    let page = gateway
        .send(Call::get(&format!("/api/v1/bots/{chef}/messages")).cookie(&amy))
        .await;
    assert_eq!(page.body["messages"].as_array().unwrap().len(), 4);
    let meter = usage(&gateway, &amy).await;
    assert_eq!(meter["allowance"]["turns_today"], 2);
    assert_eq!(meter["allowance"]["exhausted"], true);
    assert_eq!(meter["allowance"]["limits"]["daily_turns"], 2);

    // The window rolls.
    gateway.state.clock.advance(DAY_MS);
    assert_eq!(say(&gateway, &amy, chef).await.status, StatusCode::OK);
}

#[tokio::test]
async fn weekly_tokens_are_measured_from_codex_and_enforced() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    set_limits(
        &gateway,
        &rob,
        amy_id,
        json!({"daily_turns": null, "weekly_tokens": 200}),
    )
    .await;
    let chef = create_bot(&gateway, &amy, "Chef").await;
    turn(&gateway, &amy, chef).await;
    // Each turn grows the thread's total by 120; only growth counts.
    assert_eq!(
        usage(&gateway, &amy).await["allowance"]["tokens_this_week"],
        120
    );
    turn(&gateway, &amy, chef).await;
    let meter = usage(&gateway, &amy).await;
    assert_eq!(meter["allowance"]["tokens_this_week"], 240);
    assert_eq!(meter["allowance"]["exhausted"], true);
    assert_eq!(say(&gateway, &amy, chef).await.code(), "limit_reached");
    gateway.state.clock.advance(7 * DAY_MS);
    assert_eq!(
        usage(&gateway, &amy).await["allowance"]["tokens_this_week"],
        0
    );
}

#[tokio::test]
async fn steering_is_not_a_turn_but_needs_tokens() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    set_limits(
        &gateway,
        &rob,
        amy_id,
        json!({"daily_turns": 1, "weekly_tokens": null}),
    )
    .await;
    gateway.fake.with(|b| b.finish_turns = false);
    let chef = create_bot(&gateway, &amy, "Chef").await;
    assert_eq!(say(&gateway, &amy, chef).await.body["delivery"], "new_turn");
    gateway
        .wait_for(&amy, &format!("/api/v1/bots/{chef}"), |b| {
            b["status"] == "working"
        })
        .await;
    // The turn allowance is used, but steering the running turn is fine.
    let steer = say(&gateway, &amy, chef).await;
    assert_eq!(steer.status, StatusCode::OK, "{:?}", steer.body);
    assert_eq!(steer.body["delivery"], "steered");
    assert_eq!(usage(&gateway, &amy).await["allowance"]["turns_today"], 1);
}

#[tokio::test]
async fn the_meter_shows_the_subscription_windows() {
    let (gateway, rob) = setup().await;
    let meter = usage(&gateway, &rob).await;
    let bucket = &meter["subscription"][0];
    assert_eq!(bucket["id"], "codex");
    assert_eq!(bucket["primary"]["used_percent"], 12);
    assert_eq!(bucket["primary"]["window_minutes"], 10080);
    // Codex reports seconds; the wire carries milliseconds.
    assert_eq!(bucket["primary"]["resets_at"], 1_800_600_000_000i64);
    assert_eq!(bucket["reached"], false);
    assert_eq!(meter["allowance"]["exhausted"], false);

    // A rate-limit update from the app server replaces the bucket.
    gateway.fake.inject(
        UserId(1),
        json!({"method": "account/rateLimits/updated", "params": {"rateLimits": {
            "limitId": "codex", "limitName": null, "planType": "pro",
            "primary": {"usedPercent": 100, "windowDurationMins": 10080, "resetsAt": 1_800_700_000},
            "secondary": null, "rateLimitReachedType": "rate_limit_reached"}}}),
    );
    let meter = gateway
        .wait_for(&rob, "/api/v1/usage", |b| {
            b["subscription"][0]["reached"] == true
        })
        .await;
    assert_eq!(meter["subscription"][0]["primary"]["used_percent"], 100);
    // Clients hear about it on the event socket.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let events = gateway
            .state
            .events
            .since(UserId(1), bokkie_protocol::events::Seq(0), 100)
            .await
            .unwrap();
        if events.iter().any(|e| {
            matches!(
                &e.event,
                bokkie_protocol::events::Event::UsageUpdated { usage }
                    if usage.subscription.first().is_some_and(|b| b.reached)
            )
        }) {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "no usage event");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn usage_is_per_user_and_private() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let (ben, ben_id) = gateway.member(&rob, "ben").await;
    set_limits(
        &gateway,
        &rob,
        ben_id,
        json!({"daily_turns": 1, "weekly_tokens": null}),
    )
    .await;
    let chef = create_bot(&gateway, &amy, "Chef").await;
    turn(&gateway, &amy, chef).await;
    turn(&gateway, &amy, chef).await;
    // Amy's turns do not count against Ben.
    let bens = usage(&gateway, &ben).await;
    assert_eq!(bens["allowance"]["turns_today"], 0);
    assert_eq!(bens["allowance"]["exhausted"], false);
    assert_eq!(usage(&gateway, &amy).await["allowance"]["turns_today"], 2);
    // Members cannot read another account's usage; an admin can, as counts.
    let peek = gateway
        .send(Call::get(&format!("/api/v1/admin/users/{amy_id}/usage")).cookie(&ben))
        .await;
    assert_eq!(peek.status, StatusCode::FORBIDDEN);
    let admin_view = gateway
        .send(Call::get(&format!("/api/v1/admin/users/{amy_id}/usage")).cookie(&rob))
        .await;
    assert_eq!(admin_view.status, StatusCode::OK);
    assert_eq!(admin_view.body["turns_today"], 2);
    assert!(admin_view.body.get("messages").is_none());
    let missing = gateway
        .send(Call::get("/api/v1/admin/users/999/usage").cookie(&rob))
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

/// Writes from many tasks at once must all land: a read-then-write
/// transaction that is not immediate can fail outright under WAL.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_usage_writes_are_not_lost() {
    let (gateway, rob) = setup().await;
    let bots = [
        create_bot(&gateway, &rob, "One").await,
        create_bot(&gateway, &rob, "Two").await,
    ];
    let user = UserId(1);
    let clock = gateway.state.clock.clone();
    let mut tasks = Vec::new();
    for bot in bots {
        let (events, clock) = (gateway.state.events.clone(), clock.clone());
        // Each thread's total grows by 10 per report.
        tasks.push(tokio::spawn(async move {
            for total in (10..=500).step_by(10) {
                let mut batch = events.begin(user).await.unwrap();
                bokkie_gateway::usage::record_tokens(
                    &mut batch,
                    clock.now(),
                    user,
                    bokkie_protocol::BotId(bot),
                    total,
                )
                .await
                .unwrap();
                batch.commit().await.unwrap();
            }
        }));
    }
    // Meanwhile, other writers.
    let events = gateway.state.events.clone();
    tasks.push(tokio::spawn(async move {
        for _ in 0..200 {
            events
                .append(
                    user,
                    bokkie_protocol::events::Event::BotError {
                        bot_id: bokkie_protocol::BotId(1),
                        message: "noise".into(),
                    },
                )
                .await
                .unwrap();
        }
    }));
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(
        usage(&gateway, &rob).await["allowance"]["tokens_this_week"],
        1000
    );
}

/// Sends to several Bots at once, say from two devices, must not all pass
/// the turn check before any of them is counted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_sends_to_several_bots_cannot_overrun_the_turn_limit() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    set_limits(
        &gateway,
        &rob,
        amy_id,
        json!({"daily_turns": 1, "weekly_tokens": null}),
    )
    .await;
    let mut bots = Vec::new();
    for name in ["One", "Two", "Three", "Four"] {
        bots.push(create_bot(&gateway, &amy, name).await);
    }
    let calls = bots
        .iter()
        .map(|bot| {
            Call::post(&format!("/api/v1/bots/{bot}/messages"))
                .cookie(&amy)
                .json(&json!({"text": "hi"}))
        })
        .collect();
    let replies = gateway.send_all(calls).await;
    let accepted = replies
        .iter()
        .filter(|r| r.status == StatusCode::OK)
        .count();
    assert_eq!(accepted, 1, "{replies:?}");
    assert!(
        replies
            .iter()
            .filter(|r| r.status != StatusCode::OK)
            .all(|r| r.code() == "limit_reached")
    );
    assert_eq!(gateway.fake.received("turn/start").len(), 1);
    // Only the accepted message was stored.
    let mut stored = 0;
    for bot in &bots {
        let page = gateway
            .send(Call::get(&format!("/api/v1/bots/{bot}/messages")).cookie(&amy))
            .await;
        stored += page.body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["author"] == "user")
            .count();
    }
    assert_eq!(stored, 1);
}

/// Makes every insert into `table` fail, as a full disk would.
async fn break_inserts(gateway: &Gateway, table: &str) {
    sqlx::query(&format!(
        "CREATE TRIGGER broken_{table} BEFORE INSERT ON {table}
         BEGIN SELECT RAISE(ABORT, 'disk full'); END"
    ))
    .execute(&gateway.state.db)
    .await
    .unwrap();
}

async fn mend_inserts(gateway: &Gateway, table: &str) {
    sqlx::query(&format!("DROP TRIGGER broken_{table}"))
        .execute(&gateway.state.db)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_turn_is_counted_before_codex_is_asked_to_start_it() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    // Counting the turn fails, so Codex is never asked to start it.
    break_inserts(&gateway, "turns").await;
    let failed = say(&gateway, &rob, chef).await;
    assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(gateway.fake.received("turn/start").is_empty());
    mend_inserts(&gateway, "turns").await;
    turn(&gateway, &rob, chef).await;
    assert_eq!(usage(&gateway, &rob).await["allowance"]["turns_today"], 1);
}

#[tokio::test]
async fn a_turn_codex_started_stays_counted_when_its_answer_is_lost() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    // The app server starts the turn and exits before answering.
    gateway.fake.with(|b| b.hang_up_on_turn = true);
    let lost = say(&gateway, &rob, chef).await;
    assert_eq!(
        lost.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{:?}",
        lost.body
    );
    assert_eq!(gateway.fake.received("turn/start").len(), 1);
    assert_eq!(usage(&gateway, &rob).await["allowance"]["turns_today"], 1);
}

#[tokio::test]
async fn a_turn_codex_refuses_to_start_is_not_counted() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    gateway.fake.with(|b| b.reject_turns = true);
    let refused = say(&gateway, &rob, chef).await;
    assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(gateway.fake.received("turn/start").len(), 1);
    let allowance = &usage(&gateway, &rob).await["allowance"];
    assert_eq!(allowance["turns_today"], 0, "{allowance}");
    // The meter's last event agrees.
    let log = gateway
        .state
        .events
        .since(UserId(1), bokkie_protocol::events::Seq(0), 1_000)
        .await
        .unwrap();
    let last_usage = log
        .iter()
        .rev()
        .find_map(|e| match &e.event {
            bokkie_protocol::events::Event::UsageUpdated { usage } => Some(usage.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(last_usage.allowance.turns_today, 0);
}

/// The user's usage meter in their latest `usage_updated` event, if any.
async fn last_usage_event(gateway: &Gateway, user: i64) -> Option<bokkie_protocol::api::Usage> {
    let log = gateway
        .state
        .events
        .since(UserId(user), bokkie_protocol::events::Seq(0), 10_000)
        .await
        .unwrap();
    log.into_iter().rev().find_map(|e| match e.event {
        bokkie_protocol::events::Event::UsageUpdated { usage } => Some(usage),
        _ => None,
    })
}

#[tokio::test]
async fn an_admin_changing_limits_announces_the_new_allowance() {
    let (gateway, rob) = setup().await;
    let (_amy, amy_id) = gateway.member(&rob, "amy").await;
    assert!(last_usage_event(&gateway, amy_id).await.is_none());
    // A change that leaves the limits alone announces no usage.
    let renamed = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/admin/users/{amy_id}"))
                .cookie(&rob)
                .json(&json!({"display_name": "Amy"})),
        )
        .await;
    assert_eq!(renamed.status, StatusCode::OK);
    assert!(last_usage_event(&gateway, amy_id).await.is_none());

    set_limits(
        &gateway,
        &rob,
        amy_id,
        json!({"daily_turns": 0, "weekly_tokens": 5000}),
    )
    .await;
    let announced = last_usage_event(&gateway, amy_id).await.unwrap();
    assert_eq!(announced.allowance.limits.daily_turns, Some(0));
    assert_eq!(announced.allowance.limits.weekly_tokens, Some(5000));
    assert!(announced.allowance.exhausted);
    // Only Amy hears of it.
    assert!(last_usage_event(&gateway, 1).await.is_none());
}

async fn rows(gateway: &Gateway, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(&gateway.state.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn rolled_over_allowance_windows_are_announced_and_pruned() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    set_limits(
        &gateway,
        &rob,
        amy_id,
        json!({"daily_turns": 1, "weekly_tokens": null}),
    )
    .await;
    let chef = create_bot(&gateway, &amy, "Chef").await;
    turn(&gateway, &amy, chef).await;
    assert!(
        last_usage_event(&gateway, amy_id)
            .await
            .unwrap()
            .allowance
            .exhausted
    );
    let clock = &gateway.state.clock;
    let conversations = &gateway.state.conversations;

    // Nothing has left a window yet.
    let start = clock.now();
    clock.advance(DAY_MS - 1);
    assert!(
        conversations
            .roll_over(start, clock.now())
            .await
            .unwrap()
            .is_empty()
    );

    // The turn leaves the day: Amy hears that she has turns again.
    let before = clock.now();
    clock.advance(1);
    let announced = conversations.roll_over(before, clock.now()).await.unwrap();
    assert_eq!(announced, [UserId(amy_id)]);
    let usage = last_usage_event(&gateway, amy_id).await.unwrap();
    assert_eq!(usage.allowance.turns_today, 0);
    assert_eq!(usage.allowance.tokens_this_week, 120);
    assert!(!usage.allowance.exhausted);
    assert_eq!(
        rows(&gateway, "turns").await,
        0,
        "a turn out of its window is pruned"
    );
    assert_eq!(rows(&gateway, "token_usage").await, 1);

    // A week on, the tokens leave too.
    let before = clock.now();
    clock.advance(6 * DAY_MS);
    let announced = conversations.roll_over(before, clock.now()).await.unwrap();
    assert_eq!(announced, [UserId(amy_id)]);
    let usage = last_usage_event(&gateway, amy_id).await.unwrap();
    assert_eq!(usage.allowance.tokens_this_week, 0);
    assert_eq!(rows(&gateway, "token_usage").await, 0);
}

#[tokio::test]
async fn a_subscription_window_that_resets_is_read_again_and_announced() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    turn(&gateway, &rob, chef).await;
    // The meter caches the window, which resets at 1_800_600_000 s.
    let meter = usage(&gateway, &rob).await;
    assert_eq!(meter["subscription"][0]["primary"]["used_percent"], 12);
    gateway.fake.with(|b| {
        b.rate_limits = json!({
            "rateLimits": {"limitId": "codex", "limitName": null, "planType": "pro",
                "primary": {"usedPercent": 0, "windowDurationMins": 10080, "resetsAt": 1_801_200_000},
                "secondary": null, "rateLimitReachedType": null},
            "rateLimitsByLimitId": null,
        })
    });
    let clock = &gateway.state.clock;
    let before = clock.now();
    clock.advance(1_800_600_000_000 - before.0);
    let announced = gateway
        .state
        .conversations
        .roll_over(before, clock.now())
        .await
        .unwrap();
    assert_eq!(announced, [UserId(1)]);
    let usage = last_usage_event(&gateway, 1).await.unwrap();
    let primary = usage.subscription[0].primary.as_ref().unwrap();
    assert_eq!(primary.used_percent, 0);
    assert_eq!(
        primary.resets_at,
        Some(bokkie_protocol::Timestamp(1_801_200_000_000))
    );
}

/// Every `bot_error` message in the user's log.
async fn bot_errors(gateway: &Gateway, user: i64) -> Vec<String> {
    gateway
        .state
        .events
        .since(UserId(user), bokkie_protocol::events::Seq(0), 10_000)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            bokkie_protocol::events::Event::BotError { message, .. } => Some(message),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_reply_that_uses_up_the_token_allowance_overshoots_by_one_response() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    set_limits(
        &gateway,
        &rob,
        amy_id,
        json!({"daily_turns": null, "weekly_tokens": 100}),
    )
    .await;
    // As with the pinned Codex, a chat-only turn reports its 120 tokens
    // once the model's response is complete.
    let chef = create_bot(&gateway, &amy, "Chef").await;
    turn(&gateway, &amy, chef).await;
    let page = gateway
        .send(Call::get(&format!("/api/v1/bots/{chef}/messages")).cookie(&amy))
        .await;
    let reply = page.body["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(reply["state"], "complete", "the reply was not cut short");
    assert_eq!(reply["text"], "Hello there");
    // The overshoot is that one response, and it is counted.
    let meter = usage(&gateway, &amy).await;
    assert_eq!(meter["allowance"]["tokens_this_week"], 120);
    assert_eq!(meter["allowance"]["exhausted"], true);
    // Nothing claims a stop that did not happen, and nothing more runs.
    assert!(bot_errors(&gateway, amy_id).await.is_empty());
    assert_eq!(say(&gateway, &amy, chef).await.code(), "limit_reached");
}

#[tokio::test]
async fn a_turn_still_running_when_the_token_allowance_is_used_up_is_stopped() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    set_limits(
        &gateway,
        &rob,
        amy_id,
        json!({"daily_turns": null, "weekly_tokens": 100}),
    )
    .await;
    // Rob's turn runs on, unreported.
    gateway.fake.with(|b| b.finish_turns = false);
    let robs = create_bot(&gateway, &rob, "Butler").await;
    assert_eq!(say(&gateway, &rob, robs).await.status, StatusCode::OK);
    gateway
        .wait_for(&rob, &format!("/api/v1/bots/{robs}"), |b| {
            b["status"] == "working"
        })
        .await;
    // Amy's turn reports 120 tokens and would go on, as a turn that asks
    // the model again does.
    gateway.fake.with(|b| b.usage_mid_turn = true);
    let chef = create_bot(&gateway, &amy, "Chef").await;
    assert_eq!(say(&gateway, &amy, chef).await.status, StatusCode::OK);
    gateway
        .wait_for(&amy, &format!("/api/v1/bots/{chef}/messages"), |b| {
            b["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["author"] == "bot" && m["state"] == "interrupted")
        })
        .await;
    // Only Amy's turn is stopped, once, and she is told why.
    let interrupts = gateway.fake.received("turn/interrupt");
    assert_eq!(interrupts.len(), 1);
    assert_eq!(interrupts[0].user, UserId(amy_id));
    let errors = bot_errors(&gateway, amy_id).await;
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("allowance"));
    assert!(bot_errors(&gateway, 1).await.is_empty());
    let butler = gateway
        .send(Call::get(&format!("/api/v1/bots/{robs}")).cookie(&rob))
        .await;
    assert_eq!(butler.body["status"], "working");
    assert_eq!(say(&gateway, &amy, chef).await.code(), "limit_reached");
}

#[tokio::test]
async fn a_turn_under_the_token_allowance_is_not_stopped() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    set_limits(
        &gateway,
        &rob,
        amy_id,
        json!({"daily_turns": null, "weekly_tokens": 1_000}),
    )
    .await;
    gateway.fake.with(|b| b.usage_mid_turn = true);
    let chef = create_bot(&gateway, &amy, "Chef").await;
    turn(&gateway, &amy, chef).await;
    assert!(gateway.fake.received("turn/interrupt").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hung_codex_call_does_not_hold_up_sends_to_other_bots() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let tutor = create_bot(&gateway, &rob, "Tutor").await;
    // Chef's turn/start is never answered.
    gateway.fake.with(|b| b.hang_turns = true);
    let hung = gateway.spawn(
        Call::post(&format!("/api/v1/bots/{chef}/messages"))
            .cookie(&rob)
            .json(&json!({"text": "hi"})),
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while gateway.fake.received("turn/start").is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "Chef's turn was never started"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    gateway.fake.with(|b| b.hang_turns = false);
    let other = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        say(&gateway, &rob, tutor),
    )
    .await
    .expect("a send to another Bot does not wait for the hung call");
    assert_eq!(other.status, StatusCode::OK, "{:?}", other.body);
    assert!(!hung.is_finished());
    hung.abort();
}

#[tokio::test]
async fn a_turn_whose_answer_cannot_be_read_stays_counted() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    // Codex starts the turn, but its answer is unreadable: the turn may run.
    gateway.fake.with(|b| b.garble_turn_answers = true);
    let unread = say(&gateway, &rob, chef).await;
    assert_eq!(
        unread.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{:?}",
        unread.body
    );
    assert_eq!(usage(&gateway, &rob).await["allowance"]["turns_today"], 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_rollover_pass_is_retried_from_where_it_started() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    set_limits(
        &gateway,
        &rob,
        amy_id,
        json!({"daily_turns": 1, "weekly_tokens": null}),
    )
    .await;
    let chef = create_bot(&gateway, &amy, "Chef").await;
    turn(&gateway, &amy, chef).await;
    let db = gateway.state.db.clone();
    let rename = |from: &'static str, to: &'static str| {
        let db = db.clone();
        async move {
            sqlx::query(&format!("ALTER TABLE {from} RENAME TO {to}"))
                .execute(&db)
                .await
        }
    };
    tokio::spawn(
        gateway
            .state
            .conversations
            .clone()
            .run_rollovers(std::time::Duration::from_millis(20)),
    );
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    // The passes fail while the turn leaves its window.
    rename("token_usage", "token_usage_away").await.unwrap();
    gateway.state.clock.advance(DAY_MS);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    rename("token_usage_away", "token_usage").await.unwrap();
    // A later pass still covers the moment it left.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let usage = last_usage_event(&gateway, amy_id).await.unwrap();
        if usage.allowance.turns_today == 0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the rollover is announced"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}
