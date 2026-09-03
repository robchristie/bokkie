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
    let (_, cancelled) = http_json(
        address,
        "POST",
        "/obligations/api-one/cancel",
        Some(json!({})),
    )
    .unwrap();
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
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(100)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
