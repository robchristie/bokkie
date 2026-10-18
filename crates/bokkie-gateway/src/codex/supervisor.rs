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
    /// This thread failed its own checks. Unlike [`CodexError::Refused`],
    /// which may concern the app server or the whole gateway, this is about
    /// the thread alone, so its Bot may record it.
    #[error("thread refused: {0}")]
    ThreadRefused(Refusal),
    /// The app server failed or did not answer, so the request may or may
    /// not have taken effect.
    #[error("app server: {0}")]
    AppServer(String),
    /// The app server answered the request with an error: it had no effect.
    #[error("app server rejected the request: {0}")]
    Rejected(String),
}

impl CodexError {
    /// Whether the request certainly had no effect on Codex.
    pub fn had_no_effect(&self) -> bool {
        // A refusal is decided before the request is sent.
        !matches!(self, Self::AppServer(_))
    }
}

impl From<bokkie_codex::Error> for CodexError {
    fn from(error: bokkie_codex::Error) -> Self {
        match error {
            bokkie_codex::Error::Rpc(error) => Self::Rejected(error.to_string()),
            error => Self::AppServer(error.to_string()),
        }
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

/// Context fragments for a turn, by key.
pub type Context =
    std::collections::BTreeMap<String, bokkie_codex::protocol::AdditionalContextEntry>;

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
        /// Extra environment for the child, such as a test's `PATH`.
        env: Vec<(OsString, OsString)>,
    },
    /// Connect to something in-process, for tests.
    InProcess(Connect),
}

impl std::fmt::Debug for Launcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Process {
                program,
                overrides,
                env,
            } => f
                .debug_struct("Process")
                .field("program", program)
                .field("overrides", overrides)
                .field("env", env)
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

/// Refusals that outlive any one app server.
///
/// A thread that fails its checks stays refused on every later app server.
/// A safety rule broken mid-turn shows that the chat-only configuration did
/// not hold, so it taints Codex for the whole gateway: every app server is
/// stopped and none is served again until the gateway restarts, which is
/// the operator's decision. Neither survives a gateway restart.
#[derive(Debug)]
pub struct Latch {
    threads: std::sync::Mutex<HashMap<(UserId, String), String>>,
    /// Set once, with the first reason. Every app server's watcher stops its
    /// app server when it changes.
    tainted: tokio::sync::watch::Sender<Option<String>>,
}

impl Default for Latch {
    fn default() -> Self {
        Self {
            threads: Default::default(),
            tainted: tokio::sync::watch::channel(None).0,
        }
    }
}

impl Latch {
    fn refuse_thread(&self, user: UserId, thread_id: &str, reason: &str) {
        lock(&self.threads).insert((user, thread_id.into()), reason.into());
    }

    fn thread_refusal(&self, user: UserId, thread_id: &str) -> Option<String> {
        lock(&self.threads)
            .get(&(user, thread_id.to_owned()))
            .cloned()
    }

    fn taint(&self, reason: &str) {
        self.tainted.send_if_modified(|tainted| {
            if tainted.is_some() {
                return false;
            }
            *tainted = Some(reason.to_owned());
            true
        });
    }

    /// Why Codex is refused for the whole gateway, if it is.
    pub fn tainted(&self) -> Option<String> {
        self.tainted.borrow().clone()
    }
}

/// Starts, checks and stops each user's app server.
#[derive(Debug)]
pub struct Supervisor {
    data_dir: PathBuf,
    catalog: Catalog,
    launcher: Launcher,
    events: mpsc::UnboundedSender<RuntimeEvent>,
    slots: std::sync::Mutex<HashMap<UserId, Slot>>,
    latch: Arc<Latch>,
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
                latch: Arc::default(),
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

    /// Why Codex is refused for the whole gateway, if it is.
    pub fn tainted(&self) -> Option<String> {
        self.latch.tainted()
    }

    /// The user's running app server, started and checked if needed.
    pub async fn runtime(&self, user: UserId) -> Result<Arc<Runtime>, CodexError> {
        if let Some(reason) = self.latch.tainted() {
            return Err(Refusal::new(format!(
                "Codex is refused until the gateway restarts: {reason}"
            ))
            .into());
        }
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
            Launcher::Process {
                program,
                overrides,
                env,
            } => {
                let mut command = AppServerCommand::new(program, &paths.codex_home);
                command.env.extend(env.iter().cloned());
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
            latch: self.latch.clone(),
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
    latch: Arc<Latch>,
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
            return Err(CodexError::ThreadRefused(refusal));
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
        if let Some(reason) = self.latch.tainted() {
            return Err(Refusal::new(reason).into());
        }
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
        self.is_alive()
            && lock(&self.served).contains(thread_id)
            && self.latch.thread_refusal(self.user, thread_id).is_none()
    }

    fn ensure_not_refused(&self, thread_id: &str) -> Result<(), CodexError> {
        if let Some(reason) = self.latch.tainted() {
            return Err(Refusal::new(reason).into());
        }
        match self.latch.thread_refusal(self.user, thread_id) {
            Some(reason) => Err(CodexError::ThreadRefused(Refusal::new(reason))),
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
        self.latch.refuse_thread(self.user, thread_id, reason);
    }

    /// Starts a turn on a served thread, with optional context fragments.
    pub async fn start_turn(
        &self,
        thread_id: &str,
        text: &str,
        context: Option<Context>,
    ) -> Result<Turn, CodexError> {
        self.ensure_served(thread_id)?;
        let mut params = TurnStartParams::text(thread_id, text);
        params.additional_context = context;
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
        context: Option<Context>,
    ) -> Result<String, CodexError> {
        self.ensure_served(thread_id)?;
        Ok(timed(self.client.turn_steer(&TurnSteerParams {
            thread_id: thread_id.into(),
            expected_turn_id: turn_id.into(),
            input: vec![UserInput::Text { text: text.into() }],
            additional_context: context,
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
    let mut tainted = runtime.latch.tainted.subscribe();
    drop(runtime);
    loop {
        let message = tokio::select! {
            message = incoming.recv() => message,
            _ = tainted.wait_for(Option::is_some) => {
                // Codex is refused gateway-wide: stop this app server too.
                if let Some(runtime) = weak.upgrade() {
                    runtime.stop();
                }
                break;
            }
        };
        let Some(message) = message else {
            break;
        };
        let Some(runtime) = weak.upgrade() else {
            break;
        };
        match message {
            Incoming::Notification(notification) => {
                if let ServerNotification::ItemStarted(item)
                | ServerNotification::ItemCompleted(item) = &notification
                    && let Err(refusal) = guard::check_item(&item.item)
                {
                    violation(&runtime, Some(&item.thread_id), &refusal, &send).await;
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
                        violation(&runtime, Some(&params.thread_id), &refusal, &send).await;
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
                        violation(&runtime, Some(&params.thread_id), &refusal, &send).await;
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
                                violation(&runtime, Some(thread_id), &refusal, &send).await;
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
                // Fail closed: an approval that cannot be read is still one.
                if method.contains("pproval") {
                    let refusal =
                        Refusal::new(format!("the app server sent an unreadable {method}"));
                    violation(&runtime, None, &refusal, &send).await;
                }
            }
            Incoming::Malformed { line, error } => {
                tracing::warn!(user = %user, %error, "undecodable app-server message");
                // Fail closed: an item that cannot be checked is refused.
                let value: Option<serde_json::Value> = serde_json::from_str(&line).ok();
                let method = value.as_ref().and_then(|v| v["method"].as_str());
                if matches!(method, Some("item/started" | "item/completed")) {
                    let thread = value
                        .as_ref()
                        .and_then(|v| v["params"]["threadId"].as_str())
                        .map(str::to_owned);
                    let refusal =
                        Refusal::new("the app server sent an item that could not be checked");
                    violation(&runtime, thread.as_deref(), &refusal, &send).await;
                }
            }
        }
    }
    alive.store(false, Ordering::SeqCst);
    // Whatever closed the stream, a child left running would go unwatched:
    // kill it.
    if let Some(runtime) = weak.upgrade() {
        runtime.stop();
    }
    tracing::info!(user = %user, "app server closed");
    send(RuntimeEventKind::Exited);
}

/// Handles a broken safety rule: refuses the thread, taints Codex for the
/// whole gateway, which stops every app server, and reports it.
async fn violation(
    runtime: &Runtime,
    thread_id: Option<&str>,
    refusal: &Refusal,
    send: &impl Fn(RuntimeEventKind),
) {
    tracing::error!(
        user = %runtime.user,
        thread = thread_id.unwrap_or("-"),
        %refusal,
        "safety rule broken; stopping Codex for the whole gateway until it restarts"
    );
    if let Some(thread_id) = thread_id {
        runtime.refuse(thread_id, &refusal.0);
        send(RuntimeEventKind::Refused {
            thread_id: thread_id.into(),
            reason: refusal.0.clone(),
        });
    }
    runtime.latch.taint(&refusal.0);
    runtime.stop();
}
