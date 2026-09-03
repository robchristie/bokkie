use std::{
    path::Path,
    str::FromStr,
    sync::atomic::{AtomicI64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{
    Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params, types::Type,
};
use serde_json::json;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    ApprovalDecision, Attempt, AttemptOutcome, AuditEvent, Claim, Completion, NewObligation,
    Obligation, ObligationState, Recurrence,
    gardener::{
        CANONICAL_DEFAULT_BRANCH, CANONICAL_REPOSITORY, GardenerEvent, GardenerImplementationRun,
        GardenerInspection, GardenerRunEvent, GardenerRunPhase, GardenerVerificationVerdict,
        InspectionResult, NewGardenerImplementationRun, NewGardenerInspection,
        NewRepositoryRegistration, Proposal, ProposalObservation, RepositoryRegistration,
        normalise_goal_prompt, proposal_fingerprint,
    },
    recurrence::RecurrenceError,
};

const MIGRATIONS: &[(i64, &str, &str)] = &[
    (
        1,
        "0001_obligation_kernel.sql",
        include_str!("../migrations/0001_obligation_kernel.sql"),
    ),
    (
        2,
        "0002_append_only_guards.sql",
        include_str!("../migrations/0002_append_only_guards.sql"),
    ),
    (
        3,
        "0003_coding_gardener_state.sql",
        include_str!("../migrations/0003_coding_gardener_state.sql"),
    ),
    (
        4,
        "0004_coding_gardener_runs.sql",
        include_str!("../migrations/0004_coding_gardener_runs.sql"),
    ),
];

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("SQLite error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Recurrence(#[from] RecurrenceError),
    #[error("obligation {0:?} was not found")]
    NotFound(String),
    #[error("invalid obligation: {0}")]
    Invalid(String),
    #[error("transition conflict: {0}")]
    Conflict(String),
    #[error("claim is stale or no longer owns the lease")]
    Fenced,
}

pub trait UnixClock {
    fn now(&self) -> i64;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl UnixClock for SystemClock {
    fn now(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before Unix epoch")
            .as_secs() as i64
    }
}

#[derive(Debug)]
pub struct ManualClock {
    now: AtomicI64,
}

impl ManualClock {
    pub fn new(now: i64) -> Self {
        Self {
            now: AtomicI64::new(now),
        }
    }

    pub fn set(&self, now: i64) {
        self.now.store(now, Ordering::SeqCst);
    }

    pub fn advance(&self, seconds: i64) {
        self.now.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl UnixClock for ManualClock {
    fn now(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
}

pub struct Store {
    connection: Connection,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let connection = Connection::open(path)?;
        Self::initialise(connection)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::initialise(Connection::open_in_memory()?)
    }

    fn initialise(connection: Connection) -> Result<Self, StoreError> {
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;

        let mut store = Self { connection };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<(), StoreError> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                applied_at INTEGER NOT NULL DEFAULT (unixepoch())
            );",
        )?;

        for &(version, name, sql) in MIGRATIONS {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let existing: Option<String> = transaction
                .query_row(
                    "SELECT name FROM schema_migrations WHERE version = ?1",
                    [version],
                    |row| row.get(0),
                )
                .optional()?;
            match existing {
                Some(existing) if existing == name => {
                    transaction.commit()?;
                    continue;
                }
                Some(existing) => {
                    return Err(StoreError::Invalid(format!(
                        "migration {version} is recorded as {existing:?}, expected {name:?}"
                    )));
                }
                None => {}
            }

            transaction.execute_batch(sql)?;
            transaction.execute(
                "INSERT INTO schema_migrations(version, name) VALUES (?1, ?2)",
                params![version, name],
            )?;
            transaction.commit()?;
        }
        Ok(())
    }

    pub fn create(&mut self, new: NewObligation, now: i64) -> Result<Obligation, StoreError> {
        validate_new(&new)?;
        let id = new.id.clone();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        apply_transition(&transaction, Transition::Create { new, now })?;
        transaction.commit()?;
        self.get(&id)?.ok_or(StoreError::NotFound(id))
    }

    pub fn get(&self, id: &str) -> Result<Option<Obligation>, StoreError> {
        self.connection
            .query_row(
                "SELECT * FROM obligations WHERE id = ?1",
                [id],
                obligation_from_row,
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn list(&self) -> Result<Vec<Obligation>, StoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT * FROM obligations ORDER BY created_at, id")?;
        let rows = statement.query_map([], obligation_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn decide_approval(
        &mut self,
        id: &str,
        decision: ApprovalDecision,
        actor: &str,
        note: Option<&str>,
        now: i64,
    ) -> Result<(), StoreError> {
        if actor.trim().is_empty() {
            return Err(StoreError::Invalid(
                "approval actor must not be empty".to_owned(),
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        apply_transition(
            &transaction,
            Transition::Approval {
                id,
                decision,
                actor,
                note,
                now,
            },
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Recover every expired lease and consume the already-persisted attempt.
    pub fn recover_expired_leases(&mut self, now: i64) -> Result<usize, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count = recover_expired_in_transaction(&transaction, now)?;
        transaction.commit()?;
        Ok(count)
    }

    /// Atomically recover stale leases and claim up to `limit` due obligations.
    pub fn claim_due(
        &mut self,
        now: i64,
        lease_seconds: i64,
        limit: usize,
    ) -> Result<Vec<Claim>, StoreError> {
        self.claim_due_for_runner(now, lease_seconds, limit, ClaimRunner::Ordinary)
    }

    /// Claim only obligations owned by the coding-gardener runner.
    ///
    /// This uses the same obligation transition and lease fencing as ordinary
    /// work; the binding only prevents an incompatible runner from selecting it.
    pub fn claim_due_gardener(
        &mut self,
        now: i64,
        lease_seconds: i64,
        limit: usize,
    ) -> Result<Vec<Claim>, StoreError> {
        self.claim_due_for_runner(now, lease_seconds, limit, ClaimRunner::Gardener)
    }

    fn claim_due_for_runner(
        &mut self,
        now: i64,
        lease_seconds: i64,
        limit: usize,
        runner: ClaimRunner,
    ) -> Result<Vec<Claim>, StoreError> {
        if lease_seconds <= 0 {
            return Err(StoreError::Invalid(
                "lease duration must be positive".to_owned(),
            ));
        }
        if limit == 0 {
            return Ok(Vec::new());
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        recover_expired_in_transaction(&transaction, now)?;
        let binding_predicate = match runner {
            ClaimRunner::Ordinary => {
                "NOT EXISTS (SELECT 1 FROM gardener_obligation_bindings g
                             WHERE g.obligation_id = o.id)"
            }
            ClaimRunner::Gardener => {
                "EXISTS (SELECT 1 FROM gardener_obligation_bindings g
                         WHERE g.obligation_id = o.id)"
            }
        };
        let query = format!(
            "SELECT o.id
             FROM obligations o
             WHERE o.state IN ('pending', 'retry_scheduled')
               AND o.next_wake_at <= ?1
               AND {binding_predicate}
               AND (
                   o.approval_required = 0 OR
                   (SELECT decision FROM approvals a
                    WHERE a.obligation_id = o.id AND a.occurrence = o.occurrence
                    ORDER BY a.id DESC LIMIT 1) = 'approved'
               )
             ORDER BY o.next_wake_at, o.id
             LIMIT ?2"
        );
        let ids = {
            let mut statement = transaction.prepare(&query)?;
            statement
                .query_map(params![now, limit as i64], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };

        let mut claims = Vec::with_capacity(ids.len());
        for id in ids {
            let claim = apply_transition(
                &transaction,
                Transition::Claim {
                    id: &id,
                    now,
                    lease_seconds,
                },
            )?
            .claim
            .expect("claim transition returns a claim");
            claims.push(claim);
        }
        transaction.commit()?;
        Ok(claims)
    }

    pub fn register_gardener_repository(
        &mut self,
        registration: NewRepositoryRegistration,
        now: i64,
    ) -> Result<RepositoryRegistration, StoreError> {
        validate_registration(&registration)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(existing) = repository_registration(&transaction, &registration.repository)? {
            if registration_matches(&existing, &registration) {
                transaction.commit()?;
                return Ok(existing);
            }
            return Err(StoreError::Conflict(format!(
                "repository {:?} is already registered with different configuration",
                registration.repository
            )));
        }

        let obligation_id = "gardener:inspect:robchristie/bokkie".to_owned();
        apply_transition(
            &transaction,
            Transition::Create {
                new: NewObligation {
                    id: obligation_id.clone(),
                    description: "Inspect robchristie/bokkie for gardening opportunities"
                        .to_owned(),
                    scheduled_at: registration.first_inspection_at,
                    recurrence: Some(registration.inspection_recurrence.clone()),
                    approval_required: false,
                    retry: crate::RetryPolicy::default(),
                },
                now,
            },
        )?;
        transaction.execute(
            "INSERT INTO gardener_obligation_bindings(obligation_id, kind, created_at)
             VALUES (?1, 'inspection', ?2)",
            params![obligation_id, now],
        )?;
        transaction.execute(
            "INSERT INTO gardener_repositories(
                repository, default_branch, checkout_path, inspection_cron,
                inspection_timezone, first_inspection_at, inspection_obligation_id,
                created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                registration.repository,
                registration.default_branch,
                registration.checkout_path,
                registration.inspection_recurrence.expression(),
                registration.inspection_recurrence.timezone(),
                registration.first_inspection_at,
                obligation_id,
                now
            ],
        )?;
        append_gardener_event(
            &transaction,
            CANONICAL_REPOSITORY,
            None,
            None,
            "repository_registered",
            now,
            json!({"inspection_obligation_id": obligation_id}),
        )?;
        let created = repository_registration(&transaction, CANONICAL_REPOSITORY)?
            .expect("registration was inserted");
        transaction.commit()?;
        Ok(created)
    }

    pub fn gardener_repository(&self) -> Result<Option<RepositoryRegistration>, StoreError> {
        repository_registration(&self.connection, CANONICAL_REPOSITORY)
    }

    pub fn start_gardener_inspection(
        &mut self,
        claim: &Claim,
        inspection: NewGardenerInspection,
        now: i64,
    ) -> Result<GardenerInspection, StoreError> {
        validate_inspection(&inspection)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let obligation = require_obligation(&transaction, &claim.obligation_id)?;
        verify_claim(&obligation, claim, now)?;
        require_gardener_kind(&transaction, &claim.obligation_id, "inspection")?;
        let repository =
            registration_for_inspection_obligation(&transaction, &claim.obligation_id)?;
        transaction.execute(
            "INSERT INTO gardener_inspections(
                id, repository, obligation_id, occurrence, lease_generation, lease_token,
                source_commit, worktree_path, prompt_digest, started_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                inspection.id,
                repository,
                claim.obligation_id,
                claim.occurrence,
                claim.lease_generation,
                claim.lease_token,
                inspection.source_commit,
                inspection.worktree_path,
                inspection.prompt_digest,
                now
            ],
        )?;
        append_gardener_event(
            &transaction,
            &repository,
            Some(&inspection.id),
            None,
            "inspection_started",
            now,
            json!({
                "source_commit": inspection.source_commit,
                "worktree_path": inspection.worktree_path,
                "prompt_digest": inspection.prompt_digest,
                "lease_generation": claim.lease_generation
            }),
        )?;
        let created =
            gardener_inspection(&transaction, &inspection.id)?.expect("inspection was inserted");
        transaction.commit()?;
        Ok(created)
    }

    pub fn gardener_inspection(&self, id: &str) -> Result<Option<GardenerInspection>, StoreError> {
        gardener_inspection(&self.connection, id)
    }

    pub fn record_inspection_codex_thread(
        &mut self,
        claim: &Claim,
        inspection_id: &str,
        thread_id: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        self.record_inspection_codex_identity(
            claim,
            inspection_id,
            "codex_thread_id",
            thread_id,
            now,
        )
    }

    pub fn record_inspection_codex_turn(
        &mut self,
        claim: &Claim,
        inspection_id: &str,
        turn_id: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        self.record_inspection_codex_identity(claim, inspection_id, "codex_turn_id", turn_id, now)
    }

    fn record_inspection_codex_identity(
        &mut self,
        claim: &Claim,
        inspection_id: &str,
        column: &str,
        value: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        if value.trim().is_empty() {
            return Err(StoreError::Invalid(
                "Codex identity must not be empty".to_owned(),
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inspection = require_current_inspection(&transaction, claim, inspection_id, now)?;
        let existing = match column {
            "codex_thread_id" => inspection.codex_thread_id.as_deref(),
            "codex_turn_id" => inspection.codex_turn_id.as_deref(),
            _ => unreachable!("identity columns are fixed by the public methods"),
        };
        if let Some(existing) = existing {
            if existing == value {
                transaction.commit()?;
                return Ok(());
            }
            return Err(StoreError::Conflict(format!(
                "inspection {inspection_id:?} already has a different {column}"
            )));
        }
        let query = format!("UPDATE gardener_inspections SET {column} = ?2 WHERE id = ?1");
        transaction.execute(&query, params![inspection_id, value])?;
        append_gardener_event(
            &transaction,
            &inspection.repository,
            Some(inspection_id),
            None,
            "inspection_identity_recorded",
            now,
            json!({"identity": column, "value": value}),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn finish_gardener_inspection(
        &mut self,
        claim: &Claim,
        inspection_id: &str,
        result: &InspectionResult,
        now: i64,
    ) -> Result<Vec<Proposal>, StoreError> {
        validate_inspection_result(result)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inspection = require_current_inspection(&transaction, claim, inspection_id, now)?;
        if inspection.result_json.is_some() {
            return Err(StoreError::Conflict(format!(
                "inspection {inspection_id:?} already has a terminal result"
            )));
        }
        let result_json = serde_json::to_string(result)
            .map_err(|error| StoreError::Invalid(format!("invalid inspection result: {error}")))?;
        transaction.execute(
            "UPDATE gardener_inspections SET result_json = ?2, completed_at = ?3 WHERE id = ?1",
            params![inspection_id, result_json, now],
        )?;

        let mut proposals = Vec::new();
        for raw_prompt in &result.proposed_goal_prompts {
            let prompt = normalise_goal_prompt(raw_prompt);
            if prompt.is_empty() {
                return Err(StoreError::Invalid(
                    "proposed goal prompt must not be empty".to_owned(),
                ));
            }
            let fingerprint = proposal_fingerprint(&inspection.repository, &prompt);
            let obligation_id = format!("gardener:implement:{fingerprint}");
            let existing = proposal(&transaction, &fingerprint)?;
            if existing.is_none() {
                apply_transition(
                    &transaction,
                    Transition::Create {
                        new: NewObligation {
                            id: obligation_id.clone(),
                            description: format!(
                                "Implement approved gardener proposal {fingerprint}"
                            ),
                            scheduled_at: now,
                            recurrence: None,
                            approval_required: true,
                            retry: crate::RetryPolicy {
                                max_attempts: 1,
                                ..crate::RetryPolicy::default()
                            },
                        },
                        now,
                    },
                )?;
                transaction.execute(
                    "INSERT INTO gardener_obligation_bindings(obligation_id, kind, created_at)
                     VALUES (?1, 'implementation', ?2)",
                    params![obligation_id, now],
                )?;
                transaction.execute(
                    "INSERT INTO gardener_proposals(
                        fingerprint, repository, prompt, implementation_obligation_id, created_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        fingerprint,
                        inspection.repository,
                        prompt,
                        obligation_id,
                        now
                    ],
                )?;
                append_gardener_event(
                    &transaction,
                    &inspection.repository,
                    Some(inspection_id),
                    Some(&fingerprint),
                    "proposal_created",
                    now,
                    json!({"implementation_obligation_id": obligation_id}),
                )?;
            }
            let inserted = transaction.execute(
                "INSERT INTO gardener_proposal_observations(
                    proposal_fingerprint, inspection_id, source_commit, observed_at
                 ) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(proposal_fingerprint, inspection_id) DO NOTHING",
                params![fingerprint, inspection_id, inspection.source_commit, now],
            )?;
            if inserted == 1 {
                append_gardener_event(
                    &transaction,
                    &inspection.repository,
                    Some(inspection_id),
                    Some(&fingerprint),
                    "proposal_observed",
                    now,
                    json!({"source_commit": inspection.source_commit}),
                )?;
            }
            let item = proposal(&transaction, &fingerprint)?.expect("proposal exists");
            if !proposals
                .iter()
                .any(|known: &Proposal| known.fingerprint == item.fingerprint)
            {
                proposals.push(item);
            }
        }
        append_gardener_event(
            &transaction,
            &inspection.repository,
            Some(inspection_id),
            None,
            "inspection_completed",
            now,
            json!({"proposal_count": proposals.len()}),
        )?;
        transaction.commit()?;
        Ok(proposals)
    }

    pub fn gardener_proposal(&self, fingerprint: &str) -> Result<Option<Proposal>, StoreError> {
        proposal(&self.connection, fingerprint)
    }

    pub fn gardener_obligation_kind(
        &self,
        obligation_id: &str,
    ) -> Result<Option<crate::GardenerObligationKind>, StoreError> {
        self.connection
            .query_row(
                "SELECT kind FROM gardener_obligation_bindings WHERE obligation_id = ?1",
                [obligation_id],
                |row| match row.get::<_, String>(0)?.as_str() {
                    "inspection" => Ok(crate::GardenerObligationKind::Inspection),
                    "implementation" => Ok(crate::GardenerObligationKind::Implementation),
                    value => Err(rusqlite::Error::FromSqlConversionFailure(
                        0,
                        Type::Text,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("unknown gardener obligation kind {value:?}"),
                        )),
                    )),
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn proposal_observations(
        &self,
        fingerprint: &str,
    ) -> Result<Vec<ProposalObservation>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id, proposal_fingerprint, inspection_id, source_commit, observed_at
             FROM gardener_proposal_observations
             WHERE proposal_fingerprint = ?1 ORDER BY id",
        )?;
        let rows = statement.query_map([fingerprint], |row| {
            Ok(ProposalObservation {
                id: row.get(0)?,
                proposal_fingerprint: row.get(1)?,
                inspection_id: row.get(2)?,
                source_commit: row.get(3)?,
                observed_at: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn decide_gardener_proposal(
        &mut self,
        fingerprint: &str,
        decision: ApprovalDecision,
        actor: &str,
        note: Option<&str>,
        now: i64,
    ) -> Result<Proposal, StoreError> {
        if actor.trim().is_empty() {
            return Err(StoreError::Invalid(
                "approval actor must not be empty".to_owned(),
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = proposal(&transaction, fingerprint)?
            .ok_or_else(|| StoreError::NotFound(fingerprint.to_owned()))?;
        apply_transition(
            &transaction,
            Transition::Approval {
                id: &current.implementation_obligation_id,
                decision,
                actor,
                note,
                now,
            },
        )?;
        append_gardener_event(
            &transaction,
            &current.repository,
            None,
            Some(fingerprint),
            &format!("proposal_{decision}"),
            now,
            json!({"actor": actor, "note": note}),
        )?;
        let decided = proposal(&transaction, fingerprint)?.expect("proposal remains present");
        transaction.commit()?;
        Ok(decided)
    }

    pub fn gardener_events(&self) -> Result<Vec<GardenerEvent>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT sequence, repository, inspection_id, proposal_fingerprint,
                    event_type, occurred_at, details_json
             FROM gardener_events ORDER BY sequence",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(GardenerEvent {
                sequence: row.get(0)?,
                repository: row.get(1)?,
                inspection_id: row.get(2)?,
                proposal_fingerprint: row.get(3)?,
                event_type: row.get(4)?,
                occurred_at: row.get(5)?,
                details_json: row.get(6)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Persist the local intent for one approved implementation claim before
    /// any implementation process or Git side effect starts.
    pub fn create_gardener_implementation_run(
        &mut self,
        claim: &Claim,
        new: NewGardenerImplementationRun,
        now: i64,
    ) -> Result<GardenerImplementationRun, StoreError> {
        validate_new_implementation_run(&new)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let obligation = require_obligation(&transaction, &claim.obligation_id)?;
        verify_claim(&obligation, claim, now)?;
        require_gardener_kind(&transaction, &claim.obligation_id, "implementation")?;

        let (fingerprint, repository): (String, String) = transaction
            .query_row(
                "SELECT p.fingerprint, p.repository
                 FROM gardener_proposals p
                 WHERE p.implementation_obligation_id = ?1
                   AND EXISTS (
                       SELECT 1 FROM approvals a
                       WHERE a.obligation_id = p.implementation_obligation_id
                         AND a.occurrence = ?2 AND a.decision = 'approved'
                   )",
                params![claim.obligation_id, claim.occurrence],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::Conflict(format!(
                    "implementation obligation {:?} lacks a current approved proposal",
                    claim.obligation_id
                ))
            })?;
        let source_commit: String = transaction
            .query_row(
                "SELECT source_commit FROM gardener_proposal_observations
                 WHERE proposal_fingerprint = ?1 ORDER BY id DESC LIMIT 1",
                [&fingerprint],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::Conflict(format!(
                    "proposal {fingerprint:?} has no source observation"
                ))
            })?;

        if let Some(existing) = gardener_implementation_run_for_lease(
            &transaction,
            &claim.obligation_id,
            claim.lease_generation,
        )? {
            if existing.id == new.id
                && existing.proposal_fingerprint == fingerprint
                && existing.occurrence == claim.occurrence
                && existing.attempt_number == claim.attempt_number
                && existing.source_commit == source_commit
                && existing.implementation_worktree_path == new.implementation_worktree_path
                && existing.branch == new.branch
            {
                transaction.commit()?;
                return Ok(existing);
            }
            return Err(StoreError::Conflict(format!(
                "claim {:?} generation {} already has a different implementation run",
                claim.obligation_id, claim.lease_generation
            )));
        }
        if gardener_implementation_run(&transaction, &new.id)?.is_some() {
            return Err(StoreError::Conflict(format!(
                "implementation run id {:?} is already in use",
                new.id
            )));
        }

        transaction.execute(
            "INSERT INTO gardener_implementation_runs(
                id, repository, proposal_fingerprint, obligation_id, occurrence,
                attempt_number, lease_generation, lease_token, source_commit,
                implementation_worktree_path, branch, phase, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'created', ?12, ?12)",
            params![
                new.id,
                repository,
                fingerprint,
                claim.obligation_id,
                claim.occurrence,
                claim.attempt_number,
                claim.lease_generation,
                claim.lease_token,
                source_commit,
                new.implementation_worktree_path,
                new.branch,
                now,
            ],
        )?;
        append_gardener_run_event(
            &transaction,
            &new.id,
            "implementation_run_created",
            now,
            json!({
                "proposal_fingerprint": fingerprint,
                "source_commit": source_commit,
                "worktree_path": new.implementation_worktree_path,
                "branch": new.branch,
                "occurrence": claim.occurrence,
                "attempt_number": claim.attempt_number,
                "lease_generation": claim.lease_generation
            }),
        )?;
        let created = gardener_implementation_run(&transaction, &new.id)?
            .expect("implementation run was inserted");
        transaction.commit()?;
        Ok(created)
    }

    pub fn gardener_implementation_run(
        &self,
        id: &str,
    ) -> Result<Option<GardenerImplementationRun>, StoreError> {
        gardener_implementation_run(&self.connection, id)
    }

    pub fn gardener_implementation_runs(
        &self,
    ) -> Result<Vec<GardenerImplementationRun>, StoreError> {
        query_gardener_implementation_runs(
            &self.connection,
            "SELECT * FROM gardener_implementation_runs ORDER BY created_at, id",
            [],
        )
    }

    pub fn gardener_implementation_runs_for_obligation(
        &self,
        obligation_id: &str,
    ) -> Result<Vec<GardenerImplementationRun>, StoreError> {
        query_gardener_implementation_runs(
            &self.connection,
            "SELECT * FROM gardener_implementation_runs
             WHERE obligation_id = ?1 ORDER BY lease_generation, id",
            [obligation_id],
        )
    }

    pub fn gardener_run_events(&self, run_id: &str) -> Result<Vec<GardenerRunEvent>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT sequence, run_id, event_type, occurred_at, details_json
             FROM gardener_run_events WHERE run_id = ?1 ORDER BY sequence",
        )?;
        let rows = statement.query_map([run_id], |row| {
            Ok(GardenerRunEvent {
                sequence: row.get(0)?,
                run_id: row.get(1)?,
                event_type: row.get(2)?,
                occurred_at: row.get(3)?,
                details_json: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn record_implementation_codex_thread(
        &mut self,
        claim: &Claim,
        run_id: &str,
        thread_id: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        validate_nonempty("implementation Codex thread id", thread_id)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = require_current_gardener_run(&transaction, claim, run_id, now)?;
        if idempotent_or_conflict(
            run_id,
            "implementation Codex thread id",
            run.implementation_thread_id.as_deref(),
            thread_id,
        )? {
            transaction.commit()?;
            return Ok(());
        }
        require_run_phase(&run, GardenerRunPhase::Created)?;
        transaction.execute(
            "UPDATE gardener_implementation_runs
             SET implementation_thread_id = ?2, implementation_thread_recorded_at = ?3,
                 phase = 'implementation_thread_recorded', updated_at = ?3
             WHERE id = ?1",
            params![run_id, thread_id, now],
        )?;
        append_gardener_run_event(
            &transaction,
            run_id,
            "implementation_thread_recorded",
            now,
            json!({"thread_id": thread_id}),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn record_implementation_codex_turn(
        &mut self,
        claim: &Claim,
        run_id: &str,
        turn_id: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        validate_nonempty("implementation Codex turn id", turn_id)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = require_current_gardener_run(&transaction, claim, run_id, now)?;
        if idempotent_or_conflict(
            run_id,
            "implementation Codex turn id",
            run.implementation_turn_id.as_deref(),
            turn_id,
        )? {
            transaction.commit()?;
            return Ok(());
        }
        require_run_phase(&run, GardenerRunPhase::ImplementationThreadRecorded)?;
        transaction.execute(
            "UPDATE gardener_implementation_runs
             SET implementation_turn_id = ?2, implementation_turn_recorded_at = ?3,
                 phase = 'implementation_turn_recorded', updated_at = ?3
             WHERE id = ?1",
            params![run_id, turn_id, now],
        )?;
        append_gardener_run_event(
            &transaction,
            run_id,
            "implementation_turn_recorded",
            now,
            json!({"turn_id": turn_id}),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn finish_gardener_implementation(
        &mut self,
        claim: &Claim,
        run_id: &str,
        final_message_json: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        validate_structured_message(final_message_json)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = require_current_gardener_run(&transaction, claim, run_id, now)?;
        if idempotent_or_conflict(
            run_id,
            "implementation final message",
            run.implementation_final_message_json.as_deref(),
            final_message_json,
        )? {
            transaction.commit()?;
            return Ok(());
        }
        require_run_phase(&run, GardenerRunPhase::ImplementationTurnRecorded)?;
        transaction.execute(
            "UPDATE gardener_implementation_runs
             SET implementation_final_message_json = ?2, implementation_finished_at = ?3,
                 phase = 'implementation_finished', updated_at = ?3
             WHERE id = ?1",
            params![run_id, final_message_json, now],
        )?;
        append_gardener_run_event(
            &transaction,
            run_id,
            "implementation_finished",
            now,
            json!({
                "final_message": serde_json::from_str::<serde_json::Value>(final_message_json)
                    .expect("structured message was validated")
            }),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn record_gardener_git_commit(
        &mut self,
        claim: &Claim,
        run_id: &str,
        git_commit: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        validate_hex_identity("Git commit", git_commit, &[40, 64])?;
        self.record_gardener_run_head(
            claim,
            run_id,
            git_commit,
            GardenerRunPhase::ImplementationFinished,
            "git_commit",
            "git_commit_recorded_at",
            GardenerRunPhase::GitCommitRecorded,
            "git_commit_recorded",
            now,
        )
    }

    pub fn record_gardener_push_observation(
        &mut self,
        claim: &Claim,
        run_id: &str,
        pushed_head: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        validate_hex_identity("pushed head", pushed_head, &[40, 64])?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = require_current_gardener_run(&transaction, claim, run_id, now)?;
        if idempotent_or_conflict(
            run_id,
            "pushed head",
            run.pushed_head.as_deref(),
            pushed_head,
        )? {
            transaction.commit()?;
            return Ok(());
        }
        require_run_phase(&run, GardenerRunPhase::GitCommitRecorded)?;
        if run.git_commit.as_deref() != Some(pushed_head) {
            return Err(StoreError::Conflict(format!(
                "run {run_id:?} pushed head does not equal its recorded Git commit"
            )));
        }
        transaction.execute(
            "UPDATE gardener_implementation_runs
             SET pushed_head = ?2, push_observed_at = ?3,
                 phase = 'push_observed', updated_at = ?3 WHERE id = ?1",
            params![run_id, pushed_head, now],
        )?;
        append_gardener_run_event(
            &transaction,
            run_id,
            "push_observed",
            now,
            json!({"head": pushed_head, "branch": run.branch}),
        )?;
        transaction.commit()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    /// Record a GitHub pull request only after it has been observed ready.
    pub fn record_gardener_ready_pull_request(
        &mut self,
        claim: &Claim,
        run_id: &str,
        number: u64,
        url: &str,
        head: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        if number == 0 || number > i64::MAX as u64 {
            return Err(StoreError::Invalid(
                "GitHub pull-request number must be positive".to_owned(),
            ));
        }
        let expected_url = format!("https://github.com/{CANONICAL_REPOSITORY}/pull/{number}");
        if url != expected_url {
            return Err(StoreError::Invalid(format!(
                "GitHub pull-request URL must be {expected_url:?}"
            )));
        }
        validate_hex_identity("pull-request head", head, &[40, 64])?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = require_current_gardener_run(&transaction, claim, run_id, now)?;
        if let Some(existing_number) = run.pull_request_number {
            if existing_number == number
                && run.pull_request_url.as_deref() == Some(url)
                && run.pull_request_head.as_deref() == Some(head)
            {
                transaction.commit()?;
                return Ok(());
            }
            return Err(StoreError::Conflict(format!(
                "run {run_id:?} already has a different GitHub pull request"
            )));
        }
        require_run_phase(&run, GardenerRunPhase::PushObserved)?;
        if run.git_commit.as_deref() != Some(head) {
            return Err(StoreError::Conflict(format!(
                "run {run_id:?} pull-request head does not equal its recorded Git commit"
            )));
        }
        transaction.execute(
            "UPDATE gardener_implementation_runs
             SET pull_request_number = ?2, pull_request_url = ?3, pull_request_head = ?4,
                 pull_request_recorded_at = ?5, phase = 'pull_request_ready', updated_at = ?5
             WHERE id = ?1",
            params![run_id, number as i64, url, head, now],
        )?;
        append_gardener_run_event(
            &transaction,
            run_id,
            "pull_request_ready",
            now,
            json!({"number": number, "url": url, "head": head}),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn start_gardener_verification(
        &mut self,
        claim: &Claim,
        run_id: &str,
        worktree_path: &str,
        head: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        validate_absolute_path("verification worktree path", worktree_path)?;
        validate_hex_identity("verification head", head, &[40, 64])?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = require_current_gardener_run(&transaction, claim, run_id, now)?;
        if let Some(existing_path) = run.verification_worktree_path.as_deref() {
            if existing_path == worktree_path && run.verification_head.as_deref() == Some(head) {
                transaction.commit()?;
                return Ok(());
            }
            return Err(StoreError::Conflict(format!(
                "run {run_id:?} already has a different verification intent"
            )));
        }
        require_run_phase(&run, GardenerRunPhase::PullRequestReady)?;
        if run.pull_request_head.as_deref() != Some(head) {
            return Err(StoreError::Conflict(format!(
                "run {run_id:?} verification head does not equal its stored pull-request head"
            )));
        }
        if run.implementation_worktree_path == worktree_path {
            return Err(StoreError::Conflict(format!(
                "run {run_id:?} verification must use a separate worktree"
            )));
        }
        transaction.execute(
            "UPDATE gardener_implementation_runs
             SET verification_worktree_path = ?2, verification_head = ?3,
                 verification_started_at = ?4, phase = 'verification_started', updated_at = ?4
             WHERE id = ?1",
            params![run_id, worktree_path, head, now],
        )?;
        append_gardener_run_event(
            &transaction,
            run_id,
            "verification_started",
            now,
            json!({"worktree_path": worktree_path, "head": head}),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn record_verification_codex_thread(
        &mut self,
        claim: &Claim,
        run_id: &str,
        thread_id: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        validate_nonempty("verification Codex thread id", thread_id)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = require_current_gardener_run(&transaction, claim, run_id, now)?;
        if idempotent_or_conflict(
            run_id,
            "verification Codex thread id",
            run.verification_thread_id.as_deref(),
            thread_id,
        )? {
            transaction.commit()?;
            return Ok(());
        }
        require_run_phase(&run, GardenerRunPhase::VerificationStarted)?;
        if run.implementation_thread_id.as_deref() == Some(thread_id) {
            return Err(StoreError::Conflict(format!(
                "run {run_id:?} verification must use a fresh Codex thread"
            )));
        }
        transaction.execute(
            "UPDATE gardener_implementation_runs
             SET verification_thread_id = ?2, verification_thread_recorded_at = ?3,
                 phase = 'verification_thread_recorded', updated_at = ?3 WHERE id = ?1",
            params![run_id, thread_id, now],
        )?;
        append_gardener_run_event(
            &transaction,
            run_id,
            "verification_thread_recorded",
            now,
            json!({"thread_id": thread_id}),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn record_verification_codex_turn(
        &mut self,
        claim: &Claim,
        run_id: &str,
        turn_id: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        validate_nonempty("verification Codex turn id", turn_id)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = require_current_gardener_run(&transaction, claim, run_id, now)?;
        if idempotent_or_conflict(
            run_id,
            "verification Codex turn id",
            run.verification_turn_id.as_deref(),
            turn_id,
        )? {
            transaction.commit()?;
            return Ok(());
        }
        require_run_phase(&run, GardenerRunPhase::VerificationThreadRecorded)?;
        if run.implementation_turn_id.as_deref() == Some(turn_id) {
            return Err(StoreError::Conflict(format!(
                "run {run_id:?} verification must use a fresh Codex turn"
            )));
        }
        transaction.execute(
            "UPDATE gardener_implementation_runs
             SET verification_turn_id = ?2, verification_turn_recorded_at = ?3,
                 phase = 'verification_turn_recorded', updated_at = ?3 WHERE id = ?1",
            params![run_id, turn_id, now],
        )?;
        append_gardener_run_event(
            &transaction,
            run_id,
            "verification_turn_recorded",
            now,
            json!({"turn_id": turn_id}),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn finish_gardener_verification(
        &mut self,
        claim: &Claim,
        run_id: &str,
        verdict: GardenerVerificationVerdict,
        reported_head: &str,
        summary: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        validate_hex_identity("verification reported head", reported_head, &[40, 64])?;
        validate_nonempty("verification summary", summary)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = require_current_gardener_run(&transaction, claim, run_id, now)?;
        if let Some(existing) = run.verification_verdict {
            if existing == verdict
                && run.verification_reported_head.as_deref() == Some(reported_head)
                && run.verification_summary.as_deref() == Some(summary)
            {
                transaction.commit()?;
                return Ok(());
            }
            return Err(StoreError::Conflict(format!(
                "run {run_id:?} already has a different verification verdict"
            )));
        }
        require_run_phase(&run, GardenerRunPhase::VerificationTurnRecorded)?;
        if run.pull_request_head.as_deref() != Some(reported_head)
            || run.verification_head.as_deref() != Some(reported_head)
        {
            return Err(StoreError::Conflict(format!(
                "run {run_id:?} verification reported a head other than the stored pull-request head"
            )));
        }
        transaction.execute(
            "UPDATE gardener_implementation_runs
             SET verification_verdict = ?2, verification_reported_head = ?3,
                 verification_summary = ?4, verification_finished_at = ?5,
                 phase = 'verification_finished', updated_at = ?5 WHERE id = ?1",
            params![run_id, verdict.to_string(), reported_head, summary, now],
        )?;
        append_gardener_run_event(
            &transaction,
            run_id,
            "verification_finished",
            now,
            json!({"verdict": verdict, "reported_head": reported_head, "summary": summary}),
        )?;
        transaction.commit()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn record_gardener_run_head(
        &mut self,
        claim: &Claim,
        run_id: &str,
        value: &str,
        expected_phase: GardenerRunPhase,
        column: &str,
        timestamp_column: &str,
        next_phase: GardenerRunPhase,
        event_type: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = require_current_gardener_run(&transaction, claim, run_id, now)?;
        let existing = match column {
            "git_commit" => run.git_commit.as_deref(),
            _ => unreachable!("run head columns are fixed by callers"),
        };
        if idempotent_or_conflict(run_id, column, existing, value)? {
            transaction.commit()?;
            return Ok(());
        }
        require_run_phase(&run, expected_phase)?;
        let query = format!(
            "UPDATE gardener_implementation_runs SET {column} = ?2, {timestamp_column} = ?3,
             phase = ?4, updated_at = ?3 WHERE id = ?1"
        );
        transaction.execute(&query, params![run_id, value, now, next_phase.to_string()])?;
        append_gardener_run_event(
            &transaction,
            run_id,
            event_type,
            now,
            json!({"head": value}),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn renew_lease(
        &mut self,
        claim: &Claim,
        now: i64,
        lease_seconds: i64,
    ) -> Result<i64, StoreError> {
        if lease_seconds <= 0 {
            return Err(StoreError::Invalid(
                "lease duration must be positive".to_owned(),
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = apply_transition(
            &transaction,
            Transition::Renew {
                claim,
                now,
                lease_seconds,
            },
        )?;
        transaction.commit()?;
        Ok(result.lease_expires_at.expect("renew returns lease expiry"))
    }

    pub fn complete(
        &mut self,
        claim: &Claim,
        completion: Completion,
        now: i64,
    ) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        apply_transition(
            &transaction,
            Transition::Complete {
                claim,
                completion,
                now,
            },
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn retry_attention(&mut self, id: &str, now: i64) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        apply_transition(&transaction, Transition::RetryAttention { id, now })?;
        transaction.commit()?;
        Ok(())
    }

    pub fn cancel(&mut self, id: &str, now: i64) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        apply_transition(&transaction, Transition::Cancel { id, now })?;
        transaction.commit()?;
        Ok(())
    }

    pub fn attempts(&self, id: &str) -> Result<Vec<Attempt>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id, obligation_id, occurrence, attempt_number, lease_generation,
                    lease_token, claimed_at, completed_at, outcome, retryable, error, evidence
             FROM attempts WHERE obligation_id = ?1 ORDER BY id",
        )?;
        let rows = statement.query_map([id], attempt_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn events(&self, id: &str) -> Result<Vec<AuditEvent>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT sequence, obligation_id, occurrence, event_type, occurred_at,
                    from_state, to_state, details_json
             FROM audit_events WHERE obligation_id = ?1 ORDER BY sequence",
        )?;
        let rows = statement.query_map([id], audit_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    #[cfg(test)]
    fn pragma_string(&self, name: &str) -> String {
        self.connection
            .pragma_query_value(None, name, |row| row.get(0))
            .unwrap()
    }

    #[cfg(test)]
    fn pragma_i64(&self, name: &str) -> i64 {
        self.connection
            .pragma_query_value(None, name, |row| row.get(0))
            .unwrap()
    }
}

#[derive(Debug, Clone, Copy)]
enum ClaimRunner {
    Ordinary,
    Gardener,
}

fn validate_registration(registration: &NewRepositoryRegistration) -> Result<(), StoreError> {
    if registration.repository != CANONICAL_REPOSITORY {
        return Err(StoreError::Invalid(format!(
            "only canonical repository {CANONICAL_REPOSITORY:?} can be registered"
        )));
    }
    if registration.default_branch != CANONICAL_DEFAULT_BRANCH {
        return Err(StoreError::Invalid(format!(
            "canonical repository default branch must be {CANONICAL_DEFAULT_BRANCH:?}"
        )));
    }
    if registration.checkout_path.trim().is_empty()
        || !Path::new(&registration.checkout_path).is_absolute()
    {
        return Err(StoreError::Invalid(
            "repository checkout path must be absolute".to_owned(),
        ));
    }
    Ok(())
}

fn registration_matches(
    existing: &RepositoryRegistration,
    requested: &NewRepositoryRegistration,
) -> bool {
    existing.repository == requested.repository
        && existing.default_branch == requested.default_branch
        && existing.checkout_path == requested.checkout_path
        && existing.inspection_cron == requested.inspection_recurrence.expression()
        && existing.inspection_timezone == requested.inspection_recurrence.timezone()
        && existing.first_inspection_at == requested.first_inspection_at
}

fn repository_registration(
    connection: &Connection,
    repository: &str,
) -> Result<Option<RepositoryRegistration>, StoreError> {
    connection
        .query_row(
            "SELECT repository, default_branch, checkout_path, inspection_cron,
                    inspection_timezone, first_inspection_at, inspection_obligation_id,
                    created_at, updated_at
             FROM gardener_repositories WHERE repository = ?1",
            [repository],
            |row| {
                Ok(RepositoryRegistration {
                    repository: row.get(0)?,
                    default_branch: row.get(1)?,
                    checkout_path: row.get(2)?,
                    inspection_cron: row.get(3)?,
                    inspection_timezone: row.get(4)?,
                    first_inspection_at: row.get(5)?,
                    inspection_obligation_id: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::from)
}

fn registration_for_inspection_obligation(
    connection: &Connection,
    obligation_id: &str,
) -> Result<String, StoreError> {
    connection
        .query_row(
            "SELECT repository FROM gardener_repositories
             WHERE inspection_obligation_id = ?1",
            [obligation_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| {
            StoreError::Conflict(format!(
                "obligation {obligation_id:?} is not a registered inspection obligation"
            ))
        })
}

fn require_gardener_kind(
    connection: &Connection,
    obligation_id: &str,
    expected: &str,
) -> Result<(), StoreError> {
    let kind: Option<String> = connection
        .query_row(
            "SELECT kind FROM gardener_obligation_bindings WHERE obligation_id = ?1",
            [obligation_id],
            |row| row.get(0),
        )
        .optional()?;
    if kind.as_deref() != Some(expected) {
        return Err(StoreError::Conflict(format!(
            "obligation {obligation_id:?} is not gardener kind {expected:?}"
        )));
    }
    Ok(())
}

fn validate_inspection(inspection: &NewGardenerInspection) -> Result<(), StoreError> {
    if inspection.id.trim().is_empty() {
        return Err(StoreError::Invalid(
            "inspection id must not be empty".to_owned(),
        ));
    }
    validate_hex_identity("source commit", &inspection.source_commit, &[40, 64])?;
    validate_hex_identity("inspection prompt digest", &inspection.prompt_digest, &[64])?;
    if inspection.worktree_path.trim().is_empty()
        || !Path::new(&inspection.worktree_path).is_absolute()
    {
        return Err(StoreError::Invalid(
            "inspection worktree path must be absolute".to_owned(),
        ));
    }
    Ok(())
}

fn validate_hex_identity(name: &str, value: &str, lengths: &[usize]) -> Result<(), StoreError> {
    if !lengths.contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(StoreError::Invalid(format!(
            "{name} must be a {}-character hexadecimal identity",
            lengths
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join("- or ")
        )));
    }
    Ok(())
}

fn validate_inspection_result(result: &InspectionResult) -> Result<(), StoreError> {
    if result.summary.trim().is_empty() {
        return Err(StoreError::Invalid(
            "inspection result summary must not be empty".to_owned(),
        ));
    }
    Ok(())
}

fn gardener_inspection(
    connection: &Connection,
    id: &str,
) -> Result<Option<GardenerInspection>, StoreError> {
    connection
        .query_row(
            "SELECT id, repository, obligation_id, occurrence, lease_generation,
                    source_commit, worktree_path, prompt_digest, codex_thread_id,
                    codex_turn_id, result_json, started_at, completed_at
             FROM gardener_inspections WHERE id = ?1",
            [id],
            |row| {
                Ok(GardenerInspection {
                    id: row.get(0)?,
                    repository: row.get(1)?,
                    obligation_id: row.get(2)?,
                    occurrence: row.get(3)?,
                    lease_generation: row.get(4)?,
                    source_commit: row.get(5)?,
                    worktree_path: row.get(6)?,
                    prompt_digest: row.get(7)?,
                    codex_thread_id: row.get(8)?,
                    codex_turn_id: row.get(9)?,
                    result_json: row.get(10)?,
                    started_at: row.get(11)?,
                    completed_at: row.get(12)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::from)
}

fn require_current_inspection(
    transaction: &Transaction<'_>,
    claim: &Claim,
    inspection_id: &str,
    now: i64,
) -> Result<GardenerInspection, StoreError> {
    let obligation = require_obligation(transaction, &claim.obligation_id)?;
    verify_claim(&obligation, claim, now)?;
    require_gardener_kind(transaction, &claim.obligation_id, "inspection")?;
    let inspection = gardener_inspection(transaction, inspection_id)?
        .ok_or_else(|| StoreError::NotFound(inspection_id.to_owned()))?;
    if inspection.obligation_id != claim.obligation_id
        || inspection.occurrence != claim.occurrence
        || inspection.lease_generation != claim.lease_generation
    {
        return Err(StoreError::Fenced);
    }
    Ok(inspection)
}

fn validate_new_implementation_run(new: &NewGardenerImplementationRun) -> Result<(), StoreError> {
    validate_nonempty("implementation run id", &new.id)?;
    validate_absolute_path(
        "implementation worktree path",
        &new.implementation_worktree_path,
    )?;
    let suffix = new.branch.strip_prefix("codex/gardener-").ok_or_else(|| {
        StoreError::Invalid("implementation branch must start with \"codex/gardener-\"".to_owned())
    })?;
    if suffix.is_empty()
        || suffix.starts_with('/')
        || suffix.ends_with('/')
        || suffix.contains("..")
        || suffix.chars().any(char::is_whitespace)
    {
        return Err(StoreError::Invalid(
            "implementation branch must be a dedicated codex/gardener-* name".to_owned(),
        ));
    }
    Ok(())
}

fn validate_absolute_path(name: &str, value: &str) -> Result<(), StoreError> {
    if value.trim().is_empty() || !Path::new(value).is_absolute() {
        return Err(StoreError::Invalid(format!("{name} must be absolute")));
    }
    Ok(())
}

fn validate_nonempty(name: &str, value: &str) -> Result<(), StoreError> {
    if value.trim().is_empty() {
        return Err(StoreError::Invalid(format!("{name} must not be empty")));
    }
    Ok(())
}

fn validate_structured_message(message: &str) -> Result<(), StoreError> {
    let value: serde_json::Value = serde_json::from_str(message).map_err(|error| {
        StoreError::Invalid(format!(
            "implementation final message must be valid JSON: {error}"
        ))
    })?;
    if !value.is_object() {
        return Err(StoreError::Invalid(
            "implementation final message must be a JSON object".to_owned(),
        ));
    }
    Ok(())
}

fn gardener_implementation_run(
    connection: &Connection,
    id: &str,
) -> Result<Option<GardenerImplementationRun>, StoreError> {
    connection
        .query_row(
            "SELECT * FROM gardener_implementation_runs WHERE id = ?1",
            [id],
            gardener_implementation_run_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

fn gardener_implementation_run_for_lease(
    connection: &Connection,
    obligation_id: &str,
    lease_generation: u64,
) -> Result<Option<GardenerImplementationRun>, StoreError> {
    connection
        .query_row(
            "SELECT * FROM gardener_implementation_runs
             WHERE obligation_id = ?1 AND lease_generation = ?2",
            params![obligation_id, lease_generation],
            gardener_implementation_run_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

fn query_gardener_implementation_runs<P: rusqlite::Params>(
    connection: &Connection,
    query: &str,
    parameters: P,
) -> Result<Vec<GardenerImplementationRun>, StoreError> {
    let mut statement = connection.prepare(query)?;
    let rows = statement.query_map(parameters, gardener_implementation_run_from_row)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn gardener_implementation_run_from_row(
    row: &Row<'_>,
) -> rusqlite::Result<GardenerImplementationRun> {
    let verdict_index = row.as_ref().column_index("verification_verdict")?;
    let verdict = row
        .get::<_, Option<String>>(verdict_index)?
        .map(|value| parse_value(value, verdict_index))
        .transpose()?;
    Ok(GardenerImplementationRun {
        id: row.get("id")?,
        repository: row.get("repository")?,
        proposal_fingerprint: row.get("proposal_fingerprint")?,
        obligation_id: row.get("obligation_id")?,
        occurrence: row.get("occurrence")?,
        attempt_number: row.get("attempt_number")?,
        lease_generation: row.get("lease_generation")?,
        lease_token: row.get("lease_token")?,
        source_commit: row.get("source_commit")?,
        implementation_worktree_path: row.get("implementation_worktree_path")?,
        branch: row.get("branch")?,
        phase: parse_column(row, "phase")?,
        implementation_thread_id: row.get("implementation_thread_id")?,
        implementation_turn_id: row.get("implementation_turn_id")?,
        implementation_final_message_json: row.get("implementation_final_message_json")?,
        git_commit: row.get("git_commit")?,
        pushed_head: row.get("pushed_head")?,
        pull_request_number: row.get("pull_request_number")?,
        pull_request_url: row.get("pull_request_url")?,
        pull_request_head: row.get("pull_request_head")?,
        verification_worktree_path: row.get("verification_worktree_path")?,
        verification_head: row.get("verification_head")?,
        verification_thread_id: row.get("verification_thread_id")?,
        verification_turn_id: row.get("verification_turn_id")?,
        verification_verdict: verdict,
        verification_reported_head: row.get("verification_reported_head")?,
        verification_summary: row.get("verification_summary")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        implementation_thread_recorded_at: row.get("implementation_thread_recorded_at")?,
        implementation_turn_recorded_at: row.get("implementation_turn_recorded_at")?,
        implementation_finished_at: row.get("implementation_finished_at")?,
        git_commit_recorded_at: row.get("git_commit_recorded_at")?,
        push_observed_at: row.get("push_observed_at")?,
        pull_request_recorded_at: row.get("pull_request_recorded_at")?,
        verification_started_at: row.get("verification_started_at")?,
        verification_thread_recorded_at: row.get("verification_thread_recorded_at")?,
        verification_turn_recorded_at: row.get("verification_turn_recorded_at")?,
        verification_finished_at: row.get("verification_finished_at")?,
    })
}

fn require_current_gardener_run(
    transaction: &Transaction<'_>,
    claim: &Claim,
    run_id: &str,
    now: i64,
) -> Result<GardenerImplementationRun, StoreError> {
    let obligation = require_obligation(transaction, &claim.obligation_id)?;
    verify_claim(&obligation, claim, now)?;
    require_gardener_kind(transaction, &claim.obligation_id, "implementation")?;
    let run = gardener_implementation_run(transaction, run_id)?
        .ok_or_else(|| StoreError::NotFound(run_id.to_owned()))?;
    if run.obligation_id != claim.obligation_id
        || run.occurrence != claim.occurrence
        || run.attempt_number != claim.attempt_number
        || run.lease_generation != claim.lease_generation
        || run.lease_token != claim.lease_token
    {
        return Err(StoreError::Fenced);
    }
    Ok(run)
}

fn require_run_phase(
    run: &GardenerImplementationRun,
    expected: GardenerRunPhase,
) -> Result<(), StoreError> {
    if run.phase != expected {
        return Err(StoreError::Conflict(format!(
            "run {:?} is in phase {}, expected {}",
            run.id, run.phase, expected
        )));
    }
    Ok(())
}

fn idempotent_or_conflict(
    run_id: &str,
    identity: &str,
    existing: Option<&str>,
    proposed: &str,
) -> Result<bool, StoreError> {
    match existing {
        None => Ok(false),
        Some(existing) if existing == proposed => Ok(true),
        Some(_) => Err(StoreError::Conflict(format!(
            "run {run_id:?} already has a different {identity}"
        ))),
    }
}

fn proposal(connection: &Connection, fingerprint: &str) -> Result<Option<Proposal>, StoreError> {
    connection
        .query_row(
            "SELECT p.fingerprint, p.repository, p.prompt, p.implementation_obligation_id,
                    o.state,
                    (SELECT decision FROM approvals a
                     WHERE a.obligation_id = o.id AND a.occurrence = o.occurrence
                     ORDER BY a.id DESC LIMIT 1),
                    (SELECT COUNT(*) FROM gardener_proposal_observations po
                     WHERE po.proposal_fingerprint = p.fingerprint),
                    p.created_at
             FROM gardener_proposals p
             JOIN obligations o ON o.id = p.implementation_obligation_id
             WHERE p.fingerprint = ?1",
            [fingerprint],
            |row| {
                let decision: Option<String> = row.get(5)?;
                Ok(Proposal {
                    fingerprint: row.get(0)?,
                    repository: row.get(1)?,
                    prompt: row.get(2)?,
                    implementation_obligation_id: row.get(3)?,
                    obligation_state: parse_index(row, 4)?,
                    approval_decision: decision.map(|value| parse_value(value, 5)).transpose()?,
                    observation_count: row.get(6)?,
                    created_at: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::from)
}

#[allow(clippy::too_many_arguments)]
fn append_gardener_event(
    transaction: &Transaction<'_>,
    repository: &str,
    inspection_id: Option<&str>,
    proposal_fingerprint: Option<&str>,
    event_type: &str,
    now: i64,
    details: serde_json::Value,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO gardener_events(
            repository, inspection_id, proposal_fingerprint, event_type, occurred_at, details_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            repository,
            inspection_id,
            proposal_fingerprint,
            event_type,
            now,
            details.to_string()
        ],
    )?;
    Ok(())
}

fn append_gardener_run_event(
    transaction: &Transaction<'_>,
    run_id: &str,
    event_type: &str,
    now: i64,
    details: serde_json::Value,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO gardener_run_events(run_id, event_type, occurred_at, details_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![run_id, event_type, now, details.to_string()],
    )?;
    Ok(())
}

enum Transition<'a> {
    Create {
        new: NewObligation,
        now: i64,
    },
    Approval {
        id: &'a str,
        decision: ApprovalDecision,
        actor: &'a str,
        note: Option<&'a str>,
        now: i64,
    },
    Claim {
        id: &'a str,
        now: i64,
        lease_seconds: i64,
    },
    Renew {
        claim: &'a Claim,
        now: i64,
        lease_seconds: i64,
    },
    Complete {
        claim: &'a Claim,
        completion: Completion,
        now: i64,
    },
    LeaseExpired {
        id: &'a str,
        now: i64,
    },
    RetryAttention {
        id: &'a str,
        now: i64,
    },
    Cancel {
        id: &'a str,
        now: i64,
    },
}

#[derive(Default)]
struct TransitionResult {
    claim: Option<Claim>,
    lease_expires_at: Option<i64>,
}

/// The sole semantic owner for projection mutations and their audit events.
fn apply_transition(
    transaction: &Transaction<'_>,
    transition: Transition<'_>,
) -> Result<TransitionResult, StoreError> {
    match transition {
        Transition::Create { new, now } => {
            let state = if new.approval_required {
                ObligationState::AwaitingApproval
            } else {
                ObligationState::Pending
            };
            let next_wake = (!new.approval_required).then_some(new.scheduled_at);
            let (cron, timezone) = new
                .recurrence
                .as_ref()
                .map(|value| (Some(value.expression()), Some(value.timezone())))
                .unwrap_or((None, None));
            transaction.execute(
                "INSERT INTO obligations(
                    id, description, state, occurrence, scheduled_at, next_wake_at,
                    recurrence_cron, recurrence_timezone, approval_required,
                    attempts_made, max_attempts, retry_base_seconds, retry_max_seconds,
                    lease_generation, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?10, ?11, 0, ?12, ?12)",
                params![
                    new.id,
                    new.description,
                    state.to_string(),
                    new.scheduled_at,
                    next_wake,
                    cron,
                    timezone,
                    new.approval_required,
                    new.retry.max_attempts,
                    new.retry.base_delay_seconds,
                    new.retry.max_delay_seconds,
                    now
                ],
            )?;
            append_event(
                transaction,
                &new.id,
                1,
                "created",
                now,
                None,
                state,
                json!({"scheduled_at": new.scheduled_at}),
            )?;
        }
        Transition::Approval {
            id,
            decision,
            actor,
            note,
            now,
        } => {
            let obligation = require_obligation(transaction, id)?;
            if obligation.state != ObligationState::AwaitingApproval {
                return Err(StoreError::Conflict(format!(
                    "approval requires awaiting_approval, found {}",
                    obligation.state
                )));
            }
            transaction.execute(
                "INSERT INTO approvals(obligation_id, occurrence, decision, actor, note, decided_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, obligation.occurrence, decision.to_string(), actor, note, now],
            )?;
            let next_state = match decision {
                ApprovalDecision::Approved => ObligationState::Pending,
                ApprovalDecision::Rejected => ObligationState::Attention,
            };
            let next_wake =
                (decision == ApprovalDecision::Approved).then_some(obligation.scheduled_at);
            transaction.execute(
                "UPDATE obligations SET state = ?2, next_wake_at = ?3, updated_at = ?4
                 WHERE id = ?1",
                params![id, next_state.to_string(), next_wake, now],
            )?;
            append_event(
                transaction,
                id,
                obligation.occurrence,
                &decision.to_string(),
                now,
                Some(obligation.state),
                next_state,
                json!({"actor": actor, "note": note}),
            )?;
        }
        Transition::Claim {
            id,
            now,
            lease_seconds,
        } => {
            let obligation = require_obligation(transaction, id)?;
            if !matches!(
                obligation.state,
                ObligationState::Pending | ObligationState::RetryScheduled
            ) || obligation.next_wake_at.is_none_or(|wake| wake > now)
            {
                return Err(StoreError::Conflict("obligation is not due".to_owned()));
            }
            if obligation.approval_required
                && !latest_approval_is_approved(transaction, &obligation)?
            {
                return Err(StoreError::Conflict(
                    "current occurrence lacks approval".to_owned(),
                ));
            }

            let attempt_number = obligation.attempts_made + 1;
            let lease_generation = obligation.lease_generation + 1;
            let lease_token = Uuid::new_v4().to_string();
            let lease_expires_at = now.saturating_add(lease_seconds);
            transaction.execute(
                "UPDATE obligations SET state = 'running', next_wake_at = NULL,
                    attempts_made = ?2, lease_token = ?3, lease_generation = ?4,
                    lease_expires_at = ?5, updated_at = ?6
                 WHERE id = ?1",
                params![
                    id,
                    attempt_number,
                    lease_token,
                    lease_generation,
                    lease_expires_at,
                    now
                ],
            )?;
            transaction.execute(
                "INSERT INTO attempts(
                    obligation_id, occurrence, attempt_number, lease_generation,
                    lease_token, claimed_at, outcome
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running')",
                params![
                    id,
                    obligation.occurrence,
                    attempt_number,
                    lease_generation,
                    lease_token,
                    now
                ],
            )?;
            append_event(
                transaction,
                id,
                obligation.occurrence,
                "claimed",
                now,
                Some(obligation.state),
                ObligationState::Running,
                json!({
                    "attempt_number": attempt_number,
                    "lease_generation": lease_generation,
                    "lease_expires_at": lease_expires_at
                }),
            )?;
            return Ok(TransitionResult {
                claim: Some(Claim {
                    obligation_id: id.to_owned(),
                    occurrence: obligation.occurrence,
                    attempt_number,
                    lease_token,
                    lease_generation,
                    lease_expires_at,
                    description: obligation.description,
                }),
                lease_expires_at: None,
            });
        }
        Transition::Renew {
            claim,
            now,
            lease_seconds,
        } => {
            let obligation = require_obligation(transaction, &claim.obligation_id)?;
            verify_claim(&obligation, claim, now)?;
            let lease_expires_at = obligation
                .lease_expires_at
                .unwrap_or(now)
                .max(now)
                .saturating_add(lease_seconds);
            transaction.execute(
                "UPDATE obligations SET lease_expires_at = ?2, updated_at = ?3 WHERE id = ?1",
                params![claim.obligation_id, lease_expires_at, now],
            )?;
            append_event(
                transaction,
                &claim.obligation_id,
                claim.occurrence,
                "lease_renewed",
                now,
                Some(ObligationState::Running),
                ObligationState::Running,
                json!({
                    "lease_generation": claim.lease_generation,
                    "lease_expires_at": lease_expires_at
                }),
            )?;
            return Ok(TransitionResult {
                claim: None,
                lease_expires_at: Some(lease_expires_at),
            });
        }
        Transition::Complete {
            claim,
            completion,
            now,
        } => {
            let obligation = require_obligation(transaction, &claim.obligation_id)?;
            verify_claim(&obligation, claim, now)?;
            match completion {
                Completion::Succeeded { evidence } => {
                    finish_attempt(
                        transaction,
                        claim,
                        now,
                        AttemptOutcome::Succeeded,
                        None,
                        None,
                        evidence.as_deref(),
                    )?;
                    if let (Some(expression), Some(timezone)) = (
                        obligation.recurrence_cron.as_deref(),
                        obligation.recurrence_timezone.as_deref(),
                    ) {
                        let next_occurrence =
                            Recurrence::new(expression, timezone).and_then(|recurrence| {
                                recurrence.next_after(now.max(obligation.scheduled_at))
                            });
                        match next_occurrence {
                            Ok(next) => {
                                let next_state = if obligation.approval_required {
                                    ObligationState::AwaitingApproval
                                } else {
                                    ObligationState::Pending
                                };
                                let next_wake = (!obligation.approval_required).then_some(next);
                                transaction.execute(
                                    "UPDATE obligations SET state = ?2, occurrence = occurrence + 1,
                                        scheduled_at = ?3, next_wake_at = ?4, attempts_made = 0,
                                        lease_token = NULL, lease_expires_at = NULL,
                                        last_error = NULL, last_evidence = ?5, updated_at = ?6
                                     WHERE id = ?1",
                                    params![
                                        claim.obligation_id,
                                        next_state.to_string(),
                                        next,
                                        next_wake,
                                        evidence,
                                        now
                                    ],
                                )?;
                                append_event(
                                    transaction,
                                    &claim.obligation_id,
                                    obligation.occurrence + 1,
                                    "occurrence_scheduled",
                                    now,
                                    Some(ObligationState::Running),
                                    next_state,
                                    json!({
                                        "completed_occurrence": obligation.occurrence,
                                        "scheduled_at": next,
                                        "evidence": evidence
                                    }),
                                )?;
                            }
                            Err(RecurrenceError::Exhausted) => {
                                complete_terminal_success(
                                    transaction,
                                    &obligation,
                                    now,
                                    evidence.as_deref(),
                                    Some("recurrence_exhausted"),
                                )?;
                            }
                            Err(error) => {
                                preserve_success_as_attention(
                                    transaction,
                                    &obligation,
                                    now,
                                    evidence.as_deref(),
                                    &error,
                                )?;
                            }
                        }
                    } else {
                        complete_terminal_success(
                            transaction,
                            &obligation,
                            now,
                            evidence.as_deref(),
                            None,
                        )?;
                    }
                }
                Completion::Failed {
                    retryable,
                    error,
                    evidence,
                } => {
                    finish_attempt(
                        transaction,
                        claim,
                        now,
                        AttemptOutcome::Failed,
                        Some(retryable),
                        Some(&error),
                        evidence.as_deref(),
                    )?;
                    schedule_failure(
                        transaction,
                        &obligation,
                        now,
                        retryable,
                        &error,
                        evidence.as_deref(),
                        "failed",
                    )?;
                }
            }
        }
        Transition::LeaseExpired { id, now } => {
            let obligation = require_obligation(transaction, id)?;
            if obligation.state != ObligationState::Running
                || obligation
                    .lease_expires_at
                    .is_none_or(|expiry| expiry > now)
            {
                return Err(StoreError::Conflict("lease is not expired".to_owned()));
            }
            let changed = transaction.execute(
                "UPDATE attempts SET completed_at = ?3, outcome = 'lease_expired', retryable = 1,
                    error = 'lease expired before completion'
                 WHERE obligation_id = ?1 AND lease_generation = ?2 AND completed_at IS NULL",
                params![id, obligation.lease_generation, now],
            )?;
            if changed != 1 {
                return Err(StoreError::Fenced);
            }
            schedule_failure(
                transaction,
                &obligation,
                now,
                true,
                "lease expired before completion",
                None,
                "lease_expired",
            )?;
        }
        Transition::RetryAttention { id, now } => {
            let obligation = require_obligation(transaction, id)?;
            if obligation.state != ObligationState::Attention {
                return Err(StoreError::Conflict(format!(
                    "retry requires attention, found {}",
                    obligation.state
                )));
            }
            let next_state = if obligation.approval_required {
                ObligationState::AwaitingApproval
            } else {
                ObligationState::Pending
            };
            let next_wake = (!obligation.approval_required).then_some(now);
            transaction.execute(
                "UPDATE obligations SET state = ?2, next_wake_at = ?3,
                    last_error = NULL, updated_at = ?4 WHERE id = ?1",
                params![id, next_state.to_string(), next_wake, now],
            )?;
            append_event(
                transaction,
                id,
                obligation.occurrence,
                "attention_retried",
                now,
                Some(obligation.state),
                next_state,
                json!({}),
            )?;
        }
        Transition::Cancel { id, now } => {
            let obligation = require_obligation(transaction, id)?;
            if obligation.state.is_terminal() {
                return Err(StoreError::Conflict(format!(
                    "cannot cancel terminal state {}",
                    obligation.state
                )));
            }
            if obligation.state == ObligationState::Running {
                return Err(StoreError::Conflict(
                    "cannot cancel while a runner owns an active claim".to_owned(),
                ));
            }
            transaction.execute(
                "UPDATE obligations SET state = 'cancelled', next_wake_at = NULL,
                    lease_token = NULL, lease_expires_at = NULL, updated_at = ?2 WHERE id = ?1",
                params![id, now],
            )?;
            append_event(
                transaction,
                id,
                obligation.occurrence,
                "cancelled",
                now,
                Some(obligation.state),
                ObligationState::Cancelled,
                json!({}),
            )?;
        }
    }
    Ok(TransitionResult::default())
}

fn complete_terminal_success(
    transaction: &Transaction<'_>,
    obligation: &Obligation,
    now: i64,
    evidence: Option<&str>,
    reason: Option<&str>,
) -> Result<(), StoreError> {
    let details = match reason {
        Some(reason) => json!({"evidence": evidence, "reason": reason}),
        None => json!({"evidence": evidence}),
    };
    transaction.execute(
        "UPDATE obligations SET state = 'completed', lease_token = NULL,
            lease_expires_at = NULL, last_error = NULL,
            last_evidence = ?2, updated_at = ?3 WHERE id = ?1",
        params![obligation.id, evidence, now],
    )?;
    append_event(
        transaction,
        &obligation.id,
        obligation.occurrence,
        "completed",
        now,
        Some(ObligationState::Running),
        ObligationState::Completed,
        details,
    )?;
    Ok(())
}

fn preserve_success_as_attention(
    transaction: &Transaction<'_>,
    obligation: &Obligation,
    now: i64,
    evidence: Option<&str>,
    error: &RecurrenceError,
) -> Result<(), StoreError> {
    let error = format!("could not calculate next recurrence after successful attempt: {error}");
    transaction.execute(
        "UPDATE obligations SET state = 'attention', next_wake_at = NULL,
            lease_token = NULL, lease_expires_at = NULL, last_error = ?2,
            last_evidence = ?3, updated_at = ?4 WHERE id = ?1",
        params![obligation.id, error, evidence, now],
    )?;
    append_event(
        transaction,
        &obligation.id,
        obligation.occurrence,
        "recurrence_evaluation_attention",
        now,
        Some(ObligationState::Running),
        ObligationState::Attention,
        json!({"error": error, "evidence": evidence}),
    )?;
    Ok(())
}

fn recover_expired_in_transaction(
    transaction: &Transaction<'_>,
    now: i64,
) -> Result<usize, StoreError> {
    let ids = {
        let mut statement = transaction.prepare(
            "SELECT id FROM obligations
             WHERE state = 'running' AND lease_expires_at <= ?1
             ORDER BY lease_expires_at, id",
        )?;
        statement
            .query_map([now], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    for id in &ids {
        apply_transition(transaction, Transition::LeaseExpired { id, now })?;
    }
    Ok(ids.len())
}

fn schedule_failure(
    transaction: &Transaction<'_>,
    obligation: &Obligation,
    now: i64,
    retryable: bool,
    error: &str,
    evidence: Option<&str>,
    event_prefix: &str,
) -> Result<(), StoreError> {
    let can_retry = retryable && obligation.attempts_made < obligation.max_attempts;
    let (next_state, next_wake, event_type) = if can_retry {
        let delay = retry_delay(
            obligation.retry_base_seconds,
            obligation.retry_max_seconds,
            obligation.attempts_made,
        );
        (
            ObligationState::RetryScheduled,
            Some(now.saturating_add(delay)),
            format!("{event_prefix}_retry_scheduled"),
        )
    } else {
        (
            ObligationState::Attention,
            None,
            format!("{event_prefix}_attention"),
        )
    };
    transaction.execute(
        "UPDATE obligations SET state = ?2, next_wake_at = ?3,
            lease_token = NULL, lease_expires_at = NULL, last_error = ?4,
            last_evidence = ?5, updated_at = ?6 WHERE id = ?1",
        params![
            obligation.id,
            next_state.to_string(),
            next_wake,
            error,
            evidence,
            now
        ],
    )?;
    append_event(
        transaction,
        &obligation.id,
        obligation.occurrence,
        &event_type,
        now,
        Some(ObligationState::Running),
        next_state,
        json!({
            "attempt_number": obligation.attempts_made,
            "error": error,
            "retry_at": next_wake,
            "evidence": evidence
        }),
    )?;
    Ok(())
}

fn finish_attempt(
    transaction: &Transaction<'_>,
    claim: &Claim,
    now: i64,
    outcome: AttemptOutcome,
    retryable: Option<bool>,
    error: Option<&str>,
    evidence: Option<&str>,
) -> Result<(), StoreError> {
    let changed = transaction.execute(
        "UPDATE attempts SET completed_at = ?3, outcome = ?4, retryable = ?5,
            error = ?6, evidence = ?7
         WHERE obligation_id = ?1 AND lease_generation = ?2 AND completed_at IS NULL",
        params![
            claim.obligation_id,
            claim.lease_generation,
            now,
            outcome.to_string(),
            retryable,
            error,
            evidence
        ],
    )?;
    if changed != 1 {
        return Err(StoreError::Fenced);
    }
    Ok(())
}

fn verify_claim(obligation: &Obligation, claim: &Claim, now: i64) -> Result<(), StoreError> {
    if obligation.state != ObligationState::Running
        || obligation.occurrence != claim.occurrence
        || obligation.lease_generation != claim.lease_generation
        || obligation.lease_token.as_deref() != Some(&claim.lease_token)
        || obligation
            .lease_expires_at
            .is_none_or(|expiry| expiry <= now)
    {
        return Err(StoreError::Fenced);
    }
    Ok(())
}

fn latest_approval_is_approved(
    transaction: &Transaction<'_>,
    obligation: &Obligation,
) -> Result<bool, StoreError> {
    let decision: Option<String> = transaction
        .query_row(
            "SELECT decision FROM approvals
             WHERE obligation_id = ?1 AND occurrence = ?2
             ORDER BY id DESC LIMIT 1",
            params![obligation.id, obligation.occurrence],
            |row| row.get(0),
        )
        .optional()?;
    Ok(decision.as_deref() == Some("approved"))
}

fn retry_delay(base: i64, maximum: i64, attempt_number: u32) -> i64 {
    let exponent = attempt_number.saturating_sub(1).min(62);
    base.saturating_mul(1_i64 << exponent).min(maximum)
}

#[allow(clippy::too_many_arguments)]
fn append_event(
    transaction: &Transaction<'_>,
    id: &str,
    occurrence: u32,
    event_type: &str,
    now: i64,
    from: Option<ObligationState>,
    to: ObligationState,
    details: serde_json::Value,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO audit_events(
            obligation_id, occurrence, event_type, occurred_at, from_state, to_state, details_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            occurrence,
            event_type,
            now,
            from.map(|value| value.to_string()),
            to.to_string(),
            details.to_string()
        ],
    )?;
    Ok(())
}

fn validate_new(new: &NewObligation) -> Result<(), StoreError> {
    if new.id.trim().is_empty() {
        return Err(StoreError::Invalid("id must not be empty".to_owned()));
    }
    if new.description.trim().is_empty() {
        return Err(StoreError::Invalid(
            "description must not be empty".to_owned(),
        ));
    }
    if new.retry.max_attempts == 0 {
        return Err(StoreError::Invalid(
            "max attempts must be positive".to_owned(),
        ));
    }
    if new.retry.base_delay_seconds <= 0
        || new.retry.max_delay_seconds < new.retry.base_delay_seconds
    {
        return Err(StoreError::Invalid(
            "retry delays must be positive and bounded above the base".to_owned(),
        ));
    }
    Ok(())
}

fn require_obligation(transaction: &Transaction<'_>, id: &str) -> Result<Obligation, StoreError> {
    transaction
        .query_row(
            "SELECT * FROM obligations WHERE id = ?1",
            [id],
            obligation_from_row,
        )
        .optional()?
        .ok_or_else(|| StoreError::NotFound(id.to_owned()))
}

fn obligation_from_row(row: &Row<'_>) -> rusqlite::Result<Obligation> {
    Ok(Obligation {
        id: row.get("id")?,
        description: row.get("description")?,
        state: parse_column(row, "state")?,
        occurrence: row.get("occurrence")?,
        scheduled_at: row.get("scheduled_at")?,
        next_wake_at: row.get("next_wake_at")?,
        recurrence_cron: row.get("recurrence_cron")?,
        recurrence_timezone: row.get("recurrence_timezone")?,
        approval_required: row.get("approval_required")?,
        attempts_made: row.get("attempts_made")?,
        max_attempts: row.get("max_attempts")?,
        retry_base_seconds: row.get("retry_base_seconds")?,
        retry_max_seconds: row.get("retry_max_seconds")?,
        lease_token: row.get("lease_token")?,
        lease_generation: row.get("lease_generation")?,
        lease_expires_at: row.get("lease_expires_at")?,
        last_error: row.get("last_error")?,
        last_evidence: row.get("last_evidence")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn attempt_from_row(row: &Row<'_>) -> rusqlite::Result<Attempt> {
    Ok(Attempt {
        id: row.get(0)?,
        obligation_id: row.get(1)?,
        occurrence: row.get(2)?,
        attempt_number: row.get(3)?,
        lease_generation: row.get(4)?,
        lease_token: row.get(5)?,
        claimed_at: row.get(6)?,
        completed_at: row.get(7)?,
        outcome: parse_index(row, 8)?,
        retryable: row.get(9)?,
        error: row.get(10)?,
        evidence: row.get(11)?,
    })
}

fn audit_from_row(row: &Row<'_>) -> rusqlite::Result<AuditEvent> {
    let from: Option<String> = row.get(5)?;
    Ok(AuditEvent {
        sequence: row.get(0)?,
        obligation_id: row.get(1)?,
        occurrence: row.get(2)?,
        event_type: row.get(3)?,
        occurred_at: row.get(4)?,
        from_state: from.map(|value| parse_value(value, 5)).transpose()?,
        to_state: parse_index(row, 6)?,
        details_json: row.get(7)?,
    })
}

fn parse_column<T>(row: &Row<'_>, name: &str) -> rusqlite::Result<T>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    let index = row.as_ref().column_index(name)?;
    let value: String = row.get(index)?;
    parse_value(value, index)
}

fn parse_index<T>(row: &Row<'_>, index: usize) -> rusqlite::Result<T>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    let value: String = row.get(index)?;
    parse_value(value, index)
}

fn parse_value<T>(value: String, index: usize) -> rusqlite::Result<T>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    value.parse::<T>().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                error.to_string(),
            )),
        )
    })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::{FakeOutcome, FakeRunner, RetryPolicy, run_one};

    fn one_off(id: &str, scheduled_at: i64) -> NewObligation {
        NewObligation {
            id: id.to_owned(),
            description: format!("work for {id}"),
            scheduled_at,
            recurrence: None,
            approval_required: false,
            retry: RetryPolicy {
                max_attempts: 2,
                base_delay_seconds: 10,
                max_delay_seconds: 60,
            },
        }
    }

    #[test]
    fn migrations_are_reentrant_and_connection_is_hardened() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("state.sqlite3");
        let store = Store::open(&path).unwrap();
        assert_eq!(store.pragma_string("journal_mode"), "wal");
        assert_eq!(store.pragma_i64("synchronous"), 2);
        assert_eq!(store.pragma_i64("foreign_keys"), 1);
        assert_eq!(store.pragma_i64("busy_timeout"), 5_000);
        let migrations: Vec<(i64, String)> = {
            let mut statement = store
                .connection
                .prepare("SELECT version, name FROM schema_migrations ORDER BY version")
                .unwrap();
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(
            migrations,
            [
                (1, "0001_obligation_kernel.sql".to_owned()),
                (2, "0002_append_only_guards.sql".to_owned()),
                (3, "0003_coding_gardener_state.sql".to_owned()),
                (4, "0004_coding_gardener_runs.sql".to_owned())
            ]
        );
        drop(store);
        assert!(Store::open(path).is_ok());
    }

    #[test]
    fn fake_runner_persists_success_and_audit_history() {
        let mut store = Store::open_in_memory().unwrap();
        let clock = ManualClock::new(1_000);
        store.create(one_off("a", 1_000), clock.now()).unwrap();
        let claim = store.claim_due(clock.now(), 30, 1).unwrap().remove(0);
        let mut runner = FakeRunner::new([FakeOutcome::Succeed {
            evidence: Some("deterministic evidence".to_owned()),
        }]);
        run_one(&mut store, &mut runner, &clock, &claim).unwrap();

        assert_eq!(
            store.get("a").unwrap().unwrap().state,
            ObligationState::Completed
        );
        let attempts = store.attempts("a").unwrap();
        assert_eq!(attempts[0].outcome, AttemptOutcome::Succeeded);
        assert_eq!(
            attempts[0].evidence.as_deref(),
            Some("deterministic evidence")
        );
        assert_eq!(runner.invocations(), &[claim]);
        assert_eq!(
            store
                .events("a")
                .unwrap()
                .into_iter()
                .map(|event| event.event_type)
                .collect::<Vec<_>>(),
            ["created", "claimed", "completed"]
        );
    }

    #[test]
    fn successful_final_finite_recurrence_completes_atomically() {
        let mut store = Store::open_in_memory().unwrap();
        let mut obligation = one_off("finite", 100);
        obligation.recurrence = Some(Recurrence::new("0 0 0 1 1 * 1970", "UTC").unwrap());
        store.create(obligation, 100).unwrap();
        let claim = store.claim_due(100, 30, 1).unwrap().remove(0);

        store
            .complete(
                &claim,
                Completion::Succeeded {
                    evidence: Some("final finite occurrence ran".to_owned()),
                },
                101,
            )
            .unwrap();

        let completed = store.get("finite").unwrap().unwrap();
        assert_eq!(completed.state, ObligationState::Completed);
        assert_eq!(completed.next_wake_at, None);
        assert_eq!(
            completed.last_evidence.as_deref(),
            Some("final finite occurrence ran")
        );
        assert_eq!(
            store.attempts("finite").unwrap()[0].outcome,
            AttemptOutcome::Succeeded
        );
        let completed_event = store.events("finite").unwrap().pop().unwrap();
        assert_eq!(completed_event.event_type, "completed");
        assert_eq!(completed_event.to_state, ObligationState::Completed);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&completed_event.details_json).unwrap()["reason"],
            "recurrence_exhausted"
        );
    }

    #[test]
    fn successful_attempt_survives_unexpected_recurrence_evaluation_error() {
        let mut store = Store::open_in_memory().unwrap();
        let mut obligation = one_off("invalid-next", 100);
        obligation.recurrence = Some(Recurrence::new("* * * * *", "UTC").unwrap());
        store.create(obligation, 100).unwrap();
        let claim = store.claim_due(100, i64::MAX, 1).unwrap().remove(0);

        store
            .complete(
                &claim,
                Completion::Succeeded {
                    evidence: Some("work already succeeded".to_owned()),
                },
                i64::MAX - 1,
            )
            .unwrap();

        let attention = store.get("invalid-next").unwrap().unwrap();
        assert_eq!(attention.state, ObligationState::Attention);
        assert_eq!(attention.next_wake_at, None);
        assert!(
            attention
                .last_error
                .as_deref()
                .unwrap()
                .contains("timestamp is outside the supported range")
        );
        assert_eq!(
            attention.last_evidence.as_deref(),
            Some("work already succeeded")
        );
        assert_eq!(
            store.attempts("invalid-next").unwrap()[0].outcome,
            AttemptOutcome::Succeeded
        );
        let event = store.events("invalid-next").unwrap().pop().unwrap();
        assert_eq!(event.event_type, "recurrence_evaluation_attention");
        assert_eq!(event.to_state, ObligationState::Attention);
    }

    #[test]
    fn failures_back_off_then_exhaust_into_attention() {
        let mut store = Store::open_in_memory().unwrap();
        store.create(one_off("a", 100), 100).unwrap();
        let first = store.claim_due(100, 20, 1).unwrap().remove(0);
        store
            .complete(
                &first,
                Completion::Failed {
                    retryable: true,
                    error: "first".to_owned(),
                    evidence: None,
                },
                101,
            )
            .unwrap();
        let pending = store.get("a").unwrap().unwrap();
        assert_eq!(pending.state, ObligationState::RetryScheduled);
        assert_eq!(pending.next_wake_at, Some(111));

        let second = store.claim_due(111, 20, 1).unwrap().remove(0);
        store
            .complete(
                &second,
                Completion::Failed {
                    retryable: true,
                    error: "second".to_owned(),
                    evidence: None,
                },
                112,
            )
            .unwrap();
        let exhausted = store.get("a").unwrap().unwrap();
        assert_eq!(exhausted.state, ObligationState::Attention);
        assert_eq!(exhausted.next_wake_at, None);
        assert_eq!(store.attempts("a").unwrap().len(), 2);
    }

    #[test]
    fn expired_claim_is_fenced_and_recovered_with_persisted_attempt() {
        let mut store = Store::open_in_memory().unwrap();
        store.create(one_off("a", 100), 100).unwrap();
        let stale = store.claim_due(100, 5, 1).unwrap().remove(0);
        assert_eq!(store.recover_expired_leases(105).unwrap(), 1);
        assert!(matches!(
            store.complete(&stale, Completion::Succeeded { evidence: None }, 105),
            Err(StoreError::Fenced)
        ));
        assert_eq!(
            store.attempts("a").unwrap()[0].outcome,
            AttemptOutcome::LeaseExpired
        );
        assert_eq!(store.get("a").unwrap().unwrap().next_wake_at, Some(115));
    }

    #[test]
    fn final_expired_lease_becomes_visible_attention() {
        let mut store = Store::open_in_memory().unwrap();
        let mut obligation = one_off("a", 100);
        obligation.retry.max_attempts = 1;
        store.create(obligation, 100).unwrap();
        store.claim_due(100, 5, 1).unwrap();

        assert_eq!(store.recover_expired_leases(105).unwrap(), 1);
        let recovered = store.get("a").unwrap().unwrap();
        assert_eq!(recovered.state, ObligationState::Attention);
        assert_eq!(recovered.next_wake_at, None);
        assert_eq!(
            store.attempts("a").unwrap()[0].outcome,
            AttemptOutcome::LeaseExpired
        );
    }

    #[test]
    fn operator_retry_adds_a_new_attempt_without_rewriting_history() {
        let mut store = Store::open_in_memory().unwrap();
        let mut obligation = one_off("a", 100);
        obligation.retry.max_attempts = 1;
        store.create(obligation, 100).unwrap();
        let first = store.claim_due(100, 5, 1).unwrap().remove(0);
        store
            .complete(
                &first,
                Completion::Failed {
                    retryable: false,
                    error: "operator action needed".to_owned(),
                    evidence: None,
                },
                101,
            )
            .unwrap();

        store.retry_attention("a", 200).unwrap();
        let second = store.claim_due(200, 5, 1).unwrap().remove(0);
        assert_eq!(second.attempt_number, 2);
        store
            .complete(&second, Completion::Succeeded { evidence: None }, 201)
            .unwrap();
        assert_eq!(store.attempts("a").unwrap().len(), 2);
        assert_eq!(
            store.get("a").unwrap().unwrap().state,
            ObligationState::Completed
        );
    }

    #[test]
    fn audit_events_cannot_be_changed_or_deleted() {
        let mut store = Store::open_in_memory().unwrap();
        store.create(one_off("a", 100), 100).unwrap();
        assert!(
            store
                .connection
                .execute("DELETE FROM audit_events", [])
                .is_err()
        );
        assert!(
            store
                .connection
                .execute("UPDATE audit_events SET event_type = 'changed'", [])
                .is_err()
        );
        assert_eq!(store.events("a").unwrap()[0].event_type, "created");
    }

    #[test]
    fn approval_is_bound_to_each_recurring_occurrence() {
        let mut store = Store::open_in_memory().unwrap();
        let mut new = one_off("a", 1_782_864_000);
        new.approval_required = true;
        new.recurrence = Some(Recurrence::new("30 9 * * *", "Australia/Adelaide").unwrap());
        store.create(new, 1_782_864_000).unwrap();
        assert!(store.claim_due(1_782_864_000, 30, 1).unwrap().is_empty());
        store
            .decide_approval(
                "a",
                ApprovalDecision::Approved,
                "operator",
                None,
                1_782_864_001,
            )
            .unwrap();
        let claim = store.claim_due(1_782_864_001, 30, 1).unwrap().remove(0);
        store
            .complete(
                &claim,
                Completion::Succeeded { evidence: None },
                1_782_864_002,
            )
            .unwrap();
        let next = store.get("a").unwrap().unwrap();
        assert_eq!(next.occurrence, 2);
        assert_eq!(next.state, ObligationState::AwaitingApproval);
        assert!(store.claim_due(i64::MAX / 2, 30, 1).unwrap().is_empty());
    }

    #[test]
    fn two_connections_cannot_claim_one_attempt() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("state.sqlite3");
        let mut first = Store::open(&path).unwrap();
        let mut second = Store::open(&path).unwrap();
        first.create(one_off("a", 100), 100).unwrap();
        assert_eq!(first.claim_due(100, 30, 1).unwrap().len(), 1);
        assert!(second.claim_due(100, 30, 1).unwrap().is_empty());
        assert_eq!(second.attempts("a").unwrap().len(), 1);
    }

    fn gardener_registration(first_inspection_at: i64) -> NewRepositoryRegistration {
        NewRepositoryRegistration {
            repository: CANONICAL_REPOSITORY.to_owned(),
            default_branch: CANONICAL_DEFAULT_BRANCH.to_owned(),
            checkout_path: "/srv/bokkie".to_owned(),
            inspection_recurrence: Recurrence::new("* * * * *", "Australia/Adelaide").unwrap(),
            first_inspection_at,
        }
    }

    fn new_inspection(id: &str, source_commit: char) -> NewGardenerInspection {
        NewGardenerInspection {
            id: id.to_owned(),
            source_commit: source_commit.to_string().repeat(40),
            worktree_path: format!("/tmp/{id}"),
            prompt_digest: "d".repeat(64),
        }
    }

    fn inspection_result(prompt: &str) -> InspectionResult {
        InspectionResult {
            summary: "One bounded improvement was found".to_owned(),
            proposed_goal_prompts: vec![prompt.to_owned()],
        }
    }

    fn approved_implementation_claim(store: &mut Store, lease_seconds: i64) -> (Claim, Proposal) {
        store
            .register_gardener_repository(gardener_registration(1_000), 900)
            .unwrap();
        let inspection_claim = store.claim_due_gardener(1_000, 60, 1).unwrap().remove(0);
        store
            .start_gardener_inspection(
                &inspection_claim,
                new_inspection("inspection-for-run", 'a'),
                1_001,
            )
            .unwrap();
        let proposal = store
            .finish_gardener_inspection(
                &inspection_claim,
                "inspection-for-run",
                &inspection_result("Implement one bounded store improvement."),
                1_002,
            )
            .unwrap()
            .remove(0);
        store
            .decide_gardener_proposal(
                &proposal.fingerprint,
                ApprovalDecision::Approved,
                "operator",
                None,
                1_003,
            )
            .unwrap();
        let claim = store
            .claim_due_gardener(1_003, lease_seconds, 10)
            .unwrap()
            .into_iter()
            .find(|claim| claim.obligation_id == proposal.implementation_obligation_id)
            .unwrap();
        (claim, proposal)
    }

    fn new_implementation_run(id: &str) -> NewGardenerImplementationRun {
        NewGardenerImplementationRun {
            id: id.to_owned(),
            implementation_worktree_path: format!("/tmp/{id}-implementation"),
            branch: format!("codex/gardener-{id}"),
        }
    }

    fn advance_run_to_pull_request(store: &mut Store, claim: &Claim, run_id: &str, head: &str) {
        store
            .record_implementation_codex_thread(claim, run_id, "implementation-thread", 1_005)
            .unwrap();
        store
            .record_implementation_codex_turn(claim, run_id, "implementation-turn", 1_006)
            .unwrap();
        store
            .finish_gardener_implementation(
                claim,
                run_id,
                r#"{"summary":"implementation completed"}"#,
                1_007,
            )
            .unwrap();
        store
            .record_gardener_git_commit(claim, run_id, head, 1_008)
            .unwrap();
        store
            .record_gardener_push_observation(claim, run_id, head, 1_009)
            .unwrap();
        store
            .record_gardener_ready_pull_request(
                claim,
                run_id,
                42,
                "https://github.com/robchristie/bokkie/pull/42",
                head,
                1_010,
            )
            .unwrap();
    }

    #[test]
    fn gardener_registration_is_atomic_idempotent_and_survives_reopen() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("state.sqlite3");
        let mut store = Store::open(&path).unwrap();
        let requested = gardener_registration(1_000);
        let created = store
            .register_gardener_repository(requested.clone(), 900)
            .unwrap();
        assert_eq!(created.repository, CANONICAL_REPOSITORY);
        assert_eq!(created.default_branch, CANONICAL_DEFAULT_BRANCH);
        assert_eq!(created.created_at, 900);
        assert_eq!(created.updated_at, 900);
        let obligation = store
            .get(&created.inspection_obligation_id)
            .unwrap()
            .unwrap();
        assert_eq!(obligation.state, ObligationState::Pending);
        assert_eq!(obligation.next_wake_at, Some(1_000));
        assert_eq!(obligation.recurrence_cron.as_deref(), Some("* * * * *"));
        assert_eq!(
            obligation.recurrence_timezone.as_deref(),
            Some("Australia/Adelaide")
        );
        assert!(store.claim_due(1_000, 30, 1).unwrap().is_empty());

        let repeated = store
            .register_gardener_repository(requested.clone(), 950)
            .unwrap();
        assert_eq!(repeated, created);
        assert_eq!(store.list().unwrap().len(), 1);
        let mut changed = requested;
        changed.checkout_path = "/srv/a-different-checkout".to_owned();
        assert!(matches!(
            store.register_gardener_repository(changed, 951),
            Err(StoreError::Conflict(_))
        ));
        drop(store);

        let mut reopened = Store::open(path).unwrap();
        assert_eq!(reopened.gardener_repository().unwrap(), Some(created));
        let claim = reopened.claim_due_gardener(1_000, 30, 1).unwrap().remove(0);
        assert_eq!(claim.obligation_id, "gardener:inspect:robchristie/bokkie");
        assert_eq!(
            reopened
                .gardener_obligation_kind(&claim.obligation_id)
                .unwrap(),
            Some(crate::GardenerObligationKind::Inspection)
        );
    }

    #[test]
    fn gardener_registration_rejects_noncanonical_identity() {
        let mut store = Store::open_in_memory().unwrap();
        let mut wrong_repository = gardener_registration(1_000);
        wrong_repository.repository = "someone/else".to_owned();
        assert!(matches!(
            store.register_gardener_repository(wrong_repository, 900),
            Err(StoreError::Invalid(_))
        ));
        let mut wrong_branch = gardener_registration(1_000);
        wrong_branch.default_branch = "develop".to_owned();
        assert!(matches!(
            store.register_gardener_repository(wrong_branch, 900),
            Err(StoreError::Invalid(_))
        ));
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn inspection_identity_updates_are_write_once_and_exact_claim_fenced() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .register_gardener_repository(gardener_registration(1_000), 900)
            .unwrap();
        let claim = store.claim_due_gardener(1_000, 10, 1).unwrap().remove(0);
        let started = store
            .start_gardener_inspection(&claim, new_inspection("inspection-1", 'a'), 1_001)
            .unwrap();
        assert_eq!(started.source_commit, "a".repeat(40));
        store
            .record_inspection_codex_thread(&claim, "inspection-1", "thread-1", 1_002)
            .unwrap();
        store
            .record_inspection_codex_thread(&claim, "inspection-1", "thread-1", 1_003)
            .unwrap();
        assert!(matches!(
            store.record_inspection_codex_thread(&claim, "inspection-1", "different-thread", 1_003),
            Err(StoreError::Conflict(_))
        ));
        store
            .record_inspection_codex_turn(&claim, "inspection-1", "turn-1", 1_004)
            .unwrap();

        store.recover_expired_leases(1_010).unwrap();
        assert!(matches!(
            store.record_inspection_codex_turn(&claim, "inspection-1", "turn-2", 1_010),
            Err(StoreError::Fenced)
        ));
        let retained = store.gardener_inspection("inspection-1").unwrap().unwrap();
        assert_eq!(retained.codex_thread_id.as_deref(), Some("thread-1"));
        assert_eq!(retained.codex_turn_id.as_deref(), Some("turn-1"));
    }

    #[test]
    fn equivalent_prompts_reuse_immutable_proposal_across_commits() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("state.sqlite3");
        let mut store = Store::open(&path).unwrap();
        let registration = store
            .register_gardener_repository(gardener_registration(1_000), 900)
            .unwrap();
        let first_claim = store.claim_due_gardener(1_000, 60, 1).unwrap().remove(0);
        store
            .start_gardener_inspection(&first_claim, new_inspection("inspection-1", 'a'), 1_001)
            .unwrap();
        let first = store
            .finish_gardener_inspection(
                &first_claim,
                "inspection-1",
                &inspection_result("\r\nImprove the lease test.  \r\n"),
                1_002,
            )
            .unwrap()
            .remove(0);
        assert_eq!(first.prompt, "Improve the lease test.");
        assert_eq!(first.observation_count, 1);
        let implementation = store
            .get(&first.implementation_obligation_id)
            .unwrap()
            .unwrap();
        assert_eq!(implementation.state, ObligationState::AwaitingApproval);
        assert_eq!(implementation.max_attempts, 1);
        store
            .complete(
                &first_claim,
                Completion::Succeeded {
                    evidence: Some("inspection-1".to_owned()),
                },
                1_003,
            )
            .unwrap();

        let next_at = store
            .get(&registration.inspection_obligation_id)
            .unwrap()
            .unwrap()
            .next_wake_at
            .unwrap();
        let second_claim = store.claim_due_gardener(next_at, 60, 1).unwrap().remove(0);
        store
            .start_gardener_inspection(
                &second_claim,
                new_inspection("inspection-2", 'b'),
                next_at + 1,
            )
            .unwrap();
        let second = store
            .finish_gardener_inspection(
                &second_claim,
                "inspection-2",
                &inspection_result("Improve the lease test."),
                next_at + 2,
            )
            .unwrap()
            .remove(0);
        assert_eq!(second.fingerprint, first.fingerprint);
        assert_eq!(
            second.implementation_obligation_id,
            first.implementation_obligation_id
        );
        assert_eq!(second.observation_count, 2);
        let observations = store.proposal_observations(&first.fingerprint).unwrap();
        assert_eq!(observations.len(), 2);
        assert_eq!(observations[0].inspection_id, "inspection-1");
        assert_eq!(observations[0].source_commit, "a".repeat(40));
        assert_eq!(observations[1].inspection_id, "inspection-2");
        assert_eq!(observations[1].source_commit, "b".repeat(40));
        assert_eq!(
            store
                .list()
                .unwrap()
                .into_iter()
                .filter(|item| item.id.starts_with("gardener:implement:"))
                .count(),
            1
        );
        let fingerprint = first.fingerprint;
        drop(store);
        let reopened = Store::open(path).unwrap();
        let durable = reopened.gardener_proposal(&fingerprint).unwrap().unwrap();
        assert_eq!(durable.observation_count, 2);
        assert_eq!(
            reopened.proposal_observations(&fingerprint).unwrap().len(),
            2
        );
    }

    #[test]
    fn proposal_decision_uses_occurrence_approval_and_content_stays_immutable() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .register_gardener_repository(gardener_registration(1_000), 900)
            .unwrap();
        let inspection_claim = store.claim_due_gardener(1_000, 60, 1).unwrap().remove(0);
        store
            .start_gardener_inspection(
                &inspection_claim,
                new_inspection("inspection-1", 'a'),
                1_001,
            )
            .unwrap();
        let proposal = store
            .finish_gardener_inspection(
                &inspection_claim,
                "inspection-1",
                &inspection_result("Add a deterministic recovery test."),
                1_002,
            )
            .unwrap()
            .remove(0);
        let approved = store
            .decide_gardener_proposal(
                &proposal.fingerprint,
                ApprovalDecision::Approved,
                "operator",
                Some("content reviewed"),
                1_003,
            )
            .unwrap();
        assert_eq!(approved.prompt, proposal.prompt);
        assert_eq!(approved.approval_decision, Some(ApprovalDecision::Approved));
        assert_eq!(approved.obligation_state, ObligationState::Pending);
        assert!(store.claim_due(1_003, 60, 10).unwrap().is_empty());
        let implementation_claim = store
            .claim_due_gardener(1_003, 60, 10)
            .unwrap()
            .into_iter()
            .find(|claim| claim.obligation_id == proposal.implementation_obligation_id)
            .unwrap();
        assert_eq!(
            store
                .gardener_obligation_kind(&implementation_claim.obligation_id)
                .unwrap(),
            Some(crate::GardenerObligationKind::Implementation)
        );
        assert!(
            store
                .connection
                .execute(
                    "UPDATE gardener_proposals SET prompt = 'changed' WHERE fingerprint = ?1",
                    [&proposal.fingerprint],
                )
                .is_err()
        );
        assert!(
            store
                .connection
                .execute(
                    "UPDATE approvals SET decision = 'rejected'
                     WHERE obligation_id = ?1",
                    [&proposal.implementation_obligation_id],
                )
                .is_err()
        );
        assert_eq!(
            store
                .gardener_proposal(&proposal.fingerprint)
                .unwrap()
                .unwrap()
                .prompt,
            "Add a deterministic recovery test."
        );
    }

    #[test]
    fn rejection_remains_visible_as_proposal_attention() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .register_gardener_repository(gardener_registration(1_000), 900)
            .unwrap();
        let claim = store.claim_due_gardener(1_000, 60, 1).unwrap().remove(0);
        store
            .start_gardener_inspection(&claim, new_inspection("inspection-1", 'a'), 1_001)
            .unwrap();
        let proposal = store
            .finish_gardener_inspection(
                &claim,
                "inspection-1",
                &inspection_result("Document the invariant."),
                1_002,
            )
            .unwrap()
            .remove(0);
        let rejected = store
            .decide_gardener_proposal(
                &proposal.fingerprint,
                ApprovalDecision::Rejected,
                "operator",
                None,
                1_003,
            )
            .unwrap();
        assert_eq!(rejected.approval_decision, Some(ApprovalDecision::Rejected));
        assert_eq!(rejected.obligation_state, ObligationState::Attention);
        assert!(store.claim_due_gardener(2_000, 60, 10).unwrap().is_empty());
    }

    #[test]
    fn terminal_inspection_result_and_gardening_history_are_append_only() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .register_gardener_repository(gardener_registration(1_000), 900)
            .unwrap();
        let claim = store.claim_due_gardener(1_000, 60, 1).unwrap().remove(0);
        store
            .start_gardener_inspection(&claim, new_inspection("inspection-1", 'a'), 1_001)
            .unwrap();
        store
            .finish_gardener_inspection(
                &claim,
                "inspection-1",
                &inspection_result("Keep this prompt."),
                1_002,
            )
            .unwrap();
        let retained = store.gardener_inspection("inspection-1").unwrap().unwrap();
        assert_eq!(retained.completed_at, Some(1_002));
        assert_eq!(
            serde_json::from_str::<InspectionResult>(retained.result_json.as_deref().unwrap())
                .unwrap(),
            inspection_result("Keep this prompt.")
        );
        assert!(matches!(
            store.finish_gardener_inspection(
                &claim,
                "inspection-1",
                &inspection_result("Replace it."),
                1_003
            ),
            Err(StoreError::Conflict(_))
        ));
        assert!(
            store
                .connection
                .execute("DELETE FROM gardener_events", [])
                .is_err()
        );
        assert!(
            store
                .connection
                .execute(
                    "UPDATE gardener_repositories SET checkout_path = '/changed'",
                    [],
                )
                .is_err()
        );
        assert!(
            store
                .connection
                .execute("DELETE FROM gardener_obligation_bindings", [])
                .is_err()
        );
        assert!(
            store
                .connection
                .execute(
                    "UPDATE gardener_inspections SET source_commit = ?1 WHERE id = 'inspection-1'",
                    ["c".repeat(40)],
                )
                .is_err()
        );
        assert!(
            store
                .connection
                .execute("DELETE FROM gardener_proposal_observations", [])
                .is_err()
        );
        assert!(!store.gardener_events().unwrap().is_empty());
    }

    #[test]
    fn implementation_run_enforces_ordering_write_once_heads_and_fresh_verification() {
        let mut store = Store::open_in_memory().unwrap();
        let (claim, proposal) = approved_implementation_claim(&mut store, 1_000);
        let run = store
            .create_gardener_implementation_run(&claim, new_implementation_run("run-1"), 1_004)
            .unwrap();
        assert_eq!(run.phase, GardenerRunPhase::Created);
        assert_eq!(run.proposal_fingerprint, proposal.fingerprint);
        assert_eq!(run.source_commit, "a".repeat(40));
        assert_eq!(run.occurrence, claim.occurrence);
        assert_eq!(run.attempt_number, claim.attempt_number);
        assert_eq!(run.lease_generation, claim.lease_generation);
        assert_eq!(run.lease_token, claim.lease_token);

        let repeated = store
            .create_gardener_implementation_run(&claim, new_implementation_run("run-1"), 1_004)
            .unwrap();
        assert_eq!(repeated, run);
        assert!(matches!(
            store.create_gardener_implementation_run(
                &claim,
                new_implementation_run("different-run"),
                1_004
            ),
            Err(StoreError::Conflict(_))
        ));
        assert!(matches!(
            store.record_implementation_codex_turn(&claim, "run-1", "too-early", 1_005),
            Err(StoreError::Conflict(_))
        ));

        store
            .record_implementation_codex_thread(&claim, "run-1", "implementation-thread", 1_005)
            .unwrap();
        store
            .record_implementation_codex_thread(&claim, "run-1", "implementation-thread", 1_006)
            .unwrap();
        assert!(matches!(
            store.record_implementation_codex_thread(&claim, "run-1", "replacement-thread", 1_006),
            Err(StoreError::Conflict(_))
        ));
        store
            .record_implementation_codex_turn(&claim, "run-1", "implementation-turn", 1_006)
            .unwrap();
        assert!(matches!(
            store.finish_gardener_implementation(&claim, "run-1", "not JSON", 1_007),
            Err(StoreError::Invalid(_))
        ));
        store
            .finish_gardener_implementation(&claim, "run-1", r#"{"summary":"done"}"#, 1_007)
            .unwrap();

        let head = "b".repeat(40);
        let other_head = "c".repeat(40);
        store
            .record_gardener_git_commit(&claim, "run-1", &head, 1_008)
            .unwrap();
        assert!(matches!(
            store.record_gardener_git_commit(&claim, "run-1", &other_head, 1_008),
            Err(StoreError::Conflict(_))
        ));
        assert!(matches!(
            store.record_gardener_push_observation(&claim, "run-1", &other_head, 1_009),
            Err(StoreError::Conflict(_))
        ));
        store
            .record_gardener_push_observation(&claim, "run-1", &head, 1_009)
            .unwrap();
        assert!(matches!(
            store.record_gardener_ready_pull_request(
                &claim,
                "run-1",
                42,
                "https://github.com/robchristie/bokkie/pull/42",
                &other_head,
                1_010
            ),
            Err(StoreError::Conflict(_))
        ));
        store
            .record_gardener_ready_pull_request(
                &claim,
                "run-1",
                42,
                "https://github.com/robchristie/bokkie/pull/42",
                &head,
                1_010,
            )
            .unwrap();

        assert!(matches!(
            store.start_gardener_verification(
                &claim,
                "run-1",
                "/tmp/run-1-implementation",
                &head,
                1_011
            ),
            Err(StoreError::Conflict(_))
        ));
        assert!(matches!(
            store.start_gardener_verification(
                &claim,
                "run-1",
                "/tmp/run-1-verification",
                &other_head,
                1_011
            ),
            Err(StoreError::Conflict(_))
        ));
        store
            .start_gardener_verification(&claim, "run-1", "/tmp/run-1-verification", &head, 1_011)
            .unwrap();
        assert!(matches!(
            store.record_verification_codex_thread(&claim, "run-1", "implementation-thread", 1_012),
            Err(StoreError::Conflict(_))
        ));
        store
            .record_verification_codex_thread(&claim, "run-1", "verification-thread", 1_012)
            .unwrap();
        assert!(matches!(
            store.record_verification_codex_turn(&claim, "run-1", "implementation-turn", 1_013),
            Err(StoreError::Conflict(_))
        ));
        store
            .record_verification_codex_turn(&claim, "run-1", "verification-turn", 1_013)
            .unwrap();
        assert!(matches!(
            store.finish_gardener_verification(
                &claim,
                "run-1",
                GardenerVerificationVerdict::Pass,
                &other_head,
                "wrong head",
                1_014
            ),
            Err(StoreError::Conflict(_))
        ));
        store
            .finish_gardener_verification(
                &claim,
                "run-1",
                GardenerVerificationVerdict::Pass,
                &head,
                "Exact head passes independent verification.",
                1_014,
            )
            .unwrap();
        assert!(matches!(
            store.finish_gardener_verification(
                &claim,
                "run-1",
                GardenerVerificationVerdict::Blocking,
                &head,
                "changed",
                1_015
            ),
            Err(StoreError::Conflict(_))
        ));

        let finished = store.gardener_implementation_run("run-1").unwrap().unwrap();
        assert_eq!(finished.phase, GardenerRunPhase::VerificationFinished);
        assert_eq!(
            finished.verification_verdict,
            Some(GardenerVerificationVerdict::Pass)
        );
        assert_eq!(finished.pull_request_head.as_deref(), Some(head.as_str()));
        assert_eq!(
            finished.verification_reported_head.as_deref(),
            Some(head.as_str())
        );
        assert_eq!(store.gardener_implementation_runs().unwrap(), [finished]);
        assert_eq!(
            store
                .gardener_implementation_runs_for_obligation(&claim.obligation_id)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(store.gardener_run_events("run-1").unwrap().len(), 11);
    }

    #[test]
    fn implementation_run_captures_the_latest_proposal_observation_commit() {
        let mut store = Store::open_in_memory().unwrap();
        let registration = store
            .register_gardener_repository(gardener_registration(1_000), 900)
            .unwrap();
        let first_claim = store.claim_due_gardener(1_000, 60, 1).unwrap().remove(0);
        store
            .start_gardener_inspection(
                &first_claim,
                new_inspection("source-a-inspection", 'a'),
                1_001,
            )
            .unwrap();
        let prompt = "Implement the observed improvement.";
        let proposal = store
            .finish_gardener_inspection(
                &first_claim,
                "source-a-inspection",
                &inspection_result(prompt),
                1_002,
            )
            .unwrap()
            .remove(0);
        store
            .complete(
                &first_claim,
                Completion::Succeeded { evidence: None },
                1_003,
            )
            .unwrap();

        let next_at = store
            .get(&registration.inspection_obligation_id)
            .unwrap()
            .unwrap()
            .next_wake_at
            .unwrap();
        let second_claim = store.claim_due_gardener(next_at, 60, 1).unwrap().remove(0);
        store
            .start_gardener_inspection(
                &second_claim,
                new_inspection("source-b-inspection", 'b'),
                next_at + 1,
            )
            .unwrap();
        store
            .finish_gardener_inspection(
                &second_claim,
                "source-b-inspection",
                &inspection_result(prompt),
                next_at + 2,
            )
            .unwrap();
        store
            .decide_gardener_proposal(
                &proposal.fingerprint,
                ApprovalDecision::Approved,
                "operator",
                None,
                next_at + 3,
            )
            .unwrap();
        let implementation_claim = store
            .claim_due_gardener(next_at + 3, 1_000, 10)
            .unwrap()
            .into_iter()
            .find(|claim| claim.obligation_id == proposal.implementation_obligation_id)
            .unwrap();
        let run = store
            .create_gardener_implementation_run(
                &implementation_claim,
                new_implementation_run("latest-source-run"),
                next_at + 4,
            )
            .unwrap();
        assert_eq!(run.source_commit, "b".repeat(40));
    }

    #[test]
    fn every_run_write_is_fenced_after_the_claim_expires() {
        let mut store = Store::open_in_memory().unwrap();
        let (claim, _) = approved_implementation_claim(&mut store, 10);
        store
            .create_gardener_implementation_run(&claim, new_implementation_run("fenced-run"), 1_004)
            .unwrap();
        store.recover_expired_leases(1_013).unwrap();
        assert!(matches!(
            store.record_implementation_codex_thread(&claim, "fenced-run", "late-thread", 1_013),
            Err(StoreError::Fenced)
        ));
        let retained = store
            .gardener_implementation_run("fenced-run")
            .unwrap()
            .unwrap();
        assert_eq!(retained.phase, GardenerRunPhase::Created);
        assert!(retained.implementation_thread_id.is_none());
    }

    #[test]
    fn implementation_run_and_append_only_events_survive_reopen() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("state.sqlite3");
        let mut store = Store::open(&path).unwrap();
        let (claim, _) = approved_implementation_claim(&mut store, 1_000);
        store
            .create_gardener_implementation_run(
                &claim,
                new_implementation_run("durable-run"),
                1_004,
            )
            .unwrap();
        let head = "d".repeat(40);
        advance_run_to_pull_request(&mut store, &claim, "durable-run", &head);
        let before = store
            .gardener_implementation_run("durable-run")
            .unwrap()
            .unwrap();
        let events_before = store.gardener_run_events("durable-run").unwrap();
        assert_eq!(events_before.len(), 7);
        assert!(
            store
                .connection
                .execute("DELETE FROM gardener_run_events", [])
                .is_err()
        );
        assert!(
            store
                .connection
                .execute(
                    "UPDATE gardener_run_events SET event_type = 'changed' WHERE run_id = ?1",
                    ["durable-run"]
                )
                .is_err()
        );
        assert!(
            store
                .connection
                .execute(
                    "UPDATE gardener_implementation_runs SET git_commit = ?2 WHERE id = ?1",
                    params!["durable-run", "e".repeat(40)]
                )
                .is_err()
        );
        drop(store);

        let reopened = Store::open(path).unwrap();
        assert_eq!(
            reopened.gardener_implementation_run("durable-run").unwrap(),
            Some(before)
        );
        assert_eq!(
            reopened.gardener_run_events("durable-run").unwrap(),
            events_before
        );
    }
}
