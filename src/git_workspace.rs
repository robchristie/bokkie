//! Narrow Git and GitHub process adapter for the coding gardener.
//!
//! The adapter deliberately owns no durable workflow state. Callers persist
//! intent before invoking each external operation and persist the exact
//! identities returned here before moving to the next operation.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use thiserror::Error;

#[cfg(test)]
use crate::process::NoopHeartbeat;
use crate::process::{
    CancellationToken, EffectRisk, ProcessError, ProcessEvidence, ProcessHeartbeat, ProcessLimits,
    ProcessOutcome, ProcessSupervisor,
};

pub const CANONICAL_REPOSITORY: &str = "robchristie/bokkie";
pub const CANONICAL_DEFAULT_BRANCH: &str = "main";
pub const GARDENER_BRANCH_PREFIX: &str = "codex/gardener-";
const CANONICAL_HTTPS_URL: &str = "https://github.com/robchristie/bokkie.git";
const EXECUTABLE_FILE_BUSY_ATTEMPTS: usize = 4;
const EXECUTABLE_FILE_BUSY_BACKOFF: Duration = Duration::from_millis(5);
const DEFAULT_EXECUTION_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

/// An exact SHA-1 Git commit identity.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
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
    heartbeat_interval: Duration,
    execution_timeout: Duration,
    process_limits: ProcessLimits,
    cancellation: CancellationToken,
}

impl GitWorkspace {
    pub fn new(
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
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            execution_timeout: DEFAULT_EXECUTION_TIMEOUT,
            process_limits: ProcessLimits::default(),
            cancellation: CancellationToken::new(),
        })
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
        self.git_success(
            &self.checkout,
            ["fetch", "--quiet", "origin", CANONICAL_DEFAULT_BRANCH],
            EffectRisk::None,
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
        let branch = worktree
            .branch()
            .ok_or_else(|| GitWorkspaceError::InvalidBranch("detached worktree".to_owned()))?;
        validate_branch(branch)?;
        self.verify_branch_path(&worktree.path, branch, heartbeat)?;
        self.verify_head_path(&worktree.path, expected_head, heartbeat)?;
        let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
        self.ensure_canonical_origin(&worktree.path, RemoteOperation::Push, heartbeat)?;
        self.git_success(
            &worktree.path,
            ["push", "origin", refspec.as_str()],
            EffectRisk::AmbiguousOnInterruption,
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
        let output = self.git_success(
            &self.checkout,
            ["ls-remote", "--refs", "origin", reference.as_str()],
            EffectRisk::None,
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
    pub fn create_ready_pull_request(
        &self,
        branch: &str,
        expected_head: &CommitId,
        title: &str,
        body: &str,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<PullRequestIdentity, GitWorkspaceError> {
        validate_branch(branch)?;
        if title.trim().is_empty() {
            return Err(GitWorkspaceError::InvalidPullRequest(
                "title must not be empty".to_owned(),
            ));
        }
        self.gh_success(
            [
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
            ],
            EffectRisk::AmbiguousOnInterruption,
            heartbeat,
        )?;
        self.observe_pull_request(branch, expected_head, heartbeat)
    }

    /// Observes a PR exclusively through structured `gh` JSON output.
    pub fn observe_pull_request(
        &self,
        branch: &str,
        expected_head: &CommitId,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<PullRequestIdentity, GitWorkspaceError> {
        validate_branch(branch)?;
        let output = self.gh_success(
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
            heartbeat,
        )?;
        let observation: PullRequestObservation = serde_json::from_str(&output)?;
        validate_pull_request_observation(branch, expected_head, observation)
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
        let output = self.git_output(cwd, arguments, risk, heartbeat)?;
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
        self.run(&self.git_executable, cwd, arguments, risk, heartbeat)
    }

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
        let output = self.run(
            &self.gh_executable,
            &self.checkout,
            arguments,
            risk,
            heartbeat,
        )?;
        if !output.status.success() {
            return Err(self
                .process_failure(&self.gh_executable, &self.checkout, &output)
                .into());
        }
        stdout(&self.gh_executable, output.stdout)
    }

    fn run<I, S>(
        &self,
        program: &Path,
        cwd: &Path,
        arguments: I,
        risk: EffectRisk,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<ExecutedOutput, GitWorkspaceError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let arguments = arguments.into_iter().map(Into::into).collect::<Vec<_>>();
        let displayed_arguments = display_arguments(&arguments);
        self.run_observed(
            program,
            cwd,
            arguments,
            displayed_arguments,
            risk,
            heartbeat,
        )
    }

    fn run_observed<I, S>(
        &self,
        program: &Path,
        cwd: &Path,
        arguments: I,
        displayed_arguments: Vec<String>,
        risk: EffectRisk,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<ExecutedOutput, GitWorkspaceError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let arguments = arguments.into_iter().map(Into::into).collect::<Vec<_>>();
        let supervisor = ProcessSupervisor::new(
            self.heartbeat_interval,
            self.process_limits,
            self.cancellation.clone(),
        )
        .map_err(GitWorkspaceError::InvalidSupervision)?;
        let mut child = retry_executable_file_busy(|| {
            let mut command = Command::new(program);
            command.args(&arguments).current_dir(cwd);
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
        "https://github.com/robchristie/bokkie"
            | CANONICAL_HTTPS_URL
            | "git@github.com:robchristie/bokkie"
            | "git@github.com:robchristie/bokkie.git"
            | "ssh://git@github.com/robchristie/bokkie"
            | "ssh://git@github.com/robchristie/bokkie.git"
    )
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

fn validate_pull_request_observation(
    branch: &str,
    expected_head: &CommitId,
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
    if observation.is_draft {
        return Err(GitWorkspaceError::InvalidPullRequest(
            "pull request is a draft".to_owned(),
        ));
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
