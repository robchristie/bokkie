//! Bounded coding-gardener execution coordinator.
//!
//! The coordinator persists each external identity before moving to the next
//! effect. It deliberately leaves obligation transitions to [`Store`] and
//! never places Git, GitHub, or Codex work inside a database transaction.

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    Claim, Completion, GardenerObligationKind, GardenerVerificationResult,
    GardenerVerificationVerdict, InspectionResult, NewGardenerImplementationRun,
    NewGardenerInspection, RunResult, Store, StoreError, UnixClock,
    app_server::{AppServerClient, AppServerObserver, TurnKind, TurnRequest},
    gardener::CANONICAL_REPOSITORY,
    git_workspace::{CommitId, GitWorkspace, GitWorkspaceError, RegisteredWorktree},
};

const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

/// Executable and isolation configuration required to enable gardener claims.
#[derive(Clone, Debug)]
pub struct GardenerRuntimeConfig {
    worktree_root: PathBuf,
    codex_executable: PathBuf,
    git_executable: PathBuf,
    gh_executable: PathBuf,
    heartbeat_interval: Duration,
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
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
        }
    }

    pub fn with_heartbeat_interval(mut self, interval: Duration) -> Self {
        self.heartbeat_interval = interval;
        self
    }

    pub fn worktree_root(&self) -> &Path {
        &self.worktree_root
    }

    pub(crate) fn validate(&self, lease_seconds: i64) -> Result<PathBuf, GardenerRunnerError> {
        if lease_seconds < 3 {
            return Err(GardenerRunnerError::Configuration(
                "gardener lease duration must be at least three seconds".to_owned(),
            ));
        }
        if self.heartbeat_interval.is_zero()
            || self.heartbeat_interval > Duration::from_secs(lease_seconds as u64 / 3)
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
        fs::canonicalize(&self.worktree_root).map_err(|error| {
            GardenerRunnerError::Configuration(format!(
                "cannot canonicalise gardener worktree root {}: {error}",
                self.worktree_root.display()
            ))
        })
    }
}

#[derive(Debug, Error)]
pub enum GardenerRunnerError {
    #[error("invalid gardener configuration: {0}")]
    Configuration(String),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Git(#[from] GitWorkspaceError),
    #[error("Codex app-server failed: {0}")]
    AppServer(String),
    #[error("invalid structured Codex result: {0}")]
    InvalidResult(String),
    #[error("worktree cleanup failed after {context}: {cleanup}")]
    Cleanup { context: String, cleanup: String },
}

/// Executes already-claimed gardener work while retaining the caller's clock
/// and lease boundary. The caller remains responsible for `Store::complete`.
pub struct GardenerRunner<'a> {
    config: &'a GardenerRuntimeConfig,
    lease_seconds: i64,
    clock: &'a dyn UnixClock,
}

impl<'a> GardenerRunner<'a> {
    pub fn new(
        config: &'a GardenerRuntimeConfig,
        lease_seconds: i64,
        clock: &'a dyn UnixClock,
    ) -> Result<Self, GardenerRunnerError> {
        config.validate(lease_seconds)?;
        Ok(Self {
            config,
            lease_seconds,
            clock,
        })
    }

    pub fn execute(&self, store: &mut Store, claim: &Claim) -> RunResult {
        let kind = store.gardener_obligation_kind(&claim.obligation_id);
        let retryable = matches!(&kind, Ok(Some(GardenerObligationKind::Inspection)));
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
                        evidence: Some(evidence),
                    }
                }
                Ok(Success::NeedsAttention { error, evidence }) => Completion::Failed {
                    retryable: false,
                    error,
                    evidence: Some(evidence),
                },
                Err(error) => Completion::Failed {
                    retryable,
                    error: error.to_string(),
                    evidence: Some(format!(
                        "coding gardener failed for obligation {:?}, occurrence {}, attempt {}, lease generation {}: {error}",
                        claim.obligation_id,
                        claim.occurrence,
                        claim.attempt_number,
                        claim.lease_generation
                    )),
                },
            },
        }
    }

    fn run_inspection(
        &self,
        store: &mut Store,
        claim: &Claim,
    ) -> Result<Success, GardenerRunnerError> {
        let root = self.config.validate(self.lease_seconds)?;
        let repository = store.gardener_repository()?.ok_or_else(|| {
            GardenerRunnerError::Configuration(
                "gardener runtime is enabled without a repository registration".to_owned(),
            )
        })?;
        let git = GitWorkspace::new(
            &repository.checkout_path,
            &self.config.git_executable,
            &self.config.gh_executable,
        )?;

        self.heartbeat(store, claim)?;
        let source = git.resolve_origin_main()?;
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
        let worktree = git.create_detached_worktree(&worktree_path, &source)?;
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
                AppServerClient::new(&self.config.codex_executable)
                    .with_heartbeat_interval(self.config.heartbeat_interval)
                    .run(
                        &TurnRequest {
                            kind: TurnKind::Inspection,
                            cwd: worktree.path(),
                            prompt: &prompt,
                            output_schema: &inspection_schema(),
                        },
                        &mut observer,
                    )
                    .map_err(|error| GardenerRunnerError::AppServer(error.to_string()))?
            };
            let parsed: InspectionResult = serde_json::from_str(&result.final_message)
                .map_err(|error| GardenerRunnerError::InvalidResult(error.to_string()))?;
            if parsed.proposed_goal_prompts.len() > 3 {
                return Err(GardenerRunnerError::InvalidResult(
                    "inspection returned more than three goal prompts".to_owned(),
                ));
            }
            self.heartbeat(store, claim)?;
            git.verify_head(&worktree, &source)?;
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
            }),
            (Err(error), Err(cleanup)) => Err(GardenerRunnerError::Cleanup {
                context: error.to_string(),
                cleanup: cleanup.to_string(),
            }),
        }
    }

    fn run_implementation(
        &self,
        store: &mut Store,
        claim: &Claim,
    ) -> Result<Success, GardenerRunnerError> {
        let root = self.config.validate(self.lease_seconds)?;
        let repository = store.gardener_repository()?.ok_or_else(|| {
            GardenerRunnerError::Configuration(
                "gardener runtime is enabled without a repository registration".to_owned(),
            )
        })?;
        let git = GitWorkspace::new(
            &repository.checkout_path,
            &self.config.git_executable,
            &self.config.gh_executable,
        )?;
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
            .gardener_proposal(&run.proposal_fingerprint)?
            .ok_or_else(|| StoreError::NotFound(run.proposal_fingerprint.clone()))?;
        let source = CommitId::parse(run.source_commit.clone())?;

        self.heartbeat(store, claim)?;
        let implementation = git.create_branch_worktree(&implementation_path, &branch, &source)?;
        self.heartbeat(store, claim)?;
        let mut verification: Option<RegisteredWorktree> = None;
        let operation = (|| {
            let prompt = implementation_prompt(&source, &proposal.prompt);
            let result = {
                let mut observer = StoreObserver::implementation(
                    store,
                    claim,
                    &run_id,
                    self.clock,
                    self.lease_seconds,
                );
                AppServerClient::new(&self.config.codex_executable)
                    .with_heartbeat_interval(self.config.heartbeat_interval)
                    .run(
                        &TurnRequest {
                            kind: TurnKind::Implementation,
                            cwd: implementation.path(),
                            prompt: &prompt,
                            output_schema: &implementation_schema(),
                        },
                        &mut observer,
                    )
                    .map_err(|error| GardenerRunnerError::AppServer(error.to_string()))?
            };
            let final_value: Value = serde_json::from_str(&result.final_message)
                .map_err(|error| GardenerRunnerError::InvalidResult(error.to_string()))?;
            if !final_value.is_object() {
                return Err(GardenerRunnerError::InvalidResult(
                    "implementation final message is not a JSON object".to_owned(),
                ));
            }
            store.finish_gardener_implementation(
                claim,
                &run_id,
                &result.final_message,
                self.clock.now(),
            )?;

            self.heartbeat(store, claim)?;
            git.verify_head(&implementation, &source)?;
            let commit = git.commit_all(&implementation, &commit_message(&proposal.prompt))?;
            self.heartbeat(store, claim)?;
            store.record_gardener_git_commit(claim, &run_id, commit.as_str(), self.clock.now())?;

            self.heartbeat(store, claim)?;
            git.push_branch(&implementation, &commit)?;
            self.heartbeat(store, claim)?;
            let pushed = git.observe_remote_branch(&branch, &commit)?;
            self.heartbeat(store, claim)?;
            store.record_gardener_push_observation(
                claim,
                &run_id,
                pushed.as_str(),
                self.clock.now(),
            )?;

            self.heartbeat(store, claim)?;
            let pull_request = git.create_ready_pull_request(
                &branch,
                &commit,
                &commit_message(&proposal.prompt),
                &pull_request_body(&source, &proposal.prompt),
            )?;
            self.heartbeat(store, claim)?;
            store.record_gardener_ready_pull_request(
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
            verification =
                Some(git.create_detached_worktree(&verification_path, &pull_request.head)?);
            self.heartbeat(store, claim)?;
            let verification_worktree = verification.as_ref().expect("worktree was created");
            git.verify_head(verification_worktree, &pull_request.head)?;
            let verification_prompt = verification_prompt(&pull_request.head, &proposal.prompt);
            let result = {
                let mut observer = StoreObserver::verification(
                    store,
                    claim,
                    &run_id,
                    self.clock,
                    self.lease_seconds,
                );
                AppServerClient::new(&self.config.codex_executable)
                    .with_heartbeat_interval(self.config.heartbeat_interval)
                    .run(
                        &TurnRequest {
                            kind: TurnKind::Verification,
                            cwd: verification_worktree.path(),
                            prompt: &verification_prompt,
                            output_schema: &verification_schema(),
                        },
                        &mut observer,
                    )
                    .map_err(|error| GardenerRunnerError::AppServer(error.to_string()))?
            };
            let verdict: GardenerVerificationResult =
                serde_json::from_str(&result.final_message)
                    .map_err(|error| GardenerRunnerError::InvalidResult(error.to_string()))?;
            let reported_head = CommitId::parse(verdict.head.clone())?;
            if reported_head != pull_request.head {
                return Err(GardenerRunnerError::InvalidResult(format!(
                    "verification reported head {reported_head}, expected exact pull-request head {}",
                    pull_request.head
                )));
            }
            self.heartbeat(store, claim)?;
            git.verify_head(verification_worktree, &pull_request.head)?;
            store.finish_gardener_verification(
                claim,
                &run_id,
                verdict.verdict,
                reported_head.as_str(),
                &verdict.summary,
                self.clock.now(),
            )?;
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
                        "created ready pull request {} at exact head {commit} and passed independent verification",
                        pull_request.url
                    )))
                } else {
                    Ok(Success::NeedsAttention {
                        error: format!(
                            "independent verification returned {} for exact head {commit}",
                            verdict.verdict
                        ),
                        evidence: format!(
                            "ready pull request {} is preserved at {commit}; verification summary: {}",
                            pull_request.url, verdict.summary
                        ),
                    })
                }
            }
            (Err(error), Ok(())) => Err(error),
            (Ok((commit, pull_request, _)), Err(cleanup)) => Err(GardenerRunnerError::Cleanup {
                context: format!(
                    "external work completed for ready pull request {} at {commit}",
                    pull_request.url
                ),
                cleanup: cleanup.to_string(),
            }),
            (Err(error), Err(cleanup)) => Err(GardenerRunnerError::Cleanup {
                context: error.to_string(),
                cleanup: cleanup.to_string(),
            }),
        }
    }

    fn heartbeat(&self, store: &mut Store, claim: &Claim) -> Result<(), GardenerRunnerError> {
        store.renew_lease(claim, self.clock.now(), self.lease_seconds)?;
        Ok(())
    }

    fn cleanup(
        &self,
        store: &mut Store,
        claim: &Claim,
        git: &GitWorkspace,
        worktree: &RegisteredWorktree,
    ) -> Result<(), GardenerRunnerError> {
        self.heartbeat(store, claim)?;
        git.remove_clean_worktree(worktree)?;
        self.heartbeat(store, claim)
    }
}

enum Success {
    Inspection(String),
    Implementation(String),
    NeedsAttention { error: String, evidence: String },
}

enum ObserverTarget<'a> {
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
            "summary": {"type": "string", "minLength": 1},
            "proposed_goal_prompts": {
                "type": "array",
                "maxItems": 3,
                "items": {"type": "string", "minLength": 1}
            }
        }
    })
}

fn implementation_schema() -> Value {
    json!({"type": "object", "additionalProperties": true})
}

fn verification_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["verdict", "head", "summary"],
        "properties": {
            "verdict": {"type": "string", "enum": ["pass", "blocking", "inconclusive"]},
            "head": {"type": "string", "pattern": "^[0-9a-f]{40}$"},
            "summary": {"type": "string", "minLength": 1}
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
        "Automated coding-gardener implementation from exact base `{source}`. This pull request is ready for review and is not merged automatically.\n\nApproved goal:\n\n{prompt}"
    )
}

#[cfg(test)]
mod tests;
