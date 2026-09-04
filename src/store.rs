use std::{
    path::Path,
    str::FromStr,
    sync::atomic::{AtomicI64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bokkie_operator_api::ActionPrecondition;
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, Transaction, TransactionBehavior, params,
    types::Type,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    ApprovalDecision, Attempt, AttemptOutcome, AuditEvent, Claim, Completion, FailureDisposition,
    MAX_APPROVAL_ACTOR_CHARS, MAX_APPROVAL_NOTE_CHARS, MAX_AUDIT_DETAILS_BYTES,
    MAX_AUDIT_EVENT_TYPE_CHARS, MAX_COMPLETION_ERROR_CHARS, MAX_COMPLETION_EVIDENCE_CHARS,
    MAX_OBLIGATION_DESCRIPTION_CHARS, MAX_OBLIGATION_ID_CHARS, MAX_RECURRENCE_EXPRESSION_CHARS,
    MAX_RECURRENCE_TIMEZONE_CHARS, NewObligation, Obligation, ObligationState, Recurrence,
    gardener::{
        CANONICAL_DEFAULT_BRANCH, CANONICAL_REPOSITORY, GardenerCandidateQualification,
        GardenerEvent, GardenerImplementationRun, GardenerInspection, GardenerPublicationState,
        GardenerReproducibilityManifest, GardenerRunEvent, GardenerRunPhase,
        GardenerVerificationVerdict, InspectionResult, MAX_GARDENER_MODEL_ITEM_CHARS,
        MAX_GARDENER_MODEL_ITEMS, MAX_GARDENER_MODEL_MESSAGE_BYTES, MAX_GARDENER_MODEL_TEXT_CHARS,
        MAX_GARDENER_PROMPT_CHARS, MAX_GARDENER_PROMPTS, NewGardenerImplementationRun,
        NewGardenerInspection, NewRepositoryRegistration, Proposal, ProposalInstance,
        ProposalObservation, RepositoryRegistration, normalise_goal_prompt, proposal_fingerprint,
        proposal_instance_id,
    },
    recurrence::RecurrenceError,
};

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
    pub(crate) connection: Connection,
}

impl Store {
    /// Open a database and bring its schema to the current immutable manifest.
    /// Service startup should call this exactly once before starting consumers.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let mut connection = Connection::open(path)?;
        Self::configure_before_migration(&connection)?;
        crate::migrations::migrate(&mut connection)?;
        Self::initialise_migrated(connection)
    }

    /// Open an already migrated database without performing schema writes.
    pub fn open_compatible(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        crate::migrations::validate_current(&connection)?;
        Self::initialise_compatible(connection)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let mut connection = Connection::open_in_memory()?;
        Self::configure_before_migration(&connection)?;
        crate::migrations::migrate(&mut connection)?;
        Self::initialise_migrated(connection)
    }

    fn configure_before_migration(connection: &Connection) -> Result<(), StoreError> {
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    }

    fn initialise_migrated(connection: Connection) -> Result<Self, StoreError> {
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;

        Ok(Self { connection })
    }

    fn initialise_compatible(connection: Connection) -> Result<Self, StoreError> {
        let journal_mode: String =
            connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if !journal_mode.eq_ignore_ascii_case("wal") {
            return Err(StoreError::Invalid(format!(
                "database journal mode is {journal_mode:?}, expected \"wal\"; startup initialisation is required"
            )));
        }
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        Ok(Self { connection })
    }

    pub(crate) fn with_deferred_read<T>(
        &self,
        operation: impl FnOnce(&Self) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let transaction = self.connection.unchecked_transaction()?;
        match operation(self) {
            Ok(value) => {
                transaction.commit()?;
                Ok(value)
            }
            Err(error) => Err(error),
        }
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

    /// Read each obligation together with the immutable audit sequence that
    /// represents its current state. The correlated value and obligation row
    /// come from one SQLite statement snapshot, so capabilities cannot combine
    /// an old state with a newer revision (or the reverse).
    pub(crate) fn list_with_state_revisions(&self) -> Result<Vec<(Obligation, i64)>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT o.*,
                    (SELECT sequence FROM audit_events a
                     WHERE a.obligation_id = o.id
                     ORDER BY sequence DESC LIMIT 1) AS state_revision
             FROM obligations o ORDER BY o.created_at, o.id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((obligation_from_row(row)?, row.get("state_revision")?))
        })?;
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
        self.decide_approval_inner(id, decision, actor, note, None, now)
    }

    pub fn decide_approval_if_current(
        &mut self,
        id: &str,
        decision: ApprovalDecision,
        actor: &str,
        note: Option<&str>,
        precondition: &ActionPrecondition,
        now: i64,
    ) -> Result<(), StoreError> {
        self.decide_approval_inner(id, decision, actor, note, Some(precondition), now)
    }

    fn decide_approval_inner(
        &mut self,
        id: &str,
        decision: ApprovalDecision,
        actor: &str,
        note: Option<&str>,
        precondition: Option<&ActionPrecondition>,
        now: i64,
    ) -> Result<(), StoreError> {
        validate_bounded_text("approval actor", actor, MAX_APPROVAL_ACTOR_CHARS, false)?;
        if let Some(note) = note {
            validate_bounded_text("approval note", note, MAX_APPROVAL_NOTE_CHARS, true)?;
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(precondition) = precondition {
            validate_action_precondition(&transaction, id, precondition, None, None)?;
        }
        let is_gardener_proposal = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM gardener_proposal_instances
                WHERE implementation_obligation_id = ?1
             )",
            [id],
            |row| row.get::<_, bool>(0),
        )?;
        if is_gardener_proposal {
            return Err(StoreError::Conflict(
                "gardener proposals require the exact proposal decision path".to_owned(),
            ));
        }
        apply_transition(
            &transaction,
            Transition::Approval {
                id,
                decision,
                actor,
                note,
                gardener_instance: None,
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
               AND NOT EXISTS (
                   SELECT 1
                   FROM gardener_proposal_instances pi
                   JOIN gardener_proposal_instance_supersessions s
                     ON s.superseded_instance_id = pi.id
                   WHERE pi.implementation_obligation_id = o.id
               )
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

    pub fn gardener_inspections(&self) -> Result<Vec<GardenerInspection>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id, repository, obligation_id, occurrence, lease_generation,
                    source_commit, worktree_path, prompt_digest, codex_thread_id,
                    codex_turn_id, result_json, started_at, completed_at
             FROM gardener_inspections ORDER BY started_at, id",
        )?;
        let rows = statement.query_map([], gardener_inspection_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
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
            let observation_id: i64 = transaction.query_row(
                "SELECT id FROM gardener_proposal_observations
                 WHERE proposal_fingerprint = ?1 AND inspection_id = ?2",
                params![fingerprint, inspection_id],
                |row| row.get(0),
            )?;
            let source_commit = inspection.source_commit.to_ascii_lowercase();
            let instance =
                proposal_instance_for_source(&transaction, &fingerprint, &source_commit)?;
            let instance = match instance {
                Some(instance) => instance,
                None => create_proposal_instance(
                    &transaction,
                    &fingerprint,
                    &inspection.repository,
                    &source_commit,
                    observation_id,
                    inspection_id,
                    now,
                )?,
            };
            transaction.execute(
                "INSERT INTO gardener_proposal_observation_instances(
                    observation_id, instance_id, mapped_at
                 ) VALUES (?1, ?2, ?3)
                 ON CONFLICT(observation_id) DO NOTHING",
                params![observation_id, instance.id, now],
            )?;
            if inserted == 1 {
                append_gardener_event(
                    &transaction,
                    &inspection.repository,
                    Some(inspection_id),
                    Some(&fingerprint),
                    "proposal_observed",
                    now,
                    json!({
                        "source_commit": source_commit,
                        "proposal_instance_id": instance.id,
                        "generation": instance.generation,
                        "source_observation_id": instance.source_observation_id
                    }),
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

    pub fn gardener_proposals(&self) -> Result<Vec<Proposal>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT p.fingerprint, p.repository, p.prompt, pi.implementation_obligation_id,
                    o.state,
                    (SELECT d.decision FROM gardener_proposal_instance_decisions d
                     WHERE d.instance_id = pi.id ORDER BY d.approval_id DESC LIMIT 1),
                    (SELECT COUNT(*) FROM gardener_proposal_observations po
                     WHERE po.proposal_fingerprint = p.fingerprint),
                    p.created_at
             FROM gardener_proposals p
             JOIN gardener_proposal_instances pi
               ON pi.proposal_fingerprint = p.fingerprint
              AND pi.generation = (SELECT max(current.generation)
                                   FROM gardener_proposal_instances current
                                   WHERE current.proposal_fingerprint = p.fingerprint)
             JOIN obligations o ON o.id = pi.implementation_obligation_id
             ORDER BY p.created_at, p.fingerprint",
        )?;
        let rows = statement.query_map([], proposal_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
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

    /// Observations durably mapped to one exact source-bound proposal instance.
    pub fn proposal_instance_observations(
        &self,
        instance_id: &str,
    ) -> Result<Vec<ProposalObservation>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT po.id, po.proposal_fingerprint, po.inspection_id,
                    po.source_commit, po.observed_at
             FROM gardener_proposal_observations po
             JOIN gardener_proposal_observation_instances oi
               ON oi.observation_id = po.id
             WHERE oi.instance_id = ?1
             ORDER BY po.id",
        )?;
        let rows = statement.query_map([instance_id], |row| {
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

    pub fn gardener_proposal_instance(
        &self,
        instance_id: &str,
    ) -> Result<Option<ProposalInstance>, StoreError> {
        proposal_instance(&self.connection, instance_id)
    }

    pub fn gardener_proposal_instances(
        &self,
        fingerprint: &str,
    ) -> Result<Vec<ProposalInstance>, StoreError> {
        let mut statement = self.connection.prepare(PROPOSAL_INSTANCE_SELECT)?;
        let rows = statement.query_map([fingerprint], proposal_instance_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Read every proposal instance from one SQLite snapshot. The HTTP list
    /// projection must not combine a proposal catalogue from one database
    /// state with per-proposal generations from another.
    pub fn gardener_proposal_instances_all(&self) -> Result<Vec<ProposalInstance>, StoreError> {
        self.gardener_proposal_instances_all_with_hook(|| Ok(()))
    }

    fn gardener_proposal_instances_all_with_hook(
        &self,
        between_catalogue_and_instances: impl FnOnce() -> Result<(), StoreError>,
    ) -> Result<Vec<ProposalInstance>, StoreError> {
        self.with_deferred_read(|store| {
            let proposals = store.gardener_proposals()?;
            between_catalogue_and_instances()?;
            let mut instances = Vec::new();
            for proposal in proposals {
                instances.extend(store.gardener_proposal_instances(&proposal.fingerprint)?);
            }
            Ok(instances)
        })
    }

    pub fn decide_gardener_proposal(
        &mut self,
        fingerprint: &str,
        decision: ApprovalDecision,
        actor: &str,
        note: Option<&str>,
        now: i64,
    ) -> Result<Proposal, StoreError> {
        let instances = self.gardener_proposal_instances(fingerprint)?;
        if instances.len() != 1 {
            return Err(StoreError::Conflict(
                "legacy gardener decision is ambiguous; select an exact proposal instance"
                    .to_owned(),
            ));
        }
        self.decide_gardener_proposal_instance_inner(
            &instances[0].id,
            decision,
            actor,
            note,
            None,
            now,
        )
        .and_then(|_| {
            self.gardener_proposal(fingerprint)?
                .ok_or_else(|| StoreError::NotFound(fingerprint.to_owned()))
        })
    }

    pub fn decide_gardener_proposal_if_current(
        &mut self,
        fingerprint: &str,
        decision: ApprovalDecision,
        actor: &str,
        note: Option<&str>,
        precondition: &ActionPrecondition,
        now: i64,
    ) -> Result<Proposal, StoreError> {
        let instances = self.gardener_proposal_instances(fingerprint)?;
        if instances.len() != 1 {
            return Err(StoreError::Conflict(
                "legacy gardener decision is ambiguous; select an exact proposal instance"
                    .to_owned(),
            ));
        }
        self.decide_gardener_proposal_instance_inner(
            &instances[0].id,
            decision,
            actor,
            note,
            Some((precondition, false)),
            now,
        )?;
        self.gardener_proposal(fingerprint)?
            .ok_or_else(|| StoreError::NotFound(fingerprint.to_owned()))
    }

    pub fn decide_gardener_proposal_instance(
        &mut self,
        instance_id: &str,
        decision: ApprovalDecision,
        actor: &str,
        note: Option<&str>,
        now: i64,
    ) -> Result<ProposalInstance, StoreError> {
        self.decide_gardener_proposal_instance_inner(instance_id, decision, actor, note, None, now)
    }

    pub fn decide_gardener_proposal_instance_if_current(
        &mut self,
        instance_id: &str,
        decision: ApprovalDecision,
        actor: &str,
        note: Option<&str>,
        precondition: &ActionPrecondition,
        now: i64,
    ) -> Result<ProposalInstance, StoreError> {
        self.decide_gardener_proposal_instance_inner(
            instance_id,
            decision,
            actor,
            note,
            Some((precondition, true)),
            now,
        )
    }

    fn decide_gardener_proposal_instance_inner(
        &mut self,
        instance_id: &str,
        decision: ApprovalDecision,
        actor: &str,
        note: Option<&str>,
        precondition: Option<(&ActionPrecondition, bool)>,
        now: i64,
    ) -> Result<ProposalInstance, StoreError> {
        validate_bounded_text("approval actor", actor, MAX_APPROVAL_ACTOR_CHARS, false)?;
        if let Some(note) = note {
            validate_bounded_text("approval note", note, MAX_APPROVAL_NOTE_CHARS, true)?;
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = proposal_instance(&transaction, instance_id)?
            .ok_or_else(|| StoreError::NotFound(instance_id.to_owned()))?;
        if current.superseded_by.is_some() {
            return Err(StoreError::Conflict(format!(
                "proposal instance {instance_id:?} has been superseded"
            )));
        }
        if let Some((precondition, require_exact_precondition)) = precondition {
            validate_action_precondition(
                &transaction,
                &current.implementation_obligation_id,
                precondition,
                Some(&current.proposal_fingerprint),
                require_exact_precondition.then_some(&current),
            )?;
        }
        let obligation = require_obligation(&transaction, &current.implementation_obligation_id)?;
        apply_transition(
            &transaction,
            Transition::Approval {
                id: &current.implementation_obligation_id,
                decision,
                actor,
                note,
                gardener_instance: Some(GardenerDecisionContext {
                    instance_id: &current.id,
                    proposal_fingerprint: &current.proposal_fingerprint,
                    source_commit: &current.source_commit,
                    source_observation_id: current.source_observation_id,
                    source_inspection_id: &current.source_inspection_id,
                    generation: current.generation,
                }),
                now,
            },
        )?;
        let approval_id: i64 = transaction.query_row(
            "SELECT id FROM approvals
             WHERE obligation_id = ?1 AND occurrence = ?2
             ORDER BY id DESC LIMIT 1",
            params![current.implementation_obligation_id, obligation.occurrence],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO gardener_proposal_instance_decisions(
                approval_id, instance_id, proposal_fingerprint, source_commit,
                generation, obligation_id, occurrence, decision, recorded_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                approval_id,
                current.id,
                current.proposal_fingerprint,
                current.source_commit,
                current.generation,
                current.implementation_obligation_id,
                obligation.occurrence,
                decision.to_string(),
                now,
            ],
        )?;
        append_gardener_event(
            &transaction,
            &current.repository,
            None,
            Some(&current.proposal_fingerprint),
            &format!("proposal_{decision}"),
            now,
            json!({
                "actor": actor,
                "note": note,
                "proposal_instance_id": current.id,
                "source_commit": current.source_commit,
                "source_observation_id": current.source_observation_id,
                "source_inspection_id": current.source_inspection_id,
                "generation": current.generation
            }),
        )?;
        let decided = proposal_instance(&transaction, instance_id)?
            .expect("proposal instance remains present");
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

        let instance = proposal_instance_for_obligation(&transaction, &claim.obligation_id)?
            .ok_or_else(|| {
                StoreError::Conflict(format!(
                    "implementation obligation {:?} is not bound to an exact proposal instance",
                    claim.obligation_id
                ))
            })?;
        if instance.superseded_by.is_some() {
            return Err(StoreError::Fenced);
        }
        let approved: bool = transaction.query_row(
            "SELECT coalesce((
                SELECT d.decision = 'approved' AND a.decision = 'approved'
                FROM gardener_proposal_instance_decisions d
                JOIN approvals a ON a.id = d.approval_id
                WHERE d.instance_id = ?1 AND d.obligation_id = ?2
                  AND d.occurrence = ?3
                ORDER BY d.approval_id DESC LIMIT 1
             ), 0)",
            params![instance.id, claim.obligation_id, claim.occurrence],
            |row| row.get(0),
        )?;
        if !approved {
            return Err(StoreError::Conflict(format!(
                "proposal instance {:?} lacks an exact approval",
                instance.id
            )));
        }
        let fingerprint = instance.proposal_fingerprint.clone();
        let repository = instance.repository.clone();
        let source_commit = instance.source_commit.clone();

        if let Some(existing) = gardener_implementation_run_for_lease(
            &transaction,
            &claim.obligation_id,
            claim.lease_generation,
        )? {
            if existing.id == new.id
                && existing.proposal_fingerprint == fingerprint
                && existing.proposal_instance_id == instance.id
                && existing.proposal_generation == instance.generation
                && existing.source_observation_id == instance.source_observation_id
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
        transaction.execute(
            "INSERT INTO gardener_implementation_run_instances(
                run_id, instance_id, proposal_fingerprint, source_commit, generation, mapped_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                new.id,
                instance.id,
                fingerprint,
                source_commit,
                instance.generation,
                now
            ],
        )?;
        append_gardener_run_event(
            &transaction,
            &new.id,
            "implementation_run_created",
            now,
            json!({
                "proposal_fingerprint": fingerprint,
                "proposal_instance_id": instance.id,
                "proposal_generation": instance.generation,
                "source_observation_id": instance.source_observation_id,
                "source_inspection_id": instance.source_inspection_id,
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
            "SELECT r.*,
                    ri.instance_id AS exact_proposal_instance_id,
                    ri.generation AS exact_proposal_generation,
                    pi.source_observation_id AS exact_source_observation_id,
                    pi.source_inspection_id AS exact_source_inspection_id,
                    CASE WHEN ready.run_id IS NULL THEN r.publication_state ELSE 'ready' END
                        AS effective_publication_state,
                    ready.ready_at AS effective_pull_request_ready_at
             FROM gardener_implementation_runs r
             JOIN gardener_implementation_run_instances ri ON ri.run_id = r.id
             JOIN gardener_proposal_instances pi ON pi.id = ri.instance_id
             LEFT JOIN gardener_pull_request_ready_observations ready ON ready.run_id = r.id
             ORDER BY r.created_at, r.id",
            [],
        )
    }

    pub fn gardener_implementation_runs_for_obligation(
        &self,
        obligation_id: &str,
    ) -> Result<Vec<GardenerImplementationRun>, StoreError> {
        query_gardener_implementation_runs(
            &self.connection,
            "SELECT r.*,
                    ri.instance_id AS exact_proposal_instance_id,
                    ri.generation AS exact_proposal_generation,
                    pi.source_observation_id AS exact_source_observation_id,
                    pi.source_inspection_id AS exact_source_inspection_id,
                    CASE WHEN ready.run_id IS NULL THEN r.publication_state ELSE 'ready' END
                        AS effective_publication_state,
                    ready.ready_at AS effective_pull_request_ready_at
             FROM gardener_implementation_runs r
             JOIN gardener_implementation_run_instances ri ON ri.run_id = r.id
             JOIN gardener_proposal_instances pi ON pi.id = ri.instance_id
             LEFT JOIN gardener_pull_request_ready_observations ready ON ready.run_id = r.id
             WHERE r.obligation_id = ?1 ORDER BY r.lease_generation, r.id",
            [obligation_id],
        )
    }

    /// Persist the immutable tool, prompt, schema and policy identities before
    /// implementation execution begins.
    pub fn record_gardener_reproducibility_manifest(
        &mut self,
        claim: &Claim,
        manifest: &GardenerReproducibilityManifest,
        now: i64,
    ) -> Result<(), StoreError> {
        validate_reproducibility_manifest(manifest)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = require_current_gardener_run(&transaction, claim, &manifest.run_id, now)?;
        require_run_phase(&run, GardenerRunPhase::Created)?;
        if run.source_commit != manifest.source_commit {
            return Err(StoreError::Conflict(format!(
                "run {:?} reproducibility source does not match its immutable source commit",
                manifest.run_id
            )));
        }
        if let Some(existing) = gardener_reproducibility_manifest(&transaction, &manifest.run_id)? {
            if existing == *manifest {
                transaction.commit()?;
                return Ok(());
            }
            return Err(StoreError::Conflict(format!(
                "run {:?} already has a different reproducibility manifest",
                manifest.run_id
            )));
        }
        transaction.execute(
            "INSERT INTO gardener_run_reproducibility(
                run_id, bokkie_build, source_commit, prompt_digest,
                implementation_schema_digest, verification_schema_digest,
                codex_profile, codex_model, executable_manifest_json,
                sandbox_policy_digest, environment_policy_digest,
                check_commands_json, recorded_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                manifest.run_id,
                manifest.bokkie_build,
                manifest.source_commit,
                manifest.prompt_digest,
                manifest.implementation_schema_digest,
                manifest.verification_schema_digest,
                manifest.codex_profile,
                manifest.codex_model,
                manifest.executable_manifest_json,
                manifest.sandbox_policy_digest,
                manifest.environment_policy_digest,
                manifest.check_commands_json,
                manifest.recorded_at,
            ],
        )?;
        append_gardener_run_event(
            &transaction,
            &manifest.run_id,
            "reproducibility_manifest_recorded",
            now,
            json!({
                "source_commit": manifest.source_commit,
                "prompt_digest": manifest.prompt_digest,
                "environment_policy_digest": manifest.environment_policy_digest,
                "sandbox_policy_digest": manifest.sandbox_policy_digest,
            }),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn gardener_reproducibility_manifest(
        &self,
        run_id: &str,
    ) -> Result<Option<GardenerReproducibilityManifest>, StoreError> {
        gardener_reproducibility_manifest(&self.connection, run_id)
    }

    /// Record Bokkie's credential-free candidate manifests and passing checks
    /// before a branch may be pushed.
    pub fn record_gardener_candidate_qualification(
        &mut self,
        claim: &Claim,
        qualification: &GardenerCandidateQualification,
        now: i64,
    ) -> Result<(), StoreError> {
        validate_candidate_qualification(qualification)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = require_current_gardener_run(&transaction, claim, &qualification.run_id, now)?;
        require_run_phase(&run, GardenerRunPhase::GitCommitRecorded)?;
        if run.git_commit.as_deref() != Some(&qualification.head) {
            return Err(StoreError::Conflict(format!(
                "run {:?} candidate qualification does not match its recorded Git commit",
                qualification.run_id
            )));
        }
        if let Some(existing) =
            gardener_candidate_qualification(&transaction, &qualification.run_id)?
        {
            if existing == *qualification {
                transaction.commit()?;
                return Ok(());
            }
            return Err(StoreError::Conflict(format!(
                "run {:?} already has different candidate qualification evidence",
                qualification.run_id
            )));
        }
        transaction.execute(
            "INSERT INTO gardener_candidate_qualifications(
                run_id, head, diff_manifest_json, tree_manifest_json,
                checks_json, duration_ms, qualified_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                qualification.run_id,
                qualification.head,
                qualification.diff_manifest_json,
                qualification.tree_manifest_json,
                qualification.checks_json,
                i64::try_from(qualification.duration_ms).map_err(|_| StoreError::Invalid(
                    "candidate qualification duration is out of range".to_owned()
                ))?,
                qualification.qualified_at,
            ],
        )?;
        append_gardener_run_event(
            &transaction,
            &qualification.run_id,
            "candidate_qualified",
            now,
            json!({
                "head": qualification.head,
                "duration_ms": qualification.duration_ms,
                "diff_manifest_digest": json_digest(&qualification.diff_manifest_json),
                "tree_manifest_digest": json_digest(&qualification.tree_manifest_json),
                "checks_digest": json_digest(&qualification.checks_json),
            }),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn gardener_candidate_qualification(
        &self,
        run_id: &str,
    ) -> Result<Option<GardenerCandidateQualification>, StoreError> {
        gardener_candidate_qualification(&self.connection, run_id)
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
        let qualification =
            gardener_candidate_qualification(&transaction, run_id)?.ok_or_else(|| {
                StoreError::Conflict(format!(
                    "run {run_id:?} has no passing candidate qualification"
                ))
            })?;
        if qualification.head != pushed_head {
            return Err(StoreError::Conflict(format!(
                "run {run_id:?} candidate qualification does not match its pushed head"
            )));
        }
        if !candidate_checks_all_passed(&qualification.checks_json)? {
            return Err(StoreError::Conflict(format!(
                "run {run_id:?} candidate checks did not all pass"
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
    /// Record the exact identity of a newly created draft pull request.
    pub fn record_gardener_draft_pull_request(
        &mut self,
        claim: &Claim,
        run_id: &str,
        number: u64,
        url: &str,
        head: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        validate_pull_request_identity(number, url, head)?;
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
                 pull_request_recorded_at = ?5, phase = 'pull_request_ready',
                 publication_state = 'draft', updated_at = ?5
             WHERE id = ?1",
            params![run_id, number as i64, url, head, now],
        )?;
        append_gardener_run_event(
            &transaction,
            run_id,
            "pull_request_draft_recorded",
            now,
            json!({"number": number, "url": url, "head": head}),
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Backwards-compatible name retained for callers compiled against P1.
    /// P2 always records the pull request as a draft at this boundary.
    pub fn record_gardener_ready_pull_request(
        &mut self,
        claim: &Claim,
        run_id: &str,
        number: u64,
        url: &str,
        head: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        self.record_gardener_draft_pull_request(claim, run_id, number, url, head, now)
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
        if run.publication_state != GardenerPublicationState::Draft {
            return Err(StoreError::Conflict(format!(
                "run {run_id:?} verification must start from a recorded draft pull request"
            )));
        }
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
        if summary.chars().count() > MAX_GARDENER_MODEL_TEXT_CHARS {
            return Err(StoreError::Invalid(
                "verification summary must be at most 16384 characters".to_owned(),
            ));
        }
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
        let update = if verdict == GardenerVerificationVerdict::Pass {
            "UPDATE gardener_implementation_runs
             SET verification_verdict = ?2, verification_reported_head = ?3,
                 verification_summary = ?4, verification_finished_at = ?5,
                 phase = 'verification_finished', publication_state = 'ready_pending',
                 updated_at = ?5 WHERE id = ?1"
        } else {
            "UPDATE gardener_implementation_runs
             SET verification_verdict = ?2, verification_reported_head = ?3,
                 verification_summary = ?4, verification_finished_at = ?5,
                 phase = 'verification_finished', updated_at = ?5 WHERE id = ?1"
        };
        transaction.execute(
            update,
            params![run_id, verdict.to_string(), reported_head, summary, now],
        )?;
        append_gardener_run_event(
            &transaction,
            run_id,
            "verification_finished",
            now,
            json!({"verdict": verdict, "reported_head": reported_head, "summary": summary}),
        )?;
        if verdict == GardenerVerificationVerdict::Pass {
            append_gardener_run_event(
                &transaction,
                run_id,
                "pull_request_ready_requested",
                now,
                json!({"head": reported_head}),
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Persist the intent to promote a verified draft before the credentialed
    /// GitHub effect occurs. A crash leaves a visible state to reconcile.
    pub fn request_gardener_pull_request_ready(
        &mut self,
        claim: &Claim,
        run_id: &str,
        head: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        validate_hex_identity("pull-request head", head, &[40, 64])?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = require_current_gardener_run(&transaction, claim, run_id, now)?;
        if run.publication_state == GardenerPublicationState::ReadyPending
            && run.pull_request_head.as_deref() == Some(head)
            && run.verification_verdict == Some(GardenerVerificationVerdict::Pass)
        {
            transaction.commit()?;
            return Ok(());
        }
        Err(StoreError::Conflict(format!(
            "run {run_id:?} cannot promote without a persisted passing exact-head verification intent"
        )))
    }

    /// Record the independently re-observed ready identity after GitHub has
    /// accepted the promotion.
    pub fn record_gardener_pull_request_ready(
        &mut self,
        claim: &Claim,
        run_id: &str,
        number: u64,
        url: &str,
        head: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        validate_pull_request_identity(number, url, head)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = require_current_gardener_run(&transaction, claim, run_id, now)?;
        if let Some((existing_number, existing_url, existing_head, _)) =
            gardener_ready_observation(&transaction, run_id)?
        {
            if existing_number == number && existing_url == url && existing_head == head {
                transaction.commit()?;
                return Ok(());
            }
            return Err(StoreError::Conflict(format!(
                "run {run_id:?} is ready with a different pull-request identity"
            )));
        }
        if run.publication_state != GardenerPublicationState::ReadyPending
            || run.pull_request_number != Some(number)
            || run.pull_request_url.as_deref() != Some(url)
            || run.pull_request_head.as_deref() != Some(head)
            || run.verification_verdict != Some(GardenerVerificationVerdict::Pass)
        {
            return Err(StoreError::Conflict(format!(
                "run {run_id:?} ready observation does not match its verified draft"
            )));
        }
        transaction.execute(
            "INSERT INTO gardener_pull_request_ready_observations(run_id, number, url, head, ready_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![run_id, number as i64, url, head, now],
        )?;
        append_gardener_run_event(
            &transaction,
            run_id,
            "pull_request_ready_recorded",
            now,
            json!({"number": number, "url": url, "head": head}),
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
        let superseded_gardener_instance: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1
                FROM gardener_proposal_instances pi
                JOIN gardener_proposal_instance_supersessions s
                  ON s.superseded_instance_id = pi.id
                WHERE pi.implementation_obligation_id = ?1
             )",
            [&claim.obligation_id],
            |row| row.get(0),
        )?;
        if superseded_gardener_instance {
            return Err(StoreError::Fenced);
        }
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
        validate_completion(&completion)?;
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
        self.retry_attention_inner(id, None, now)
    }

    pub fn retry_attention_if_current(
        &mut self,
        id: &str,
        precondition: &ActionPrecondition,
        now: i64,
    ) -> Result<(), StoreError> {
        self.retry_attention_inner(id, Some(precondition), now)
    }

    fn retry_attention_inner(
        &mut self,
        id: &str,
        precondition: Option<&ActionPrecondition>,
        now: i64,
    ) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if proposal_instance_for_obligation(&transaction, id)?
            .is_some_and(|instance| instance.superseded_by.is_some())
        {
            return Err(StoreError::Conflict(format!(
                "superseded gardener proposal instance obligation {id:?} cannot be retried"
            )));
        }
        if let Some(precondition) = precondition {
            validate_action_precondition(&transaction, id, precondition, None, None)?;
        }
        apply_transition(&transaction, Transition::RetryAttention { id, now })?;
        transaction.commit()?;
        Ok(())
    }

    pub fn cancel(&mut self, id: &str, now: i64) -> Result<(), StoreError> {
        self.cancel_inner(id, None, now)
    }

    pub fn cancel_if_current(
        &mut self,
        id: &str,
        precondition: &ActionPrecondition,
        now: i64,
    ) -> Result<(), StoreError> {
        self.cancel_inner(id, Some(precondition), now)
    }

    fn cancel_inner(
        &mut self,
        id: &str,
        precondition: Option<&ActionPrecondition>,
        now: i64,
    ) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(precondition) = precondition {
            validate_action_precondition(&transaction, id, precondition, None, None)?;
        }
        apply_transition(&transaction, Transition::Cancel { id, now })?;
        transaction.commit()?;
        Ok(())
    }

    pub fn attempts(&self, id: &str) -> Result<Vec<Attempt>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id, obligation_id, occurrence, attempt_number, lease_generation,
                    lease_token, claimed_at, completed_at, outcome, retryable,
                    failure_disposition, error, evidence
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
    if inspection.id.trim().is_empty() || inspection.id.chars().count() > 160 {
        return Err(StoreError::Invalid(
            "inspection id must be non-empty and at most 160 characters".to_owned(),
        ));
    }
    validate_hex_identity("source commit", &inspection.source_commit, &[40, 64])?;
    if inspection.source_commit != inspection.source_commit.to_ascii_lowercase() {
        return Err(StoreError::Invalid(
            "source commit must use canonical lowercase hexadecimal".to_owned(),
        ));
    }
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

const MAX_GARDENER_MANIFEST_BYTES: usize = 1024 * 1024;

fn validate_json_array(name: &str, value: &str) -> Result<serde_json::Value, StoreError> {
    if value.len() > MAX_GARDENER_MANIFEST_BYTES {
        return Err(StoreError::Invalid(format!(
            "{name} exceeds {MAX_GARDENER_MANIFEST_BYTES} bytes"
        )));
    }
    let parsed: serde_json::Value = serde_json::from_str(value)
        .map_err(|error| StoreError::Invalid(format!("{name} is invalid JSON: {error}")))?;
    if !parsed.is_array() {
        return Err(StoreError::Invalid(format!("{name} must be a JSON array")));
    }
    Ok(parsed)
}

fn validate_optional_bounded(name: &str, value: Option<&str>) -> Result<(), StoreError> {
    if value.is_some_and(|value| value.trim().is_empty() || value.len() > 256) {
        return Err(StoreError::Invalid(format!(
            "{name} must be non-empty and at most 256 bytes when present"
        )));
    }
    Ok(())
}

fn validate_reproducibility_manifest(
    manifest: &GardenerReproducibilityManifest,
) -> Result<(), StoreError> {
    validate_nonempty("reproducibility run id", &manifest.run_id)?;
    if manifest.bokkie_build.trim().is_empty() || manifest.bokkie_build.len() > 256 {
        return Err(StoreError::Invalid(
            "Bokkie build identity must be non-empty and at most 256 bytes".to_owned(),
        ));
    }
    validate_hex_identity(
        "reproducibility source commit",
        &manifest.source_commit,
        &[40, 64],
    )?;
    validate_hex_identity("prompt digest", &manifest.prompt_digest, &[64])?;
    validate_hex_identity(
        "implementation schema digest",
        &manifest.implementation_schema_digest,
        &[64],
    )?;
    validate_hex_identity(
        "verification schema digest",
        &manifest.verification_schema_digest,
        &[64],
    )?;
    validate_hex_identity(
        "sandbox policy digest",
        &manifest.sandbox_policy_digest,
        &[64],
    )?;
    validate_hex_identity(
        "environment policy digest",
        &manifest.environment_policy_digest,
        &[64],
    )?;
    validate_optional_bounded("Codex profile", manifest.codex_profile.as_deref())?;
    validate_optional_bounded("Codex model", manifest.codex_model.as_deref())?;
    let executables =
        validate_json_array("executable manifest", &manifest.executable_manifest_json)?;
    if executables.as_array().is_none_or(Vec::is_empty) {
        return Err(StoreError::Invalid(
            "executable manifest must identify at least one executable".to_owned(),
        ));
    }
    let checks = validate_json_array("check commands", &manifest.check_commands_json)?;
    if checks.as_array().is_none_or(Vec::is_empty) {
        return Err(StoreError::Invalid(
            "check commands must not be empty".to_owned(),
        ));
    }
    Ok(())
}

fn validate_candidate_qualification(
    qualification: &GardenerCandidateQualification,
) -> Result<(), StoreError> {
    validate_nonempty("candidate qualification run id", &qualification.run_id)?;
    validate_hex_identity(
        "candidate qualification head",
        &qualification.head,
        &[40, 64],
    )?;
    validate_json_array("candidate diff manifest", &qualification.diff_manifest_json)?;
    validate_json_array("candidate tree manifest", &qualification.tree_manifest_json)?;
    let checks = validate_json_array("candidate checks", &qualification.checks_json)?;
    let checks = checks.as_array().expect("array was validated");
    if checks.is_empty()
        || checks.iter().any(|check| {
            check
                .get("executable")
                .is_none_or(|executable| !executable.is_object())
                || check
                    .get("arguments")
                    .is_none_or(|arguments| !arguments.is_array())
                || check
                    .get("duration_millis")
                    .and_then(serde_json::Value::as_u64)
                    .is_none()
                || check.get("status").is_none_or(|status| {
                    !matches!(
                        status.get("kind").and_then(serde_json::Value::as_str),
                        Some("passed" | "failed" | "interrupted")
                    )
                })
                || check
                    .get("evidence")
                    .is_none_or(|evidence| !evidence.is_object())
        })
    {
        return Err(StoreError::Invalid(
            "candidate checks must be a non-empty array of typed bounded results".to_owned(),
        ));
    }
    Ok(())
}

fn candidate_checks_all_passed(value: &str) -> Result<bool, StoreError> {
    let checks = validate_json_array("candidate checks", value)?;
    Ok(checks.as_array().is_some_and(|checks| {
        !checks.is_empty()
            && checks.iter().all(|check| {
                check
                    .pointer("/status/kind")
                    .and_then(serde_json::Value::as_str)
                    == Some("passed")
            })
    }))
}

fn validate_pull_request_identity(number: u64, url: &str, head: &str) -> Result<(), StoreError> {
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
    validate_hex_identity("pull-request head", head, &[40, 64])
}

fn json_digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn validate_inspection_result(result: &InspectionResult) -> Result<(), StoreError> {
    if result.summary.trim().is_empty()
        || result.summary.chars().count() > MAX_GARDENER_MODEL_TEXT_CHARS
    {
        return Err(StoreError::Invalid(
            "inspection result summary must be non-empty and at most 16384 characters".to_owned(),
        ));
    }
    if result.proposed_goal_prompts.len() > MAX_GARDENER_PROMPTS {
        return Err(StoreError::Invalid(
            "inspection result may contain at most three proposed goal prompts".to_owned(),
        ));
    }
    if result.proposed_goal_prompts.iter().any(|prompt| {
        prompt.trim().is_empty() || prompt.chars().count() > MAX_GARDENER_PROMPT_CHARS
    }) {
        return Err(StoreError::Invalid(
            "inspection goal prompts must be non-empty and at most 6000 characters".to_owned(),
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
            gardener_inspection_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

fn gardener_inspection_from_row(row: &Row<'_>) -> rusqlite::Result<GardenerInspection> {
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
    if new.id.chars().count() > 160 {
        return Err(StoreError::Invalid(
            "implementation run id must be at most 160 characters".to_owned(),
        ));
    }
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
    if message.len() > MAX_GARDENER_MODEL_MESSAGE_BYTES {
        return Err(StoreError::Invalid(
            "implementation final message must be at most 262144 bytes".to_owned(),
        ));
    }
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
    let object = value.as_object().expect("object was checked");
    let summary = object.get("summary").and_then(serde_json::Value::as_str);
    let bounded_list = |name: &str| {
        object.get(name).is_some_and(|value| {
            value.as_array().is_some_and(|items| {
                items.len() <= MAX_GARDENER_MODEL_ITEMS
                    && items.iter().all(|item| {
                        item.as_str().is_some_and(|item| {
                            !item.trim().is_empty()
                                && item.chars().count() <= MAX_GARDENER_MODEL_ITEM_CHARS
                        })
                    })
            })
        })
    };
    if summary.is_none_or(|summary| {
        summary.trim().is_empty() || summary.chars().count() > MAX_GARDENER_MODEL_TEXT_CHARS
    }) || !bounded_list("changed_paths")
        || !bounded_list("checks")
    {
        return Err(StoreError::Invalid(
            "implementation final message exceeds the typed field bounds".to_owned(),
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
            "SELECT r.*,
                    ri.instance_id AS exact_proposal_instance_id,
                    ri.generation AS exact_proposal_generation,
                    pi.source_observation_id AS exact_source_observation_id,
                    pi.source_inspection_id AS exact_source_inspection_id,
                    CASE WHEN ready.run_id IS NULL THEN r.publication_state ELSE 'ready' END
                        AS effective_publication_state,
                    ready.ready_at AS effective_pull_request_ready_at
             FROM gardener_implementation_runs r
             JOIN gardener_implementation_run_instances ri ON ri.run_id = r.id
             JOIN gardener_proposal_instances pi ON pi.id = ri.instance_id
             LEFT JOIN gardener_pull_request_ready_observations ready ON ready.run_id = r.id
             WHERE r.id = ?1",
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
            "SELECT r.*,
                    ri.instance_id AS exact_proposal_instance_id,
                    ri.generation AS exact_proposal_generation,
                    pi.source_observation_id AS exact_source_observation_id,
                    pi.source_inspection_id AS exact_source_inspection_id,
                    CASE WHEN ready.run_id IS NULL THEN r.publication_state ELSE 'ready' END
                        AS effective_publication_state,
                    ready.ready_at AS effective_pull_request_ready_at
             FROM gardener_implementation_runs r
             JOIN gardener_implementation_run_instances ri ON ri.run_id = r.id
             JOIN gardener_proposal_instances pi ON pi.id = ri.instance_id
             LEFT JOIN gardener_pull_request_ready_observations ready ON ready.run_id = r.id
             WHERE r.obligation_id = ?1 AND r.lease_generation = ?2",
            params![obligation_id, lease_generation],
            gardener_implementation_run_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

fn gardener_reproducibility_manifest(
    connection: &Connection,
    run_id: &str,
) -> Result<Option<GardenerReproducibilityManifest>, StoreError> {
    connection
        .query_row(
            "SELECT run_id, bokkie_build, source_commit, prompt_digest,
                    implementation_schema_digest, verification_schema_digest,
                    codex_profile, codex_model, executable_manifest_json,
                    sandbox_policy_digest, environment_policy_digest,
                    check_commands_json, recorded_at
             FROM gardener_run_reproducibility WHERE run_id = ?1",
            [run_id],
            |row| {
                Ok(GardenerReproducibilityManifest {
                    run_id: row.get(0)?,
                    bokkie_build: row.get(1)?,
                    source_commit: row.get(2)?,
                    prompt_digest: row.get(3)?,
                    implementation_schema_digest: row.get(4)?,
                    verification_schema_digest: row.get(5)?,
                    codex_profile: row.get(6)?,
                    codex_model: row.get(7)?,
                    executable_manifest_json: row.get(8)?,
                    sandbox_policy_digest: row.get(9)?,
                    environment_policy_digest: row.get(10)?,
                    check_commands_json: row.get(11)?,
                    recorded_at: row.get(12)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::from)
}

fn gardener_candidate_qualification(
    connection: &Connection,
    run_id: &str,
) -> Result<Option<GardenerCandidateQualification>, StoreError> {
    connection
        .query_row(
            "SELECT run_id, head, diff_manifest_json, tree_manifest_json,
                    checks_json, duration_ms, qualified_at
             FROM gardener_candidate_qualifications WHERE run_id = ?1",
            [run_id],
            |row| {
                let duration: i64 = row.get(5)?;
                Ok(GardenerCandidateQualification {
                    run_id: row.get(0)?,
                    head: row.get(1)?,
                    diff_manifest_json: row.get(2)?,
                    tree_manifest_json: row.get(3)?,
                    checks_json: row.get(4)?,
                    duration_ms: u64::try_from(duration).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(5, Type::Integer, Box::new(error))
                    })?,
                    qualified_at: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::from)
}

fn gardener_ready_observation(
    connection: &Connection,
    run_id: &str,
) -> Result<Option<(u64, String, String, i64)>, StoreError> {
    connection
        .query_row(
            "SELECT number, url, head, ready_at
             FROM gardener_pull_request_ready_observations WHERE run_id = ?1",
            [run_id],
            |row| {
                let number: i64 = row.get(0)?;
                Ok((
                    u64::try_from(number).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(0, Type::Integer, Box::new(error))
                    })?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                ))
            },
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
        proposal_instance_id: row.get("exact_proposal_instance_id")?,
        proposal_generation: row.get("exact_proposal_generation")?,
        source_observation_id: row.get("exact_source_observation_id")?,
        source_inspection_id: row.get("exact_source_inspection_id")?,
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
        publication_state: parse_column(row, "effective_publication_state")?,
        pull_request_ready_at: row.get("effective_pull_request_ready_at")?,
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
    let superseded: bool = transaction.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM gardener_proposal_instance_supersessions
            WHERE superseded_instance_id = ?1
         )",
        [&run.proposal_instance_id],
        |row| row.get(0),
    )?;
    if superseded {
        return Err(StoreError::Fenced);
    }
    let has_exact_authority: bool = transaction.query_row(
        "SELECT coalesce((
            SELECT d.decision = 'approved' AND a.decision = 'approved'
            FROM gardener_proposal_instance_decisions d
            JOIN approvals a ON a.id = d.approval_id
            WHERE d.instance_id = ?1 AND d.obligation_id = ?2
              AND d.occurrence = ?3
            ORDER BY d.approval_id DESC LIMIT 1
         ), 0)",
        params![run.proposal_instance_id, run.obligation_id, run.occurrence],
        |row| row.get(0),
    )?;
    if !has_exact_authority {
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
            "SELECT p.fingerprint, p.repository, p.prompt, pi.implementation_obligation_id,
                    o.state,
                    (SELECT d.decision FROM gardener_proposal_instance_decisions d
                     WHERE d.instance_id = pi.id ORDER BY d.approval_id DESC LIMIT 1),
                    (SELECT COUNT(*) FROM gardener_proposal_observations po
                     WHERE po.proposal_fingerprint = p.fingerprint),
                    p.created_at
             FROM gardener_proposals p
             JOIN gardener_proposal_instances pi
               ON pi.proposal_fingerprint = p.fingerprint
              AND pi.generation = (SELECT max(current.generation)
                                   FROM gardener_proposal_instances current
                                   WHERE current.proposal_fingerprint = p.fingerprint)
             JOIN obligations o ON o.id = pi.implementation_obligation_id
             WHERE p.fingerprint = ?1",
            [fingerprint],
            proposal_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

fn proposal_from_row(row: &Row<'_>) -> rusqlite::Result<Proposal> {
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
}

const PROPOSAL_INSTANCE_SELECT: &str =
    "SELECT pi.id, pi.proposal_fingerprint, p.repository, p.prompt,
            pi.source_commit, pi.source_observation_id, pi.source_inspection_id,
            pi.generation, pi.implementation_obligation_id, o.state,
            (SELECT d.decision FROM gardener_proposal_instance_decisions d
             WHERE d.instance_id = pi.id ORDER BY d.approval_id DESC LIMIT 1),
            s.superseding_instance_id,
            (SELECT count(*) FROM gardener_proposal_observation_instances oi
             WHERE oi.instance_id = pi.id),
            pi.created_at
     FROM gardener_proposal_instances pi
     JOIN gardener_proposals p ON p.fingerprint = pi.proposal_fingerprint
     JOIN obligations o ON o.id = pi.implementation_obligation_id
     LEFT JOIN gardener_proposal_instance_supersessions s
       ON s.superseded_instance_id = pi.id
     WHERE pi.proposal_fingerprint = ?1
     ORDER BY pi.generation";

fn proposal_instance(
    connection: &Connection,
    instance_id: &str,
) -> Result<Option<ProposalInstance>, StoreError> {
    connection
        .query_row(
            "SELECT pi.id, pi.proposal_fingerprint, p.repository, p.prompt,
                    pi.source_commit, pi.source_observation_id, pi.source_inspection_id,
                    pi.generation, pi.implementation_obligation_id, o.state,
                    (SELECT d.decision FROM gardener_proposal_instance_decisions d
                     WHERE d.instance_id = pi.id ORDER BY d.approval_id DESC LIMIT 1),
                    s.superseding_instance_id,
                    (SELECT count(*) FROM gardener_proposal_observation_instances oi
                     WHERE oi.instance_id = pi.id),
                    pi.created_at
             FROM gardener_proposal_instances pi
             JOIN gardener_proposals p ON p.fingerprint = pi.proposal_fingerprint
             JOIN obligations o ON o.id = pi.implementation_obligation_id
             LEFT JOIN gardener_proposal_instance_supersessions s
               ON s.superseded_instance_id = pi.id
             WHERE pi.id = ?1",
            [instance_id],
            proposal_instance_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

fn proposal_instance_for_source(
    connection: &Connection,
    fingerprint: &str,
    source_commit: &str,
) -> Result<Option<ProposalInstance>, StoreError> {
    let instance_id: Option<String> = connection
        .query_row(
            "SELECT id FROM gardener_proposal_instances
             WHERE proposal_fingerprint = ?1 AND source_commit = ?2",
            params![fingerprint, source_commit],
            |row| row.get(0),
        )
        .optional()?;
    instance_id
        .as_deref()
        .map(|id| proposal_instance(connection, id))
        .transpose()
        .map(Option::flatten)
}

fn proposal_instance_for_obligation(
    connection: &Connection,
    obligation_id: &str,
) -> Result<Option<ProposalInstance>, StoreError> {
    let instance_id: Option<String> = connection
        .query_row(
            "SELECT id FROM gardener_proposal_instances
             WHERE implementation_obligation_id = ?1",
            [obligation_id],
            |row| row.get(0),
        )
        .optional()?;
    instance_id
        .as_deref()
        .map(|id| proposal_instance(connection, id))
        .transpose()
        .map(Option::flatten)
}

fn proposal_instance_from_row(row: &Row<'_>) -> rusqlite::Result<ProposalInstance> {
    let decision: Option<String> = row.get(10)?;
    Ok(ProposalInstance {
        id: row.get(0)?,
        proposal_fingerprint: row.get(1)?,
        repository: row.get(2)?,
        prompt: row.get(3)?,
        source_commit: row.get(4)?,
        source_observation_id: row.get(5)?,
        source_inspection_id: row.get(6)?,
        generation: row.get(7)?,
        implementation_obligation_id: row.get(8)?,
        obligation_state: parse_index(row, 9)?,
        approval_decision: decision.map(|value| parse_value(value, 10)).transpose()?,
        superseded_by: row.get(11)?,
        observation_count: row.get(12)?,
        created_at: row.get(13)?,
    })
}

#[allow(clippy::too_many_arguments)]
fn create_proposal_instance(
    transaction: &Transaction<'_>,
    fingerprint: &str,
    repository: &str,
    source_commit: &str,
    source_observation_id: i64,
    source_inspection_id: &str,
    now: i64,
) -> Result<ProposalInstance, StoreError> {
    let generation: u32 = transaction.query_row(
        "SELECT coalesce(max(generation), 0) + 1
         FROM gardener_proposal_instances WHERE proposal_fingerprint = ?1",
        [fingerprint],
        |row| row.get(0),
    )?;
    let obligation_id = if generation == 1 {
        transaction.query_row(
            "SELECT implementation_obligation_id FROM gardener_proposals
             WHERE fingerprint = ?1",
            [fingerprint],
            |row| row.get(0),
        )?
    } else {
        let id = format!("gardener:implement:{fingerprint}:g{generation}");
        apply_transition(
            transaction,
            Transition::Create {
                new: NewObligation {
                    id: id.clone(),
                    description: format!(
                        "Implement approved gardener proposal {fingerprint} generation {generation}"
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
            params![id, now],
        )?;
        id
    };
    let instance_id = proposal_instance_id(fingerprint, source_commit, generation);
    transaction.execute(
        "INSERT INTO gardener_proposal_instances(
            id, proposal_fingerprint, source_commit, source_observation_id,
            source_inspection_id, generation, implementation_obligation_id, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            instance_id,
            fingerprint,
            source_commit,
            source_observation_id,
            source_inspection_id,
            generation,
            obligation_id,
            now,
        ],
    )?;

    if generation > 1 {
        let previous = transaction.query_row(
            "SELECT id, implementation_obligation_id
             FROM gardener_proposal_instances
             WHERE proposal_fingerprint = ?1 AND generation = ?2",
            params![fingerprint, generation - 1],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        transaction.execute(
            "INSERT INTO gardener_proposal_instance_supersessions(
                superseded_instance_id, superseding_instance_id, occurred_at
             ) VALUES (?1, ?2, ?3)",
            params![previous.0, instance_id, now],
        )?;
        let previous_obligation = require_obligation(transaction, &previous.1)?;
        if cancel_transition_is_legal(&previous_obligation)
            && !previous_obligation.state.is_terminal()
        {
            apply_transition(
                transaction,
                Transition::Cancel {
                    id: &previous.1,
                    now,
                },
            )?;
        }
        append_gardener_event(
            transaction,
            repository,
            Some(source_inspection_id),
            Some(fingerprint),
            "proposal_instance_superseded",
            now,
            json!({
                "proposal_instance_id": previous.0,
                "superseding_instance_id": instance_id,
                "source_commit": source_commit,
                "generation": generation
            }),
        )?;
    }
    append_gardener_event(
        transaction,
        repository,
        Some(source_inspection_id),
        Some(fingerprint),
        "proposal_instance_created",
        now,
        json!({
            "proposal_instance_id": instance_id,
            "source_commit": source_commit,
            "source_observation_id": source_observation_id,
            "source_inspection_id": source_inspection_id,
            "generation": generation,
            "implementation_obligation_id": obligation_id
        }),
    )?;
    proposal_instance(transaction, &instance_id)?
        .ok_or_else(|| StoreError::Conflict("new proposal instance was not readable".to_owned()))
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
    validate_bounded_text(
        "audit event type",
        event_type,
        MAX_AUDIT_EVENT_TYPE_CHARS,
        false,
    )?;
    let details = serialise_audit_details(details)?;
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
            details
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
    validate_bounded_text(
        "audit event type",
        event_type,
        MAX_AUDIT_EVENT_TYPE_CHARS,
        false,
    )?;
    let details = serialise_audit_details(details)?;
    transaction.execute(
        "INSERT INTO gardener_run_events(run_id, event_type, occurred_at, details_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![run_id, event_type, now, details],
    )?;
    Ok(())
}

struct GardenerDecisionContext<'a> {
    instance_id: &'a str,
    proposal_fingerprint: &'a str,
    source_commit: &'a str,
    source_observation_id: i64,
    source_inspection_id: &'a str,
    generation: u32,
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
        gardener_instance: Option<GardenerDecisionContext<'a>>,
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

pub(crate) fn approval_transition_is_legal(obligation: &Obligation) -> bool {
    obligation.state == ObligationState::AwaitingApproval
}

pub(crate) fn generic_approval_transition_is_legal(
    obligation: &Obligation,
    is_gardener_proposal: bool,
) -> bool {
    approval_transition_is_legal(obligation) && !is_gardener_proposal
}

pub(crate) fn gardener_proposal_transition_is_legal(
    obligation: &Obligation,
    is_gardener_proposal: bool,
) -> bool {
    approval_transition_is_legal(obligation) && is_gardener_proposal
}

pub(crate) fn retry_transition_is_legal(obligation: &Obligation) -> bool {
    obligation.state == ObligationState::Attention
}

pub(crate) fn cancel_transition_is_legal(obligation: &Obligation) -> bool {
    !obligation.state.is_terminal() && obligation.state != ObligationState::Running
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
            gardener_instance,
            now,
        } => {
            let obligation = require_obligation(transaction, id)?;
            if !approval_transition_is_legal(&obligation) {
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
            let details = gardener_instance.map_or_else(
                || json!({"actor": actor, "note": note}),
                |instance| {
                    json!({
                        "actor": actor,
                        "note": note,
                        "proposal_instance_id": instance.instance_id,
                        "proposal_fingerprint": instance.proposal_fingerprint,
                        "source_commit": instance.source_commit,
                        "source_observation_id": instance.source_observation_id,
                        "source_inspection_id": instance.source_inspection_id,
                        "generation": instance.generation
                    })
                },
            );
            append_event(
                transaction,
                id,
                obligation.occurrence,
                &decision.to_string(),
                now,
                Some(obligation.state),
                next_state,
                details,
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
                    lease_expires_at = ?5, failure_disposition = NULL, updated_at = ?6
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
            // A heartbeat grants a bounded horizon from the heartbeat itself.
            // It must not accumulate unused time from a previous lease.
            let lease_expires_at = now.saturating_add(lease_seconds);
            if obligation.lease_expires_at == Some(lease_expires_at) {
                return Ok(TransitionResult {
                    claim: None,
                    lease_expires_at: Some(lease_expires_at),
                });
            }
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
                        AttemptCompletion {
                            outcome: AttemptOutcome::Succeeded,
                            retryable: None,
                            disposition: None,
                            error: None,
                            evidence: evidence.as_deref(),
                        },
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
                                        last_error = NULL, last_evidence = ?5,
                                        failure_disposition = NULL, updated_at = ?6
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
                    disposition,
                    error,
                    evidence,
                } => {
                    finish_attempt(
                        transaction,
                        claim,
                        now,
                        AttemptCompletion {
                            outcome: AttemptOutcome::Failed,
                            retryable: Some(disposition.legacy_retryable()),
                            disposition: Some(disposition),
                            error: Some(&error),
                            evidence: evidence.as_deref(),
                        },
                    )?;
                    schedule_failure(
                        transaction,
                        &obligation,
                        now,
                        disposition,
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
                    failure_disposition = 'retry_safe', error = 'lease expired before completion'
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
                FailureDisposition::RetrySafe,
                "lease expired before completion",
                None,
                "lease_expired",
            )?;
        }
        Transition::RetryAttention { id, now } => {
            let obligation = require_obligation(transaction, id)?;
            if !retry_transition_is_legal(&obligation) {
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
                    last_error = NULL, failure_disposition = NULL,
                    updated_at = ?4 WHERE id = ?1",
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
            if !cancel_transition_is_legal(&obligation) {
                return Err(StoreError::Conflict(
                    "cannot cancel while a runner owns an active claim".to_owned(),
                ));
            }
            transaction.execute(
                "UPDATE obligations SET state = 'cancelled', next_wake_at = NULL,
                    lease_token = NULL, lease_expires_at = NULL,
                    failure_disposition = 'cancelled', updated_at = ?2 WHERE id = ?1",
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
                json!({"failure_disposition": FailureDisposition::Cancelled}),
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
            last_evidence = ?2, failure_disposition = NULL, updated_at = ?3 WHERE id = ?1",
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
            last_evidence = ?3, failure_disposition = 'needs_reconciliation',
            updated_at = ?4 WHERE id = ?1",
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
        json!({
            "error": error,
            "evidence": evidence,
            "failure_disposition": FailureDisposition::NeedsReconciliation
        }),
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
    disposition: FailureDisposition,
    error: &str,
    evidence: Option<&str>,
    event_prefix: &str,
) -> Result<(), StoreError> {
    let can_retry =
        disposition.is_retry_safe() && obligation.attempts_made < obligation.max_attempts;
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
            last_evidence = ?5, failure_disposition = ?6, updated_at = ?7 WHERE id = ?1",
        params![
            obligation.id,
            next_state.to_string(),
            next_wake,
            error,
            evidence,
            disposition.to_string(),
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
            "evidence": evidence,
            "failure_disposition": disposition
        }),
    )?;
    Ok(())
}

struct AttemptCompletion<'a> {
    outcome: AttemptOutcome,
    retryable: Option<bool>,
    disposition: Option<FailureDisposition>,
    error: Option<&'a str>,
    evidence: Option<&'a str>,
}

fn finish_attempt(
    transaction: &Transaction<'_>,
    claim: &Claim,
    now: i64,
    completion: AttemptCompletion<'_>,
) -> Result<(), StoreError> {
    let changed = transaction.execute(
        "UPDATE attempts SET completed_at = ?3, outcome = ?4, retryable = ?5,
            failure_disposition = ?6, error = ?7, evidence = ?8
         WHERE obligation_id = ?1 AND lease_generation = ?2 AND completed_at IS NULL",
        params![
            claim.obligation_id,
            claim.lease_generation,
            now,
            completion.outcome.to_string(),
            completion.retryable,
            completion.disposition.map(|value| value.to_string()),
            completion.error,
            completion.evidence
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

/// Validate the exact operator state inside the same IMMEDIATE transaction as
/// the requested mutation. Every obligation mutation in this store appends an
/// audit event atomically, making the latest append-only sequence a durable,
/// monotonic state revision even when timestamps, state and occurrence repeat.
fn validate_action_precondition(
    transaction: &Transaction<'_>,
    obligation_id: &str,
    precondition: &ActionPrecondition,
    gardener_fingerprint: Option<&str>,
    exact_gardener_instance: Option<&ProposalInstance>,
) -> Result<(), StoreError> {
    if precondition.obligation_id != obligation_id {
        return Err(StoreError::Conflict(format!(
            "action targets obligation {obligation_id:?}, but the reviewed precondition targets {:?}",
            precondition.obligation_id
        )));
    }
    if precondition.gardener_fingerprint.as_deref() != gardener_fingerprint {
        return Err(StoreError::Conflict(
            "action does not match the reviewed gardener proposal fingerprint".to_owned(),
        ));
    }
    if let Some(instance) = exact_gardener_instance {
        let exact_matches = precondition.gardener_proposal_instance_id.as_deref()
            == Some(instance.id.as_str())
            && precondition.gardener_source_commit.as_deref()
                == Some(instance.source_commit.as_str())
            && precondition.gardener_source_observation_id == Some(instance.source_observation_id)
            && precondition.gardener_source_inspection_id.as_deref()
                == Some(instance.source_inspection_id.as_str())
            && precondition.gardener_generation == Some(instance.generation);
        if !exact_matches {
            return Err(StoreError::Conflict(
                "action does not match the reviewed source-bound gardener proposal instance"
                    .to_owned(),
            ));
        }
    }
    let obligation = require_obligation(transaction, obligation_id)?;
    let state_revision: i64 = transaction.query_row(
        "SELECT sequence FROM audit_events
         WHERE obligation_id = ?1 ORDER BY sequence DESC LIMIT 1",
        [obligation_id],
        |row| row.get(0),
    )?;
    if obligation.occurrence != precondition.occurrence
        || state_revision != precondition.state_revision
    {
        return Err(StoreError::Conflict(format!(
            "reviewed obligation state is stale (expected occurrence {} at revision {}, found occurrence {} at revision {})",
            precondition.occurrence,
            precondition.state_revision,
            obligation.occurrence,
            state_revision
        )));
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
    validate_bounded_text(
        "audit event type",
        event_type,
        MAX_AUDIT_EVENT_TYPE_CHARS,
        false,
    )?;
    let details = serialise_audit_details(details)?;
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
            details
        ],
    )?;
    Ok(())
}

fn serialise_audit_details(details: serde_json::Value) -> Result<String, StoreError> {
    fn contains_nul(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::String(value) => value.contains('\0'),
            serde_json::Value::Array(values) => values.iter().any(contains_nul),
            serde_json::Value::Object(values) => values
                .iter()
                .any(|(key, value)| key.contains('\0') || contains_nul(value)),
            _ => false,
        }
    }
    if contains_nul(&details) {
        return Err(StoreError::Invalid(
            "audit event details must not contain NUL".to_owned(),
        ));
    }
    let details = details.to_string();
    if details.len() > MAX_AUDIT_DETAILS_BYTES {
        return Err(StoreError::Invalid(format!(
            "audit event details must be at most {MAX_AUDIT_DETAILS_BYTES} bytes"
        )));
    }
    Ok(details)
}

fn validate_new(new: &NewObligation) -> Result<(), StoreError> {
    validate_bounded_text("id", &new.id, MAX_OBLIGATION_ID_CHARS, false)?;
    validate_bounded_text(
        "description",
        &new.description,
        MAX_OBLIGATION_DESCRIPTION_CHARS,
        false,
    )?;
    if let Some(recurrence) = &new.recurrence {
        validate_bounded_text(
            "recurrence expression",
            recurrence.expression(),
            MAX_RECURRENCE_EXPRESSION_CHARS,
            false,
        )?;
        validate_bounded_text(
            "recurrence timezone",
            recurrence.timezone(),
            MAX_RECURRENCE_TIMEZONE_CHARS,
            false,
        )?;
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

fn validate_completion(completion: &Completion) -> Result<(), StoreError> {
    match completion {
        Completion::Succeeded { evidence } => {
            if let Some(evidence) = evidence {
                validate_bounded_text(
                    "completion evidence",
                    evidence,
                    MAX_COMPLETION_EVIDENCE_CHARS,
                    true,
                )?;
            }
        }
        Completion::Failed {
            error, evidence, ..
        } => {
            validate_bounded_text("completion error", error, MAX_COMPLETION_ERROR_CHARS, false)?;
            if let Some(evidence) = evidence {
                validate_bounded_text(
                    "completion evidence",
                    evidence,
                    MAX_COMPLETION_EVIDENCE_CHARS,
                    true,
                )?;
            }
        }
    }
    Ok(())
}

fn validate_bounded_text(
    name: &str,
    value: &str,
    max_chars: usize,
    allow_empty: bool,
) -> Result<(), StoreError> {
    if value.contains('\0') {
        return Err(StoreError::Invalid(format!("{name} must not contain NUL")));
    }
    if (!allow_empty && value.trim().is_empty()) || value.chars().count() > max_chars {
        let requirement = if allow_empty { "" } else { "non-empty and " };
        return Err(StoreError::Invalid(format!(
            "{name} must be {requirement}at most {max_chars} Unicode characters"
        )));
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
        failure_disposition: parse_optional_column(row, "failure_disposition")?,
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
        failure_disposition: parse_optional_index(row, 10)?,
        error: row.get(11)?,
        evidence: row.get(12)?,
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

fn parse_optional_column<T>(row: &Row<'_>, name: &str) -> rusqlite::Result<Option<T>>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    let index = row.as_ref().column_index(name)?;
    let value: Option<String> = row.get(index)?;
    value.map(|value| parse_value(value, index)).transpose()
}

fn parse_optional_index<T>(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<T>>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    let value: Option<String> = row.get(index)?;
    value.map(|value| parse_value(value, index)).transpose()
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
    use std::collections::BTreeSet;

    use proptest::prelude::*;
    use proptest::test_runner::RngSeed;
    use tempfile::TempDir;

    use super::*;
    use crate::migrations::MIGRATIONS;
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

    fn assert_projection_agrees_with_latest_event(store: &Store, id: &str) {
        let obligation = store.get(id).unwrap().unwrap();
        let events = store.events(id).unwrap();
        assert_eq!(events.last().unwrap().to_state, obligation.state);
        if !obligation.state.is_terminal() {
            assert!(
                obligation.next_wake_at.is_some()
                    || (obligation.state == ObligationState::Running
                        && obligation.lease_token.is_some()
                        && obligation.lease_expires_at.is_some())
                    || matches!(
                        obligation.state,
                        ObligationState::AwaitingApproval | ObligationState::Attention
                    ),
                "non-terminal obligation {id:?} lost durable liveness: {obligation:?}"
            );
        }
    }

    #[test]
    fn lifecycle_text_boundaries_count_unicode_and_reject_nul() {
        let mut store = Store::open_in_memory().unwrap();
        let mut exact = one_off(&"🦘".repeat(MAX_OBLIGATION_ID_CHARS), 10);
        exact.description = "界".repeat(MAX_OBLIGATION_DESCRIPTION_CHARS);
        store.create(exact, 1).unwrap();

        let too_long = one_off(&"🦘".repeat(MAX_OBLIGATION_ID_CHARS + 1), 10);
        assert!(matches!(
            store.create(too_long, 1),
            Err(StoreError::Invalid(_))
        ));
        let nul = one_off("bad\0id", 10);
        assert!(matches!(store.create(nul, 1), Err(StoreError::Invalid(_))));

        let approval = one_off("approval-limits", 10);
        let approval = NewObligation {
            approval_required: true,
            ..approval
        };
        store.create(approval, 1).unwrap();
        store
            .decide_approval(
                "approval-limits",
                ApprovalDecision::Approved,
                &"人".repeat(MAX_APPROVAL_ACTOR_CHARS),
                Some(&"注".repeat(MAX_APPROVAL_NOTE_CHARS)),
                2,
            )
            .unwrap();

        let approval = one_off("approval-too-long", 10);
        let approval = NewObligation {
            approval_required: true,
            ..approval
        };
        store.create(approval, 1).unwrap();
        assert!(matches!(
            store.decide_approval(
                "approval-too-long",
                ApprovalDecision::Approved,
                &"人".repeat(MAX_APPROVAL_ACTOR_CHARS + 1),
                None,
                2,
            ),
            Err(StoreError::Invalid(_))
        ));

        let claim = store.claim_due(10, 20, 1).unwrap().remove(0);
        assert!(matches!(
            store.complete(
                &claim,
                Completion::Failed {
                    disposition: FailureDisposition::Terminal,
                    error: "e".repeat(MAX_COMPLETION_ERROR_CHARS + 1),
                    evidence: None,
                },
                11,
            ),
            Err(StoreError::Invalid(_))
        ));
        // Rejection happens before the attempt update, so the valid claim still owns the lease.
        store
            .complete(
                &claim,
                Completion::Failed {
                    disposition: FailureDisposition::HumanDecision,
                    error: "界".repeat(MAX_COMPLETION_ERROR_CHARS),
                    evidence: Some("証".repeat(MAX_COMPLETION_EVIDENCE_CHARS)),
                },
                11,
            )
            .unwrap();
    }

    #[test]
    fn audit_metadata_limits_are_enforced_before_insert() {
        let mut store = Store::open_in_memory().unwrap();
        store.create(one_off("audit-limits", 10), 1).unwrap();
        let transaction = store.connection.transaction().unwrap();
        assert!(matches!(
            append_event(
                &transaction,
                "audit-limits",
                1,
                &"界".repeat(MAX_AUDIT_EVENT_TYPE_CHARS + 1),
                2,
                Some(ObligationState::Pending),
                ObligationState::Pending,
                json!({}),
            ),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            append_event(
                &transaction,
                "audit-limits",
                1,
                "bounded",
                2,
                Some(ObligationState::Pending),
                ObligationState::Pending,
                json!({"metadata": "contains\0nul"}),
            ),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            append_event(
                &transaction,
                "audit-limits",
                1,
                "bounded",
                2,
                Some(ObligationState::Pending),
                ObligationState::Pending,
                json!({"metadata": "x".repeat(MAX_AUDIT_DETAILS_BYTES)}),
            ),
            Err(StoreError::Invalid(_))
        ));
    }

    #[test]
    fn every_failure_disposition_is_persisted_and_only_retry_safe_auto_retries() {
        for (index, disposition) in [
            FailureDisposition::RetrySafe,
            FailureDisposition::NeedsReconciliation,
            FailureDisposition::HumanDecision,
            FailureDisposition::Terminal,
            FailureDisposition::Cancelled,
        ]
        .into_iter()
        .enumerate()
        {
            let mut store = Store::open_in_memory().unwrap();
            let id = format!("disposition-{index}");
            store.create(one_off(&id, 10), 1).unwrap();
            let claim = store.claim_due(10, 20, 1).unwrap().remove(0);
            store
                .complete(
                    &claim,
                    Completion::Failed {
                        disposition,
                        error: "typed failure".to_owned(),
                        evidence: Some("durable intent".to_owned()),
                    },
                    11,
                )
                .unwrap();
            let obligation = store.get(&id).unwrap().unwrap();
            assert_eq!(obligation.failure_disposition, Some(disposition));
            assert_eq!(
                store.attempts(&id).unwrap()[0].failure_disposition,
                Some(disposition)
            );
            assert_eq!(
                obligation.state,
                if disposition == FailureDisposition::RetrySafe {
                    ObligationState::RetryScheduled
                } else {
                    ObligationState::Attention
                }
            );
            let details: serde_json::Value =
                serde_json::from_str(&store.events(&id).unwrap().last().unwrap().details_json)
                    .unwrap();
            assert_eq!(details["failure_disposition"], disposition.to_string());
            assert_projection_agrees_with_latest_event(&store, &id);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 12,
            failure_persistence: None,
            rng_seed: RngSeed::Fixed(0xB0_11_1E),
            ..ProptestConfig::default()
        })]

        #[test]
        fn two_store_lifecycle_model_preserves_fencing_liveness_and_typed_intent(
            lease_seconds in 5_i64..40,
            renewal_delta in 0_i64..5,
            disposition_index in 0_usize..5,
        ) {
            let dispositions = [
                FailureDisposition::RetrySafe,
                FailureDisposition::NeedsReconciliation,
                FailureDisposition::HumanDecision,
                FailureDisposition::Terminal,
                FailureDisposition::Cancelled,
            ];
            let disposition = dispositions[disposition_index];
            let directory = TempDir::new().unwrap();
            let path = directory.path().join("model.sqlite");
            let mut first = Store::open(&path).unwrap();
            let mut second = Store::open(&path).unwrap();
            let mut obligation = one_off("model", 100);
            obligation.approval_required = true;
            obligation.retry.max_attempts = 3;
            first.create(obligation, 90).unwrap();
            second.decide_approval(
                "model",
                ApprovalDecision::Approved,
                "worker α",
                Some("generated approval"),
                95,
            ).unwrap();

            let stale = first.claim_due(100, lease_seconds, 1).unwrap().remove(0);
            prop_assert!(second.claim_due(100, lease_seconds, 1).unwrap().is_empty());
            let renewal_at = 100 + renewal_delta;
            let renewed_until = second.renew_lease(&stale, renewal_at, lease_seconds).unwrap();
            prop_assert_eq!(renewed_until, renewal_at.saturating_add(lease_seconds));

            first.recover_expired_leases(renewed_until).unwrap();
            let stale_is_fenced = matches!(
                second.complete(
                    &stale,
                    Completion::Succeeded { evidence: None },
                    renewed_until,
                ),
                Err(StoreError::Fenced)
            );
            prop_assert!(stale_is_fenced);
            let after_expiry = first.get("model").unwrap().unwrap();
            prop_assert_eq!(after_expiry.state, ObligationState::RetryScheduled);
            prop_assert_eq!(after_expiry.failure_disposition, Some(FailureDisposition::RetrySafe));

            let retry_at = after_expiry.next_wake_at.unwrap();
            let current = second.claim_due(retry_at, lease_seconds, 1).unwrap().remove(0);
            prop_assert!(first.claim_due(retry_at, lease_seconds, 1).unwrap().is_empty());
            first.complete(
                &current,
                Completion::Failed {
                    disposition,
                    error: "generated typed outcome".to_owned(),
                    evidence: Some("persisted ambiguity or terminal intent".to_owned()),
                },
                retry_at + 1,
            ).unwrap();

            let completed = second.get("model").unwrap().unwrap();
            prop_assert_eq!(completed.failure_disposition, Some(disposition));
            prop_assert_eq!(
                completed.state,
                if disposition == FailureDisposition::RetrySafe {
                    ObligationState::RetryScheduled
                } else {
                    ObligationState::Attention
                }
            );
            let latest = second.events("model").unwrap().pop().unwrap();
            prop_assert_eq!(latest.to_state, completed.state);
            prop_assert!(latest.details_json.contains(&disposition.to_string()));
            assert_projection_agrees_with_latest_event(&second, "model");

            if completed.state == ObligationState::Attention {
                first.retry_attention("model", retry_at + 2).unwrap();
                second.decide_approval(
                    "model",
                    ApprovalDecision::Approved,
                    "operator",
                    Some("approved reconciled retry"),
                    retry_at + 2,
                ).unwrap();
                let retried = second.claim_due(retry_at + 2, lease_seconds, 1).unwrap().remove(0);
                first.complete(
                    &retried,
                    Completion::Succeeded { evidence: Some("operator reconciled".to_owned()) },
                    retry_at + 3,
                ).unwrap();
            }

            first.create(one_off("cancel-model", retry_at + 10), retry_at + 3).unwrap();
            second.cancel("cancel-model", retry_at + 4).unwrap();
            prop_assert_eq!(
                first.get("cancel-model").unwrap().unwrap().failure_disposition,
                Some(FailureDisposition::Cancelled)
            );
            assert_projection_agrees_with_latest_event(&first, "model");
            assert_projection_agrees_with_latest_event(&first, "cancel-model");
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
                (4, "0004_coding_gardener_runs.sql".to_owned()),
                (5, "0005_gardener_trust_publication.sql".to_owned()),
                (6, "0006_source_bound_proposal_generations.sql".to_owned()),
                (7, "0007_immutable_migration_manifest.sql".to_owned()),
                (8, "0008_typed_failure_dispositions.sql".to_owned())
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
    fn lease_renewals_are_bounded_by_each_heartbeat_and_identical_renewal_is_a_no_op() {
        let mut store = Store::open_in_memory().unwrap();
        store.create(one_off("lease", 100), 100).unwrap();
        let claim = store.claim_due(100, 30, 1).unwrap().remove(0);

        for (heartbeat, expected_expiry) in [(101, 131), (110, 140), (120, 150)] {
            assert_eq!(
                store.renew_lease(&claim, heartbeat, 30).unwrap(),
                expected_expiry
            );
            let obligation = store.get("lease").unwrap().unwrap();
            assert_eq!(obligation.lease_expires_at, Some(expected_expiry));
            assert_eq!(obligation.updated_at, heartbeat);
        }

        let events_before = store.events("lease").unwrap();
        let updated_at_before = store.get("lease").unwrap().unwrap().updated_at;
        assert_eq!(store.renew_lease(&claim, 120, 30).unwrap(), 150);
        assert_eq!(store.events("lease").unwrap(), events_before);
        assert_eq!(
            store.get("lease").unwrap().unwrap().updated_at,
            updated_at_before
        );
    }

    #[test]
    fn renewal_is_fenced_at_the_lease_expiry_boundary() {
        let mut store = Store::open_in_memory().unwrap();
        store.create(one_off("lease", 100), 100).unwrap();
        let claim = store.claim_due(100, 30, 1).unwrap().remove(0);

        assert_eq!(store.renew_lease(&claim, 129, 30).unwrap(), 159);
        let events_before = store.events("lease").unwrap();
        assert!(matches!(
            store.renew_lease(&claim, 159, 30),
            Err(StoreError::Fenced)
        ));
        assert_eq!(store.events("lease").unwrap(), events_before);
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
                    disposition: FailureDisposition::RetrySafe,
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
                    disposition: FailureDisposition::RetrySafe,
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
                    disposition: FailureDisposition::Terminal,
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

    fn observe_scheduled_generation(
        store: &mut Store,
        inspection_obligation_id: &str,
        inspection_id: &str,
        source_commit: char,
        prompt: &str,
    ) -> Proposal {
        let due = store
            .get(inspection_obligation_id)
            .unwrap()
            .unwrap()
            .next_wake_at
            .unwrap();
        let claim = store
            .claim_due_gardener(due, 60, 10)
            .unwrap()
            .into_iter()
            .find(|claim| claim.obligation_id == inspection_obligation_id)
            .unwrap();
        store
            .start_gardener_inspection(&claim, new_inspection(inspection_id, source_commit), due)
            .unwrap();
        let proposal = store
            .finish_gardener_inspection(&claim, inspection_id, &inspection_result(prompt), due + 1)
            .unwrap()
            .remove(0);
        store
            .complete(
                &claim,
                Completion::Succeeded {
                    evidence: Some(inspection_id.to_owned()),
                },
                due + 2,
            )
            .unwrap();
        proposal
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

    fn passing_candidate_qualification(
        run_id: &str,
        head: &str,
        qualified_at: i64,
    ) -> GardenerCandidateQualification {
        GardenerCandidateQualification {
            run_id: run_id.to_owned(),
            head: head.to_owned(),
            diff_manifest_json: r#"[{"path":"src/lib.rs","mode":"100644","kind":"text","size":1}]"#.to_owned(),
            tree_manifest_json: r#"[{"path":"src/lib.rs","mode":"100644","kind":"text","size":1}]"#.to_owned(),
            checks_json: r#"[{"executable":{"role":"candidate_check","path":"/test/cargo","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","version":"cargo test"},"arguments":["test","--all-targets","--locked"],"duration_millis":1,"status":{"kind":"passed"},"evidence":{"stdout":{},"stderr":{}}}]"#.to_owned(),
            duration_ms: 1,
            qualified_at,
        }
    }

    fn reproducibility_manifest(run_id: &str, source: &str) -> GardenerReproducibilityManifest {
        GardenerReproducibilityManifest {
            run_id: run_id.to_owned(),
            bokkie_build: "bokkie 0.1.0 test-build".to_owned(),
            source_commit: source.to_owned(),
            prompt_digest: "1".repeat(64),
            implementation_schema_digest: "2".repeat(64),
            verification_schema_digest: "3".repeat(64),
            codex_profile: Some("test-profile".to_owned()),
            codex_model: Some("test-model".to_owned()),
            executable_manifest_json: r#"[{"role":"codex","path":"/test/codex","version":"codex test","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]"#.to_owned(),
            sandbox_policy_digest: "4".repeat(64),
            environment_policy_digest: "5".repeat(64),
            check_commands_json: r#"[{"executable":"/test/cargo","arguments":["test","--locked"]}]"#.to_owned(),
            recorded_at: 1_004,
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
                r#"{"summary":"implementation completed","changed_paths":[],"checks":[]}"#,
                1_007,
            )
            .unwrap();
        store
            .record_gardener_git_commit(claim, run_id, head, 1_008)
            .unwrap();
        store
            .record_gardener_candidate_qualification(
                claim,
                &passing_candidate_qualification(run_id, head, 1_008),
                1_008,
            )
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
    fn all_proposal_instances_are_from_one_snapshot_during_atomic_multi_proposal_write() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("proposal-instance-snapshot.sqlite");
        let mut writer = Store::open(&path).unwrap();
        let registration = writer
            .register_gardener_repository(gardener_registration(1_000), 900)
            .unwrap();
        let first_claim = writer.claim_due_gardener(1_000, 60, 1).unwrap().remove(0);
        writer
            .start_gardener_inspection(&first_claim, new_inspection("multi-source-a", 'a'), 1_001)
            .unwrap();
        let result = InspectionResult {
            summary: "Two bounded improvements were found".to_owned(),
            proposed_goal_prompts: vec![
                "Implement atomic candidate alpha.".to_owned(),
                "Implement atomic candidate beta.".to_owned(),
            ],
        };
        let proposals = writer
            .finish_gardener_inspection(&first_claim, "multi-source-a", &result, 1_002)
            .unwrap();
        assert_eq!(proposals.len(), 2);
        writer
            .complete(
                &first_claim,
                Completion::Succeeded { evidence: None },
                1_003,
            )
            .unwrap();

        let next_at = writer
            .get(&registration.inspection_obligation_id)
            .unwrap()
            .unwrap()
            .next_wake_at
            .unwrap();
        let second_claim = writer.claim_due_gardener(next_at, 60, 1).unwrap().remove(0);
        writer
            .start_gardener_inspection(
                &second_claim,
                new_inspection("multi-source-b", 'b'),
                next_at + 1,
            )
            .unwrap();

        let reader = Store::open_compatible(&path).unwrap();
        let during = reader
            .gardener_proposal_instances_all_with_hook(|| {
                writer
                    .finish_gardener_inspection(
                        &second_claim,
                        "multi-source-b",
                        &result,
                        next_at + 2,
                    )
                    .map(|_| ())
            })
            .unwrap();
        let expected_fingerprints = proposals
            .iter()
            .map(|proposal| proposal.fingerprint.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(during.len(), 2);
        assert!(during.iter().all(|instance| instance.generation == 1));
        assert_eq!(
            during
                .iter()
                .map(|instance| instance.proposal_fingerprint.clone())
                .collect::<BTreeSet<_>>(),
            expected_fingerprints
        );

        let after = reader.gardener_proposal_instances_all().unwrap();
        assert_eq!(after.len(), 4);
        for proposal in proposals {
            let generations = after
                .iter()
                .filter(|instance| instance.proposal_fingerprint == proposal.fingerprint)
                .map(|instance| instance.generation)
                .collect::<Vec<_>>();
            assert_eq!(generations, [1, 2]);
        }
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
    fn equivalent_prompts_create_source_bound_generations_across_commits() {
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
        assert_ne!(
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
            2
        );
        let instances = store
            .gardener_proposal_instances(&first.fingerprint)
            .unwrap();
        assert_eq!(instances.len(), 2);
        assert_eq!(instances[0].generation, 1);
        assert_eq!(instances[0].source_commit, "a".repeat(40));
        assert_eq!(
            instances[0].superseded_by.as_deref(),
            Some(instances[1].id.as_str())
        );
        assert_eq!(instances[0].obligation_state, ObligationState::Cancelled);
        assert_eq!(instances[1].generation, 2);
        assert_eq!(instances[1].source_commit, "b".repeat(40));
        assert_eq!(
            instances[1].obligation_state,
            ObligationState::AwaitingApproval
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
        assert_eq!(
            reopened
                .gardener_proposal_instances(&fingerprint)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn repeated_same_source_observations_dedupe_to_one_instance() {
        let mut store = Store::open_in_memory().unwrap();
        let registration = store
            .register_gardener_repository(gardener_registration(1_000), 900)
            .unwrap();
        let prompt = "Keep one source-bound generation.";
        let first = observe_scheduled_generation(
            &mut store,
            &registration.inspection_obligation_id,
            "same-source-1",
            'a',
            prompt,
        );
        let second = observe_scheduled_generation(
            &mut store,
            &registration.inspection_obligation_id,
            "same-source-2",
            'a',
            prompt,
        );
        assert_eq!(first.fingerprint, second.fingerprint);
        assert_eq!(
            first.implementation_obligation_id,
            second.implementation_obligation_id
        );
        let instances = store
            .gardener_proposal_instances(&first.fingerprint)
            .unwrap();
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].generation, 1);
        assert_eq!(instances[0].observation_count, 2);
        assert_eq!(instances[0].source_inspection_id, "same-source-1");
    }

    #[test]
    fn supersession_rejects_stale_decisions_and_pre_run_claims() {
        let mut store = Store::open_in_memory().unwrap();
        let registration = store
            .register_gardener_repository(gardener_registration(1_000), 900)
            .unwrap();
        let prompt = "Fence stale source authority.";
        let first = observe_scheduled_generation(
            &mut store,
            &registration.inspection_obligation_id,
            "race-source-a",
            'a',
            prompt,
        );
        let first_instance = store
            .gardener_proposal_instances(&first.fingerprint)
            .unwrap()
            .remove(0);
        store
            .decide_gardener_proposal_instance(
                &first_instance.id,
                ApprovalDecision::Approved,
                "operator",
                None,
                1_003,
            )
            .unwrap();
        let stale_claim = store
            .claim_due_gardener(1_003, 1_000, 10)
            .unwrap()
            .into_iter()
            .find(|claim| claim.obligation_id == first_instance.implementation_obligation_id)
            .unwrap();
        observe_scheduled_generation(
            &mut store,
            &registration.inspection_obligation_id,
            "race-source-b",
            'b',
            prompt,
        );
        let instances = store
            .gardener_proposal_instances(&first.fingerprint)
            .unwrap();
        assert_eq!(instances.len(), 2);
        assert_eq!(
            instances[1].obligation_state,
            ObligationState::AwaitingApproval
        );
        assert!(matches!(
            store.decide_gardener_proposal_instance(
                &first_instance.id,
                ApprovalDecision::Rejected,
                "late-operator",
                None,
                1_100,
            ),
            Err(StoreError::Conflict(_))
        ));
        assert!(matches!(
            store.create_gardener_implementation_run(
                &stale_claim,
                new_implementation_run("stale-pre-run"),
                1_100,
            ),
            Err(StoreError::Fenced)
        ));
        assert!(matches!(
            store.renew_lease(&stale_claim, 1_100, 1_000),
            Err(StoreError::Fenced)
        ));
    }

    #[test]
    fn approval_recorded_before_a_new_source_is_not_inherited() {
        let mut store = Store::open_in_memory().unwrap();
        let registration = store
            .register_gardener_repository(gardener_registration(1_000), 900)
            .unwrap();
        let prompt = "Do not inherit source authority.";
        let first = observe_scheduled_generation(
            &mut store,
            &registration.inspection_obligation_id,
            "approval-race-a",
            'a',
            prompt,
        );
        let first_instance = store
            .gardener_proposal_instances(&first.fingerprint)
            .unwrap()
            .remove(0);
        let inspection_due = store
            .get(&registration.inspection_obligation_id)
            .unwrap()
            .unwrap()
            .next_wake_at
            .unwrap();
        let inspection_claim = store
            .claim_due_gardener(inspection_due, 60, 1)
            .unwrap()
            .remove(0);
        store
            .start_gardener_inspection(
                &inspection_claim,
                new_inspection("approval-race-b", 'b'),
                inspection_due,
            )
            .unwrap();
        store
            .decide_gardener_proposal_instance(
                &first_instance.id,
                ApprovalDecision::Approved,
                "operator",
                None,
                inspection_due,
            )
            .unwrap();
        store
            .finish_gardener_inspection(
                &inspection_claim,
                "approval-race-b",
                &inspection_result(prompt),
                inspection_due + 1,
            )
            .unwrap();
        let instances = store
            .gardener_proposal_instances(&first.fingerprint)
            .unwrap();
        assert_eq!(
            instances[0].approval_decision,
            Some(ApprovalDecision::Approved)
        );
        assert_eq!(instances[0].obligation_state, ObligationState::Cancelled);
        assert_eq!(instances[1].approval_decision, None);
        assert_eq!(
            instances[1].obligation_state,
            ObligationState::AwaitingApproval
        );
        let claims = store.claim_due_gardener(2_000, 60, 10).unwrap();
        assert!(claims.iter().all(|claim| {
            claim.obligation_id != first_instance.implementation_obligation_id
                && claim.obligation_id != instances[1].implementation_obligation_id
        }));
    }

    #[test]
    fn exact_run_created_before_supersession_is_fenced_at_its_stored_source() {
        let mut store = Store::open_in_memory().unwrap();
        let registration = store
            .register_gardener_repository(gardener_registration(1_000), 900)
            .unwrap();
        let prompt = "Preserve already-durable exact work.";
        let first = observe_scheduled_generation(
            &mut store,
            &registration.inspection_obligation_id,
            "continuing-source-a",
            'a',
            prompt,
        );
        let first_instance = store
            .gardener_proposal_instances(&first.fingerprint)
            .unwrap()
            .remove(0);
        store
            .decide_gardener_proposal_instance(
                &first_instance.id,
                ApprovalDecision::Approved,
                "operator",
                None,
                1_003,
            )
            .unwrap();
        let claim = store
            .claim_due_gardener(1_003, 1_000, 10)
            .unwrap()
            .into_iter()
            .find(|claim| claim.obligation_id == first_instance.implementation_obligation_id)
            .unwrap();
        let run = store
            .create_gardener_implementation_run(
                &claim,
                new_implementation_run("continuing-run"),
                1_004,
            )
            .unwrap();
        observe_scheduled_generation(
            &mut store,
            &registration.inspection_obligation_id,
            "continuing-source-b",
            'b',
            prompt,
        );
        assert!(matches!(
            store.record_gardener_reproducibility_manifest(
                &claim,
                &reproducibility_manifest("continuing-run", &run.source_commit),
                1_100,
            ),
            Err(StoreError::Fenced)
        ));
        assert!(matches!(
            store.renew_lease(&claim, 1_100, 1_000),
            Err(StoreError::Fenced)
        ));
        let retained = store
            .gardener_implementation_run("continuing-run")
            .unwrap()
            .unwrap();
        assert_eq!(retained.source_commit, "a".repeat(40));
        assert_eq!(retained.proposal_instance_id, first_instance.id);
        assert_eq!(retained.proposal_generation, 1);
        assert!(
            store
                .gardener_reproducibility_manifest("continuing-run")
                .unwrap()
                .is_none()
        );
        store
            .complete(
                &claim,
                Completion::Failed {
                    disposition: FailureDisposition::Terminal,
                    error: "superseded heartbeat was fenced".to_owned(),
                    evidence: None,
                },
                1_101,
            )
            .unwrap();
        assert_eq!(
            store
                .get(&first_instance.implementation_obligation_id)
                .unwrap()
                .unwrap()
                .state,
            ObligationState::Attention
        );
        let projected = store
            .operator_snapshot(1_102)
            .unwrap()
            .obligations
            .into_iter()
            .find(|item| item.id == first_instance.implementation_obligation_id)
            .unwrap();
        assert!(!projected.capabilities.retry.available);
        assert!(!projected.capabilities.approve_gardener_proposal.available);
        assert!(!projected.capabilities.reject_gardener_proposal.available);
        assert!(matches!(
            store.retry_attention(&first_instance.implementation_obligation_id, 1_102),
            Err(StoreError::Conflict(_))
        ));
        assert_eq!(
            store
                .get(&first_instance.implementation_obligation_id)
                .unwrap()
                .unwrap()
                .state,
            ObligationState::Attention
        );
    }

    #[test]
    fn source_generation_evidence_tables_reject_updates_and_deletes() {
        let mut store = Store::open_in_memory().unwrap();
        let registration = store
            .register_gardener_repository(gardener_registration(1_000), 900)
            .unwrap();
        let prompt = "Keep source generation evidence immutable.";
        let first = observe_scheduled_generation(
            &mut store,
            &registration.inspection_obligation_id,
            "immutable-source-a",
            'a',
            prompt,
        );
        observe_scheduled_generation(
            &mut store,
            &registration.inspection_obligation_id,
            "immutable-source-b",
            'b',
            prompt,
        );
        let instances = store
            .gardener_proposal_instances(&first.fingerprint)
            .unwrap();
        let current = instances.last().unwrap();
        store
            .decide_gardener_proposal_instance(
                &current.id,
                ApprovalDecision::Approved,
                "operator",
                None,
                1_100,
            )
            .unwrap();
        let claim = store
            .claim_due_gardener(1_100, 60, 10)
            .unwrap()
            .into_iter()
            .find(|claim| claim.obligation_id == current.implementation_obligation_id)
            .unwrap();
        store
            .create_gardener_implementation_run(
                &claim,
                new_implementation_run("immutable-run-map"),
                1_101,
            )
            .unwrap();

        for table in [
            "gardener_proposal_instances",
            "gardener_proposal_observation_instances",
            "gardener_proposal_instance_supersessions",
            "gardener_proposal_instance_decisions",
            "gardener_implementation_run_instances",
        ] {
            let update = format!("UPDATE {table} SET rowid = rowid");
            assert!(store.connection.execute(&update, []).is_err(), "{table}");
            let delete = format!("DELETE FROM {table}");
            assert!(store.connection.execute(&delete, []).is_err(), "{table}");
        }
    }

    #[test]
    fn terminal_rejected_and_cancelled_instances_do_not_block_a_later_generation() {
        for disposition in ["completed", "rejected", "cancelled"] {
            let mut store = Store::open_in_memory().unwrap();
            let registration = store
                .register_gardener_repository(gardener_registration(1_000), 900)
                .unwrap();
            let prompt = format!("Reopen after {disposition}.");
            let first = observe_scheduled_generation(
                &mut store,
                &registration.inspection_obligation_id,
                &format!("{disposition}-source-a"),
                'a',
                &prompt,
            );
            let first_instance = store
                .gardener_proposal_instances(&first.fingerprint)
                .unwrap()
                .remove(0);
            match disposition {
                "completed" => {
                    store
                        .decide_gardener_proposal_instance(
                            &first_instance.id,
                            ApprovalDecision::Approved,
                            "operator",
                            None,
                            1_003,
                        )
                        .unwrap();
                    let claim = store
                        .claim_due_gardener(1_003, 60, 10)
                        .unwrap()
                        .into_iter()
                        .find(|claim| {
                            claim.obligation_id == first_instance.implementation_obligation_id
                        })
                        .unwrap();
                    store
                        .complete(
                            &claim,
                            Completion::Succeeded {
                                evidence: Some("terminal".to_owned()),
                            },
                            1_004,
                        )
                        .unwrap();
                }
                "rejected" => {
                    store
                        .decide_gardener_proposal_instance(
                            &first_instance.id,
                            ApprovalDecision::Rejected,
                            "operator",
                            None,
                            1_003,
                        )
                        .unwrap();
                }
                "cancelled" => store
                    .cancel(&first_instance.implementation_obligation_id, 1_003)
                    .unwrap(),
                _ => unreachable!(),
            }
            observe_scheduled_generation(
                &mut store,
                &registration.inspection_obligation_id,
                &format!("{disposition}-source-b"),
                'b',
                &prompt,
            );
            let instances = store
                .gardener_proposal_instances(&first.fingerprint)
                .unwrap();
            assert_eq!(instances.len(), 2, "{disposition}");
            assert_eq!(
                instances[1].obligation_state,
                ObligationState::AwaitingApproval,
                "{disposition}"
            );
            assert_eq!(instances[1].approval_decision, None, "{disposition}");
        }
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
    fn repeated_exact_decisions_preserve_history_and_use_the_latest_mapping() {
        let mut store = Store::open_in_memory().unwrap();
        let registration = store
            .register_gardener_repository(gardener_registration(1_000), 900)
            .unwrap();
        let proposal = observe_scheduled_generation(
            &mut store,
            &registration.inspection_obligation_id,
            "decision-history",
            'a',
            "Retain exact decision history.",
        );
        let instance = store
            .gardener_proposal_instances(&proposal.fingerprint)
            .unwrap()
            .remove(0);
        store
            .decide_gardener_proposal_instance(
                &instance.id,
                ApprovalDecision::Rejected,
                "first-operator",
                None,
                1_003,
            )
            .unwrap();
        store
            .retry_attention(&instance.implementation_obligation_id, 1_004)
            .unwrap();
        let approved = store
            .decide_gardener_proposal_instance(
                &instance.id,
                ApprovalDecision::Approved,
                "second-operator",
                Some("reconsidered"),
                1_005,
            )
            .unwrap();
        assert_eq!(approved.approval_decision, Some(ApprovalDecision::Approved));
        assert_eq!(approved.obligation_state, ObligationState::Pending);
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT count(*) FROM gardener_proposal_instance_decisions
                     WHERE instance_id = ?1",
                    [&instance.id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            2
        );
        let audit = store
            .events(&instance.implementation_obligation_id)
            .unwrap();
        let details: serde_json::Value =
            serde_json::from_str(&audit.last().unwrap().details_json).unwrap();
        assert_eq!(details["proposal_instance_id"], instance.id);
        assert_eq!(details["source_commit"], instance.source_commit);
        assert_eq!(
            details["source_observation_id"],
            instance.source_observation_id
        );
        assert_eq!(details["generation"], 1);
    }

    #[test]
    fn inspection_field_limits_count_unicode_characters() {
        assert!(
            validate_inspection_result(&InspectionResult {
                summary: "bounded".to_owned(),
                proposed_goal_prompts: vec!["🦘".repeat(MAX_GARDENER_PROMPT_CHARS)],
            })
            .is_ok()
        );
        assert!(matches!(
            validate_inspection_result(&InspectionResult {
                summary: "bounded".to_owned(),
                proposed_goal_prompts: vec!["🦘".repeat(MAX_GARDENER_PROMPT_CHARS + 1)],
            }),
            Err(StoreError::Invalid(_))
        ));
        let mut store = Store::open_in_memory().unwrap();
        let registration = store
            .register_gardener_repository(gardener_registration(1_000), 900)
            .unwrap();
        let due = store
            .get(&registration.inspection_obligation_id)
            .unwrap()
            .unwrap()
            .next_wake_at
            .unwrap();
        let claim = store.claim_due_gardener(due, 60, 1).unwrap().remove(0);
        store
            .start_gardener_inspection(&claim, new_inspection("unicode-limit", 'a'), due)
            .unwrap();
        store
            .finish_gardener_inspection(
                &claim,
                "unicode-limit",
                &InspectionResult {
                    summary: "🦘".repeat(MAX_GARDENER_MODEL_TEXT_CHARS),
                    proposed_goal_prompts: Vec::new(),
                },
                due + 1,
            )
            .unwrap();
        store
            .complete(&claim, Completion::Succeeded { evidence: None }, due + 2)
            .unwrap();

        let next_due = store
            .get(&registration.inspection_obligation_id)
            .unwrap()
            .unwrap()
            .next_wake_at
            .unwrap();
        let next_claim = store.claim_due_gardener(next_due, 60, 1).unwrap().remove(0);
        store
            .start_gardener_inspection(
                &next_claim,
                new_inspection("unicode-over-limit", 'b'),
                next_due,
            )
            .unwrap();
        assert!(matches!(
            store.finish_gardener_inspection(
                &next_claim,
                "unicode-over-limit",
                &InspectionResult {
                    summary: "🦘".repeat(MAX_GARDENER_MODEL_TEXT_CHARS + 1),
                    proposed_goal_prompts: Vec::new(),
                },
                next_due + 1,
            ),
            Err(StoreError::Invalid(_))
        ));
        assert!(
            store
                .gardener_inspection("unicode-over-limit")
                .unwrap()
                .unwrap()
                .result_json
                .is_none()
        );
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
        let manifest = reproducibility_manifest("run-1", &run.source_commit);
        store
            .record_gardener_reproducibility_manifest(&claim, &manifest, 1_004)
            .unwrap();
        store
            .record_gardener_reproducibility_manifest(&claim, &manifest, 1_004)
            .unwrap();
        let mut changed_manifest = manifest.clone();
        changed_manifest.environment_policy_digest = "6".repeat(64);
        assert!(matches!(
            store.record_gardener_reproducibility_manifest(&claim, &changed_manifest, 1_004),
            Err(StoreError::Conflict(_))
        ));
        assert_eq!(
            store.gardener_reproducibility_manifest("run-1").unwrap(),
            Some(manifest)
        );

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
        assert!(matches!(
            store.finish_gardener_implementation(
                &claim,
                "run-1",
                r#"{"summary":"missing schema-required lists"}"#,
                1_007
            ),
            Err(StoreError::Invalid(_))
        ));
        store
            .finish_gardener_implementation(
                &claim,
                "run-1",
                r#"{"summary":"done","changed_paths":[],"checks":[]}"#,
                1_007,
            )
            .unwrap();

        let head = "b".repeat(40);
        let other_head = "c".repeat(40);
        store
            .record_gardener_git_commit(&claim, "run-1", &head, 1_008)
            .unwrap();
        assert!(matches!(
            store.record_gardener_push_observation(&claim, "run-1", &head, 1_009),
            Err(StoreError::Conflict(_))
        ));
        store
            .record_gardener_candidate_qualification(
                &claim,
                &passing_candidate_qualification("run-1", &head, 1_008),
                1_008,
            )
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
        store
            .request_gardener_pull_request_ready(&claim, "run-1", &head, 1_015)
            .unwrap();
        store
            .record_gardener_pull_request_ready(
                &claim,
                "run-1",
                42,
                "https://github.com/robchristie/bokkie/pull/42",
                &head,
                1_016,
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
        assert_eq!(finished.publication_state, GardenerPublicationState::Ready);
        assert_eq!(finished.pull_request_ready_at, Some(1_016));
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
        assert_eq!(store.gardener_run_events("run-1").unwrap().len(), 15);
    }

    #[test]
    fn implementation_run_uses_the_exact_approved_proposal_instance_source() {
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
        let instances = store
            .gardener_proposal_instances(&proposal.fingerprint)
            .unwrap();
        let latest = instances.last().unwrap();
        store
            .decide_gardener_proposal_instance(
                &latest.id,
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
            .find(|claim| claim.obligation_id == latest.implementation_obligation_id)
            .unwrap();
        let run = store
            .create_gardener_implementation_run(
                &implementation_claim,
                new_implementation_run("latest-source-run"),
                next_at + 4,
            )
            .unwrap();
        assert_eq!(run.source_commit, "b".repeat(40));
        assert_eq!(run.proposal_instance_id, latest.id);
        assert_eq!(run.proposal_generation, 2);
        assert_eq!(run.source_observation_id, latest.source_observation_id);
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
        assert_eq!(events_before.len(), 8);
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

    fn create_legacy_v5_database(
        path: &Path,
        sources: &[char],
        approved: bool,
        run_source: Option<char>,
        running_without_run: bool,
    ) {
        let connection = Connection::open(path).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        connection
            .execute_batch(
                "CREATE TABLE schema_migrations (
                    version INTEGER PRIMARY KEY,
                    name TEXT NOT NULL UNIQUE,
                    applied_at INTEGER NOT NULL DEFAULT (unixepoch())
                 );",
            )
            .unwrap();
        for migration in &MIGRATIONS[..5] {
            connection.execute_batch(migration.sql).unwrap();
            connection
                .execute(
                    "INSERT INTO schema_migrations(version, name) VALUES (?1, ?2)",
                    params![migration.version, migration.name],
                )
                .unwrap();
        }
        let fingerprint = proposal_fingerprint(CANONICAL_REPOSITORY, "Legacy goal.");
        let implementation_id = format!("gardener:implement:{fingerprint}");
        connection
            .execute(
                "INSERT INTO obligations(
                    id, description, state, occurrence, scheduled_at, next_wake_at,
                    approval_required, attempts_made, max_attempts, retry_base_seconds,
                    retry_max_seconds, lease_generation, lease_token, lease_expires_at,
                    created_at, updated_at
                 ) VALUES ('legacy-inspection-obligation', 'inspect', 'pending', 1, 10, 10,
                           0, 0, 3, 60, 3600, 0, NULL, NULL, 10, 10)",
                [],
            )
            .unwrap();
        let is_running = run_source.is_some() || running_without_run;
        let implementation_state = if is_running {
            "running"
        } else if approved {
            "pending"
        } else {
            "awaiting_approval"
        };
        connection
            .execute(
                "INSERT INTO obligations(
                    id, description, state, occurrence, scheduled_at, next_wake_at,
                    approval_required, attempts_made, max_attempts, retry_base_seconds,
                    retry_max_seconds, lease_generation, lease_token, lease_expires_at,
                    created_at, updated_at
                 ) VALUES (?1, 'implement', ?2, 1, 20, ?3, 1, ?4, 1, 60, 3600,
                           ?4, ?5, ?6, 20, 20)",
                params![
                    implementation_id,
                    implementation_state,
                    (approved && !is_running).then_some(20),
                    i64::from(is_running),
                    is_running.then_some("legacy-token"),
                    is_running.then_some(10_000_i64),
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO gardener_obligation_bindings(obligation_id, kind, created_at)
                 VALUES ('legacy-inspection-obligation', 'inspection', 10),
                        (?1, 'implementation', 20)",
                [&implementation_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO gardener_repositories(
                    repository, default_branch, checkout_path, inspection_cron,
                    inspection_timezone, first_inspection_at, inspection_obligation_id,
                    created_at, updated_at
                 ) VALUES (?1, 'main', '/legacy', '* * * * *', 'UTC', 10,
                           'legacy-inspection-obligation', 10, 10)",
                [CANONICAL_REPOSITORY],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO gardener_proposals(
                    fingerprint, repository, prompt, implementation_obligation_id, created_at
                 ) VALUES (?1, ?2, 'Legacy goal.', ?3, 20)",
                params![fingerprint, CANONICAL_REPOSITORY, implementation_id],
            )
            .unwrap();
        for (index, source) in sources.iter().enumerate() {
            let inspection_id = format!("legacy-inspection-{}", index + 1);
            let source_commit = source.to_string().repeat(40);
            connection
                .execute(
                    "INSERT INTO gardener_inspections(
                        id, repository, obligation_id, occurrence, lease_generation,
                        lease_token, source_commit, worktree_path, prompt_digest,
                        result_json, started_at, completed_at
                     ) VALUES (?1, ?2, 'legacy-inspection-obligation', ?3, ?3, 'token',
                               ?4, ?5, ?6, ?7, ?8, ?8)",
                    params![
                        inspection_id,
                        CANONICAL_REPOSITORY,
                        (index + 1) as i64,
                        source_commit,
                        format!("/legacy/{inspection_id}"),
                        "d".repeat(64),
                        r#"{"summary":"legacy","proposed_goal_prompts":[]}"#,
                        30 + index as i64,
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO gardener_proposal_observations(
                        proposal_fingerprint, inspection_id, source_commit, observed_at
                     ) VALUES (?1, ?2, ?3, ?4)",
                    params![fingerprint, inspection_id, source_commit, 40 + index as i64],
                )
                .unwrap();
        }
        if approved {
            connection
                .execute(
                    "INSERT INTO approvals(
                        obligation_id, occurrence, decision, actor, note, decided_at
                     ) VALUES (?1, 1, 'approved', 'legacy-operator', 'legacy-note', 50)",
                    [&implementation_id],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO audit_events(
                        obligation_id, occurrence, event_type, occurred_at,
                        from_state, to_state, details_json
                     ) VALUES (?1, 1, 'approved', 50, 'awaiting_approval', 'pending',
                               '{\"legacy\":true}')",
                    [&implementation_id],
                )
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO gardener_events(
                    repository, proposal_fingerprint, event_type, occurred_at, details_json
                 ) VALUES (?1, ?2, 'legacy_marker', 51, '{\"legacy\":true}')",
                params![CANONICAL_REPOSITORY, fingerprint],
            )
            .unwrap();
        if is_running {
            connection
                .execute(
                    "INSERT INTO attempts(
                        obligation_id, occurrence, attempt_number, lease_generation,
                        lease_token, claimed_at, outcome
                     ) VALUES (?1, 1, 1, 1, 'legacy-token', 52, 'running')",
                    [&implementation_id],
                )
                .unwrap();
        }
        if let Some(source) = run_source {
            connection
                .execute(
                    "INSERT INTO gardener_implementation_runs(
                        id, repository, proposal_fingerprint, obligation_id, occurrence,
                        attempt_number, lease_generation, lease_token, source_commit,
                        implementation_worktree_path, branch, phase, created_at, updated_at
                     ) VALUES ('legacy-run', ?1, ?2, ?3, 1, 1, 1, 'legacy-token',
                               ?4, '/legacy/run', 'codex/gardener-legacy', 'created', 52, 52)",
                    params![
                        CANONICAL_REPOSITORY,
                        fingerprint,
                        implementation_id,
                        source.to_string().repeat(40)
                    ],
                )
                .unwrap();
        }
    }

    #[test]
    fn migration_backfills_one_source_authority_and_exact_run_without_rewriting_evidence() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("legacy.sqlite3");
        create_legacy_v5_database(&path, &['A'], true, Some('A'), false);

        let store = Store::open(&path).unwrap();
        let fingerprint = proposal_fingerprint(CANONICAL_REPOSITORY, "Legacy goal.");
        let instances = store.gardener_proposal_instances(&fingerprint).unwrap();
        assert_eq!(instances.len(), 1);
        assert_eq!(
            instances[0].approval_decision,
            Some(ApprovalDecision::Approved)
        );
        assert_eq!(instances[0].source_observation_id, 1);
        assert_eq!(instances[0].source_commit, "a".repeat(40));
        let exact_observations = store
            .proposal_instance_observations(&instances[0].id)
            .unwrap();
        assert_eq!(exact_observations.len(), 1);
        assert_eq!(exact_observations[0].source_commit, "A".repeat(40));
        let run = store
            .gardener_implementation_run("legacy-run")
            .unwrap()
            .unwrap();
        assert_eq!(run.proposal_instance_id, instances[0].id);
        assert_eq!(run.proposal_generation, 1);
        assert_eq!(run.source_observation_id, 1);
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT details_json FROM audit_events WHERE sequence = 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            r#"{"legacy":true}"#
        );
        assert_eq!(
            store
                .connection
                .query_row("SELECT count(*) FROM approvals", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .connection
                .query_row("SELECT count(*) FROM gardener_events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn migration_quarantines_ambiguous_multi_source_approval() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("legacy.sqlite3");
        create_legacy_v5_database(&path, &['a', 'b'], true, None, false);

        let store = Store::open(&path).unwrap();
        let fingerprint = proposal_fingerprint(CANONICAL_REPOSITORY, "Legacy goal.");
        let instances = store.gardener_proposal_instances(&fingerprint).unwrap();
        assert_eq!(instances.len(), 2);
        assert_eq!(instances[0].approval_decision, None);
        assert_eq!(instances[0].obligation_state, ObligationState::Cancelled);
        assert_eq!(instances[1].approval_decision, None);
        assert_eq!(
            instances[1].obligation_state,
            ObligationState::AwaitingApproval
        );
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT count(*) FROM gardener_proposal_instance_decisions",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .connection
                .query_row("SELECT count(*) FROM approvals", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT details_json FROM audit_events WHERE sequence = 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            r#"{"legacy":true}"#
        );
        assert_eq!(
            store
                .connection
                .query_row("SELECT count(*) FROM gardener_events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn migration_keeps_ambiguous_running_work_leased_but_prevents_reclaim() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("legacy.sqlite3");
        create_legacy_v5_database(&path, &['a', 'b'], true, None, true);

        let mut store = Store::open(&path).unwrap();
        let fingerprint = proposal_fingerprint(CANONICAL_REPOSITORY, "Legacy goal.");
        let instances = store.gardener_proposal_instances(&fingerprint).unwrap();
        assert_eq!(instances[0].obligation_state, ObligationState::Running);
        assert!(instances[0].superseded_by.is_some());
        assert_eq!(
            instances[1].obligation_state,
            ObligationState::AwaitingApproval
        );
        store.recover_expired_leases(10_000).unwrap();
        assert_eq!(
            store
                .get(&instances[0].implementation_obligation_id)
                .unwrap()
                .unwrap()
                .state,
            ObligationState::Attention
        );
        assert!(
            store
                .claim_due_gardener(20_000, 60, 10)
                .unwrap()
                .into_iter()
                .all(|claim| claim.obligation_id != instances[0].implementation_obligation_id)
        );
    }

    #[test]
    fn migration_keeps_an_ambiguous_legacy_run_visible_but_fences_continuation() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("legacy.sqlite3");
        create_legacy_v5_database(&path, &['a', 'b'], true, Some('a'), false);

        let mut store = Store::open(&path).unwrap();
        let run = store
            .gardener_implementation_run("legacy-run")
            .unwrap()
            .unwrap();
        assert_eq!(run.source_commit, "a".repeat(40));
        assert_eq!(run.proposal_generation, 1);
        let obligation = store.get(&run.obligation_id).unwrap().unwrap();
        let claim = Claim {
            obligation_id: obligation.id,
            occurrence: obligation.occurrence,
            attempt_number: obligation.attempts_made,
            lease_token: obligation.lease_token.unwrap(),
            lease_generation: obligation.lease_generation,
            lease_expires_at: obligation.lease_expires_at.unwrap(),
            description: obligation.description,
        };
        assert!(matches!(
            store.record_implementation_codex_thread(
                &claim,
                "legacy-run",
                "must-not-continue",
                100,
            ),
            Err(StoreError::Fenced)
        ));
        assert!(store.gardener_run_events("legacy-run").unwrap().is_empty());
    }
}
