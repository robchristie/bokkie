//! Bot definitions: `bokkie-gateway bot apply` and `bot run`, through the
//! gateway's control socket and directly, against a temporary data
//! directory and a fake app server.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;

use axum::http::{Method, StatusCode};
use bokkie_gateway::control::{self, Request};
use bokkie_gateway::definitions::{self, APPLIED, DRY_RUN, Definition};
use bokkie_protocol::UserId;
use bokkie_protocol::api::{Permissions, Role};
use bokkie_protocol::events::Seq;
use common::{Call, Gateway};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A neutral example: a Bot with two routines.
const GARDENER: &str = r#"
name = "Gardener"
label = "Garden notes"
description = "Keep track of the garden. Be brief."
avatar = "🌱"
grants = ["notes", "notify"]

[[routines]]
name = "Watering"
prompt = "Remind me which beds need water today."
schedule = { repeat = "daily", at = "07:00" }

[[routines]]
name = "Weekly review"
prompt = "Review the week's notes."
schedule = { repeat = "days", days = ["sun"], at = "17:00" }
push = "notify_only"
"#;

async fn setup() -> (Gateway, String) {
    let gateway = Gateway::new().await;
    gateway.route().await;
    let rob = gateway.admin("rob").await;
    (gateway, rob)
}

async fn apply(gateway: &Gateway, account: &str, text: &str, dry_run: bool) -> Vec<String> {
    let definition = Definition::parse(text).unwrap();
    definitions::apply(&gateway.state, account, &definition, dry_run)
        .await
        .unwrap_or_else(|f| panic!("{}", f.message))
        .lines
}

async fn refused(gateway: &Gateway, account: &str, text: &str) -> String {
    let definition = Definition::parse(text).unwrap();
    match definitions::apply(&gateway.state, account, &definition, false).await {
        Ok(report) => panic!("applied: {:?}", report.lines),
        Err(failure) => failure.message,
    }
}

async fn latest(gateway: &Gateway, user: i64) -> Seq {
    gateway.state.events.latest(UserId(user)).await.unwrap()
}

async fn bots(gateway: &Gateway, cookie: &str) -> Vec<Value> {
    let reply = gateway.send(Call::get("/api/v1/bots").cookie(cookie)).await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    reply.body["bots"].as_array().unwrap().clone()
}

async fn routines(gateway: &Gateway, cookie: &str, bot: i64) -> Vec<Value> {
    let reply = gateway
        .send(Call::get(&format!("/api/v1/bots/{bot}/routines")).cookie(cookie))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    reply.body["routines"].as_array().unwrap().clone()
}

fn names(routines: &[Value]) -> Vec<&str> {
    let mut names: Vec<&str> = routines
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    names
}

async fn say(gateway: &Gateway, cookie: &str, bot: i64, text: &str) {
    let reply = gateway
        .send(
            Call::post(&format!("/api/v1/bots/{bot}/messages"))
                .cookie(cookie)
                .json(&json!({"text": text})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    gateway
        .wait_for(cookie, &format!("/api/v1/bots/{bot}"), |b| {
            b["status"] == "idle"
        })
        .await;
}

#[tokio::test]
async fn a_definition_creates_its_bot_and_applying_it_again_changes_nothing() {
    let (gateway, rob) = setup().await;
    let lines = apply(&gateway, "rob", GARDENER, false).await;
    assert_eq!(
        lines,
        [
            APPLIED,
            "create the Bot \"Gardener\" in rob's account",
            "grant it notes, notify",
            "create the routine \"Watering\": daily at 07:00, next at 07:00 on Saturday 16 \
             January 2027 (Australia/Adelaide)",
            "create the routine \"Weekly review\": on Sunday at 17:00, next at 17:00 on \
             Sunday 17 January 2027 (Australia/Adelaide)",
        ]
    );
    let listed = bots(&gateway, &rob).await;
    assert_eq!(listed.len(), 1);
    let bot = &listed[0];
    assert_eq!(bot["name"], "Gardener");
    assert_eq!(bot["label"], "Garden notes");
    assert_eq!(bot["description"], "Keep track of the garden. Be brief.");
    assert_eq!(bot["avatar"], "🌱");
    assert_eq!(bot["grants"], json!(["notes", "notify"]));
    let id = bot["id"].as_i64().unwrap();
    let made = routines(&gateway, &rob, id).await;
    assert_eq!(names(&made), ["Watering", "Weekly review"]);
    let weekly = made.iter().find(|r| r["name"] == "Weekly review").unwrap();
    assert_eq!(weekly["push"], "notify_only");
    assert_eq!(weekly["paused"], false);

    // Applied again, it changes nothing: not even an event.
    let before = latest(&gateway, 1).await;
    let lines = apply(&gateway, " ROB ", GARDENER, false).await;
    assert_eq!(
        lines,
        [
            APPLIED,
            "change nothing: the Bot \"Gardener\" in rob's account already matches"
        ]
    );
    assert_eq!(latest(&gateway, 1).await, before);

    // Names match whatever their case, and the file's spelling wins.
    let again = GARDENER
        .replace("name = \"Gardener\"", "name = \"gardener\"")
        .replace("name = \"Watering\"", "name = \"WATERING\"");
    let lines = apply(&gateway, "rob", &again, false).await;
    assert_eq!(
        lines,
        [
            APPLIED,
            "change the Bot \"gardener\"'s name",
            "change the routine \"WATERING\"'s name",
        ]
    );
    let listed = bots(&gateway, &rob).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], id);
    assert_eq!(listed[0]["name"], "gardener");
    assert_eq!(
        names(&routines(&gateway, &rob, id).await),
        ["WATERING", "Weekly review"]
    );
}

#[tokio::test]
async fn a_dry_run_says_what_it_would_do_and_changes_nothing() {
    let (gateway, rob) = setup().await;
    let before = latest(&gateway, 1).await;
    let lines = apply(&gateway, "rob", GARDENER, true).await;
    assert_eq!(lines[0], DRY_RUN);
    assert_eq!(lines[1], "create the Bot \"Gardener\" in rob's account");
    assert_eq!(lines[2], "grant it notes, notify");
    assert!(lines[3].starts_with("create the routine \"Watering\""));
    assert!(bots(&gateway, &rob).await.is_empty());
    assert_eq!(latest(&gateway, 1).await, before);

    // And of a change to a Bot that exists.
    apply(&gateway, "rob", GARDENER, false).await;
    let bot = bots(&gateway, &rob).await[0].clone();
    let id = bot["id"].as_i64().unwrap();
    let before = latest(&gateway, 1).await;
    let changed = GARDENER
        .replace("Be brief.", "Be very brief.")
        .replace("\"07:00\"", "\"06:30\"");
    let lines = apply(&gateway, "rob", &changed, true).await;
    assert_eq!(
        lines,
        [
            DRY_RUN,
            "change the Bot \"Gardener\"'s description",
            "change the routine \"Watering\"'s schedule; daily at 06:30, next at 06:30 on \
             Saturday 16 January 2027 (Australia/Adelaide)",
        ]
    );
    assert_eq!(latest(&gateway, 1).await, before);
    let after = bots(&gateway, &rob).await[0].clone();
    assert_eq!(after["description"], bot["description"]);
    assert_eq!(after["updated_at"], bot["updated_at"]);
    let watering = routines(&gateway, &rob, id).await;
    let watering = watering.iter().find(|r| r["name"] == "Watering").unwrap();
    assert_eq!(watering["schedule"]["at"], "07:00");
}

#[tokio::test]
async fn an_update_changes_what_differs_and_keeps_what_the_file_leaves_out() {
    let (gateway, rob) = setup().await;
    apply(&gateway, "rob", GARDENER, false).await;
    let id = bots(&gateway, &rob).await[0]["id"].as_i64().unwrap();
    // The owner pins the Bot, which no definition touches.
    let reply = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/bots/{id}"))
                .cookie(&rob)
                .json(&json!({"pinned": true})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);

    // No label, avatar or grants, and only one of the routines: the rest
    // stays as it is.
    let lines = apply(
        &gateway,
        "rob",
        r#"
        name = "Gardener"
        description = "Keep track of the garden and the weather."

        [[routines]]
        name = "Watering"
        prompt = "Remind me which beds need water."
        schedule = { repeat = "weekdays", at = "06:45" }
        paused = true
        "#,
        false,
    )
    .await;
    assert_eq!(
        lines,
        [
            APPLIED,
            "change the Bot \"Gardener\"'s description",
            "change the routine \"Watering\"'s prompt and schedule, and pause it; weekdays \
             at 06:45 (Australia/Adelaide), paused",
            "keep the routine \"Weekly review\", which the definition does not name",
        ]
    );
    let bot = bots(&gateway, &rob).await[0].clone();
    assert_eq!(bot["label"], "Garden notes");
    assert_eq!(bot["avatar"], "🌱");
    assert_eq!(bot["grants"], json!(["notes", "notify"]));
    assert_eq!(bot["pinned"], true);
    assert_eq!(
        bot["description"],
        "Keep track of the garden and the weather."
    );
    let after = routines(&gateway, &rob, id).await;
    let watering = after.iter().find(|r| r["name"] == "Watering").unwrap();
    assert_eq!(watering["prompt"], "Remind me which beds need water.");
    assert_eq!(watering["paused"], true);
    assert_eq!(watering["schedule"]["repeat"], "weekdays");

    // Resumed, and the weekly review removed by name; a name the Bot does
    // not have is reported and passed over.
    let lines = apply(
        &gateway,
        "rob",
        r#"
        name = "Gardener"
        grants = []
        remove_routines = ["weekly review", "Frost watch"]

        [[routines]]
        name = "Watering"
        prompt = "Remind me which beds need water."
        schedule = { repeat = "weekdays", at = "06:45" }
        paused = false
        "#,
        false,
    )
    .await;
    assert_eq!(
        lines,
        [
            APPLIED,
            "set the grants to none (from notes, notify)",
            "change the routine \"Watering\": resume it; weekdays at 06:45, next at 06:45 on \
             Monday 18 January 2027 (Australia/Adelaide)",
            "remove the routine \"Weekly review\" and its run history",
            "leave the routine \"Frost watch\" out: the Bot has no routine of that name",
        ]
    );
    assert_eq!(names(&routines(&gateway, &rob, id).await), ["Watering"]);
    assert_eq!(bots(&gateway, &rob).await[0]["grants"], json!([]));
}

#[tokio::test]
async fn a_profile_or_grant_change_reaches_the_bots_next_turn() {
    let (gateway, rob) = setup().await;
    apply(&gateway, "rob", GARDENER, false).await;
    let id = bots(&gateway, &rob).await[0]["id"].as_i64().unwrap();
    say(&gateway, &rob, id, "Hello").await;
    assert_eq!(gateway.fake.received("thread/start").len(), 1);

    // A profile change goes to the thread's next turn as the profile
    // fragment, as an edit in the client does.
    let lines = apply(
        &gateway,
        "rob",
        "name = \"Gardener\"\ndescription = \"Only talk about roses.\"",
        false,
    )
    .await;
    assert_eq!(
        lines[1],
        "change the Bot \"Gardener\"'s description; its next turn is told its new profile"
    );
    say(&gateway, &rob, id, "And now?").await;
    let turns = gateway.fake.received("turn/start");
    let profile = &turns.last().unwrap().params["additionalContext"]["bokkie-profile"];
    assert!(
        profile["value"]
            .as_str()
            .unwrap()
            .contains("Only talk about roses."),
        "{profile}"
    );
    assert_eq!(gateway.fake.received("thread/start").len(), 1);

    // A grant change starts a fresh thread at the next turn, behind the
    // capabilities marker.
    let lines = apply(
        &gateway,
        "rob",
        "name = \"Gardener\"\ngrants = [\"notes\"]",
        false,
    )
    .await;
    assert_eq!(
        lines[1],
        "set the grants to notes (from notes, notify); its next turn starts a fresh thread"
    );
    say(&gateway, &rob, id, "Still there?").await;
    assert_eq!(gateway.fake.received("thread/start").len(), 2);
    let page = gateway
        .send(Call::get(&format!("/api/v1/bots/{id}/messages")).cookie(&rob))
        .await;
    let messages = page.body["messages"].as_array().unwrap();
    assert!(
        messages
            .iter()
            .any(|m| m["kind"] == "marker" && m["text"] == "Capabilities changed: notes"),
        "{messages:?}"
    );
}

#[tokio::test]
async fn refusals_name_their_cause_and_change_nothing() {
    let (gateway, rob) = setup().await;
    let (ann_cookie, ann) = gateway.member(&rob, "ann").await;
    let (bea_cookie, bea) = gateway
        .invited(
            &rob,
            "bea",
            Role::Member,
            Permissions {
                may_create_bots: false,
                may_edit_bot_profiles: false,
            },
        )
        .await;
    let rob_before = latest(&gateway, 1).await;
    let ann_before = latest(&gateway, ann).await;

    assert_eq!(
        refused(&gateway, "nobody", GARDENER).await,
        "No account is named \"nobody\"; this command creates none"
    );
    assert_eq!(
        refused(
            &gateway,
            "rob",
            &GARDENER.replace("[\"notes\", \"notify\"]", "[\"notes\", \"web.search\"]")
        )
        .await,
        "The grants: web.search is not available yet"
    );
    // A member may add only the grants a member may add.
    assert_eq!(
        refused(
            &gateway,
            "ann",
            &GARDENER.replace("[\"notes\", \"notify\"]", "[\"notes\", \"web.fetch\"]")
        )
        .await,
        "The grants: Only an admin can grant web.fetch"
    );
    assert_eq!(
        refused(&gateway, "bea", GARDENER).await,
        "bea may not create Bots; an admin makes that account's Bots"
    );
    let every_five = GARDENER.replace(
        "{ repeat = \"daily\", at = \"07:00\" }",
        "{ repeat = \"hours\", every = 5, at = \"07:00\" }",
    );
    assert_eq!(
        refused(&gateway, "rob", &every_five).await,
        "The routine \"Watering\": A routine repeats every 1, 2, 3, 4, 6, 8 or 12 hours"
    );
    let long = GARDENER.replace("Remind me which beds need water today.", &"x".repeat(8_001));
    assert_eq!(
        refused(&gateway, "rob", &long).await,
        "The routine \"Watering\": A routine's prompt needs 1 to 8000 characters"
    );
    // Twenty-one routines are one more than an account may have, and the
    // refusal of the last leaves even the Bot unmade.
    let mut many = String::from("name = \"Gardener\"\n");
    for n in 1..=21 {
        many.push_str(&format!(
            "[[routines]]\nname = \"Check {n}\"\nprompt = \"Check.\"\n\
             schedule = {{ repeat = \"daily\", at = \"07:00\" }}\n"
        ));
    }
    assert_eq!(
        refused(&gateway, "rob", &many).await,
        "The routine \"Check 21\": rob already has 20 routines, the most an account may have"
    );

    assert!(bots(&gateway, &rob).await.is_empty());
    assert!(bots(&gateway, &ann_cookie).await.is_empty());
    assert!(bots(&gateway, &bea_cookie).await.is_empty());
    assert_eq!(latest(&gateway, 1).await, rob_before);
    assert_eq!(latest(&gateway, ann).await, ann_before);

    // A member's Bot whose profile the member may not edit: its profile
    // stays, though its routines may change.
    let made = gateway
        .send(
            Call::post(&format!("/api/v1/admin/users/{bea}/bots"))
                .cookie(&rob)
                .json(&json!({"name": "Gardener", "label": "", "description": ""})),
        )
        .await;
    assert_eq!(made.status, StatusCode::CREATED, "{:?}", made.body);
    assert_eq!(
        refused(&gateway, "bea", GARDENER).await,
        "bea may not change its Bots' profiles"
    );
    let lines = apply(
        &gateway,
        "bea",
        "name = \"Gardener\"\nlabel = \"\"\n[[routines]]\nname = \"Watering\"\n\
         prompt = \"Water.\"\nschedule = { repeat = \"daily\", at = \"07:00\" }",
        false,
    )
    .await;
    assert!(
        lines[1].starts_with("create the routine \"Watering\""),
        "{lines:?}"
    );

    // A disabled account is refused.
    let reply = gateway
        .send(
            Call::new(Method::PATCH, &format!("/api/v1/admin/users/{ann}"))
                .cookie(&rob)
                .json(&json!({"disabled": true})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    assert_eq!(
        refused(&gateway, "ann", GARDENER).await,
        "The account ann is disabled"
    );
}

#[tokio::test]
async fn run_starts_a_named_routine_now() {
    let (gateway, rob) = setup().await;
    apply(
        &gateway,
        "rob",
        &GARDENER.replace(
            "prompt = \"Review the week's notes.\"",
            "prompt = \"Review the week's notes.\"\npaused = true",
        ),
        false,
    )
    .await;
    let id = bots(&gateway, &rob).await[0]["id"].as_i64().unwrap();
    let report = definitions::run(&gateway.state, "rob", "gardener", "weekly REVIEW")
        .await
        .unwrap_or_else(|f| panic!("{}", f.message));
    assert_eq!(report.lines.len(), 1);
    assert!(
        report.lines[0].starts_with("run ")
            && report.lines[0].contains("of the routine \"Weekly review\" on \"Gardener\": "),
        "{:?}",
        report.lines
    );
    let weekly = routines(&gateway, &rob, id).await;
    let weekly = weekly
        .iter()
        .find(|r| r["name"] == "Weekly review")
        .unwrap();
    let runs = gateway
        .wait_for(
            &rob,
            &format!("/api/v1/bots/{id}/routines/{}/runs", weekly["id"]),
            |b| b["runs"][0]["state"] == "succeeded",
        )
        .await;
    assert_eq!(runs["runs"][0]["trigger"], "manual");
    // Still paused.
    let weekly = routines(&gateway, &rob, id).await;
    let weekly = weekly
        .iter()
        .find(|r| r["name"] == "Weekly review")
        .unwrap();
    assert_eq!(weekly["paused"], true);

    let missing = |bot: &'static str, routine: &'static str| {
        let state = gateway.state.clone();
        async move {
            definitions::run(&state, "rob", bot, routine)
                .await
                .unwrap_err()
                .message
        }
    };
    assert_eq!(
        missing("Chef", "Watering").await,
        "rob has no Bot named \"Chef\""
    );
    assert_eq!(
        missing("Gardener", "Frost watch").await,
        "The Bot \"Gardener\" has no routine named \"Frost watch\""
    );
}

/// Serves `gateway`'s control socket, as `bokkie-gateway serve` does.
async fn serve_control(gateway: &Gateway) {
    let listener = control::listen(gateway.dir.path()).await.unwrap();
    tokio::spawn(control::serve(gateway.state.clone(), listener));
}

#[tokio::test]
async fn the_control_socket_is_the_gateways_alone() {
    let (gateway, rob) = setup().await;
    serve_control(&gateway).await;
    let path = control::socket_path(gateway.dir.path());
    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(path.parent().unwrap()), 0o700);
    assert_eq!(mode(&path), 0o600);

    let answer = control::send(
        gateway.dir.path(),
        &Request::Apply {
            account: "rob".into(),
            definition: GARDENER.into(),
            dry_run: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(answer.error, None);
    assert_eq!(answer.lines[0], APPLIED);
    assert_eq!(bots(&gateway, &rob).await.len(), 1);
    let answer = control::send(
        gateway.dir.path(),
        &Request::Apply {
            account: "rob".into(),
            definition: "name = \"Gardener\"\nlabels = \"x\"".into(),
            dry_run: true,
        },
    )
    .await
    .unwrap();
    assert!(answer.lines.is_empty());
    let error = answer.error.unwrap();
    assert!(
        error.starts_with("the definition is not valid:") && error.contains("labels"),
        "{error}"
    );

    // A second gateway on the same data directory is refused.
    let second = control::listen(gateway.dir.path()).await.unwrap_err();
    assert!(second.contains("another gateway"), "{second}");

    // An oversized request is refused without being read whole.
    let mut stream = tokio::net::UnixStream::connect(&path).await.unwrap();
    let big = vec![b'a'; control::MAX_REQUEST_BYTES as usize + 10];
    stream.write_all(&big).await.unwrap();
    stream.shutdown().await.unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).await.unwrap();
    let answer: Value = serde_json::from_str(&answer).unwrap();
    assert!(
        answer["error"].as_str().unwrap().contains("larger than"),
        "{answer}"
    );
}

#[tokio::test]
async fn a_socket_left_by_a_stopped_gateway_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = control::socket_path(dir.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    assert!(path.exists());
    control::listen(dir.path()).await.unwrap();

    // A path too long for a Unix socket is refused before anything is
    // made, by the gateway and the command alike.
    let deep = dir.path().join("d".repeat(control::MAX_SOCKET_PATH_BYTES));
    let error = control::listen(&deep).await.unwrap_err();
    assert!(error.contains("shorter data directory"), "{error}");
    assert!(!deep.exists());
    let request = Request::Run {
        account: "rob".into(),
        bot: "Gardener".into(),
        routine: "Watering".into(),
    };
    let error = control::send(&deep, &request).await.unwrap_err();
    assert!(error.contains("shorter data directory"), "{error}");

    // Anything else in its place is not removed.
    let other = tempfile::tempdir().unwrap();
    let path = control::socket_path(other.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "not a socket").unwrap();
    let error = control::listen(other.path()).await.unwrap_err();
    assert!(error.contains("in the way"), "{error}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "not a socket");
}

async fn command(data: &std::path::Path, args: &[&str], input: &str) -> (bool, String, String) {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_bokkie-gateway"))
        .args(args)
        .env("BOKKIE_DATA", data)
        .env("BOKKIE_LOG", "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input.as_bytes()).await.unwrap();
    drop(stdin);
    let output = child.wait_with_output().await.unwrap();
    (
        output.status.success(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[tokio::test]
async fn the_command_applies_a_definition_from_standard_input_or_a_file() {
    let (gateway, rob) = setup().await;
    serve_control(&gateway).await;
    let data = gateway.dir.path();

    let (ok, out, err) = command(data, &["bot", "apply", "--dry-run", "rob", "-"], GARDENER).await;
    assert!(ok, "{err}");
    let mut lines = out.lines();
    assert_eq!(lines.next(), Some(DRY_RUN));
    assert_eq!(
        lines.next(),
        Some("- create the Bot \"Gardener\" in rob's account")
    );
    assert!(bots(&gateway, &rob).await.is_empty());

    let file = data.join("gardener.toml");
    std::fs::write(&file, GARDENER).unwrap();
    let (ok, out, err) = command(data, &["bot", "apply", "rob", file.to_str().unwrap()], "").await;
    assert!(ok, "{err}");
    assert!(out.starts_with(APPLIED), "{out}");
    assert_eq!(bots(&gateway, &rob).await.len(), 1);

    let (ok, out, err) = command(data, &["bot", "run", "rob", "Gardener", "Watering"], "").await;
    assert!(ok, "{err}");
    assert!(out.contains("of the routine \"Watering\""), "{out}");

    // A refusal fails the command with the gateway's reason.
    let (ok, _, err) = command(data, &["bot", "apply", "nobody", "-"], GARDENER).await;
    assert!(!ok);
    assert!(err.contains("No account is named \"nobody\""), "{err}");
    // As does a wrong use, with the usage.
    let (ok, _, err) = command(data, &["bot", "apply", "rob"], "").await;
    assert!(!ok);
    assert!(err.contains("bot apply [--dry-run]"), "{err}");
    // And no gateway to ask.
    let empty = tempfile::tempdir().unwrap();
    let (ok, _, err) = command(empty.path(), &["bot", "apply", "rob", "-"], GARDENER).await;
    assert!(!ok);
    assert!(err.contains("needs a running gateway"), "{err}");
}
