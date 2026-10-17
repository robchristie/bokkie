//! The stdio transport, request correlation and the app-server process.

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};

use crate::protocol::{
    CancelLoginAccountParams, CancelLoginAccountResponse, CommandExecParams, CommandExecResponse,
    ConfigReadParams, ConfigReadResponse, GetAccountParams, GetAccountRateLimitsResponse,
    GetAccountResponse, InitializeParams, InitializeResponse, LoginAccountParams,
    LoginAccountResponse, ModelListParams, ModelListResponse, PermissionProfileListParams,
    PermissionProfileListResponse, ServerNotification, ServerRequest, ServerRequestKind,
    ThreadResumeParams, ThreadResumeResponse, ThreadStartParams, ThreadStartResponse,
    TurnInterruptParams, TurnInterruptResponse, TurnStartParams, TurnStartResponse,
};
use crate::rpc::{Error, Message, RequestId, RpcError};

/// A message from the server that is not a response to one of our requests.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Notification(ServerNotification),
    Request(ServerRequest),
    /// A server request whose parameters did not decode. The server still
    /// waits for an answer, so answer it, for example with an error.
    MalformedRequest {
        id: RequestId,
        method: String,
        error: String,
    },
    /// A line that could not be decoded. The connection stays open.
    Malformed {
        line: String,
        error: String,
    },
}

type Pending = HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>;
type SharedPending = Arc<std::sync::Mutex<Option<Pending>>>;

/// A connection to one app server. Cloning shares the connection.
///
/// Every method is cancellation-safe: a dropped request future never leaves
/// a partial line on the wire, because one writer task writes whole lines,
/// and it removes its pending entry.
#[derive(Clone)]
pub struct Client {
    lines: mpsc::UnboundedSender<String>,
    pending: SharedPending,
    next_id: Arc<AtomicI64>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").finish_non_exhaustive()
    }
}

/// Removes a pending entry when its request future ends for any reason.
struct PendingGuard<'a> {
    pending: &'a SharedPending,
    id: i64,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        if let Some(pending) = lock(self.pending).as_mut() {
            pending.remove(&self.id);
        }
    }
}

fn lock(pending: &SharedPending) -> std::sync::MutexGuard<'_, Option<Pending>> {
    pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Client {
    /// Starts a client over any byte stream pair. Incoming notifications and
    /// server requests arrive on the returned receiver, which closes when the
    /// server's output ends.
    pub fn new<R, W>(reader: R, writer: W) -> (Self, mpsc::UnboundedReceiver<Incoming>)
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let (lines_tx, lines_rx) = mpsc::unbounded_channel();
        let client = Self {
            lines: lines_tx,
            pending: Arc::new(std::sync::Mutex::new(Some(HashMap::new()))),
            next_id: Arc::new(AtomicI64::new(1)),
        };
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        tokio::spawn(read_loop(reader, client.pending.clone(), incoming_tx));
        tokio::spawn(write_loop(writer, lines_rx, client.pending.clone()));
        (client, incoming_rx)
    }

    /// Sends a request and waits for its result.
    pub async fn request<P, R>(&self, method: &str, params: Option<&P>) -> Result<R, Error>
    where
        P: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let value = self
            .request_value(method, params.map(serde_json::to_value).transpose()?)
            .await?;
        Ok(serde_json::from_value(value)?)
    }

    /// Sends a request with raw JSON parameters and returns the raw result.
    pub async fn request_value(&self, method: &str, params: Option<Value>) -> Result<Value, Error> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        lock(&self.pending)
            .as_mut()
            .ok_or(Error::Closed)?
            .insert(id, tx);
        let _guard = PendingGuard {
            pending: &self.pending,
            id,
        };
        self.send(&Message::Request {
            id: RequestId::Integer(id),
            method: method.to_owned(),
            params,
        })?;
        Ok(rx.await.map_err(|_| Error::Closed)??)
    }

    /// Sends a notification.
    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), Error> {
        self.send(&Message::Notification {
            method: method.to_owned(),
            params,
        })
    }

    /// Answers a server request.
    pub async fn respond<T: Serialize>(&self, id: RequestId, result: &T) -> Result<(), Error> {
        self.send(&Message::Response {
            id,
            result: serde_json::to_value(result)?,
        })
    }

    /// Answers a server request with an error.
    pub async fn respond_error(&self, id: RequestId, error: RpcError) -> Result<(), Error> {
        self.send(&Message::Error {
            id: Some(id),
            error,
        })
    }

    /// Queues one whole line for the writer task.
    fn send(&self, message: &Message) -> Result<(), Error> {
        let mut line = message.encode();
        line.push('\n');
        self.lines.send(line).map_err(|_| Error::Closed)
    }

    // -- Typed methods ------------------------------------------------------

    /// Performs the `initialize` handshake and sends `initialized`.
    pub async fn initialize(&self, params: &InitializeParams) -> Result<InitializeResponse, Error> {
        let response = self.request("initialize", Some(params)).await?;
        self.notify("initialized", None).await?;
        Ok(response)
    }

    pub async fn model_list(&self, params: &ModelListParams) -> Result<ModelListResponse, Error> {
        self.request("model/list", Some(params)).await
    }

    pub async fn account_read(
        &self,
        params: &GetAccountParams,
    ) -> Result<GetAccountResponse, Error> {
        self.request("account/read", Some(params)).await
    }

    pub async fn account_login_start(
        &self,
        params: &LoginAccountParams,
    ) -> Result<LoginAccountResponse, Error> {
        self.request("account/login/start", Some(params)).await
    }

    pub async fn account_login_cancel(
        &self,
        params: &CancelLoginAccountParams,
    ) -> Result<CancelLoginAccountResponse, Error> {
        self.request("account/login/cancel", Some(params)).await
    }

    pub async fn account_rate_limits_read(&self) -> Result<GetAccountRateLimitsResponse, Error> {
        self.request::<(), _>("account/rateLimits/read", None).await
    }

    pub async fn config_read(
        &self,
        params: &ConfigReadParams,
    ) -> Result<ConfigReadResponse, Error> {
        self.request("config/read", Some(params)).await
    }

    pub async fn permission_profile_list(
        &self,
        params: &PermissionProfileListParams,
    ) -> Result<PermissionProfileListResponse, Error> {
        self.request("permissionProfile/list", Some(params)).await
    }

    pub async fn thread_start(
        &self,
        params: &ThreadStartParams,
    ) -> Result<ThreadStartResponse, Error> {
        self.request("thread/start", Some(params)).await
    }

    pub async fn thread_resume(
        &self,
        params: &ThreadResumeParams,
    ) -> Result<ThreadResumeResponse, Error> {
        self.request("thread/resume", Some(params)).await
    }

    pub async fn turn_start(&self, params: &TurnStartParams) -> Result<TurnStartResponse, Error> {
        self.request("turn/start", Some(params)).await
    }

    pub async fn command_exec(
        &self,
        params: &CommandExecParams,
    ) -> Result<CommandExecResponse, Error> {
        self.request("command/exec", Some(params)).await
    }

    pub async fn turn_interrupt(
        &self,
        params: &TurnInterruptParams,
    ) -> Result<TurnInterruptResponse, Error> {
        self.request("turn/interrupt", Some(params)).await
    }
}

async fn write_loop<W>(
    mut writer: W,
    mut lines: mpsc::UnboundedReceiver<String>,
    pending: SharedPending,
) where
    W: AsyncWrite + Unpin,
{
    while let Some(line) = lines.recv().await {
        let written = async {
            writer.write_all(line.as_bytes()).await?;
            writer.flush().await
        };
        if written.await.is_err() {
            break;
        }
    }
    // The server can no longer hear us: fail every outstanding request.
    lock(&pending).take();
}

async fn read_loop<R>(reader: R, pending: SharedPending, incoming: mpsc::UnboundedSender<Incoming>)
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(reader);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = match std::str::from_utf8(&buffer) {
            Ok(line) => line.trim_end_matches(['\n', '\r']).to_owned(),
            Err(error) => {
                let _ = incoming.send(Incoming::Malformed {
                    line: String::from_utf8_lossy(&buffer).into_owned(),
                    error: error.to_string(),
                });
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(event) = dispatch(line, &pending) {
            // A dropped receiver only means nobody is listening.
            let _ = incoming.send(event);
        }
    }
    // Fail every outstanding and future request.
    lock(&pending).take();
}

fn dispatch(line: String, pending: &SharedPending) -> Option<Incoming> {
    match Message::decode(&line) {
        Ok(Message::Response { id, result }) => {
            complete(pending, &id, Ok(result));
            None
        }
        Ok(Message::Error {
            id: Some(id),
            error,
        }) => {
            complete(pending, &id, Err(error));
            None
        }
        Ok(Message::Error { id: None, error }) => Some(Incoming::Malformed {
            line,
            error: error.to_string(),
        }),
        Ok(Message::Notification { method, params }) => {
            Some(match ServerNotification::decode(method, params) {
                Ok(notification) => Incoming::Notification(notification),
                Err(error) => Incoming::Malformed {
                    line,
                    error: error.to_string(),
                },
            })
        }
        Ok(Message::Request { id, method, params }) => {
            Some(match ServerRequestKind::decode(method.clone(), params) {
                Ok(kind) => Incoming::Request(ServerRequest { id, kind }),
                Err(error) => Incoming::MalformedRequest {
                    id,
                    method,
                    error: error.to_string(),
                },
            })
        }
        Err(error) => {
            // An undecodable response still answers its request: fail it
            // rather than leave it waiting until the connection closes.
            let value: Option<Value> = serde_json::from_str(&line).ok();
            let id = value
                .as_ref()
                .filter(|value| value.get("method").is_none())
                .and_then(|value| {
                    serde_json::from_value::<RequestId>(value.get("id")?.clone()).ok()
                });
            if let Some(id) = id {
                complete(
                    pending,
                    &id,
                    Err(RpcError {
                        code: RpcError::PARSE_ERROR,
                        message: format!("undecodable response: {error}"),
                        data: None,
                    }),
                );
            }
            Some(Incoming::Malformed {
                line,
                error: error.to_string(),
            })
        }
    }
}

fn complete(pending: &SharedPending, id: &RequestId, result: Result<Value, RpcError>) {
    let RequestId::Integer(id) = id else {
        return;
    };
    let sender = lock(pending)
        .as_mut()
        .and_then(|pending| pending.remove(id));
    if let Some(sender) = sender {
        let _ = sender.send(result);
    }
}

/// How to start `codex app-server`.
#[derive(Debug, Clone)]
pub struct AppServerCommand {
    /// The `codex` executable.
    pub program: PathBuf,
    /// The `CODEX_HOME` for this app server.
    pub codex_home: PathBuf,
    /// Extra `codex` arguments placed before `app-server`, such as `-c`
    /// overrides.
    pub args: Vec<OsString>,
    /// Environment variables for the child. The child does not inherit the
    /// parent's environment except `PATH`, `HOME`, `USER`, `LANG` and `TZ`.
    pub env: Vec<(OsString, OsString)>,
    /// Where the child's standard error goes.
    pub stderr: Option<PathBuf>,
    pub current_dir: Option<PathBuf>,
}

impl AppServerCommand {
    pub fn new(program: impl Into<PathBuf>, codex_home: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            codex_home: codex_home.into(),
            args: Vec::new(),
            env: Vec::new(),
            stderr: None,
            current_dir: None,
        }
    }
}

/// A running `codex app-server` child process and its client.
#[derive(Debug)]
pub struct AppServer {
    pub child: Child,
    pub client: Client,
}

impl AppServer {
    /// Spawns the app server. It is killed when the returned value is dropped.
    pub fn spawn(
        command: &AppServerCommand,
    ) -> Result<(Self, mpsc::UnboundedReceiver<Incoming>), Error> {
        let mut process = Command::new(&command.program);
        process
            .args(&command.args)
            .arg("app-server")
            .env_clear()
            .env("CODEX_HOME", &command.codex_home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true);
        for name in ["PATH", "HOME", "USER", "LANG", "TZ"] {
            if let Some(value) = std::env::var_os(name) {
                process.env(name, value);
            }
        }
        process.envs(command.env.iter().map(|(k, v)| (k, v)));
        if let Some(dir) = &command.current_dir {
            process.current_dir(dir);
        }
        match &command.stderr {
            Some(path) => {
                // Codex diagnostics can be sensitive: keep them private.
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .mode(0o600)
                    .open(path)?;
                process.stderr(file);
            }
            None => {
                process.stderr(Stdio::inherit());
            }
        }
        let mut child = process.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let (client, incoming) = Client::new(stdout, stdin);
        Ok((Self { child, client }, incoming))
    }

    /// The operating-system process ID, while the child runs.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }
}
