//! Bounded coding-gardener execution coordinator.
//!
//! The coordinator persists each external identity before moving to the next
//! effect. It deliberately leaves obligation transitions to [`Store`] and
//! never places Git, GitHub, or Codex work inside a database transaction.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    Claim, Completion, FailureDisposition, GardenerCandidateQualification,
    GardenerImplementationResult, GardenerObligationKind, GardenerReproducibilityManifest,
    GardenerVerificationResult, GardenerVerificationVerdict, InspectionResult,
    MAX_COMPLETION_ERROR_CHARS, MAX_COMPLETION_EVIDENCE_CHARS, NewGardenerImplementationRun,
    NewGardenerInspection, RunResult, Store, StoreError, UnixClock,
    app_server::{AppServerClient, AppServerError, AppServerObserver, TurnKind, TurnRequest},
    gardener::{
        CANONICAL_REPOSITORY, MAX_GARDENER_MODEL_ITEM_CHARS, MAX_GARDENER_MODEL_ITEMS,
        MAX_GARDENER_MODEL_TEXT_CHARS, MAX_GARDENER_PROMPT_CHARS, MAX_GARDENER_PROMPTS,
    },
    git_workspace::{
        CandidateCheckCommand, CandidateCheckStatus, CommitId, GitWorkspace, GitWorkspaceError,
        RegisteredWorktree,
    },
    process::{
        CancellationToken, NoopHeartbeat, ProcessHeartbeat, ProcessLimits, ProcessOutcome,
        ProcessSupervisor,
    },
    runner::bounded_runtime_text,
    runtime_trust::{
        ChildEnvironment, ExecutableIdentity, ExecutableRole, GardenerExecutableIdentities,
        GardenerExecutablePaths, GitHubCredential, ProcessPolicy, RuntimeTrustError,
    },
};

const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const DEFAULT_PROCESS_TIMEOUT: Duration = Duration::from_secs(30 * 60);

fn canonical_check_arguments() -> Vec<Vec<OsString>> {
    [
        &["test", "--all-targets", "--locked"][..],
        &[
            "clippy",
            "--all-targets",
            "--all-features",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
        &["fmt", "--all", "--", "--check"],
    ]
    .into_iter()
    .map(|arguments| arguments.iter().map(OsString::from).collect())
    .collect()
}

/// Executable and isolation configuration required to enable gardener claims.
#[derive(Clone, Debug)]
pub struct GardenerRuntimeConfig {
    worktree_root: PathBuf,
    codex_executable: PathBuf,
    git_executable: PathBuf,
    gh_executable: PathBuf,
    github_public_observer_executable: PathBuf,
    candidate_sandbox_executable: PathBuf,
    candidate_check_executable: PathBuf,
    candidate_check_arguments: Vec<Vec<OsString>>,
    child_environment: ChildEnvironment,
    github_credential: Option<GitHubCredential>,
    codex_profile: Option<String>,
    codex_model: Option<String>,
    heartbeat_interval: Duration,
    process_timeout: Duration,
    process_limits: ProcessLimits,
    cancellation: CancellationToken,
}

impl GardenerRuntimeConfig {
    pub fn new(
        worktree_root: impl Into<PathBuf>,
        codex_executable: impl Into<PathBuf>,
        git_executable: impl Into<PathBuf>,
        gh_executable: impl Into<PathBuf>,
    ) -> Self {
        Self {
            worktree_root: worktree_root.into(),
            codex_executable: codex_executable.into(),
            git_executable: git_executable.into(),
            gh_executable: gh_executable.into(),
            github_public_observer_executable: PathBuf::from("curl"),
            candidate_sandbox_executable: PathBuf::from("bwrap"),
            candidate_check_executable: PathBuf::from("cargo"),
            candidate_check_arguments: canonical_check_arguments(),
            child_environment: ChildEnvironment::captured_current()
                .expect("current process paths form a valid compatibility environment"),
            github_credential: None,
            codex_profile: None,
            codex_model: None,
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            process_timeout: DEFAULT_PROCESS_TIMEOUT,
            process_limits: ProcessLimits::default(),
            cancellation: CancellationToken::new(),
        }
    }

    pub fn with_heartbeat_interval(mut self, interval: Duration) -> Self {
        self.heartbeat_interval = interval;
        self
    }

    pub fn with_process_timeout(mut self, timeout: Duration) -> Self {
        self.process_timeout = timeout;
        self
    }

    pub fn with_process_limits(mut self, limits: ProcessLimits) -> Self {
        self.process_limits = limits;
        self
    }

    pub fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }

    pub fn with_child_environment(mut self, environment: ChildEnvironment) -> Self {
        self.child_environment = environment;
        self
    }

    pub fn with_github_credential(mut self, credential: GitHubCredential) -> Self {
        self.github_credential = Some(credential);
        self
    }

    pub fn with_candidate_checks<I, A>(mut self, executable: impl Into<PathBuf>, checks: I) -> Self
    where
        I: IntoIterator<Item = A>,
        A: IntoIterator,
        A::Item: Into<OsString>,
    {
        self.candidate_check_executable = executable.into();
        self.candidate_check_arguments = checks
            .into_iter()
            .map(|arguments| arguments.into_iter().map(Into::into).collect())
            .collect();
        self
    }

    pub fn with_candidate_sandbox(mut self, executable: impl Into<PathBuf>) -> Self {
        self.candidate_sandbox_executable = executable.into();
        self
    }

    pub fn with_github_public_observer(mut self, executable: impl Into<PathBuf>) -> Self {
        self.github_public_observer_executable = executable.into();
        self
    }

    pub fn with_codex_identity(mut self, profile: Option<String>, model: Option<String>) -> Self {
        self.codex_profile = profile;
        self.codex_model = model;
        self
    }

    pub fn worktree_root(&self) -> &Path {
        &self.worktree_root
    }

    pub(crate) fn validate(
        &self,
        lease_seconds: i64,
    ) -> Result<ValidatedGardenerRuntime, GardenerRunnerError> {
        if lease_seconds < 3 {
            return Err(GardenerRunnerError::Configuration(
                "gardener lease duration must be at least three seconds".to_owned(),
            ));
        }
        if self.process_timeout.is_zero() {
            return Err(GardenerRunnerError::Configuration(
                "gardener process timeout must be positive".to_owned(),
            ));
        }
        if self.heartbeat_interval.is_zero()
            || self
                .heartbeat_interval
                .checked_mul(3)
                .is_none_or(|interval| interval > Duration::from_secs(lease_seconds as u64))
        {
            return Err(GardenerRunnerError::Configuration(format!(
                "gardener heartbeat interval must be positive and no more than one third of the {lease_seconds}-second lease"
            )));
        }
        if !self.worktree_root.is_absolute() || !self.worktree_root.is_dir() {
            return Err(GardenerRunnerError::Configuration(format!(
                "gardener worktree root must be an existing absolute directory: {}",
                self.worktree_root.display()
            )));
        }
        let worktree_root = fs::canonicalize(&self.worktree_root).map_err(|error| {
            GardenerRunnerError::Configuration(format!(
                "cannot canonicalise gardener worktree root {}: {error}",
                self.worktree_root.display()
            ))
        })?;
        if self.candidate_check_arguments.is_empty() {
            return Err(GardenerRunnerError::Configuration(
                "at least one fixed candidate check is required".to_owned(),
            ));
        }
        let supervisor = ProcessSupervisor::new(
            self.heartbeat_interval,
            self.process_limits,
            self.cancellation.clone(),
        )
        .map_err(GardenerRunnerError::Configuration)?;
        let mut heartbeat = NoopHeartbeat;
        let executable_identities = GardenerExecutableIdentities::resolve(
            &GardenerExecutablePaths::new(
                &self.codex_executable,
                &self.git_executable,
                &self.gh_executable,
                &self.github_public_observer_executable,
            ),
            &self.child_environment,
            &supervisor,
            self.process_timeout,
            &mut heartbeat,
        )?;
        let check_identity = ExecutableIdentity::resolve(
            ExecutableRole::CandidateCheck,
            &self.candidate_check_executable,
            &["--version"],
            &self.child_environment,
            &supervisor,
            self.process_timeout,
            &mut heartbeat,
        )?;
        let sandbox_identity = ExecutableIdentity::resolve(
            ExecutableRole::CandidateSandbox,
            &self.candidate_sandbox_executable,
            &["--version"],
            &self.child_environment,
            &supervisor,
            self.process_timeout,
            &mut heartbeat,
        )?;
        let codex_process_boundary_identity = ExecutableIdentity::resolve(
            ExecutableRole::CodexProcessBoundary,
            &self.candidate_sandbox_executable,
            &["--version"],
            &self.child_environment,
            &supervisor,
            self.process_timeout,
            &mut heartbeat,
        )?;
        let candidate_checks = self
            .candidate_check_arguments
            .iter()
            .map(|arguments| {
                CandidateCheckCommand::sandboxed(
                    sandbox_identity.clone(),
                    check_identity.clone(),
                    arguments.clone(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ValidatedGardenerRuntime {
            worktree_root,
            child_environment: self.child_environment.clone(),
            executable_identities,
            codex_process_boundary_identity,
            candidate_checks,
            github_credential: self.github_credential.clone(),
            codex_profile: self.codex_profile.clone(),
            codex_model: self.codex_model.clone(),
        })
    }

    fn git_workspace(
        &self,
        runtime: &ValidatedGardenerRuntime,
        checkout: &Path,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<GitWorkspace, GitWorkspaceError> {
        let workspace = GitWorkspace::from_trust(
            checkout,
            runtime.executable_identities.git.clone(),
            runtime.executable_identities.gh.clone(),
            runtime.executable_identities.github_public_observer.clone(),
            runtime.child_environment.clone(),
            heartbeat,
        )?;
        let workspace = workspace.with_supervision(
            self.heartbeat_interval,
            self.process_timeout,
            self.process_limits,
            self.cancellation.clone(),
        )?;
        Ok(match &runtime.github_credential {
            Some(credential) => workspace.with_github_credential(credential.clone()),
            None => workspace,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedGardenerRuntime {
    worktree_root: PathBuf,
    child_environment: ChildEnvironment,
    executable_identities: GardenerExecutableIdentities,
    codex_process_boundary_identity: ExecutableIdentity,
    candidate_checks: Vec<CandidateCheckCommand>,
    github_credential: Option<GitHubCredential>,
    codex_profile: Option<String>,
    codex_model: Option<String>,
}

#[derive(Debug, Error)]
pub enum GardenerRunnerError {
    #[error("invalid gardener configuration: {0}")]
    Configuration(String),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Git(#[from] GitWorkspaceError),
    #[error(transparent)]
    RuntimeTrust(#[from] RuntimeTrustError),
    #[error("Codex app-server failed: {0}")]
    AppServer(#[source] Box<AppServerError>),
    #[error("invalid structured Codex result: {0}")]
    InvalidResult(String),
    #[error("candidate checks did not pass: {0}")]
    CandidateChecks(String),
    #[error("worktree cleanup failed after {context}: {cleanup}")]
    Cleanup {
        context: String,
        cleanup: String,
        disposition: FailureDisposition,
    },
}

impl GardenerRunnerError {
    fn failure_disposition(&self, kind: GardenerObligationKind) -> FailureDisposition {
        match self {
            Self::Cleanup { disposition, .. } => *disposition,
            Self::Git(error) if error.is_ambiguous_external_state() => {
                FailureDisposition::NeedsReconciliation
            }
            Self::Git(crate::git_workspace::GitWorkspaceError::Supervision { outcome, .. })
                if matches!(outcome.as_ref(), ProcessOutcome::Cancelled(_)) =>
            {
                FailureDisposition::Cancelled
            }
            Self::AppServer(error)
                if matches!(
                    error.as_ref(),
                    AppServerError::Supervision { outcome }
                        if matches!(outcome.as_ref(), ProcessOutcome::AmbiguousExternalState { .. })
                ) =>
            {
                FailureDisposition::NeedsReconciliation
            }
            Self::AppServer(error)
                if matches!(
                    error.as_ref(),
                    AppServerError::Supervision { outcome }
                        if matches!(outcome.as_ref(), ProcessOutcome::Cancelled(_))
                ) =>
            {
                FailureDisposition::Cancelled
            }
            _ if kind == GardenerObligationKind::Inspection => FailureDisposition::RetrySafe,
            _ => FailureDisposition::Terminal,
        }
    }
}

fn combine_dispositions(
    first: FailureDisposition,
    second: FailureDisposition,
) -> FailureDisposition {
    use FailureDisposition::{Cancelled, HumanDecision, NeedsReconciliation, RetrySafe, Terminal};
    match (first, second) {
        (NeedsReconciliation, _) | (_, NeedsReconciliation) => NeedsReconciliation,
        (HumanDecision, _) | (_, HumanDecision) => HumanDecision,
        (Cancelled, _) | (_, Cancelled) => Cancelled,
        (Terminal, _) | (_, Terminal) => Terminal,
        (RetrySafe, RetrySafe) => RetrySafe,
    }
}

/// Executes already-claimed gardener work while retaining the caller's clock
/// and lease boundary. The caller remains responsible for `Store::complete`.
pub struct GardenerRunner<'a> {
    config: &'a GardenerRuntimeConfig,
    runtime: ValidatedGardenerRuntime,
    lease_seconds: i64,
    clock: &'a dyn UnixClock,
}

impl<'a> GardenerRunner<'a> {
    pub fn new(
        config: &'a GardenerRuntimeConfig,
        lease_seconds: i64,
        clock: &'a dyn UnixClock,
    ) -> Result<Self, GardenerRunnerError> {
        let runtime = config.validate(lease_seconds)?;
        Ok(Self {
            config,
            runtime,
            lease_seconds,
            clock,
        })
    }

    pub fn execute(&self, store: &mut Store, claim: &Claim) -> RunResult {
        let kind = store.gardener_obligation_kind(&claim.obligation_id);
        let failure_kind = kind
            .as_ref()
            .ok()
            .and_then(|kind| *kind)
            .unwrap_or(GardenerObligationKind::Implementation);
        let result = match kind {
            Ok(Some(GardenerObligationKind::Inspection)) => self.run_inspection(store, claim),
            Ok(Some(GardenerObligationKind::Implementation)) => {
                self.run_implementation(store, claim)
            }
            Ok(None) => Err(GardenerRunnerError::Configuration(format!(
                "obligation {:?} is not bound to the coding gardener",
                claim.obligation_id
            ))),
            Err(error) => Err(error.into()),
        };

        RunResult {
            completion: match result {
                Ok(Success::Inspection(evidence)) | Ok(Success::Implementation(evidence)) => {
                    Completion::Succeeded {
                        evidence: Some(bounded_runtime_text(
                            evidence,
                            MAX_COMPLETION_EVIDENCE_CHARS,
                        )),
                    }
                }
                Ok(Success::NeedsAttention { error, evidence }) => Completion::Failed {
                    disposition: FailureDisposition::HumanDecision,
                    error: bounded_runtime_text(error, MAX_COMPLETION_ERROR_CHARS),
                    evidence: Some(bounded_runtime_text(
                        evidence,
                        MAX_COMPLETION_EVIDENCE_CHARS,
                    )),
                },
                Err(error) => {
                    let disposition = error.failure_disposition(failure_kind);
                    Completion::Failed {
                        disposition,
                        error: bounded_runtime_text(error.to_string(), MAX_COMPLETION_ERROR_CHARS),
                        evidence: Some(bounded_runtime_text(
                            format!(
                                "coding gardener failed for obligation {:?}, occurrence {}, attempt {}, lease generation {}: {error}",
                                claim.obligation_id,
                                claim.occurrence,
                                claim.attempt_number,
                                claim.lease_generation
                            ),
                            MAX_COMPLETION_EVIDENCE_CHARS,
                        )),
                    }
                }
            },
        }
    }

    fn run_inspection(
        &self,
        store: &mut Store,
        claim: &Claim,
    ) -> Result<Success, GardenerRunnerError> {
        let root = &self.runtime.worktree_root;
        let repository = store.gardener_repository()?.ok_or_else(|| {
            GardenerRunnerError::Configuration(
                "gardener runtime is enabled without a repository registration".to_owned(),
            )
        })?;
        let git = self.observe_process(store, claim, |observer| {
            self.config.git_workspace(
                &self.runtime,
                Path::new(&repository.checkout_path),
                observer,
            )
        })?;

        self.heartbeat(store, claim)?;
        let source =
            self.observe_process(store, claim, |observer| git.resolve_origin_main(observer))?;
        self.heartbeat(store, claim)?;

        let unique = Uuid::new_v4().simple().to_string();
        let inspection_id = format!("inspection-{unique}");
        let worktree_path = root.join(&inspection_id);
        let prompt = inspection_prompt(&source);
        let prompt_digest = digest(&prompt);
        store.start_gardener_inspection(
            claim,
            NewGardenerInspection {
                id: inspection_id.clone(),
                source_commit: source.to_string(),
                worktree_path: path_string(&worktree_path)?,
                prompt_digest,
            },
            self.clock.now(),
        )?;

        self.heartbeat(store, claim)?;
        let worktree = self.observe_process(store, claim, |observer| {
            git.create_detached_worktree(&worktree_path, &source, observer)
        })?;
        self.heartbeat(store, claim)?;
        let operation = (|| {
            let result = {
                let mut observer = StoreObserver::inspection(
                    store,
                    claim,
                    &inspection_id,
                    self.clock,
                    self.lease_seconds,
                );
                AppServerClient::from_trust(
                    self.runtime.executable_identities.codex.clone(),
                    self.runtime.codex_process_boundary_identity.clone(),
                    self.runtime.child_environment.clone(),
                )
                .map_err(|error| GardenerRunnerError::AppServer(Box::new(error)))?
                .with_heartbeat_interval(self.config.heartbeat_interval)
                .with_execution_timeout(self.config.process_timeout)
                .with_process_limits(self.config.process_limits)
                .with_cancellation(self.config.cancellation.clone())
                .run(
                    &TurnRequest {
                        kind: TurnKind::Inspection,
                        cwd: worktree.path(),
                        prompt: &prompt,
                        output_schema: &inspection_schema(),
                    },
                    &mut observer,
                )
                .map_err(|error| GardenerRunnerError::AppServer(Box::new(error)))?
            };
            let parsed: InspectionResult = serde_json::from_str(&result.final_message)
                .map_err(|error| GardenerRunnerError::InvalidResult(error.to_string()))?;
            if parsed.proposed_goal_prompts.len() > 3 {
                return Err(GardenerRunnerError::InvalidResult(
                    "inspection returned more than three goal prompts".to_owned(),
                ));
            }
            self.heartbeat(store, claim)?;
            self.observe_process(store, claim, |observer| {
                git.verify_head(&worktree, &source, observer)
            })?;
            store.finish_gardener_inspection(claim, &inspection_id, &parsed, self.clock.now())?;
            Ok::<_, GardenerRunnerError>(parsed.proposed_goal_prompts.len())
        })();

        let cleanup = self.cleanup(store, claim, &git, &worktree);
        match (operation, cleanup) {
            (Ok(count), Ok(())) => Ok(Success::Inspection(format!(
                "inspected {CANONICAL_REPOSITORY} at {source}; recorded {count} proposal(s) from {inspection_id}"
            ))),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(cleanup)) => Err(GardenerRunnerError::Cleanup {
                context: format!("completed inspection {inspection_id} at {source}"),
                cleanup: cleanup.to_string(),
                disposition: cleanup.failure_disposition(GardenerObligationKind::Inspection),
            }),
            (Err(error), Err(cleanup)) => {
                let cleanup_disposition =
                    cleanup.failure_disposition(GardenerObligationKind::Inspection);
                let disposition = combine_dispositions(
                    error.failure_disposition(GardenerObligationKind::Inspection),
                    cleanup_disposition,
                );
                Err(GardenerRunnerError::Cleanup {
                    context: error.to_string(),
                    cleanup: cleanup.to_string(),
                    disposition,
                })
            }
        }
    }

    fn run_implementation(
        &self,
        store: &mut Store,
        claim: &Claim,
    ) -> Result<Success, GardenerRunnerError> {
        let root = &self.runtime.worktree_root;
        let repository = store.gardener_repository()?.ok_or_else(|| {
            GardenerRunnerError::Configuration(
                "gardener runtime is enabled without a repository registration".to_owned(),
            )
        })?;
        let git = self.observe_process(store, claim, |observer| {
            self.config.git_workspace(
                &self.runtime,
                Path::new(&repository.checkout_path),
                observer,
            )
        })?;
        let unique = Uuid::new_v4().simple().to_string();
        let run_id = format!("run-{unique}");
        let branch = format!("codex/gardener-{unique}");
        let implementation_path = root.join(format!("implementation-{unique}"));
        let run = store.create_gardener_implementation_run(
            claim,
            NewGardenerImplementationRun {
                id: run_id.clone(),
                implementation_worktree_path: path_string(&implementation_path)?,
                branch: branch.clone(),
            },
            self.clock.now(),
        )?;
        let proposal = store
            .gardener_proposal_instance(&run.proposal_instance_id)?
            .ok_or_else(|| StoreError::NotFound(run.proposal_instance_id.clone()))?;
        let source = CommitId::parse(run.source_commit.clone())?;
        let prompt = implementation_prompt(&source, &proposal.prompt);
        let manifest =
            reproducibility_manifest(&self.runtime, &run_id, &source, &prompt, self.clock.now())?;
        store.record_gardener_reproducibility_manifest(claim, &manifest, self.clock.now())?;

        self.heartbeat(store, claim)?;
        let implementation = self.observe_process(store, claim, |observer| {
            git.create_branch_worktree(&implementation_path, &branch, &source, observer)
        })?;
        self.heartbeat(store, claim)?;
        let mut verification: Option<RegisteredWorktree> = None;
        let operation = (|| {
            let result = {
                let mut observer = StoreObserver::implementation(
                    store,
                    claim,
                    &run_id,
                    self.clock,
                    self.lease_seconds,
                );
                AppServerClient::from_trust(
                    self.runtime.executable_identities.codex.clone(),
                    self.runtime.codex_process_boundary_identity.clone(),
                    self.runtime.child_environment.clone(),
                )
                .map_err(|error| GardenerRunnerError::AppServer(Box::new(error)))?
                .with_heartbeat_interval(self.config.heartbeat_interval)
                .with_execution_timeout(self.config.process_timeout)
                .with_process_limits(self.config.process_limits)
                .with_cancellation(self.config.cancellation.clone())
                .run(
                    &TurnRequest {
                        kind: TurnKind::Implementation,
                        cwd: implementation.path(),
                        prompt: &prompt,
                        output_schema: &implementation_schema(),
                    },
                    &mut observer,
                )
                .map_err(|error| GardenerRunnerError::AppServer(Box::new(error)))?
            };
            let final_value: GardenerImplementationResult =
                serde_json::from_str(&result.final_message)
                    .map_err(|error| GardenerRunnerError::InvalidResult(error.to_string()))?;
            validate_implementation_result(&final_value)?;
            store.finish_gardener_implementation(
                claim,
                &run_id,
                &result.final_message,
                self.clock.now(),
            )?;

            self.heartbeat(store, claim)?;
            self.observe_process(store, claim, |observer| {
                git.verify_head(&implementation, &source, observer)
            })?;
            let commit = self.observe_process(store, claim, |observer| {
                git.commit_all(&implementation, &commit_message(&proposal.prompt), observer)
            })?;
            self.heartbeat(store, claim)?;
            store.record_gardener_git_commit(claim, &run_id, commit.as_str(), self.clock.now())?;

            self.heartbeat(store, claim)?;
            let qualification = self.observe_process(store, claim, |observer| {
                git.qualify_candidate(
                    &implementation,
                    &source,
                    &commit,
                    &self.runtime.candidate_checks,
                    observer,
                )
            })?;
            let candidate_evidence = GardenerCandidateQualification {
                run_id: run_id.clone(),
                head: commit.to_string(),
                diff_manifest_json: serde_json::to_string(&qualification.manifest.diff)
                    .map_err(|error| GardenerRunnerError::InvalidResult(error.to_string()))?,
                tree_manifest_json: serde_json::to_string(&qualification.manifest.tree)
                    .map_err(|error| GardenerRunnerError::InvalidResult(error.to_string()))?,
                checks_json: serde_json::to_string(&qualification.checks)
                    .map_err(|error| GardenerRunnerError::InvalidResult(error.to_string()))?,
                duration_ms: qualification
                    .checks
                    .iter()
                    .map(|check| check.duration_millis)
                    .sum(),
                qualified_at: self.clock.now(),
            };
            store.record_gardener_candidate_qualification(
                claim,
                &candidate_evidence,
                self.clock.now(),
            )?;
            let failed_checks = qualification
                .checks
                .iter()
                .filter(|check| check.status != CandidateCheckStatus::Passed)
                .map(|check| {
                    format!(
                        "{} {:?}",
                        check.executable.path().display(),
                        check.arguments
                    )
                })
                .collect::<Vec<_>>();
            if !failed_checks.is_empty() {
                return Err(GardenerRunnerError::CandidateChecks(
                    failed_checks.join(", "),
                ));
            }

            self.heartbeat(store, claim)?;
            self.observe_process(store, claim, |observer| {
                git.push_branch(&implementation, &commit, observer)
            })?;
            self.heartbeat(store, claim)?;
            let pushed = self.observe_process(store, claim, |observer| {
                git.observe_remote_branch(&branch, &commit, observer)
            })?;
            self.heartbeat(store, claim)?;
            store.record_gardener_push_observation(
                claim,
                &run_id,
                pushed.as_str(),
                self.clock.now(),
            )?;

            self.heartbeat(store, claim)?;
            let pull_request = self.observe_process(store, claim, |observer| {
                git.create_draft_pull_request(
                    &branch,
                    &commit,
                    &commit_message(&proposal.prompt),
                    &pull_request_body(&source, &proposal.prompt),
                    observer,
                )
            })?;
            self.heartbeat(store, claim)?;
            store.record_gardener_draft_pull_request(
                claim,
                &run_id,
                pull_request.number,
                &pull_request.url,
                pull_request.head.as_str(),
                self.clock.now(),
            )?;

            let verification_path = root.join(format!("verification-{unique}"));
            store.start_gardener_verification(
                claim,
                &run_id,
                &path_string(&verification_path)?,
                pull_request.head.as_str(),
                self.clock.now(),
            )?;
            self.heartbeat(store, claim)?;
            verification = Some(self.observe_process(store, claim, |observer| {
                git.create_detached_worktree(&verification_path, &pull_request.head, observer)
            })?);
            self.heartbeat(store, claim)?;
            let verification_worktree = verification.as_ref().expect("worktree was created");
            self.observe_process(store, claim, |observer| {
                git.verify_head(verification_worktree, &pull_request.head, observer)
            })?;
            let verification_prompt = verification_prompt(&pull_request.head, &proposal.prompt);
            let result = {
                let mut observer = StoreObserver::verification(
                    store,
                    claim,
                    &run_id,
                    self.clock,
                    self.lease_seconds,
                );
                AppServerClient::from_trust(
                    self.runtime.executable_identities.codex.clone(),
                    self.runtime.codex_process_boundary_identity.clone(),
                    self.runtime.child_environment.clone(),
                )
                .map_err(|error| GardenerRunnerError::AppServer(Box::new(error)))?
                .with_heartbeat_interval(self.config.heartbeat_interval)
                .with_execution_timeout(self.config.process_timeout)
                .with_process_limits(self.config.process_limits)
                .with_cancellation(self.config.cancellation.clone())
                .run(
                    &TurnRequest {
                        kind: TurnKind::Verification,
                        cwd: verification_worktree.path(),
                        prompt: &verification_prompt,
                        output_schema: &verification_schema(),
                    },
                    &mut observer,
                )
                .map_err(|error| GardenerRunnerError::AppServer(Box::new(error)))?
            };
            let verdict: GardenerVerificationResult =
                serde_json::from_str(&result.final_message)
                    .map_err(|error| GardenerRunnerError::InvalidResult(error.to_string()))?;
            validate_verification_result(&verdict)?;
            let reported_head = CommitId::parse(verdict.head.clone())?;
            if reported_head != pull_request.head {
                return Err(GardenerRunnerError::InvalidResult(format!(
                    "verification reported head {reported_head}, expected exact pull-request head {}",
                    pull_request.head
                )));
            }
            self.heartbeat(store, claim)?;
            self.observe_process(store, claim, |observer| {
                git.verify_head(verification_worktree, &pull_request.head, observer)
            })?;
            self.heartbeat(store, claim)?;
            self.observe_process(store, claim, |observer| {
                git.observe_draft_pull_request(&branch, &pull_request.head, observer)
            })?;
            store.finish_gardener_verification(
                claim,
                &run_id,
                verdict.verdict,
                reported_head.as_str(),
                &verdict.summary,
                self.clock.now(),
            )?;
            let pull_request = if verdict.verdict == GardenerVerificationVerdict::Pass {
                store.request_gardener_pull_request_ready(
                    claim,
                    &run_id,
                    pull_request.head.as_str(),
                    self.clock.now(),
                )?;
                self.heartbeat(store, claim)?;
                let ready = self.observe_process(store, claim, |observer| {
                    git.mark_pull_request_ready(&branch, &pull_request.head, observer)
                })?;
                self.heartbeat(store, claim)?;
                store.record_gardener_pull_request_ready(
                    claim,
                    &run_id,
                    ready.number,
                    &ready.url,
                    ready.head.as_str(),
                    self.clock.now(),
                )?;
                ready
            } else {
                pull_request
            };
            Ok::<_, GardenerRunnerError>((commit, pull_request, verdict))
        })();

        let verification_cleanup = verification
            .as_ref()
            .map(|worktree| self.cleanup(store, claim, &git, worktree))
            .transpose();
        let implementation_cleanup = self.cleanup(store, claim, &git, &implementation);
        let cleanup = verification_cleanup.and(implementation_cleanup);
        match (operation, cleanup) {
            (Ok((commit, pull_request, verdict)), Ok(())) => {
                if verdict.verdict == GardenerVerificationVerdict::Pass {
                    Ok(Success::Implementation(format!(
                        "promoted pull request {} to ready at exact head {commit} after passing candidate checks and independent verification",
                        pull_request.url
                    )))
                } else {
                    Ok(Success::NeedsAttention {
                        error: format!(
                            "independent verification returned {} for exact head {commit}",
                            verdict.verdict
                        ),
                        evidence: format!(
                            "draft pull request {} is preserved at {commit}; verification summary: {}",
                            pull_request.url, verdict.summary
                        ),
                    })
                }
            }
            (Err(error), Ok(())) => Err(error),
            (Ok((commit, pull_request, _)), Err(cleanup)) => Err(GardenerRunnerError::Cleanup {
                context: format!(
                    "external work completed for pull request {} at {commit}",
                    pull_request.url
                ),
                cleanup: cleanup.to_string(),
                disposition: FailureDisposition::NeedsReconciliation,
            }),
            (Err(error), Err(cleanup)) => {
                let cleanup_disposition =
                    cleanup.failure_disposition(GardenerObligationKind::Implementation);
                let disposition = combine_dispositions(
                    error.failure_disposition(GardenerObligationKind::Implementation),
                    cleanup_disposition,
                );
                Err(GardenerRunnerError::Cleanup {
                    context: error.to_string(),
                    cleanup: cleanup.to_string(),
                    disposition,
                })
            }
        }
    }

    fn heartbeat(&self, store: &mut Store, claim: &Claim) -> Result<(), GardenerRunnerError> {
        store.renew_lease(claim, self.clock.now(), self.lease_seconds)?;
        Ok(())
    }

    fn observe_process<T>(
        &self,
        store: &mut Store,
        claim: &Claim,
        operation: impl FnOnce(&mut dyn ProcessHeartbeat) -> Result<T, GitWorkspaceError>,
    ) -> Result<T, GardenerRunnerError> {
        let mut observer = StoreObserver::process(store, claim, self.clock, self.lease_seconds);
        operation(&mut observer).map_err(Into::into)
    }

    fn cleanup(
        &self,
        store: &mut Store,
        claim: &Claim,
        git: &GitWorkspace,
        worktree: &RegisteredWorktree,
    ) -> Result<(), GardenerRunnerError> {
        self.heartbeat(store, claim)?;
        self.observe_process(store, claim, |observer| {
            git.remove_clean_worktree(worktree, observer)
        })?;
        self.heartbeat(store, claim)
    }
}

enum Success {
    Inspection(String),
    Implementation(String),
    NeedsAttention { error: String, evidence: String },
}

enum ObserverTarget<'a> {
    Process,
    Inspection(&'a str),
    Implementation(&'a str),
    Verification(&'a str),
}

struct StoreObserver<'a> {
    store: &'a mut Store,
    claim: &'a Claim,
    target: ObserverTarget<'a>,
    clock: &'a dyn UnixClock,
    lease_seconds: i64,
}

impl<'a> StoreObserver<'a> {
    fn process(
        store: &'a mut Store,
        claim: &'a Claim,
        clock: &'a dyn UnixClock,
        lease_seconds: i64,
    ) -> Self {
        Self {
            store,
            claim,
            target: ObserverTarget::Process,
            clock,
            lease_seconds,
        }
    }

    fn inspection(
        store: &'a mut Store,
        claim: &'a Claim,
        id: &'a str,
        clock: &'a dyn UnixClock,
        lease_seconds: i64,
    ) -> Self {
        Self {
            store,
            claim,
            target: ObserverTarget::Inspection(id),
            clock,
            lease_seconds,
        }
    }

    fn implementation(
        store: &'a mut Store,
        claim: &'a Claim,
        id: &'a str,
        clock: &'a dyn UnixClock,
        lease_seconds: i64,
    ) -> Self {
        Self {
            store,
            claim,
            target: ObserverTarget::Implementation(id),
            clock,
            lease_seconds,
        }
    }

    fn verification(
        store: &'a mut Store,
        claim: &'a Claim,
        id: &'a str,
        clock: &'a dyn UnixClock,
        lease_seconds: i64,
    ) -> Self {
        Self {
            store,
            claim,
            target: ObserverTarget::Verification(id),
            clock,
            lease_seconds,
        }
    }

    fn result(&mut self, result: Result<(), StoreError>) -> Result<(), String> {
        result.map_err(|error| error.to_string())
    }
}

impl AppServerObserver for StoreObserver<'_> {
    fn record_thread(&mut self, thread_id: &str) -> Result<(), String> {
        let now = self.clock.now();
        let result = match self.target {
            ObserverTarget::Process => Ok(()),
            ObserverTarget::Inspection(id) => self
                .store
                .record_inspection_codex_thread(self.claim, id, thread_id, now),
            ObserverTarget::Implementation(id) => self
                .store
                .record_implementation_codex_thread(self.claim, id, thread_id, now),
            ObserverTarget::Verification(id) => self
                .store
                .record_verification_codex_thread(self.claim, id, thread_id, now),
        };
        self.result(result)
    }

    fn record_turn(&mut self, turn_id: &str) -> Result<(), String> {
        let now = self.clock.now();
        let result = match self.target {
            ObserverTarget::Process => Ok(()),
            ObserverTarget::Inspection(id) => self
                .store
                .record_inspection_codex_turn(self.claim, id, turn_id, now),
            ObserverTarget::Implementation(id) => self
                .store
                .record_implementation_codex_turn(self.claim, id, turn_id, now),
            ObserverTarget::Verification(id) => self
                .store
                .record_verification_codex_turn(self.claim, id, turn_id, now),
        };
        self.result(result)
    }

    fn heartbeat(&mut self) -> Result<(), String> {
        ProcessHeartbeat::heartbeat(self)
    }
}

impl ProcessHeartbeat for StoreObserver<'_> {
    fn heartbeat(&mut self) -> Result<(), String> {
        let result = self
            .store
            .renew_lease(self.claim, self.clock.now(), self.lease_seconds)
            .map(|_| ());
        self.result(result)
    }
}

fn inspection_prompt(source: &CommitId) -> String {
    format!(
        "You are performing a bounded, read-only coding-gardener inspection of only {CANONICAL_REPOSITORY} at exact commit {source}. Read AGENTS.md, README.md, and relevant files under docs/plans/ before inspecting the repository. Do not modify files, run network commands, start implementation, commit, push, open a pull request, or request permissions. Return one JSON object matching the supplied schema. Propose at most three independently useful, concrete goal prompts for maintainability, correctness, tests, or documentation. Each goal prompt must be self-contained, constrained to {CANONICAL_REPOSITORY}, and suitable for separate human approval. If no worthwhile work is supported by repository evidence, return an empty proposed_goal_prompts array."
    )
}

fn reproducibility_manifest(
    runtime: &ValidatedGardenerRuntime,
    run_id: &str,
    source: &CommitId,
    prompt: &str,
    recorded_at: i64,
) -> Result<GardenerReproducibilityManifest, GardenerRunnerError> {
    let mut executables = vec![
        &runtime.executable_identities.codex,
        &runtime.codex_process_boundary_identity,
        &runtime.executable_identities.git,
        &runtime.executable_identities.gh,
        &runtime.executable_identities.github_public_observer,
    ];
    for check in &runtime.candidate_checks {
        if let Some(sandbox) = check.sandbox() {
            if !executables
                .iter()
                .any(|identity| identity.path() == sandbox.path())
            {
                executables.push(sandbox);
            }
        }
        if !executables
            .iter()
            .any(|identity| identity.path() == check.executable().path())
        {
            executables.push(check.executable());
        }
    }
    let check_commands = runtime
        .candidate_checks
        .iter()
        .map(|check| {
            let arguments = check
                .arguments()
                .iter()
                .map(|argument| {
                    argument.to_str().map(str::to_owned).ok_or_else(|| {
                        GardenerRunnerError::Configuration(
                            "candidate check arguments must be valid UTF-8".to_owned(),
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!({
                "sandbox": check.sandbox(),
                "executable": check.executable(),
                "arguments": arguments,
            }))
        })
        .collect::<Result<Vec<_>, GardenerRunnerError>>()?;
    let environment_policy = json!({
        "environment": runtime.child_environment,
        "roles": [
            ProcessPolicy::Codex,
            ProcessPolicy::GitLocal,
            ProcessPolicy::GitRemoteRead,
            ProcessPolicy::GitHubMutationGit,
            ProcessPolicy::GitHubRead,
            ProcessPolicy::GitHubPublicRead,
            ProcessPolicy::GitHubMutationCli,
            ProcessPolicy::CodexProcessBoundary,
            ProcessPolicy::CandidateCheck,
            ProcessPolicy::CandidateSandbox,
        ]
    });
    let sandbox_policy = json!({
        "codex_process_boundary": {
            "pid_namespace": "private",
            "procfs": "private",
            "remaining_descendants": "killed_when_namespace_init_exits",
            "parent_loss": "die_with_parent"
        },
        "inspection": {"access": "read_only", "network": false},
        "implementation": {"access": "workspace_write", "network": false},
        "verification": {"access": "read_only", "network": false},
        "candidate_checks": {
            "access": "isolated_copy_write",
            "network": false,
            "process_namespace": "private",
            "home": "ephemeral",
            "authoritative_state": "not_mounted"
        },
        "approval_policy": "never",
    });
    Ok(GardenerReproducibilityManifest {
        run_id: run_id.to_owned(),
        bokkie_build: bokkie_build_identity()?,
        source_commit: source.to_string(),
        prompt_digest: digest(prompt),
        implementation_schema_digest: digest(&implementation_schema().to_string()),
        verification_schema_digest: digest(&verification_schema().to_string()),
        codex_profile: runtime.codex_profile.clone(),
        codex_model: runtime.codex_model.clone(),
        executable_manifest_json: serde_json::to_string(&executables)
            .map_err(|error| GardenerRunnerError::InvalidResult(error.to_string()))?,
        sandbox_policy_digest: digest(&sandbox_policy.to_string()),
        environment_policy_digest: digest(&environment_policy.to_string()),
        check_commands_json: serde_json::to_string(&check_commands)
            .map_err(|error| GardenerRunnerError::InvalidResult(error.to_string()))?,
        recorded_at,
    })
}

fn bokkie_build_identity() -> Result<String, GardenerRunnerError> {
    let path = std::env::current_exe().map_err(|error| {
        GardenerRunnerError::Configuration(format!(
            "cannot resolve the running Bokkie executable: {error}"
        ))
    })?;
    let path = fs::canonicalize(&path).map_err(|error| {
        GardenerRunnerError::Configuration(format!(
            "cannot canonicalise the running Bokkie executable {}: {error}",
            path.display()
        ))
    })?;
    let bytes = fs::read(&path).map_err(|error| {
        GardenerRunnerError::Configuration(format!(
            "cannot identify the running Bokkie executable {}: {error}",
            path.display()
        ))
    })?;
    serde_json::to_string(&json!({
        "package": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
        "path": path,
        "sha256": format!("{:x}", Sha256::digest(bytes)),
    }))
    .map_err(|error| GardenerRunnerError::InvalidResult(error.to_string()))
}

fn validate_implementation_result(
    result: &GardenerImplementationResult,
) -> Result<(), GardenerRunnerError> {
    if result.summary.trim().is_empty()
        || result.summary.chars().count() > MAX_GARDENER_MODEL_TEXT_CHARS
        || result.changed_paths.len() > MAX_GARDENER_MODEL_ITEMS
        || result.checks.len() > MAX_GARDENER_MODEL_ITEMS
        || result
            .changed_paths
            .iter()
            .chain(&result.checks)
            .any(|value| {
                value.trim().is_empty() || value.chars().count() > MAX_GARDENER_MODEL_ITEM_CHARS
            })
    {
        return Err(GardenerRunnerError::InvalidResult(
            "implementation result exceeds the typed field bounds".to_owned(),
        ));
    }
    Ok(())
}

fn validate_verification_result(
    result: &GardenerVerificationResult,
) -> Result<(), GardenerRunnerError> {
    let invalid_fields = result.summary.trim().is_empty()
        || result.summary.chars().count() > MAX_GARDENER_MODEL_TEXT_CHARS
        || result.blocking_findings.len() > MAX_GARDENER_MODEL_ITEMS
        || result.validation.len() > MAX_GARDENER_MODEL_ITEMS
        || result
            .blocking_findings
            .iter()
            .chain(&result.validation)
            .any(|value| {
                value.trim().is_empty() || value.chars().count() > MAX_GARDENER_MODEL_ITEM_CHARS
            });
    let contradictory_pass =
        result.verdict == GardenerVerificationVerdict::Pass && !result.blocking_findings.is_empty();
    if invalid_fields || contradictory_pass {
        return Err(GardenerRunnerError::InvalidResult(
            "verification result exceeds field bounds or contradicts its verdict".to_owned(),
        ));
    }
    Ok(())
}

fn implementation_prompt(source: &CommitId, approved_prompt: &str) -> String {
    format!(
        "You are implementing one immutable, human-approved coding-gardener goal in only {CANONICAL_REPOSITORY}, from exact base commit {source}. Work only in the supplied isolated worktree. Read and obey AGENTS.md, README.md, and relevant docs/plans material. Network access, permission escalation, commits, pushes, pull-request creation, merges, releases, and deployment are forbidden. Do not change HEAD or create a Git commit; Bokkie owns all Git and GitHub effects after your turn. Implement and locally verify only the approved goal below. Return a JSON object summarising the changes and checks.\n\n<approved-goal>\n{approved_prompt}\n</approved-goal>"
    )
}

fn verification_prompt(head: &CommitId, approved_prompt: &str) -> String {
    format!(
        "You are the independent, read-only verifier for only {CANONICAL_REPOSITORY} at exact pull-request head {head}. Use only the supplied detached worktree. Read AGENTS.md, README.md, and relevant docs/plans material. Do not modify files, use the network, request permissions, commit, push, create or update a pull request, merge, release, or deploy. Verify whether the exact checked-out head satisfies the immutable approved goal below. Return only one JSON object matching the supplied schema. Set verdict to pass only when the exact head is adequately verified; use blocking for a demonstrated defect and inconclusive when evidence is insufficient. The head field must be exactly {head}.\n\n<approved-goal>\n{approved_prompt}\n</approved-goal>"
    )
}

fn inspection_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["summary", "proposed_goal_prompts"],
        "properties": {
            "summary": {"type": "string", "minLength": 1, "maxLength": MAX_GARDENER_MODEL_TEXT_CHARS},
            "proposed_goal_prompts": {
                "type": "array",
                "maxItems": MAX_GARDENER_PROMPTS,
                "items": {"type": "string", "minLength": 1, "maxLength": MAX_GARDENER_PROMPT_CHARS}
            }
        }
    })
}

fn implementation_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["summary", "changed_paths", "checks"],
        "properties": {
            "summary": {"type": "string", "minLength": 1, "maxLength": MAX_GARDENER_MODEL_TEXT_CHARS},
            "changed_paths": {
                "type": "array", "maxItems": MAX_GARDENER_MODEL_ITEMS,
                "items": {"type": "string", "minLength": 1, "maxLength": MAX_GARDENER_MODEL_ITEM_CHARS}
            },
            "checks": {
                "type": "array", "maxItems": MAX_GARDENER_MODEL_ITEMS,
                "items": {"type": "string", "minLength": 1, "maxLength": MAX_GARDENER_MODEL_ITEM_CHARS}
            }
        }
    })
}

fn verification_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["verdict", "head", "summary", "blocking_findings", "validation"],
        "properties": {
            "verdict": {"type": "string", "enum": ["pass", "blocking", "inconclusive"]},
            "head": {"type": "string", "pattern": "^[0-9a-f]{40}$"},
            "summary": {"type": "string", "minLength": 1, "maxLength": MAX_GARDENER_MODEL_TEXT_CHARS},
            "blocking_findings": {
                "type": "array", "maxItems": MAX_GARDENER_MODEL_ITEMS,
                "items": {"type": "string", "minLength": 1, "maxLength": MAX_GARDENER_MODEL_ITEM_CHARS}
            },
            "validation": {
                "type": "array", "maxItems": MAX_GARDENER_MODEL_ITEMS,
                "items": {"type": "string", "minLength": 1, "maxLength": MAX_GARDENER_MODEL_ITEM_CHARS}
            }
        }
    })
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn path_string(path: &Path) -> Result<String, GardenerRunnerError> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        GardenerRunnerError::Configuration(format!(
            "gardener worktree path is not valid UTF-8: {}",
            path.display()
        ))
    })
}

fn commit_message(prompt: &str) -> String {
    let summary = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("Apply approved gardener change");
    let mut message = format!("Gardener: {summary}");
    while message.len() > 72 {
        message.pop();
    }
    message.trim_end().to_owned()
}

fn pull_request_body(source: &CommitId, prompt: &str) -> String {
    format!(
        "Automated coding-gardener implementation from exact base `{source}`. This pull request is a draft pending independent exact-head qualification and is not merged automatically.\n\nApproved goal:\n\n{prompt}"
    )
}

#[cfg(test)]
mod tests;
