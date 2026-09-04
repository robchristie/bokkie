//! Narrow Git and GitHub process adapter for the coding gardener.
//!
//! The adapter deliberately owns no durable workflow state. Callers persist
//! intent before invoking each external operation and persist the exact
//! identities returned here before moving to the next operation.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

#[cfg(test)]
use crate::process::NoopHeartbeat;
use crate::process::{
    CancellationToken, EffectRisk, Interruption, ProcessError, ProcessEvidence, ProcessHeartbeat,
    ProcessLimits, ProcessOutcome, ProcessSupervisor,
};
use crate::runtime_trust::{
    ChildEnvironment, ExecutableIdentity, ExecutableRole, GitHubCredential, ProcessPolicy,
    RuntimeTrustError,
};

pub const CANONICAL_REPOSITORY: &str = "robchristie/bokkie";
pub const CANONICAL_DEFAULT_BRANCH: &str = "main";
pub const GARDENER_BRANCH_PREFIX: &str = "codex/gardener-";
const CANONICAL_HTTPS_URL: &str = "https://github.com/robchristie/bokkie.git";
const CANONICAL_GITHUB_API: &str = "https://api.github.com";
const EXECUTABLE_FILE_BUSY_ATTEMPTS: usize = 4;
const EXECUTABLE_FILE_BUSY_BACKOFF: Duration = Duration::from_millis(5);
const DEFAULT_EXECUTION_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const EMPTY_TREE_ID: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// An exact SHA-1 Git commit identity.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct CommitId(String);

impl CommitId {
    pub fn parse(value: impl Into<String>) -> Result<Self, GitWorkspaceError> {
        let value = value.into();
        if value.len() != 40
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(GitWorkspaceError::InvalidCommit(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommitId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// The kind of isolation created for a gardener operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorktreeKind {
    Detached,
    Branch { branch: String },
}

/// A worktree registered by this adapter instance.
///
/// Fields are private so cleanup cannot be redirected to an arbitrary path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredWorktree {
    path: PathBuf,
    owner_checkout: PathBuf,
    source_commit: CommitId,
    kind: WorktreeKind,
}

impl RegisteredWorktree {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn source_commit(&self) -> &CommitId {
        &self.source_commit
    }

    pub fn kind(&self) -> &WorktreeKind {
        &self.kind
    }

    pub fn branch(&self) -> Option<&str> {
        match &self.kind {
            WorktreeKind::Detached => None,
            WorktreeKind::Branch { branch } => Some(branch),
        }
    }
}

/// Exact, independently observed identity of a ready open pull request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PullRequestIdentity {
    pub repository: String,
    pub number: u64,
    pub url: String,
    pub branch: String,
    pub head: CommitId,
}

/// One immutable entry in the candidate commit's tracked tree.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CandidateTreeEntry {
    pub path: String,
    pub mode: String,
    pub object_type: String,
    pub object_id: String,
    pub byte_size: Option<u64>,
    pub binary: bool,
    pub symlink_target: Option<String>,
}

/// One path changed between the approved source and candidate commits.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CandidateDiffEntry {
    pub path: String,
    pub status: String,
    pub old_mode: String,
    pub new_mode: String,
    pub old_object_id: String,
    pub new_object_id: String,
    pub byte_size: Option<u64>,
    pub binary: bool,
    pub symlink_target: Option<String>,
}

/// Deterministic exact-commit tree and source-to-candidate diff evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CandidateManifest {
    pub source: CommitId,
    pub candidate: CommitId,
    pub tree: Vec<CandidateTreeEntry>,
    pub diff: Vec<CandidateDiffEntry>,
    pub sha256: String,
}

/// A fixed no-shell candidate check resolved and versioned at startup.
#[derive(Clone, Debug)]
pub struct CandidateCheckCommand {
    sandbox: Option<ExecutableIdentity>,
    executable: ExecutableIdentity,
    arguments: Vec<OsString>,
}

impl CandidateCheckCommand {
    pub fn new<I, S>(
        executable: ExecutableIdentity,
        arguments: I,
    ) -> Result<Self, GitWorkspaceError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        if executable.role() != ExecutableRole::CandidateCheck {
            return Err(GitWorkspaceError::InvalidCandidateManifest(
                "candidate check executable has the wrong role".to_owned(),
            ));
        }
        Ok(Self {
            sandbox: None,
            executable,
            arguments: arguments.into_iter().map(Into::into).collect(),
        })
    }

    pub fn sandboxed<I, S>(
        sandbox: ExecutableIdentity,
        executable: ExecutableIdentity,
        arguments: I,
    ) -> Result<Self, GitWorkspaceError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        if sandbox.role() != ExecutableRole::CandidateSandbox {
            return Err(GitWorkspaceError::InvalidCandidateManifest(
                "candidate sandbox executable has the wrong role".to_owned(),
            ));
        }
        let mut command = Self::new(executable, arguments)?;
        command.sandbox = Some(sandbox);
        Ok(command)
    }

    pub fn sandbox(&self) -> Option<&ExecutableIdentity> {
        self.sandbox.as_ref()
    }

    pub fn executable(&self) -> &ExecutableIdentity {
        &self.executable
    }

    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CandidateCheckStatus {
    Passed,
    Failed { exit_code: Option<i32> },
    Interrupted { detail: String },
}

/// Bounded evidence from one credential-free candidate check.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CandidateCheckResult {
    pub sandbox: Option<ExecutableIdentity>,
    pub executable: ExecutableIdentity,
    pub arguments: Vec<String>,
    pub duration_millis: u64,
    pub status: CandidateCheckStatus,
    pub evidence: ProcessEvidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CandidateQualification {
    pub manifest: CandidateManifest,
    pub checks: Vec<CandidateCheckResult>,
}

#[derive(Debug)]
pub struct ProcessFailure {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub cwd: PathBuf,
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
    pub evidence: ProcessEvidence,
}

struct ExecutedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: String,
    evidence: ProcessEvidence,
    arguments: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GitTopology {
    git_dir: PathBuf,
    common_dir: PathBuf,
}

impl fmt::Display for ProcessFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {:?} in {} exited with {}; stdout: {}; stderr: {}; {}",
            self.program.display(),
            self.arguments,
            self.cwd.display(),
            self.status,
            self.stdout.trim_end(),
            self.stderr.trim_end(),
            self.evidence,
        )
    }
}

#[derive(Debug, Error)]
pub enum GitWorkspaceError {
    #[error("invalid process supervision configuration: {0}")]
    InvalidSupervision(String),
    #[error("checkout path must be an existing absolute directory: {0}")]
    InvalidCheckout(PathBuf),
    #[error("invalid exact commit identity {0:?}; expected 40 lowercase hexadecimal characters")]
    InvalidCommit(String),
    #[error("invalid dedicated gardener branch {0:?}")]
    InvalidBranch(String),
    #[error("worktree path must be absolute, non-root, and lexically safe: {0}")]
    UnsafeWorktreePath(PathBuf),
    #[error("worktree path already exists: {0}")]
    WorktreePathExists(PathBuf),
    #[error("branch already exists: {0}")]
    BranchExists(String),
    #[error("cannot start {program} {arguments:?} in {cwd}: {source}")]
    Spawn {
        program: PathBuf,
        arguments: Vec<String>,
        cwd: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("unable to collect output from started {program} {arguments:?} in {cwd}: {source}")]
    Wait {
        program: PathBuf,
        arguments: Vec<String>,
        cwd: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("supervised process {program} {arguments:?} in {cwd} ended with {outcome}")]
    Supervision {
        program: PathBuf,
        arguments: Vec<String>,
        cwd: PathBuf,
        outcome: Box<ProcessOutcome>,
    },
    #[error("process failed: {0}")]
    Command(#[from] Box<ProcessFailure>),
    #[error("{stream} from {program} was not UTF-8: {source}")]
    NonUtf8Output {
        program: PathBuf,
        stream: &'static str,
        #[source]
        source: std::string::FromUtf8Error,
    },
    #[error("expected HEAD {expected} in {path}, observed {actual}")]
    HeadMismatch {
        path: PathBuf,
        expected: CommitId,
        actual: CommitId,
    },
    #[error("expected branch {expected:?} in {path}, observed {actual:?}")]
    BranchMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("worktree has no non-ignored changes to commit: {0}")]
    NoChanges(PathBuf),
    #[error("commit left additional non-ignored changes in {path}: {status}")]
    CommitLeftChanges { path: PathBuf, status: String },
    #[error("commit message must not be empty")]
    EmptyCommitMessage,
    #[error("worktree {path} belongs to checkout {actual}, not {expected}")]
    WrongCheckout {
        path: PathBuf,
        expected: PathBuf,
        actual: PathBuf,
    },
    #[error("worktree is not registered with the canonical checkout: {0}")]
    WorktreeNotRegistered(PathBuf),
    #[error("refusing to remove dirty worktree {path}; status: {status}")]
    DirtyWorktree { path: PathBuf, status: String },
    #[error("worktree removal was not complete for {0}")]
    RemovalIncomplete(PathBuf),
    #[error("cannot inspect filesystem path {path}: {source}")]
    Filesystem {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid GitHub JSON: {0}")]
    InvalidPullRequestJson(#[from] serde_json::Error),
    #[error("invalid pull-request observation: {0}")]
    InvalidPullRequest(String),
    #[error("invalid remote branch observation: {0}")]
    InvalidRemoteBranch(String),
    #[error("refusing {operation} through noncanonical effective origin URL(s): {urls:?}")]
    NonCanonicalOrigin {
        operation: &'static str,
        urls: Vec<String>,
    },
    #[error("runtime trust validation failed: {0}")]
    RuntimeTrust(#[from] RuntimeTrustError),
    #[error("{operation} requires an explicitly supplied GitHub credential")]
    MissingGitHubCredential { operation: &'static str },
    #[error(
        "Git topology changed for {path}: expected git dir {expected_git_dir} and common dir {expected_common_dir}, observed git dir {actual_git_dir} and common dir {actual_common_dir}"
    )]
    GitTopologyChanged {
        path: PathBuf,
        expected_git_dir: PathBuf,
        expected_common_dir: PathBuf,
        actual_git_dir: PathBuf,
        actual_common_dir: PathBuf,
    },
    #[error("invalid candidate manifest: {0}")]
    InvalidCandidateManifest(String),
    #[error("repository-local Git configuration is unsafe in {path}: {keys:?}")]
    UnsafeLocalGitConfig { path: PathBuf, keys: Vec<String> },
}

impl std::error::Error for ProcessFailure {}

impl GitWorkspaceError {
    pub fn is_ambiguous_external_state(&self) -> bool {
        matches!(
            self,
            Self::Supervision {
                outcome,
                ..
            } if matches!(outcome.as_ref(), ProcessOutcome::AmbiguousExternalState { .. })
        )
    }
}

/// Process-backed Git/GitHub adapter for the one canonical gardener target.
#[derive(Clone, Debug)]
pub struct GitWorkspace {
    checkout: PathBuf,
    git_executable: PathBuf,
    gh_executable: PathBuf,
    github_public_observer_executable: Option<PathBuf>,
    git_identity: Option<ExecutableIdentity>,
    gh_identity: Option<ExecutableIdentity>,
    github_public_observer_identity: Option<ExecutableIdentity>,
    environment: ChildEnvironment,
    github_credential: Option<GitHubCredential>,
    expected_checkout_topology: Option<GitTopology>,
    require_github_credential: bool,
    heartbeat_interval: Duration,
    execution_timeout: Duration,
    process_limits: ProcessLimits,
    cancellation: CancellationToken,
}

impl GitWorkspace {
    // Untrusted construction is private. Besides initialising `from_trust`, it
    // exists only for the adapter's compatibility tests; runtime callers must
    // cross `from_trust`.
    fn new(
        checkout: impl AsRef<Path>,
        git_executable: impl Into<PathBuf>,
        gh_executable: impl Into<PathBuf>,
    ) -> Result<Self, GitWorkspaceError> {
        let supplied = checkout.as_ref();
        if !supplied.is_absolute() || !supplied.is_dir() {
            return Err(GitWorkspaceError::InvalidCheckout(supplied.to_owned()));
        }
        let checkout =
            fs::canonicalize(supplied).map_err(|source| GitWorkspaceError::Filesystem {
                path: supplied.to_owned(),
                source,
            })?;
        Ok(Self {
            checkout,
            git_executable: git_executable.into(),
            gh_executable: gh_executable.into(),
            github_public_observer_executable: None,
            git_identity: None,
            gh_identity: None,
            github_public_observer_identity: None,
            environment: ChildEnvironment::captured_current()?,
            github_credential: None,
            expected_checkout_topology: None,
            require_github_credential: false,
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            execution_timeout: DEFAULT_EXECUTION_TIMEOUT,
            process_limits: ProcessLimits::default(),
            cancellation: CancellationToken::new(),
        })
    }

    /// Constructs the production adapter from startup-resolved identities.
    /// The canonical checkout topology is captured here for later mutation
    /// revalidation; no credential is exercised by construction.
    pub fn from_trust(
        checkout: impl AsRef<Path>,
        git_identity: ExecutableIdentity,
        gh_identity: ExecutableIdentity,
        github_public_observer_identity: ExecutableIdentity,
        environment: ChildEnvironment,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<Self, GitWorkspaceError> {
        if git_identity.role() != ExecutableRole::Git
            || gh_identity.role() != ExecutableRole::GitHub
            || github_public_observer_identity.role() != ExecutableRole::GitHubPublicObserver
        {
            return Err(GitWorkspaceError::InvalidSupervision(
                "GitWorkspace requires Git, GitHub, and public observer executable identities"
                    .to_owned(),
            ));
        }
        git_identity.verify_unchanged()?;
        gh_identity.verify_unchanged()?;
        github_public_observer_identity.verify_unchanged()?;
        let mut workspace = Self::new(
            checkout,
            git_identity.invocation_path().to_owned(),
            gh_identity.invocation_path().to_owned(),
        )?;
        workspace.git_identity = Some(git_identity);
        workspace.gh_identity = Some(gh_identity);
        workspace.github_public_observer_executable =
            Some(github_public_observer_identity.invocation_path().to_owned());
        workspace.github_public_observer_identity = Some(github_public_observer_identity);
        workspace.environment = environment;
        workspace.require_github_credential = true;
        workspace.expected_checkout_topology =
            Some(workspace.observe_topology(&workspace.checkout.clone(), heartbeat)?);
        Ok(workspace)
    }

    pub fn with_github_credential(mut self, credential: GitHubCredential) -> Self {
        self.github_credential = Some(credential);
        self
    }

    pub fn git_identity(&self) -> Option<&ExecutableIdentity> {
        self.git_identity.as_ref()
    }

    pub fn gh_identity(&self) -> Option<&ExecutableIdentity> {
        self.gh_identity.as_ref()
    }

    pub fn github_public_observer_identity(&self) -> Option<&ExecutableIdentity> {
        self.github_public_observer_identity.as_ref()
    }

    pub fn with_supervision(
        mut self,
        heartbeat_interval: Duration,
        execution_timeout: Duration,
        process_limits: ProcessLimits,
        cancellation: CancellationToken,
    ) -> Result<Self, GitWorkspaceError> {
        ProcessSupervisor::new(heartbeat_interval, process_limits, cancellation.clone())
            .map_err(GitWorkspaceError::InvalidSupervision)?;
        if execution_timeout.is_zero() {
            return Err(GitWorkspaceError::InvalidSupervision(
                "process execution timeout must be positive".to_owned(),
            ));
        }
        self.heartbeat_interval = heartbeat_interval;
        self.execution_timeout = execution_timeout;
        self.process_limits = process_limits;
        self.cancellation = cancellation;
        Ok(self)
    }

    pub fn checkout(&self) -> &Path {
        &self.checkout
    }

    /// Updates and resolves exactly `origin/main` after a read-only remote fetch.
    pub fn resolve_origin_main(
        &self,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<CommitId, GitWorkspaceError> {
        self.ensure_canonical_origin(&self.checkout, RemoteOperation::Fetch, heartbeat)?;
        self.git_success_policy(
            &self.checkout,
            ["fetch", "--quiet", "origin", CANONICAL_DEFAULT_BRANCH],
            EffectRisk::None,
            ProcessPolicy::GitRemoteRead,
            heartbeat,
        )?;
        self.resolve_revision(
            &self.checkout,
            &format!("refs/remotes/origin/{CANONICAL_DEFAULT_BRANCH}^{{commit}}"),
            heartbeat,
        )
    }

    /// Creates a detached worktree at an exact commit and immediately verifies HEAD.
    pub fn create_detached_worktree(
        &self,
        path: impl AsRef<Path>,
        source_commit: &CommitId,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<RegisteredWorktree, GitWorkspaceError> {
        let path = normalise_new_worktree_path(path.as_ref())?;
        self.ensure_canonical_origin(&self.checkout, RemoteOperation::WorktreeCreation, heartbeat)?;
        self.git_success(
            &self.checkout,
            [
                OsString::from("worktree"),
                OsString::from("add"),
                OsString::from("--detach"),
                path.as_os_str().to_owned(),
                OsString::from(source_commit.as_str()),
            ],
            EffectRisk::AmbiguousOnInterruption,
            heartbeat,
        )?;
        self.verify_head_path(&path, source_commit, heartbeat)?;
        self.verify_detached(&path, heartbeat)?;
        Ok(RegisteredWorktree {
            path,
            owner_checkout: self.checkout.clone(),
            source_commit: source_commit.clone(),
            kind: WorktreeKind::Detached,
        })
    }

    /// Creates a new dedicated branch worktree from an exact source commit.
    pub fn create_branch_worktree(
        &self,
        path: impl AsRef<Path>,
        branch: &str,
        source_commit: &CommitId,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<RegisteredWorktree, GitWorkspaceError> {
        validate_branch(branch)?;
        let path = normalise_new_worktree_path(path.as_ref())?;
        if self.local_branch_exists(branch, heartbeat)? {
            return Err(GitWorkspaceError::BranchExists(branch.to_owned()));
        }
        self.ensure_canonical_origin(&self.checkout, RemoteOperation::WorktreeCreation, heartbeat)?;
        self.git_success(
            &self.checkout,
            [
                OsString::from("worktree"),
                OsString::from("add"),
                OsString::from("-b"),
                OsString::from(branch),
                path.as_os_str().to_owned(),
                OsString::from(source_commit.as_str()),
            ],
            EffectRisk::AmbiguousOnInterruption,
            heartbeat,
        )?;
        self.verify_head_path(&path, source_commit, heartbeat)?;
        self.verify_branch_path(&path, branch, heartbeat)?;
        Ok(RegisteredWorktree {
            path,
            owner_checkout: self.checkout.clone(),
            source_commit: source_commit.clone(),
            kind: WorktreeKind::Branch {
                branch: branch.to_owned(),
            },
        })
    }

    /// Verifies a worktree's exact HEAD against caller-owned durable evidence.
    pub fn verify_head(
        &self,
        worktree: &RegisteredWorktree,
        expected: &CommitId,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<(), GitWorkspaceError> {
        self.ensure_owned(worktree)?;
        self.verify_head_path(&worktree.path, expected, heartbeat)
    }

    /// Returns Git porcelain v1 status, including all untracked non-ignored files.
    pub fn porcelain_status(
        &self,
        worktree: &RegisteredWorktree,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<String, GitWorkspaceError> {
        self.ensure_owned(worktree)?;
        self.status_path(&worktree.path, heartbeat)
    }

    /// Builds deterministic exact-tree/diff evidence, then runs each fixed
    /// check without a shell, network credentials, or ambient environment.
    /// Exact HEAD and worktree registration are checked around every command.
    pub fn qualify_candidate(
        &self,
        worktree: &RegisteredWorktree,
        source: &CommitId,
        candidate: &CommitId,
        checks: &[CandidateCheckCommand],
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<CandidateQualification, GitWorkspaceError> {
        self.ensure_owned(worktree)?;
        self.revalidate_checkout_topology(heartbeat)?;
        self.revalidate_worktree_registration(worktree, candidate, heartbeat)?;
        let status = self.status_path(&worktree.path, heartbeat)?;
        if !status.is_empty() {
            return Err(GitWorkspaceError::DirtyWorktree {
                path: worktree.path.clone(),
                status,
            });
        }
        let manifest = self.candidate_manifest(&worktree.path, source, candidate, heartbeat)?;
        let mut results = Vec::with_capacity(checks.len());
        for check in checks {
            if self.require_github_credential && check.sandbox.is_none() {
                return Err(GitWorkspaceError::InvalidCandidateManifest(
                    "trusted candidate checks require an OS sandbox executable".to_owned(),
                ));
            }
            self.revalidate_worktree_registration(worktree, candidate, heartbeat)?;
            check.executable.verify_unchanged()?;
            if let Some(sandbox) = &check.sandbox {
                sandbox.verify_unchanged()?;
            }
            let arguments = check
                .arguments
                .iter()
                .map(|argument| {
                    argument.to_str().map(str::to_owned).ok_or_else(|| {
                        GitWorkspaceError::InvalidCandidateManifest(
                            "candidate check argument is not valid UTF-8".to_owned(),
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let supervisor = ProcessSupervisor::new(
                self.heartbeat_interval,
                self.process_limits,
                self.cancellation.clone(),
            )
            .map_err(GitWorkspaceError::InvalidSupervision)?;
            let deadline = Instant::now()
                .checked_add(self.execution_timeout)
                .ok_or_else(|| {
                    GitWorkspaceError::InvalidSupervision(
                        "candidate check deadline is out of range".to_owned(),
                    )
                })?;
            let sandbox_directory = check
                .sandbox
                .as_ref()
                .map(|_| CandidateSandboxDirectory::materialise(&worktree.path, &manifest.tree))
                .transpose()?;
            let mut command =
                if let (Some(sandbox), Some(directory)) = (&check.sandbox, &sandbox_directory) {
                    self.sandboxed_check_command(sandbox, check, directory)?
                } else {
                    let mut command = Command::new(check.executable.invocation_path());
                    command.args(&check.arguments).current_dir(&worktree.path);
                    self.environment
                        .apply(&mut command, ProcessPolicy::CandidateCheck, None)?;
                    command
                };
            let started = Instant::now();
            let mut child = supervisor
                .spawn(&mut command, deadline, EffectRisk::None)
                .map_err(|source| GitWorkspaceError::Spawn {
                    program: check.sandbox.as_ref().map_or_else(
                        || check.executable.path().to_owned(),
                        |sandbox| sandbox.path().to_owned(),
                    ),
                    arguments: arguments.clone(),
                    cwd: sandbox_directory
                        .as_ref()
                        .map_or_else(|| worktree.path.clone(), |directory| directory.root.clone()),
                    source: match source {
                        ProcessError::Spawn(source) | ProcessError::Io(source) => source,
                        ProcessError::IoWorkerPanicked => io::Error::other("I/O worker panicked"),
                    },
                })?;
            child.close_stdin();
            let outcome = child
                .wait(heartbeat)
                .map_err(|source| GitWorkspaceError::Wait {
                    program: check.sandbox.as_ref().map_or_else(
                        || check.executable.path().to_owned(),
                        |sandbox| sandbox.path().to_owned(),
                    ),
                    arguments: arguments.clone(),
                    cwd: sandbox_directory
                        .as_ref()
                        .map_or_else(|| worktree.path.clone(), |directory| directory.root.clone()),
                    source: match source {
                        ProcessError::Spawn(source) | ProcessError::Io(source) => source,
                        ProcessError::IoWorkerPanicked => io::Error::other("I/O worker panicked"),
                    },
                })?;
            let elapsed = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            let (status, evidence) = match outcome {
                ProcessOutcome::Completed { status, evidence } if status.success() => {
                    (CandidateCheckStatus::Passed, evidence)
                }
                ProcessOutcome::Completed { status, evidence } => (
                    CandidateCheckStatus::Failed {
                        exit_code: status.code(),
                    },
                    evidence,
                ),
                outcome => (
                    CandidateCheckStatus::Interrupted {
                        detail: outcome.to_string(),
                    },
                    outcome.evidence().clone(),
                ),
            };
            results.push(CandidateCheckResult {
                sandbox: check.sandbox.clone(),
                executable: check.executable.clone(),
                arguments,
                duration_millis: elapsed,
                status,
                evidence,
            });
        }
        self.revalidate_worktree_registration(worktree, candidate, heartbeat)?;
        let final_status = self.status_path(&worktree.path, heartbeat)?;
        if !final_status.is_empty() {
            return Err(GitWorkspaceError::DirtyWorktree {
                path: worktree.path.clone(),
                status: final_status,
            });
        }
        Ok(CandidateQualification {
            manifest,
            checks: results,
        })
    }

    /// Commits all currently observed non-ignored changes in a branch worktree.
    pub fn commit_all(
        &self,
        worktree: &RegisteredWorktree,
        message: &str,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<CommitId, GitWorkspaceError> {
        self.ensure_owned(worktree)?;
        let branch = worktree
            .branch()
            .ok_or_else(|| GitWorkspaceError::InvalidBranch("detached worktree".to_owned()))?;
        self.verify_branch_path(&worktree.path, branch, heartbeat)?;
        if message.trim().is_empty() {
            return Err(GitWorkspaceError::EmptyCommitMessage);
        }
        if self.status_path(&worktree.path, heartbeat)?.is_empty() {
            return Err(GitWorkspaceError::NoChanges(worktree.path.clone()));
        }

        self.git_success(
            &worktree.path,
            ["add", "--all"],
            EffectRisk::AmbiguousOnInterruption,
            heartbeat,
        )?;
        self.git_success(
            &worktree.path,
            ["commit", "--message", message],
            EffectRisk::AmbiguousOnInterruption,
            heartbeat,
        )?;
        let commit = self.resolve_revision(&worktree.path, "HEAD^{commit}", heartbeat)?;
        self.verify_head_path(&worktree.path, &commit, heartbeat)?;
        let status = self.status_path(&worktree.path, heartbeat)?;
        if !status.is_empty() {
            return Err(GitWorkspaceError::CommitLeftChanges {
                path: worktree.path.clone(),
                status,
            });
        }
        Ok(commit)
    }

    /// Pushes exactly one validated dedicated branch to `origin`.
    pub fn push_branch(
        &self,
        worktree: &RegisteredWorktree,
        expected_head: &CommitId,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<(), GitWorkspaceError> {
        self.ensure_owned(worktree)?;
        self.ensure_mutation_credential("push")?;
        let branch = worktree
            .branch()
            .ok_or_else(|| GitWorkspaceError::InvalidBranch("detached worktree".to_owned()))?;
        validate_branch(branch)?;
        self.verify_branch_path(&worktree.path, branch, heartbeat)?;
        self.verify_head_path(&worktree.path, expected_head, heartbeat)?;
        self.revalidate_checkout_topology(heartbeat)?;
        self.revalidate_worktree_registration(worktree, expected_head, heartbeat)?;
        let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
        self.ensure_canonical_origin(&worktree.path, RemoteOperation::Push, heartbeat)?;
        self.git_success_policy(
            &worktree.path,
            ["push", "origin", refspec.as_str()],
            EffectRisk::AmbiguousOnInterruption,
            ProcessPolicy::GitHubMutationGit,
            heartbeat,
        )?;
        Ok(())
    }

    /// Independently observes the exact remote branch ref with `git ls-remote`.
    ///
    /// This deliberately does not inspect a local remote-tracking ref or infer
    /// identity from the preceding push process status.
    pub fn observe_remote_branch(
        &self,
        branch: &str,
        expected_head: &CommitId,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<CommitId, GitWorkspaceError> {
        validate_branch(branch)?;
        let reference = format!("refs/heads/{branch}");
        self.ensure_canonical_origin(&self.checkout, RemoteOperation::Fetch, heartbeat)?;
        let output = self.git_success_policy(
            &self.checkout,
            ["ls-remote", "--refs", "origin", reference.as_str()],
            EffectRisk::None,
            ProcessPolicy::GitRemoteRead,
            heartbeat,
        )?;
        let mut lines = output.lines();
        let line = lines.next().ok_or_else(|| {
            GitWorkspaceError::InvalidRemoteBranch(format!(
                "remote branch {reference:?} was not found"
            ))
        })?;
        if lines.next().is_some() {
            return Err(GitWorkspaceError::InvalidRemoteBranch(format!(
                "remote branch {reference:?} produced more than one result"
            )));
        }
        let mut fields = line.split_whitespace();
        let head = fields.next().ok_or_else(|| {
            GitWorkspaceError::InvalidRemoteBranch("missing commit identity".to_owned())
        })?;
        let observed_reference = fields.next().ok_or_else(|| {
            GitWorkspaceError::InvalidRemoteBranch("missing branch ref".to_owned())
        })?;
        if fields.next().is_some() || observed_reference != reference {
            return Err(GitWorkspaceError::InvalidRemoteBranch(format!(
                "expected exactly {reference:?}, observed {line:?}"
            )));
        }
        let observed = CommitId::parse(head.to_owned())?;
        if &observed != expected_head {
            return Err(GitWorkspaceError::InvalidRemoteBranch(format!(
                "expected head {expected_head}, observed {observed}"
            )));
        }
        Ok(observed)
    }

    /// Creates a ready PR, then independently observes its structured identity.
    /// Retained for compatibility fixtures; trusted production adapters must
    /// use the draft/check/ready sequence.
    pub fn create_ready_pull_request(
        &self,
        branch: &str,
        expected_head: &CommitId,
        title: &str,
        body: &str,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<PullRequestIdentity, GitWorkspaceError> {
        if self.require_github_credential {
            return Err(GitWorkspaceError::InvalidPullRequest(
                "trusted publication must create a draft and promote it only after qualification"
                    .to_owned(),
            ));
        }
        self.create_pull_request(branch, expected_head, title, body, false, heartbeat)
    }

    /// Creates a draft pull request after revalidating the exact local and
    /// remote branch identity immediately before the credential-bearing call.
    pub fn create_draft_pull_request(
        &self,
        branch: &str,
        expected_head: &CommitId,
        title: &str,
        body: &str,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<PullRequestIdentity, GitWorkspaceError> {
        self.create_pull_request(branch, expected_head, title, body, true, heartbeat)
    }

    fn create_pull_request(
        &self,
        branch: &str,
        expected_head: &CommitId,
        title: &str,
        body: &str,
        draft: bool,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<PullRequestIdentity, GitWorkspaceError> {
        validate_branch(branch)?;
        if title.trim().is_empty() {
            return Err(GitWorkspaceError::InvalidPullRequest(
                "title must not be empty".to_owned(),
            ));
        }
        if self.require_github_credential {
            self.revalidate_publication_branch(branch, expected_head, heartbeat)?;
        }
        let mut arguments = vec![
            "pr",
            "create",
            "--repo",
            CANONICAL_REPOSITORY,
            "--base",
            CANONICAL_DEFAULT_BRANCH,
            "--head",
            branch,
            "--title",
            title,
            "--body",
            body,
        ];
        if draft {
            arguments.push("--draft");
        }
        self.gh_success_policy(
            arguments,
            EffectRisk::AmbiguousOnInterruption,
            ProcessPolicy::GitHubMutationCli,
            heartbeat,
        )?;
        self.observe_pull_request_state(branch, expected_head, draft, heartbeat)
    }

    /// Observes a ready PR through the credential-free public GitHub API in a
    /// trusted runtime. The legacy constructor retains structured `gh` output
    /// solely for compatibility with local adapter tests.
    pub fn observe_pull_request(
        &self,
        branch: &str,
        expected_head: &CommitId,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<PullRequestIdentity, GitWorkspaceError> {
        if self.require_github_credential {
            self.revalidate_publication_branch(branch, expected_head, heartbeat)?;
        }
        self.observe_pull_request_state(branch, expected_head, false, heartbeat)
    }

    pub fn observe_draft_pull_request(
        &self,
        branch: &str,
        expected_head: &CommitId,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<PullRequestIdentity, GitWorkspaceError> {
        if self.require_github_credential {
            self.revalidate_publication_branch(branch, expected_head, heartbeat)?;
        }
        self.observe_pull_request_state(branch, expected_head, true, heartbeat)
    }

    fn observe_pull_request_state(
        &self,
        branch: &str,
        expected_head: &CommitId,
        expected_draft: bool,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<PullRequestIdentity, GitWorkspaceError> {
        validate_branch(branch)?;
        if self.github_public_observer_identity.is_some() {
            return self.observe_public_pull_request_state(
                branch,
                expected_head,
                expected_draft,
                heartbeat,
            );
        }
        let output = self.gh_success_policy(
            [
                "pr",
                "view",
                branch,
                "--repo",
                CANONICAL_REPOSITORY,
                "--json",
                "number,url,headRefOid,state,isDraft",
            ],
            EffectRisk::None,
            ProcessPolicy::GitHubRead,
            heartbeat,
        )?;
        let observation: PullRequestObservation = serde_json::from_str(&output)?;
        validate_pull_request_observation(branch, expected_head, expected_draft, observation)
    }

    fn observe_public_pull_request_state(
        &self,
        branch: &str,
        expected_head: &CommitId,
        expected_draft: bool,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<PullRequestIdentity, GitWorkspaceError> {
        let executable = self
            .github_public_observer_executable
            .as_ref()
            .ok_or_else(|| {
                GitWorkspaceError::InvalidSupervision(
                    "trusted pull-request observation requires its startup identity".to_owned(),
                )
            })?;
        let head = format!("robchristie:{branch}");
        let url = format!(
            "{CANONICAL_GITHUB_API}/repos/{CANONICAL_REPOSITORY}/pulls?state=open&head={}&per_page=2",
            percent_encode_query_value(&head)
        );
        let timeout_seconds = self.execution_timeout.as_secs().max(1).to_string();
        let output = self.public_observer_success(
            executable,
            [
                "--disable".to_owned(),
                "--fail-with-body".to_owned(),
                "--silent".to_owned(),
                "--show-error".to_owned(),
                "--proto".to_owned(),
                "=https".to_owned(),
                "--tlsv1.2".to_owned(),
                "--connect-timeout".to_owned(),
                "10".to_owned(),
                "--max-time".to_owned(),
                timeout_seconds,
                "--header".to_owned(),
                "Accept: application/vnd.github+json".to_owned(),
                "--header".to_owned(),
                "X-GitHub-Api-Version: 2022-11-28".to_owned(),
                url,
            ],
            heartbeat,
        )?;
        let mut observations: Vec<PublicPullRequestObservation> = serde_json::from_str(&output)?;
        if observations.len() != 1 {
            return Err(GitWorkspaceError::InvalidPullRequest(format!(
                "expected exactly one public open pull request for branch {branch:?}, observed {}",
                observations.len()
            )));
        }
        validate_public_pull_request_observation(
            branch,
            expected_head,
            expected_draft,
            observations.pop().expect("length checked"),
        )
    }

    /// Promotes an already-observed exact-head draft after caller-owned checks
    /// have passed. The branch is revalidated immediately before `gh pr ready`.
    pub fn mark_pull_request_ready(
        &self,
        branch: &str,
        expected_head: &CommitId,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<PullRequestIdentity, GitWorkspaceError> {
        self.observe_draft_pull_request(branch, expected_head, heartbeat)?;
        self.revalidate_publication_branch(branch, expected_head, heartbeat)?;
        self.gh_success_policy(
            ["pr", "ready", branch, "--repo", CANONICAL_REPOSITORY],
            EffectRisk::AmbiguousOnInterruption,
            ProcessPolicy::GitHubMutationCli,
            heartbeat,
        )?;
        self.observe_pull_request(branch, expected_head, heartbeat)
    }

    /// Removes a registered clean worktree without force and verifies removal.
    pub fn remove_clean_worktree(
        &self,
        worktree: &RegisteredWorktree,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<(), GitWorkspaceError> {
        self.ensure_owned(worktree)?;
        validate_existing_removal_path(&worktree.path)?;
        let registered = self.registered_worktree_paths(heartbeat)?;
        if !registered.iter().any(|path| path == &worktree.path) {
            return Err(GitWorkspaceError::WorktreeNotRegistered(
                worktree.path.clone(),
            ));
        }
        let status = self.status_path(&worktree.path, heartbeat)?;
        if !status.is_empty() {
            return Err(GitWorkspaceError::DirtyWorktree {
                path: worktree.path.clone(),
                status,
            });
        }
        self.git_success(
            &self.checkout,
            [
                OsString::from("worktree"),
                OsString::from("remove"),
                worktree.path.as_os_str().to_owned(),
            ],
            EffectRisk::AmbiguousOnInterruption,
            heartbeat,
        )?;
        if worktree.path.exists()
            || self
                .registered_worktree_paths(heartbeat)?
                .iter()
                .any(|path| path == &worktree.path)
        {
            return Err(GitWorkspaceError::RemovalIncomplete(worktree.path.clone()));
        }
        Ok(())
    }

    fn ensure_owned(&self, worktree: &RegisteredWorktree) -> Result<(), GitWorkspaceError> {
        if worktree.owner_checkout != self.checkout {
            return Err(GitWorkspaceError::WrongCheckout {
                path: worktree.path.clone(),
                expected: self.checkout.clone(),
                actual: worktree.owner_checkout.clone(),
            });
        }
        Ok(())
    }

    fn candidate_manifest(
        &self,
        cwd: &Path,
        source: &CommitId,
        candidate: &CommitId,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<CandidateManifest, GitWorkspaceError> {
        self.verify_head_path(cwd, candidate, heartbeat)?;
        let tree_binary = self.binary_paths(EMPTY_TREE_ID, candidate.as_str(), cwd, heartbeat)?;
        let tree_output = self.git_success(
            cwd,
            [
                "ls-tree",
                "-r",
                "-z",
                "-l",
                "--full-tree",
                candidate.as_str(),
            ],
            EffectRisk::None,
            heartbeat,
        )?;
        let mut tree = Vec::new();
        for record in tree_output.split('\0').filter(|record| !record.is_empty()) {
            let (metadata, path) = record.split_once('\t').ok_or_else(|| {
                GitWorkspaceError::InvalidCandidateManifest(
                    "git ls-tree record is missing its path separator".to_owned(),
                )
            })?;
            let fields = metadata.split_whitespace().collect::<Vec<_>>();
            if fields.len() != 4 {
                return Err(GitWorkspaceError::InvalidCandidateManifest(format!(
                    "git ls-tree record has {} metadata fields",
                    fields.len()
                )));
            }
            let byte_size = if fields[3] == "-" {
                None
            } else {
                Some(fields[3].parse::<u64>().map_err(|_| {
                    GitWorkspaceError::InvalidCandidateManifest(
                        "git ls-tree byte size is invalid".to_owned(),
                    )
                })?)
            };
            let symlink_target = if fields[0] == "120000" {
                Some(
                    self.git_success(
                        cwd,
                        ["cat-file", "blob", fields[2]],
                        EffectRisk::None,
                        heartbeat,
                    )?
                    .trim_end_matches(['\r', '\n'])
                    .to_owned(),
                )
            } else {
                None
            };
            tree.push(CandidateTreeEntry {
                path: path.to_owned(),
                mode: fields[0].to_owned(),
                object_type: fields[1].to_owned(),
                object_id: fields[2].to_owned(),
                byte_size,
                binary: tree_binary.get(path).copied().unwrap_or(false),
                symlink_target,
            });
        }
        tree.sort_by(|left, right| left.path.cmp(&right.path));

        let diff_binary = self.binary_paths(source.as_str(), candidate.as_str(), cwd, heartbeat)?;
        let diff_output = self.git_success(
            cwd,
            [
                "diff",
                "--raw",
                "--no-abbrev",
                "--no-renames",
                "-z",
                source.as_str(),
                candidate.as_str(),
            ],
            EffectRisk::None,
            heartbeat,
        )?;
        let mut records = diff_output.split('\0').filter(|record| !record.is_empty());
        let mut diff = Vec::new();
        while let Some(header) = records.next() {
            let path = records.next().ok_or_else(|| {
                GitWorkspaceError::InvalidCandidateManifest(
                    "git diff raw record is missing its path".to_owned(),
                )
            })?;
            let fields = header
                .strip_prefix(':')
                .ok_or_else(|| {
                    GitWorkspaceError::InvalidCandidateManifest(
                        "git diff raw record is missing its prefix".to_owned(),
                    )
                })?
                .split_whitespace()
                .collect::<Vec<_>>();
            if fields.len() != 5 {
                return Err(GitWorkspaceError::InvalidCandidateManifest(format!(
                    "git diff raw record has {} metadata fields",
                    fields.len()
                )));
            }
            let current = tree.iter().find(|entry| entry.path == path);
            diff.push(CandidateDiffEntry {
                path: path.to_owned(),
                status: fields[4].to_owned(),
                old_mode: fields[0].to_owned(),
                new_mode: fields[1].to_owned(),
                old_object_id: fields[2].to_owned(),
                new_object_id: fields[3].to_owned(),
                byte_size: current.and_then(|entry| entry.byte_size),
                binary: diff_binary.get(path).copied().unwrap_or(false),
                symlink_target: current.and_then(|entry| entry.symlink_target.clone()),
            });
        }
        diff.sort_by(|left, right| left.path.cmp(&right.path));
        let canonical =
            serde_json::to_vec(&(source, candidate, &tree, &diff)).map_err(|error| {
                GitWorkspaceError::InvalidCandidateManifest(format!(
                    "cannot serialise candidate manifest: {error}"
                ))
            })?;
        let sha256 = format!("{:x}", Sha256::digest(canonical));
        Ok(CandidateManifest {
            source: source.clone(),
            candidate: candidate.clone(),
            tree,
            diff,
            sha256,
        })
    }

    fn sandboxed_check_command(
        &self,
        sandbox: &ExecutableIdentity,
        check: &CandidateCheckCommand,
        directory: &CandidateSandboxDirectory,
    ) -> Result<Command, GitWorkspaceError> {
        let check_name = check
            .executable
            .invocation_path()
            .file_name()
            .ok_or_else(|| {
                GitWorkspaceError::InvalidCandidateManifest(
                    "candidate check executable has no file name".to_owned(),
                )
            })?;
        let sandbox_check = Path::new("/bokkie").join(check_name);
        let mut arguments = vec![
            OsString::from("--die-with-parent"),
            OsString::from("--new-session"),
            OsString::from("--unshare-all"),
            OsString::from("--clearenv"),
            OsString::from("--proc"),
            OsString::from("/proc"),
            OsString::from("--dev"),
            OsString::from("/dev"),
            OsString::from("--tmpfs"),
            OsString::from("/tmp"),
            OsString::from("--ro-bind"),
            OsString::from("/usr"),
            OsString::from("/usr"),
        ];
        for path in ["/bin", "/lib", "/lib64"] {
            append_system_mount(&mut arguments, Path::new(path))?;
        }
        for path in [
            "/home",
            "/home/bokkie",
            "/home/bokkie/.cargo",
            "/runtime",
            "/runtime/rustup",
            "/bokkie",
        ] {
            arguments.extend([OsString::from("--dir"), OsString::from(path)]);
        }
        append_optional_cache_mount(
            &mut arguments,
            &self.environment.home().join(".cargo/registry"),
            Path::new("/home/bokkie/.cargo/registry"),
        )?;
        append_optional_cache_mount(
            &mut arguments,
            &self.environment.home().join(".rustup/toolchains"),
            Path::new("/runtime/rustup/toolchains"),
        )?;
        arguments.extend([
            OsString::from("--bind"),
            directory.workspace.as_os_str().to_owned(),
            OsString::from("/workspace"),
            OsString::from("--ro-bind"),
            check.executable.invocation_path().as_os_str().to_owned(),
            sandbox_check.as_os_str().to_owned(),
            OsString::from("--chdir"),
            OsString::from("/workspace"),
        ]);
        for (key, value) in [
            ("HOME", "/home/bokkie"),
            ("XDG_CONFIG_HOME", "/home/bokkie/.config"),
            ("XDG_CACHE_HOME", "/home/bokkie/.cache"),
            ("CARGO_HOME", "/home/bokkie/.cargo"),
            ("RUSTUP_HOME", "/runtime/rustup"),
            ("PATH", "/bokkie:/usr/local/bin:/usr/bin:/bin"),
            ("CARGO_NET_OFFLINE", "true"),
            ("CARGO_TERM_COLOR", "never"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GH_PROMPT_DISABLED", "1"),
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
        ] {
            arguments.extend([
                OsString::from("--setenv"),
                OsString::from(key),
                OsString::from(value),
            ]);
        }
        arguments.push(sandbox_check.into_os_string());
        arguments.extend(check.arguments.iter().cloned());
        let mut command = Command::new(sandbox.invocation_path());
        command.args(arguments).current_dir(&directory.root);
        self.environment
            .apply(&mut command, ProcessPolicy::CandidateSandbox, None)?;
        Ok(command)
    }

    fn binary_paths(
        &self,
        old: &str,
        new: &str,
        cwd: &Path,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<HashMap<String, bool>, GitWorkspaceError> {
        let output = self.git_success(
            cwd,
            ["diff", "--numstat", "--no-renames", "-z", old, new],
            EffectRisk::None,
            heartbeat,
        )?;
        let mut result = HashMap::new();
        for record in output.split('\0').filter(|record| !record.is_empty()) {
            let mut fields = record.splitn(3, '\t');
            let additions = fields.next().unwrap_or_default();
            let deletions = fields.next().unwrap_or_default();
            let path = fields.next().ok_or_else(|| {
                GitWorkspaceError::InvalidCandidateManifest(
                    "git diff numstat record is missing its path".to_owned(),
                )
            })?;
            result.insert(path.to_owned(), additions == "-" || deletions == "-");
        }
        Ok(result)
    }

    fn ensure_mutation_credential(&self, operation: &'static str) -> Result<(), GitWorkspaceError> {
        if self.require_github_credential && self.github_credential.is_none() {
            return Err(GitWorkspaceError::MissingGitHubCredential { operation });
        }
        Ok(())
    }

    fn revalidate_publication_branch(
        &self,
        branch: &str,
        expected_head: &CommitId,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<(), GitWorkspaceError> {
        self.ensure_mutation_credential("pull-request mutation")?;
        self.revalidate_checkout_topology(heartbeat)?;
        self.ensure_canonical_origin(&self.checkout, RemoteOperation::Fetch, heartbeat)?;
        let local_head = self.resolve_revision(
            &self.checkout,
            &format!("refs/heads/{branch}^{{commit}}"),
            heartbeat,
        )?;
        if &local_head != expected_head {
            return Err(GitWorkspaceError::HeadMismatch {
                path: self.checkout.clone(),
                expected: expected_head.clone(),
                actual: local_head,
            });
        }
        self.observe_remote_branch(branch, expected_head, heartbeat)?;
        Ok(())
    }

    fn revalidate_checkout_topology(
        &self,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<(), GitWorkspaceError> {
        let Some(expected) = &self.expected_checkout_topology else {
            return Ok(());
        };
        let actual = self.observe_topology(&self.checkout, heartbeat)?;
        if actual != *expected {
            return Err(GitWorkspaceError::GitTopologyChanged {
                path: self.checkout.clone(),
                expected_git_dir: expected.git_dir.clone(),
                expected_common_dir: expected.common_dir.clone(),
                actual_git_dir: actual.git_dir,
                actual_common_dir: actual.common_dir,
            });
        }
        Ok(())
    }

    fn revalidate_worktree_registration(
        &self,
        worktree: &RegisteredWorktree,
        expected_head: &CommitId,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<(), GitWorkspaceError> {
        let registered = self.registered_worktree_paths(heartbeat)?;
        if !registered.iter().any(|path| path == &worktree.path) {
            return Err(GitWorkspaceError::WorktreeNotRegistered(
                worktree.path.clone(),
            ));
        }
        let topology = self.observe_topology(&worktree.path, heartbeat)?;
        if let Some(checkout) = &self.expected_checkout_topology {
            if topology.common_dir != checkout.common_dir {
                return Err(GitWorkspaceError::GitTopologyChanged {
                    path: worktree.path.clone(),
                    expected_git_dir: topology.git_dir.clone(),
                    expected_common_dir: checkout.common_dir.clone(),
                    actual_git_dir: topology.git_dir,
                    actual_common_dir: topology.common_dir,
                });
            }
        }
        self.verify_head_path(&worktree.path, expected_head, heartbeat)?;
        match &worktree.kind {
            WorktreeKind::Detached => self.verify_detached(&worktree.path, heartbeat),
            WorktreeKind::Branch { branch } => {
                self.verify_branch_path(&worktree.path, branch, heartbeat)
            }
        }
    }

    fn observe_topology(
        &self,
        cwd: &Path,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<GitTopology, GitWorkspaceError> {
        let git_dir = self.git_success(
            cwd,
            ["rev-parse", "--path-format=absolute", "--git-dir"],
            EffectRisk::None,
            heartbeat,
        )?;
        let common_dir = self.git_success(
            cwd,
            ["rev-parse", "--path-format=absolute", "--git-common-dir"],
            EffectRisk::None,
            heartbeat,
        )?;
        Ok(GitTopology {
            git_dir: canonical_output_path(cwd, git_dir.trim())?,
            common_dir: canonical_output_path(cwd, common_dir.trim())?,
        })
    }

    fn ensure_canonical_origin(
        &self,
        cwd: &Path,
        operation: RemoteOperation,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<(), GitWorkspaceError> {
        let outputs = match operation {
            RemoteOperation::Fetch => {
                vec![self.git_success(
                    cwd,
                    ["remote", "get-url", "--all", "origin"],
                    EffectRisk::None,
                    heartbeat,
                )?]
            }
            RemoteOperation::WorktreeCreation => vec![
                self.git_success(
                    cwd,
                    ["remote", "get-url", "--all", "origin"],
                    EffectRisk::None,
                    heartbeat,
                )?,
                self.git_success(
                    cwd,
                    ["remote", "get-url", "--push", "--all", "origin"],
                    EffectRisk::None,
                    heartbeat,
                )?,
            ],
            RemoteOperation::Push => {
                vec![self.git_success(
                    cwd,
                    ["remote", "get-url", "--push", "--all", "origin"],
                    EffectRisk::None,
                    heartbeat,
                )?]
            }
        };
        let urls = outputs
            .iter()
            .flat_map(|output| output.lines().map(str::to_owned))
            .collect::<Vec<_>>();
        if urls.is_empty() || !urls.iter().all(|url| is_canonical_repository_url(url)) {
            return Err(GitWorkspaceError::NonCanonicalOrigin {
                operation: operation.description(),
                urls,
            });
        }
        Ok(())
    }

    fn local_branch_exists(
        &self,
        branch: &str,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<bool, GitWorkspaceError> {
        let reference = format!("refs/heads/{branch}");
        let output = self.git_output(
            &self.checkout,
            ["show-ref", "--verify", "--quiet", reference.as_str()],
            EffectRisk::None,
            heartbeat,
        )?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(self
                .process_failure(&self.git_executable, &self.checkout, &output)
                .into()),
        }
    }

    fn verify_head_path(
        &self,
        path: &Path,
        expected: &CommitId,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<(), GitWorkspaceError> {
        let actual = self.resolve_revision(path, "HEAD^{commit}", heartbeat)?;
        if &actual != expected {
            return Err(GitWorkspaceError::HeadMismatch {
                path: path.to_owned(),
                expected: expected.clone(),
                actual,
            });
        }
        Ok(())
    }

    fn verify_detached(
        &self,
        path: &Path,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<(), GitWorkspaceError> {
        let output = self.git_output(
            path,
            ["symbolic-ref", "--quiet", "HEAD"],
            EffectRisk::None,
            heartbeat,
        )?;
        match output.status.code() {
            Some(1) => Ok(()),
            Some(0) => Err(GitWorkspaceError::BranchMismatch {
                path: path.to_owned(),
                expected: "detached HEAD".to_owned(),
                actual: stdout(&self.git_executable, output.stdout)?
                    .trim()
                    .to_owned(),
            }),
            _ => Err(self
                .process_failure(&self.git_executable, path, &output)
                .into()),
        }
    }

    fn verify_branch_path(
        &self,
        path: &Path,
        expected: &str,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<(), GitWorkspaceError> {
        let actual = self
            .git_success(
                path,
                ["symbolic-ref", "--quiet", "--short", "HEAD"],
                EffectRisk::None,
                heartbeat,
            )?
            .trim()
            .to_owned();
        if actual != expected {
            return Err(GitWorkspaceError::BranchMismatch {
                path: path.to_owned(),
                expected: expected.to_owned(),
                actual,
            });
        }
        Ok(())
    }

    fn resolve_revision(
        &self,
        cwd: &Path,
        revision: &str,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<CommitId, GitWorkspaceError> {
        let stdout = self.git_success(
            cwd,
            ["rev-parse", "--verify", revision],
            EffectRisk::None,
            heartbeat,
        )?;
        let value = stdout.strip_suffix('\n').unwrap_or(&stdout);
        let value = value.strip_suffix('\r').unwrap_or(value);
        CommitId::parse(value.to_owned())
    }

    fn status_path(
        &self,
        path: &Path,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<String, GitWorkspaceError> {
        self.git_success(
            path,
            ["status", "--porcelain=v1", "--untracked-files=all"],
            EffectRisk::None,
            heartbeat,
        )
    }

    fn registered_worktree_paths(
        &self,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<Vec<PathBuf>, GitWorkspaceError> {
        let output = self.git_success(
            &self.checkout,
            ["worktree", "list", "--porcelain"],
            EffectRisk::None,
            heartbeat,
        )?;
        output
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .map(|path| {
                fs::canonicalize(path).map_err(|source| GitWorkspaceError::Filesystem {
                    path: PathBuf::from(path),
                    source,
                })
            })
            .collect()
    }

    fn git_success<I, S>(
        &self,
        cwd: &Path,
        arguments: I,
        risk: EffectRisk,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<String, GitWorkspaceError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.git_success_policy(cwd, arguments, risk, ProcessPolicy::GitLocal, heartbeat)
    }

    fn git_success_policy<I, S>(
        &self,
        cwd: &Path,
        arguments: I,
        risk: EffectRisk,
        policy: ProcessPolicy,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<String, GitWorkspaceError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let output = self.git_output_policy(cwd, arguments, risk, policy, heartbeat)?;
        if !output.status.success() {
            return Err(self
                .process_failure(&self.git_executable, cwd, &output)
                .into());
        }
        stdout(&self.git_executable, output.stdout)
    }

    fn git_output<I, S>(
        &self,
        cwd: &Path,
        arguments: I,
        risk: EffectRisk,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<ExecutedOutput, GitWorkspaceError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.git_output_policy(cwd, arguments, risk, ProcessPolicy::GitLocal, heartbeat)
    }

    fn git_output_policy<I, S>(
        &self,
        cwd: &Path,
        arguments: I,
        risk: EffectRisk,
        policy: ProcessPolicy,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<ExecutedOutput, GitWorkspaceError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.reject_unsafe_local_git_config(cwd, heartbeat)?;
        self.run(
            &self.git_executable,
            cwd,
            arguments,
            risk,
            policy,
            heartbeat,
        )
    }

    fn reject_unsafe_local_git_config(
        &self,
        cwd: &Path,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<(), GitWorkspaceError> {
        let mut unsafe_keys = Vec::new();
        let mut scopes = vec!["--local"];
        let mut index = 0;
        while let Some(scope) = scopes.get(index).copied() {
            index += 1;
            let output = self.run(
                &self.git_executable,
                cwd,
                [
                    "config",
                    scope,
                    "--null",
                    "--name-only",
                    "--no-includes",
                    "--list",
                ],
                EffectRisk::None,
                ProcessPolicy::GitLocal,
                heartbeat,
            )?;
            if !output.status.success() {
                return Err(self
                    .process_failure(&self.git_executable, cwd, &output)
                    .into());
            }
            let names = stdout(&self.git_executable, output.stdout)?;
            if scope == "--local"
                && names
                    .split('\0')
                    .any(|name| name.eq_ignore_ascii_case("extensions.worktreeConfig"))
            {
                let worktree_config = self.run(
                    &self.git_executable,
                    cwd,
                    [
                        "rev-parse",
                        "--path-format=absolute",
                        "--git-path",
                        "config.worktree",
                    ],
                    EffectRisk::None,
                    ProcessPolicy::GitLocal,
                    heartbeat,
                )?;
                if !worktree_config.status.success() {
                    return Err(self
                        .process_failure(&self.git_executable, cwd, &worktree_config)
                        .into());
                }
                let path = stdout(&self.git_executable, worktree_config.stdout)?;
                if Path::new(path.trim()).is_file() {
                    scopes.push("--worktree");
                }
            }
            unsafe_keys.extend(
                names
                    .split('\0')
                    .filter(|name| !name.is_empty())
                    .filter(|name| is_unsafe_local_git_key(name))
                    .map(str::to_ascii_lowercase),
            );
        }
        unsafe_keys.sort();
        unsafe_keys.dedup();
        if unsafe_keys.is_empty() {
            Ok(())
        } else {
            Err(GitWorkspaceError::UnsafeLocalGitConfig {
                path: cwd.to_owned(),
                keys: unsafe_keys,
            })
        }
    }

    fn gh_success_policy<I, S>(
        &self,
        arguments: I,
        risk: EffectRisk,
        policy: ProcessPolicy,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<String, GitWorkspaceError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let output = self.run(
            &self.gh_executable,
            &self.checkout,
            arguments,
            risk,
            policy,
            heartbeat,
        )?;
        if !output.status.success() {
            return Err(self
                .process_failure(&self.gh_executable, &self.checkout, &output)
                .into());
        }
        stdout(&self.gh_executable, output.stdout)
    }

    fn public_observer_success<I, S>(
        &self,
        executable: &Path,
        arguments: I,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<String, GitWorkspaceError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let output = self.run(
            executable,
            &self.checkout,
            arguments,
            EffectRisk::None,
            ProcessPolicy::GitHubPublicRead,
            heartbeat,
        )?;
        if !output.status.success() {
            return Err(self
                .process_failure(executable, &self.checkout, &output)
                .into());
        }
        stdout(executable, output.stdout)
    }

    #[cfg(test)]
    fn gh_success<I, S>(
        &self,
        arguments: I,
        risk: EffectRisk,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<String, GitWorkspaceError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.gh_success_policy(arguments, risk, ProcessPolicy::GitHubRead, heartbeat)
    }

    fn run<I, S>(
        &self,
        program: &Path,
        cwd: &Path,
        arguments: I,
        risk: EffectRisk,
        policy: ProcessPolicy,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<ExecutedOutput, GitWorkspaceError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let arguments = arguments.into_iter().map(Into::into).collect::<Vec<_>>();
        let displayed_arguments = display_arguments(&arguments);
        let identity = if program == self.git_executable {
            self.git_identity.as_ref()
        } else if program == self.gh_executable {
            self.gh_identity.as_ref()
        } else if self
            .github_public_observer_executable
            .as_deref()
            .is_some_and(|observer| program == observer)
        {
            self.github_public_observer_identity.as_ref()
        } else {
            None
        };
        if let Some(identity) = identity {
            identity.verify_unchanged()?;
        }
        let supervisor = ProcessSupervisor::new(
            self.heartbeat_interval,
            self.process_limits,
            self.cancellation.clone(),
        )
        .map_err(GitWorkspaceError::InvalidSupervision)?;
        let mut child = retry_executable_file_busy(|| {
            let mut command = Command::new(program);
            command.args(&arguments).current_dir(cwd);
            let credential = if matches!(
                policy,
                ProcessPolicy::GitHubMutationGit | ProcessPolicy::GitHubMutationCli
            ) {
                self.github_credential.as_ref()
            } else {
                None
            };
            self.environment
                .apply(&mut command, policy, credential)
                .map_err(|error| io::Error::other(error.to_string()))?;
            let deadline = Instant::now()
                .checked_add(self.execution_timeout)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "execution deadline is out of range",
                    )
                })?;
            supervisor
                .spawn(&mut command, deadline, risk)
                .map_err(|error| match error {
                    ProcessError::Spawn(source) | ProcessError::Io(source) => source,
                    ProcessError::IoWorkerPanicked => io::Error::other("I/O worker panicked"),
                })
        })
        .map_err(|source| GitWorkspaceError::Spawn {
            program: program.to_owned(),
            arguments: displayed_arguments.clone(),
            cwd: cwd.to_owned(),
            source,
        })?;
        child.close_stdin();
        let outcome = child
            .wait(heartbeat)
            .map_err(|error| GitWorkspaceError::Wait {
                program: program.to_owned(),
                arguments: displayed_arguments.clone(),
                cwd: cwd.to_owned(),
                source: match error {
                    ProcessError::Spawn(source) | ProcessError::Io(source) => source,
                    ProcessError::IoWorkerPanicked => io::Error::other("I/O worker panicked"),
                },
            })?;
        let outcome = if matches!(
            policy,
            ProcessPolicy::GitHubMutationGit | ProcessPolicy::GitHubMutationCli
        ) {
            if let Some(credential) = &self.github_credential {
                redact_outcome(outcome, credential)
            } else {
                outcome
            }
        } else {
            outcome
        };
        match outcome {
            ProcessOutcome::Completed { status, evidence } => Ok(ExecutedOutput {
                status,
                stdout: evidence.stdout.tail_bytes.clone(),
                stderr: evidence.stderr.tail.clone(),
                evidence,
                arguments: displayed_arguments,
            }),
            outcome => Err(GitWorkspaceError::Supervision {
                program: program.to_owned(),
                arguments: displayed_arguments,
                cwd: cwd.to_owned(),
                outcome: Box::new(outcome),
            }),
        }
    }

    fn process_failure(
        &self,
        program: &Path,
        cwd: &Path,
        executed: &ExecutedOutput,
    ) -> Box<ProcessFailure> {
        Box::new(ProcessFailure {
            program: program.to_owned(),
            arguments: executed.arguments.clone(),
            cwd: cwd.to_owned(),
            status: executed.status,
            stdout: String::from_utf8_lossy(&executed.stdout).into_owned(),
            stderr: executed.stderr.clone(),
            evidence: executed.evidence.clone(),
        })
    }
}

fn retry_executable_file_busy<T>(mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    for attempt in 0..EXECUTABLE_FILE_BUSY_ATTEMPTS {
        match operation() {
            Err(source)
                if source.kind() == io::ErrorKind::ExecutableFileBusy
                    && attempt + 1 < EXECUTABLE_FILE_BUSY_ATTEMPTS =>
            {
                thread::sleep(EXECUTABLE_FILE_BUSY_BACKOFF);
            }
            result => return result,
        }
    }
    unreachable!("the final executable-file-busy attempt returns")
}

fn redact_outcome(outcome: ProcessOutcome, credential: &GitHubCredential) -> ProcessOutcome {
    match outcome {
        ProcessOutcome::Completed { status, evidence } => ProcessOutcome::Completed {
            status,
            evidence: redact_evidence(evidence, credential),
        },
        ProcessOutcome::TimedOut(evidence) => {
            ProcessOutcome::TimedOut(redact_evidence(evidence, credential))
        }
        ProcessOutcome::Cancelled(evidence) => {
            ProcessOutcome::Cancelled(redact_evidence(evidence, credential))
        }
        ProcessOutcome::OutputLimit {
            stream,
            limit,
            evidence,
        } => ProcessOutcome::OutputLimit {
            stream,
            limit,
            evidence: redact_evidence(evidence, credential),
        },
        ProcessOutcome::HeartbeatFailure { message, evidence } => {
            ProcessOutcome::HeartbeatFailure {
                message: credential.redact_text(&message),
                evidence: redact_evidence(evidence, credential),
            }
        }
        ProcessOutcome::AmbiguousExternalState { cause, evidence } => {
            ProcessOutcome::AmbiguousExternalState {
                cause: redact_interruption(cause, credential),
                evidence: redact_evidence(evidence, credential),
            }
        }
    }
}

fn redact_interruption(interruption: Interruption, credential: &GitHubCredential) -> Interruption {
    match interruption {
        Interruption::HeartbeatFailure { message } => Interruption::HeartbeatFailure {
            message: credential.redact_text(&message),
        },
        other => other,
    }
}

fn redact_evidence(
    mut evidence: ProcessEvidence,
    credential: &GitHubCredential,
) -> ProcessEvidence {
    for output in [&mut evidence.stdout, &mut evidence.stderr] {
        output.tail = credential.redact_text(&output.tail);
        output.tail_bytes = credential.redact_bytes(&output.tail_bytes);
        output.retained_bytes = output.tail_bytes.len();
    }
    evidence
}

struct CandidateSandboxDirectory {
    root: PathBuf,
    workspace: PathBuf,
}

impl CandidateSandboxDirectory {
    fn materialise(source: &Path, tree: &[CandidateTreeEntry]) -> Result<Self, GitWorkspaceError> {
        let root = std::env::temp_dir().join(format!("bokkie-candidate-{}", Uuid::new_v4()));
        fs::create_dir(&root).map_err(|source| GitWorkspaceError::Filesystem {
            path: root.clone(),
            source,
        })?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(|source| {
            GitWorkspaceError::Filesystem {
                path: root.clone(),
                source,
            }
        })?;
        let workspace = root.join("workspace");
        fs::create_dir(&workspace).map_err(|source| GitWorkspaceError::Filesystem {
            path: workspace.clone(),
            source,
        })?;
        let directory = Self { root, workspace };
        if let Err(error) = directory.copy_tree(source, tree) {
            drop(directory);
            return Err(error);
        }
        Ok(directory)
    }

    fn copy_tree(
        &self,
        source: &Path,
        tree: &[CandidateTreeEntry],
    ) -> Result<(), GitWorkspaceError> {
        for entry in tree {
            let relative = safe_manifest_path(&entry.path)?;
            let destination = self.workspace.join(relative);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|source| GitWorkspaceError::Filesystem {
                    path: parent.to_owned(),
                    source,
                })?;
            }
            match entry.mode.as_str() {
                "100644" | "100755" => {
                    let source_path = source.join(relative);
                    fs::copy(&source_path, &destination).map_err(|source| {
                        GitWorkspaceError::Filesystem {
                            path: source_path,
                            source,
                        }
                    })?;
                    let mode = if entry.mode == "100755" { 0o755 } else { 0o644 };
                    fs::set_permissions(&destination, fs::Permissions::from_mode(mode)).map_err(
                        |source| GitWorkspaceError::Filesystem {
                            path: destination.clone(),
                            source,
                        },
                    )?;
                }
                "120000" => {
                    let target = entry.symlink_target.as_deref().ok_or_else(|| {
                        GitWorkspaceError::InvalidCandidateManifest(format!(
                            "symlink {} has no target",
                            entry.path
                        ))
                    })?;
                    symlink(target, &destination).map_err(|source| {
                        GitWorkspaceError::Filesystem {
                            path: destination.clone(),
                            source,
                        }
                    })?;
                }
                mode => {
                    return Err(GitWorkspaceError::InvalidCandidateManifest(format!(
                        "candidate sandbox does not support mode {mode} at {}",
                        entry.path
                    )));
                }
            }
        }
        Ok(())
    }
}

impl Drop for CandidateSandboxDirectory {
    fn drop(&mut self) {
        let temporary_root = std::env::temp_dir();
        if self.root.parent() == Some(temporary_root.as_path())
            && self
                .root
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("bokkie-candidate-"))
        {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

fn safe_manifest_path(path: &str) -> Result<&Path, GitWorkspaceError> {
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(GitWorkspaceError::InvalidCandidateManifest(format!(
            "candidate path is not a safe relative path: {path:?}"
        )));
    }
    Ok(path)
}

fn append_system_mount(
    arguments: &mut Vec<OsString>,
    path: &Path,
) -> Result<(), GitWorkspaceError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(GitWorkspaceError::Filesystem {
                path: path.to_owned(),
                source,
            });
        }
    };
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(path).map_err(|source| GitWorkspaceError::Filesystem {
            path: path.to_owned(),
            source,
        })?;
        arguments.extend([
            OsString::from("--symlink"),
            target.into_os_string(),
            path.as_os_str().to_owned(),
        ]);
    } else {
        arguments.extend([
            OsString::from("--ro-bind"),
            path.as_os_str().to_owned(),
            path.as_os_str().to_owned(),
        ]);
    }
    Ok(())
}

fn append_optional_cache_mount(
    arguments: &mut Vec<OsString>,
    source: &Path,
    destination: &Path,
) -> Result<(), GitWorkspaceError> {
    match fs::metadata(source) {
        Ok(metadata) if metadata.is_dir() => {
            arguments.extend([
                OsString::from("--ro-bind"),
                source.as_os_str().to_owned(),
                destination.as_os_str().to_owned(),
            ]);
            Ok(())
        }
        Ok(_) => Err(GitWorkspaceError::InvalidCandidateManifest(format!(
            "candidate cache root is not a directory: {}",
            source.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source_error) => Err(GitWorkspaceError::Filesystem {
            path: source.to_owned(),
            source: source_error,
        }),
    }
}

#[derive(Clone, Copy)]
enum RemoteOperation {
    Fetch,
    WorktreeCreation,
    Push,
}

impl RemoteOperation {
    fn description(self) -> &'static str {
        match self {
            Self::Fetch => "fetch or remote observation",
            Self::WorktreeCreation => "worktree creation",
            Self::Push => "push",
        }
    }
}

fn is_canonical_repository_url(url: &str) -> bool {
    matches!(
        url,
        "https://github.com/robchristie/bokkie" | CANONICAL_HTTPS_URL
    )
}

fn is_unsafe_local_git_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key == "include.path"
        || key.starts_with("includeif.")
        || key.starts_with("credential.")
        || key == "credential.helper"
        || key.starts_with("http.")
        || key.starts_with("https.")
        || key.starts_with("filter.")
        || key.starts_with("submodule.")
        || key.starts_with("protocol.")
        || key == "core.hookspath"
        || key == "core.fsmonitor"
        || key == "core.fsmonitorhookversion"
        || key == "core.sshcommand"
        || key == "core.gitproxy"
        || key == "core.attributesfile"
        || key == "core.excludesfile"
        || key == "core.worktree"
        || key == "core.alternaterefscommand"
        || key == "core.askpass"
        || key == "core.editor"
        || key == "sequence.editor"
        || key.starts_with("pager.")
        || key.starts_with("diff.")
        || key.starts_with("merge.")
        || key.starts_with("push.")
        || key == "extensions.partialclone"
        || (key.starts_with("remote.")
            && !key.ends_with(".url")
            && !key.ends_with(".pushurl")
            && !key.ends_with(".fetch")
            && !key.ends_with(".tagopt"))
}

fn canonical_output_path(cwd: &Path, output: &str) -> Result<PathBuf, GitWorkspaceError> {
    let path = PathBuf::from(output);
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    fs::canonicalize(&path).map_err(|source| GitWorkspaceError::Filesystem { path, source })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PullRequestObservation {
    number: u64,
    url: String,
    head_ref_oid: String,
    state: String,
    is_draft: bool,
}

#[derive(Debug, Deserialize)]
struct PublicPullRequestObservation {
    number: u64,
    html_url: String,
    state: String,
    draft: bool,
    head: PublicPullRequestRef,
    base: PublicPullRequestRef,
}

#[derive(Debug, Deserialize)]
struct PublicPullRequestRef {
    #[serde(rename = "ref")]
    ref_name: String,
    sha: String,
    repo: PublicRepositoryIdentity,
}

#[derive(Debug, Deserialize)]
struct PublicRepositoryIdentity {
    full_name: String,
}

fn validate_public_pull_request_observation(
    branch: &str,
    expected_head: &CommitId,
    expected_draft: bool,
    observation: PublicPullRequestObservation,
) -> Result<PullRequestIdentity, GitWorkspaceError> {
    if observation.head.ref_name != branch
        || observation.head.repo.full_name != CANONICAL_REPOSITORY
        || observation.base.ref_name != CANONICAL_DEFAULT_BRANCH
        || observation.base.repo.full_name != CANONICAL_REPOSITORY
    {
        return Err(GitWorkspaceError::InvalidPullRequest(
            "public observation does not identify the exact canonical head and base".to_owned(),
        ));
    }
    if observation.state != "open" {
        return Err(GitWorkspaceError::InvalidPullRequest(format!(
            "expected public open state, observed {:?}",
            observation.state
        )));
    }
    validate_pull_request_observation(
        branch,
        expected_head,
        expected_draft,
        PullRequestObservation {
            number: observation.number,
            url: observation.html_url,
            head_ref_oid: observation.head.sha,
            state: "OPEN".to_owned(),
            is_draft: observation.draft,
        },
    )
}

fn validate_pull_request_observation(
    branch: &str,
    expected_head: &CommitId,
    expected_draft: bool,
    observation: PullRequestObservation,
) -> Result<PullRequestIdentity, GitWorkspaceError> {
    if observation.number == 0 {
        return Err(GitWorkspaceError::InvalidPullRequest(
            "pull-request number must be positive".to_owned(),
        ));
    }
    let expected_url = format!(
        "https://github.com/{CANONICAL_REPOSITORY}/pull/{}",
        observation.number
    );
    if observation.url != expected_url {
        return Err(GitWorkspaceError::InvalidPullRequest(format!(
            "URL {:?} does not identify canonical pull request {expected_url}",
            observation.url
        )));
    }
    let head = CommitId::parse(observation.head_ref_oid)?;
    if &head != expected_head {
        return Err(GitWorkspaceError::InvalidPullRequest(format!(
            "expected head {expected_head}, observed {head}"
        )));
    }
    if observation.state != "OPEN" {
        return Err(GitWorkspaceError::InvalidPullRequest(format!(
            "expected OPEN state, observed {:?}",
            observation.state
        )));
    }
    if observation.is_draft != expected_draft {
        return Err(GitWorkspaceError::InvalidPullRequest(if expected_draft {
            "pull request is not a draft".to_owned()
        } else {
            "pull request is a draft".to_owned()
        }));
    }
    Ok(PullRequestIdentity {
        repository: CANONICAL_REPOSITORY.to_owned(),
        number: observation.number,
        url: observation.url,
        branch: branch.to_owned(),
        head,
    })
}

fn validate_branch(branch: &str) -> Result<(), GitWorkspaceError> {
    let Some(suffix) = branch.strip_prefix(GARDENER_BRANCH_PREFIX) else {
        return Err(GitWorkspaceError::InvalidBranch(branch.to_owned()));
    };
    if suffix.is_empty()
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || suffix.starts_with('.')
        || suffix.ends_with('.')
        || suffix.contains("..")
    {
        return Err(GitWorkspaceError::InvalidBranch(branch.to_owned()));
    }
    Ok(())
}

fn percent_encode_query_value(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(&mut encoded, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    encoded
}

fn normalise_new_worktree_path(path: &Path) -> Result<PathBuf, GitWorkspaceError> {
    validate_path_shape(path)?;
    match fs::symlink_metadata(path) {
        Ok(_) => return Err(GitWorkspaceError::WorktreePathExists(path.to_owned())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(GitWorkspaceError::Filesystem {
                path: path.to_owned(),
                source,
            });
        }
    }
    let parent = path
        .parent()
        .ok_or_else(|| GitWorkspaceError::UnsafeWorktreePath(path.to_owned()))?;
    let parent = fs::canonicalize(parent).map_err(|source| GitWorkspaceError::Filesystem {
        path: parent.to_owned(),
        source,
    })?;
    let name = path
        .file_name()
        .ok_or_else(|| GitWorkspaceError::UnsafeWorktreePath(path.to_owned()))?;
    Ok(parent.join(name))
}

fn validate_existing_removal_path(path: &Path) -> Result<(), GitWorkspaceError> {
    validate_path_shape(path)?;
    let canonical = fs::canonicalize(path).map_err(|source| GitWorkspaceError::Filesystem {
        path: path.to_owned(),
        source,
    })?;
    if canonical != path {
        return Err(GitWorkspaceError::UnsafeWorktreePath(path.to_owned()));
    }
    Ok(())
}

fn validate_path_shape(path: &Path) -> Result<(), GitWorkspaceError> {
    if !path.is_absolute()
        || path.parent().is_none()
        || path
            .to_str()
            .is_none_or(|value| value.contains(['\n', '\r']))
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(GitWorkspaceError::UnsafeWorktreePath(path.to_owned()));
    }
    Ok(())
}

fn stdout(program: &Path, output: Vec<u8>) -> Result<String, GitWorkspaceError> {
    String::from_utf8(output).map_err(|source| GitWorkspaceError::NonUtf8Output {
        program: program.to_owned(),
        stream: "stdout",
        source,
    })
}

fn display_arguments(arguments: &[OsString]) -> Vec<String> {
    arguments
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tempfile::TempDir;

    use super::*;

    struct RepositoryFixture {
        root: TempDir,
        checkout: PathBuf,
        origin: PathBuf,
        git_executable: PathBuf,
        git_log: PathBuf,
        source: CommitId,
    }

    impl RepositoryFixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let origin = root.path().join("origin.git");
            let checkout = root.path().join("checkout");
            git(
                root.path(),
                [
                    "init",
                    "--bare",
                    "--initial-branch=main",
                    origin.to_str().unwrap(),
                ],
            );
            git(
                root.path(),
                ["init", "--initial-branch=main", checkout.to_str().unwrap()],
            );
            git(&checkout, ["config", "user.name", "Gardener Test"]);
            git(
                &checkout,
                ["config", "user.email", "gardener@example.invalid"],
            );
            fs::write(checkout.join("README.md"), "initial\n").unwrap();
            git(&checkout, ["add", "README.md"]);
            git(&checkout, ["commit", "-m", "Initial"]);
            git(
                &checkout,
                ["remote", "add", "origin", origin.to_str().unwrap()],
            );
            git(&checkout, ["push", "-u", "origin", "main"]);
            let source = CommitId::parse(git_stdout(&checkout, ["rev-parse", "HEAD"])).unwrap();
            git(
                &checkout,
                ["remote", "set-url", "origin", CANONICAL_HTTPS_URL],
            );
            let git_log = root.path().join("git.log");
            let git_executable = local_git_transport(root.path(), &origin, &git_log);
            Self {
                root,
                checkout,
                origin,
                git_executable,
                git_log,
                source,
            }
        }

        fn adapter(&self, gh: impl Into<PathBuf>) -> GitWorkspace {
            GitWorkspace::new(&self.checkout, &self.git_executable, gh).unwrap()
        }

        fn path(&self, name: &str) -> PathBuf {
            self.root.path().join(name)
        }
    }

    #[test]
    fn resolves_origin_main_and_creates_exact_detached_and_branch_heads() {
        let fixture = RepositoryFixture::new();
        let adapter = fixture.adapter("unused-gh");
        assert_eq!(
            adapter.resolve_origin_main(&mut NoopHeartbeat).unwrap(),
            fixture.source
        );

        let detached = adapter
            .create_detached_worktree(
                fixture.path("inspection"),
                &fixture.source,
                &mut NoopHeartbeat,
            )
            .unwrap();
        assert_eq!(detached.kind(), &WorktreeKind::Detached);
        assert_eq!(
            git_stdout(detached.path(), ["rev-parse", "HEAD"]),
            fixture.source.as_str()
        );
        assert!(git_fails(
            detached.path(),
            ["symbolic-ref", "--quiet", "HEAD"]
        ));

        let branch_name = "codex/gardener-exact-head";
        let branch = adapter
            .create_branch_worktree(
                fixture.path("implementation"),
                branch_name,
                &fixture.source,
                &mut NoopHeartbeat,
            )
            .unwrap();
        assert_eq!(branch.branch(), Some(branch_name));
        adapter
            .verify_head(&branch, &fixture.source, &mut NoopHeartbeat)
            .unwrap();
        assert_eq!(
            git_stdout(branch.path(), ["branch", "--show-current"]),
            branch_name
        );

        adapter
            .remove_clean_worktree(&detached, &mut NoopHeartbeat)
            .unwrap();
        adapter
            .remove_clean_worktree(&branch, &mut NoopHeartbeat)
            .unwrap();
    }

    #[test]
    fn commits_all_dirty_non_ignored_changes_and_pushes_only_the_named_ref() {
        let fixture = RepositoryFixture::new();
        let adapter = fixture.adapter("unused-gh");
        let branch_name = "codex/gardener-dirty-commit";
        let worktree = adapter
            .create_branch_worktree(
                fixture.path("implementation"),
                branch_name,
                &fixture.source,
                &mut NoopHeartbeat,
            )
            .unwrap();
        fs::write(worktree.path().join("README.md"), "changed\n").unwrap();
        fs::write(worktree.path().join("new.txt"), "new\n").unwrap();
        fs::write(worktree.path().join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(worktree.path().join("ignored.txt"), "ignored\n").unwrap();

        let status = adapter
            .porcelain_status(&worktree, &mut NoopHeartbeat)
            .unwrap();
        assert!(status.contains("README.md"));
        assert!(status.contains("new.txt"));
        assert!(!status.contains("ignored.txt"));
        let commit = adapter
            .commit_all(&worktree, "Test gardener commit", &mut NoopHeartbeat)
            .unwrap();
        assert_ne!(commit, fixture.source);
        assert_eq!(
            adapter
                .porcelain_status(&worktree, &mut NoopHeartbeat)
                .unwrap(),
            ""
        );
        assert!(matches!(
            adapter.commit_all(&worktree, "Nothing", &mut NoopHeartbeat),
            Err(GitWorkspaceError::NoChanges(_))
        ));

        adapter
            .push_branch(&worktree, &commit, &mut NoopHeartbeat)
            .unwrap();
        assert_eq!(
            adapter
                .observe_remote_branch(branch_name, &commit, &mut NoopHeartbeat)
                .unwrap(),
            commit
        );
        assert_eq!(
            git_stdout(
                fixture.root.path(),
                [
                    "--git-dir",
                    fixture.origin.to_str().unwrap(),
                    "rev-parse",
                    &format!("refs/heads/{branch_name}")
                ]
            ),
            commit.as_str()
        );
        assert!(git_fails(
            fixture.root.path(),
            [
                "--git-dir",
                fixture.origin.to_str().unwrap(),
                "show-ref",
                "--verify",
                "refs/heads/unrelated"
            ]
        ));
        adapter
            .remove_clean_worktree(&worktree, &mut NoopHeartbeat)
            .unwrap();
    }

    #[test]
    fn candidate_qualification_is_exact_deterministic_and_credential_free() {
        let Some(sandbox_path) = [
            Path::new("/usr/bin/bwrap"),
            Path::new("/usr/local/bin/bwrap"),
        ]
        .into_iter()
        .find(|path| path.is_file()) else {
            return;
        };
        let fixture = RepositoryFixture::new();
        let adapter = fixture.adapter("unused-gh");
        let branch = "codex/gardener-qualified";
        let worktree = adapter
            .create_branch_worktree(
                fixture.path("qualified"),
                branch,
                &fixture.source,
                &mut NoopHeartbeat,
            )
            .unwrap();
        fs::write(worktree.path().join("README.md"), "qualified\n").unwrap();
        fs::write(worktree.path().join("binary.dat"), b"before\0after").unwrap();
        symlink("README.md", worktree.path().join("readme-link")).unwrap();
        let candidate = adapter
            .commit_all(&worktree, "Qualified candidate", &mut NoopHeartbeat)
            .unwrap();

        let authoritative_sentinel = fixture.path("authoritative-state-sentinel");
        fs::write(&authoritative_sentinel, "must stay unreachable\n").unwrap();
        let host_network_namespace = fs::read_link("/proc/self/ns/net").unwrap();
        let check = fixture.path("candidate-check");
        write_executable(
            &check,
            &format!(
                "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'candidate-check 1'; exit 0; fi\n[ \"$HOME\" = /home/bokkie ] || exit 21\n[ ! -e .git ] || exit 22\n[ ! -e '{}' ] || exit 23\n[ \"$(readlink /proc/self/ns/net)\" != '{}' ] || exit 24\n[ -z \"${{GH_TOKEN-}}${{GITHUB_TOKEN-}}${{AWS_SECRET_ACCESS_KEY-}}${{SSH_AUTH_SOCK-}}\" ] || exit 25\nif touch /usr/bokkie-sandbox-write 2>/dev/null; then exit 26; fi\nprintf 'sandbox mutation\\n' > README.md\nprintf 'checked\\n'\n",
                shell_single_quote(&authoritative_sentinel),
                host_network_namespace.to_string_lossy(),
            ),
        );
        let environment = ChildEnvironment::new(
            fixture.path("home"),
            fixture.path("config"),
            fixture.path("cache"),
            vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")],
        )
        .unwrap();
        let supervisor = ProcessSupervisor::new(
            Duration::from_millis(10),
            ProcessLimits::default(),
            CancellationToken::new(),
        )
        .unwrap();
        let identity = ExecutableIdentity::resolve(
            ExecutableRole::CandidateCheck,
            &check,
            &["--version"],
            &environment,
            &supervisor,
            Duration::from_secs(1),
            &mut NoopHeartbeat,
        )
        .unwrap();
        let sandbox = ExecutableIdentity::resolve(
            ExecutableRole::CandidateSandbox,
            sandbox_path,
            &["--version"],
            &environment,
            &supervisor,
            Duration::from_secs(1),
            &mut NoopHeartbeat,
        )
        .unwrap();
        let command = CandidateCheckCommand::sandboxed(sandbox, identity, ["check"]).unwrap();

        let qualification = adapter
            .qualify_candidate(
                &worktree,
                &fixture.source,
                &candidate,
                &[command],
                &mut NoopHeartbeat,
            )
            .unwrap();

        assert_eq!(qualification.manifest.source, fixture.source);
        assert_eq!(qualification.manifest.candidate, candidate);
        assert_eq!(qualification.manifest.sha256.len(), 64);
        let binary = qualification
            .manifest
            .tree
            .iter()
            .find(|entry| entry.path == "binary.dat")
            .unwrap();
        assert!(binary.binary);
        assert_eq!(binary.byte_size, Some(12));
        let link = qualification
            .manifest
            .tree
            .iter()
            .find(|entry| entry.path == "readme-link")
            .unwrap();
        assert_eq!(link.mode, "120000");
        assert_eq!(link.symlink_target.as_deref(), Some("README.md"));
        assert_eq!(qualification.checks.len(), 1);
        assert_eq!(qualification.checks[0].status, CandidateCheckStatus::Passed);
        assert_eq!(qualification.checks[0].evidence.stdout.tail, "checked\n");
        assert!(qualification.checks[0].sandbox.is_some());
        assert_eq!(
            fs::read_to_string(worktree.path().join("README.md")).unwrap(),
            "qualified\n"
        );
        assert_eq!(
            fs::read_to_string(authoritative_sentinel).unwrap(),
            "must stay unreachable\n"
        );
    }

    #[test]
    fn trusted_publication_creates_draft_then_promotes_with_narrow_credentials() {
        let fixture = RepositoryFixture::new();
        let state = fixture.path("gh-state");
        let log = fixture.path("gh-trust.log");
        let observer_log = fixture.path("observer-trust.log");
        let gh = fixture.path("trusted-gh");
        let observer = fixture.path("trusted-curl");
        write_executable(
            &gh,
            &format!(
                r#"#!/bin/sh
if [ "$1" = --version ]; then echo 'gh version 1'; exit 0; fi
if [ -n "${{GH_TOKEN-}}" ]; then auth=credential; else auth=none; fi
printf '%s|%s\n' "$auth" "$*" >> '{}'
case "$1 $2" in
  'pr create') printf draft > '{}' ;;
  'pr ready') printf ready > '{}' ;;
esac
"#,
                shell_single_quote(&log),
                shell_single_quote(&state),
                shell_single_quote(&state),
            ),
        );
        write_executable(
            &observer,
            &format!(
                r#"#!/bin/sh
if [ "$1" = --version ]; then echo 'curl 1'; exit 0; fi
if [ -n "${{GH_TOKEN-}}" ] || [ -n "${{GITHUB_TOKEN-}}" ]; then auth=credential; else auth=none; fi
printf '%s|%s\n' "$auth" "$*" >> '{}'
if [ "$(cat '{}')" = draft ]; then draft=true; else draft=false; fi
printf '[{{"number":42,"html_url":"https://github.com/robchristie/bokkie/pull/42","state":"open","draft":%s,"head":{{"ref":"codex/gardener-draft-ready","sha":"{}","repo":{{"full_name":"robchristie/bokkie"}}}},"base":{{"ref":"main","sha":"{}","repo":{{"full_name":"robchristie/bokkie"}}}}}}]\n' "$draft"
"#,
                shell_single_quote(&observer_log),
                shell_single_quote(&state),
                fixture.source,
                fixture.source,
            ),
        );
        let environment = ChildEnvironment::new(
            fixture.path("home"),
            fixture.path("config"),
            fixture.path("cache"),
            vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")],
        )
        .unwrap();
        let supervisor = ProcessSupervisor::new(
            Duration::from_millis(10),
            ProcessLimits::default(),
            CancellationToken::new(),
        )
        .unwrap();
        let git_identity = ExecutableIdentity::resolve(
            ExecutableRole::Git,
            &fixture.git_executable,
            &["--version"],
            &environment,
            &supervisor,
            Duration::from_secs(1),
            &mut NoopHeartbeat,
        )
        .unwrap();
        let gh_identity = ExecutableIdentity::resolve(
            ExecutableRole::GitHub,
            &gh,
            &["--version"],
            &environment,
            &supervisor,
            Duration::from_secs(1),
            &mut NoopHeartbeat,
        )
        .unwrap();
        let observer_identity = ExecutableIdentity::resolve(
            ExecutableRole::GitHubPublicObserver,
            &observer,
            &["--version"],
            &environment,
            &supervisor,
            Duration::from_secs(1),
            &mut NoopHeartbeat,
        )
        .unwrap();
        let adapter = GitWorkspace::from_trust(
            &fixture.checkout,
            git_identity,
            gh_identity,
            observer_identity,
            environment,
            &mut NoopHeartbeat,
        )
        .unwrap()
        .with_github_credential(GitHubCredential::new("fake-token").unwrap());
        let branch = "codex/gardener-draft-ready";
        let worktree = adapter
            .create_branch_worktree(
                fixture.path("draft-ready"),
                branch,
                &fixture.source,
                &mut NoopHeartbeat,
            )
            .unwrap();
        adapter
            .push_branch(&worktree, &fixture.source, &mut NoopHeartbeat)
            .unwrap();

        adapter
            .create_draft_pull_request(
                branch,
                &fixture.source,
                "Draft candidate",
                "Body",
                &mut NoopHeartbeat,
            )
            .unwrap();
        adapter
            .mark_pull_request_ready(branch, &fixture.source, &mut NoopHeartbeat)
            .unwrap();

        let log = fs::read_to_string(log).unwrap();
        assert!(log.contains("credential|pr create"));
        assert!(log.contains("--draft"));
        assert!(log.contains("credential|pr ready"));
        assert!(!log.contains("pr view"));
        assert!(!log.contains("fake-token"));
        let observer_log = fs::read_to_string(observer_log).unwrap();
        assert!(observer_log.contains("none|--disable --fail-with-body"));
        assert!(observer_log.contains("https://api.github.com/repos/robchristie/bokkie/pulls"));
        assert!(!observer_log.contains("credential"));
        assert!(!observer_log.contains("fake-token"));
    }

    #[test]
    fn real_gh_requires_authentication_even_for_public_pr_view() {
        let Some(gh) = [Path::new("/usr/bin/gh"), Path::new("/usr/local/bin/gh")]
            .into_iter()
            .find(|path| path.is_file())
        else {
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let environment = ChildEnvironment::new(
            root.path().join("home"),
            root.path().join("config"),
            root.path().join("cache"),
            vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")],
        )
        .unwrap();
        let supervisor = ProcessSupervisor::new(
            Duration::from_millis(10),
            ProcessLimits::default(),
            CancellationToken::new(),
        )
        .unwrap();
        let mut command = Command::new(gh);
        command.args([
            "pr",
            "view",
            "1",
            "--repo",
            CANONICAL_REPOSITORY,
            "--json",
            "number",
        ]);
        environment
            .apply(&mut command, ProcessPolicy::GitHubRead, None)
            .unwrap();
        let mut child = supervisor
            .spawn(
                &mut command,
                Instant::now() + Duration::from_secs(3),
                EffectRisk::None,
            )
            .unwrap();
        child.close_stdin();
        let outcome = child.wait(&mut NoopHeartbeat).unwrap();
        assert!(matches!(
            outcome,
            ProcessOutcome::Completed { status, .. } if status.code() == Some(4)
        ));
    }

    #[test]
    fn unsafe_local_git_configuration_blocks_before_credentialled_processes() {
        let fixture = RepositoryFixture::new();
        let gh = fixture.path("version-gh");
        let observer = fixture.path("version-curl");
        write_executable(&gh, "#!/bin/sh\necho 'gh version 1'\n");
        write_executable(&observer, "#!/bin/sh\necho 'curl 1'\n");
        let environment = ChildEnvironment::new(
            fixture.path("home"),
            fixture.path("config"),
            fixture.path("cache"),
            vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")],
        )
        .unwrap();
        let supervisor = ProcessSupervisor::new(
            Duration::from_millis(10),
            ProcessLimits::default(),
            CancellationToken::new(),
        )
        .unwrap();
        let git_identity = ExecutableIdentity::resolve(
            ExecutableRole::Git,
            &fixture.git_executable,
            &["--version"],
            &environment,
            &supervisor,
            Duration::from_secs(1),
            &mut NoopHeartbeat,
        )
        .unwrap();
        let gh_identity = ExecutableIdentity::resolve(
            ExecutableRole::GitHub,
            &gh,
            &["--version"],
            &environment,
            &supervisor,
            Duration::from_secs(1),
            &mut NoopHeartbeat,
        )
        .unwrap();
        let observer_identity = ExecutableIdentity::resolve(
            ExecutableRole::GitHubPublicObserver,
            &observer,
            &["--version"],
            &environment,
            &supervisor,
            Duration::from_secs(1),
            &mut NoopHeartbeat,
        )
        .unwrap();
        let adapter = GitWorkspace::from_trust(
            &fixture.checkout,
            git_identity,
            gh_identity,
            observer_identity,
            environment,
            &mut NoopHeartbeat,
        )
        .unwrap()
        .with_github_credential(GitHubCredential::new("fake-token").unwrap());
        let worktree = adapter
            .create_branch_worktree(
                fixture.path("unsafe-config"),
                "codex/gardener-unsafe-config",
                &fixture.source,
                &mut NoopHeartbeat,
            )
            .unwrap();

        for (key, value) in [
            ("http.proxy", "http://hostile.invalid"),
            ("http.sslVerify", "false"),
            ("include.path", "/hostile/include"),
            ("credential.helper", "!hostile-helper"),
            ("filter.hostile.clean", "/hostile/filter"),
            ("core.fsmonitor", "/hostile/fsmonitor"),
            ("remote.origin.uploadpack", "/hostile/upload-pack"),
        ] {
            git(&fixture.checkout, ["config", key, value]);
            fs::write(&fixture.git_log, "").unwrap();

            let error = adapter
                .push_branch(&worktree, &fixture.source, &mut NoopHeartbeat)
                .unwrap_err();

            assert!(matches!(
                error,
                GitWorkspaceError::UnsafeLocalGitConfig { ref keys, .. }
                    if keys == &[key.to_ascii_lowercase()]
            ));
            let log = fs::read_to_string(&fixture.git_log).unwrap();
            assert!(!log.lines().any(|line| line.starts_with("push ")));
            git(&fixture.checkout, ["config", "--unset-all", key]);
        }

        git(
            &fixture.checkout,
            ["config", "extensions.worktreeConfig", "true"],
        );
        git(
            &fixture.checkout,
            [
                "config",
                "--worktree",
                "http.proxy",
                "http://hostile.invalid",
            ],
        );
        fs::write(&fixture.git_log, "").unwrap();
        let error = adapter
            .push_branch(&worktree, &fixture.source, &mut NoopHeartbeat)
            .unwrap_err();
        assert!(
            matches!(
                &error,
                GitWorkspaceError::UnsafeLocalGitConfig { keys, .. }
                    if keys == &["http.proxy"]
            ),
            "unexpected error: {error:?}"
        );
        assert!(
            !fs::read_to_string(&fixture.git_log)
                .unwrap()
                .lines()
                .any(|line| line.starts_with("push "))
        );
    }

    #[test]
    fn remote_branch_observation_rejects_a_missing_or_mismatched_ref() {
        let fixture = RepositoryFixture::new();
        let adapter = fixture.adapter("unused-gh");
        let branch = "codex/gardener-observed";
        assert!(matches!(
            adapter.observe_remote_branch(branch, &fixture.source, &mut NoopHeartbeat),
            Err(GitWorkspaceError::InvalidRemoteBranch(message))
                if message.contains("was not found")
        ));

        git(
            &fixture.checkout,
            ["branch", branch, fixture.source.as_str()],
        );
        git(
            &fixture.checkout,
            ["push", fixture.origin.to_str().unwrap(), branch],
        );
        let other = CommitId::parse("a".repeat(40)).unwrap();
        assert!(matches!(
            adapter.observe_remote_branch(branch, &other, &mut NoopHeartbeat),
            Err(GitWorkspaceError::InvalidRemoteBranch(message))
                if message.contains("expected head")
        ));
    }

    #[test]
    fn rejects_a_noncanonical_effective_fetch_url_before_fetching_or_creating_a_worktree() {
        let fixture = RepositoryFixture::new();
        let noncanonical = fixture.path("noncanonical-fetch.git");
        git(
            fixture.root.path(),
            [
                "init",
                "--bare",
                "--initial-branch=main",
                noncanonical.to_str().unwrap(),
            ],
        );
        git(
            &fixture.checkout,
            ["remote", "set-url", "origin", "test-fetch:"],
        );
        git(
            &fixture.checkout,
            [
                "config",
                &format!("url.file://{}.insteadOf", noncanonical.display()),
                "test-fetch:",
            ],
        );
        fs::write(&fixture.git_log, "").unwrap();

        let error = fixture
            .adapter("unused-gh")
            .resolve_origin_main(&mut NoopHeartbeat)
            .unwrap_err();

        assert!(matches!(
            error,
            GitWorkspaceError::NonCanonicalOrigin {
                operation: "fetch or remote observation",
                ref urls,
            } if urls == &[format!("file://{}", noncanonical.display())]
        ));
        let log = fs::read_to_string(&fixture.git_log).unwrap();
        assert!(log.contains("remote get-url --all origin"));
        assert!(!log.lines().any(|line| line.starts_with("fetch ")));
        assert_eq!(
            git_stdout(&fixture.checkout, ["rev-parse", "refs/remotes/origin/main"]),
            fixture.source.as_str()
        );

        let worktree_path = fixture.path("rejected-worktree");
        assert!(matches!(
            fixture.adapter("unused-gh").create_detached_worktree(
                &worktree_path,
                &fixture.source,
                &mut NoopHeartbeat,
            ),
            Err(GitWorkspaceError::NonCanonicalOrigin {
                operation: "worktree creation",
                ..
            })
        ));
        assert!(!worktree_path.exists());
        let log = fs::read_to_string(&fixture.git_log).unwrap();
        assert!(!log.contains("worktree add"));
    }

    #[test]
    fn rechecks_and_rejects_a_noncanonical_effective_push_url_before_pushing() {
        let fixture = RepositoryFixture::new();
        let adapter = fixture.adapter("unused-gh");
        let branch_name = "codex/gardener-rejected-push";
        let worktree = adapter
            .create_branch_worktree(
                fixture.path("implementation"),
                branch_name,
                &fixture.source,
                &mut NoopHeartbeat,
            )
            .unwrap();
        fs::write(worktree.path().join("README.md"), "changed\n").unwrap();
        let commit = adapter
            .commit_all(&worktree, "Test rejected push", &mut NoopHeartbeat)
            .unwrap();

        let noncanonical = fixture.path("noncanonical-push.git");
        git(
            fixture.root.path(),
            [
                "init",
                "--bare",
                "--initial-branch=main",
                noncanonical.to_str().unwrap(),
            ],
        );
        git(
            &fixture.checkout,
            [
                "config",
                &format!("url.file://{}.pushInsteadOf", noncanonical.display()),
                CANONICAL_HTTPS_URL,
            ],
        );
        fs::write(&fixture.git_log, "").unwrap();

        let error = adapter
            .push_branch(&worktree, &commit, &mut NoopHeartbeat)
            .unwrap_err();

        assert!(matches!(
            error,
            GitWorkspaceError::NonCanonicalOrigin {
                operation: "push",
                ref urls,
            } if urls == &[format!("file://{}", noncanonical.display())]
        ));
        let log = fs::read_to_string(&fixture.git_log).unwrap();
        assert!(log.contains("remote get-url --push --all origin"));
        assert!(!log.lines().any(|line| line.starts_with("push ")));
        for remote in [&fixture.origin, &noncanonical] {
            assert!(git_fails(
                fixture.root.path(),
                [
                    "--git-dir",
                    remote.to_str().unwrap(),
                    "show-ref",
                    "--verify",
                    &format!("refs/heads/{branch_name}")
                ]
            ));
        }
    }

    #[test]
    fn refuses_branch_and_path_collisions() {
        let fixture = RepositoryFixture::new();
        let adapter = fixture.adapter("unused-gh");
        let branch = "codex/gardener-collision";
        git(
            &fixture.checkout,
            ["branch", branch, fixture.source.as_str()],
        );
        assert!(matches!(
            adapter.create_branch_worktree(
                fixture.path("branch"),
                branch,
                &fixture.source,
                &mut NoopHeartbeat,
            ),
            Err(GitWorkspaceError::BranchExists(found)) if found == branch
        ));

        let existing = fixture.path("existing");
        fs::create_dir(&existing).unwrap();
        assert!(matches!(
            adapter.create_detached_worktree(
                &existing,
                &fixture.source,
                &mut NoopHeartbeat,
            ),
            Err(GitWorkspaceError::WorktreePathExists(path)) if path == existing
        ));
    }

    #[test]
    fn creates_and_observes_a_ready_pr_from_structured_json() {
        let fixture = RepositoryFixture::new();
        let log = fixture.path("gh.log");
        let gh = fake_gh(
            fixture.root.path(),
            &log,
            &format!(
                r#"{{"number":42,"url":"https://github.com/robchristie/bokkie/pull/42","headRefOid":"{}","state":"OPEN","isDraft":false}}"#,
                fixture.source
            ),
        );
        let adapter = fixture.adapter(gh);
        let branch = "codex/gardener-ready-pr";
        let identity = adapter
            .create_ready_pull_request(
                branch,
                &fixture.source,
                "A title",
                "A body",
                &mut NoopHeartbeat,
            )
            .unwrap();
        assert_eq!(
            identity,
            PullRequestIdentity {
                repository: CANONICAL_REPOSITORY.to_owned(),
                number: 42,
                url: "https://github.com/robchristie/bokkie/pull/42".to_owned(),
                branch: branch.to_owned(),
                head: fixture.source.clone(),
            }
        );
        let arguments = fs::read_to_string(log).unwrap();
        assert!(arguments.contains("create\n--repo\nrobchristie/bokkie\n--base\nmain"));
        assert!(arguments.contains("view\ncodex/gardener-ready-pr\n--repo\nrobchristie/bokkie"));
        assert!(!arguments.contains("--draft"));
        assert!(!arguments.contains("merge"));
    }

    #[test]
    fn rejects_pr_head_mismatch_and_non_ready_observations() {
        let fixture = RepositoryFixture::new();
        let other_head = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let log = fixture.path("gh.log");
        let gh = fake_gh(
            fixture.root.path(),
            &log,
            &format!(
                r#"{{"number":7,"url":"https://github.com/robchristie/bokkie/pull/7","headRefOid":"{other_head}","state":"OPEN","isDraft":false}}"#
            ),
        );
        let error = fixture
            .adapter(gh)
            .observe_pull_request(
                "codex/gardener-mismatch",
                &fixture.source,
                &mut NoopHeartbeat,
            )
            .unwrap_err();
        assert!(error.to_string().contains("expected head"));

        let gh = fake_gh(
            fixture.root.path(),
            &fixture.path("draft.log"),
            &format!(
                r#"{{"number":8,"url":"https://github.com/robchristie/bokkie/pull/8","headRefOid":"{}","state":"OPEN","isDraft":true}}"#,
                fixture.source
            ),
        );
        let error = fixture
            .adapter(gh)
            .observe_pull_request("codex/gardener-draft", &fixture.source, &mut NoopHeartbeat)
            .unwrap_err();
        assert!(error.to_string().contains("draft"));
    }

    #[test]
    fn rejects_malformed_json_and_a_noncanonical_pr_url() {
        let fixture = RepositoryFixture::new();
        let gh = fake_gh(
            fixture.root.path(),
            &fixture.path("malformed.log"),
            "not JSON",
        );
        assert!(matches!(
            fixture.adapter(gh).observe_pull_request(
                "codex/gardener-malformed",
                &fixture.source,
                &mut NoopHeartbeat,
            ),
            Err(GitWorkspaceError::InvalidPullRequestJson(_))
        ));

        let gh = fake_gh(
            fixture.root.path(),
            &fixture.path("repository.log"),
            &format!(
                r#"{{"number":9,"url":"https://github.com/someone/else/pull/9","headRefOid":"{}","state":"OPEN","isDraft":false}}"#,
                fixture.source
            ),
        );
        let error = fixture
            .adapter(gh)
            .observe_pull_request(
                "codex/gardener-repository",
                &fixture.source,
                &mut NoopHeartbeat,
            )
            .unwrap_err();
        assert!(error.to_string().contains("canonical pull request"));
    }

    #[test]
    fn cleanup_refuses_unsafe_dirty_and_foreign_worktrees() {
        let fixture = RepositoryFixture::new();
        let adapter = fixture.adapter("unused-gh");
        assert!(matches!(
            normalise_new_worktree_path(Path::new("/")),
            Err(GitWorkspaceError::UnsafeWorktreePath(_))
        ));
        assert!(matches!(
            normalise_new_worktree_path(Path::new("relative")),
            Err(GitWorkspaceError::UnsafeWorktreePath(_))
        ));

        let worktree = adapter
            .create_detached_worktree(
                fixture.path("retained"),
                &fixture.source,
                &mut NoopHeartbeat,
            )
            .unwrap();
        fs::write(worktree.path().join("untracked.txt"), "retain me\n").unwrap();
        assert!(matches!(
            adapter.remove_clean_worktree(&worktree, &mut NoopHeartbeat),
            Err(GitWorkspaceError::DirtyWorktree { .. })
        ));
        assert!(worktree.path().exists());

        let other = RepositoryFixture::new();
        assert!(matches!(
            other
                .adapter("unused-gh")
                .remove_clean_worktree(&worktree, &mut NoopHeartbeat),
            Err(GitWorkspaceError::WrongCheckout { .. })
        ));
        assert!(worktree.path().exists());
    }

    #[test]
    fn validates_exact_identity_and_dedicated_branch_names() {
        assert!(CommitId::parse("a".repeat(40)).is_ok());
        assert!(CommitId::parse("A".repeat(40)).is_err());
        assert!(CommitId::parse("a".repeat(39)).is_err());
        assert!(validate_branch("codex/gardener-valid_1.2").is_ok());
        assert!(validate_branch("codex/not-gardener").is_err());
        assert!(validate_branch("codex/gardener-").is_err());
        assert!(validate_branch("codex/gardener-../main").is_err());
    }

    #[test]
    fn retries_only_pre_start_executable_file_busy_errors() {
        let busy_attempts = Cell::new(0);
        let result = retry_executable_file_busy(|| {
            let attempt = busy_attempts.get();
            busy_attempts.set(attempt + 1);
            if attempt < 3 {
                Err(io::Error::from(io::ErrorKind::ExecutableFileBusy))
            } else {
                Ok(())
            }
        });
        assert!(result.is_ok());
        assert_eq!(busy_attempts.get(), 4);

        let persistent_busy_attempts = Cell::new(0);
        let error = retry_executable_file_busy(|| {
            persistent_busy_attempts.set(persistent_busy_attempts.get() + 1);
            Err::<(), _>(io::Error::from(io::ErrorKind::ExecutableFileBusy))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ExecutableFileBusy);
        assert_eq!(persistent_busy_attempts.get(), 4);

        let other_attempts = Cell::new(0);
        let error = retry_executable_file_busy(|| {
            other_attempts.set(other_attempts.get() + 1);
            Err::<(), _>(io::Error::from(io::ErrorKind::PermissionDenied))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(other_attempts.get(), 1);
    }

    #[test]
    fn started_non_zero_process_is_not_retried() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("invocations.log");
        let executable = root.path().join("failing-process");
        write_executable(
            &executable,
            &format!(
                "#!/bin/sh\nprintf invoked >> '{}'\nexit 23\n",
                shell_single_quote(&log)
            ),
        );
        let workspace = GitWorkspace::new(root.path(), &executable, &executable).unwrap();

        assert!(matches!(
            workspace.gh_success(
                std::iter::empty::<OsString>(),
                EffectRisk::None,
                &mut NoopHeartbeat,
            ),
            Err(GitWorkspaceError::Command(_))
        ));
        assert_eq!(fs::read_to_string(log).unwrap(), "invoked");
    }

    #[test]
    fn shutdown_cancellation_stops_a_running_git_command() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("never-git");
        write_executable(&executable, "#!/bin/sh\nwhile :; do :; done\n");
        let cancellation = CancellationToken::new();
        let cancelling = cancellation.clone();
        let workspace = GitWorkspace::new(root.path(), &executable, "unused-gh")
            .unwrap()
            .with_supervision(
                Duration::from_millis(5),
                Duration::from_secs(2),
                ProcessLimits::default(),
                cancellation,
            )
            .unwrap();
        let cancel_thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            cancelling.cancel();
        });

        let started = Instant::now();
        let error = workspace
            .git_success(
                root.path(),
                std::iter::empty::<OsString>(),
                EffectRisk::None,
                &mut NoopHeartbeat,
            )
            .unwrap_err();
        cancel_thread.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));

        let GitWorkspaceError::Supervision { outcome, .. } = error else {
            panic!("expected supervised cancellation");
        };
        assert!(matches!(outcome.as_ref(), ProcessOutcome::Cancelled(_)));
    }

    #[test]
    fn interrupted_gh_mutation_reports_ambiguous_external_state() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("never-gh");
        write_executable(&executable, "#!/bin/sh\nwhile :; do :; done\n");
        let cancellation = CancellationToken::new();
        let cancelling = cancellation.clone();
        let workspace = GitWorkspace::new(root.path(), "unused-git", &executable)
            .unwrap()
            .with_supervision(
                Duration::from_millis(5),
                Duration::from_secs(2),
                ProcessLimits::default(),
                cancellation,
            )
            .unwrap();
        let cancel_thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            cancelling.cancel();
        });
        let head = CommitId::parse("a".repeat(40)).unwrap();

        let error = workspace
            .create_ready_pull_request(
                "codex/gardener-ambiguous",
                &head,
                "A title",
                "A body",
                &mut NoopHeartbeat,
            )
            .unwrap_err();
        cancel_thread.join().unwrap();

        assert!(error.is_ambiguous_external_state());
    }

    fn fake_gh(root: &Path, log: &Path, json: &str) -> PathBuf {
        let script = root.join(format!("gh-{}", log.file_stem().unwrap().to_string_lossy()));
        let contents = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nif [ \"$1\" = pr ] && [ \"$2\" = view ]; then\n  printf '%s\\n' '{}'\nfi\n",
            shell_single_quote(log),
            shell_single_quote(Path::new(json)),
        );
        write_executable(&script, &contents);
        script
    }

    fn local_git_transport(root: &Path, origin: &Path, log: &Path) -> PathBuf {
        let script = root.join("git-with-local-canonical-transport");
        let rewrite = format!(
            "url.file://{}.insteadOf={CANONICAL_HTTPS_URL}",
            origin.display()
        );
        let contents = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$1\" in\n  fetch|push|ls-remote) exec git -c '{}' \"$@\" ;;\n  *) exec git \"$@\" ;;\nesac\n",
            shell_single_quote(log),
            shell_single_quote(Path::new(&rewrite)),
        );
        write_executable(&script, &contents);
        script
    }

    fn write_executable(path: &Path, contents: &str) {
        static NEXT_TEMPORARY_FILE: AtomicUsize = AtomicUsize::new(0);

        let temporary_path = path.with_file_name(format!(
            ".{}.{}.tmp",
            path.file_name().unwrap().to_string_lossy(),
            NEXT_TEMPORARY_FILE.fetch_add(1, Ordering::Relaxed),
        ));
        let mut temporary = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
            .unwrap();
        temporary.write_all(contents.as_bytes()).unwrap();
        let mut permissions = temporary.metadata().unwrap().permissions();
        permissions.set_mode(0o755);
        temporary.set_permissions(permissions).unwrap();
        temporary.sync_all().unwrap();
        drop(temporary);
        fs::rename(temporary_path, path).unwrap();
    }

    fn shell_single_quote(path: &Path) -> String {
        path.to_string_lossy().replace('\'', "'\\''")
    }

    fn git<I, S>(cwd: &Path, arguments: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_stdout<I, S>(cwd: &Path, arguments: I) -> String
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn git_fails<I, S>(cwd: &Path, arguments: I) -> bool
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        !Command::new("git")
            .args(arguments)
            .current_dir(cwd)
            .output()
            .unwrap()
            .status
            .success()
    }
}
