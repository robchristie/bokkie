//! The gateway's control socket, through which an operator's command
//! reaches the serving gateway.
//!
//! `bokkie-gateway bot apply` and `bot run` run as a second process beside
//! `bokkie-gateway serve`, in the same container and on the same data
//! directory (`docker compose exec`). They cannot change the database
//! themselves: only the serving gateway numbers each user's events and
//! tells their open connections, holds each Bot's send lock, and starts
//! routine runs. So the serving gateway listens on a Unix socket in its
//! data directory, `control/gateway.sock`, and the command sends it one
//! request and prints its answer.
//!
//! The socket is the gateway's alone: its directory is mode 700 and the
//! socket mode 600, and the gateway answers only a peer running as the
//! user that owns the socket, which is the gateway's own. That user can
//! already read and write the whole data directory, database included, so
//! the socket gives it nothing more. It is never reachable over a network.
//!
//! One request per connection, one JSON line each way: a [`Request`], at
//! most [`MAX_REQUEST_BYTES`], then the end of the peer's writing, and a
//! [`Response`].

use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::AppState;
use crate::definitions::{self, Definition};

/// The largest request the gateway reads, a definition included.
pub const MAX_REQUEST_BYTES: u64 = 1024 * 1024;
/// How long a peer has to send its request.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The largest answer the command reads.
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

/// The longest path a Unix socket may have, in bytes: `sun_path` holds
/// 108 with the terminating zero.
pub const MAX_SOCKET_PATH_BYTES: usize = 107;

/// The control socket of the gateway serving `data_dir`.
pub fn socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join("control").join("gateway.sock")
}

/// Refuses a control socket path too long to bind or reach.
pub fn check_path(data_dir: &Path) -> Result<PathBuf, String> {
    let path = socket_path(data_dir);
    let length = path.as_os_str().len();
    if length > MAX_SOCKET_PATH_BYTES {
        return Err(format!(
            "the control socket {} is {length} bytes long, and a Unix socket's path is at most \
             {MAX_SOCKET_PATH_BYTES}; give the gateway a shorter data directory",
            path.display()
        ));
    }
    Ok(path)
}

/// What the command asks the gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
    /// Apply a Bot definition, given as its TOML text, to the account
    /// named `account`; with `dry_run`, check it and change nothing.
    Apply {
        account: String,
        definition: String,
        dry_run: bool,
    },
    /// Run the routine named `routine` of the Bot named `bot` now.
    Run {
        account: String,
        bot: String,
        routine: String,
    },
}

/// The gateway's answer: what it did, line by line, or why it refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub lines: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    fn refused(error: impl Into<String>) -> Self {
        Self {
            lines: Vec::new(),
            error: Some(error.into()),
        }
    }
}

/// A bound control socket, with the user that may use it.
#[derive(Debug)]
pub struct Listener {
    listener: UnixListener,
    owner: u32,
}

/// Binds the control socket of the gateway serving `data_dir`. A socket
/// left by a gateway that stopped is replaced; one that a running gateway
/// answers on is refused, since two gateways must never share a data
/// directory.
pub async fn listen(data_dir: &Path) -> Result<Listener, String> {
    let path = check_path(data_dir)?;
    let dir = path.parent().expect("the socket is in a directory");
    let failed = |doing: &str, e: std::io::Error| format!("{doing} {}: {e}", dir.display());
    std::fs::create_dir_all(dir).map_err(|e| failed("creating", e))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| failed("securing", e))?;
    match std::fs::symlink_metadata(&path) {
        Ok(found) if found.file_type().is_socket() => {
            if UnixStream::connect(&path).await.is_ok() {
                return Err(format!(
                    "another gateway is serving this data directory ({} answers)",
                    path.display()
                ));
            }
            std::fs::remove_file(&path)
                .map_err(|e| format!("removing the old {}: {e}", path.display()))?;
        }
        Ok(_) => {
            return Err(format!(
                "{} is in the way of the control socket",
                path.display()
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", path.display())),
    }
    let listener =
        UnixListener::bind(&path).map_err(|e| format!("binding {}: {e}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("securing {}: {e}", path.display()))?;
    let owner = std::fs::metadata(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .uid();
    Ok(Listener { listener, owner })
}

/// Answers the control socket's requests until the gateway stops, each
/// connection in a task of its own.
pub async fn serve(state: AppState, listener: Listener) {
    loop {
        let stream = match listener.listener.accept().await {
            Ok((stream, _)) => stream,
            Err(e) => {
                tracing::warn!(error = %e, "accepting on the control socket failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let state = state.clone();
        let owner = listener.owner;
        tokio::spawn(async move {
            if let Err(e) = connection(&state, stream, owner).await {
                tracing::warn!(error = %e, "a control connection failed");
            }
        });
    }
}

async fn connection(state: &AppState, mut stream: UnixStream, owner: u32) -> std::io::Result<()> {
    let peer = stream.peer_cred()?.uid();
    let response = if peer != owner {
        tracing::warn!(peer, "refused a control connection from another user");
        Response::refused("Only the gateway's own user may use its control socket")
    } else {
        match tokio::time::timeout(REQUEST_TIMEOUT, read_request(&mut stream)).await {
            Err(_) => Response::refused("No request arrived in time"),
            Ok(Err(error)) => Response::refused(error),
            Ok(Ok(request)) => answer(state, request).await,
        }
    };
    let mut line = serde_json::to_vec(&response).map_err(std::io::Error::other)?;
    line.push(b'\n');
    stream.write_all(&line).await?;
    stream.shutdown().await
}

async fn read_request(stream: &mut UnixStream) -> Result<Request, String> {
    let mut reader = BufReader::new(stream.take(MAX_REQUEST_BYTES + 1));
    let mut line = Vec::new();
    reader
        .read_until(b'\n', &mut line)
        .await
        .map_err(|e| format!("Reading the request failed: {e}"))?;
    if line.len() as u64 > MAX_REQUEST_BYTES {
        // Read the rest, so that closing the connection does not reset it
        // before the peer has the answer.
        let _ = tokio::io::copy(reader.into_inner().get_mut(), &mut tokio::io::sink()).await;
        return Err(format!(
            "The request is larger than {MAX_REQUEST_BYTES} bytes"
        ));
    }
    serde_json::from_slice(&line).map_err(|e| format!("The request is not valid: {e}"))
}

/// Carries out one request.
pub async fn answer(state: &AppState, request: Request) -> Response {
    let result = match request {
        Request::Apply {
            account,
            definition,
            dry_run,
        } => match Definition::parse(&definition) {
            Ok(definition) => definitions::apply(state, &account, &definition, dry_run).await,
            Err(error) => return Response::refused(error),
        },
        Request::Run {
            account,
            bot,
            routine,
        } => definitions::run(state, &account, &bot, &routine).await,
    };
    match result {
        Ok(report) => Response {
            lines: report.lines,
            error: None,
        },
        Err(failure) => {
            // An internal failure's message is generic; the cause is in
            // the gateway's log.
            Response::refused(failure.message)
        }
    }
}

/// Sends one request to the gateway serving `data_dir` and returns its
/// answer.
pub async fn send(data_dir: &Path, request: &Request) -> Result<Response, String> {
    let path = check_path(data_dir)?;
    let mut stream = UnixStream::connect(&path).await.map_err(|e| {
        format!(
            "cannot reach the gateway at {} ({e}); the command needs a running gateway on the \
             same data directory",
            path.display()
        )
    })?;
    let mut line = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    line.push(b'\n');
    stream
        .write_all(&line)
        .await
        .map_err(|e| format!("sending the request: {e}"))?;
    stream
        .shutdown()
        .await
        .map_err(|e| format!("sending the request: {e}"))?;
    let mut answer = Vec::new();
    BufReader::new(stream.take(MAX_RESPONSE_BYTES))
        .read_until(b'\n', &mut answer)
        .await
        .map_err(|e| format!("reading the answer: {e}"))?;
    serde_json::from_slice(&answer).map_err(|e| format!("the gateway's answer is not valid: {e}"))
}
