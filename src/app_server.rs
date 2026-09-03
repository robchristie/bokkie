//! Narrow client for the Codex app-server JSONL protocol.
//!
//! This module deliberately models only the calls used by the coding gardener.
//! Unknown notifications remain forward-compatible, while every server request
//! is rejected because the gardener never grants interactive approvals.

use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use thiserror::Error;

const INITIALIZE_REQUEST_ID: i64 = 1;
const THREAD_START_REQUEST_ID: i64 = 2;
const TURN_START_REQUEST_ID: i64 = 3;
const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const GRACEFUL_EXIT_WAIT: Duration = Duration::from_millis(500);

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
    #[error("app-server protocol failed: {0}")]
    Protocol(String),
    #[error("app-server observer failed during {callback}: {message}")]
    Observer {
        callback: &'static str,
        message: String,
    },
    #[error("app-server session failed: {reason}; process status: {status:?}; stderr: {stderr}")]
    Session {
        reason: String,
        status: Option<String>,
        stderr: String,
    },
}

/// Production client that starts one `app-server` child for each request.
#[derive(Clone, Debug)]
pub struct AppServerClient {
    executable: PathBuf,
    heartbeat_interval: Duration,
}

impl AppServerClient {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
        }
    }

    pub fn with_heartbeat_interval(mut self, interval: Duration) -> Self {
        self.heartbeat_interval = interval;
        self
    }

    /// Starts Codex app-server and runs exactly one fresh thread and turn.
    pub fn run(
        &self,
        request: &TurnRequest<'_>,
        observer: &mut dyn AppServerObserver,
    ) -> Result<TurnResult, AppServerError> {
        if self.heartbeat_interval.is_zero() {
            return Err(AppServerError::InvalidRequest(
                "heartbeat interval must be positive".to_owned(),
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

        let mut transport = ChildTransport::spawn(&self.executable, request.cwd)?;
        let result = run_protocol(
            &mut transport,
            request,
            cwd,
            observer,
            self.heartbeat_interval,
        );
        let diagnostics = transport.reap();

        match result {
            Ok(result) => {
                diagnostics.reap_error?;
                Ok(result)
            }
            Err(error) => Err(AppServerError::Session {
                reason: error.to_string(),
                status: diagnostics.status.map(|status| status.to_string()),
                stderr: diagnostics.stderr.trim_end().to_owned(),
            }),
        }
    }
}

trait LineTransport {
    fn send(&mut self, message: &Value) -> Result<(), AppServerError>;
    fn receive(&mut self, timeout: Duration) -> Result<Receive, AppServerError>;
}

#[derive(Debug)]
enum Receive {
    Line(String),
    Timeout,
    Eof,
}

#[derive(Debug)]
enum ReaderMessage {
    Line(String),
    Eof,
    Error(io::Error),
}

struct ChildTransport {
    child: Child,
    stdin: Option<BufWriter<ChildStdin>>,
    receiver: Receiver<ReaderMessage>,
    stdout_reader: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<io::Result<String>>>,
}

impl ChildTransport {
    fn spawn(executable: &Path, cwd: &Path) -> Result<Self, AppServerError> {
        let mut child = Command::new(executable)
            .arg("app-server")
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| AppServerError::Spawn {
                executable: executable.to_owned(),
                source,
            })?;

        let stdin = child.stdin.take().expect("piped child stdin is available");
        let stdout = child
            .stdout
            .take()
            .expect("piped child stdout is available");
        let stderr = child
            .stderr
            .take()
            .expect("piped child stderr is available");
        let (sender, receiver) = mpsc::channel();
        let stdout_reader = thread::spawn(move || read_stdout(stdout, sender));
        let stderr_reader = thread::spawn(move || read_stderr(stderr));

        Ok(Self {
            child,
            stdin: Some(BufWriter::new(stdin)),
            receiver,
            stdout_reader: Some(stdout_reader),
            stderr_reader: Some(stderr_reader),
        })
    }

    fn reap(mut self) -> ProcessDiagnostics {
        self.stdin.take();
        let deadline = Instant::now() + GRACEFUL_EXIT_WAIT;
        let mut reap_error = None;
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    if let Err(error) = self.child.kill()
                        && error.kind() != io::ErrorKind::InvalidInput
                    {
                        reap_error = Some(AppServerError::Io(error));
                    }
                    match self.child.wait() {
                        Ok(status) => break Some(status),
                        Err(error) => {
                            reap_error = Some(AppServerError::Io(error));
                            break None;
                        }
                    }
                }
                Err(error) => {
                    reap_error = Some(AppServerError::Io(error));
                    break None;
                }
            }
        };

        if let Some(handle) = self.stdout_reader.take() {
            let _ = handle.join();
        }
        let stderr = self
            .stderr_reader
            .take()
            .and_then(|handle| handle.join().ok())
            .and_then(Result::ok)
            .unwrap_or_default();

        ProcessDiagnostics {
            status,
            stderr,
            reap_error: match reap_error {
                Some(error) => Err(error),
                None => Ok(()),
            },
        }
    }
}

impl LineTransport for ChildTransport {
    fn send(&mut self, message: &Value) -> Result<(), AppServerError> {
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            AppServerError::Protocol("app-server stdin is already closed".to_owned())
        })?;
        serde_json::to_writer(&mut *stdin, message)
            .map_err(|error| AppServerError::Protocol(error.to_string()))?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }

    fn receive(&mut self, timeout: Duration) -> Result<Receive, AppServerError> {
        match self.receiver.recv_timeout(timeout) {
            Ok(ReaderMessage::Line(line)) => Ok(Receive::Line(line)),
            Ok(ReaderMessage::Eof) | Err(RecvTimeoutError::Disconnected) => Ok(Receive::Eof),
            Ok(ReaderMessage::Error(error)) => Err(AppServerError::Io(error)),
            Err(RecvTimeoutError::Timeout) => Ok(Receive::Timeout),
        }
    }
}

struct ProcessDiagnostics {
    status: Option<ExitStatus>,
    stderr: String,
    reap_error: Result<(), AppServerError>,
}

fn read_stdout(stdout: ChildStdout, sender: mpsc::Sender<ReaderMessage>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                let _ = sender.send(ReaderMessage::Eof);
                return;
            }
            Ok(_) => {
                if sender.send(ReaderMessage::Line(line)).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = sender.send(ReaderMessage::Error(error));
                return;
            }
        }
    }
}

fn read_stderr(mut stderr: ChildStderr) -> io::Result<String> {
    let mut output = String::new();
    stderr.read_to_string(&mut output)?;
    Ok(output)
}

fn run_protocol(
    transport: &mut dyn LineTransport,
    request: &TurnRequest<'_>,
    cwd: &str,
    observer: &mut dyn AppServerObserver,
    heartbeat_interval: Duration,
) -> Result<TurnResult, AppServerError> {
    let mut heartbeat = Heartbeat::new(observer, heartbeat_interval);
    transport.send(&json!({
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
    }))?;
    wait_for_response(transport, INITIALIZE_REQUEST_ID, &mut heartbeat)?;

    transport.send(&json!({ "method": "initialized" }))?;
    transport.send(&json!({
        "id": THREAD_START_REQUEST_ID,
        "method": "thread/start",
        "params": {
            "cwd": cwd,
            "sandbox": request.kind.thread_sandbox(),
            "approvalPolicy": "never",
        },
    }))?;
    let thread_response = wait_for_response(transport, THREAD_START_REQUEST_ID, &mut heartbeat)?;
    let thread_id = required_string(&thread_response, &["thread", "id"], "thread/start response")?;
    heartbeat
        .observer
        .record_thread(&thread_id)
        .map_err(|message| AppServerError::Observer {
            callback: "record_thread",
            message,
        })?;

    transport.send(&json!({
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
    }))?;
    let turn_response = wait_for_response(transport, TURN_START_REQUEST_ID, &mut heartbeat)?;
    let turn_id = required_string(&turn_response, &["turn", "id"], "turn/start response")?;
    heartbeat
        .observer
        .record_turn(&turn_id)
        .map_err(|message| AppServerError::Observer {
            callback: "record_turn",
            message,
        })?;

    let final_message = wait_for_completion(transport, &thread_id, &turn_id, &mut heartbeat)?;
    Ok(TurnResult {
        thread_id,
        turn_id,
        final_message,
    })
}

fn wait_for_response(
    transport: &mut dyn LineTransport,
    expected_id: i64,
    heartbeat: &mut Heartbeat<'_>,
) -> Result<Value, AppServerError> {
    loop {
        match heartbeat.receive(transport)? {
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
                    return deny_server_request(transport, id, &method);
                }
            },
            Receive::Timeout => unreachable!("heartbeat consumes transport timeouts"),
        }
    }
}

fn wait_for_completion(
    transport: &mut dyn LineTransport,
    thread_id: &str,
    turn_id: &str,
    heartbeat: &mut Heartbeat<'_>,
) -> Result<String, AppServerError> {
    let mut final_message = None;
    loop {
        match heartbeat.receive(transport)? {
            Receive::Eof => {
                return Err(AppServerError::Protocol(
                    "unexpected EOF while waiting for turn/completed".to_owned(),
                ));
            }
            Receive::Line(line) => match parse_message(&line)? {
                Message::ServerRequest { id, method } => {
                    return deny_server_request(transport, id, &method);
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
                    return final_message.ok_or_else(|| {
                        AppServerError::Protocol(format!(
                            "turn {turn_id} completed without a final agent message"
                        ))
                    });
                }
                Message::Notification { .. } => {}
            },
            Receive::Timeout => unreachable!("heartbeat consumes transport timeouts"),
        }
    }
}

struct Heartbeat<'a> {
    observer: &'a mut dyn AppServerObserver,
    interval: Duration,
    last: Instant,
}

impl<'a> Heartbeat<'a> {
    fn new(observer: &'a mut dyn AppServerObserver, interval: Duration) -> Self {
        Self {
            observer,
            interval,
            last: Instant::now(),
        }
    }

    fn receive(&mut self, transport: &mut dyn LineTransport) -> Result<Receive, AppServerError> {
        loop {
            let timeout = self.interval.saturating_sub(self.last.elapsed());
            if timeout.is_zero() {
                self.emit()?;
                continue;
            }
            match transport.receive(timeout)? {
                Receive::Timeout => self.emit()?,
                received => return Ok(received),
            }
        }
    }

    fn emit(&mut self) -> Result<(), AppServerError> {
        self.observer
            .heartbeat()
            .map_err(|message| AppServerError::Observer {
                callback: "heartbeat",
                message,
            })?;
        self.last = Instant::now();
        Ok(())
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
    transport.send(&json!({"id": id, "result": result}))?;
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
        fn send(&mut self, message: &Value) -> Result<(), AppServerError> {
            self.sent.push(message.clone());
            Ok(())
        }

        fn receive(&mut self, _timeout: Duration) -> Result<Receive, AppServerError> {
            Ok(self.received.pop_front().unwrap_or(Receive::Eof))
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
            Duration::from_secs(1),
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
    fn eof_aborts_instead_of_accepting_partial_output() {
        let mut messages = successful_exchange([]);
        messages.pop();
        let mut transport = FakeTransport::new(messages);

        let error = run_fake(&mut transport, TurnKind::Inspection, &mut NoopObserver).unwrap_err();

        assert!(error.to_string().contains("unexpected EOF"));
    }
}
