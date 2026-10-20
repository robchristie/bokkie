//! Routines: the API, the scheduler's rules on the manual clock, and
//! isolation between users.
//!
//! The gateway's clock starts at 2027-01-15T08:00Z, which is 18:30 on a
//! Friday in Adelaide (+10:30), every account's default time zone.

mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use bokkie_gateway::clock::{DAY_MS, HOUR_MS, MINUTE_MS};
use bokkie_gateway::codex::Owner;
use bokkie_gateway::routines::{Routines, store};
use bokkie_protocol::api::{BotStatus, RunState};
use bokkie_protocol::events::{Event, Seq};
use bokkie_protocol::{BotId, UserId};
use common::{Call, Gateway};
use serde_json::{Value, json};

/// 2027-01-15T08:00Z, the manual clock's start: 18:30 in Adelaide.
const START: i64 = 1_800_000_000_000;

async fn setup() -> (Gateway, String) {
    let gateway = Gateway::new().await;
    gateway.route().await;
    let admin = gateway.admin("rob").await;
    (gateway, admin)
}

async fn create_bot(gateway: &Gateway, cookie: &str, name: &str) -> i64 {
    let reply = gateway
        .send(Call::post("/api/v1/bots").cookie(cookie).json(&json!({
            "name": name, "label": "", "description": ""})))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{:?}", reply.body);
    reply.body["id"].as_i64().unwrap()
}

fn routines_path(bot: i64) -> String {
    format!("/api/v1/bots/{bot}/routines")
}

fn routine_path(bot: i64, routine: i64) -> String {
    format!("/api/v1/bots/{bot}/routines/{routine}")
}

async fn create_routine(gateway: &Gateway, cookie: &str, bot: i64, body: Value) -> Value {
    let reply = gateway
        .send(Call::post(&routines_path(bot)).cookie(cookie).json(&body))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{:?}", reply.body);
    reply.body
}

/// A daily routine at `at`, Adelaide time.
async fn daily(gateway: &Gateway, cookie: &str, bot: i64, name: &str, at: &str) -> Value {
    create_routine(
        gateway,
        cookie,
        bot,
        json!({"name": name, "prompt": format!("Run {name}."),
               "schedule": {"repeat": "daily", "at": at}}),
    )
    .await
}

async fn patch_routine(
    gateway: &Gateway,
    cookie: &str,
    bot: i64,
    routine: i64,
    body: Value,
) -> common::Reply {
    gateway
        .send(
            Call::new(Method::PATCH, &routine_path(bot, routine))
                .cookie(cookie)
                .json(&body),
        )
        .await
}

async fn run_now(gateway: &Gateway, cookie: &str, bot: i64, routine: i64) -> common::Reply {
    gateway
        .send(Call::post(&format!("{}/run", routine_path(bot, routine))).cookie(cookie))
        .await
}

async fn runs(gateway: &Gateway, cookie: &str, bot: i64, routine: i64) -> Vec<Value> {
    let reply = gateway
        .send(Call::get(&format!("{}/runs", routine_path(bot, routine))).cookie(cookie))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    reply.body["runs"].as_array().unwrap().clone()
}

/// Waits until the routine's newest run satisfies `done`, and returns it.
async fn wait_run(
    gateway: &Gateway,
    cookie: &str,
    bot: i64,
    routine: i64,
    done: impl Fn(&Value) -> bool,
) -> Value {
    let body = gateway
        .wait_for(
            cookie,
            &format!("{}/runs", routine_path(bot, routine)),
            |b| {
                b["runs"]
                    .as_array()
                    .and_then(|r| r.first())
                    .is_some_and(&done)
            },
        )
        .await;
    body["runs"][0].clone()
}

async fn routine(gateway: &Gateway, cookie: &str, bot: i64, routine: i64) -> Value {
    let reply = gateway
        .send(Call::get(&routine_path(bot, routine)).cookie(cookie))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    reply.body
}

async fn messages(gateway: &Gateway, cookie: &str, bot: i64) -> Vec<Value> {
    gateway
        .send(Call::get(&format!("/api/v1/bots/{bot}/messages")).cookie(cookie))
        .await
        .body["messages"]
        .as_array()
        .unwrap()
        .clone()
}

/// The scheduler's pass at the clock's time, and the starts and deadlines
/// it set off.
async fn pass(gateway: &Gateway) {
    let routines = &gateway.state.routines;
    routines.pass(gateway.state.clock.now()).await.unwrap();
    routines.settle().await;
}

/// Runs the scheduler's pass until the routine's newest run satisfies
/// `done`, as the scheduler's loop would when a Bot's turn ends. Several
/// runs of one Bot start one at a time, whichever comes first.
async fn pass_until(
    gateway: &Gateway,
    cookie: &str,
    bot: i64,
    routine: i64,
    done: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        pass(gateway).await;
        let newest = runs(gateway, cookie, bot, routine).await.first().cloned();
        if let Some(run) = newest.filter(|r| done(r)) {
            return run;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting on routine {routine}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

fn advance(gateway: &Gateway, ms: i64) {
    gateway.state.clock.advance(ms);
}

/// Moves the clock to `at` milliseconds after the start.
fn set_clock(gateway: &Gateway, at: i64) {
    let now = gateway.state.clock.now_ms();
    assert!(START + at >= now, "the clock only moves forward");
    gateway.state.clock.advance(START + at - now);
}

async fn events(gateway: &Gateway, user: i64) -> Vec<Event> {
    gateway
        .state
        .events
        .since(UserId(user), Seq(0), 10_000)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect()
}

fn turn_starts(gateway: &Gateway) -> Vec<Value> {
    gateway
        .fake
        .received("turn/start")
        .into_iter()
        .map(|r| r.params)
        .collect()
}

#[tokio::test]
async fn routines_are_created_listed_edited_paused_resumed_and_deleted() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;

    let created = create_routine(
        &gateway,
        &rob,
        chef,
        json!({"name": " Morning digest ", "prompt": "What is new?",
               "schedule": {"repeat": "days", "days": ["mon", "fri"], "at": "07:00"},
               "push": "notify_only"}),
    )
    .await;
    let id = created["id"].as_i64().unwrap();
    assert_eq!(created["bot_id"], chef);
    assert_eq!(created["name"], "Morning digest");
    assert_eq!(created["push"], "notify_only");
    assert_eq!(created["paused"], false);
    assert_eq!(created["failures"], 0);
    // Friday 18:30 now: Monday 07:00 +10:30 is next.
    assert_eq!(
        created["next_run_at"],
        START + 2 * DAY_MS + 12 * HOUR_MS + 30 * MINUTE_MS
    );

    let list = gateway
        .send(Call::get(&routines_path(chef)).cookie(&rob))
        .await;
    assert_eq!(list.body["routines"].as_array().unwrap().len(), 1);
    assert_eq!(list.body["routines"][0], created);
    let all = gateway
        .send(Call::get("/api/v1/routines").cookie(&rob))
        .await;
    assert_eq!(all.body["routines"][0], created);
    assert_eq!(routine(&gateway, &rob, chef, id).await, created);

    // A new schedule is scheduled from now.
    let edited = patch_routine(
        &gateway,
        &rob,
        chef,
        id,
        json!({"prompt": "What is new today?",
               "schedule": {"repeat": "hours", "every": 2, "at": "07:15"}}),
    )
    .await;
    assert_eq!(edited.status, StatusCode::OK, "{:?}", edited.body);
    assert_eq!(edited.body["prompt"], "What is new today?");
    assert_eq!(edited.body["name"], "Morning digest");
    assert_eq!(edited.body["next_run_at"], START + 45 * MINUTE_MS, "19:15");

    let paused = patch_routine(&gateway, &rob, chef, id, json!({"paused": true})).await;
    assert_eq!(paused.body["paused"], true);
    assert_eq!(paused.body["next_run_at"], Value::Null);
    advance(&gateway, DAY_MS);
    let resumed = patch_routine(&gateway, &rob, chef, id, json!({"paused": false})).await;
    assert_eq!(resumed.body["paused"], false);
    assert_eq!(
        resumed.body["next_run_at"],
        START + DAY_MS + 45 * MINUTE_MS,
        "resuming schedules from now, not from when it paused"
    );

    let deleted = gateway
        .send(Call::new(Method::DELETE, &routine_path(chef, id)).cookie(&rob))
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    let gone = gateway
        .send(Call::get(&routine_path(chef, id)).cookie(&rob))
        .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);

    // Each change was announced, in order.
    let routine_events: Vec<Event> = events(&gateway, 1)
        .await
        .into_iter()
        .filter(|e| {
            matches!(
                e,
                Event::RoutineCreated { .. }
                    | Event::RoutineUpdated { .. }
                    | Event::RoutineDeleted { .. }
            )
        })
        .collect();
    assert_eq!(routine_events.len(), 5, "{routine_events:?}");
    assert!(matches!(&routine_events[0], Event::RoutineCreated { routine } if routine.id.0 == id));
    match &routine_events[2] {
        Event::RoutineUpdated { routine } => assert!(routine.paused),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        routine_events[4],
        Event::RoutineDeleted { bot_id, routine_id } if bot_id.0 == chef && routine_id.0 == id
    ));
}

#[tokio::test]
async fn routines_are_checked_and_limited_to_twenty() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let bad = [
        json!({"name": "", "prompt": "x", "schedule": {"repeat": "daily", "at": "07:00"}}),
        json!({"name": "a\nb", "prompt": "x", "schedule": {"repeat": "daily", "at": "07:00"}}),
        json!({"name": "x".repeat(61), "prompt": "x", "schedule": {"repeat": "daily", "at": "07:00"}}),
        json!({"name": "n", "prompt": "", "schedule": {"repeat": "daily", "at": "07:00"}}),
        json!({"name": "n", "prompt": "x".repeat(8_001), "schedule": {"repeat": "daily", "at": "07:00"}}),
        json!({"name": "n", "prompt": "x", "schedule": {"repeat": "daily", "at": "7:00"}}),
        json!({"name": "n", "prompt": "x", "schedule": {"repeat": "daily", "at": "24:00"}}),
        json!({"name": "n", "prompt": "x", "schedule": {"repeat": "days", "days": [], "at": "07:00"}}),
        json!({"name": "n", "prompt": "x", "schedule": {"repeat": "hours", "every": 5, "at": "07:00"}}),
        json!({"name": "n", "prompt": "x", "schedule": {"repeat": "minutes", "every": 5, "at": "07:00"}}),
    ];
    for body in bad {
        let reply = gateway
            .send(Call::post(&routines_path(chef)).cookie(&rob).json(&body))
            .await;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{body}: {:?}",
            reply.body
        );
    }
    // Names are unique per Bot, whatever their case.
    daily(&gateway, &rob, chef, "Digest", "07:00").await;
    let twice = gateway
        .send(Call::post(&routines_path(chef)).cookie(&rob).json(&json!({
            "name": "DIGEST", "prompt": "x", "schedule": {"repeat": "daily", "at": "08:00"}})))
        .await;
    assert_eq!(twice.status, StatusCode::CONFLICT);
    // Another Bot may use the name.
    let baker = create_bot(&gateway, &rob, "Baker").await;
    daily(&gateway, &rob, baker, "Digest", "07:00").await;
    // Twenty routines across the user's Bots, and no more.
    for i in 2..20 {
        daily(&gateway, &rob, chef, &format!("Routine {i}"), "07:00").await;
    }
    let over = gateway
        .send(Call::post(&routines_path(baker)).cookie(&rob).json(&json!({
            "name": "One too many", "prompt": "x", "schedule": {"repeat": "daily", "at": "08:00"}})))
        .await;
    assert_eq!(over.status, StatusCode::CONFLICT, "{:?}", over.body);
    assert!(over.body["message"].as_str().unwrap().contains("20"));
    // A routine on a Bot that does not exist.
    let missing = gateway
        .send(Call::post(&routines_path(999)).cookie(&rob).json(&json!({
            "name": "x", "prompt": "x", "schedule": {"repeat": "daily", "at": "08:00"}})))
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_time_zone_is_the_users_own_and_reschedules_their_routines() {
    let (gateway, rob) = setup().await;
    let session = gateway
        .send(Call::get("/api/v1/session").cookie(&rob))
        .await;
    assert_eq!(session.body["user"]["time_zone"], "Australia/Adelaide");
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let digest = daily(&gateway, &rob, chef, "Digest", "19:00").await;
    let id = digest["id"].as_i64().unwrap();
    assert_eq!(digest["next_run_at"], START + 30 * MINUTE_MS);

    for bad in ["", "Mars/Olympus", "+10:30", "Australia/Adelaide/../x"] {
        let reply = gateway
            .send(
                Call::new(Method::PUT, "/api/v1/session/time-zone")
                    .cookie(&rob)
                    .json(&json!({"time_zone": bad})),
            )
            .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    // 08:00Z is 08:00 in London in January: today's 19:00 there is next.
    let set = gateway
        .send(
            Call::new(Method::PUT, "/api/v1/session/time-zone")
                .cookie(&rob)
                .json(&json!({"time_zone": "europe/london"})),
        )
        .await;
    assert_eq!(set.status, StatusCode::OK, "{:?}", set.body);
    assert_eq!(set.body["user"]["time_zone"], "Europe/London");
    let session = gateway
        .send(Call::get("/api/v1/session").cookie(&rob))
        .await;
    assert_eq!(session.body["user"]["time_zone"], "Europe/London");
    assert_eq!(
        routine(&gateway, &rob, chef, id).await["next_run_at"],
        START + 11 * HOUR_MS
    );
    // The account and the routine were announced.
    let log = events(&gateway, 1).await;
    assert!(
        log.iter().any(
            |e| matches!(e, Event::AccountUpdated { user } if user.time_zone == "Europe/London")
        )
    );
    assert!(log.iter().any(|e| matches!(
        e,
        Event::RoutineUpdated { routine } if routine.next_run_at.map(|t| t.0) == Some(START + 11 * HOUR_MS)
    )));
}

#[tokio::test]
async fn a_due_routine_runs_in_the_bots_own_thread_behind_a_marker() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    // The Bot's thread exists already: the routine runs in it.
    let said = gateway
        .send(
            Call::post(&format!("/api/v1/bots/{chef}/messages"))
                .cookie(&rob)
                .json(&json!({"text": "hi"})),
        )
        .await;
    assert_eq!(said.status, StatusCode::OK);
    gateway
        .wait_for(&rob, &format!("/api/v1/bots/{chef}"), |b| {
            b["status"] == "idle"
        })
        .await;
    let digest = daily(&gateway, &rob, chef, "Morning digest", "19:00").await;
    let id = digest["id"].as_i64().unwrap();

    // Not yet due.
    advance(&gateway, 29 * MINUTE_MS);
    pass(&gateway).await;
    assert!(runs(&gateway, &rob, chef, id).await.is_empty());
    // Due: it runs.
    advance(&gateway, MINUTE_MS);
    pass(&gateway).await;
    let run = wait_run(&gateway, &rob, chef, id, |r| r["state"] == "succeeded").await;
    assert_eq!(run["trigger"], "schedule");
    assert_eq!(run["scheduled_for"], START + 30 * MINUTE_MS);
    assert_eq!(run["started_at"], START + 30 * MINUTE_MS);
    assert_eq!(run["reason"], Value::Null);
    assert_eq!(run["tokens"], 120);

    // One thread: the routine's turn ran on the Bot's own.
    assert_eq!(gateway.fake.received("thread/start").len(), 1);
    let starts = turn_starts(&gateway);
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[1]["threadId"], starts[0]["threadId"]);
    assert_eq!(starts[1]["input"][0]["text"], "Run Morning digest.");
    let about = starts[1]["additionalContext"]["bokkie-routine"]["value"]
        .as_str()
        .unwrap();
    assert!(about.contains("routine \"Morning digest\""), "{about}");
    assert!(
        about.contains("19:00 on Friday 15 January 2027 (Australia/Adelaide)"),
        "{about}"
    );

    // The conversation: a marker naming the routine and its time, with the
    // prompt folded into it, then the reply.
    let conversation = messages(&gateway, &rob, chef).await;
    let marker = &conversation[2];
    assert_eq!(marker["kind"], "marker");
    assert_eq!(marker["author"], "bot");
    assert_eq!(marker["text"], "Morning digest · 19:00");
    assert_eq!(marker["routine"]["routine_id"], id);
    assert_eq!(marker["routine"]["run_id"], run["id"]);
    assert_eq!(marker["routine"]["prompt"], "Run Morning digest.");
    let reply = &conversation[3];
    assert_eq!(reply["text"], "Hello there");
    assert_eq!(reply["state"], "complete");
    assert_eq!(run["messages"], json!([marker["id"], reply["id"]]));

    // It counted as a turn, and is scheduled for tomorrow.
    let usage = gateway.send(Call::get("/api/v1/usage").cookie(&rob)).await;
    assert_eq!(usage.body["allowance"]["turns_today"], 2);
    assert_eq!(
        routine(&gateway, &rob, chef, id).await["next_run_at"],
        START + 30 * MINUTE_MS + DAY_MS
    );
    // Its life went out on the event socket: queued, running, succeeded.
    let states: Vec<String> = events(&gateway, 1)
        .await
        .into_iter()
        .filter_map(|e| match e {
            Event::RoutineRunUpdated { run } => Some(format!("{:?}", run.state)),
            _ => None,
        })
        .collect();
    assert_eq!(states, ["Waiting", "Running", "Succeeded"]);
    // Nothing more runs today.
    advance(&gateway, HOUR_MS);
    pass(&gateway).await;
    assert_eq!(runs(&gateway, &rob, chef, id).await.len(), 1);
}

#[tokio::test]
async fn run_now_takes_the_scheduled_path_marked_manual() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let digest = create_routine(
        &gateway,
        &rob,
        chef,
        json!({"name": "Digest", "prompt": "Go.", "paused": true,
               "schedule": {"repeat": "weekdays", "at": "07:00"}}),
    )
    .await;
    let id = digest["id"].as_i64().unwrap();
    // Even a paused routine runs now.
    let reply = run_now(&gateway, &rob, chef, id).await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    assert_eq!(reply.body["trigger"], "manual");
    assert_eq!(reply.body["scheduled_for"], START);
    let run = wait_run(&gateway, &rob, chef, id, |r| r["state"] == "succeeded").await;
    assert_eq!(run["id"], reply.body["id"]);
    let conversation = messages(&gateway, &rob, chef).await;
    assert_eq!(conversation[0]["text"], "Digest · 18:30 · test run");
    assert_eq!(conversation[0]["routine"]["prompt"], "Go.");
    assert_eq!(conversation[1]["text"], "Hello there");
    let about = turn_starts(&gateway)[0]["additionalContext"]["bokkie-routine"]["value"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(about.contains("test run"), "{about}");
    // Still paused, and nothing about its schedule changed.
    let after = routine(&gateway, &rob, chef, id).await;
    assert_eq!(after["paused"], true);
    assert_eq!(after["next_run_at"], Value::Null);
}

#[tokio::test]
async fn misfires_within_six_hours_run_once_late_and_older_ones_are_missed() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let hourly = create_routine(
        &gateway,
        &rob,
        chef,
        json!({"name": "Hourly", "prompt": "Tick.",
               "schedule": {"repeat": "hours", "every": 1, "at": "00:00"}}),
    )
    .await;
    let hourly = hourly["id"].as_i64().unwrap();
    let digest = daily(&gateway, &rob, chef, "Digest", "19:00").await;
    let digest = digest["id"].as_i64().unwrap();

    // Down from before 19:00 until 21:40: three hourly times and one daily
    // time missed, all within six hours. Each routine runs once, for its
    // latest time.
    set_clock(&gateway, 3 * HOUR_MS + 10 * MINUTE_MS);
    let late = pass_until(&gateway, &rob, chef, hourly, |r| r["state"] == "succeeded").await;
    assert_eq!(
        late["scheduled_for"],
        START + 2 * HOUR_MS + 30 * MINUTE_MS,
        "21:00"
    );
    assert_eq!(late["started_at"], START + 3 * HOUR_MS + 10 * MINUTE_MS);
    assert_eq!(runs(&gateway, &rob, chef, hourly).await.len(), 1);
    assert_eq!(
        routine(&gateway, &rob, chef, hourly).await["next_run_at"],
        START + 3 * HOUR_MS + 30 * MINUTE_MS,
        "22:00"
    );
    // The two ran one after the other on the Bot.
    let daily_run = pass_until(&gateway, &rob, chef, digest, |r| r["state"] == "succeeded").await;
    assert_eq!(daily_run["scheduled_for"], START + 30 * MINUTE_MS);
    assert_eq!(runs(&gateway, &rob, chef, digest).await.len(), 1);

    // Down for seven hours past the next daily time: one missed run.
    set_clock(&gateway, DAY_MS + 7 * HOUR_MS + 30 * MINUTE_MS);
    let before = turn_starts(&gateway).len();
    pass(&gateway).await;
    let missed = runs(&gateway, &rob, chef, digest).await;
    assert_eq!(missed.len(), 2);
    assert_eq!(missed[0]["state"], "missed");
    assert_eq!(
        missed[0]["reason"],
        "Bokkie was not running when it was due"
    );
    assert_eq!(missed[0]["scheduled_for"], START + DAY_MS + 30 * MINUTE_MS);
    assert_eq!(missed[0]["started_at"], Value::Null);
    assert_eq!(missed[0]["messages"], json!([]));
    assert_eq!(
        routine(&gateway, &rob, chef, digest).await["next_run_at"],
        START + 2 * DAY_MS + 30 * MINUTE_MS
    );
    // The hourly one ran its latest time, 02:00, which is now.
    pass_until(&gateway, &rob, chef, hourly, |r| {
        r["state"] == "succeeded"
            && r["scheduled_for"] == START + DAY_MS + 7 * HOUR_MS + 30 * MINUTE_MS
    })
    .await;
    assert_eq!(turn_starts(&gateway).len(), before + 1);

    // Days of downtime coalesce into one late run of the latest time.
    set_clock(&gateway, 4 * DAY_MS + HOUR_MS);
    let three_days = pass_until(&gateway, &rob, chef, digest, |r| {
        r["state"] == "succeeded" && r["scheduled_for"] == START + 4 * DAY_MS + 30 * MINUTE_MS
    })
    .await;
    assert_eq!(
        three_days["scheduled_for"],
        START + 4 * DAY_MS + 30 * MINUTE_MS
    );
    assert_eq!(runs(&gateway, &rob, chef, digest).await.len(), 3);
}

#[tokio::test]
async fn a_busy_bot_is_waited_for_and_never_steered() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| b.finish_turns = false);
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let digest = daily(&gateway, &rob, chef, "Digest", "19:00").await;
    let id = digest["id"].as_i64().unwrap();
    // The user's turn is running when the routine falls due.
    let said = gateway
        .send(
            Call::post(&format!("/api/v1/bots/{chef}/messages"))
                .cookie(&rob)
                .json(&json!({"text": "a long question"})),
        )
        .await;
    assert_eq!(said.status, StatusCode::OK);
    set_clock(&gateway, 30 * MINUTE_MS);
    pass(&gateway).await;
    let waiting = &runs(&gateway, &rob, chef, id).await[0];
    assert_eq!(waiting["state"], "waiting");
    assert_eq!(waiting["reason"], "Waiting for the Bot to finish its turn");
    advance(&gateway, 29 * MINUTE_MS);
    pass(&gateway).await;
    assert_eq!(runs(&gateway, &rob, chef, id).await[0]["state"], "waiting");
    // Never steered, never a second turn.
    assert!(gateway.fake.received("turn/steer").is_empty());
    assert_eq!(turn_starts(&gateway).len(), 1);

    // The user's turn ends: the run starts.
    let stopped = gateway
        .send(Call::post(&format!("/api/v1/bots/{chef}/stop")).cookie(&rob))
        .await;
    assert_eq!(stopped.body["stopped"], true);
    gateway
        .wait_for(&rob, &format!("/api/v1/bots/{chef}"), |b| {
            b["status"] == "idle"
        })
        .await;
    pass(&gateway).await;
    let running = wait_run(&gateway, &rob, chef, id, |r| r["state"] == "running").await;
    assert_eq!(running["reason"], Value::Null);
    assert_eq!(turn_starts(&gateway).len(), 2);
    assert_eq!(turn_starts(&gateway)[1]["input"][0]["text"], "Run Digest.");

    // A message from the user now steers the routine's turn, as any
    // message to a working Bot does.
    let steer = gateway
        .send(
            Call::post(&format!("/api/v1/bots/{chef}/messages"))
                .cookie(&rob)
                .json(&json!({"text": "and also"})),
        )
        .await;
    assert_eq!(steer.body["delivery"], "steered");
}

#[tokio::test]
async fn a_bot_busy_for_thirty_minutes_skips_the_run() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| b.finish_turns = false);
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let digest = daily(&gateway, &rob, chef, "Digest", "19:00").await;
    let id = digest["id"].as_i64().unwrap();
    gateway
        .send(
            Call::post(&format!("/api/v1/bots/{chef}/messages"))
                .cookie(&rob)
                .json(&json!({"text": "a long question"})),
        )
        .await;
    set_clock(&gateway, 30 * MINUTE_MS);
    pass(&gateway).await;
    advance(&gateway, 30 * MINUTE_MS);
    pass(&gateway).await;
    let skipped = &runs(&gateway, &rob, chef, id).await[0];
    assert_eq!(skipped["state"], "skipped");
    assert_eq!(skipped["reason"], "The Bot was busy for 30 minutes");
    assert_eq!(skipped["finished_at"], START + HOUR_MS);
    assert!(gateway.fake.received("turn/steer").is_empty());
    assert_eq!(turn_starts(&gateway).len(), 1);
    // A skip is not a failure.
    assert_eq!(routine(&gateway, &rob, chef, id).await["failures"], 0);
}

#[tokio::test]
async fn one_run_per_routine_at_a_time() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| b.finish_turns = false);
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let hourly = create_routine(
        &gateway,
        &rob,
        chef,
        json!({"name": "Hourly", "prompt": "Tick.",
               "schedule": {"repeat": "hours", "every": 1, "at": "00:00"}}),
    )
    .await;
    let id = hourly["id"].as_i64().unwrap();
    // Run it ten minutes before it falls due, so that the pass at the due
    // time is within the run's 15-minute deadline and does not race its
    // interrupt.
    set_clock(&gateway, 20 * MINUTE_MS);
    let first = run_now(&gateway, &rob, chef, id).await;
    assert_eq!(first.body["state"], "running", "{:?}", first.body);
    // Asked for again while it runs: skipped.
    let again = run_now(&gateway, &rob, chef, id).await;
    assert_eq!(again.body["state"], "skipped");
    assert_eq!(
        again.body["reason"],
        "The routine's previous run was still going"
    );
    // Falling due while it runs: skipped too.
    set_clock(&gateway, 30 * MINUTE_MS);
    pass(&gateway).await;
    let all = runs(&gateway, &rob, chef, id).await;
    assert_eq!(all.len(), 3);
    assert_eq!(all[0]["state"], "skipped");
    assert_eq!(all[0]["trigger"], "schedule");
    assert_eq!(all[2]["state"], "running");
    assert_eq!(turn_starts(&gateway).len(), 1);
}

#[tokio::test]
async fn a_run_is_interrupted_after_fifteen_minutes() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| b.finish_turns = false);
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let digest = daily(&gateway, &rob, chef, "Digest", "07:00").await;
    let id = digest["id"].as_i64().unwrap();
    let run = run_now(&gateway, &rob, chef, id).await;
    assert_eq!(run.body["state"], "running");
    advance(&gateway, 15 * MINUTE_MS - 1);
    pass(&gateway).await;
    assert!(gateway.fake.received("turn/interrupt").is_empty());
    advance(&gateway, 1);
    pass(&gateway).await;
    let interrupts = gateway.fake.received("turn/interrupt");
    assert_eq!(interrupts.len(), 1);
    let ended = wait_run(&gateway, &rob, chef, id, |r| r["state"] == "interrupted").await;
    assert_eq!(
        ended["reason"],
        "The run took longer than 15 minutes, so it was stopped"
    );
    assert_eq!(routine(&gateway, &rob, chef, id).await["failures"], 1);
    let conversation = messages(&gateway, &rob, chef).await;
    assert_eq!(conversation.last().unwrap()["state"], "interrupted");
}

#[tokio::test]
async fn a_run_whose_turn_does_not_confirm_its_interrupt_ends_anyway() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| b.finish_turns = false);
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let digest = daily(&gateway, &rob, chef, "Digest", "07:00").await;
    let id = digest["id"].as_i64().unwrap();
    let run = run_now(&gateway, &rob, chef, id).await;
    assert_eq!(run.body["state"], "running");
    // The app server refuses the interrupt, as if the turn had ended
    // unseen.
    let turn = gateway.fake.received("turn/start").len();
    assert_eq!(turn, 1);
    gateway.fake.with(|b| {
        let ids: Vec<String> = (0..100).map(|i| format!("turn-{i}")).collect();
        b.finished.extend(ids);
    });
    advance(&gateway, 15 * MINUTE_MS);
    pass(&gateway).await;
    assert_eq!(runs(&gateway, &rob, chef, id).await[0]["state"], "running");
    advance(&gateway, MINUTE_MS);
    pass(&gateway).await;
    let ended = &runs(&gateway, &rob, chef, id).await[0];
    assert_eq!(ended["state"], "interrupted");
    assert_eq!(
        ended["reason"],
        "The run took longer than 15 minutes, so it was stopped"
    );
}

#[tokio::test]
async fn three_failed_runs_in_a_row_pause_the_routine() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let digest = daily(&gateway, &rob, chef, "Digest", "07:00").await;
    let id = digest["id"].as_i64().unwrap();
    // A success between failures clears them.
    gateway.fake.with(|b| b.reject_turns = true);
    let failed = run_now(&gateway, &rob, chef, id).await;
    assert_eq!(failed.body["state"], "failed");
    assert_eq!(failed.body["reason"], "The Bots' runtime is unavailable");
    assert_eq!(routine(&gateway, &rob, chef, id).await["failures"], 1);
    gateway.fake.with(|b| b.reject_turns = false);
    run_now(&gateway, &rob, chef, id).await;
    wait_run(&gateway, &rob, chef, id, |r| r["state"] == "succeeded").await;
    assert_eq!(routine(&gateway, &rob, chef, id).await["failures"], 0);

    gateway.fake.with(|b| b.reject_turns = true);
    for failures in 1..=3 {
        let failed = run_now(&gateway, &rob, chef, id).await;
        assert_eq!(failed.body["state"], "failed");
        assert_eq!(
            routine(&gateway, &rob, chef, id).await["failures"],
            failures
        );
    }
    let paused = routine(&gateway, &rob, chef, id).await;
    assert_eq!(paused["paused"], true);
    assert_eq!(paused["next_run_at"], Value::Null);
    assert!(events(&gateway, 1).await.iter().any(|e| matches!(
        e,
        Event::RoutineUpdated { routine } if routine.paused && routine.failures == 3
    )));
    // Paused, it does not run on its schedule.
    advance(&gateway, 2 * DAY_MS);
    pass(&gateway).await;
    assert_eq!(runs(&gateway, &rob, chef, id).await.len(), 5);
    // Resuming clears the failures.
    let resumed = patch_routine(&gateway, &rob, chef, id, json!({"paused": false})).await;
    assert_eq!(resumed.body["failures"], 0);
    assert_eq!(resumed.body["paused"], false);
}

#[tokio::test]
async fn a_refused_allowance_fails_the_run() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let reply = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/admin/users/{amy_id}"))
                .cookie(&rob)
                .json(&json!({"limits": {"daily_turns": 1, "weekly_tokens": null}})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    let chef = create_bot(&gateway, &amy, "Chef").await;
    let digest = daily(&gateway, &amy, chef, "Digest", "07:00").await;
    let id = digest["id"].as_i64().unwrap();
    // The run uses the day's one turn.
    run_now(&gateway, &amy, chef, id).await;
    wait_run(&gateway, &amy, chef, id, |r| r["state"] == "succeeded").await;
    let usage = gateway.send(Call::get("/api/v1/usage").cookie(&amy)).await;
    assert_eq!(usage.body["allowance"]["turns_today"], 1);
    // The next has none left.
    let refused = run_now(&gateway, &amy, chef, id).await;
    assert_eq!(refused.body["state"], "failed");
    assert!(
        refused.body["reason"]
            .as_str()
            .unwrap()
            .contains("used your allowance"),
        "{:?}",
        refused.body
    );
    assert_eq!(turn_starts(&gateway).len(), 1);
    assert_eq!(messages(&gateway, &amy, chef).await.len(), 2, "no marker");
}

#[tokio::test]
async fn a_signed_out_household_holds_runs_for_six_hours() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let digest = daily(&gateway, &rob, chef, "Digest", "19:00").await;
    let id = digest["id"].as_i64().unwrap();
    let auth = gateway.dir.path().join("household/codex/auth.json");
    std::fs::remove_file(&auth).unwrap();

    // Due while signed out: it waits.
    set_clock(&gateway, 30 * MINUTE_MS);
    pass(&gateway).await;
    let waiting = &runs(&gateway, &rob, chef, id).await[0];
    assert_eq!(waiting["state"], "waiting");
    assert_eq!(
        waiting["reason"],
        "Waiting for the household's ChatGPT sign-in"
    );
    // Signed in again within six hours, it runs late, once.
    advance(&gateway, 5 * HOUR_MS);
    pass(&gateway).await;
    assert_eq!(runs(&gateway, &rob, chef, id).await[0]["state"], "waiting");
    common::fake_codex::sign_in_household(gateway.dir.path(), 86_400);
    pass(&gateway).await;
    let late = wait_run(&gateway, &rob, chef, id, |r| r["state"] == "succeeded").await;
    assert_eq!(late["scheduled_for"], START + 30 * MINUTE_MS);
    assert_eq!(late["started_at"], START + 5 * HOUR_MS + 30 * MINUTE_MS);
    assert_eq!(runs(&gateway, &rob, chef, id).await.len(), 1);

    // Signed out past six hours: missed.
    std::fs::remove_file(&auth).unwrap();
    set_clock(&gateway, DAY_MS + 30 * MINUTE_MS);
    pass(&gateway).await;
    advance(&gateway, 6 * HOUR_MS);
    pass(&gateway).await;
    assert_eq!(runs(&gateway, &rob, chef, id).await[0]["state"], "waiting");
    advance(&gateway, 1);
    pass(&gateway).await;
    let missed = &runs(&gateway, &rob, chef, id).await[0];
    assert_eq!(missed["state"], "missed");
    assert_eq!(missed["reason"], "ChatGPT was signed out for the household");
    assert_eq!(turn_starts(&gateway).len(), 1);
}

#[tokio::test]
async fn a_disabled_users_routines_do_not_run() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let chef = create_bot(&gateway, &amy, "Chef").await;
    let digest = daily(&gateway, &amy, chef, "Digest", "19:00").await;
    let id = digest["id"].as_i64().unwrap();
    let disabled = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/admin/users/{amy_id}"))
                .cookie(&rob)
                .json(&json!({"disabled": true})),
        )
        .await;
    assert_eq!(disabled.status, StatusCode::OK);
    set_clock(&gateway, 30 * MINUTE_MS);
    pass(&gateway).await;
    assert!(turn_starts(&gateway).is_empty());
    let run = gateway
        .state
        .routines
        .to_api(
            &bokkie_gateway::routines::store::active_runs(
                &gateway.state.db,
                bokkie_protocol::api::RunState::Missed,
            )
            .await
            .unwrap()[0],
        )
        .await
        .unwrap();
    assert_eq!(run.routine_id.0, id);
    assert_eq!(run.reason.as_deref(), Some("Your account is disabled"));
    assert_eq!(gateway.fake.starts(UserId(amy_id)), 0);
}

#[tokio::test]
async fn a_restart_ends_running_runs_and_keeps_the_schedule() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| b.finish_turns = false);
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let digest = daily(&gateway, &rob, chef, "Digest", "19:00").await;
    let id = digest["id"].as_i64().unwrap();
    let run = run_now(&gateway, &rob, chef, id).await;
    assert_eq!(run.body["state"], "running");

    // A new gateway on the same database: the turn went with the old one.
    let restarted = Routines::new(
        gateway.state.db.clone(),
        gateway.state.clock.clone(),
        gateway.state.events.clone(),
        gateway.state.conversations.clone(),
    );
    restarted.recover().await.unwrap();
    let ended = &runs(&gateway, &rob, chef, id).await[0];
    assert_eq!(ended["state"], "failed");
    assert_eq!(ended["reason"], "Bokkie restarted during the run");
    // Its schedule survived: down past 19:00, it runs late when the new
    // gateway's scheduler starts.
    gateway.fake.with(|b| b.finish_turns = true);
    gateway
        .state
        .conversations
        .stop(UserId(1), bokkie_protocol::BotId(chef))
        .await
        .unwrap();
    gateway
        .wait_for(&rob, &format!("/api/v1/bots/{chef}"), |b| {
            b["status"] == "idle"
        })
        .await;
    set_clock(&gateway, 2 * HOUR_MS);
    restarted.pass(gateway.state.clock.now()).await.unwrap();
    restarted.settle().await;
    let late = wait_run(&gateway, &rob, chef, id, |r| r["state"] == "succeeded").await;
    assert_eq!(late["scheduled_for"], START + 30 * MINUTE_MS);
    assert_eq!(late["trigger"], "schedule");
}

#[tokio::test]
async fn runs_are_kept_ninety_days() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let digest = daily(&gateway, &rob, chef, "Digest", "07:00").await;
    let id = digest["id"].as_i64().unwrap();
    run_now(&gateway, &rob, chef, id).await;
    wait_run(&gateway, &rob, chef, id, |r| r["state"] == "succeeded").await;
    let conversations = &gateway.state.conversations;
    let clock = &gateway.state.clock;
    let before = clock.now();
    clock.advance(90 * DAY_MS - 1);
    conversations.roll_over(before, clock.now()).await.unwrap();
    assert_eq!(runs(&gateway, &rob, chef, id).await.len(), 1);
    let before = clock.now();
    clock.advance(1);
    conversations.roll_over(before, clock.now()).await.unwrap();
    assert!(runs(&gateway, &rob, chef, id).await.is_empty());
    // The conversation keeps its marker and reply.
    assert_eq!(messages(&gateway, &rob, chef).await.len(), 2);
}

#[tokio::test]
async fn one_user_cannot_reach_another_users_routines() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    let amy_bot = create_bot(&gateway, &amy, "Chef").await;
    let amy_routine = daily(&gateway, &amy, amy_bot, "Digest", "19:00").await;
    let amy_routine = amy_routine["id"].as_i64().unwrap();
    run_now(&gateway, &amy, amy_bot, amy_routine).await;
    wait_run(&gateway, &amy, amy_bot, amy_routine, |r| {
        r["state"] == "succeeded"
    })
    .await;
    // Oversight on: the admin may read Amy's conversations, but not
    // through these routes, which are the signed-in user's own.
    gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/admin/users/{amy_id}"))
                .cookie(&rob)
                .json(&json!({"oversight": true})),
        )
        .await;
    let rob_bot = create_bot(&gateway, &rob, "Rob's").await;
    let body = json!({"name": "Mine", "prompt": "x",
                      "schedule": {"repeat": "daily", "at": "07:00"}});
    for (bot, routine) in [(amy_bot, amy_routine), (rob_bot, amy_routine)] {
        let path = routine_path(bot, routine);
        let calls = [
            Call::get(&path),
            Call::new(Method::PATCH, &path).json(&json!({"paused": true})),
            Call::new(Method::DELETE, &path),
            Call::post(&format!("{path}/run")),
            Call::get(&format!("{path}/runs")),
        ];
        for call in calls {
            let reply = gateway.send(call.cookie(&rob)).await;
            assert_eq!(
                reply.status,
                StatusCode::NOT_FOUND,
                "{path}: {:?}",
                reply.body
            );
        }
    }
    for call in [
        Call::get(&routines_path(amy_bot)),
        Call::post(&routines_path(amy_bot)).json(&body),
    ] {
        let reply = gateway.send(call.cookie(&rob)).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
    }
    // Rob's own list holds only his.
    let mine = daily(&gateway, &rob, rob_bot, "Mine", "07:00").await;
    let all = gateway
        .send(Call::get("/api/v1/routines").cookie(&rob))
        .await;
    assert_eq!(all.body["routines"], json!([mine]));
    // Amy's routine and history are untouched, and she cannot reach Rob's.
    let still = routine(&gateway, &amy, amy_bot, amy_routine).await;
    assert_eq!(still["paused"], false);
    assert_eq!(runs(&gateway, &amy, amy_bot, amy_routine).await.len(), 1);
    let mine_id = mine["id"].as_i64().unwrap();
    for (bot, routine) in [(rob_bot, mine_id), (amy_bot, mine_id)] {
        let reply = gateway
            .send(Call::post(&format!("{}/run", routine_path(bot, routine))).cookie(&amy))
            .await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
    }
    let amy_all = gateway
        .send(Call::get("/api/v1/routines").cookie(&amy))
        .await;
    assert_eq!(amy_all.body["routines"].as_array().unwrap().len(), 1);
    assert_eq!(amy_all.body["routines"][0]["id"], amy_routine);
    // Each user's events stay in their own log.
    let rob_log = events(&gateway, 1).await;
    assert!(!rob_log.iter().any(|e| match e {
        Event::RoutineCreated { routine } | Event::RoutineUpdated { routine } =>
            routine.id.0 == amy_routine,
        Event::RoutineRunUpdated { run } => run.routine_id.0 == amy_routine,
        _ => false,
    }));
    let amy_log = events(&gateway, amy_id).await;
    assert!(!amy_log.iter().any(|e| match e {
        Event::RoutineCreated { routine } => routine.id.0 == mine_id,
        _ => false,
    }));
    // A run of Amy's ran on Amy's app server, in Amy's Bot's thread.
    let starts = gateway.fake.received("turn/start");
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].user(), UserId(amy_id));
}

#[tokio::test]
async fn a_turn_that_ends_before_its_start_is_answered_still_ends_its_run() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| b.finish_before_answer = true);
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let digest = daily(&gateway, &rob, chef, "Digest", "07:00").await;
    let id = digest["id"].as_i64().unwrap();
    run_now(&gateway, &rob, chef, id).await;
    let run = wait_run(&gateway, &rob, chef, id, |r| r["state"] == "succeeded").await;
    assert_eq!(run["messages"].as_array().unwrap().len(), 2);
    gateway
        .wait_for(&rob, &format!("/api/v1/bots/{chef}"), |b| {
            b["status"] == "idle"
        })
        .await;
}

#[tokio::test]
async fn an_app_server_that_stops_fails_its_running_runs() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| b.finish_turns = false);
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let digest = daily(&gateway, &rob, chef, "Digest", "07:00").await;
    let id = digest["id"].as_i64().unwrap();
    let run = run_now(&gateway, &rob, chef, id).await;
    assert_eq!(run.body["state"], "running");
    gateway.fake.crash(UserId(1));
    // Wake the watcher so it sees the closed stream.
    let runtime = gateway.state.supervisor.running(UserId(1)).await.unwrap();
    let _ = runtime.rate_limits().await;
    let failed = wait_run(&gateway, &rob, chef, id, |r| r["state"] == "failed").await;
    assert_eq!(failed["reason"], "The Bots' runtime stopped during the run");
    assert_eq!(routine(&gateway, &rob, chef, id).await["failures"], 1);
}

/// Starts a turn of the user's own that runs until it is stopped.
async fn busy(gateway: &Gateway, cookie: &str, bot: i64) {
    let said = gateway
        .send(
            Call::post(&format!("/api/v1/bots/{bot}/messages"))
                .cookie(cookie)
                .json(&json!({"text": "a long question"})),
        )
        .await;
    assert_eq!(said.status, StatusCode::OK, "{:?}", said.body);
}

/// Asks the Bot's turn to stop. The turn ends a moment later.
async fn interrupt(gateway: &Gateway, cookie: &str, bot: i64) {
    let stopped = gateway
        .send(Call::post(&format!("/api/v1/bots/{bot}/stop")).cookie(cookie))
        .await;
    assert_eq!(stopped.body["stopped"], true);
}

/// Stops the Bot's turn and waits until it is idle. With the scheduler's
/// loop running, a waiting run can start as soon as the Bot is idle, too
/// soon for a poll to see it: use [`interrupt`] there.
async fn stop(gateway: &Gateway, cookie: &str, bot: i64) {
    interrupt(gateway, cookie, bot).await;
    gateway
        .wait_for(cookie, &format!("/api/v1/bots/{bot}"), |b| {
            b["status"] == "idle"
        })
        .await;
}

/// Waits, a few seconds at most, until `condition` holds.
async fn eventually(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the condition holds in time");
}

#[tokio::test]
async fn pausing_a_routine_skips_its_waiting_run() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| b.finish_turns = false);
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let digest = daily(&gateway, &rob, chef, "Digest", "19:00").await;
    let id = digest["id"].as_i64().unwrap();
    // Due while the user's turn runs: it waits.
    busy(&gateway, &rob, chef).await;
    set_clock(&gateway, 30 * MINUTE_MS);
    pass(&gateway).await;
    assert_eq!(runs(&gateway, &rob, chef, id).await[0]["state"], "waiting");

    // Paused: the waiting run is skipped, and says why.
    let paused = patch_routine(&gateway, &rob, chef, id, json!({"paused": true})).await;
    assert_eq!(paused.status, StatusCode::OK, "{:?}", paused.body);
    let skipped = &runs(&gateway, &rob, chef, id).await[0];
    assert_eq!(skipped["state"], "skipped");
    assert_eq!(
        skipped["reason"],
        "The routine was paused before the run started"
    );
    assert_eq!(skipped["finished_at"], START + 30 * MINUTE_MS);
    // A skip is not a failure.
    assert_eq!(routine(&gateway, &rob, chef, id).await["failures"], 0);
    let ended = events(&gateway, 1).await.into_iter().any(|e| {
        matches!(e, Event::RoutineRunUpdated { run }
            if run.routine_id.0 == id && format!("{:?}", run.state) == "Skipped")
    });
    assert!(ended, "the skip goes out on the event socket");

    // When the Bot is idle, nothing starts.
    stop(&gateway, &rob, chef).await;
    pass(&gateway).await;
    assert_eq!(turn_starts(&gateway).len(), 1);
    assert_eq!(runs(&gateway, &rob, chef, id).await.len(), 1);
    // Paused, it can still be run by hand.
    let manual = run_now(&gateway, &rob, chef, id).await;
    assert_eq!(manual.body["state"], "running", "{:?}", manual.body);
    assert_eq!(manual.body["trigger"], "manual");
}

#[tokio::test]
async fn a_paused_routines_scheduled_run_never_starts() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| b.finish_turns = false);
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let digest = daily(&gateway, &rob, chef, "Digest", "19:00").await;
    let id = digest["id"].as_i64().unwrap();
    busy(&gateway, &rob, chef).await;
    set_clock(&gateway, 30 * MINUTE_MS);
    pass(&gateway).await;
    assert_eq!(runs(&gateway, &rob, chef, id).await[0]["state"], "waiting");
    // Paused by a path that leaves its waiting run waiting, as a pause
    // that raced the run's start could.
    sqlx::query("UPDATE routines SET paused = 1, next_run_at = NULL WHERE id = ?")
        .bind(id)
        .execute(&gateway.state.db)
        .await
        .unwrap();
    stop(&gateway, &rob, chef).await;
    pass(&gateway).await;
    // The start refuses it.
    assert_eq!(turn_starts(&gateway).len(), 1);
    let skipped = &runs(&gateway, &rob, chef, id).await[0];
    assert_eq!(skipped["state"], "skipped");
    assert_eq!(
        skipped["reason"],
        "The routine was paused before the run started"
    );
    assert!(
        messages(&gateway, &rob, chef)
            .await
            .iter()
            .all(|m| m["kind"] != "marker" || m["routine"].is_null()),
        "no marker for a run that never started"
    );
}

#[tokio::test]
async fn a_pause_waits_for_a_start_under_way() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| {
        b.finish_turns = false;
        b.hold_thread_starts = true;
    });
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let digest = daily(&gateway, &rob, chef, "Digest", "19:00").await;
    let id = digest["id"].as_i64().unwrap();
    set_clock(&gateway, 30 * MINUTE_MS);
    // The run's start waits on its Bot's new thread.
    let routines = &gateway.state.routines;
    routines.pass(gateway.state.clock.now()).await.unwrap();
    eventually(|| gateway.fake.received("thread/start").len() == 1).await;
    let pause = patch_routine(&gateway, &rob, chef, id, json!({"paused": true}));
    tokio::pin!(pause);
    let early = tokio::time::timeout(Duration::from_millis(100), &mut pause).await;
    assert!(early.is_err(), "the pause waits for the start");
    assert_eq!(runs(&gateway, &rob, chef, id).await[0]["state"], "waiting");
    // The start finishes first: the run is running, and the pause leaves it
    // to finish.
    gateway.fake.thread_starts.add_permits(1);
    let paused = pause.await;
    routines.settle().await;
    assert_eq!(paused.status, StatusCode::OK, "{:?}", paused.body);
    assert_eq!(paused.body["paused"], true);
    let run = &runs(&gateway, &rob, chef, id).await[0];
    assert_eq!(run["state"], "running");
    assert_eq!(turn_starts(&gateway).len(), 1);
}

#[tokio::test]
async fn a_tool_call_made_before_its_run_is_running_is_the_runs() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| {
        b.finish_turns = false;
        b.hold_turn_answers = true;
    });
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let granted = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/bots/{chef}"))
                .cookie(&rob)
                .json(&json!({"grants": ["notes"]})),
        )
        .await;
    assert_eq!(granted.status, StatusCode::OK, "{:?}", granted.body);
    let digest = daily(&gateway, &rob, chef, "Digest", "19:00").await;
    let id = digest["id"].as_i64().unwrap();
    set_clock(&gateway, 30 * MINUTE_MS);
    let routines = &gateway.state.routines;
    routines.pass(gateway.state.clock.now()).await.unwrap();
    // Codex has started the turn but not yet answered `turn/start`, so the
    // run still waits; the model calls a tool.
    eventually(|| gateway.fake.with(|b| b.turns.len()) == 1).await;
    let (thread, turn) = gateway.fake.with(|b| b.turns[0].clone());
    gateway.fake.inject(
        UserId(1),
        json!({"id": 901, "method": "item/tool/call", "params": {
            "threadId": thread, "turnId": turn, "callId": "call-901",
            "tool": "notes_append", "namespace": null, "arguments": {"text": "early"}}}),
    );
    eventually(|| {
        gateway
            .fake
            .received("<response>")
            .iter()
            .any(|r| r.params["id"] == 901)
    })
    .await;
    let calls_path = format!("/api/v1/bots/{chef}/tool-calls");
    let early = gateway.send(Call::get(&calls_path).cookie(&rob)).await;
    assert_eq!(early.body["calls"][0]["outcome"], "ok", "{:?}", early.body);
    assert_eq!(runs(&gateway, &rob, chef, id).await[0]["state"], "waiting");

    // `turn/start` answers: the run is running, and the call is its.
    gateway.fake.turn_answers.add_permits(1);
    routines.settle().await;
    let run = &runs(&gateway, &rob, chef, id).await[0];
    assert_eq!(run["state"], "running");
    let calls = gateway.send(Call::get(&calls_path).cookie(&rob)).await;
    assert_eq!(calls.body["calls"][0]["routine_run_id"], run["id"]);
}

#[tokio::test]
async fn one_users_slow_app_server_holds_up_no_one_elses_runs() {
    let (gateway, rob) = setup().await;
    let (amy, amy_id) = gateway.member(&rob, "amy").await;
    gateway.fake.with(|b| {
        b.finish_turns = false;
        b.hold_thread_starts_of = vec![Owner::User(UserId(amy_id))];
    });
    let amy_bot = create_bot(&gateway, &amy, "Chef").await;
    let amy_routine = daily(&gateway, &amy, amy_bot, "Digest", "19:00").await;
    let amy_routine = amy_routine["id"].as_i64().unwrap();
    let rob_bot = create_bot(&gateway, &rob, "Chef").await;
    let rob_routine = daily(&gateway, &rob, rob_bot, "Digest", "19:00").await;
    let rob_routine = rob_routine["id"].as_i64().unwrap();
    let routines = &gateway.state.routines;
    let pass_now = || async {
        tokio::time::timeout(
            Duration::from_secs(5),
            routines.pass(gateway.state.clock.now()),
        )
        .await
        .expect("the pass waits on no app server")
        .unwrap();
    };

    // Both fall due; Amy's app server does not answer her Bot's new thread.
    set_clock(&gateway, 30 * MINUTE_MS);
    pass_now().await;
    let running = wait_run(&gateway, &rob, rob_bot, rob_routine, |r| {
        r["state"] == "running"
    })
    .await;
    assert_eq!(running["started_at"], START + 30 * MINUTE_MS);
    eventually(|| gateway.fake.received("thread/start").len() == 2).await;
    let amy_waiting = &runs(&gateway, &amy, amy_bot, amy_routine).await[0];
    assert_eq!(amy_waiting["state"], "waiting");

    // Rob's run passes its deadline and is stopped on time, while Amy's
    // start still waits; nor is her run started twice.
    advance(&gateway, 15 * MINUTE_MS);
    pass_now().await;
    let interrupted = wait_run(&gateway, &rob, rob_bot, rob_routine, |r| {
        r["state"] == "interrupted"
    })
    .await;
    assert_eq!(
        interrupted["reason"],
        "The run took longer than 15 minutes, so it was stopped"
    );
    let amy_threads = gateway
        .fake
        .received("thread/start")
        .iter()
        .filter(|r| r.owner == Owner::User(UserId(amy_id)))
        .count();
    assert_eq!(amy_threads, 1);
    assert_eq!(
        runs(&gateway, &amy, amy_bot, amy_routine).await[0]["state"],
        "waiting"
    );

    // Her app server answers: her run starts.
    gateway.fake.thread_starts.add_permits(1);
    let amy_running = wait_run(&gateway, &amy, amy_bot, amy_routine, |r| {
        r["state"] == "running"
    })
    .await;
    assert_eq!(amy_running["scheduled_for"], START + 30 * MINUTE_MS);
    routines.settle().await;
}

#[tokio::test]
async fn an_attempt_that_does_not_start_a_run_never_ends_a_running_one() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| b.finish_turns = false);
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let digest = daily(&gateway, &rob, chef, "Digest", "07:00").await;
    let id = digest["id"].as_i64().unwrap();
    // A snapshot of the run, taken as a pass takes one while it waits.
    let queued = run_now(&gateway, &rob, chef, id).await;
    assert_eq!(queued.body["state"], "running", "{:?}", queued.body);
    let run_id = bokkie_protocol::RoutineRunId(queued.body["id"].as_i64().unwrap());
    let snapshot = store::run(&gateway.state.db, UserId(1), run_id)
        .await
        .unwrap()
        .unwrap();
    // A late attempt from that snapshot finds the Bot busy for 30 minutes
    // since the run was queued, with a message to it being sent.
    advance(&gateway, 30 * MINUTE_MS);
    let sending = gateway
        .state
        .conversations
        .lock_bot(UserId(1), BotId(chef))
        .await;
    gateway
        .state
        .routines
        .try_start(&snapshot, gateway.state.clock.now())
        .await
        .unwrap();
    drop(sending);
    // The run that started is left running, and counted neither way.
    let run = &runs(&gateway, &rob, chef, id).await[0];
    assert_eq!(run["state"], "running");
    assert_eq!(run["reason"], Value::Null);
    assert_eq!(routine(&gateway, &rob, chef, id).await["failures"], 0);
}

#[tokio::test]
async fn the_scheduler_loop_passes_on_its_tick() {
    let (gateway, rob) = setup().await;
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let digest = daily(&gateway, &rob, chef, "Digest", "19:00").await;
    let id = digest["id"].as_i64().unwrap();
    let scheduler = tokio::spawn(
        gateway
            .state
            .routines
            .clone()
            .run(Duration::from_millis(20)),
    );
    // Nothing is due; then its time comes, and the loop runs it.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(runs(&gateway, &rob, chef, id).await.is_empty());
    set_clock(&gateway, 30 * MINUTE_MS);
    let run = wait_run(&gateway, &rob, chef, id, |r| r["state"] == "succeeded").await;
    assert_eq!(run["trigger"], "schedule");
    assert_eq!(run["scheduled_for"], START + 30 * MINUTE_MS);
    scheduler.abort();
}

#[tokio::test]
async fn the_scheduler_loop_passes_when_a_bot_turns_idle() {
    let (gateway, rob) = setup().await;
    gateway.fake.with(|b| b.finish_turns = false);
    let chef = create_bot(&gateway, &rob, "Chef").await;
    let digest = daily(&gateway, &rob, chef, "Digest", "19:00").await;
    let id = digest["id"].as_i64().unwrap();
    busy(&gateway, &rob, chef).await;
    set_clock(&gateway, 30 * MINUTE_MS);
    // A tick an hour apart: the loop's first pass, at once, queues the run,
    // which waits for the Bot.
    let scheduler = tokio::spawn(
        gateway
            .state
            .routines
            .clone()
            .run(Duration::from_secs(3600)),
    );
    let waiting = wait_run(&gateway, &rob, chef, id, |r| {
        r["reason"] == "Waiting for the Bot to finish its turn"
    })
    .await;
    assert_eq!(waiting["state"], "waiting");
    // The user's turn ends: the loop passes at once, long before its tick.
    // The run starts as soon as the Bot is idle, so the idle status may be
    // gone before a poll could see it; the event log keeps it.
    interrupt(&gateway, &rob, chef).await;
    let running = wait_run(&gateway, &rob, chef, id, |r| r["state"] == "running").await;
    assert_eq!(running["trigger"], "schedule");
    assert_eq!(turn_starts(&gateway).len(), 2);
    scheduler.abort();
    // The run started only after the user's turn had ended.
    let log = events(&gateway, 1).await;
    let position = |wanted: &dyn Fn(&Event) -> bool| log.iter().position(wanted);
    let working = position(&|e| {
        matches!(e, Event::BotStatusChanged { bot_id, status: BotStatus::Working } if bot_id.0 == chef)
    })
    .expect("the user's turn runs");
    let idle = log[working..]
        .iter()
        .position(|e| {
            matches!(e, Event::BotStatusChanged { bot_id, status: BotStatus::Idle } if bot_id.0 == chef)
        })
        .map(|at| working + at)
        .expect("the user's turn ends");
    let started = position(&|e| {
        matches!(e, Event::RoutineRunUpdated { run }
            if run.routine_id.0 == id && run.state == RunState::Running)
    })
    .expect("the run starts");
    assert!(idle < started, "the run started while the Bot was working");
}
