//! One Codex app server per user, and the household's.
//!
//! Each user's app server runs with its own `CODEX_HOME`, home directory and
//! workspace under `<data>/users/<id>/`. The supervisor starts it on first
//! use, checks it before handing it out, and starts a new one if it exits.
//!
//! Every app server uses one ChatGPT sign-in, the household's (see
//! [`super::household`]). The household app server, under
//! `<data>/household/`, holds the only real `auth.json` and runs no threads;
//! it alone signs in, refreshes and signs out. Each user's `auth.json` is a
//! symbolic link to it. Before a user's app server starts, and before each
//! use of one, the household app server must be running and its access
//! token must lie outside the refresh window, refreshed first if it does
//! not. So no member app server starts, and no turn runs, while the
//! household is signed out.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use bokkie_codex::protocol::{
    AccountLoginCompletedNotification, ApprovalDecision, ApprovalResponse, AskForApproval,
    CancelLoginAccountParams, ClientInfo, ConfigReadParams, DynamicToolCallParams,
    DynamicToolCallResponse, GetAccountParams, GetAccountRateLimitsResponse, GetAccountResponse,
    InitializeCapabilities, InitializeParams, LoginAccountParams, LoginAccountResponse,
    ModelListParams, ServerNotification, ServerRequestKind, ThreadResumeParams,
    ThreadSessionResponse, ThreadStartParams, Turn, TurnInterruptParams, TurnStartParams,
    TurnStatus, TurnSteerParams, UserInput,
};
use bokkie_codex::{AppServer, AppServerCommand, Client, Incoming, RequestId, RpcError};
use bokkie_protocol::api::LoginState;
use bokkie_protocol::{Timestamp, UserId};
use tokio::sync::mpsc;

use super::catalog::Catalog;
use super::config::{self, PROFILE};
use super::guard::{self, Expected, Refusal};
use super::household::{self, AuthFile, HouseholdSettings, Login};
use crate::grants::GrantSet;
use crate::tools;

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
    /// The household's ChatGPT sign-in is signed out, or could not be
    /// refreshed, so no member app server starts and no turn runs.
    #[error("the household is signed out: {0}")]
    SignedOut(String),
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
        // A refusal or a signed-out household is decided before the request
        // is sent.
        !matches!(self, Self::AppServer(_))
    }
}

impl From<bokkie_codex::Error> for CodexError {
    fn from(error: bokkie_codex::Error) -> Self {
        match error {
            // The client reports an answer it could not decode as a parse
            // error of its own; the request may have taken effect.
            bokkie_codex::Error::Rpc(error)
                if error.code != bokkie_codex::RpcError::PARSE_ERROR =>
            {
                Self::Rejected(error.to_string())
            }
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

/// Whose app server: the household's, or a user's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Owner {
    /// The household app server, which holds the shared sign-in and runs no
    /// threads.
    Household,
    User(UserId),
}

impl std::fmt::Display for Owner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Household => f.write_str("household"),
            Self::User(user) => write!(f, "user {user}"),
        }
    }
}

/// The directories of one app server.
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
        Self::under(data_dir.join("users").join(user.to_string()))
    }

    /// The household app server's directories, `<data>/household/`.
    pub fn household(data_dir: &Path) -> Self {
        Self::under(data_dir.join("household"))
    }

    pub fn of(data_dir: &Path, owner: Owner) -> Self {
        match owner {
            Owner::Household => Self::household(data_dir),
            Owner::User(user) => Self::new(data_dir, user),
        }
    }

    fn under(root: PathBuf) -> Self {
        Self {
            codex_home: root.join("codex"),
            home: root.join("home"),
            workspace: root.join("workspace"),
            log: root.join("app-server.log"),
            root,
        }
    }

    /// This app server's `auth.json`: the household's file, or a user's
    /// link to it.
    pub fn auth_json(&self) -> PathBuf {
        self.codex_home.join("auth.json")
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

/// Connects to an in-process app server, for tests.
pub type Connect =
    Arc<dyn Fn(Owner, &UserPaths) -> (Client, mpsc::UnboundedReceiver<Incoming>) + Send + Sync>;

/// Context fragments for a turn, by key.
pub type Context =
    std::collections::BTreeMap<String, bokkie_codex::protocol::AdditionalContextEntry>;

/// One app server, behind a lock held while it starts.
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
        /// Extra environment for the child, such as a test's `PATH`. The
        /// gateway's own environment never reaches the child, so only a
        /// test can set these.
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
    /// A web search completed. Sent just before the `item/completed`
    /// notification that reports it, with what this app server's read loop
    /// found when it checked the item: whether the thread was granted
    /// `web.search`. A search on any other thread was a violation, already
    /// acted on.
    WebSearchCompleted {
        thread_id: String,
        turn_id: String,
        item_id: String,
        query: String,
        action: Option<serde_json::Value>,
        granted: bool,
    },
    /// The app server exited. Threads must be resumed on its successor.
    Exited,
    /// The model called one of Bokkie's tools on a thread that was granted
    /// it. The broker serves it and answers through `reply`.
    ToolCall {
        call: DynamicToolCallParams,
        reply: ToolReply,
    },
}

/// The answer to one `item/tool/call`, given at most once. A reply dropped
/// unanswered answers the call with an error, because Codex sets no
/// deadline of its own and would wait until the turn is interrupted.
#[derive(Debug, Clone)]
pub struct ToolReply(Arc<ReplySlot>);

#[derive(Debug)]
struct ReplySlot(std::sync::Mutex<Option<(Client, RequestId)>>);

impl ToolReply {
    fn new(client: Client, id: RequestId) -> Self {
        Self(Arc::new(ReplySlot(std::sync::Mutex::new(Some((
            client, id,
        ))))))
    }

    /// Answers the call, unless it was answered already.
    pub async fn answer(&self, response: &DynamicToolCallResponse) {
        let taken = lock(&self.0.0).take();
        if let Some((client, id)) = taken
            && let Err(error) = client.respond(id, response).await
        {
            tracing::warn!(%error, "could not answer a tool call");
        }
    }
}

impl Drop for ReplySlot {
    fn drop(&mut self) {
        let taken = lock(&self.0).take();
        if let Some((client, id)) = taken
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            runtime.spawn(async move {
                let answer = DynamicToolCallResponse::text(false, tools::NOT_AVAILABLE);
                let _ = client.respond(id, &answer).await;
            });
        }
    }
}

/// Refusals that outlive any one app server.
///
/// A thread that fails its checks stays refused on every later app server.
/// A safety rule broken mid-turn shows that the capability rule did not
/// hold, so it taints Codex for the whole gateway: every app server is
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

/// The household's ChatGPT sign-in, as an admin sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HouseholdStatus {
    pub signed_in: bool,
    pub email: Option<String>,
    pub plan: Option<String>,
}

/// Starts, checks and stops each user's app server and the household's.
#[derive(Debug)]
pub struct Supervisor {
    /// This supervisor, for the tasks it starts.
    me: Weak<Supervisor>,
    data_dir: PathBuf,
    catalog: Catalog,
    launcher: Launcher,
    settings: HouseholdSettings,
    events: mpsc::UnboundedSender<RuntimeEvent>,
    slots: std::sync::Mutex<HashMap<UserId, Slot>>,
    household: Slot,
    /// Held while the household's sign-in is refreshed or signed out, so
    /// that only one refresh runs at a time.
    refreshing: tokio::sync::Mutex<()>,
    /// When a refresh last failed, by the system clock (Unix seconds).
    refresh_failed_at: std::sync::Mutex<Option<i64>>,
    /// The latest device-code sign-in of the household.
    login: std::sync::Mutex<Option<Login>>,
    /// Users whose app server was restarted after a credential failure, and
    /// not since seen to complete a turn. Each is restarted once.
    recovering: std::sync::Mutex<HashSet<UserId>>,
    recoveries: AtomicU64,
    /// The serial number the next app server gets.
    serials: AtomicU64,
    latch: Arc<Latch>,
}

impl Supervisor {
    /// A supervisor and the stream of everything users' app servers report.
    pub fn new(
        data_dir: impl Into<PathBuf>,
        catalog: Catalog,
        launcher: Launcher,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<RuntimeEvent>) {
        Self::with_settings(data_dir, catalog, launcher, HouseholdSettings::default())
    }

    /// The same, with the household's refresh timing.
    pub fn with_settings(
        data_dir: impl Into<PathBuf>,
        catalog: Catalog,
        launcher: Launcher,
        settings: HouseholdSettings,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<RuntimeEvent>) {
        let (events, receiver) = mpsc::unbounded_channel();
        let data_dir = data_dir.into();
        let supervisor = Arc::new_cyclic(|me| Self {
            me: me.clone(),
            data_dir,
            catalog,
            launcher,
            settings,
            events,
            slots: Default::default(),
            household: Default::default(),
            refreshing: Default::default(),
            refresh_failed_at: Default::default(),
            login: Default::default(),
            recovering: Default::default(),
            recoveries: AtomicU64::new(0),
            serials: AtomicU64::new(1),
            latch: Arc::default(),
        });
        (supervisor, receiver)
    }

    pub fn paths(&self, user: UserId) -> UserPaths {
        UserPaths::new(&self.data_dir, user)
    }

    pub fn household_paths(&self) -> UserPaths {
        UserPaths::household(&self.data_dir)
    }

    fn slot(&self, user: UserId) -> Slot {
        lock(&self.slots).entry(user).or_default().clone()
    }

    /// Why Codex is refused for the whole gateway, if it is.
    pub fn tainted(&self) -> Option<String> {
        self.latch.tainted()
    }

    fn ensure_untainted(&self) -> Result<(), CodexError> {
        match self.latch.tainted() {
            Some(reason) => Err(Refusal::new(format!(
                "Codex is refused until the gateway restarts: {reason}"
            ))
            .into()),
            None => Ok(()),
        }
    }

    // -- Users' app servers --------------------------------------------------

    /// The user's running app server, started and checked if needed. The
    /// household must be signed in, with its token outside the refresh
    /// window, every time.
    pub async fn runtime(&self, user: UserId) -> Result<Arc<Runtime>, CodexError> {
        self.ensure_untainted()?;
        self.ensure_household().await?;
        let auth = self.paths(user).auth_json();
        let slot = self.slot(user);
        let mut slot = slot.lock().await;
        if let Some(runtime) = slot.as_ref().filter(|r| r.is_alive()) {
            // The link must still be the household's.
            if household::inspect(&auth).ok() == Some(AuthFile::Link) {
                return Ok(runtime.clone());
            }
            tracing::error!(user = %user, "the user's auth.json is no longer the household's link");
            runtime.stop();
            *slot = None;
            return Err(Refusal::new("the user's auth.json is not the household's sign-in").into());
        }
        if let Some(old) = slot.take() {
            old.stop();
        }
        self.link(user).await?;
        let runtime = self.start(Owner::User(user)).await?;
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

    /// Stops every user's app server.
    async fn stop_users(&self) {
        let slots: Vec<Slot> = lock(&self.slots).values().cloned().collect();
        for slot in slots {
            if let Some(runtime) = slot.lock().await.take() {
                runtime.stop();
            }
        }
    }

    /// Makes the user's `auth.json` the link to the household's. A user's
    /// own earlier sign-in is signed out first, through the user's app
    /// server, which revokes it. Anything else there is refused.
    async fn link(&self, user: UserId) -> Result<(), CodexError> {
        let paths = self.paths(user);
        paths
            .create()
            .map_err(|e| CodexError::AppServer(format!("creating the user's directories: {e}")))?;
        let auth = paths.auth_json();
        let found = household::inspect(&auth)
            .map_err(|e| CodexError::AppServer(format!("reading the user's auth.json: {e}")))?;
        match found {
            AuthFile::Link => return Ok(()),
            AuthFile::Missing => {}
            AuthFile::File => self.sign_out_own(user).await?,
            AuthFile::Other(what) => {
                tracing::error!(user = %user, what, "refusing the user's app server: auth.json is not the household's link");
                return Err(Refusal::new(format!(
                    "the user's auth.json is {what}, not the household's sign-in"
                ))
                .into());
            }
        }
        household::make_link(&auth)
            .map_err(|e| CodexError::AppServer(format!("linking the user's auth.json: {e}")))?;
        match household::inspect(&auth) {
            Ok(AuthFile::Link) => {
                tracing::info!(user = %user, "linked the user's auth.json to the household's");
                Ok(())
            }
            _ => Err(Refusal::new("the user's auth.json is not the household's sign-in").into()),
        }
    }

    /// Signs out a user's own earlier sign-in through the user's app server,
    /// which asks the authority to revoke it and deletes the file (even if
    /// the revoke request fails). The only time the gateway asks
    /// a user's app server to sign out.
    async fn sign_out_own(&self, user: UserId) -> Result<(), CodexError> {
        tracing::info!(user = %user, "signing out the user's own earlier ChatGPT sign-in");
        let runtime = self.start(Owner::User(user)).await?;
        let result = timed(runtime.client.account_logout()).await;
        runtime.stop();
        result?;
        match household::inspect(&self.paths(user).auth_json()) {
            Ok(AuthFile::Missing) => {
                tracing::info!(user = %user, "signed out the user's own earlier sign-in");
                Ok(())
            }
            _ => Err(Refusal::new("the user's own earlier sign-in was not signed out").into()),
        }
    }

    /// Signs out every user's own earlier sign-in and links them all to the
    /// household's, as the gateway starts, whether or not the household is
    /// signed in. A user whose sign-out fails is left as it is, and refused
    /// until it succeeds.
    pub async fn sign_out_own_sign_ins(&self) {
        let Ok(entries) = std::fs::read_dir(self.data_dir.join("users")) else {
            return;
        };
        for entry in entries.flatten() {
            let Some(user) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse().ok())
                .map(UserId)
            else {
                continue;
            };
            let auth = self.paths(user).auth_json();
            if household::inspect(&auth).ok() != Some(AuthFile::File) {
                continue;
            }
            let slot = self.slot(user);
            let _slot = slot.lock().await;
            if let Err(error) = self.link(user).await {
                tracing::error!(user = %user, %error, "could not sign out the user's own sign-in");
            }
        }
    }

    // -- The household -------------------------------------------------------

    /// The household app server, started and checked if needed.
    pub async fn household(&self) -> Result<Arc<Runtime>, CodexError> {
        self.ensure_untainted()?;
        let mut slot = self.household.lock().await;
        if let Some(runtime) = slot.as_ref().filter(|r| r.is_alive()) {
            return Ok(runtime.clone());
        }
        if let Some(old) = slot.take() {
            old.stop();
        }
        let paths = self.household_paths();
        paths.create().map_err(|e| {
            CodexError::AppServer(format!("creating the household's directories: {e}"))
        })?;
        match household::inspect(&paths.auth_json()) {
            Ok(AuthFile::Missing | AuthFile::File) => {}
            Ok(AuthFile::Link | AuthFile::Other(_)) => {
                return Err(Refusal::new("the household's auth.json is not a file").into());
            }
            Err(e) => {
                return Err(CodexError::AppServer(format!(
                    "reading the household's auth.json: {e}"
                )));
            }
        }
        let runtime = self.start(Owner::Household).await?;
        *slot = Some(runtime.clone());
        Ok(runtime)
    }

    /// The expiry of the household's access token, in Unix seconds.
    async fn expiry(&self) -> Option<i64> {
        household::access_token_expiry(&self.household_paths().auth_json()).await
    }

    fn outside_window(&self, expiry: i64) -> bool {
        expiry - household::unix_now() > self.settings.refresh_ahead.as_secs() as i64
    }

    /// Makes sure the household app server runs and its access token lies
    /// outside the refresh window, refreshing it first if not. Fails while
    /// the household is signed out, or its token cannot be refreshed.
    pub async fn ensure_household(&self) -> Result<(), CodexError> {
        let household = self.household().await?;
        match self.expiry().await {
            None => return Err(CodexError::SignedOut("no ChatGPT sign-in".into())),
            Some(expiry) if self.outside_window(expiry) => return Ok(()),
            Some(_) => {}
        }
        let _refreshing = self.refreshing.lock().await;
        // Another request may have refreshed it meanwhile.
        match self.expiry().await {
            None => return Err(CodexError::SignedOut("no ChatGPT sign-in".into())),
            Some(expiry) if self.outside_window(expiry) => return Ok(()),
            Some(_) => {}
        }
        let failed_at = *lock(&self.refresh_failed_at);
        if failed_at.is_some_and(|at| {
            household::unix_now() - at < self.settings.retry_after.as_secs() as i64
        }) {
            return Err(CodexError::SignedOut(
                "the sign-in could not be refreshed".into(),
            ));
        }
        self.refresh_with(&household).await
    }

    /// Refreshes the household's sign-in now, whatever its expiry. The
    /// refresher uses [`Supervisor::ensure_household`] instead; this is for
    /// tests and operators.
    pub async fn refresh_household(&self) -> Result<(), CodexError> {
        let household = self.household().await?;
        let _refreshing = self.refreshing.lock().await;
        self.refresh_with(&household).await
    }

    /// Asks the household app server to refresh, then checks the file. Call
    /// with `refreshing` held.
    async fn refresh_with(&self, household: &Runtime) -> Result<(), CodexError> {
        tracing::info!("refreshing the household's ChatGPT sign-in");
        // `account/read` with `refreshToken` refreshes before it answers.
        // Whatever it answers, the file says whether the refresh happened.
        if let Err(error) = timed(household.client.account_read(&GetAccountParams {
            refresh_token: true,
        }))
        .await
        {
            tracing::warn!(%error, "the household's account read failed while refreshing");
        }
        match self.expiry().await {
            Some(expiry) if self.outside_window(expiry) => {
                *lock(&self.refresh_failed_at) = None;
                tracing::info!(
                    expires_in_s = expiry - household::unix_now(),
                    "refreshed the household's ChatGPT sign-in"
                );
                Ok(())
            }
            Some(_) => {
                *lock(&self.refresh_failed_at) = Some(household::unix_now());
                tracing::error!("the household's ChatGPT sign-in could not be refreshed");
                Err(CodexError::SignedOut(
                    "the sign-in could not be refreshed".into(),
                ))
            }
            None => Err(CodexError::SignedOut("no ChatGPT sign-in".into())),
        }
    }

    /// Starts the household app server and then keeps its sign-in fresh,
    /// looking every [`HouseholdSettings::check_every`], until the gateway
    /// stops.
    pub async fn run_refresher(self: Arc<Self>) {
        if let Err(error) = self.household().await {
            tracing::error!(%error, "the household's app server did not start");
        }
        let mut tick = tokio::time::interval(self.settings.check_every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if self.tainted().is_some() {
                continue;
            }
            // Nothing to refresh while signed out.
            if self.expiry().await.is_none() {
                continue;
            }
            if let Err(error) = self.ensure_household().await {
                tracing::warn!(%error, "the household's sign-in is not usable");
            }
        }
    }

    /// The household's sign-in. Signed in means usable: the household app
    /// server runs and its token lies outside the refresh window. With
    /// `detail`, for admins, also asks the app server for the account's
    /// email and plan.
    pub async fn household_status(&self, detail: bool) -> HouseholdStatus {
        let signed_out = HouseholdStatus {
            signed_in: false,
            email: None,
            plan: None,
        };
        if self.ensure_household().await.is_err() {
            return signed_out;
        }
        if !detail {
            return HouseholdStatus {
                signed_in: true,
                ..signed_out
            };
        }
        let Ok(household) = self.household().await else {
            return signed_out;
        };
        match timed(household.client.account_read(&GetAccountParams::default())).await {
            Ok(GetAccountResponse {
                account:
                    Some(bokkie_codex::protocol::Account::Chatgpt {
                        email, plan_type, ..
                    }),
                ..
            }) => HouseholdStatus {
                signed_in: true,
                email,
                plan: Some(plan_type),
            },
            Ok(GetAccountResponse { account: None, .. }) => signed_out,
            Ok(_) => HouseholdStatus {
                signed_in: true,
                ..signed_out
            },
            Err(error) => {
                tracing::warn!(%error, "reading the household's account failed");
                signed_out
            }
        }
    }

    /// Starts a device-code sign-in of the household app server, cancelling
    /// any pending one.
    pub async fn start_login(&self, now: Timestamp) -> Result<Login, CodexError> {
        let household = self.household().await?;
        self.cancel_login(now).await?;
        match timed(
            household
                .client
                .account_login_start(&LoginAccountParams::ChatgptDeviceCode),
        )
        .await?
        {
            LoginAccountResponse::ChatgptDeviceCode {
                login_id,
                user_code,
                verification_url,
            } => {
                let login = Login {
                    runtime: household.serial,
                    login_id,
                    verification_url,
                    user_code,
                    state: LoginState::Pending,
                    expires_at: Timestamp(now.0 + household::DEVICE_CODE_LIFETIME_MS),
                    error: None,
                };
                *lock(&self.login) = Some(login.clone());
                tracing::info!("started a device-code sign-in of the household");
                Ok(login)
            }
            LoginAccountResponse::Unknown => Err(CodexError::Rejected(
                "Codex did not offer a device-code sign-in".into(),
            )),
        }
    }

    /// The latest device-code sign-in, if any since the gateway started.
    pub fn login(&self, now: Timestamp) -> Option<Login> {
        let mut login = lock(&self.login);
        if let Some(login) = login.as_mut() {
            login.expire(now);
        }
        login.clone()
    }

    /// Cancels a pending device-code sign-in. Returns the latest sign-in.
    pub async fn cancel_login(&self, now: Timestamp) -> Result<Option<Login>, CodexError> {
        let pending = self
            .login(now)
            .filter(|login| login.state == LoginState::Pending);
        if let Some(pending) = pending {
            if let Some(login) = lock(&self.login)
                .as_mut()
                .filter(|l| l.login_id == pending.login_id)
            {
                login.state = LoginState::Cancelled;
            }
            if let Ok(household) = self.household().await {
                let cancelled = timed(household.client.account_login_cancel(
                    &CancelLoginAccountParams {
                        login_id: pending.login_id,
                    },
                ))
                .await;
                if let Err(error) = cancelled {
                    tracing::warn!(%error, "cancelling the household's sign-in failed");
                }
            }
            tracing::info!("cancelled a device-code sign-in of the household");
        }
        Ok(self.login(now))
    }

    fn login_completed(&self, completed: &AccountLoginCompletedNotification) {
        let mut login = lock(&self.login);
        let Some(login) = login
            .as_mut()
            .filter(|l| completed.login_id.as_deref() == Some(l.login_id.as_str()))
        else {
            return;
        };
        login.completed(completed.success, completed.error.clone());
        tracing::info!(state = ?login.state, "the household's device-code sign-in ended");
        if completed.success {
            *lock(&self.refresh_failed_at) = None;
        }
    }

    /// The household app server with this serial number has closed, so a
    /// sign-in still pending on it has failed. A sign-in started on a later
    /// household app server is left alone: the closed one's watcher can
    /// finish after its successor has started and taken a new sign-in.
    pub fn household_exited(&self, runtime: u64) {
        if let Some(login) = lock(&self.login).as_mut() {
            login.app_server_stopped(runtime);
        }
    }

    /// Signs the household out of ChatGPT, which revokes the sign-in for
    /// every app server, and stops every user's app server, since each
    /// holds the revoked tokens in memory.
    pub async fn sign_out_household(&self, now: Timestamp) -> Result<(), CodexError> {
        let household = self.household().await?;
        self.cancel_login(now).await?;
        let _refreshing = self.refreshing.lock().await;
        timed(household.client.account_logout()).await?;
        self.stop_users().await;
        tracing::info!("signed the household out of ChatGPT");
        Ok(())
    }

    // -- Recovery ------------------------------------------------------------

    /// How many times a user's app server was restarted after a credential
    /// failure.
    pub fn recoveries(&self) -> u64 {
        self.recoveries.load(Ordering::SeqCst)
    }

    fn turn_ended(&self, user: UserId, turn: &Turn, runtime: &Arc<Runtime>) {
        match turn.status {
            TurnStatus::Completed => {
                lock(&self.recovering).remove(&user);
            }
            TurnStatus::Failed if turn.error.as_ref().is_some_and(|e| e.is_unauthorized()) => {
                if let Some(me) = self.me.upgrade() {
                    tokio::spawn(me.recover(user, Arc::downgrade(runtime)));
                }
            }
            _ => {}
        }
    }

    /// Restarts a user's app server whose turn failed on its credential,
    /// once, if the household's sign-in is good, so that it reads the file
    /// again. Not again until one of its turns completes.
    async fn recover(self: Arc<Self>, user: UserId, failed: Weak<Runtime>) {
        if !lock(&self.recovering).insert(user) {
            tracing::error!(user = %user, "the user's app server failed on its credential again after a restart; not restarting it again");
            return;
        }
        if let Err(error) = self.ensure_household().await {
            lock(&self.recovering).remove(&user);
            tracing::warn!(user = %user, %error, "not restarting the user's app server: the household's sign-in is not good");
            return;
        }
        {
            let slot = self.slot(user);
            let mut slot = slot.lock().await;
            let current = slot
                .as_ref()
                .zip(failed.upgrade())
                .is_some_and(|(current, failed)| Arc::ptr_eq(current, &failed));
            if current && let Some(runtime) = slot.take() {
                runtime.stop();
            }
        }
        tracing::warn!(user = %user, "restarting the user's app server after a credential failure");
        self.recoveries.fetch_add(1, Ordering::SeqCst);
        if let Err(error) = self.runtime(user).await {
            tracing::error!(user = %user, %error, "restarting the user's app server failed");
        }
    }

    // -- Starting ------------------------------------------------------------

    async fn start(&self, owner: Owner) -> Result<Arc<Runtime>, CodexError> {
        let paths = UserPaths::of(&self.data_dir, owner);
        paths
            .create()
            .map_err(|e| CodexError::AppServer(format!("creating {owner}'s directories: {e}")))?;
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
                let (client, incoming) = connect(owner, &paths);
                (client, incoming, None)
            }
        };
        let sink = match owner {
            Owner::Household => Sink::Household(self.me.clone()),
            Owner::User(user) => Sink::User(user, self.events.clone(), self.me.clone()),
        };
        let runtime = Arc::new(Runtime {
            owner,
            serial: self.serials.fetch_add(1, Ordering::SeqCst),
            client,
            server: std::sync::Mutex::new(server),
            expected,
            alive: Arc::new(AtomicBool::new(true)),
            served: Default::default(),
            latch: self.latch.clone(),
        });
        tokio::spawn(watch(runtime.clone(), incoming, sink));
        if let Err(error) = runtime.startup_checks().await {
            tracing::error!(%owner, %error, "refusing the app server");
            runtime.stop();
            return Err(error);
        }
        tracing::info!(%owner, "app server ready");
        Ok(runtime)
    }
}

/// Where an app server's messages go.
#[derive(Debug, Clone)]
enum Sink {
    User(
        UserId,
        mpsc::UnboundedSender<RuntimeEvent>,
        Weak<Supervisor>,
    ),
    Household(Weak<Supervisor>),
}

impl Sink {
    fn send(&self, kind: RuntimeEventKind) {
        if let Self::User(user, events, _) = self {
            let _ = events.send(RuntimeEvent { user: *user, kind });
        }
    }
}

/// A running, checked app server.
#[derive(Debug)]
pub struct Runtime {
    pub owner: Owner,
    /// Tells this app server apart from the owner's earlier and later ones.
    pub serial: u64,
    client: Client,
    /// The child process, killed when the runtime stops or drops.
    server: std::sync::Mutex<Option<AppServer>>,
    expected: Expected,
    alive: Arc<AtomicBool>,
    /// Threads that passed their checks on this app server, with the grants
    /// each was started with: the only tools it may call.
    served: std::sync::Mutex<HashMap<String, GrantSet>>,
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

    /// The user whose app server this is; the household's runs no threads.
    fn user(&self) -> Result<UserId, CodexError> {
        match self.owner {
            Owner::User(user) => Ok(user),
            Owner::Household => {
                Err(Refusal::new("the household's app server runs no threads").into())
            }
        }
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

    fn served(
        &self,
        response: ThreadSessionResponse,
        grants: &GrantSet,
    ) -> Result<ThreadSessionResponse, CodexError> {
        let user = self.user()?;
        if let Err(refusal) = guard::check_thread(&response, &self.expected) {
            tracing::error!(user = %user, thread = %response.thread.id, %refusal, "refusing a thread");
            self.refuse(user, &response.thread.id, &refusal.0);
            return Err(CodexError::ThreadRefused(refusal));
        }
        lock(&self.served).insert(response.thread.id.clone(), grants.clone());
        Ok(response)
    }

    /// Starts a thread in the user's workspace under the `bokkie` profile,
    /// offering exactly the tools of `grants`, with web search on only for
    /// `web.search`, and checks it before serving it.
    pub async fn start_thread(
        &self,
        developer_instructions: String,
        grants: &GrantSet,
    ) -> Result<ThreadSessionResponse, CodexError> {
        self.user()?;
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
            dynamic_tools: tools::specs(grants),
            config: config::thread_overrides(grants),
            ..ThreadStartParams::default()
        }))
        .await?;
        self.served(response, grants)
    }

    /// Loads a thread into this app server and checks it before serving it.
    /// `grants` are those the thread started with: Codex restores its tools
    /// and its developer instructions from then, but not its configuration
    /// overrides, so they are sent again.
    pub async fn resume_thread(
        &self,
        thread_id: &str,
        developer_instructions: String,
        grants: &GrantSet,
    ) -> Result<ThreadSessionResponse, CodexError> {
        self.ensure_not_refused(thread_id)?;
        self.check_config().await?;
        let response = timed(self.client.thread_resume(&ThreadResumeParams {
            thread_id: thread_id.into(),
            cwd: Some(self.expected.workspace.clone()),
            approval_policy: Some(AskForApproval::Never),
            permissions: Some(PROFILE.into()),
            developer_instructions: Some(developer_instructions),
            config: config::thread_overrides(grants),
        }))
        .await?;
        if response.thread.id != thread_id {
            return Err(Refusal::new("thread/resume answered for a different thread").into());
        }
        self.served(response, grants)
    }

    /// The grants a thread served here started with.
    pub fn thread_grants(&self, thread_id: &str) -> Option<GrantSet> {
        lock(&self.served).get(thread_id).cloned()
    }

    /// Whether the thread passed its checks on this app server.
    pub fn is_served(&self, thread_id: &str) -> bool {
        let Ok(user) = self.user() else {
            return false;
        };
        self.is_alive()
            && lock(&self.served).contains_key(thread_id)
            && self.latch.thread_refusal(user, thread_id).is_none()
    }

    fn ensure_not_refused(&self, thread_id: &str) -> Result<(), CodexError> {
        let user = self.user()?;
        if let Some(reason) = self.latch.tainted() {
            return Err(Refusal::new(reason).into());
        }
        match self.latch.thread_refusal(user, thread_id) {
            Some(reason) => Err(CodexError::ThreadRefused(Refusal::new(reason))),
            None => Ok(()),
        }
    }

    fn ensure_served(&self, thread_id: &str) -> Result<(), CodexError> {
        self.ensure_not_refused(thread_id)?;
        if !lock(&self.served).contains_key(thread_id) {
            return Err(
                Refusal::new("the thread has not passed its checks on this app server").into(),
            );
        }
        Ok(())
    }

    fn refuse(&self, user: UserId, thread_id: &str, reason: &str) {
        lock(&self.served).remove(thread_id);
        self.latch.refuse_thread(user, thread_id, reason);
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

    /// Adds a message to the running turn of a served thread. The
    /// `userMessage` item that records it in the thread carries
    /// `client_id`.
    pub async fn steer_turn(
        &self,
        thread_id: &str,
        turn_id: &str,
        text: &str,
        client_id: &str,
        context: Option<Context>,
    ) -> Result<String, CodexError> {
        self.ensure_served(thread_id)?;
        Ok(timed(self.client.turn_steer(&TurnSteerParams {
            thread_id: thread_id.into(),
            expected_turn_id: turn_id.into(),
            input: vec![UserInput::Text { text: text.into() }],
            client_user_message_id: Some(client_id.into()),
            additional_context: context,
        }))
        .await?
        .turn_id)
    }

    /// Interrupts a turn. Works on refused threads too.
    pub async fn interrupt_turn(&self, thread_id: &str, turn_id: &str) -> Result<(), CodexError> {
        self.user()?;
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
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Reads one app server's messages until it closes: checks every item,
/// answers every server request, and forwards a user's notifications.
async fn watch(runtime: Arc<Runtime>, mut incoming: mpsc::UnboundedReceiver<Incoming>, sink: Sink) {
    let owner = runtime.owner;
    let serial = runtime.serial;
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
                {
                    // A thread this app server never served has no grants.
                    let grants = runtime.thread_grants(&item.thread_id).unwrap_or_default();
                    let checked = guard::check_item(&item.item, &grants);
                    if let Err(refusal) = &checked {
                        violation(&runtime, Some(&item.thread_id), refusal, &sink).await;
                    }
                    // The verdict travels with the search, so recording it
                    // never depends on which app server is running later.
                    if let (
                        ServerNotification::ItemCompleted(_),
                        bokkie_codex::protocol::ThreadItem::WebSearch {
                            id, query, action, ..
                        },
                    ) = (&notification, &item.item)
                    {
                        sink.send(RuntimeEventKind::WebSearchCompleted {
                            thread_id: item.thread_id.clone(),
                            turn_id: item.turn_id.clone(),
                            item_id: id.clone(),
                            query: query.clone(),
                            action: action.clone(),
                            granted: checked.is_ok(),
                        });
                    }
                }
                match (&sink, &notification) {
                    (Sink::User(user, _, supervisor), ServerNotification::TurnCompleted(done)) => {
                        if let Some(supervisor) = supervisor.upgrade() {
                            supervisor.turn_ended(*user, &done.turn, &runtime);
                        }
                    }
                    (
                        Sink::Household(supervisor),
                        ServerNotification::AccountLoginCompleted(completed),
                    ) => {
                        if let Some(supervisor) = supervisor.upgrade() {
                            supervisor.login_completed(completed);
                        }
                    }
                    _ => {}
                }
                sink.send(RuntimeEventKind::Notification(notification));
            }
            Incoming::Request(request) => {
                let answer = match request.kind {
                    ServerRequestKind::DynamicToolCall(call) => {
                        match (&sink, runtime.thread_grants(&call.thread_id)) {
                            // Granted: the broker serves it in its own task,
                            // so this loop never waits on a tool.
                            (Sink::User(..), Some(grants))
                                if tools::allows(
                                    &grants,
                                    &call.tool,
                                    call.namespace.as_deref(),
                                ) =>
                            {
                                sink.send(RuntimeEventKind::ToolCall {
                                    call,
                                    reply: ToolReply::new(client.clone(), request.id),
                                });
                                Ok(())
                            }
                            // A tool the thread was not granted.
                            (_, Some(_)) => {
                                let refusal = Refusal::new(format!(
                                    "the model called {}, which its thread was not granted",
                                    tool_name(&call)
                                ));
                                violation(&runtime, Some(&call.thread_id), &refusal, &sink).await;
                                client
                                    .respond(
                                        request.id,
                                        &DynamicToolCallResponse::text(false, tools::NOT_AVAILABLE),
                                    )
                                    .await
                            }
                            // A thread this app server does not serve.
                            (_, None) => {
                                tracing::warn!(%owner, tool = %tool_name(&call), "a tool call from a thread that is not served here");
                                client
                                    .respond(
                                        request.id,
                                        &DynamicToolCallResponse::text(false, tools::NOT_AVAILABLE),
                                    )
                                    .await
                            }
                        }
                    }
                    ServerRequestKind::CommandExecutionApproval(params) => {
                        let refusal = Refusal::new("the app server asked to approve a command");
                        violation(&runtime, Some(&params.thread_id), &refusal, &sink).await;
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
                        violation(&runtime, Some(&params.thread_id), &refusal, &sink).await;
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
                            // Any other approval also breaks the capability rule.
                            if let Some(thread_id) = params
                                .as_ref()
                                .and_then(|p| p.get("threadId").or_else(|| p.get("conversationId")))
                                .and_then(|t| t.as_str())
                            {
                                let refusal = Refusal::new(format!("the app server sent {method}"));
                                violation(&runtime, Some(thread_id), &refusal, &sink).await;
                            }
                        }
                        client
                            .respond_error(request.id, RpcError::method_not_found(&method))
                            .await
                    }
                };
                if let Err(error) = answer {
                    tracing::warn!(%owner, %error, "could not answer a server request");
                }
            }
            Incoming::MalformedRequest { id, method, error } => {
                tracing::warn!(%owner, %method, %error, "undecodable server request");
                let _ = client
                    .respond_error(id, RpcError::method_not_found(&method))
                    .await;
                // Fail closed: an approval that cannot be read is still one.
                if method.contains("pproval") {
                    let refusal =
                        Refusal::new(format!("the app server sent an unreadable {method}"));
                    violation(&runtime, None, &refusal, &sink).await;
                }
            }
            Incoming::Malformed { line, error } => {
                tracing::warn!(%owner, %error, "undecodable app-server message");
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
                    violation(&runtime, thread.as_deref(), &refusal, &sink).await;
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
    tracing::info!(%owner, "app server closed");
    if let Sink::Household(supervisor) = &sink
        && let Some(supervisor) = supervisor.upgrade()
    {
        supervisor.household_exited(serial);
    }
    sink.send(RuntimeEventKind::Exited);
}

/// A tool call's name for logs, bounded: the model chose it.
fn tool_name(call: &DynamicToolCallParams) -> String {
    tools::display_name(&call.tool, call.namespace.as_deref())
}

/// Handles a broken safety rule: refuses the thread, taints Codex for the
/// whole gateway, which stops every app server, and reports it.
async fn violation(runtime: &Runtime, thread_id: Option<&str>, refusal: &Refusal, sink: &Sink) {
    tracing::error!(
        owner = %runtime.owner,
        thread = thread_id.unwrap_or("-"),
        %refusal,
        "safety rule broken; stopping Codex for the whole gateway until it restarts"
    );
    if let (Some(thread_id), Owner::User(user)) = (thread_id, runtime.owner) {
        runtime.refuse(user, thread_id, &refusal.0);
        sink.send(RuntimeEventKind::Refused {
            thread_id: thread_id.into(),
            reason: refusal.0.clone(),
        });
    }
    runtime.latch.taint(&refusal.0);
    runtime.stop();
}
