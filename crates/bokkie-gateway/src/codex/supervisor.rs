//! One Codex app server per user.
//!
//! Each user's app server runs with its own `CODEX_HOME`, home directory and
//! workspace under `<data>/users/<id>/`. The supervisor starts it on first
//! use, checks it before handing it out, and starts a new one if it exits.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bokkie_codex::protocol::{
    ApprovalDecision, ApprovalResponse, AskForApproval, ClientInfo, ConfigReadParams,
    DynamicToolCallResponse, DynamicToolSpec, GetAccountParams, GetAccountRateLimitsResponse,
    GetAccountResponse, InitializeCapabilities, InitializeParams, LoginAccountParams,
    LoginAccountResponse, ModelListParams, ServerNotification, ServerRequestKind,
    ThreadResumeParams, ThreadSessionResponse, ThreadStartParams, Turn, TurnInterruptParams,
    TurnStartParams, TurnSteerParams, UserInput,
};
use bokkie_codex::{AppServer, AppServerCommand, Client, Incoming, RpcError};
use bokkie_protocol::UserId;
use tokio::sync::mpsc;

use super::catalog::Catalog;
use super::config::{self, PROFILE};
use super::guard::{self, Expected, Refusal};

/// How long one app-server request may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// A failure to use a user's app server.
#[derive(Debug, thiserror::Error)]
pub enum CodexError {
    /// A safety check failed. The thread or app server is not served.
    #[error("refused: {0}")]
    Refused(#[from] Refusal),
    /// The app server failed or did not answer.
    #[error("app server: {0}")]
    AppServer(String),
}

impl From<bokkie_codex::Error> for CodexError {
    fn from(error: bokkie_codex::Error) -> Self {
        Self::AppServer(error.to_string())
    }
}

async fn timed<T>(
    call: impl std::future::Future<Output = Result<T, bokkie_codex::Error>>,
) -> Result<T, CodexError> {
    tokio::time::timeout(REQUEST_TIMEOUT, call)
        .await
        .map_err(|_| CodexError::AppServer("the app server did not answer in time".into()))?
        .map_err(Into::into)
}

/// The directories of one user.
#[derive(Debug, Clone)]
pub struct UserPaths {
    pub root: PathBuf,
    pub codex_home: PathBuf,
    pub home: PathBuf,
    pub workspace: PathBuf,
    pub log: PathBuf,
}

impl UserPaths {
    pub fn new(data_dir: &Path, user: UserId) -> Self {
        let root = data_dir.join("users").join(user.to_string());
        Self {
            codex_home: root.join("codex"),
            home: root.join("home"),
            workspace: root.join("workspace"),
            log: root.join("app-server.log"),
            root,
        }
    }

    /// Creates the directories, private to the gateway's user.
    fn create(&self) -> std::io::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        for dir in [&self.root, &self.codex_home, &self.home, &self.workspace] {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
}

fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let temporary = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(temporary, path)
}

/// Connects to an in-process app server for a user.
pub type Connect =
    Arc<dyn Fn(UserId, &UserPaths) -> (Client, mpsc::UnboundedReceiver<Incoming>) + Send + Sync>;

/// One user's runtime, behind a lock held while it starts.
type Slot = Arc<tokio::sync::Mutex<Option<Arc<Runtime>>>>;

/// How app servers are started.
#[derive(Clone)]
pub enum Launcher {
    /// Run the `codex` binary.
    Process {
        program: PathBuf,
        /// `-c key=value` overrides placed before `app-server`. Tests use
        /// them to point Codex at a mock model provider.
        overrides: Vec<String>,
    },
    /// Connect to something in-process, for tests.
    InProcess(Connect),
}

impl std::fmt::Debug for Launcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Process { program, overrides } => f
                .debug_struct("Process")
                .field("program", program)
                .field("overrides", overrides)
                .finish(),
            Self::InProcess(_) => f.write_str("InProcess"),
        }
    }
}

/// Something a user's app server reported, for the gateway to route.
#[derive(Debug, Clone)]
pub struct RuntimeEvent {
    pub user: UserId,
    pub kind: RuntimeEventKind,
}

#[derive(Debug, Clone)]
pub enum RuntimeEventKind {
    Notification(ServerNotification),
    /// A thread broke a safety rule mid-turn. The supervisor has already
    /// interrupted the turn and refuses the thread from now on.
    Refused {
        thread_id: String,
        reason: String,
    },
    /// The app server exited. Threads must be resumed on its successor.
    Exited,
}

/// Starts, checks and stops each user's app server.
#[derive(Debug)]
pub struct Supervisor {
    data_dir: PathBuf,
    catalog: Catalog,
    launcher: Launcher,
    events: mpsc::UnboundedSender<RuntimeEvent>,
    slots: std::sync::Mutex<HashMap<UserId, Slot>>,
}

impl Supervisor {
    /// A supervisor and the stream of everything its app servers report.
    pub fn new(
        data_dir: impl Into<PathBuf>,
        catalog: Catalog,
        launcher: Launcher,
    ) -> (Self, mpsc::UnboundedReceiver<RuntimeEvent>) {
        let (events, receiver) = mpsc::unbounded_channel();
        (
            Self {
                data_dir: data_dir.into(),
                catalog,
                launcher,
                events,
                slots: Default::default(),
            },
            receiver,
        )
    }

    pub fn paths(&self, user: UserId) -> UserPaths {
        UserPaths::new(&self.data_dir, user)
    }

    fn slot(&self, user: UserId) -> Slot {
        self.slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(user)
            .or_default()
            .clone()
    }

    /// The user's running app server, started and checked if needed.
    pub async fn runtime(&self, user: UserId) -> Result<Arc<Runtime>, CodexError> {
        let slot = self.slot(user);
        let mut slot = slot.lock().await;
        if let Some(runtime) = slot.as_ref().filter(|r| r.is_alive()) {
            return Ok(runtime.clone());
        }
        if let Some(old) = slot.take() {
            old.stop();
        }
        let runtime = self.start(user).await?;
        *slot = Some(runtime.clone());
        Ok(runtime)
    }

    /// The user's app server if it is running, without starting one.
    pub async fn running(&self, user: UserId) -> Option<Arc<Runtime>> {
        let slot = self.slot(user);
        let slot = slot.lock().await;
        slot.as_ref().filter(|r| r.is_alive()).cloned()
    }

    /// Stops the user's app server, if any.
    pub async fn stop(&self, user: UserId) {
        let slot = self.slot(user);
        if let Some(runtime) = slot.lock().await.take() {
            runtime.stop();
        }
    }

    async fn start(&self, user: UserId) -> Result<Arc<Runtime>, CodexError> {
        let paths = self.paths(user);
        paths
            .create()
            .map_err(|e| CodexError::AppServer(format!("creating the user's directories: {e}")))?;
        // The catalog must still be Bokkie's before any app server reads it.
        let catalog = super::catalog::load(&self.catalog.path)?;
        write_private(
            &paths.codex_home.join("config.toml"),
            &config::render(&self.data_dir, &catalog.path),
        )
        .map_err(|e| CodexError::AppServer(format!("writing config.toml: {e}")))?;
        let expected = Expected::new(&self.data_dir, &catalog, &paths.workspace);

        let (client, incoming, server) = match &self.launcher {
            Launcher::Process { program, overrides } => {
                let mut command = AppServerCommand::new(program, &paths.codex_home);
                command.args = overrides
                    .iter()
                    .flat_map(|o| [OsString::from("-c"), OsString::from(o)])
                    .collect();
                command.env.push(("HOME".into(), paths.home.clone().into()));
                command.stderr = Some(paths.log.clone());
                command.current_dir = Some(paths.workspace.clone());
                let (server, incoming) = AppServer::spawn(&command)?;
                (server.client.clone(), incoming, Some(server))
            }
            Launcher::InProcess(connect) => {
                let (client, incoming) = connect(user, &paths);
                (client, incoming, None)
            }
        };
        let runtime = Arc::new(Runtime {
            user,
            client,
            server: std::sync::Mutex::new(server),
            expected,
            alive: Arc::new(AtomicBool::new(true)),
            served: Default::default(),
            refused: Default::default(),
        });
        tokio::spawn(watch(runtime.clone(), incoming, self.events.clone()));
        if let Err(error) = runtime.startup_checks().await {
            tracing::error!(user = %user, %error, "refusing the user's app server");
            runtime.stop();
            return Err(error);
        }
        tracing::info!(user = %user, "app server ready");
        Ok(runtime)
    }
}

/// A running, checked app server for one user.
#[derive(Debug)]
pub struct Runtime {
    pub user: UserId,
    client: Client,
    /// The child process, killed when the runtime stops or drops.
    server: std::sync::Mutex<Option<AppServer>>,
    expected: Expected,
    alive: Arc<AtomicBool>,
    /// Threads that passed their checks on this app server.
    served: std::sync::Mutex<HashSet<String>>,
    /// Threads refused for breaking a safety rule, with the reason.
    refused: std::sync::Mutex<HashMap<String, String>>,
}

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Runtime {
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    fn stop(&self) {
        self.alive.store(false, Ordering::SeqCst);
        // Dropping the app server kills the child.
        lock(&self.server).take();
    }

    pub fn workspace(&self) -> &str {
        &self.expected.workspace
    }

    async fn startup_checks(&self) -> Result<(), CodexError> {
        timed(self.client.initialize(&InitializeParams {
            client_info: ClientInfo {
                name: "bokkie-gateway".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                title: Some("Bokkie".into()),
            },
            capabilities: Some(InitializeCapabilities {
                experimental_api: true,
                opt_out_notification_methods: None,
            }),
        }))
        .await?;
        self.check_config().await?;
        let models = timed(self.client.model_list(&ModelListParams::default())).await?;
        guard::check_models(&models, &self.expected)?;
        Ok(())
    }

    async fn check_config(&self) -> Result<(), CodexError> {
        let config = timed(self.client.config_read(&ConfigReadParams {
            cwd: Some(self.expected.workspace.clone()),
            include_layers: false,
        }))
        .await?;
        guard::check_config(&config.config, &self.expected)?;
        Ok(())
    }

    fn served(&self, response: ThreadSessionResponse) -> Result<ThreadSessionResponse, CodexError> {
        if let Err(refusal) = guard::check_thread(&response, &self.expected) {
            tracing::error!(user = %self.user, thread = %response.thread.id, %refusal, "refusing a thread");
            self.refuse(&response.thread.id, &refusal.0);
            return Err(refusal.into());
        }
        lock(&self.served).insert(response.thread.id.clone());
        Ok(response)
    }

    /// Starts a thread in the user's workspace under the `bokkie` profile,
    /// and checks it before serving it.
    pub async fn start_thread(
        &self,
        developer_instructions: String,
        dynamic_tools: Option<Vec<DynamicToolSpec>>,
    ) -> Result<ThreadSessionResponse, CodexError> {
        self.check_config().await?;
        let response = timed(self.client.thread_start(&ThreadStartParams {
            cwd: Some(self.expected.workspace.clone()),
            approval_policy: Some(AskForApproval::Never),
            permissions: Some(PROFILE.into()),
            developer_instructions: Some(developer_instructions),
            ephemeral: Some(false),
            dynamic_tools,
            ..ThreadStartParams::default()
        }))
        .await?;
        self.served(response)
    }

    /// Loads a thread into this app server, replacing its developer
    /// instructions, and checks it before serving it.
    pub async fn resume_thread(
        &self,
        thread_id: &str,
        developer_instructions: String,
    ) -> Result<ThreadSessionResponse, CodexError> {
        self.ensure_not_refused(thread_id)?;
        self.check_config().await?;
        let response = timed(self.client.thread_resume(&ThreadResumeParams {
            thread_id: thread_id.into(),
            cwd: Some(self.expected.workspace.clone()),
            approval_policy: Some(AskForApproval::Never),
            permissions: Some(PROFILE.into()),
            developer_instructions: Some(developer_instructions),
            ..ThreadResumeParams::default()
        }))
        .await?;
        if response.thread.id != thread_id {
            return Err(Refusal::new("thread/resume answered for a different thread").into());
        }
        self.served(response)
    }

    /// Whether the thread passed its checks on this app server.
    pub fn is_served(&self, thread_id: &str) -> bool {
        lock(&self.served).contains(thread_id) && !lock(&self.refused).contains_key(thread_id)
    }

    fn ensure_not_refused(&self, thread_id: &str) -> Result<(), CodexError> {
        match lock(&self.refused).get(thread_id) {
            Some(reason) => Err(Refusal::new(reason).into()),
            None => Ok(()),
        }
    }

    fn ensure_served(&self, thread_id: &str) -> Result<(), CodexError> {
        self.ensure_not_refused(thread_id)?;
        if !lock(&self.served).contains(thread_id) {
            return Err(
                Refusal::new("the thread has not passed its checks on this app server").into(),
            );
        }
        Ok(())
    }

    fn refuse(&self, thread_id: &str, reason: &str) {
        lock(&self.served).remove(thread_id);
        lock(&self.refused).insert(thread_id.into(), reason.into());
    }

    /// Starts a turn on a served thread.
    pub async fn start_turn(&self, thread_id: &str, text: &str) -> Result<Turn, CodexError> {
        self.ensure_served(thread_id)?;
        let mut params = TurnStartParams::text(thread_id, text);
        params.approval_policy = Some(AskForApproval::Never);
        params.permissions = Some(PROFILE.into());
        params.cwd = Some(self.expected.workspace.clone());
        Ok(timed(self.client.turn_start(&params)).await?.turn)
    }

    /// Adds a message to the running turn of a served thread.
    pub async fn steer_turn(
        &self,
        thread_id: &str,
        turn_id: &str,
        text: &str,
    ) -> Result<String, CodexError> {
        self.ensure_served(thread_id)?;
        Ok(timed(self.client.turn_steer(&TurnSteerParams {
            thread_id: thread_id.into(),
            expected_turn_id: turn_id.into(),
            input: vec![UserInput::Text { text: text.into() }],
        }))
        .await?
        .turn_id)
    }

    /// Interrupts a turn. Works on refused threads too.
    pub async fn interrupt_turn(&self, thread_id: &str, turn_id: &str) -> Result<(), CodexError> {
        timed(self.client.turn_interrupt(&TurnInterruptParams {
            thread_id: thread_id.into(),
            turn_id: turn_id.into(),
        }))
        .await?;
        Ok(())
    }

    pub async fn rate_limits(&self) -> Result<GetAccountRateLimitsResponse, CodexError> {
        timed(self.client.account_rate_limits_read()).await
    }

    pub async fn account(&self) -> Result<GetAccountResponse, CodexError> {
        timed(self.client.account_read(&GetAccountParams::default())).await
    }

    /// Starts a ChatGPT device-code sign-in for this user's app server.
    pub async fn login_start(&self) -> Result<LoginAccountResponse, CodexError> {
        timed(
            self.client
                .account_login_start(&LoginAccountParams::ChatgptDeviceCode),
        )
        .await
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Reads one app server's messages until it closes: checks every item,
/// answers every server request, and forwards notifications.
async fn watch(
    runtime: Arc<Runtime>,
    mut incoming: mpsc::UnboundedReceiver<Incoming>,
    events: mpsc::UnboundedSender<RuntimeEvent>,
) {
    let user = runtime.user;
    let send = |kind| {
        let _ = events.send(RuntimeEvent { user, kind });
    };
    // Hold only a weak reference while waiting, so a stopped runtime drops.
    let weak = Arc::downgrade(&runtime);
    let client = runtime.client.clone();
    let alive = runtime.alive.clone();
    drop(runtime);
    while let Some(message) = incoming.recv().await {
        let Some(runtime) = weak.upgrade() else {
            break;
        };
        match message {
            Incoming::Notification(notification) => {
                if let ServerNotification::ItemStarted(item)
                | ServerNotification::ItemCompleted(item) = &notification
                    && let Err(refusal) = guard::check_item(&item.item)
                {
                    violation(
                        &runtime,
                        &item.thread_id,
                        Some(&item.turn_id),
                        &refusal,
                        &send,
                    )
                    .await;
                }
                send(RuntimeEventKind::Notification(notification));
            }
            Incoming::Request(request) => {
                let answer = match request.kind {
                    ServerRequestKind::DynamicToolCall(call) => {
                        // Phase 2 Bots have no tools; Phase 3 adds them.
                        tracing::warn!(user = %user, tool = %call.tool, "unexpected tool call");
                        client
                            .respond(
                                request.id,
                                &DynamicToolCallResponse::text(
                                    false,
                                    "This tool is not available.",
                                ),
                            )
                            .await
                    }
                    ServerRequestKind::CommandExecutionApproval(params) => {
                        let refusal = Refusal::new("the app server asked to approve a command");
                        violation(
                            &runtime,
                            &params.thread_id,
                            Some(&params.turn_id),
                            &refusal,
                            &send,
                        )
                        .await;
                        client
                            .respond(
                                request.id,
                                &ApprovalResponse {
                                    decision: ApprovalDecision::Decline,
                                },
                            )
                            .await
                    }
                    ServerRequestKind::FileChangeApproval(params) => {
                        let refusal = Refusal::new("the app server asked to approve a file change");
                        violation(
                            &runtime,
                            &params.thread_id,
                            Some(&params.turn_id),
                            &refusal,
                            &send,
                        )
                        .await;
                        client
                            .respond(
                                request.id,
                                &ApprovalResponse {
                                    decision: ApprovalDecision::Decline,
                                },
                            )
                            .await
                    }
                    ServerRequestKind::Unknown { method, params } => {
                        if method.contains("pproval") {
                            // Any other approval also breaks the chat-only rule.
                            if let Some(thread_id) = params
                                .as_ref()
                                .and_then(|p| p.get("threadId").or_else(|| p.get("conversationId")))
                                .and_then(|t| t.as_str())
                            {
                                let refusal = Refusal::new(format!("the app server sent {method}"));
                                violation(&runtime, thread_id, None, &refusal, &send).await;
                            }
                        }
                        client
                            .respond_error(request.id, RpcError::method_not_found(&method))
                            .await
                    }
                };
                if let Err(error) = answer {
                    tracing::warn!(user = %user, %error, "could not answer a server request");
                }
            }
            Incoming::MalformedRequest { id, method, error } => {
                tracing::warn!(user = %user, %method, %error, "undecodable server request");
                let _ = client
                    .respond_error(id, RpcError::method_not_found(&method))
                    .await;
            }
            Incoming::Malformed { error, .. } => {
                tracing::warn!(user = %user, %error, "undecodable app-server message");
            }
        }
    }
    alive.store(false, Ordering::SeqCst);
    tracing::info!(user = %user, "app server closed");
    send(RuntimeEventKind::Exited);
}

async fn violation(
    runtime: &Runtime,
    thread_id: &str,
    turn_id: Option<&str>,
    refusal: &Refusal,
    send: &impl Fn(RuntimeEventKind),
) {
    tracing::error!(user = %runtime.user, thread = %thread_id, %refusal, "safety rule broken; refusing the thread");
    runtime.refuse(thread_id, &refusal.0);
    if let Some(turn_id) = turn_id {
        // Do not wait on the answer here: the watcher must keep reading.
        let client = runtime.client.clone();
        let (thread_id, turn_id) = (thread_id.to_owned(), turn_id.to_owned());
        tokio::spawn(async move {
            let _ = client
                .turn_interrupt(&TurnInterruptParams { thread_id, turn_id })
                .await;
        });
    }
    send(RuntimeEventKind::Refused {
        thread_id: thread_id.into(),
        reason: refusal.0.clone(),
    });
}
