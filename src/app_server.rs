//! Narrow client for the Codex app-server JSONL protocol.
//!
//! This module deliberately models only the calls used by the coding gardener.
//! Unknown notifications remain forward-compatible, while every server request
//! is rejected because the gardener never grants interactive approvals.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use thiserror::Error;

use crate::process::{
    CancellationToken, EffectRisk, JsonlReceive, ProcessError, ProcessHeartbeat, ProcessLimits,
    ProcessOutcome, ProcessSupervisor, SupervisedChild,
};
use crate::runtime_trust::{
    ChildEnvironment, ExecutableIdentity, ExecutableRole, ProcessPolicy, RuntimeTrustError,
};

const INITIALIZE_REQUEST_ID: i64 = 1;
const THREAD_START_REQUEST_ID: i64 = 2;
const TURN_START_REQUEST_ID: i64 = 3;
const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const DEFAULT_EXECUTION_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The purpose of a Codex turn and, consequently, the access it receives.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnKind {
    Inspection,
    Verification,
    Implementation,
}

impl TurnKind {
    fn thread_sandbox(self) -> &'static str {
        match self {
            Self::Inspection | Self::Verification => "read-only",
            Self::Implementation => "workspace-write",
        }
    }

    fn turn_sandbox(self, cwd: &str) -> Value {
        match self {
            Self::Inspection | Self::Verification => json!({
                "type": "readOnly",
                "networkAccess": false,
            }),
            Self::Implementation => json!({
                "type": "workspaceWrite",
                "networkAccess": false,
                "writableRoots": [cwd],
                "excludeSlashTmp": true,
                "excludeTmpdirEnvVar": true,
            }),
        }
    }
}

/// Inputs for one fresh app-server thread and its single turn.
#[derive(Debug)]
pub struct TurnRequest<'a> {
    pub kind: TurnKind,
    pub cwd: &'a Path,
    pub prompt: &'a str,
    pub output_schema: &'a Value,
}

/// Durable external identities and the final completed agent message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnResult {
    pub thread_id: String,
    pub turn_id: String,
    pub final_message: String,
}

/// Hooks through which the caller persists identities and renews its lease.
///
/// Identity callbacks run synchronously before the client starts the next
/// protocol phase. Returning an error stops the session, ensuring later
/// external work cannot outrun durable state.
pub trait AppServerObserver {
    fn record_thread(&mut self, _thread_id: &str) -> Result<(), String> {
        Ok(())
    }

    fn record_turn(&mut self, _turn_id: &str) -> Result<(), String> {
        Ok(())
    }

    fn heartbeat(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// Observer for callers that do not own a renewable lease.
#[derive(Debug, Default)]
pub struct NoopObserver;

impl AppServerObserver for NoopObserver {}

#[derive(Debug, Error)]
pub enum AppServerError {
    #[error("cannot start app-server executable {executable}: {source}")]
    Spawn {
        executable: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid app-server request: {0}")]
    InvalidRequest(String),
    #[error("app-server I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Process(#[from] ProcessError),
    #[error("app-server supervision ended the session: {outcome}")]
    Supervision { outcome: Box<ProcessOutcome> },
    #[error("app-server protocol failed: {0}")]
    Protocol(String),
    #[error("app-server observer failed during {callback}: {message}")]
    Observer {
        callback: &'static str,
        message: String,
    },
    #[error("app-server session failed: {reason}; process status: {status:?}; {evidence}")]
    Session {
        reason: String,
        status: Option<String>,
        evidence: Box<crate::process::ProcessEvidence>,
    },
    #[error("app-server runtime trust validation failed: {0}")]
    RuntimeTrust(#[from] RuntimeTrustError),
}

/// Production client that starts one `app-server` child for each request.
#[derive(Clone, Debug)]
pub struct AppServerClient {
    executable: PathBuf,
    executable_identity: Option<ExecutableIdentity>,
    environment: ChildEnvironment,
    heartbeat_interval: Duration,
    execution_timeout: Duration,
    limits: ProcessLimits,
    cancellation: CancellationToken,
}

impl AppServerClient {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            executable_identity: None,
            environment: ChildEnvironment::captured_current()
                .expect("the compatibility child environment is valid"),
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            execution_timeout: DEFAULT_EXECUTION_TIMEOUT,
            limits: ProcessLimits::default(),
            cancellation: CancellationToken::new(),
        }
    }

    /// Constructs a client from a startup-resolved Codex identity and an
    /// explicit worker environment. This is the production gardener path.
    pub fn from_trust(
        executable: ExecutableIdentity,
        environment: ChildEnvironment,
    ) -> Result<Self, AppServerError> {
        if executable.role() != ExecutableRole::Codex {
            return Err(AppServerError::InvalidRequest(
                "app-server executable identity must have the Codex role".to_owned(),
            ));
        }
        executable.verify_unchanged()?;
        Ok(Self {
            executable: executable.path().to_owned(),
            executable_identity: Some(executable),
            environment,
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            execution_timeout: DEFAULT_EXECUTION_TIMEOUT,
            limits: ProcessLimits::default(),
            cancellation: CancellationToken::new(),
        })
    }

    pub fn executable_identity(&self) -> Option<&ExecutableIdentity> {
        self.executable_identity.as_ref()
    }

    pub fn with_heartbeat_interval(mut self, interval: Duration) -> Self {
        self.heartbeat_interval = interval;
        self
    }

    pub fn with_execution_timeout(mut self, timeout: Duration) -> Self {
        self.execution_timeout = timeout;
        self
    }

    pub fn with_process_limits(mut self, limits: ProcessLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }

    /// Starts Codex app-server and runs exactly one fresh thread and turn.
    pub fn run(
        &self,
        request: &TurnRequest<'_>,
        observer: &mut dyn AppServerObserver,
    ) -> Result<TurnResult, AppServerError> {
        if let Some(identity) = &self.executable_identity {
            identity.verify_unchanged()?;
        }
        if self.heartbeat_interval.is_zero() || self.execution_timeout.is_zero() {
            return Err(AppServerError::InvalidRequest(
                "heartbeat interval and execution timeout must be positive".to_owned(),
            ));
        }
        let cwd = request.cwd.to_str().ok_or_else(|| {
            AppServerError::InvalidRequest("working directory must be valid UTF-8".to_owned())
        })?;
        if !request.cwd.is_absolute() {
            return Err(AppServerError::InvalidRequest(
                "working directory must be absolute".to_owned(),
            ));
        }

        let supervisor = ProcessSupervisor::new(
            self.heartbeat_interval,
            self.limits,
            self.cancellation.clone(),
        )
        .map_err(AppServerError::InvalidRequest)?;
        let deadline = Instant::now()
            .checked_add(self.execution_timeout)
            .ok_or_else(|| {
                AppServerError::InvalidRequest("execution deadline is out of range".to_owned())
            })?;
        let mut transport = ChildTransport::spawn(
            &supervisor,
            &self.executable,
            &self.environment,
            request.cwd,
            deadline,
        )?;
        let result = run_protocol(
            &mut transport,
            request,
            cwd,
            observer,
            self.limits.final_message_bytes,
        );
        match result {
            Ok(result) => {
                transport.close_stdin();
                let outcome = transport.wait(observer)?;
                match outcome {
                    ProcessOutcome::Completed { status, .. } if status.success() => Ok(result),
                    outcome => Err(AppServerError::Supervision {
                        outcome: Box::new(outcome),
                    }),
                }
            }
            Err(error @ AppServerError::Supervision { .. }) => Err(error),
            Err(error) => {
                let evidence = transport.abort()?;
                Err(AppServerError::Session {
                    reason: error.to_string(),
                    status: None,
                    evidence: Box::new(evidence),
                })
            }
        }
    }
}

trait LineTransport {
    fn send(
        &mut self,
        message: &Value,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<(), AppServerError>;
    fn receive(&mut self, heartbeat: &mut dyn ProcessHeartbeat) -> Result<Receive, AppServerError>;
}

#[derive(Debug)]
enum Receive {
    Line(String),
    #[cfg(test)]
    Timeout,
    Eof,
}

struct ChildTransport {
    child: SupervisedChild,
}

impl ChildTransport {
    fn spawn(
        supervisor: &ProcessSupervisor,
        executable: &Path,
        environment: &ChildEnvironment,
        cwd: &Path,
        deadline: Instant,
    ) -> Result<Self, AppServerError> {
        let mut command = Command::new(executable);
        command.arg("app-server").current_dir(cwd);
        environment.apply(&mut command, ProcessPolicy::Codex, None)?;
        let child = supervisor
            .spawn(&mut command, deadline, EffectRisk::None)
            .map_err(|source| AppServerError::Spawn {
                executable: executable.to_owned(),
                source: match source {
                    ProcessError::Spawn(source) | ProcessError::Io(source) => source,
                    ProcessError::IoWorkerPanicked => io::Error::other("I/O worker panicked"),
                },
            })?;
        Ok(Self { child })
    }

    fn close_stdin(&mut self) {
        self.child.close_stdin();
    }

    fn wait(
        &mut self,
        observer: &mut dyn AppServerObserver,
    ) -> Result<ProcessOutcome, AppServerError> {
        self.child
            .wait(&mut AppHeartbeat(observer))
            .map_err(Into::into)
    }

    fn abort(&mut self) -> Result<crate::process::ProcessEvidence, AppServerError> {
        self.child.abort().map_err(Into::into)
    }
}

impl LineTransport for ChildTransport {
    fn send(
        &mut self,
        message: &Value,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<(), AppServerError> {
        match self.child.write_json(message, heartbeat)? {
            None => Ok(()),
            Some(outcome) => Err(AppServerError::Supervision {
                outcome: Box::new(outcome),
            }),
        }
    }

    fn receive(&mut self, heartbeat: &mut dyn ProcessHeartbeat) -> Result<Receive, AppServerError> {
        match self.child.receive_jsonl(heartbeat)? {
            JsonlReceive::Line(line) => Ok(Receive::Line(line)),
            JsonlReceive::Eof => Ok(Receive::Eof),
            JsonlReceive::Terminal(outcome) => Err(AppServerError::Supervision {
                outcome: Box::new(outcome),
            }),
        }
    }
}

struct AppHeartbeat<'a>(&'a mut dyn AppServerObserver);

impl ProcessHeartbeat for AppHeartbeat<'_> {
    fn heartbeat(&mut self) -> Result<(), String> {
        self.0.heartbeat()
    }
}

fn run_protocol(
    transport: &mut dyn LineTransport,
    request: &TurnRequest<'_>,
    cwd: &str,
    observer: &mut dyn AppServerObserver,
    final_message_limit: usize,
) -> Result<TurnResult, AppServerError> {
    let mut heartbeat = AppHeartbeat(observer);
    transport.send(
        &json!({
            "id": INITIALIZE_REQUEST_ID,
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "bokkie",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": {
                    "experimentalApi": false,
                },
            },
        }),
        &mut heartbeat,
    )?;
    wait_for_response(transport, INITIALIZE_REQUEST_ID, &mut heartbeat)?;

    transport.send(&json!({ "method": "initialized" }), &mut heartbeat)?;
    transport.send(
        &json!({
            "id": THREAD_START_REQUEST_ID,
            "method": "thread/start",
            "params": {
                "cwd": cwd,
                "sandbox": request.kind.thread_sandbox(),
                "approvalPolicy": "never",
            },
        }),
        &mut heartbeat,
    )?;
    let thread_response = wait_for_response(transport, THREAD_START_REQUEST_ID, &mut heartbeat)?;
    let thread_id = required_string(&thread_response, &["thread", "id"], "thread/start response")?;
    heartbeat
        .0
        .record_thread(&thread_id)
        .map_err(|message| AppServerError::Observer {
            callback: "record_thread",
            message,
        })?;

    transport.send(
        &json!({
            "id": TURN_START_REQUEST_ID,
            "method": "turn/start",
            "params": {
                "threadId": thread_id,
                "input": [{
                    "type": "text",
                    "text": request.prompt,
                }],
                "outputSchema": request.output_schema,
                "approvalPolicy": "never",
                "sandboxPolicy": request.kind.turn_sandbox(cwd),
            },
        }),
        &mut heartbeat,
    )?;
    let turn_response = wait_for_response(transport, TURN_START_REQUEST_ID, &mut heartbeat)?;
    let turn_id = required_string(&turn_response, &["turn", "id"], "turn/start response")?;
    heartbeat
        .0
        .record_turn(&turn_id)
        .map_err(|message| AppServerError::Observer {
            callback: "record_turn",
            message,
        })?;

    let final_message = wait_for_completion(
        transport,
        &thread_id,
        &turn_id,
        &mut heartbeat,
        final_message_limit,
    )?;
    Ok(TurnResult {
        thread_id,
        turn_id,
        final_message,
    })
}

fn wait_for_response(
    transport: &mut dyn LineTransport,
    expected_id: i64,
    heartbeat: &mut dyn ProcessHeartbeat,
) -> Result<Value, AppServerError> {
    loop {
        match transport.receive(heartbeat)? {
            Receive::Eof => {
                return Err(AppServerError::Protocol(format!(
                    "unexpected EOF while waiting for response {expected_id}"
                )));
            }
            Receive::Line(line) => match parse_message(&line)? {
                Message::Response { id, result } => {
                    if id != json!(expected_id) {
                        return Err(AppServerError::Protocol(format!(
                            "response ID mismatch: expected {expected_id}, received {id}"
                        )));
                    }
                    return result;
                }
                Message::Notification { .. } => {}
                Message::ServerRequest { id, method } => {
                    return deny_server_request(transport, id, &method, heartbeat);
                }
            },
            #[cfg(test)]
            Receive::Timeout => {}
        }
    }
}

fn wait_for_completion(
    transport: &mut dyn LineTransport,
    thread_id: &str,
    turn_id: &str,
    heartbeat: &mut dyn ProcessHeartbeat,
    final_message_limit: usize,
) -> Result<String, AppServerError> {
    let mut final_message = None;
    loop {
        match transport.receive(heartbeat)? {
            Receive::Eof => {
                return Err(AppServerError::Protocol(
                    "unexpected EOF while waiting for turn/completed".to_owned(),
                ));
            }
            Receive::Line(line) => match parse_message(&line)? {
                Message::ServerRequest { id, method } => {
                    return deny_server_request(transport, id, &method, heartbeat);
                }
                Message::Response { id, .. } => {
                    return Err(AppServerError::Protocol(format!(
                        "unexpected response with ID {id} after turn/start"
                    )));
                }
                Message::Notification { method, params } if method == "item/completed" => {
                    validate_notification_ids(&params, thread_id, turn_id, "item/completed")?;
                    if params.pointer("/item/type").and_then(Value::as_str) == Some("agentMessage")
                    {
                        final_message = Some(required_string(
                            &params,
                            &["item", "text"],
                            "item/completed notification",
                        )?);
                    }
                }
                Message::Notification { method, params } if method == "turn/completed" => {
                    let received_thread =
                        required_string(&params, &["threadId"], "turn/completed notification")?;
                    let received_turn =
                        required_string(&params, &["turn", "id"], "turn/completed notification")?;
                    validate_id("thread", thread_id, &received_thread, "turn/completed")?;
                    validate_id("turn", turn_id, &received_turn, "turn/completed")?;
                    let status = required_string(
                        &params,
                        &["turn", "status"],
                        "turn/completed notification",
                    )?;
                    if status != "completed" {
                        let detail = params
                            .pointer("/turn/error/message")
                            .and_then(Value::as_str)
                            .map(|message| format!(": {message}"))
                            .unwrap_or_default();
                        return Err(AppServerError::Protocol(format!(
                            "turn {turn_id} finished with status {status}{detail}"
                        )));
                    }
                    let final_message = final_message.ok_or_else(|| {
                        AppServerError::Protocol(format!(
                            "turn {turn_id} completed without a final agent message"
                        ))
                    })?;
                    if final_message.len() > final_message_limit {
                        return Err(AppServerError::Protocol(format!(
                            "turn {turn_id} final message exceeds the configured bound"
                        )));
                    }
                    return Ok(final_message);
                }
                Message::Notification { .. } => {}
            },
            #[cfg(test)]
            Receive::Timeout => {}
        }
    }
}

fn validate_notification_ids(
    params: &Value,
    thread_id: &str,
    turn_id: &str,
    method: &str,
) -> Result<(), AppServerError> {
    let received_thread = required_string(params, &["threadId"], method)?;
    let received_turn = required_string(params, &["turnId"], method)?;
    validate_id("thread", thread_id, &received_thread, method)?;
    validate_id("turn", turn_id, &received_turn, method)
}

fn validate_id(
    identity: &str,
    expected: &str,
    received: &str,
    method: &str,
) -> Result<(), AppServerError> {
    if received == expected {
        Ok(())
    } else {
        Err(AppServerError::Protocol(format!(
            "{method} {identity} ID mismatch: expected {expected}, received {received}"
        )))
    }
}

fn required_string(value: &Value, path: &[&str], context: &str) -> Result<String, AppServerError> {
    let mut field = value;
    for component in path {
        field = field.get(*component).ok_or_else(|| {
            AppServerError::Protocol(format!(
                "{context} is missing string field {}",
                path.join(".")
            ))
        })?;
    }
    field
        .as_str()
        .filter(|field| !field.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            AppServerError::Protocol(format!(
                "{context} has invalid string field {}",
                path.join(".")
            ))
        })
}

fn unexpected_server_request(id: Value, method: &str) -> AppServerError {
    AppServerError::Protocol(format!(
        "unexpected server request {method} with ID {id}; interactive approvals are forbidden"
    ))
}

fn deny_server_request<T>(
    transport: &mut dyn LineTransport,
    id: Value,
    method: &str,
    heartbeat: &mut dyn ProcessHeartbeat,
) -> Result<T, AppServerError> {
    let result = match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            json!({"decision": "cancel"})
        }
        "item/permissions/requestApproval" => {
            json!({"permissions": {}, "scope": "turn"})
        }
        _ => return Err(unexpected_server_request(id, method)),
    };
    transport.send(&json!({"id": id, "result": result}), heartbeat)?;
    Err(unexpected_server_request(id, method))
}

enum Message {
    Response {
        id: Value,
        result: Result<Value, AppServerError>,
    },
    ServerRequest {
        id: Value,
        method: String,
    },
    Notification {
        method: String,
        params: Value,
    },
}

fn parse_message(line: &str) -> Result<Message, AppServerError> {
    let value: Value = serde_json::from_str(line).map_err(|error| {
        AppServerError::Protocol(format!("malformed JSON from app-server: {error}"))
    })?;
    let object = value.as_object().ok_or_else(|| {
        AppServerError::Protocol("app-server message must be a JSON object".to_owned())
    })?;
    if object.contains_key("method") && !object["method"].is_string() {
        return Err(AppServerError::Protocol(
            "app-server message has an invalid method".to_owned(),
        ));
    }
    let method = object.get("method").and_then(Value::as_str);

    if let Some(id) = object.get("id") {
        if !id.is_string() && !id.is_i64() && !id.is_u64() {
            return Err(AppServerError::Protocol(
                "app-server message has an invalid request ID".to_owned(),
            ));
        }
        if let Some(method) = method {
            return Ok(Message::ServerRequest {
                id: id.clone(),
                method: method.to_owned(),
            });
        }
        let result = match (object.get("result"), object.get("error")) {
            (Some(result), None) => Ok(result.clone()),
            (None, Some(error)) => Err(AppServerError::Protocol(format!(
                "app-server returned an error for request {id}: {error}"
            ))),
            _ => {
                return Err(AppServerError::Protocol(
                    "app-server response must contain exactly one of result or error".to_owned(),
                ));
            }
        };
        return Ok(Message::Response {
            id: id.clone(),
            result,
        });
    }

    if let Some(method) = method {
        return Ok(Message::Notification {
            method: method.to_owned(),
            params: object.get("params").cloned().unwrap_or(Value::Null),
        });
    }

    Err(AppServerError::Protocol(
        "app-server message is neither a response, request nor notification".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::{fs, os::unix::fs::PermissionsExt, thread};

    struct FakeTransport {
        received: VecDeque<Receive>,
        sent: Vec<Value>,
    }

    impl FakeTransport {
        fn new(received: impl IntoIterator<Item = Receive>) -> Self {
            Self {
                received: received.into_iter().collect(),
                sent: Vec::new(),
            }
        }
    }

    impl LineTransport for FakeTransport {
        fn send(
            &mut self,
            message: &Value,
            _heartbeat: &mut dyn ProcessHeartbeat,
        ) -> Result<(), AppServerError> {
            self.sent.push(message.clone());
            Ok(())
        }

        fn receive(
            &mut self,
            heartbeat: &mut dyn ProcessHeartbeat,
        ) -> Result<Receive, AppServerError> {
            let received = self.received.pop_front().unwrap_or(Receive::Eof);
            if matches!(received, Receive::Timeout) {
                heartbeat
                    .heartbeat()
                    .map_err(|message| AppServerError::Observer {
                        callback: "heartbeat",
                        message,
                    })?;
            }
            Ok(received)
        }
    }

    #[derive(Default)]
    struct RecordingObserver {
        callbacks: Vec<String>,
    }

    impl AppServerObserver for RecordingObserver {
        fn record_thread(&mut self, thread_id: &str) -> Result<(), String> {
            self.callbacks.push(format!("thread:{thread_id}"));
            Ok(())
        }

        fn record_turn(&mut self, turn_id: &str) -> Result<(), String> {
            self.callbacks.push(format!("turn:{turn_id}"));
            Ok(())
        }

        fn heartbeat(&mut self) -> Result<(), String> {
            self.callbacks.push("heartbeat".to_owned());
            Ok(())
        }
    }

    fn line(value: Value) -> Receive {
        Receive::Line(value.to_string())
    }

    fn successful_exchange(extra: impl IntoIterator<Item = Receive>) -> Vec<Receive> {
        let mut messages = vec![
            line(json!({"id": 1, "result": {}})),
            line(json!({"id": 2, "result": {"thread": {"id": "thread-1"}}})),
            line(json!({"id": 3, "result": {"turn": {"id": "turn-1"}}})),
        ];
        messages.extend(extra);
        messages.extend([
            line(json!({
                "method": "item/completed",
                "params": {
                    "threadId": "thread-1",
                    "turnId": "turn-1",
                    "completedAtMs": 1000,
                    "item": {"type": "agentMessage", "id": "item-1", "text": "done"},
                },
            })),
            line(json!({
                "method": "turn/completed",
                "params": {
                    "threadId": "thread-1",
                    "turn": {"id": "turn-1", "status": "completed", "items": []},
                },
            })),
        ]);
        messages
    }

    fn request(kind: TurnKind) -> TurnRequest<'static> {
        static SCHEMA: std::sync::LazyLock<Value> =
            std::sync::LazyLock::new(|| json!({"type": "object"}));
        TurnRequest {
            kind,
            cwd: Path::new("/worktree"),
            prompt: "Inspect the exact head.",
            output_schema: &SCHEMA,
        }
    }

    fn run_fake(
        transport: &mut FakeTransport,
        kind: TurnKind,
        observer: &mut dyn AppServerObserver,
    ) -> Result<TurnResult, AppServerError> {
        run_protocol(
            transport,
            &request(kind),
            "/worktree",
            observer,
            ProcessLimits::default().final_message_bytes,
        )
    }

    #[test]
    fn read_only_success_uses_stable_schema_and_orders_callbacks() {
        let mut transport = FakeTransport::new(successful_exchange([
            line(json!({"method": "future/notification", "params": {"anything": true}})),
            Receive::Timeout,
        ]));
        let mut observer = RecordingObserver::default();

        let result = run_fake(&mut transport, TurnKind::Inspection, &mut observer).unwrap();

        assert_eq!(
            result,
            TurnResult {
                thread_id: "thread-1".to_owned(),
                turn_id: "turn-1".to_owned(),
                final_message: "done".to_owned(),
            }
        );
        assert_eq!(
            observer.callbacks,
            ["thread:thread-1", "turn:turn-1", "heartbeat"]
        );
        assert_eq!(transport.sent[0]["method"], "initialize");
        assert_eq!(transport.sent[1], json!({"method": "initialized"}));
        assert_eq!(transport.sent[2]["method"], "thread/start");
        assert_eq!(transport.sent[2]["params"]["sandbox"], "read-only");
        assert_eq!(transport.sent[2]["params"]["approvalPolicy"], "never");
        assert_eq!(transport.sent[3]["method"], "turn/start");
        assert_eq!(
            transport.sent[3]["params"]["sandboxPolicy"],
            json!({"type": "readOnly", "networkAccess": false})
        );
        assert_eq!(transport.sent[3]["params"]["approvalPolicy"], "never");
        assert_eq!(
            transport.sent[3]["params"]["outputSchema"],
            json!({"type": "object"})
        );
        assert_eq!(
            transport.sent[3]["params"]["input"],
            json!([{"type": "text", "text": "Inspect the exact head."}])
        );
    }

    #[test]
    fn verification_is_also_read_only() {
        let mut transport = FakeTransport::new(successful_exchange([]));

        run_fake(&mut transport, TurnKind::Verification, &mut NoopObserver).unwrap();

        assert_eq!(transport.sent[2]["params"]["sandbox"], "read-only");
        assert_eq!(
            transport.sent[3]["params"]["sandboxPolicy"],
            json!({"type": "readOnly", "networkAccess": false})
        );
    }

    #[test]
    fn implementation_is_network_disabled_and_confined_to_worktree() {
        let mut transport = FakeTransport::new(successful_exchange([]));

        run_fake(&mut transport, TurnKind::Implementation, &mut NoopObserver).unwrap();

        assert_eq!(transport.sent[2]["params"]["sandbox"], "workspace-write");
        assert_eq!(
            transport.sent[3]["params"]["sandboxPolicy"],
            json!({
                "type": "workspaceWrite",
                "networkAccess": false,
                "writableRoots": ["/worktree"],
                "excludeSlashTmp": true,
                "excludeTmpdirEnvVar": true,
            })
        );
    }

    #[test]
    fn malformed_json_aborts() {
        let mut transport = FakeTransport::new([Receive::Line("not-json".to_owned())]);

        let error = run_fake(&mut transport, TurnKind::Inspection, &mut NoopObserver).unwrap_err();

        assert!(error.to_string().contains("malformed JSON"));
    }

    #[test]
    fn mismatched_response_id_aborts() {
        let mut transport = FakeTransport::new([line(json!({"id": 9, "result": {}}))]);

        let error = run_fake(&mut transport, TurnKind::Inspection, &mut NoopObserver).unwrap_err();

        assert!(error.to_string().contains("response ID mismatch"));
    }

    #[test]
    fn mismatched_notification_identity_aborts() {
        for (thread_id, turn_id, expected_error) in [
            ("another-thread", "turn-1", "thread ID mismatch"),
            ("thread-1", "another-turn", "turn ID mismatch"),
        ] {
            let mut messages = successful_exchange([]);
            messages[3] = line(json!({
                "method": "item/completed",
                "params": {
                    "threadId": thread_id,
                    "turnId": turn_id,
                    "item": {"type": "agentMessage", "id": "item-1", "text": "unsafe"},
                },
            }));
            let mut transport = FakeTransport::new(messages);

            let error =
                run_fake(&mut transport, TurnKind::Inspection, &mut NoopObserver).unwrap_err();

            assert!(error.to_string().contains(expected_error));
        }
    }

    #[test]
    fn unexpected_approval_request_is_denied_before_abort() {
        for (id, method, expected_result) in [
            (
                json!("command-approval"),
                "item/commandExecution/requestApproval",
                json!({"decision": "cancel"}),
            ),
            (
                json!(42),
                "item/fileChange/requestApproval",
                json!({"decision": "cancel"}),
            ),
            (
                json!("permission-approval"),
                "item/permissions/requestApproval",
                json!({"permissions": {}, "scope": "turn"}),
            ),
        ] {
            let mut messages = successful_exchange([]);
            messages[3] = line(json!({
                "id": id,
                "method": method,
                "params": {"threadId": "thread-1", "turnId": "turn-1"},
            }));
            let mut transport = FakeTransport::new(messages);

            let error =
                run_fake(&mut transport, TurnKind::Implementation, &mut NoopObserver).unwrap_err();

            assert!(
                error
                    .to_string()
                    .contains("interactive approvals are forbidden")
            );
            assert!(error.to_string().contains(method));
            assert_eq!(transport.sent.len(), 5);
            assert_eq!(
                transport.sent.last(),
                Some(&json!({"id": id, "result": expected_result}))
            );
            assert_eq!(
                transport.received.len(),
                1,
                "the protocol must stop after emitting the denial"
            );
        }
    }

    #[test]
    fn unknown_server_request_aborts_without_inventing_a_response() {
        let mut messages = successful_exchange([]);
        messages[3] = line(json!({
            "id": "unknown-request",
            "method": "future/requestApproval",
            "params": {},
        }));
        let mut transport = FakeTransport::new(messages);

        let error =
            run_fake(&mut transport, TurnKind::Implementation, &mut NoopObserver).unwrap_err();

        assert!(error.to_string().contains("future/requestApproval"));
        assert_eq!(transport.sent.len(), 4);
        assert_eq!(transport.received.len(), 1);
    }

    #[test]
    fn failed_and_interrupted_completions_report_status() {
        for status in ["failed", "interrupted"] {
            let mut messages = successful_exchange([]);
            messages.truncate(3);
            messages.push(line(json!({
                "method": "turn/completed",
                "params": {
                    "threadId": "thread-1",
                    "turn": {
                        "id": "turn-1",
                        "status": status,
                        "items": [],
                        "error": {"message": "model unavailable"},
                    },
                },
            })));
            let mut transport = FakeTransport::new(messages);

            let error =
                run_fake(&mut transport, TurnKind::Inspection, &mut NoopObserver).unwrap_err();

            assert!(
                error
                    .to_string()
                    .contains(&format!("status {status}: model unavailable"))
            );
        }
    }

    #[test]
    fn observer_error_aborts_before_turn_start() {
        struct FailingObserver;

        impl AppServerObserver for FailingObserver {
            fn record_thread(&mut self, _thread_id: &str) -> Result<(), String> {
                Err("stale lease".to_owned())
            }
        }

        let mut transport = FakeTransport::new(successful_exchange([]));

        let error = run_fake(
            &mut transport,
            TurnKind::Implementation,
            &mut FailingObserver,
        )
        .unwrap_err();

        assert!(error.to_string().contains("record_thread: stale lease"));
        assert_eq!(transport.sent.len(), 3);
    }

    #[test]
    fn completed_turn_requires_final_agent_message() {
        let mut messages = successful_exchange([]);
        messages.remove(3);
        let mut transport = FakeTransport::new(messages);

        let error = run_fake(&mut transport, TurnKind::Inspection, &mut NoopObserver).unwrap_err();

        assert!(error.to_string().contains("without a final agent message"));
    }

    #[test]
    fn completed_turn_rejects_an_oversized_final_message() {
        let mut transport = FakeTransport::new(successful_exchange([]));
        let error = run_protocol(
            &mut transport,
            &request(TurnKind::Inspection),
            "/worktree",
            &mut NoopObserver,
            3,
        )
        .unwrap_err();
        assert!(error.to_string().contains("final message exceeds"));
    }

    #[test]
    fn eof_aborts_instead_of_accepting_partial_output() {
        let mut messages = successful_exchange([]);
        messages.pop();
        let mut transport = FakeTransport::new(messages);

        let error = run_fake(&mut transport, TurnKind::Inspection, &mut NoopObserver).unwrap_err();

        assert!(error.to_string().contains("unexpected EOF"));
    }

    #[test]
    fn shutdown_cancellation_stops_a_running_app_server() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("never-app-server");
        fs::write(
            &executable,
            "#!/bin/sh\n[ \"$1\" = app-server ] || exit 2\nwhile :; do :; done\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();
        let cancellation = CancellationToken::new();
        let cancelling = cancellation.clone();
        let cancel_thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            cancelling.cancel();
        });

        let schema = json!({"type": "object"});
        let turn_request = TurnRequest {
            kind: TurnKind::Inspection,
            cwd: directory.path(),
            prompt: "never completes",
            output_schema: &schema,
        };
        let started = Instant::now();
        let error = AppServerClient::new(&executable)
            .with_heartbeat_interval(Duration::from_millis(5))
            .with_execution_timeout(Duration::from_secs(2))
            .with_cancellation(cancellation)
            .run(&turn_request, &mut NoopObserver)
            .unwrap_err();
        cancel_thread.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));

        let AppServerError::Supervision { outcome } = error else {
            panic!("expected supervised cancellation");
        };
        assert!(matches!(outcome.as_ref(), ProcessOutcome::Cancelled(_)));
    }

    #[test]
    fn heartbeat_store_failure_stops_a_running_app_server() {
        struct FencedStoreObserver;
        impl AppServerObserver for FencedStoreObserver {
            fn heartbeat(&mut self) -> Result<(), String> {
                Err("Store lease heartbeat was fenced".to_owned())
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("never-app-server");
        fs::write(
            &executable,
            "#!/bin/sh\n[ \"$1\" = app-server ] || exit 2\nwhile :; do :; done\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();
        let schema = json!({"type": "object"});
        let turn_request = TurnRequest {
            kind: TurnKind::Inspection,
            cwd: directory.path(),
            prompt: "never completes",
            output_schema: &schema,
        };

        let error = AppServerClient::new(&executable)
            .with_heartbeat_interval(Duration::from_millis(5))
            .with_execution_timeout(Duration::from_secs(2))
            .run(&turn_request, &mut FencedStoreObserver)
            .unwrap_err();

        let AppServerError::Supervision { outcome } = error else {
            panic!("expected supervised heartbeat failure");
        };
        assert!(matches!(
            outcome.as_ref(),
            ProcessOutcome::HeartbeatFailure { message, .. } if message.contains("fenced")
        ));
    }

    #[test]
    fn stopped_stdin_reader_cannot_block_heartbeats_or_deadline() {
        #[derive(Default)]
        struct CountingObserver {
            heartbeats: usize,
            thread_recorded: bool,
        }
        impl AppServerObserver for CountingObserver {
            fn record_thread(&mut self, _thread_id: &str) -> Result<(), String> {
                self.thread_recorded = true;
                self.heartbeats = 0;
                Ok(())
            }

            fn heartbeat(&mut self) -> Result<(), String> {
                self.heartbeats += 1;
                Ok(())
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let executable = stopped_reader_app_server(directory.path());
        let prompt = "x".repeat(512 * 1024);
        let schema = json!({"type": "object"});
        let request = TurnRequest {
            kind: TurnKind::Inspection,
            cwd: directory.path(),
            prompt: &prompt,
            output_schema: &schema,
        };
        let mut observer = CountingObserver::default();
        let started = Instant::now();

        let error = AppServerClient::new(&executable)
            .with_heartbeat_interval(Duration::from_millis(5))
            .with_execution_timeout(Duration::from_millis(150))
            .run(&request, &mut observer)
            .unwrap_err();

        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(observer.thread_recorded);
        assert!(observer.heartbeats > 0);
        let AppServerError::Supervision { outcome } = error else {
            panic!("expected supervised deadline");
        };
        assert!(matches!(outcome.as_ref(), ProcessOutcome::TimedOut(_)));
    }

    #[test]
    fn stopped_stdin_reader_cannot_block_shutdown_cancellation() {
        struct CancellingObserver {
            cancellation: CancellationToken,
            thread: Option<thread::JoinHandle<()>>,
        }
        impl AppServerObserver for CancellingObserver {
            fn record_thread(&mut self, _thread_id: &str) -> Result<(), String> {
                let cancellation = self.cancellation.clone();
                self.thread = Some(thread::spawn(move || {
                    thread::sleep(Duration::from_millis(20));
                    cancellation.cancel();
                }));
                Ok(())
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let executable = stopped_reader_app_server(directory.path());
        let prompt = "x".repeat(512 * 1024);
        let schema = json!({"type": "object"});
        let request = TurnRequest {
            kind: TurnKind::Inspection,
            cwd: directory.path(),
            prompt: &prompt,
            output_schema: &schema,
        };
        let cancellation = CancellationToken::new();
        let mut observer = CancellingObserver {
            cancellation: cancellation.clone(),
            thread: None,
        };
        let started = Instant::now();

        let error = AppServerClient::new(&executable)
            .with_heartbeat_interval(Duration::from_millis(5))
            .with_execution_timeout(Duration::from_secs(2))
            .with_cancellation(cancellation)
            .run(&request, &mut observer)
            .unwrap_err();
        observer
            .thread
            .take()
            .expect("thread identity starts cancellation")
            .join()
            .unwrap();

        assert!(started.elapsed() < Duration::from_secs(1));
        let AppServerError::Supervision { outcome } = error else {
            panic!("expected supervised cancellation");
        };
        assert!(matches!(outcome.as_ref(), ProcessOutcome::Cancelled(_)));
    }

    fn stopped_reader_app_server(directory: &Path) -> PathBuf {
        let executable = directory.join("stopped-reader-app-server.py");
        fs::write(
            &executable,
            r#"#!/usr/bin/env python3
import json
import sys
import time

initialize = json.loads(sys.stdin.readline())
print(json.dumps({"id": initialize["id"], "result": {}}), flush=True)
sys.stdin.readline()
thread_start = json.loads(sys.stdin.readline())
print(json.dumps({"id": thread_start["id"], "result": {"thread": {"id": "thread-stopped-reader"}}}), flush=True)
while True:
    time.sleep(1)
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();
        executable
    }
}
