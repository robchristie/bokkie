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
    Obligation, ObligationState, Recurrence, recurrence::RecurrenceError,
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
        let ids = {
            let mut statement = transaction.prepare(
                "SELECT o.id
                 FROM obligations o
                 WHERE o.state IN ('pending', 'retry_scheduled')
                   AND o.next_wake_at <= ?1
                   AND (
                       o.approval_required = 0 OR
                       (SELECT decision FROM approvals a
                        WHERE a.obligation_id = o.id AND a.occurrence = o.occurrence
                        ORDER BY a.id DESC LIMIT 1) = 'approved'
                   )
                 ORDER BY o.next_wake_at, o.id
                 LIMIT ?2",
            )?;
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
                        let recurrence = Recurrence::new(expression, timezone)?;
                        let next = recurrence.next_after(now.max(obligation.scheduled_at))?;
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
                    } else {
                        transaction.execute(
                            "UPDATE obligations SET state = 'completed', lease_token = NULL,
                                lease_expires_at = NULL, last_error = NULL,
                                last_evidence = ?2, updated_at = ?3 WHERE id = ?1",
                            params![claim.obligation_id, evidence, now],
                        )?;
                        append_event(
                            transaction,
                            &claim.obligation_id,
                            obligation.occurrence,
                            "completed",
                            now,
                            Some(ObligationState::Running),
                            ObligationState::Completed,
                            json!({"evidence": evidence}),
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
                (2, "0002_append_only_guards.sql".to_owned())
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
}
