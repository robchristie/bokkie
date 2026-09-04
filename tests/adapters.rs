use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    ops::{Deref, DerefMut},
    path::Path,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};
use tempfile::TempDir;

use bokkie::{
    Completion, InspectionResult, NewGardenerImplementationRun, NewGardenerInspection,
    NewRepositoryRegistration, Recurrence, Store, proposal_fingerprint,
};

const POLL: Duration = Duration::from_millis(25);

#[test]
fn cli_lifecycle_operations_return_structured_json_and_exit_statuses() {
    let temporary = TempDir::new().unwrap();
    let database = temporary.path().join("cli.sqlite");

    let created = cli_json(
        &database,
        &[
            "create",
            "--id",
            "cli-one",
            "--description",
            "operator review",
            "--approval-required",
        ],
    );
    assert_eq!(created["state"], "awaiting_approval");
    assert_eq!(cli_json(&database, &["list"])[0]["id"], "cli-one");
    assert_eq!(cli_json(&database, &["show", "cli-one"])["id"], "cli-one");

    let rejected = cli_json(
        &database,
        &["reject", "cli-one", "--actor", "integration-test"],
    );
    assert_eq!(rejected["state"], "attention");
    assert_eq!(
        cli_json(&database, &["retry", "cli-one"])["state"],
        "awaiting_approval"
    );
    assert_eq!(
        cli_json(
            &database,
            &["approve", "cli-one", "--actor", "integration-test"]
        )["state"],
        "pending"
    );
    assert_eq!(
        cli_json(&database, &["cancel", "cli-one"])["state"],
        "cancelled"
    );
    assert!(
        cli_json(&database, &["events", "cli-one"])
            .as_array()
            .unwrap()
            .len()
            >= 5
    );
    assert_eq!(cli_json(&database, &["attempts", "cli-one"]), json!([]));

    let missing = run_cli(&database, &["show", "does-not-exist"]);
    assert_eq!(missing.status.code(), Some(3));
    let error: Value = serde_json::from_slice(&missing.stderr).unwrap();
    assert_eq!(error["error"]["code"], "not_found");
}

#[test]
fn doctor_cli_reports_clean_state_without_mutating_or_creating_a_database() {
    let temporary = TempDir::new().unwrap();
    let database = temporary.path().join("doctor.sqlite");
    drop(Store::open(&database).unwrap());
    let before = std::fs::read(&database).unwrap();

    let report = cli_json(
        &database,
        &[
            "doctor",
            "--git-executable",
            "/bin/false",
            "--github-public-observer-executable",
            "/bin/false",
        ],
    );
    assert_eq!(report["format_version"], 1);
    assert_eq!(report["repair_performed"], false);
    assert_eq!(report["summary"]["healthy"], true);
    assert!(
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["code"] == "schema.migration_manifest" && check["status"] == "pass")
    );
    assert_eq!(std::fs::read(&database).unwrap(), before);

    let missing = temporary.path().join("missing.sqlite");
    let output = run_cli(&missing, &["doctor"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        !missing.exists(),
        "doctor must not create a missing database"
    );
}

#[test]
fn gardener_cli_registers_and_exposes_persisted_state_and_decisions() {
    let temporary = TempDir::new().unwrap();
    let database = temporary.path().join("gardener-cli.sqlite");
    let checkout = temporary.path().join("checkout");
    std::fs::create_dir(&checkout).unwrap();
    let checkout = checkout.to_str().unwrap();
    let registration_arguments = [
        "gardener",
        "register",
        "--checkout-path",
        checkout,
        "--first-inspection-at",
        "2000000000",
        "--recurrence-cron",
        "30 9 * * *",
        "--recurrence-timezone",
        "Australia/Adelaide",
    ];
    let registered = cli_json(&database, &registration_arguments);
    assert_eq!(registered["repository"], "robchristie/bokkie");
    assert_eq!(registered["default_branch"], "main");
    assert_eq!(registered["checkout_path"], checkout);
    assert_eq!(
        cli_json(&database, &registration_arguments),
        registered,
        "identical registration must be idempotent"
    );
    assert_eq!(cli_json(&database, &["gardener", "repository"]), registered);

    let conflict = run_cli(
        &database,
        &[
            "gardener",
            "register",
            "--checkout-path",
            checkout,
            "--first-inspection-at",
            "2000000001",
        ],
    );
    assert_eq!(conflict.status.code(), Some(4));
    assert_eq!(
        serde_json::from_slice::<Value>(&conflict.stderr).unwrap()["error"]["code"],
        "transition_conflict"
    );
    let noncanonical = run_cli(
        &temporary.path().join("noncanonical.sqlite"),
        &[
            "gardener",
            "register",
            "--repository",
            "someone/else",
            "--checkout-path",
            checkout,
        ],
    );
    assert_eq!(noncanonical.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&noncanonical.stderr).unwrap()["error"]["code"],
        "invalid_request"
    );

    let database = temporary.path().join("gardener-state-cli.sqlite");
    let seeded = seed_gardener_state(&database, checkout);
    let inspections = cli_json(&database, &["gardener", "inspections", "list"]);
    assert_eq!(inspections.as_array().unwrap().len(), 2);
    assert_eq!(
        cli_json(
            &database,
            &["gardener", "inspections", "show", "inspection-one"]
        )["source_commit"],
        "a".repeat(40)
    );
    let proposals = cli_json(&database, &["gardener", "proposals", "list"]);
    assert_eq!(proposals.as_array().unwrap().len(), 3);
    assert_eq!(
        cli_json(
            &database,
            &[
                "gardener",
                "proposals",
                "show",
                &seeded.approved_fingerprint
            ]
        )["observation_count"],
        2
    );
    let instances = cli_json(&database, &["gardener", "proposal-instances", "list"]);
    assert_eq!(instances.as_array().unwrap().len(), 4);
    let latest = cli_json(
        &database,
        &[
            "gardener",
            "proposal-instances",
            "show",
            &seeded.approved_latest_instance_id,
        ],
    );
    assert_eq!(latest["generation"], 2);
    assert_eq!(latest["source_commit"], "b".repeat(40));
    assert_eq!(latest["source_observation_id"], 4);
    assert_eq!(latest["source_inspection_id"], "inspection-two");
    assert_eq!(
        cli_json(
            &database,
            &[
                "gardener",
                "proposal-instances",
                "observations",
                &seeded.approved_latest_instance_id,
            ],
        )
        .as_array()
        .unwrap()
        .len(),
        1
    );
    assert_eq!(
        cli_json(
            &database,
            &[
                "gardener",
                "proposals",
                "observations",
                &seeded.approved_fingerprint
            ]
        )
        .as_array()
        .unwrap()
        .len(),
        2
    );

    let before_approval = Store::open(&database)
        .unwrap()
        .claim_due_gardener(3_000, 60, 10)
        .unwrap();
    assert!(before_approval.iter().all(|claim| {
        claim.obligation_id != format!("gardener:implement:{}", seeded.approved_fingerprint)
    }));
    let ambiguous = run_cli(
        &database,
        &[
            "gardener",
            "proposals",
            "approve",
            &seeded.approved_fingerprint,
            "--actor",
            "operator",
            "--note",
            "bounded and useful",
        ],
    );
    assert_eq!(ambiguous.status.code(), Some(4));
    assert_eq!(
        serde_json::from_slice::<Value>(&ambiguous.stderr).unwrap()["error"]["code"],
        "transition_conflict"
    );
    let approved = cli_json(
        &database,
        &[
            "gardener",
            "proposal-instances",
            "approve",
            &seeded.approved_latest_instance_id,
            "--actor",
            "operator",
            "--note",
            "bounded and useful",
        ],
    );
    assert_eq!(approved["approval_decision"], "approved");
    assert_eq!(approved["obligation_state"], "pending");
    let rejected = cli_json(
        &database,
        &[
            "gardener",
            "proposal-instances",
            "reject",
            &seeded.rejected_instance_id,
            "--actor",
            "operator",
        ],
    );
    assert_eq!(rejected["approval_decision"], "rejected");
    assert_eq!(rejected["obligation_state"], "attention");

    create_seeded_run(&database, &seeded.approved_fingerprint);
    assert_eq!(
        cli_json(&database, &["gardener", "runs", "list"])[0]["id"],
        "run-one"
    );
    assert_eq!(
        cli_json(&database, &["gardener", "runs", "show", "run-one"])["phase"],
        "created"
    );
    let run_events = cli_json(&database, &["gardener", "runs", "events", "run-one"]);
    assert_eq!(run_events[0]["event_type"], "implementation_run_created");
    let run_evidence: Value =
        serde_json::from_str(run_events[0]["details_json"].as_str().unwrap()).unwrap();
    assert_eq!(run_evidence["source_commit"], "b".repeat(40));
    assert_eq!(run_evidence["branch"], "codex/gardener-run-one");

    let missing = run_cli(
        &database,
        &["gardener", "proposals", "observations", "missing"],
    );
    assert_eq!(missing.status.code(), Some(3));
    assert_eq!(
        serde_json::from_slice::<Value>(&missing.stderr).unwrap()["error"]["code"],
        "not_found"
    );
}

#[test]
fn gardener_http_exposes_registration_evidence_and_decisions() {
    let temporary = TempDir::new().unwrap();
    let database = temporary.path().join("gardener-api.sqlite");
    let checkout = temporary.path().join("checkout");
    std::fs::create_dir(&checkout).unwrap();
    let checkout = checkout.to_str().unwrap();
    let address = unused_loopback_address();
    let mut daemon = spawn_daemon(&database, address, &[]);
    wait_until(Duration::from_secs(5), || {
        http_json(address, "GET", "/health", None)
            .is_some_and(|(status, body)| status == 200 && body["status"] == "ok")
    });

    let (canonical_status, canonical_error) = http_json(
        address,
        "POST",
        "/gardener/repository",
        Some(json!({
            "repository": "someone/else",
            "checkout_path": checkout
        })),
    )
    .unwrap();
    assert_eq!(canonical_status, 400);
    assert_eq!(canonical_error["error"]["code"], "invalid_request");
    let registration_body = json!({
        "checkout_path": checkout,
        "first_inspection_at": 2_000_000_000_i64,
        "recurrence_cron": "30 9 * * *",
        "recurrence_timezone": "Australia/Adelaide"
    });
    let (status, registered) = http_json(
        address,
        "POST",
        "/gardener/repository",
        Some(registration_body.clone()),
    )
    .unwrap();
    assert_eq!(status, 201);
    let (_, duplicate) = http_json(
        address,
        "POST",
        "/gardener/repository",
        Some(registration_body),
    )
    .unwrap();
    assert_eq!(duplicate, registered);
    assert_eq!(
        http_json(address, "GET", "/gardener/repository", None)
            .unwrap()
            .1,
        registered
    );
    let (conflict_status, conflict) = http_json(
        address,
        "POST",
        "/gardener/repository",
        Some(json!({
            "checkout_path": checkout,
            "first_inspection_at": 2_000_000_001_i64
        })),
    )
    .unwrap();
    assert_eq!(conflict_status, 409);
    assert_eq!(conflict["error"]["code"], "transition_conflict");
    stop_child(&mut daemon);

    let database = temporary.path().join("gardener-state-api.sqlite");
    let seeded = seed_gardener_state(&database, checkout);
    let address = unused_loopback_address();
    let mut daemon = spawn_daemon(&database, address, &[]);
    wait_until(Duration::from_secs(5), || {
        http_json(address, "GET", "/health", None).is_some()
    });
    assert_eq!(
        http_json(address, "GET", "/gardener/inspections", None)
            .unwrap()
            .1
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        http_json(address, "GET", "/gardener/inspections/inspection-two", None)
            .unwrap()
            .1["source_commit"],
        "b".repeat(40)
    );
    assert_eq!(
        http_json(address, "GET", "/gardener/proposals", None)
            .unwrap()
            .1
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let (_, instances) = http_json(address, "GET", "/gardener/proposal-instances", None).unwrap();
    assert_eq!(instances.as_array().unwrap().len(), 4);
    let latest_instance_path = format!(
        "/gardener/proposal-instances/{}",
        seeded.approved_latest_instance_id
    );
    let (_, latest_instance) = http_json(address, "GET", &latest_instance_path, None).unwrap();
    assert_eq!(latest_instance["generation"], 2);
    assert_eq!(latest_instance["source_commit"], "b".repeat(40));
    assert_eq!(latest_instance["source_inspection_id"], "inspection-two");
    let generic_generation_two_path = format!(
        "/obligations/{}/approve",
        latest_instance["implementation_obligation_id"]
            .as_str()
            .unwrap()
    );
    let (generic_status, generic_error) = http_json(
        address,
        "POST",
        &generic_generation_two_path,
        Some(json!({"actor": "http-operator"})),
    )
    .unwrap();
    assert_eq!(generic_status, 409);
    assert_eq!(generic_error["error"]["code"], "transition_conflict");
    let exact_observations_path = format!(
        "/gardener/proposal-instances/{}/observations",
        seeded.approved_latest_instance_id
    );
    assert_eq!(
        http_json(address, "GET", &exact_observations_path, None)
            .unwrap()
            .1
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let observations_path = format!(
        "/gardener/proposals/{}/observations",
        seeded.approved_fingerprint
    );
    assert_eq!(
        http_json(address, "GET", &observations_path, None)
            .unwrap()
            .1
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let approve_path = format!(
        "/gardener/proposals/{}/approve",
        seeded.approved_fingerprint
    );
    let (ambiguous_status, ambiguous) = http_json(
        address,
        "POST",
        &approve_path,
        Some(json!({"actor": "http-operator", "note": "approved exactly"})),
    )
    .unwrap();
    assert_eq!(ambiguous_status, 409);
    assert_eq!(ambiguous["error"]["code"], "transition_conflict");
    let exact_approve_path = format!(
        "/gardener/proposal-instances/{}/approve",
        seeded.approved_latest_instance_id
    );
    let (_, approved) = http_json(
        address,
        "POST",
        &exact_approve_path,
        Some(json!({"actor": "http-operator", "note": "approved exactly"})),
    )
    .unwrap();
    assert_eq!(approved["obligation_state"], "pending");
    assert_eq!(approved["approval_decision"], "approved");
    let reject_path = format!(
        "/gardener/proposal-instances/{}/reject",
        seeded.rejected_instance_id
    );
    let (_, rejected) = http_json(
        address,
        "POST",
        &reject_path,
        Some(json!({"actor": "http-operator", "note": "not suitable"})),
    )
    .unwrap();
    assert_eq!(rejected["obligation_state"], "attention");
    let conditional_path = format!(
        "/operator/gardener/proposal-instances/{}/approve",
        seeded.conditional_instance_id
    );
    let conditional_instance = http_json(
        address,
        "GET",
        &format!(
            "/gardener/proposal-instances/{}",
            seeded.conditional_instance_id
        ),
        None,
    )
    .unwrap()
    .1;
    let conditional_body = operator_action_body(
        address,
        conditional_instance["implementation_obligation_id"]
            .as_str()
            .unwrap(),
        "approve_gardener_proposal",
        Some("http-operator"),
        Some("reviewed with a state precondition"),
    );
    let mut stale_generation_body = conditional_body.clone();
    stale_generation_body["precondition"]["gardener_generation"] = json!(2);
    let (stale_status, stale_error) = http_json(
        address,
        "POST",
        &conditional_path,
        Some(stale_generation_body),
    )
    .unwrap();
    assert_eq!(stale_status, 409);
    assert_eq!(stale_error["error"]["code"], "transition_conflict");
    let (_, conditionally_approved) =
        http_json(address, "POST", &conditional_path, Some(conditional_body)).unwrap();
    assert_eq!(conditionally_approved["obligation_state"], "pending");
    assert_eq!(conditionally_approved["approval_decision"], "approved");
    stop_child(&mut daemon);

    create_seeded_run(&database, &seeded.approved_fingerprint);
    let address = unused_loopback_address();
    let mut daemon = spawn_daemon(&database, address, &[]);
    wait_until(Duration::from_secs(5), || {
        http_json(address, "GET", "/health", None).is_some()
    });
    assert_eq!(
        http_json(address, "GET", "/gardener/runs", None).unwrap().1[0]["id"],
        "run-one"
    );
    assert_eq!(
        http_json(address, "GET", "/gardener/runs/run-one", None)
            .unwrap()
            .1["proposal_fingerprint"],
        seeded.approved_fingerprint
    );
    assert_eq!(
        http_json(address, "GET", "/gardener/runs/run-one/events", None)
            .unwrap()
            .1[0]["event_type"],
        "implementation_run_created"
    );
    let (missing_status, missing) =
        http_json(address, "GET", "/gardener/runs/missing/events", None).unwrap();
    assert_eq!(missing_status, 404);
    assert_eq!(missing["error"]["code"], "not_found");

    stop_child(&mut daemon);
}

#[test]
fn gardener_service_requires_explicit_valid_runtime_configuration() {
    let temporary = TempDir::new().unwrap();
    let database = temporary.path().join("service-config.sqlite");
    let missing_root = run_cli(&database, &["serve", "--enable-coding-gardener"]);
    assert_eq!(missing_root.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&missing_root.stderr).unwrap()["error"]["code"],
        "invalid_arguments"
    );

    let relative_root = run_cli(
        &database,
        &[
            "serve",
            "--bind",
            &unused_loopback_address().to_string(),
            "--enable-coding-gardener",
            "--gardener-worktree-root",
            "relative",
        ],
    );
    assert_eq!(relative_root.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&relative_root.stderr).unwrap()["error"]["code"],
        "invalid_request"
    );

    let excessive_heartbeat = run_cli(
        &database,
        &[
            "serve",
            "--bind",
            &unused_loopback_address().to_string(),
            "--enable-coding-gardener",
            "--gardener-worktree-root",
            temporary.path().to_str().unwrap(),
            "--lease-seconds",
            "30",
            "--gardener-heartbeat-ms",
            "10001",
        ],
    );
    assert_eq!(excessive_heartbeat.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&excessive_heartbeat.stderr).unwrap();
    assert_eq!(error["error"]["code"], "invalid_request");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("one third")
    );
}

#[test]
fn occupied_listener_prevents_startup_migration() {
    let temporary = TempDir::new().unwrap();
    let database = temporary.path().join("must-not-be-created.sqlite");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();

    let output = run_cli(
        &database,
        &["serve", "--bind", &address.to_string(), "--poll-ms", "20"],
    );
    assert!(!output.status.success());
    assert!(
        !database.exists(),
        "a failed listener bind must not create or migrate the database"
    );
}

#[test]
fn loopback_api_exposes_lifecycle_and_history() {
    let temporary = TempDir::new().unwrap();
    let database = temporary.path().join("api.sqlite");
    let address = unused_loopback_address();
    let mut daemon = spawn_daemon(&database, address, &["--fake-delay-ms", "0"]);

    wait_until(Duration::from_secs(5), || {
        http_json(address, "GET", "/health", None)
            .is_some_and(|(status, body)| status == 200 && body["status"] == "ok")
    });

    let (invalid_status, invalid) =
        http_json(address, "POST", "/obligations", Some(json!({}))).unwrap();
    assert_eq!(invalid_status, 422);
    assert_eq!(invalid["error"]["code"], "invalid_json");
    let (route_status, route_error) = http_json(address, "GET", "/missing-route", None).unwrap();
    assert_eq!(route_status, 404);
    assert_eq!(route_error["error"]["code"], "route_not_found");
    let (method_status, method_error) =
        http_json(address, "POST", "/health", Some(json!({}))).unwrap();
    assert_eq!(method_status, 405);
    assert_eq!(method_error["error"]["code"], "method_not_allowed");

    let scheduled_at = unix_now() + 3_600;
    let (status, created) = http_json(
        address,
        "POST",
        "/obligations",
        Some(json!({
            "id": "api-one",
            "description": "API lifecycle",
            "scheduled_at": scheduled_at,
            "approval_required": true,
            "max_attempts": 2,
            "retry_base_seconds": 1,
            "retry_max_seconds": 2
        })),
    )
    .unwrap();
    assert_eq!(status, 201);
    assert_eq!(created["state"], "awaiting_approval");

    let (_, listed) = http_json(address, "GET", "/obligations", None).unwrap();
    assert_eq!(listed[0]["id"], "api-one");
    let (_, shown) = http_json(address, "GET", "/obligations/api-one", None).unwrap();
    assert_eq!(shown["description"], "API lifecycle");

    let (_, rejected) = http_json(
        address,
        "POST",
        "/obligations/api-one/reject",
        Some(json!({"actor": "api-test", "note": "not yet"})),
    )
    .unwrap();
    assert_eq!(rejected["state"], "attention");
    let (_, retried) = http_json(
        address,
        "POST",
        "/obligations/api-one/retry",
        Some(json!({})),
    )
    .unwrap();
    assert_eq!(retried["state"], "awaiting_approval");
    let (_, approved) = http_json(
        address,
        "POST",
        "/obligations/api-one/approve",
        Some(json!({"actor": "api-test"})),
    )
    .unwrap();
    assert_eq!(approved["state"], "pending");
    let (_, cancelled) = http_json(address, "POST", "/obligations/api-one/cancel", None).unwrap();
    assert_eq!(cancelled["state"], "cancelled");

    let (_, events) = http_json(address, "GET", "/obligations/api-one/events", None).unwrap();
    assert!(events.as_array().unwrap().len() >= 5);
    let (_, attempts) = http_json(address, "GET", "/obligations/api-one/attempts", None).unwrap();
    assert_eq!(attempts, json!([]));

    let (missing_status, missing) =
        http_json(address, "GET", "/obligations/missing", None).unwrap();
    assert_eq!(missing_status, 404);
    assert_eq!(missing["error"]["code"], "not_found");

    stop_child(&mut daemon);
}

#[test]
fn attention_ui_probe_reads_cancels_refreshes_and_serves_same_origin_assets() {
    let temporary = TempDir::new().unwrap();
    let database = temporary.path().join("attention-ui.sqlite");
    let ui_dir = temporary.path().join("ui");
    std::fs::create_dir(&ui_dir).unwrap();
    std::fs::write(
        ui_dir.join("index.html"),
        "<!doctype html><title>Bokkie attention probe</title>",
    )
    .unwrap();
    let address = unused_loopback_address();
    let mut daemon = spawn_daemon(&database, address, &["--ui-dir", ui_dir.to_str().unwrap()]);

    wait_until(Duration::from_secs(5), || {
        http_json(address, "GET", "/health", None).is_some()
    });
    let (created_status, created) = http_json(
        address,
        "POST",
        "/obligations",
        Some(json!({
            "id": "attention-ui-future-fixture",
            "description": "Harmless future approval-bound UI probe",
            "scheduled_at": 4_102_444_800_i64,
            "approval_required": true
        })),
    )
    .unwrap();
    assert_eq!(created_status, 201);
    assert_eq!(created["state"], "awaiting_approval");

    let (_, before) = http_json(address, "GET", "/obligations", None).unwrap();
    assert_eq!(before[0]["id"], "attention-ui-future-fixture");
    assert_eq!(before[0]["state"], "awaiting_approval");

    let (cancel_status, cancelled) = http_json(
        address,
        "POST",
        "/operator/obligations/attention-ui-future-fixture/cancel",
        Some(operator_action_body(
            address,
            "attention-ui-future-fixture",
            "cancel",
            None,
            None,
        )),
    )
    .unwrap();
    assert_eq!(cancel_status, 200);
    assert_eq!(cancelled["state"], "cancelled");

    let (_, refreshed) = http_json(address, "GET", "/obligations", None).unwrap();
    assert_eq!(refreshed[0]["state"], "cancelled");
    let (_, events) = http_json(
        address,
        "GET",
        "/obligations/attention-ui-future-fixture/events",
        None,
    )
    .unwrap();
    let last = events.as_array().unwrap().last().unwrap();
    assert_eq!(last["event_type"], "cancelled");
    assert_eq!(last["from_state"], "awaiting_approval");
    assert_eq!(last["to_state"], "cancelled");

    let ui_response = http_raw(address, "GET", "/ui/").unwrap();
    assert!(ui_response.starts_with("HTTP/1.1 200"));
    assert!(ui_response.contains("Bokkie attention probe"));
    assert!(
        !ui_response
            .to_ascii_lowercase()
            .contains("access-control-allow-origin")
    );

    stop_child(&mut daemon);
}

#[test]
fn killed_daemon_recovers_expired_claim_and_succeeds_once() {
    let temporary = TempDir::new().unwrap();
    let database = temporary.path().join("recovery.sqlite");
    let create = run_cli(
        &database,
        &[
            "create",
            "--id",
            "crash-one",
            "--description",
            "crash recovery proof",
            "--max-attempts",
            "2",
            "--retry-base-seconds",
            "1",
            "--retry-max-seconds",
            "1",
        ],
    );
    assert_success(&create);

    let first_address = unused_loopback_address();
    let mut first = spawn_daemon(
        &database,
        first_address,
        &["--lease-seconds", "2", "--fake-delay-ms", "30000"],
    );
    wait_until(Duration::from_secs(5), || {
        try_cli_json(&database, &["show", "crash-one"])
            .is_some_and(|obligation| obligation["state"] == "running")
    });
    first
        .kill()
        .expect("kill first daemon after its durable claim");
    first.wait().unwrap();

    let second_address = unused_loopback_address();
    let mut second = spawn_daemon(
        &database,
        second_address,
        &["--lease-seconds", "2", "--fake-delay-ms", "0"],
    );
    wait_until(Duration::from_secs(8), || {
        try_cli_json(&database, &["show", "crash-one"])
            .is_some_and(|obligation| obligation["state"] == "completed")
    });

    let attempts = cli_json(&database, &["attempts", "crash-one"]);
    let attempts = attempts.as_array().unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["outcome"], "lease_expired");
    assert_eq!(attempts[1]["outcome"], "succeeded");
    assert_eq!(
        attempts
            .iter()
            .filter(|attempt| attempt["outcome"] == "succeeded")
            .count(),
        1
    );

    let events = cli_json(&database, &["events", "crash-one"]);
    let event_types: Vec<_> = events
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["event_type"].as_str().unwrap())
        .collect();
    assert!(event_types.contains(&"lease_expired_retry_scheduled"));
    assert_eq!(
        event_types
            .iter()
            .filter(|event_type| **event_type == "completed")
            .count(),
        1
    );

    stop_child(&mut second);
}

#[test]
fn exhausted_finite_recurrence_completes_without_stopping_unrelated_work() {
    let temporary = TempDir::new().unwrap();
    let database = temporary.path().join("finite-recurrence.sqlite");
    let finite = run_cli(
        &database,
        &[
            "create",
            "--id",
            "a-finite",
            "--description",
            "final finite occurrence",
            "--scheduled-at",
            "1",
            "--recurrence-cron",
            "0 0 0 1 1 * 1970",
            "--recurrence-timezone",
            "UTC",
        ],
    );
    assert_success(&finite);
    let unrelated = run_cli(
        &database,
        &[
            "create",
            "--id",
            "z-unrelated",
            "--description",
            "must run after finite recurrence",
            "--scheduled-at",
            "2",
        ],
    );
    assert_success(&unrelated);

    let address = unused_loopback_address();
    let mut daemon = spawn_daemon(&database, address, &["--fake-delay-ms", "0"]);
    wait_until(Duration::from_secs(5), || {
        try_cli_json(&database, &["show", "a-finite"])
            .is_some_and(|obligation| obligation["state"] == "completed")
            && try_cli_json(&database, &["show", "z-unrelated"])
                .is_some_and(|obligation| obligation["state"] == "completed")
    });

    assert!(
        daemon.try_wait().unwrap().is_none(),
        "scheduler must remain available after a finite recurrence is exhausted"
    );
    assert_eq!(
        cli_json(&database, &["attempts", "a-finite"])[0]["outcome"],
        "succeeded"
    );
    assert_eq!(
        cli_json(&database, &["attempts", "z-unrelated"])[0]["outcome"],
        "succeeded"
    );
    let finite_events = cli_json(&database, &["events", "a-finite"]);
    let completed = finite_events
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event_type"] == "completed")
        .unwrap();
    let completed_details: Value =
        serde_json::from_str(completed["details_json"].as_str().unwrap()).unwrap();
    assert_eq!(
        completed_details,
        json!({
            "evidence": "deterministic fake success for attempt 1",
            "reason": "recurrence_exhausted"
        })
    );

    stop_child(&mut daemon);
}

#[cfg(unix)]
#[test]
fn graceful_shutdown_finishes_an_observed_in_flight_claim() {
    let temporary = TempDir::new().unwrap();
    let database = temporary.path().join("graceful.sqlite");
    let create = run_cli(
        &database,
        &[
            "create",
            "--id",
            "graceful-one",
            "--description",
            "finish during shutdown",
        ],
    );
    assert_success(&create);

    let address = unused_loopback_address();
    let mut daemon = spawn_daemon(
        &database,
        address,
        &["--lease-seconds", "2", "--fake-delay-ms", "750"],
    );
    wait_until(Duration::from_secs(5), || {
        try_cli_json(&database, &["show", "graceful-one"])
            .is_some_and(|obligation| obligation["state"] == "running")
    });

    let signal = Command::new("kill")
        .arg("-TERM")
        .arg(daemon.id().to_string())
        .output()
        .unwrap();
    assert_success(&signal);
    let status = daemon.wait().unwrap();
    assert!(status.success(), "daemon exited unsuccessfully: {status}");

    let obligation = cli_json(&database, &["show", "graceful-one"]);
    assert_eq!(obligation["state"], "completed");
    let attempts = cli_json(&database, &["attempts", "graceful-one"]);
    assert_eq!(attempts.as_array().unwrap().len(), 1);
    assert_eq!(attempts[0]["outcome"], "succeeded");
}

#[test]
fn scheduler_failure_stops_http_and_exits_non_zero() {
    let temporary = TempDir::new().unwrap();
    let database = temporary.path().join("scheduler-failure.sqlite");
    let address = unused_loopback_address();
    let mut daemon = spawn_daemon(&database, address, &["--fake-delay-ms", "0"]);

    wait_until(Duration::from_secs(5), || {
        http_json(address, "GET", "/health", None)
            .is_some_and(|(status, body)| status == 200 && body["status"] == "ok")
    });

    // Removing a required table after start deterministically makes the next claim
    // fail while leaving /health readable, reproducing the former silent-stop case.
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute_batch("DROP TABLE attempts;").unwrap();
    drop(connection);
    let (health_status, health) = http_json(address, "GET", "/health", None).unwrap();
    assert_eq!(health_status, 200);
    assert_eq!(health["status"], "ok");
    let created = run_cli(
        &database,
        &[
            "create",
            "--id",
            "failure-one",
            "--description",
            "force scheduler storage failure",
        ],
    );
    assert_success(&created);

    let status = wait_for_child_exit(&mut daemon, Duration::from_secs(5));
    assert!(!status.success(), "scheduler failure must fail the service");
    let mut stderr = String::new();
    daemon
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let error: Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
    assert_eq!(error["error"]["code"], "scheduler_error");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("attempts")
    );
}

struct SeededGardener {
    approved_fingerprint: String,
    approved_latest_instance_id: String,
    rejected_instance_id: String,
    conditional_instance_id: String,
}

fn seed_gardener_state(database: &Path, checkout: &str) -> SeededGardener {
    const APPROVED_PROMPT: &str = "Implement the bounded adapter improvement.";
    const REJECTED_PROMPT: &str = "Replace the entire repository without review.";
    const HTTP_PROMPT: &str = "Document one additional operator invariant.";
    let mut store = Store::open(database).unwrap();
    store
        .register_gardener_repository(
            NewRepositoryRegistration {
                repository: "robchristie/bokkie".to_owned(),
                default_branch: "main".to_owned(),
                checkout_path: checkout.to_owned(),
                inspection_recurrence: Recurrence::new("* * * * *", "UTC").unwrap(),
                first_inspection_at: 1_000,
            },
            900,
        )
        .unwrap();

    let first_claim = store.claim_due_gardener(1_000, 500, 1).unwrap().remove(0);
    store
        .start_gardener_inspection(
            &first_claim,
            NewGardenerInspection {
                id: "inspection-one".to_owned(),
                source_commit: "a".repeat(40),
                worktree_path: "/tmp/bokkie-inspection-one".to_owned(),
                prompt_digest: "1".repeat(64),
            },
            1_001,
        )
        .unwrap();
    store
        .finish_gardener_inspection(
            &first_claim,
            "inspection-one",
            &InspectionResult {
                summary: "Three bounded candidates were observed".to_owned(),
                proposed_goal_prompts: vec![
                    APPROVED_PROMPT.to_owned(),
                    REJECTED_PROMPT.to_owned(),
                    HTTP_PROMPT.to_owned(),
                ],
            },
            1_002,
        )
        .unwrap();
    store
        .complete(
            &first_claim,
            Completion::Succeeded {
                evidence: Some("seeded inspection one".to_owned()),
            },
            1_003,
        )
        .unwrap();

    let second_claim = store
        .claim_due_gardener(2_000, 500, 10)
        .unwrap()
        .into_iter()
        .find(|claim| claim.obligation_id == "gardener:inspect:robchristie/bokkie")
        .unwrap();
    store
        .start_gardener_inspection(
            &second_claim,
            NewGardenerInspection {
                id: "inspection-two".to_owned(),
                source_commit: "b".repeat(40),
                worktree_path: "/tmp/bokkie-inspection-two".to_owned(),
                prompt_digest: "2".repeat(64),
            },
            2_001,
        )
        .unwrap();
    store
        .finish_gardener_inspection(
            &second_claim,
            "inspection-two",
            &InspectionResult {
                summary: "The same bounded candidate remains relevant".to_owned(),
                proposed_goal_prompts: vec![format!("\n{APPROVED_PROMPT}   \n")],
            },
            2_002,
        )
        .unwrap();
    store
        .complete(
            &second_claim,
            Completion::Succeeded {
                evidence: Some("seeded inspection two".to_owned()),
            },
            2_003,
        )
        .unwrap();

    let approved_fingerprint = proposal_fingerprint("robchristie/bokkie", APPROVED_PROMPT);
    let rejected_fingerprint = proposal_fingerprint("robchristie/bokkie", REJECTED_PROMPT);
    let conditional_fingerprint = proposal_fingerprint("robchristie/bokkie", HTTP_PROMPT);
    let approved_instances = store
        .gardener_proposal_instances(&approved_fingerprint)
        .unwrap();
    let rejected_instance_id = store
        .gardener_proposal_instances(&rejected_fingerprint)
        .unwrap()[0]
        .id
        .clone();
    let conditional_instance_id = store
        .gardener_proposal_instances(&conditional_fingerprint)
        .unwrap()[0]
        .id
        .clone();
    SeededGardener {
        approved_fingerprint,
        approved_latest_instance_id: approved_instances[1].id.clone(),
        rejected_instance_id,
        conditional_instance_id,
    }
}

fn create_seeded_run(database: &Path, approved_fingerprint: &str) {
    let mut store = Store::open(database).unwrap();
    let obligation_id = store
        .gardener_proposal_instances(approved_fingerprint)
        .unwrap()
        .pop()
        .unwrap()
        .implementation_obligation_id;
    let claim = store
        .claim_due_gardener(3_000, 600, 10)
        .unwrap()
        .into_iter()
        .find(|claim| claim.obligation_id == obligation_id)
        .expect("approved implementation must become claimable");
    store
        .create_gardener_implementation_run(
            &claim,
            NewGardenerImplementationRun {
                id: "run-one".to_owned(),
                implementation_worktree_path: "/tmp/bokkie-run-one".to_owned(),
                branch: "codex/gardener-run-one".to_owned(),
            },
            3_001,
        )
        .unwrap();
}

fn cli_json(database: &Path, arguments: &[&str]) -> Value {
    let output = run_cli(database, arguments);
    assert_success(&output);
    serde_json::from_slice(&output.stdout).unwrap()
}

fn try_cli_json(database: &Path, arguments: &[&str]) -> Option<Value> {
    let output = run_cli(database, arguments);
    output
        .status
        .success()
        .then(|| serde_json::from_slice(&output.stdout).unwrap())
}

fn run_cli(database: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bokkie"))
        .arg("--database")
        .arg(database)
        .args(arguments)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn spawn_daemon(database: &Path, address: SocketAddr, extra: &[&str]) -> Daemon {
    Daemon(
        Command::new(env!("CARGO_BIN_EXE_bokkie"))
            .arg("--database")
            .arg(database)
            .arg("serve")
            .arg("--bind")
            .arg(address.to_string())
            .arg("--poll-ms")
            .arg("20")
            .args(extra)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    )
}

fn stop_child(child: &mut Daemon) {
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    child.wait().unwrap();
}

fn wait_for_child_exit(child: &mut Daemon, timeout: Duration) -> std::process::ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(
            started.elapsed() < timeout,
            "child process did not exit after {timeout:?}"
        );
        thread::sleep(POLL);
    }
}

struct Daemon(Child);

impl Deref for Daemon {
    type Target = Child;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Daemon {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if self.0.try_wait().is_ok_and(|status| status.is_none()) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn unused_loopback_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let started = Instant::now();
    while !condition() {
        assert!(
            started.elapsed() < timeout,
            "condition timed out after {timeout:?}"
        );
        thread::sleep(POLL);
    }
}

fn http_json(
    address: SocketAddr,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Option<(u16, Value)> {
    let mutation_token = (method == "POST")
        .then(|| {
            http_json(address, "GET", "/bootstrap", None)?.1["mutation_token"]
                .as_str()
                .map(str::to_owned)
        })
        .flatten();
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(100)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    let token_header = mutation_token
        .map(|token| format!("X-Bokkie-Mutation-Token: {token}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\n{token_header}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).ok()?;
    let response = String::from_utf8(response).ok()?;
    let (headers, body) = response.split_once("\r\n\r\n")?;
    let status = headers.split_whitespace().nth(1)?.parse().ok()?;
    let body = serde_json::from_str(body).ok()?;
    Some((status, body))
}

fn operator_action_body(
    address: SocketAddr,
    obligation_id: &str,
    capability: &str,
    actor: Option<&str>,
    note: Option<&str>,
) -> Value {
    let (_, snapshot) = http_json(address, "GET", "/operator/snapshot", None).unwrap();
    let obligation = snapshot["obligations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == obligation_id)
        .unwrap_or_else(|| panic!("operator snapshot lacks obligation {obligation_id:?}"));
    let precondition = obligation["capabilities"][capability]["precondition"].clone();
    assert!(
        !precondition.is_null(),
        "capability {capability:?} is unavailable"
    );
    json!({
        "precondition": precondition,
        "actor": actor.unwrap_or_default(),
        "note": note,
    })
}

fn http_raw(address: SocketAddr, method: &str, path: &str) -> Option<String> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(100)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let request =
        format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    Some(response)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
