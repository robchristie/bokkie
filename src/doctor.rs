//! Read-only storage integrity and external reconciliation diagnostics.
//!
//! The doctor deliberately does not use [`crate::Store`]. It opens SQLite in
//! read-only mode, enables `query_only`, and reads all database evidence from
//! one deferred transaction. External observations begin only after that
//! transaction has ended.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    gardener::{normalise_goal_prompt, proposal_fingerprint},
    migrations::MigrationManifestEntry,
    process::{
        CancellationToken, EffectRisk, NoopHeartbeat, ProcessLimits, ProcessOutcome,
        ProcessSupervisor,
    },
};

const REPORT_FORMAT_VERSION: u32 = 1;
const DEFAULT_FINDING_LIMIT: usize = 100;
const DEFAULT_QUICK_CHECK_ERROR_LIMIT: usize = 100;
const DEFAULT_FOREIGN_KEY_ERROR_LIMIT: usize = 100;
const MAX_CONFIGURED_ERROR_LIMIT: usize = 10_000;

/// Severity of one stable diagnostic check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorStatus {
    Pass,
    Warning,
    Fail,
    Skipped,
}

/// One bounded, actionable diagnostic detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorFinding {
    pub subject: String,
    pub message: String,
}

/// A stable diagnostic category. Consumers should key behaviour on `code`,
/// not the human-facing summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub code: String,
    pub status: DoctorStatus,
    pub summary: String,
    pub findings: Vec<DoctorFinding>,
    pub findings_truncated: bool,
}

/// Snapshot markers captured inside the same deferred transaction as all
/// database checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorWatermarks {
    pub sqlite_schema_version: i64,
    pub sqlite_data_version: i64,
    pub migration_version: Option<i64>,
    pub audit_sequence: Option<i64>,
    pub gardener_event_sequence: Option<i64>,
    pub gardener_run_event_sequence: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorSummary {
    pub passed: usize,
    pub warnings: usize,
    pub failed: usize,
    pub skipped: usize,
    pub healthy: bool,
}

/// Complete JSON-serialisable diagnostic output. The doctor can never repair
/// state, and records that property explicitly for machine consumers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    pub format_version: u32,
    pub observed_at: i64,
    pub repair_performed: bool,
    pub watermarks: DoctorWatermarks,
    pub summary: DoctorSummary,
    pub checks: Vec<DoctorCheck>,
}

#[derive(Debug, Clone, Copy)]
pub struct DoctorOptions {
    pub observed_at: i64,
    pub quick_check_error_limit: usize,
    pub foreign_key_error_limit: usize,
    pub finding_limit: usize,
}

impl DoctorOptions {
    pub fn at(observed_at: i64) -> Self {
        Self {
            observed_at,
            ..Self::default()
        }
    }
}

impl Default for DoctorOptions {
    fn default() -> Self {
        Self {
            observed_at: 0,
            quick_check_error_limit: DEFAULT_QUICK_CHECK_ERROR_LIMIT,
            foreign_key_error_limit: DEFAULT_FOREIGN_KEY_ERROR_LIMIT,
            finding_limit: DEFAULT_FINDING_LIMIT,
        }
    }
}

#[derive(Debug, Error)]
pub enum DoctorError {
    #[error("cannot open database read-only: {0}")]
    Open(#[source] rusqlite::Error),
    #[error("cannot establish read-only diagnostic snapshot: {0}")]
    Snapshot(#[source] rusqlite::Error),
    #[error("diagnostic option {name} must be between 1 and {MAX_CONFIGURED_ERROR_LIMIT}")]
    InvalidLimit { name: &'static str },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredCheckout {
    pub repository: String,
    pub default_branch: String,
    pub checkout_path: PathBuf,
    pub runs: Vec<RegisteredRunCheckout>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredRunCheckout {
    pub run_id: String,
    pub implementation_worktree_path: PathBuf,
    pub verification_worktree_path: Option<PathBuf>,
    pub branch: String,
    pub source_commit: String,
    pub git_commit: Option<String>,
    pub pushed_head: Option<String>,
    pub verification_head: Option<String>,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredPullRequest {
    pub run_id: String,
    pub repository: String,
    pub number: u64,
    pub url: String,
    pub head: String,
    pub publication_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutObservation {
    pub canonical_path: PathBuf,
    pub origin_url: String,
    pub head: String,
    pub default_branch_head: Option<String>,
    pub worktrees: Vec<ObservedWorktree>,
    pub local_branches: Vec<ObservedRef>,
    pub cached_remote_branches: Vec<ObservedRef>,
    pub live_remote_branches: Result<Vec<ObservedRef>, ObservationError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedWorktree {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedRef {
    pub name: String,
    pub head: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestObservation {
    pub number: u64,
    pub url: String,
    pub head: String,
    pub state: String,
    pub draft: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ObservationError {
    #[error("observation is unavailable: {0}")]
    Unavailable(String),
    #[error("observation stopped with ambiguous external state: {0}")]
    Ambiguous(String),
    #[error("observation returned invalid evidence: {0}")]
    Invalid(String),
}

/// Read-only boundary for local Git and public GitHub observations.
pub trait ExternalObserver: Send + Sync {
    fn observe_checkout(
        &self,
        checkout: &RegisteredCheckout,
    ) -> Result<CheckoutObservation, ObservationError>;

    fn observe_pull_request(
        &self,
        pull_request: &RegisteredPullRequest,
    ) -> Result<PullRequestObservation, ObservationError>;
}

/// Explicitly disables external reconciliation while keeping the database
/// diagnostics useful and honest about the omitted observation.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoExternalObserver;

impl ExternalObserver for NoExternalObserver {
    fn observe_checkout(
        &self,
        _checkout: &RegisteredCheckout,
    ) -> Result<CheckoutObservation, ObservationError> {
        Err(ObservationError::Unavailable(
            "external observation was disabled".to_owned(),
        ))
    }

    fn observe_pull_request(
        &self,
        _pull_request: &RegisteredPullRequest,
    ) -> Result<PullRequestObservation, ObservationError> {
        Err(ObservationError::Unavailable(
            "external observation was disabled".to_owned(),
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOnlyInvocation {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub environment: Vec<(String, String)>,
    pub current_directory: Option<PathBuf>,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOnlyCommandOutput {
    pub stdout: String,
}

pub trait ReadOnlyCommandExecutor: Send + Sync {
    fn execute(
        &self,
        invocation: &ReadOnlyInvocation,
    ) -> Result<ReadOnlyCommandOutput, ObservationError>;
}

#[derive(Debug, Default)]
pub struct SystemReadOnlyCommandExecutor;

impl ReadOnlyCommandExecutor for SystemReadOnlyCommandExecutor {
    fn execute(
        &self,
        invocation: &ReadOnlyInvocation,
    ) -> Result<ReadOnlyCommandOutput, ObservationError> {
        let mut command = Command::new(&invocation.program);
        command
            .args(&invocation.arguments)
            .env_clear()
            .envs(invocation.environment.iter().cloned());
        if let Some(current_directory) = &invocation.current_directory {
            command.current_dir(current_directory);
        }

        let limits = ProcessLimits {
            stdin_message_bytes: 1,
            stdout_bytes: 256 * 1024,
            stderr_bytes: 64 * 1024,
            jsonl_line_bytes: 256 * 1024,
            final_message_bytes: 256 * 1024,
            termination_grace: Duration::from_millis(250),
            poll_interval: Duration::from_millis(10),
        };
        let supervisor =
            ProcessSupervisor::new(invocation.timeout, limits, CancellationToken::new())
                .map_err(ObservationError::Invalid)?;
        let deadline = Instant::now()
            .checked_add(invocation.timeout)
            .ok_or_else(|| ObservationError::Invalid("timeout is out of range".to_owned()))?;
        let outcome = supervisor
            .run(&mut command, deadline, EffectRisk::None, &mut NoopHeartbeat)
            .map_err(|error| ObservationError::Unavailable(error.to_string()))?;
        match outcome {
            ProcessOutcome::Completed { status, evidence } if status.success() => {
                Ok(ReadOnlyCommandOutput {
                    stdout: evidence.stdout.tail.trim().to_owned(),
                })
            }
            ProcessOutcome::Completed { status, evidence } => {
                Err(ObservationError::Unavailable(format!(
                    "command exited with {status}: {}",
                    evidence.stderr.tail.trim()
                )))
            }
            ProcessOutcome::TimedOut(_) => Err(ObservationError::Unavailable(format!(
                "command exceeded {:?}",
                invocation.timeout
            ))),
            ProcessOutcome::OutputLimit { stream, limit, .. } => {
                Err(ObservationError::Unavailable(format!(
                    "command exceeded {stream} limit of {limit} bytes"
                )))
            }
            ProcessOutcome::Cancelled(_) | ProcessOutcome::HeartbeatFailure { .. } => Err(
                ObservationError::Unavailable("command observation was interrupted".to_owned()),
            ),
            ProcessOutcome::AmbiguousExternalState { .. } => Err(ObservationError::Ambiguous(
                "read-only command reported ambiguous external state".to_owned(),
            )),
        }
    }
}

/// Credential-free observer whose complete command vocabulary is constructed
/// internally. Injecting an executor permits deterministic tests of the exact
/// allowlist without executing Git or network commands.
#[derive(Clone)]
pub struct CommandExternalObserver {
    git_program: PathBuf,
    curl_program: PathBuf,
    timeout: Duration,
    executor: Arc<dyn ReadOnlyCommandExecutor>,
}

impl std::fmt::Debug for CommandExternalObserver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CommandExternalObserver")
            .field("git_program", &self.git_program)
            .field("curl_program", &self.curl_program)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl CommandExternalObserver {
    pub fn new(
        git_program: impl Into<PathBuf>,
        curl_program: impl Into<PathBuf>,
        timeout: Duration,
    ) -> Result<Self, ObservationError> {
        Self::with_executor(
            git_program,
            curl_program,
            timeout,
            Arc::new(SystemReadOnlyCommandExecutor),
        )
    }

    pub fn with_executor(
        git_program: impl Into<PathBuf>,
        curl_program: impl Into<PathBuf>,
        timeout: Duration,
        executor: Arc<dyn ReadOnlyCommandExecutor>,
    ) -> Result<Self, ObservationError> {
        if timeout.is_zero() {
            return Err(ObservationError::Invalid(
                "external observation timeout must be positive".to_owned(),
            ));
        }
        let git_program = git_program.into();
        let curl_program = curl_program.into();
        if !git_program.is_absolute() || !curl_program.is_absolute() {
            return Err(ObservationError::Invalid(
                "Git and curl executables must be absolute paths".to_owned(),
            ));
        }
        Ok(Self {
            git_program,
            curl_program,
            timeout,
            executor,
        })
    }

    fn git(&self, checkout: &Path, arguments: &[&str]) -> Result<String, ObservationError> {
        const ALLOWED_SUBCOMMANDS: &[&str] = &["config", "for-each-ref", "rev-parse", "worktree"];
        let Some(subcommand) = arguments.first() else {
            return Err(ObservationError::Invalid(
                "empty Git observation was rejected".to_owned(),
            ));
        };
        if !ALLOWED_SUBCOMMANDS.contains(subcommand) {
            return Err(ObservationError::Invalid(format!(
                "Git subcommand {subcommand:?} is outside the read-only allowlist"
            )));
        }
        let mut invocation_arguments = vec![
            "-c".to_owned(),
            "credential.helper=".to_owned(),
            "-c".to_owned(),
            "core.hooksPath=/dev/null".to_owned(),
            "-C".to_owned(),
            checkout.to_string_lossy().into_owned(),
        ];
        invocation_arguments.extend(arguments.iter().map(|value| (*value).to_owned()));
        let invocation = ReadOnlyInvocation {
            program: self.git_program.clone(),
            arguments: invocation_arguments,
            environment: vec![
                ("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned()),
                ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
                ("GIT_OPTIONAL_LOCKS".to_owned(), "0".to_owned()),
                ("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()),
            ],
            current_directory: None,
            timeout: self.timeout,
        };
        self.executor
            .execute(&invocation)
            .map(|output| output.stdout)
    }

    fn live_https_refs(&self, repository: &str) -> Result<String, ObservationError> {
        if !valid_github_repository(repository) {
            return Err(ObservationError::Invalid(
                "registered repository is not a canonical GitHub owner/name".to_owned(),
            ));
        }
        let invocation = ReadOnlyInvocation {
            program: self.git_program.clone(),
            arguments: vec![
                "-c".to_owned(),
                "credential.helper=".to_owned(),
                "-c".to_owned(),
                "core.hooksPath=/dev/null".to_owned(),
                "-c".to_owned(),
                "protocol.allow=never".to_owned(),
                "-c".to_owned(),
                "protocol.https.allow=always".to_owned(),
                "ls-remote".to_owned(),
                "--heads".to_owned(),
                format!("https://github.com/{repository}.git"),
            ],
            environment: vec![
                ("GIT_ASKPASS".to_owned(), "/bin/false".to_owned()),
                ("GIT_CEILING_DIRECTORIES".to_owned(), "/".to_owned()),
                ("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned()),
                ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
                ("GIT_OPTIONAL_LOCKS".to_owned(), "0".to_owned()),
                ("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()),
                ("SSH_ASKPASS".to_owned(), "/bin/false".to_owned()),
            ],
            current_directory: Some(PathBuf::from("/")),
            timeout: self.timeout,
        };
        self.executor
            .execute(&invocation)
            .map(|output| output.stdout)
    }

    fn curl(&self, url: &str) -> Result<String, ObservationError> {
        let invocation = ReadOnlyInvocation {
            program: self.curl_program.clone(),
            arguments: vec![
                "--disable".to_owned(),
                "--fail".to_owned(),
                "--silent".to_owned(),
                "--show-error".to_owned(),
                "--location".to_owned(),
                "--proto".to_owned(),
                "=https".to_owned(),
                "--max-time".to_owned(),
                self.timeout.as_secs().max(1).to_string(),
                "--header".to_owned(),
                "Accept: application/vnd.github+json".to_owned(),
                "--header".to_owned(),
                "X-GitHub-Api-Version: 2022-11-28".to_owned(),
                "--user-agent".to_owned(),
                "bokkie-doctor".to_owned(),
                url.to_owned(),
            ],
            environment: Vec::new(),
            current_directory: None,
            timeout: self.timeout,
        };
        self.executor
            .execute(&invocation)
            .map(|output| output.stdout)
    }
}

impl ExternalObserver for CommandExternalObserver {
    fn observe_checkout(
        &self,
        checkout: &RegisteredCheckout,
    ) -> Result<CheckoutObservation, ObservationError> {
        let root = self.git(&checkout.checkout_path, &["rev-parse", "--show-toplevel"])?;
        let origin_url = self.git(
            &checkout.checkout_path,
            &[
                "config",
                "--local",
                "--no-includes",
                "--get-all",
                "remote.origin.url",
            ],
        )?;
        let expected_root = std::fs::canonicalize(&checkout.checkout_path)
            .unwrap_or_else(|_| checkout.checkout_path.clone());
        let observed_root = std::fs::canonicalize(&root).unwrap_or_else(|_| PathBuf::from(&root));
        if observed_root != expected_root {
            return Err(ObservationError::Invalid(format!(
                "checkout root {observed_root:?} does not match registered path {expected_root:?}"
            )));
        }
        if !origin_matches_repository(&origin_url, &checkout.repository) {
            return Err(ObservationError::Invalid(format!(
                "raw checkout origin {origin_url:?} does not identify {}",
                checkout.repository
            )));
        }
        let head = self.git(
            &checkout.checkout_path,
            &["rev-parse", "--verify", "HEAD^{commit}"],
        )?;
        let default_ref = format!("refs/remotes/origin/{}^{{commit}}", checkout.default_branch);
        let default_branch_head = self
            .git(
                &checkout.checkout_path,
                &["rev-parse", "--verify", &default_ref],
            )
            .ok();
        let worktrees = parse_worktrees(&self.git(
            &checkout.checkout_path,
            &["worktree", "list", "--porcelain", "-z"],
        )?)?;
        let refs = parse_refs(&self.git(
            &checkout.checkout_path,
            &[
                "for-each-ref",
                "--format=%(refname)%00%(objectname)",
                "refs/heads",
                "refs/remotes/origin",
            ],
        )?)?;
        let (local_branches, cached_remote_branches) = refs
            .into_iter()
            .partition(|reference| reference.name.starts_with("refs/heads/"));
        let live_remote_branches = self
            .live_https_refs(&checkout.repository)
            .and_then(|output| parse_ls_remote(&output));
        Ok(CheckoutObservation {
            canonical_path: PathBuf::from(root),
            origin_url,
            head,
            default_branch_head,
            worktrees,
            local_branches,
            cached_remote_branches,
            live_remote_branches,
        })
    }

    fn observe_pull_request(
        &self,
        pull_request: &RegisteredPullRequest,
    ) -> Result<PullRequestObservation, ObservationError> {
        let url = format!(
            "https://api.github.com/repos/{}/pulls/{}",
            pull_request.repository, pull_request.number
        );
        let value: Value = serde_json::from_str(&self.curl(&url)?)
            .map_err(|error| ObservationError::Invalid(error.to_string()))?;
        Ok(PullRequestObservation {
            number: value
                .get("number")
                .and_then(Value::as_u64)
                .ok_or_else(|| ObservationError::Invalid("GitHub number is absent".to_owned()))?,
            url: value
                .get("html_url")
                .and_then(Value::as_str)
                .ok_or_else(|| ObservationError::Invalid("GitHub URL is absent".to_owned()))?
                .to_owned(),
            head: value
                .pointer("/head/sha")
                .and_then(Value::as_str)
                .ok_or_else(|| ObservationError::Invalid("GitHub head is absent".to_owned()))?
                .to_owned(),
            state: value
                .get("state")
                .and_then(Value::as_str)
                .ok_or_else(|| ObservationError::Invalid("GitHub state is absent".to_owned()))?
                .to_owned(),
            draft: value.get("draft").and_then(Value::as_bool).ok_or_else(|| {
                ObservationError::Invalid("GitHub draft flag is absent".to_owned())
            })?,
        })
    }
}

fn parse_worktrees(output: &str) -> Result<Vec<ObservedWorktree>, ObservationError> {
    let mut worktrees = Vec::new();
    let mut current: Option<ObservedWorktree> = None;
    for raw_field in output.split(['\0', '\n']) {
        let field = raw_field.trim();
        if field.is_empty() {
            continue;
        }
        if let Some(path) = field.strip_prefix("worktree ") {
            if let Some(worktree) = current.take() {
                worktrees.push(worktree);
            }
            current = Some(ObservedWorktree {
                path: PathBuf::from(path),
                head: None,
                branch: None,
            });
        } else if let Some(head) = field.strip_prefix("HEAD ") {
            let Some(worktree) = current.as_mut() else {
                return Err(ObservationError::Invalid(
                    "worktree HEAD preceded its path".to_owned(),
                ));
            };
            worktree.head = Some(head.to_owned());
        } else if let Some(branch) = field.strip_prefix("branch ") {
            let Some(worktree) = current.as_mut() else {
                return Err(ObservationError::Invalid(
                    "worktree branch preceded its path".to_owned(),
                ));
            };
            worktree.branch = Some(branch.to_owned());
        }
    }
    if let Some(worktree) = current {
        worktrees.push(worktree);
    }
    Ok(worktrees)
}

fn parse_refs(output: &str) -> Result<Vec<ObservedRef>, ObservationError> {
    output
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (name, head) = line.split_once('\0').ok_or_else(|| {
                ObservationError::Invalid("Git ref output has no identity separator".to_owned())
            })?;
            if name.is_empty() || !is_commit_id(head) {
                return Err(ObservationError::Invalid(
                    "Git ref output contains a malformed identity".to_owned(),
                ));
            }
            Ok(ObservedRef {
                name: name.to_owned(),
                head: head.to_owned(),
            })
        })
        .collect()
}

fn parse_ls_remote(output: &str) -> Result<Vec<ObservedRef>, ObservationError> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let (head, name) = line.split_once(char::is_whitespace).ok_or_else(|| {
                ObservationError::Invalid("ls-remote output has no ref separator".to_owned())
            })?;
            let name = name.trim();
            if !name.starts_with("refs/heads/") || !is_commit_id(head) {
                return Err(ObservationError::Invalid(
                    "ls-remote output contains a malformed branch identity".to_owned(),
                ));
            }
            Ok(ObservedRef {
                name: name.to_owned(),
                head: head.to_owned(),
            })
        })
        .collect()
}

struct DatabaseSnapshotInputs {
    watermarks: DoctorWatermarks,
    checks: Vec<DoctorCheck>,
    checkouts: Vec<RegisteredCheckout>,
    pull_requests: Vec<RegisteredPullRequest>,
}

/// Diagnose an existing database without migrating, repairing, adopting or
/// otherwise mutating it. Schema-level problems are emitted as failed checks
/// wherever SQLite can still establish a snapshot; only opening or starting
/// the snapshot is a top-level error.
pub fn run_doctor(
    path: impl AsRef<Path>,
    manifest: &[MigrationManifestEntry],
    observer: &dyn ExternalObserver,
    options: DoctorOptions,
) -> Result<DoctorReport, DoctorError> {
    validate_limit("quick_check_error_limit", options.quick_check_error_limit)?;
    validate_limit("foreign_key_error_limit", options.foreign_key_error_limit)?;
    validate_limit("finding_limit", options.finding_limit)?;

    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(DoctorError::Open)?;
    connection
        .pragma_update(None, "query_only", true)
        .map_err(DoctorError::Snapshot)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .map_err(DoctorError::Snapshot)?;
    let database = inspect_database(&transaction, manifest, options);
    transaction.commit().map_err(DoctorError::Snapshot)?;

    // External observations are intentionally outside the SQLite snapshot.
    // Their output is reconciliation evidence, never authority to adopt or
    // repair a discrepancy.
    let mut checks = database.checks;
    checks.push(check_external_reconciliation(
        observer,
        &database.checkouts,
        &database.pull_requests,
        options.finding_limit,
    ));
    let summary = summarise(&checks);
    Ok(DoctorReport {
        format_version: REPORT_FORMAT_VERSION,
        observed_at: options.observed_at,
        repair_performed: false,
        watermarks: database.watermarks,
        summary,
        checks,
    })
}

fn validate_limit(name: &'static str, value: usize) -> Result<(), DoctorError> {
    if value == 0 || value > MAX_CONFIGURED_ERROR_LIMIT {
        Err(DoctorError::InvalidLimit { name })
    } else {
        Ok(())
    }
}

fn inspect_database(
    transaction: &Transaction<'_>,
    manifest: &[MigrationManifestEntry],
    options: DoctorOptions,
) -> DatabaseSnapshotInputs {
    let watermarks = capture_watermarks(transaction);
    let checks = vec![
        check_query_only(transaction),
        check_quick_check(transaction, options.quick_check_error_limit),
        check_foreign_keys(transaction, options.foreign_key_error_limit),
        check_migration_manifest(transaction, manifest, options.finding_limit),
        query_check(
            transaction,
            "obligations.attempt_exactness",
            "running and incomplete attempts agree exactly",
            ATTEMPT_EXACTNESS_SQL,
            options.finding_limit,
        ),
        query_check(
            transaction,
            "obligations.audit_projection",
            "obligation projections agree with their latest audit event",
            AUDIT_PROJECTION_SQL,
            options.finding_limit,
        ),
        check_liveness(transaction, options.observed_at, options.finding_limit),
        query_check(
            transaction,
            "gardener.phase_evidence",
            "gardener phases agree with durable evidence shape",
            GARDENER_PHASE_EVIDENCE_SQL,
            options.finding_limit,
        ),
        check_proposal_fingerprints(transaction, options.finding_limit),
        query_check(
            transaction,
            "gardener.source_chain",
            "gardener sources, generations and observations agree exactly",
            GARDENER_SOURCE_CHAIN_SQL,
            options.finding_limit,
        ),
        query_check(
            transaction,
            "gardener.decision_chain",
            "gardener decisions agree with approval and proposal-instance evidence",
            GARDENER_DECISION_CHAIN_SQL,
            options.finding_limit,
        ),
        query_check(
            transaction,
            "gardener.run_chain",
            "gardener runs agree with attempt and proposal-instance evidence",
            GARDENER_RUN_CHAIN_SQL,
            options.finding_limit,
        ),
    ];

    let checkouts = load_registered_checkouts(transaction).unwrap_or_default();
    let pull_requests = load_registered_pull_requests(transaction).unwrap_or_default();
    DatabaseSnapshotInputs {
        watermarks,
        checks,
        checkouts,
        pull_requests,
    }
}

fn check_query_only(transaction: &Transaction<'_>) -> DoctorCheck {
    match transaction.pragma_query_value(None, "query_only", |row| row.get::<_, i64>(0)) {
        Ok(1) => pass(
            "sqlite.read_only_mode",
            "database is open read-only with SQLite query_only enabled",
        ),
        Ok(value) => fail(
            "sqlite.read_only_mode",
            "SQLite query_only is not enabled",
            vec![DoctorFinding {
                subject: "connection".to_owned(),
                message: format!("PRAGMA query_only returned {value}"),
            }],
            false,
        ),
        Err(error) => query_failure("sqlite.read_only_mode", error),
    }
}

fn capture_watermarks(transaction: &Transaction<'_>) -> DoctorWatermarks {
    DoctorWatermarks {
        sqlite_schema_version: transaction
            .pragma_query_value(None, "schema_version", |row| row.get(0))
            .unwrap_or(-1),
        sqlite_data_version: transaction
            .pragma_query_value(None, "data_version", |row| row.get(0))
            .unwrap_or(-1),
        migration_version: optional_max(transaction, "schema_migrations", "version"),
        audit_sequence: optional_max(transaction, "audit_events", "sequence"),
        gardener_event_sequence: optional_max(transaction, "gardener_events", "sequence"),
        gardener_run_event_sequence: optional_max(transaction, "gardener_run_events", "sequence"),
    }
}

fn optional_max(transaction: &Transaction<'_>, table: &str, column: &str) -> Option<i64> {
    let sql = format!("SELECT max({column}) FROM {table}");
    transaction
        .query_row(&sql, [], |row| row.get::<_, Option<i64>>(0))
        .ok()
        .flatten()
}

fn check_quick_check(transaction: &Transaction<'_>, limit: usize) -> DoctorCheck {
    let sql = format!("PRAGMA quick_check({limit})");
    let result = (|| {
        let mut statement = transaction.prepare(&sql)?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()
    })();
    match result {
        Ok(rows) if rows.as_slice() == ["ok"] => {
            pass("sqlite.quick_check", "SQLite bounded quick_check passed")
        }
        Ok(rows) => {
            let truncated = rows.len() >= limit;
            fail(
                "sqlite.quick_check",
                format!(
                    "SQLite quick_check reported {} integrity error(s)",
                    rows.len()
                ),
                rows.into_iter()
                    .map(|message| DoctorFinding {
                        subject: "database".to_owned(),
                        message,
                    })
                    .collect(),
                truncated,
            )
        }
        Err(error) => query_failure("sqlite.quick_check", error),
    }
}

fn check_foreign_keys(transaction: &Transaction<'_>, limit: usize) -> DoctorCheck {
    let sql = format!(
        "SELECT \"table\", rowid, parent, fkid FROM pragma_foreign_key_check LIMIT {}",
        limit.saturating_add(1)
    );
    let result = (|| {
        let mut statement = transaction.prepare(&sql)?;
        statement
            .query_map([], |row| {
                let table: String = row.get(0)?;
                let rowid: Option<i64> = row.get(1)?;
                let parent: String = row.get(2)?;
                let foreign_key: i64 = row.get(3)?;
                Ok(DoctorFinding {
                    subject: rowid.map_or(table.clone(), |id| format!("{table}:{id}")),
                    message: format!(
                        "foreign key {foreign_key} does not reference an existing {parent} row"
                    ),
                })
            })?
            .collect::<Result<Vec<_>, _>>()
    })();
    match result {
        Ok(findings) if findings.is_empty() => {
            pass("sqlite.foreign_keys", "SQLite foreign_key_check passed")
        }
        Ok(mut findings) => {
            let truncated = findings.len() > limit;
            findings.truncate(limit);
            fail(
                "sqlite.foreign_keys",
                if truncated {
                    format!("SQLite foreign_key_check reported at least {limit} violation(s)")
                } else {
                    format!(
                        "SQLite foreign_key_check reported {} violation(s)",
                        findings.len()
                    )
                },
                findings,
                truncated,
            )
        }
        Err(error) => query_failure("sqlite.foreign_keys", error),
    }
}

fn check_migration_manifest(
    transaction: &Transaction<'_>,
    expected: &[MigrationManifestEntry],
    limit: usize,
) -> DoctorCheck {
    let result = (|| -> rusqlite::Result<Vec<(i64, String, Option<String>)>> {
        let mut statement = transaction
            .prepare("SELECT version, name, sha256 FROM schema_migrations ORDER BY version")?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect()
    })();
    let rows = match result {
        Ok(rows) => rows,
        Err(error) => return query_failure("schema.migration_manifest", error),
    };

    let mut findings = Vec::new();
    if expected.is_empty() {
        findings.push(DoctorFinding {
            subject: "build".to_owned(),
            message: "the expected migration manifest is empty".to_owned(),
        });
    }
    for (index, migration) in expected.iter().enumerate() {
        if migration.version != index as i64 + 1 {
            push_bounded(
                &mut findings,
                limit,
                DoctorFinding {
                    subject: format!("build migration {}", migration.version),
                    message: "expected manifest is not contiguous".to_owned(),
                },
            );
        }
    }
    let compared = rows.len().max(expected.len());
    for index in 0..compared {
        match (rows.get(index), expected.get(index)) {
            (Some((version, name, digest)), Some(want)) => {
                if *version != want.version {
                    push_bounded(
                        &mut findings,
                        limit,
                        DoctorFinding {
                            subject: format!("database row {}", index + 1),
                            message: format!(
                                "migration version is {version}, expected {}",
                                want.version
                            ),
                        },
                    );
                }
                if name != want.name {
                    push_bounded(
                        &mut findings,
                        limit,
                        DoctorFinding {
                            subject: format!("migration {version}"),
                            message: format!("name is {name:?}, expected {:?}", want.name),
                        },
                    );
                }
                if digest.as_deref() != Some(want.sha256) {
                    push_bounded(
                        &mut findings,
                        limit,
                        DoctorFinding {
                            subject: format!("migration {version}"),
                            message: "digest does not match the immutable build manifest"
                                .to_owned(),
                        },
                    );
                }
            }
            (Some((version, _, _)), None) => push_bounded(
                &mut findings,
                limit,
                DoctorFinding {
                    subject: format!("migration {version}"),
                    message: "database schema is newer than this build".to_owned(),
                },
            ),
            (None, Some(want)) => push_bounded(
                &mut findings,
                limit,
                DoctorFinding {
                    subject: format!("migration {}", want.version),
                    message: "required migration is not recorded".to_owned(),
                },
            ),
            (None, None) => unreachable!(),
        }
    }
    if findings.is_empty() {
        pass(
            "schema.migration_manifest",
            format!("all {} immutable migration record(s) match", rows.len()),
        )
    } else {
        let truncated = findings.len() >= limit && compared > limit;
        fail(
            "schema.migration_manifest",
            "database migration manifest is incompatible with this build",
            findings,
            truncated,
        )
    }
}

fn check_liveness(transaction: &Transaction<'_>, observed_at: i64, limit: usize) -> DoctorCheck {
    let sql = format!(
        "SELECT id AS subject, CASE
             WHEN state IN ('pending', 'retry_scheduled') THEN
                 'scheduled work has no durable next wake-up'
             WHEN state = 'running' THEN
                 'running work has no active, exactly matching attempt lease at the observation time'
             ELSE 'non-terminal work has no durable liveness condition'
         END AS message
         FROM obligations o
         WHERE state NOT IN ('completed', 'cancelled')
           AND NOT (
             (state IN ('pending', 'retry_scheduled') AND next_wake_at IS NOT NULL)
             OR state IN ('awaiting_approval', 'attention')
             OR (state = 'running' AND lease_expires_at > {observed_at}
                 AND EXISTS (
                     SELECT 1 FROM attempts a
                     WHERE a.obligation_id = o.id
                       AND a.occurrence = o.occurrence
                       AND a.attempt_number = o.attempts_made
                       AND a.lease_generation = o.lease_generation
                       AND a.lease_token = o.lease_token
                       AND a.completed_at IS NULL AND a.outcome = 'running'
                 ))
           )
         ORDER BY id"
    );
    query_check(
        transaction,
        "obligations.nonterminal_liveness",
        "every non-terminal obligation has a durable liveness condition",
        &sql,
        limit,
    )
}

fn check_proposal_fingerprints(transaction: &Transaction<'_>, limit: usize) -> DoctorCheck {
    let result = (|| {
        let mut statement = transaction.prepare(
            "SELECT fingerprint, repository, prompt FROM gardener_proposals ORDER BY fingerprint",
        )?;
        let mut rows = statement.query([])?;
        let mut findings = Vec::new();
        let mut total_invalid = 0_usize;
        while let Some(row) = rows.next()? {
            let fingerprint: String = row.get(0)?;
            let repository: String = row.get(1)?;
            let prompt: String = row.get(2)?;
            let canonical_prompt = normalise_goal_prompt(&prompt);
            let expected = proposal_fingerprint(&repository, &canonical_prompt);
            if fingerprint != expected || prompt != canonical_prompt {
                total_invalid += 1;
                push_bounded(
                    &mut findings,
                    limit,
                    DoctorFinding {
                        subject: fingerprint,
                        message: if prompt != canonical_prompt {
                            "stored prompt is not canonical and its fingerprint cannot be trusted"
                                .to_owned()
                        } else {
                            "fingerprint does not match the repository and canonical prompt"
                                .to_owned()
                        },
                    },
                );
            }
        }
        Ok::<_, rusqlite::Error>((findings, total_invalid))
    })();
    match result {
        Ok((_findings, 0)) => pass(
            "gardener.proposal_fingerprint",
            "all proposal fingerprints match canonical content",
        ),
        Ok((findings, total)) => fail(
            "gardener.proposal_fingerprint",
            format!("{total} proposal fingerprint(s) do not match canonical content"),
            findings,
            total > limit,
        ),
        Err(error) => query_failure("gardener.proposal_fingerprint", error),
    }
}

fn query_check(
    transaction: &Transaction<'_>,
    code: &'static str,
    success_summary: &str,
    sql: &str,
    limit: usize,
) -> DoctorCheck {
    let bounded_sql = format!(
        "SELECT subject, message FROM ({sql}) AS doctor_findings LIMIT {}",
        limit.saturating_add(1)
    );
    let result = (|| {
        let mut statement = transaction.prepare(&bounded_sql)?;
        statement
            .query_map([], |row| {
                Ok(DoctorFinding {
                    subject: row.get(0)?,
                    message: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
    })();
    match result {
        Ok(findings) if findings.is_empty() => pass(code, success_summary),
        Ok(mut findings) => {
            let truncated = findings.len() > limit;
            findings.truncate(limit);
            fail(
                code,
                if truncated {
                    format!("at least {limit} inconsistency or corruption finding(s)")
                } else {
                    format!("{} inconsistency or corruption finding(s)", findings.len())
                },
                findings,
                truncated,
            )
        }
        Err(error) => query_failure(code, error),
    }
}

fn pass(code: &'static str, summary: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        code: code.to_owned(),
        status: DoctorStatus::Pass,
        summary: summary.into(),
        findings: Vec::new(),
        findings_truncated: false,
    }
}

fn fail(
    code: &'static str,
    summary: impl Into<String>,
    findings: Vec<DoctorFinding>,
    findings_truncated: bool,
) -> DoctorCheck {
    DoctorCheck {
        code: code.to_owned(),
        status: DoctorStatus::Fail,
        summary: summary.into(),
        findings,
        findings_truncated,
    }
}

fn query_failure(code: &'static str, error: rusqlite::Error) -> DoctorCheck {
    fail(
        code,
        "check could not read the required schema",
        vec![DoctorFinding {
            subject: "schema".to_owned(),
            message: error.to_string(),
        }],
        false,
    )
}

fn push_bounded(findings: &mut Vec<DoctorFinding>, limit: usize, finding: DoctorFinding) {
    if findings.len() < limit {
        findings.push(finding);
    }
}

fn load_registered_checkouts(
    transaction: &Transaction<'_>,
) -> rusqlite::Result<Vec<RegisteredCheckout>> {
    let mut statement = transaction.prepare(
        "SELECT repository, default_branch, checkout_path
         FROM gardener_repositories ORDER BY repository",
    )?;
    let mut checkouts = statement
        .query_map([], |row| {
            Ok(RegisteredCheckout {
                repository: row.get(0)?,
                default_branch: row.get(1)?,
                checkout_path: PathBuf::from(row.get::<_, String>(2)?),
                runs: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut runs = transaction.prepare(
        "SELECT r.repository, r.id, r.implementation_worktree_path,
                r.verification_worktree_path, r.branch, r.source_commit,
                r.git_commit, r.pushed_head, r.verification_head,
                (o.state = 'running'
                 AND o.occurrence = r.occurrence
                 AND o.lease_generation = r.lease_generation
                 AND o.lease_token = r.lease_token) AS active
         FROM gardener_implementation_runs r
         JOIN obligations o ON o.id = r.obligation_id
         ORDER BY r.repository, r.id",
    )?;
    let rows = runs.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            RegisteredRunCheckout {
                run_id: row.get(1)?,
                implementation_worktree_path: PathBuf::from(row.get::<_, String>(2)?),
                verification_worktree_path: row.get::<_, Option<String>>(3)?.map(PathBuf::from),
                branch: row.get(4)?,
                source_commit: row.get(5)?,
                git_commit: row.get(6)?,
                pushed_head: row.get(7)?,
                verification_head: row.get(8)?,
                active: row.get(9)?,
            },
        ))
    })?;
    for row in rows {
        let (repository, run) = row?;
        if let Some(checkout) = checkouts
            .iter_mut()
            .find(|checkout| checkout.repository == repository)
        {
            checkout.runs.push(run);
        }
    }
    Ok(checkouts)
}

fn load_registered_pull_requests(
    transaction: &Transaction<'_>,
) -> rusqlite::Result<Vec<RegisteredPullRequest>> {
    let mut statement = transaction.prepare(
        "SELECT r.id, r.repository, r.pull_request_number, r.pull_request_url,
                r.pull_request_head,
                CASE WHEN ready.run_id IS NULL THEN r.publication_state ELSE 'ready' END
         FROM gardener_implementation_runs r
         LEFT JOIN gardener_pull_request_ready_observations ready ON ready.run_id = r.id
         WHERE r.pull_request_number IS NOT NULL
         ORDER BY r.id",
    )?;
    statement
        .query_map([], |row| {
            Ok(RegisteredPullRequest {
                run_id: row.get(0)?,
                repository: row.get(1)?,
                number: row.get(2)?,
                url: row.get(3)?,
                head: row.get(4)?,
                publication_state: row.get(5)?,
            })
        })?
        .collect()
}

fn check_external_reconciliation(
    observer: &dyn ExternalObserver,
    checkouts: &[RegisteredCheckout],
    pull_requests: &[RegisteredPullRequest],
    limit: usize,
) -> DoctorCheck {
    if checkouts.is_empty() && pull_requests.is_empty() {
        return DoctorCheck {
            code: "gardener.external_reconciliation".to_owned(),
            status: DoctorStatus::Skipped,
            summary: "no registered gardener checkout or pull request requires observation"
                .to_owned(),
            findings: Vec::new(),
            findings_truncated: false,
        };
    }

    let mut findings = Vec::new();
    let mut failures = 0_usize;
    let mut warnings = 0_usize;
    let mut observed = 0_usize;
    for checkout in checkouts {
        match observer.observe_checkout(checkout) {
            Ok(observation) => {
                observed += 1;
                let expected_path = std::fs::canonicalize(&checkout.checkout_path)
                    .unwrap_or_else(|_| checkout.checkout_path.clone());
                if observation.canonical_path != expected_path {
                    failures += 1;
                    push_bounded(
                        &mut findings,
                        limit,
                        DoctorFinding {
                            subject: checkout.repository.clone(),
                            message: format!(
                                "checkout resolves to {:?}, expected {:?}",
                                observation.canonical_path, expected_path
                            ),
                        },
                    );
                }
                if !origin_matches_repository(&observation.origin_url, &checkout.repository) {
                    failures += 1;
                    push_bounded(
                        &mut findings,
                        limit,
                        DoctorFinding {
                            subject: checkout.repository.clone(),
                            message: format!(
                                "checkout origin {:?} does not identify the registered repository",
                                observation.origin_url
                            ),
                        },
                    );
                }
                if !is_commit_id(&observation.head)
                    || observation
                        .default_branch_head
                        .as_deref()
                        .is_some_and(|head| !is_commit_id(head))
                {
                    failures += 1;
                    push_bounded(
                        &mut findings,
                        limit,
                        DoctorFinding {
                            subject: checkout.repository.clone(),
                            message: "Git returned a malformed commit identity".to_owned(),
                        },
                    );
                }
                if observation.default_branch_head.is_none() {
                    warnings += 1;
                    push_bounded(
                        &mut findings,
                        limit,
                        DoctorFinding {
                            subject: checkout.repository.clone(),
                            message: format!(
                                "local origin/{} commit is not observable without fetching; no fetch was attempted",
                                checkout.default_branch
                            ),
                        },
                    );
                }
                reconcile_run_git_state(
                    checkout,
                    &observation,
                    &mut failures,
                    &mut warnings,
                    &mut findings,
                    limit,
                );
            }
            Err(error) => {
                warnings += 1;
                push_bounded(
                    &mut findings,
                    limit,
                    DoctorFinding {
                        subject: checkout.repository.clone(),
                        message: error.to_string(),
                    },
                );
            }
        }
    }

    for pull_request in pull_requests {
        match observer.observe_pull_request(pull_request) {
            Ok(observation) => {
                observed += 1;
                let subject = format!("{}#{}", pull_request.repository, pull_request.number);
                if observation.number != pull_request.number
                    || observation.url != pull_request.url
                    || observation.head != pull_request.head
                {
                    failures += 1;
                    push_bounded(
                        &mut findings,
                        limit,
                        DoctorFinding {
                            subject: subject.clone(),
                            message: format!(
                                "observed number/url/head ({}, {:?}, {}) do not match durable evidence ({}, {:?}, {})",
                                observation.number,
                                observation.url,
                                observation.head,
                                pull_request.number,
                                pull_request.url,
                                pull_request.head
                            ),
                        },
                    );
                }
                if pull_request.publication_state == "draft" && !observation.draft {
                    warnings += 1;
                    push_bounded(
                        &mut findings,
                        limit,
                        DoctorFinding {
                            subject: subject.clone(),
                            message: "GitHub reports ready while durable publication evidence remains draft; repair was not attempted".to_owned(),
                        },
                    );
                }
                if pull_request.publication_state == "ready_pending" && !observation.draft {
                    warnings += 1;
                    push_bounded(
                        &mut findings,
                        limit,
                        DoctorFinding {
                            subject: subject.clone(),
                            message: "GitHub reports ready while the durable ready observation is absent; external completion is ambiguous and was not adopted".to_owned(),
                        },
                    );
                }
                if pull_request.publication_state == "ready" && observation.draft {
                    failures += 1;
                    push_bounded(
                        &mut findings,
                        limit,
                        DoctorFinding {
                            subject: subject.clone(),
                            message: "GitHub reports draft but durable evidence reports ready"
                                .to_owned(),
                        },
                    );
                }
                if observation.state != "open" {
                    warnings += 1;
                    push_bounded(
                        &mut findings,
                        limit,
                        DoctorFinding {
                            subject,
                            message: format!(
                                "GitHub reports pull request state {:?}; no lifecycle state was adopted",
                                observation.state
                            ),
                        },
                    );
                }
            }
            Err(error) => {
                warnings += 1;
                push_bounded(
                    &mut findings,
                    limit,
                    DoctorFinding {
                        subject: format!("{}#{}", pull_request.repository, pull_request.number),
                        message: error.to_string(),
                    },
                );
            }
        }
    }

    let total_findings = failures + warnings;
    DoctorCheck {
        code: "gardener.external_reconciliation".to_owned(),
        status: if failures > 0 {
            DoctorStatus::Fail
        } else if warnings > 0 {
            DoctorStatus::Warning
        } else {
            DoctorStatus::Pass
        },
        summary: format!(
            "observed {observed} external object(s); found {failures} mismatch(es) and {warnings} ambiguous or unavailable observation(s)"
        ),
        findings,
        findings_truncated: total_findings > limit,
    }
}

fn reconcile_run_git_state(
    checkout: &RegisteredCheckout,
    observation: &CheckoutObservation,
    failures: &mut usize,
    warnings: &mut usize,
    findings: &mut Vec<DoctorFinding>,
    limit: usize,
) {
    let local = refs_by_name(&observation.local_branches);
    let cached = refs_by_name(&observation.cached_remote_branches);
    let live = match &observation.live_remote_branches {
        Ok(refs) => Some(refs_by_name(refs)),
        Err(error) => {
            *warnings += 1;
            push_bounded(
                findings,
                limit,
                DoctorFinding {
                    subject: checkout.repository.clone(),
                    message: format!(
                        "live remote branches are not observable: {error}; cached refs were not treated as live authority"
                    ),
                },
            );
            None
        }
    };
    let expected_branches = checkout
        .runs
        .iter()
        .map(|run| run.branch.as_str())
        .collect::<Vec<_>>();

    for run in &checkout.runs {
        let implementation =
            observed_worktree(&observation.worktrees, &run.implementation_worktree_path);
        reconcile_worktree(
            run,
            "implementation",
            implementation,
            run.git_commit.as_deref().unwrap_or(&run.source_commit),
            failures,
            warnings,
            findings,
            limit,
        );
        if let Some(path) = &run.verification_worktree_path {
            reconcile_worktree(
                run,
                "verification",
                observed_worktree(&observation.worktrees, path),
                run.verification_head
                    .as_deref()
                    .or(run.git_commit.as_deref())
                    .unwrap_or(&run.source_commit),
                failures,
                warnings,
                findings,
                limit,
            );
        }

        let local_name = format!("refs/heads/{}", run.branch);
        let cached_name = format!("refs/remotes/origin/{}", run.branch);
        let live_name = local_name.clone();
        let expected_local_head = run.git_commit.as_deref().unwrap_or(&run.source_commit);
        match local.get(local_name.as_str()) {
            Some(head) if *head != expected_local_head => add_mismatch(
                failures,
                findings,
                limit,
                &run.run_id,
                format!(
                    "local branch {} head {} does not match durable head {}",
                    run.branch, head, expected_local_head
                ),
            ),
            Some(_) if !run.active => add_warning(
                warnings,
                findings,
                limit,
                &run.run_id,
                format!(
                    "stale local branch {} remains after the run stopped",
                    run.branch
                ),
            ),
            None if run.active => add_warning(
                warnings,
                findings,
                limit,
                &run.run_id,
                format!("active run local branch {} is missing", run.branch),
            ),
            _ => {}
        }

        let cached_head = cached.get(cached_name.as_str()).copied();
        if let (Some(cached_head), Some(expected)) = (cached_head, run.pushed_head.as_deref()) {
            if cached_head != expected {
                add_warning(
                    warnings,
                    findings,
                    limit,
                    &run.run_id,
                    format!(
                        "cached remote-tracking branch {} head {} differs from durable pushed head {}; cached evidence is not live authority",
                        run.branch, cached_head, expected
                    ),
                );
            }
        }

        if let Some(live) = &live {
            let live_head = live.get(live_name.as_str()).copied();
            match (run.pushed_head.as_deref(), live_head) {
                (Some(expected), Some(head)) if expected != head => add_mismatch(
                    failures,
                    findings,
                    limit,
                    &run.run_id,
                    format!(
                        "live remote branch {} head {} does not match durable pushed head {}",
                        run.branch, head, expected
                    ),
                ),
                (Some(_), None) => {
                    if let Some(cached_head) = cached_head {
                        add_warning(
                            warnings,
                            findings,
                            limit,
                            &run.run_id,
                            format!(
                                "branch {} exists only as cached remote-tracking ref at {}; the live remote branch is missing",
                                run.branch, cached_head
                            ),
                        );
                    } else {
                        add_mismatch(
                            failures,
                            findings,
                            limit,
                            &run.run_id,
                            format!(
                                "durably pushed live remote branch {} is missing",
                                run.branch
                            ),
                        );
                    }
                }
                (None, Some(head)) => add_warning(
                    warnings,
                    findings,
                    limit,
                    &run.run_id,
                    format!(
                        "live remote branch {} exists at {} without a durable push observation",
                        run.branch, head
                    ),
                ),
                _ => {}
            }
        }
    }

    for reference in &observation.local_branches {
        if let Some(branch) = reference.name.strip_prefix("refs/heads/codex/gardener-") {
            let full = format!("codex/gardener-{branch}");
            if !expected_branches.contains(&full.as_str()) {
                add_warning(
                    warnings,
                    findings,
                    limit,
                    &checkout.repository,
                    format!(
                        "unowned local gardener branch {full} exists at {}",
                        reference.head
                    ),
                );
            }
        }
    }
    for reference in &observation.cached_remote_branches {
        if let Some(branch) = reference
            .name
            .strip_prefix("refs/remotes/origin/codex/gardener-")
        {
            let full = format!("codex/gardener-{branch}");
            if !expected_branches.contains(&full.as_str()) {
                add_warning(
                    warnings,
                    findings,
                    limit,
                    &checkout.repository,
                    format!(
                        "unowned cached gardener branch {full} exists at {}; it is not live authority",
                        reference.head
                    ),
                );
            }
        }
    }
    if let Some(live) = &live {
        for (name, head) in live {
            if let Some(branch) = name.strip_prefix("refs/heads/codex/gardener-") {
                let full = format!("codex/gardener-{branch}");
                if !expected_branches.contains(&full.as_str()) {
                    add_warning(
                        warnings,
                        findings,
                        limit,
                        &checkout.repository,
                        format!("unowned live gardener branch {full} exists at {head}"),
                    );
                }
            }
        }
    }
    for worktree in &observation.worktrees {
        let Some(branch) = worktree
            .branch
            .as_deref()
            .and_then(|branch| branch.strip_prefix("refs/heads/"))
        else {
            continue;
        };
        if branch.starts_with("codex/gardener-") && !expected_branches.contains(&branch) {
            add_warning(
                warnings,
                findings,
                limit,
                &checkout.repository,
                format!(
                    "unowned gardener worktree {:?} is attached to branch {branch}",
                    worktree.path
                ),
            );
        }
    }
}

fn refs_by_name(refs: &[ObservedRef]) -> std::collections::BTreeMap<&str, &str> {
    refs.iter()
        .map(|reference| (reference.name.as_str(), reference.head.as_str()))
        .collect()
}

fn observed_worktree<'a>(
    worktrees: &'a [ObservedWorktree],
    expected_path: &Path,
) -> Option<&'a ObservedWorktree> {
    let expected =
        std::fs::canonicalize(expected_path).unwrap_or_else(|_| expected_path.to_owned());
    worktrees.iter().find(|worktree| {
        std::fs::canonicalize(&worktree.path).unwrap_or_else(|_| worktree.path.clone()) == expected
    })
}

#[allow(clippy::too_many_arguments)]
fn reconcile_worktree(
    run: &RegisteredRunCheckout,
    kind: &str,
    observed: Option<&ObservedWorktree>,
    expected_head: &str,
    failures: &mut usize,
    warnings: &mut usize,
    findings: &mut Vec<DoctorFinding>,
    limit: usize,
) {
    match observed {
        Some(worktree) if worktree.head.as_deref() != Some(expected_head) => add_mismatch(
            failures,
            findings,
            limit,
            &run.run_id,
            format!(
                "{kind} worktree {:?} head {:?} does not match durable head {expected_head}",
                worktree.path, worktree.head
            ),
        ),
        Some(worktree) if !run.active => add_warning(
            warnings,
            findings,
            limit,
            &run.run_id,
            format!(
                "stale {kind} worktree {:?} remains after the run stopped",
                worktree.path
            ),
        ),
        None if run.active => add_warning(
            warnings,
            findings,
            limit,
            &run.run_id,
            format!("active run {kind} worktree is missing"),
        ),
        _ => {}
    }
}

fn add_mismatch(
    failures: &mut usize,
    findings: &mut Vec<DoctorFinding>,
    limit: usize,
    subject: &str,
    message: String,
) {
    *failures += 1;
    push_bounded(
        findings,
        limit,
        DoctorFinding {
            subject: subject.to_owned(),
            message,
        },
    );
}

fn add_warning(
    warnings: &mut usize,
    findings: &mut Vec<DoctorFinding>,
    limit: usize,
    subject: &str,
    message: String,
) {
    *warnings += 1;
    push_bounded(
        findings,
        limit,
        DoctorFinding {
            subject: subject.to_owned(),
            message,
        },
    );
}

fn origin_matches_repository(origin: &str, repository: &str) -> bool {
    matches!(
        origin.trim(),
        value if value == format!("https://github.com/{repository}")
            || value == format!("https://github.com/{repository}.git")
    )
}

fn valid_github_repository(repository: &str) -> bool {
    let Some((owner, name)) = repository.split_once('/') else {
        return false;
    };
    !owner.is_empty()
        && !name.is_empty()
        && !name.contains('/')
        && owner
            .bytes()
            .chain(name.bytes())
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn is_commit_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn summarise(checks: &[DoctorCheck]) -> DoctorSummary {
    let mut summary = DoctorSummary {
        passed: 0,
        warnings: 0,
        failed: 0,
        skipped: 0,
        healthy: false,
    };
    for check in checks {
        match check.status {
            DoctorStatus::Pass => summary.passed += 1,
            DoctorStatus::Warning => summary.warnings += 1,
            DoctorStatus::Fail => summary.failed += 1,
            DoctorStatus::Skipped => summary.skipped += 1,
        }
    }
    summary.healthy = summary.failed == 0;
    summary
}

const ATTEMPT_EXACTNESS_SQL: &str = r#"
WITH incomplete AS (
    SELECT * FROM attempts WHERE completed_at IS NULL OR outcome = 'running'
), invalid_attempts AS (
    SELECT 'attempt:' || id AS subject,
           'attempt completion timestamp and outcome disagree' AS message
    FROM attempts
    WHERE (completed_at IS NULL) <> (outcome = 'running')
), invalid_running AS (
    SELECT o.id AS subject,
           'running obligation does not own exactly one matching incomplete attempt' AS message
    FROM obligations o
    WHERE o.state = 'running'
      AND (
        (SELECT count(*) FROM incomplete a WHERE a.obligation_id = o.id) <> 1
        OR
        (SELECT count(*) FROM incomplete a
         WHERE a.obligation_id = o.id
           AND a.occurrence = o.occurrence
           AND a.attempt_number = o.attempts_made
           AND a.lease_generation = o.lease_generation
           AND a.lease_token = o.lease_token
           AND a.completed_at IS NULL AND a.outcome = 'running') <> 1
      )
), invalid_nonrunning AS (
    SELECT o.id AS subject,
           'non-running obligation retains an incomplete attempt' AS message
    FROM obligations o
    WHERE o.state <> 'running'
      AND EXISTS (SELECT 1 FROM incomplete a WHERE a.obligation_id = o.id)
)
SELECT subject, message FROM invalid_attempts
UNION ALL SELECT subject, message FROM invalid_running
UNION ALL SELECT subject, message FROM invalid_nonrunning
ORDER BY subject
"#;

const AUDIT_PROJECTION_SQL: &str = r#"
WITH latest AS (
    SELECT a.* FROM audit_events a
    JOIN (
        SELECT obligation_id, max(sequence) AS sequence
        FROM audit_events GROUP BY obligation_id
    ) newest ON newest.sequence = a.sequence
)
SELECT o.id AS subject,
       CASE
         WHEN latest.sequence IS NULL THEN 'obligation has no audit event'
         WHEN latest.occurrence <> o.occurrence THEN 'latest audit occurrence disagrees with the projection'
         ELSE 'latest audit state disagrees with the projection'
       END AS message
FROM obligations o
LEFT JOIN latest ON latest.obligation_id = o.id
WHERE latest.sequence IS NULL
   OR latest.occurrence <> o.occurrence
   OR latest.to_state <> o.state
ORDER BY o.id
"#;

const GARDENER_PHASE_EVIDENCE_SQL: &str = r#"
WITH run_shape AS (
    SELECT r.id AS subject,
           'run phase does not match the greatest durable evidence phase' AS message
    FROM gardener_implementation_runs r
    WHERE (CASE r.phase
        WHEN 'created' THEN 0
        WHEN 'implementation_thread_recorded' THEN 1
        WHEN 'implementation_turn_recorded' THEN 2
        WHEN 'implementation_finished' THEN 3
        WHEN 'git_commit_recorded' THEN 4
        WHEN 'push_observed' THEN 5
        WHEN 'pull_request_ready' THEN 6
        WHEN 'verification_started' THEN 7
        WHEN 'verification_thread_recorded' THEN 8
        WHEN 'verification_turn_recorded' THEN 9
        WHEN 'verification_finished' THEN 10 ELSE -1 END)
      <> (CASE
        WHEN r.verification_verdict IS NOT NULL THEN 10
        WHEN r.verification_turn_id IS NOT NULL THEN 9
        WHEN r.verification_thread_id IS NOT NULL THEN 8
        WHEN r.verification_head IS NOT NULL THEN 7
        WHEN r.pull_request_head IS NOT NULL THEN 6
        WHEN r.pushed_head IS NOT NULL THEN 5
        WHEN r.git_commit IS NOT NULL THEN 4
        WHEN r.implementation_final_message_json IS NOT NULL THEN 3
        WHEN r.implementation_turn_id IS NOT NULL THEN 2
        WHEN r.implementation_thread_id IS NOT NULL THEN 1 ELSE 0 END)
), inspection_shape AS (
    SELECT i.id AS subject,
           'inspection thread, turn, result or completion evidence is out of order' AS message
    FROM gardener_inspections i
    WHERE (i.codex_turn_id IS NOT NULL AND i.codex_thread_id IS NULL)
       OR (i.result_json IS NOT NULL AND i.codex_turn_id IS NULL)
       OR ((i.result_json IS NULL) <> (i.completed_at IS NULL))
       OR (i.result_json IS NOT NULL AND
           (json_valid(i.result_json) = 0 OR json_type(i.result_json) <> 'object'))
), reproducibility_shape AS (
    SELECT r.id AS subject,
           'advanced run lacks matching reproducibility evidence' AS message
    FROM gardener_implementation_runs r
    LEFT JOIN gardener_run_reproducibility m ON m.run_id = r.id
    WHERE r.phase <> 'created'
      AND (m.run_id IS NULL OR lower(m.source_commit) <> lower(r.source_commit))
), qualification_shape AS (
    SELECT r.id AS subject,
           'publication phase lacks an exact-head candidate qualification' AS message
    FROM gardener_implementation_runs r
    LEFT JOIN gardener_candidate_qualifications q ON q.run_id = r.id
    WHERE r.phase IN ('push_observed', 'pull_request_ready', 'verification_started',
                      'verification_thread_recorded', 'verification_turn_recorded',
                      'verification_finished')
      AND (q.run_id IS NULL OR q.head <> r.git_commit)
    UNION ALL
    SELECT r.id, 'candidate qualification precedes durable Git commit evidence'
    FROM gardener_implementation_runs r
    JOIN gardener_candidate_qualifications q ON q.run_id = r.id
    WHERE r.git_commit IS NULL OR q.head <> r.git_commit
), ready_shape AS (
    SELECT r.id AS subject,
           'pull-request ready observation does not match the durable run identity' AS message
    FROM gardener_pull_request_ready_observations ready
    JOIN gardener_implementation_runs r ON r.id = ready.run_id
    WHERE ready.number <> r.pull_request_number
       OR ready.url <> r.pull_request_url
       OR ready.head <> r.pull_request_head
       OR r.publication_state <> 'ready_pending'
       OR r.verification_verdict <> 'pass'
), publication_shape AS (
    SELECT r.id AS subject,
           'publication state does not match pull-request evidence' AS message
    FROM gardener_implementation_runs r
    WHERE (r.publication_state = 'not_created' AND r.pull_request_number IS NOT NULL)
       OR (r.publication_state IN ('draft', 'ready_pending', 'ready')
           AND r.pull_request_number IS NULL)
       OR (r.publication_state = 'ready_pending' AND r.verification_verdict <> 'pass')
       OR (r.publication_state = 'ready' AND
           (r.pull_request_ready_at IS NULL OR r.verification_verdict <> 'pass'))
)
SELECT subject, message FROM run_shape
UNION ALL SELECT subject, message FROM inspection_shape
UNION ALL SELECT subject, message FROM reproducibility_shape
UNION ALL SELECT subject, message FROM qualification_shape
UNION ALL SELECT subject, message FROM ready_shape
UNION ALL SELECT subject, message FROM publication_shape
ORDER BY subject
"#;

const GARDENER_SOURCE_CHAIN_SQL: &str = r#"
WITH ranked AS (
    SELECT id, row_number() OVER (
        PARTITION BY proposal_fingerprint ORDER BY source_observation_id, source_commit
    ) AS expected_generation
    FROM gardener_proposal_instances
), invalid_instances AS (
    SELECT pi.id AS subject,
           'proposal instance identity, source observation, generation or obligation is inconsistent' AS message
    FROM gardener_proposal_instances pi
    JOIN gardener_proposals p ON p.fingerprint = pi.proposal_fingerprint
    LEFT JOIN gardener_proposal_observations po ON po.id = pi.source_observation_id
    LEFT JOIN gardener_inspections i ON i.id = pi.source_inspection_id
    LEFT JOIN gardener_obligation_bindings b
      ON b.obligation_id = pi.implementation_obligation_id
    JOIN ranked ON ranked.id = pi.id
    WHERE pi.id <> 'pi:' || pi.proposal_fingerprint || ':' || lower(pi.source_commit) || ':' || pi.generation
       OR po.id IS NULL
       OR po.proposal_fingerprint <> pi.proposal_fingerprint
       OR lower(po.source_commit) <> pi.source_commit
       OR po.inspection_id <> pi.source_inspection_id
       OR pi.source_observation_id <> (
           SELECT min(first_po.id) FROM gardener_proposal_observations first_po
           WHERE first_po.proposal_fingerprint = pi.proposal_fingerprint
             AND lower(first_po.source_commit) = pi.source_commit
       )
       OR i.id IS NULL OR lower(i.source_commit) <> pi.source_commit
       OR ranked.expected_generation <> pi.generation
       OR b.obligation_id IS NULL OR b.kind <> 'implementation'
       OR (pi.generation = 1 AND p.implementation_obligation_id <> pi.implementation_obligation_id)
), invalid_observations AS (
    SELECT 'observation:' || po.id AS subject,
           'proposal observation does not map exactly to its source instance' AS message
    FROM gardener_proposal_observations po
    LEFT JOIN gardener_proposal_observation_instances oi ON oi.observation_id = po.id
    LEFT JOIN gardener_proposal_instances pi ON pi.id = oi.instance_id
    WHERE oi.observation_id IS NULL
       OR pi.proposal_fingerprint <> po.proposal_fingerprint
       OR pi.source_commit <> lower(po.source_commit)
), invalid_supersessions AS (
    SELECT pi.id AS subject,
           'proposal generation does not have the exact consecutive supersession shape' AS message
    FROM gardener_proposal_instances pi
    LEFT JOIN gardener_proposal_instance_supersessions s
      ON s.superseded_instance_id = pi.id
    LEFT JOIN gardener_proposal_instances next ON next.id = s.superseding_instance_id
    WHERE (EXISTS (
             SELECT 1 FROM gardener_proposal_instances later
             WHERE later.proposal_fingerprint = pi.proposal_fingerprint
               AND later.generation = pi.generation + 1
           ) AND (next.id IS NULL
                  OR next.proposal_fingerprint <> pi.proposal_fingerprint
                  OR next.generation <> pi.generation + 1))
       OR (NOT EXISTS (
             SELECT 1 FROM gardener_proposal_instances later
             WHERE later.proposal_fingerprint = pi.proposal_fingerprint
               AND later.generation = pi.generation + 1
           ) AND s.superseded_instance_id IS NOT NULL)
)
SELECT subject, message FROM invalid_instances
UNION ALL SELECT subject, message FROM invalid_observations
UNION ALL SELECT subject, message FROM invalid_supersessions
ORDER BY subject
"#;

const GARDENER_DECISION_CHAIN_SQL: &str = r#"
WITH invalid_decisions AS (
    SELECT 'approval:' || d.approval_id AS subject,
           'exact proposal decision does not match its approval and source instance' AS message
    FROM gardener_proposal_instance_decisions d
    LEFT JOIN approvals a ON a.id = d.approval_id
    LEFT JOIN gardener_proposal_instances pi ON pi.id = d.instance_id
    WHERE a.id IS NULL OR pi.id IS NULL
       OR d.proposal_fingerprint <> pi.proposal_fingerprint
       OR d.source_commit <> pi.source_commit
       OR d.generation <> pi.generation
       OR d.obligation_id <> pi.implementation_obligation_id
       OR d.obligation_id <> a.obligation_id
       OR d.occurrence <> a.occurrence
       OR d.decision <> a.decision
), actionable_without_authority AS (
    SELECT pi.id AS subject,
           'actionable proposal instance lacks a latest exact approval' AS message
    FROM gardener_proposal_instances pi
    JOIN obligations o ON o.id = pi.implementation_obligation_id
    WHERE o.state IN ('pending', 'retry_scheduled', 'running')
      AND NOT EXISTS (
        SELECT 1 FROM gardener_proposal_instance_decisions d
        WHERE d.instance_id = pi.id
          AND d.obligation_id = o.id
          AND d.occurrence = o.occurrence
          AND d.decision = 'approved'
          AND d.approval_id = (
            SELECT max(a.id) FROM approvals a
            WHERE a.obligation_id = o.id AND a.occurrence = o.occurrence
          )
      )
)
SELECT subject, message FROM invalid_decisions
UNION ALL SELECT subject, message FROM actionable_without_authority
ORDER BY subject
"#;

const GARDENER_RUN_CHAIN_SQL: &str = r#"
WITH invalid_runs AS (
    SELECT r.id AS subject,
           'run does not match its proposal instance, obligation attempt or exact approval' AS message
    FROM gardener_implementation_runs r
    LEFT JOIN gardener_implementation_run_instances ri ON ri.run_id = r.id
    LEFT JOIN gardener_proposal_instances pi ON pi.id = ri.instance_id
    LEFT JOIN attempts a
      ON a.obligation_id = r.obligation_id
     AND a.occurrence = r.occurrence
     AND a.attempt_number = r.attempt_number
     AND a.lease_generation = r.lease_generation
     AND a.lease_token = r.lease_token
    WHERE ri.run_id IS NULL OR pi.id IS NULL OR a.id IS NULL
       OR ri.proposal_fingerprint <> r.proposal_fingerprint
       OR ri.proposal_fingerprint <> pi.proposal_fingerprint
       OR ri.source_commit <> lower(r.source_commit)
       OR ri.source_commit <> pi.source_commit
       OR ri.generation <> pi.generation
       OR r.obligation_id <> pi.implementation_obligation_id
       OR NOT EXISTS (
           SELECT 1 FROM gardener_proposal_instance_decisions d
           WHERE d.instance_id = pi.id
             AND d.obligation_id = r.obligation_id
             AND d.occurrence = r.occurrence
             AND d.decision = 'approved'
             AND d.recorded_at <= r.created_at
             AND d.approval_id = (
                 SELECT max(a2.id) FROM approvals a2
                 WHERE a2.obligation_id = r.obligation_id
                   AND a2.occurrence = r.occurrence
                   AND a2.decided_at <= r.created_at
             )
       )
)
SELECT subject, message FROM invalid_runs ORDER BY subject
"#;

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, fs, sync::Mutex};

    use rusqlite::params;
    use sha2::Digest;
    use tempfile::TempDir;

    use super::*;
    use crate::{
        NewObligation, NewRepositoryRegistration, Recurrence, RetryPolicy, Store,
        migration_manifest,
    };

    fn check<'a>(report: &'a DoctorReport, code: &str) -> &'a DoctorCheck {
        report
            .checks
            .iter()
            .find(|check| check.code == code)
            .unwrap_or_else(|| panic!("missing check {code}"))
    }

    fn migrated_database() -> (TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bokkie.sqlite3");
        drop(Store::open(&path).unwrap());
        (directory, path)
    }

    fn report(path: &Path, observed_at: i64) -> DoctorReport {
        run_doctor(
            path,
            migration_manifest(),
            &NoExternalObserver,
            DoctorOptions::at(observed_at),
        )
        .unwrap()
    }

    #[test]
    fn healthy_database_passes_without_mutation() {
        let (_directory, path) = migrated_database();
        let before = fs::read(&path).unwrap();

        let report = report(&path, 1_000);

        assert!(report.summary.healthy, "{report:#?}");
        assert!(!report.repair_performed);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["repair_performed"], false);
        assert_eq!(json["format_version"], REPORT_FORMAT_VERSION);
        assert_eq!(
            check(&report, "schema.migration_manifest").status,
            DoctorStatus::Pass
        );
        assert_eq!(
            check(&report, "gardener.external_reconciliation").status,
            DoctorStatus::Skipped
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn incompatible_schema_is_reported_without_aborting_other_checks() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unknown.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute("CREATE TABLE unrelated(value TEXT)", [])
            .unwrap();
        drop(connection);
        let before = fs::read(&path).unwrap();

        let report = report(&path, 1_000);

        assert_eq!(
            check(&report, "sqlite.quick_check").status,
            DoctorStatus::Pass
        );
        assert_eq!(
            check(&report, "schema.migration_manifest").status,
            DoctorStatus::Fail
        );
        assert_eq!(
            check(&report, "obligations.attempt_exactness").status,
            DoctorStatus::Fail
        );
        assert!(!report.summary.healthy);
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn detects_attempt_projection_liveness_and_manifest_corruption() {
        let (_directory, path) = migrated_database();
        let mut store = Store::open_compatible(&path).unwrap();
        store
            .create(
                NewObligation {
                    id: "damaged".to_owned(),
                    description: "damaged fixture".to_owned(),
                    scheduled_at: 900,
                    recurrence: None,
                    approval_required: false,
                    retry: RetryPolicy::default(),
                },
                800,
            )
            .unwrap();
        drop(store);

        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA ignore_check_constraints = ON;
                 DROP TRIGGER schema_migrations_immutable_update;
                 UPDATE schema_migrations SET sha256 = lower(hex(randomblob(32))) WHERE version = 1;
                 UPDATE obligations
                    SET state = 'running', next_wake_at = NULL, attempts_made = 1,
                        lease_generation = 1, lease_token = 'orphaned', lease_expires_at = 900
                  WHERE id = 'damaged';",
            )
            .unwrap();
        drop(connection);

        let report = report(&path, 1_000);

        for code in [
            "schema.migration_manifest",
            "obligations.attempt_exactness",
            "obligations.audit_projection",
            "obligations.nonterminal_liveness",
        ] {
            assert_eq!(check(&report, code).status, DoctorStatus::Fail, "{code}");
        }
        assert!(!report.repair_performed);
    }

    #[test]
    fn running_liveness_uses_the_exact_lease_expiry_boundary() {
        let (_directory, path) = migrated_database();
        let mut store = Store::open_compatible(&path).unwrap();
        store
            .create(
                NewObligation {
                    id: "leased".to_owned(),
                    description: "leased fixture".to_owned(),
                    scheduled_at: 900,
                    recurrence: None,
                    approval_required: false,
                    retry: RetryPolicy::default(),
                },
                800,
            )
            .unwrap();
        store.claim_due(900, 200, 1).unwrap();
        drop(store);

        assert_eq!(
            check(&report(&path, 1_099), "obligations.nonterminal_liveness").status,
            DoctorStatus::Pass
        );
        assert_eq!(
            check(&report(&path, 1_100), "obligations.nonterminal_liveness").status,
            DoctorStatus::Fail
        );
    }

    #[test]
    fn detects_unmapped_gardener_source_evidence() {
        let (_directory, path) = migrated_database();
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "foreign_keys", false)
            .unwrap();
        connection
            .execute(
                "INSERT INTO gardener_proposal_observations(
                    proposal_fingerprint, inspection_id, source_commit, observed_at
                 ) VALUES (?1, 'missing-inspection', ?2, 1000)",
                params!["f".repeat(64), "a".repeat(40)],
            )
            .unwrap();
        drop(connection);

        let report = report(&path, 1_001);

        assert_eq!(
            check(&report, "sqlite.foreign_keys").status,
            DoctorStatus::Fail
        );
        assert_eq!(
            check(&report, "gardener.source_chain").status,
            DoctorStatus::Fail
        );
    }

    #[derive(Default)]
    struct RecordingExecutor {
        invocations: Mutex<Vec<ReadOnlyInvocation>>,
        outputs: Mutex<VecDeque<Result<ReadOnlyCommandOutput, ObservationError>>>,
    }

    impl RecordingExecutor {
        fn with_outputs(outputs: impl IntoIterator<Item = &'static str>) -> Self {
            Self {
                invocations: Mutex::new(Vec::new()),
                outputs: Mutex::new(
                    outputs
                        .into_iter()
                        .map(|stdout| {
                            Ok(ReadOnlyCommandOutput {
                                stdout: stdout.to_owned(),
                            })
                        })
                        .collect(),
                ),
            }
        }
    }

    impl ReadOnlyCommandExecutor for RecordingExecutor {
        fn execute(
            &self,
            invocation: &ReadOnlyInvocation,
        ) -> Result<ReadOnlyCommandOutput, ObservationError> {
            self.invocations.lock().unwrap().push(invocation.clone());
            self.outputs
                .lock()
                .unwrap()
                .pop_front()
                .expect("fixture supplied an output for each invocation")
        }
    }

    #[derive(Default)]
    struct LocalGitExecutor;

    impl ReadOnlyCommandExecutor for LocalGitExecutor {
        fn execute(
            &self,
            invocation: &ReadOnlyInvocation,
        ) -> Result<ReadOnlyCommandOutput, ObservationError> {
            if invocation
                .arguments
                .iter()
                .any(|argument| argument == "ls-remote")
            {
                return Ok(ReadOnlyCommandOutput {
                    stdout: String::new(),
                });
            }
            SystemReadOnlyCommandExecutor.execute(invocation)
        }
    }

    #[derive(Default)]
    struct NeutralLiveRecordingExecutor {
        live_invocation: Mutex<Option<ReadOnlyInvocation>>,
    }

    impl ReadOnlyCommandExecutor for NeutralLiveRecordingExecutor {
        fn execute(
            &self,
            invocation: &ReadOnlyInvocation,
        ) -> Result<ReadOnlyCommandOutput, ObservationError> {
            if invocation
                .arguments
                .iter()
                .any(|argument| argument == "ls-remote")
            {
                *self.live_invocation.lock().unwrap() = Some(invocation.clone());
                return Ok(ReadOnlyCommandOutput {
                    stdout: String::new(),
                });
            }
            SystemReadOnlyCommandExecutor.execute(invocation)
        }
    }

    #[test]
    fn command_observer_uses_only_the_exact_read_only_allowlist() {
        let executor = Arc::new(RecordingExecutor::with_outputs([
            "/srv/bokkie",
            "https://github.com/robchristie/bokkie.git",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "worktree /srv/bokkie\0HEAD aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0branch refs/heads/main\0\0",
            "refs/heads/main\0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nrefs/remotes/origin/main\0bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\trefs/heads/main",
            r#"{"number":9,"html_url":"https://github.com/robchristie/bokkie/pull/9","head":{"sha":"cccccccccccccccccccccccccccccccccccccccc"},"state":"open","draft":false}"#,
        ]));
        let observer = CommandExternalObserver::with_executor(
            "/usr/bin/git",
            "/usr/bin/curl",
            Duration::from_secs(7),
            executor.clone(),
        )
        .unwrap();

        observer
            .observe_checkout(&RegisteredCheckout {
                repository: "robchristie/bokkie".to_owned(),
                default_branch: "main".to_owned(),
                checkout_path: PathBuf::from("/srv/bokkie"),
                runs: Vec::new(),
            })
            .unwrap();
        observer
            .observe_pull_request(&RegisteredPullRequest {
                run_id: "run-9".to_owned(),
                repository: "robchristie/bokkie".to_owned(),
                number: 9,
                url: "https://github.com/robchristie/bokkie/pull/9".to_owned(),
                head: "c".repeat(40),
                publication_state: "ready".to_owned(),
            })
            .unwrap();

        let invocations = executor.invocations.lock().unwrap();
        assert_eq!(invocations.len(), 8);
        let git_prefix = [
            "-c",
            "credential.helper=",
            "-c",
            "core.hooksPath=/dev/null",
            "-C",
            "/srv/bokkie",
        ];
        let expected_git_suffixes: [&[&str]; 6] = [
            &["rev-parse", "--show-toplevel"],
            &[
                "config",
                "--local",
                "--no-includes",
                "--get-all",
                "remote.origin.url",
            ],
            &["rev-parse", "--verify", "HEAD^{commit}"],
            &["rev-parse", "--verify", "refs/remotes/origin/main^{commit}"],
            &["worktree", "list", "--porcelain", "-z"],
            &[
                "for-each-ref",
                "--format=%(refname)%00%(objectname)",
                "refs/heads",
                "refs/remotes/origin",
            ],
        ];
        for (invocation, suffix) in invocations.iter().take(6).zip(expected_git_suffixes) {
            assert_eq!(invocation.program, Path::new("/usr/bin/git"));
            assert_eq!(
                invocation.arguments,
                git_prefix
                    .into_iter()
                    .chain(suffix.iter().copied())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                invocation.environment,
                [
                    ("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned()),
                    ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
                    ("GIT_OPTIONAL_LOCKS".to_owned(), "0".to_owned()),
                    ("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()),
                ]
            );
            assert_eq!(invocation.current_directory, None);
            assert_eq!(invocation.timeout, Duration::from_secs(7));
        }
        assert_eq!(invocations[6].program, Path::new("/usr/bin/git"));
        assert_eq!(
            invocations[6].arguments,
            [
                "-c",
                "credential.helper=",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.https.allow=always",
                "ls-remote",
                "--heads",
                "https://github.com/robchristie/bokkie.git",
            ]
            .map(str::to_owned)
        );
        assert_eq!(
            invocations[6].environment,
            [
                ("GIT_ASKPASS".to_owned(), "/bin/false".to_owned()),
                ("GIT_CEILING_DIRECTORIES".to_owned(), "/".to_owned()),
                ("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned()),
                ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
                ("GIT_OPTIONAL_LOCKS".to_owned(), "0".to_owned()),
                ("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()),
                ("SSH_ASKPASS".to_owned(), "/bin/false".to_owned()),
            ]
        );
        assert_eq!(invocations[6].current_directory, Some(PathBuf::from("/")));
        assert_eq!(invocations[6].timeout, Duration::from_secs(7));
        assert_eq!(invocations[7].program, Path::new("/usr/bin/curl"));
        assert_eq!(
            invocations[7].arguments,
            [
                "--disable",
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--proto",
                "=https",
                "--max-time",
                "7",
                "--header",
                "Accept: application/vnd.github+json",
                "--header",
                "X-GitHub-Api-Version: 2022-11-28",
                "--user-agent",
                "bokkie-doctor",
                "https://api.github.com/repos/robchristie/bokkie/pulls/9",
            ]
            .map(str::to_owned)
        );
        assert!(invocations[7].environment.is_empty());
        assert_eq!(invocations[7].current_directory, None);
    }

    #[test]
    fn invalid_checkout_identity_stops_before_live_remote_observation() {
        let executor = Arc::new(RecordingExecutor::with_outputs([
            "/srv/bokkie",
            "ext::hostile-transport",
        ]));
        let observer = CommandExternalObserver::with_executor(
            "/usr/bin/git",
            "/usr/bin/curl",
            Duration::from_secs(2),
            executor.clone(),
        )
        .unwrap();

        let error = observer
            .observe_checkout(&RegisteredCheckout {
                repository: "robchristie/bokkie".to_owned(),
                default_branch: "main".to_owned(),
                checkout_path: PathBuf::from("/srv/bokkie"),
                runs: Vec::new(),
            })
            .unwrap_err();

        assert!(matches!(error, ObservationError::Invalid(_)));
        let invocations = executor.invocations.lock().unwrap();
        assert_eq!(invocations.len(), 2);
        assert!(
            invocations
                .iter()
                .all(|invocation| !invocation.arguments.iter().any(|arg| arg == "ls-remote"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn hostile_checkout_transport_config_cannot_reach_live_observation() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let checkout = directory.path().join("checkout");
        fs::create_dir(&checkout).unwrap();
        git(&checkout, &["init", "--initial-branch=main"]);
        git(&checkout, &["config", "user.name", "Doctor fixture"]);
        git(
            &checkout,
            &["config", "user.email", "doctor@example.invalid"],
        );
        fs::write(checkout.join("README.md"), "fixture\n").unwrap();
        git(&checkout, &["add", "README.md"]);
        git(&checkout, &["commit", "-m", "Initialise fixture"]);
        git(
            &checkout,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/robchristie/bokkie.git",
            ],
        );
        git(
            &checkout,
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );

        let sentinel = directory.path().join("transport-sentinel");
        let hostile = directory.path().join("hostile-transport");
        fs::write(
            &hostile,
            format!(
                "#!/bin/sh\nprintf invoked > '{}'\nexit 1\n",
                sentinel.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&hostile).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&hostile, permissions).unwrap();
        git(&checkout, &["config", "protocol.ext.allow", "always"]);
        git(
            &checkout,
            &["config", "core.sshCommand", hostile.to_str().unwrap()],
        );
        git(
            &checkout,
            &[
                "config",
                "credential.helper",
                &format!("!{}", hostile.display()),
            ],
        );
        git(
            &checkout,
            &[
                "config",
                &format!("url.ext::{}.insteadOf", hostile.display()),
                "https://github.com/",
            ],
        );

        let executor = Arc::new(NeutralLiveRecordingExecutor::default());
        let observer = CommandExternalObserver::with_executor(
            "/usr/bin/git",
            "/usr/bin/curl",
            Duration::from_secs(2),
            executor.clone(),
        )
        .unwrap();
        observer
            .observe_checkout(&RegisteredCheckout {
                repository: "robchristie/bokkie".to_owned(),
                default_branch: "main".to_owned(),
                checkout_path: checkout,
                runs: Vec::new(),
            })
            .unwrap();

        assert!(!sentinel.exists());
        let live = executor.live_invocation.lock().unwrap().clone().unwrap();
        assert_eq!(live.current_directory, Some(PathBuf::from("/")));
        assert_eq!(
            live.arguments.last().map(String::as_str),
            Some("https://github.com/robchristie/bokkie.git")
        );
        assert!(!live.arguments.iter().any(|argument| argument == "origin"));
        assert!(!live.arguments.iter().any(|argument| argument == "-C"));
        assert!(
            live.arguments
                .windows(2)
                .any(|pair| pair == ["-c", "protocol.allow=never"])
        );
        assert!(
            live.arguments
                .windows(2)
                .any(|pair| pair == ["-c", "protocol.https.allow=always"])
        );
    }

    struct FixedCheckoutObserver(CheckoutObservation);

    impl ExternalObserver for FixedCheckoutObserver {
        fn observe_checkout(
            &self,
            _checkout: &RegisteredCheckout,
        ) -> Result<CheckoutObservation, ObservationError> {
            Ok(self.0.clone())
        }

        fn observe_pull_request(
            &self,
            _pull_request: &RegisteredPullRequest,
        ) -> Result<PullRequestObservation, ObservationError> {
            unreachable!("classification fixture has no pull requests")
        }
    }

    #[test]
    fn classifies_stale_missing_cached_mismatched_and_unowned_git_facts() {
        let checkout_path = PathBuf::from("/srv/bokkie");
        let first_branch = "codex/gardener-first";
        let second_branch = "codex/gardener-second";
        let source = "a".repeat(40);
        let first_head = "b".repeat(40);
        let second_head = "c".repeat(40);
        let unexpected_head = "d".repeat(40);
        let checkout = RegisteredCheckout {
            repository: "robchristie/bokkie".to_owned(),
            default_branch: "main".to_owned(),
            checkout_path: checkout_path.clone(),
            runs: vec![
                RegisteredRunCheckout {
                    run_id: "stopped-run".to_owned(),
                    implementation_worktree_path: PathBuf::from("/work/first"),
                    verification_worktree_path: Some(PathBuf::from("/work/verify-first")),
                    branch: first_branch.to_owned(),
                    source_commit: source.clone(),
                    git_commit: Some(first_head.clone()),
                    pushed_head: Some(first_head.clone()),
                    verification_head: None,
                    active: false,
                },
                RegisteredRunCheckout {
                    run_id: "active-run".to_owned(),
                    implementation_worktree_path: PathBuf::from("/work/second"),
                    verification_worktree_path: Some(PathBuf::from("/work/verify-second")),
                    branch: second_branch.to_owned(),
                    source_commit: source.clone(),
                    git_commit: Some(second_head.clone()),
                    pushed_head: Some(second_head.clone()),
                    verification_head: Some(second_head.clone()),
                    active: true,
                },
            ],
        };
        let observation = CheckoutObservation {
            canonical_path: checkout_path,
            origin_url: "https://github.com/robchristie/bokkie.git".to_owned(),
            head: source,
            default_branch_head: Some("e".repeat(40)),
            worktrees: vec![
                ObservedWorktree {
                    path: PathBuf::from("/work/first"),
                    head: Some(first_head.clone()),
                    branch: Some(format!("refs/heads/{first_branch}")),
                },
                ObservedWorktree {
                    path: PathBuf::from("/work/verify-first"),
                    head: Some(first_head.clone()),
                    branch: None,
                },
            ],
            local_branches: vec![
                ObservedRef {
                    name: format!("refs/heads/{first_branch}"),
                    head: first_head.clone(),
                },
                ObservedRef {
                    name: "refs/heads/codex/gardener-unowned".to_owned(),
                    head: unexpected_head.clone(),
                },
            ],
            cached_remote_branches: vec![ObservedRef {
                name: format!("refs/remotes/origin/{first_branch}"),
                head: first_head,
            }],
            live_remote_branches: Ok(vec![ObservedRef {
                name: format!("refs/heads/{second_branch}"),
                head: unexpected_head,
            }]),
        };

        let result = check_external_reconciliation(
            &FixedCheckoutObserver(observation),
            &[checkout],
            &[],
            100,
        );

        assert_eq!(result.status, DoctorStatus::Fail);
        let messages = result
            .findings
            .iter()
            .map(|finding| finding.message.as_str())
            .collect::<Vec<_>>();
        for fragment in [
            "stale implementation worktree",
            "stale verification worktree",
            "stale local branch",
            "exists only as cached remote-tracking ref",
            "active run implementation worktree is missing",
            "active run verification worktree is missing",
            "active run local branch",
            "live remote branch codex/gardener-second head",
            "unowned local gardener branch",
        ] {
            assert!(
                messages.iter().any(|message| message.contains(fragment)),
                "missing classification {fragment:?}: {messages:#?}"
            );
        }
    }

    #[test]
    fn real_git_reconciliation_leaves_database_and_repository_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let checkout = directory.path().join("checkout");
        fs::create_dir(&checkout).unwrap();
        git(&checkout, &["init", "--initial-branch=main"]);
        git(&checkout, &["config", "user.name", "Doctor fixture"]);
        git(
            &checkout,
            &["config", "user.email", "doctor@example.invalid"],
        );
        fs::write(checkout.join("README.md"), "fixture\n").unwrap();
        git(&checkout, &["add", "README.md"]);
        git(&checkout, &["commit", "-m", "Initialise fixture"]);
        git(
            &checkout,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/robchristie/bokkie.git",
            ],
        );
        git(
            &checkout,
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );

        let database = directory.path().join("bokkie.sqlite3");
        let mut store = Store::open(&database).unwrap();
        store
            .register_gardener_repository(
                NewRepositoryRegistration {
                    repository: "robchristie/bokkie".to_owned(),
                    default_branch: "main".to_owned(),
                    checkout_path: checkout.to_string_lossy().into_owned(),
                    inspection_recurrence: Recurrence::new("0 0 2 * * *", "Australia/Adelaide")
                        .unwrap(),
                    first_inspection_at: 2_000,
                },
                1_000,
            )
            .unwrap();
        drop(store);
        let database_before = fs::read(&database).unwrap();
        let git_before = directory_digest(&checkout.join(".git"));
        let observer = CommandExternalObserver::with_executor(
            "/usr/bin/git",
            "/usr/bin/curl",
            Duration::from_secs(2),
            Arc::new(LocalGitExecutor),
        )
        .unwrap();

        let report = run_doctor(
            &database,
            migration_manifest(),
            &observer,
            DoctorOptions::at(1_500),
        )
        .unwrap();

        assert_eq!(
            check(&report, "gardener.external_reconciliation").status,
            DoctorStatus::Pass,
            "{report:#?}"
        );
        assert_eq!(fs::read(&database).unwrap(), database_before);
        assert_eq!(directory_digest(&checkout.join(".git")), git_before);
    }

    fn git(checkout: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(checkout)
            .args(arguments)
            .status()
            .unwrap();
        assert!(status.success(), "git {arguments:?} failed");
    }

    fn directory_digest(root: &Path) -> String {
        fn visit(root: &Path, path: &Path, digest: &mut sha2::Sha256) {
            let mut entries = fs::read_dir(path)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                let relative = path.strip_prefix(root).unwrap();
                sha2::Digest::update(digest, relative.to_string_lossy().as_bytes());
                if path.is_dir() {
                    visit(root, &path, digest);
                } else {
                    sha2::Digest::update(digest, fs::read(path).unwrap());
                }
            }
        }
        let mut digest = sha2::Sha256::new();
        visit(root, root, &mut digest);
        format!("{:x}", sha2::Digest::finalize(digest))
    }
}
