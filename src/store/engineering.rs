//! Durable supervision uses immutable snapshot revisions and receipts. The snapshot
//! is a bounded typed aggregate, not an alternative obligation lifecycle.
use super::*;
use crate::engineering::*;
use serde::{Serialize, de::DeserializeOwned};
use std::collections::BTreeSet;

fn encode<T: Serialize>(value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|e| StoreError::Invalid(e.to_string()))
}
fn decode<T: DeserializeOwned>(value: &str) -> Result<T, StoreError> {
    serde_json::from_str(value).map_err(|e| StoreError::Invalid(e.to_string()))
}
fn digest<T: Serialize>(value: &T) -> Result<String, StoreError> {
    Ok(format!("{:x}", Sha256::digest(encode(value)?.as_bytes())))
}
fn conflict(message: &str) -> StoreError {
    StoreError::Conflict(message.into())
}
fn fresh_id() -> String {
    Uuid::new_v4().to_string()
}
fn role_name(role: EngineeringRole) -> &'static str {
    match role {
        EngineeringRole::Supervisor => "supervisor",
        EngineeringRole::Worker => "worker",
    }
}
fn actor_name(actor: &EngineeringActor) -> String {
    match actor {
        EngineeringActor::Operator { name } => format!("operator:{name}"),
        EngineeringActor::Supervisor { execution_id, .. } => format!("supervisor:{execution_id}"),
        EngineeringActor::Worker { execution_id, .. } => format!("worker:{execution_id}"),
        EngineeringActor::Reconciler { adapter_id } => format!("reconciler:{adapter_id}"),
    }
}
fn actor_execution(actor: &EngineeringActor) -> Option<&str> {
    match actor {
        EngineeringActor::Supervisor { execution_id, .. }
        | EngineeringActor::Worker { execution_id, .. } => Some(execution_id),
        _ => None,
    }
}
fn operator(actor: &EngineeringActor) -> Result<(), StoreError> {
    if matches!(actor, EngineeringActor::Operator { .. }) {
        Ok(())
    } else {
        Err(conflict("operator authority required"))
    }
}
fn supervisor(actor: &EngineeringActor) -> Result<&str, StoreError> {
    if let EngineeringActor::Supervisor { execution_id, .. } = actor {
        Ok(execution_id)
    } else {
        Err(conflict("supervisor authority required"))
    }
}
fn load(connection: &Connection, id: &str) -> Result<EngineeringOutcomeSnapshot, StoreError> {
    let raw: String = connection
        .query_row(
            "SELECT v.snapshot_json FROM engineering_outcomes o JOIN engineering_versions v
         ON v.outcome_id = o.id AND v.revision = o.state_revision WHERE o.id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| StoreError::NotFound(id.into()))?;
    let mut result: EngineeringOutcomeSnapshot = decode(&raw)?;
    result.root = connection.query_row(
        "SELECT * FROM obligations WHERE id = ?1",
        [&result.root.id],
        obligation_from_row,
    )?;
    Ok(result)
}
fn save(
    tx: &Transaction<'_>,
    state: &mut EngineeringOutcomeSnapshot,
    now: i64,
    reason: &str,
) -> Result<i64, StoreError> {
    state.state_revision = state
        .state_revision
        .checked_add(1)
        .ok_or_else(|| conflict("revision exhausted"))?;
    state.root = require_obligation(tx, &state.root.id)?;
    tx.execute(
        "INSERT INTO engineering_versions(outcome_id, revision, snapshot_json) VALUES (?1, ?2, ?3)",
        params![state.id, state.state_revision, encode(state)?],
    )?;
    tx.execute(
        "UPDATE engineering_outcomes SET state_revision = ?2 WHERE id = ?1",
        params![state.id, state.state_revision],
    )?;
    append_event(
        tx,
        &state.root.id,
        state.root.occurrence,
        reason,
        now,
        Some(state.root.state),
        state.root.state,
        json!({"outcome_id": state.id, "contract_revision": state.contract_revision, "state_revision": state.state_revision}),
    )?;
    Ok(
        tx.query_row("SELECT MAX(sequence) FROM event_envelopes", [], |r| {
            r.get(0)
        })?,
    )
}
pub(super) fn reject_generic(tx: &Transaction<'_>, id: &str) -> Result<(), StoreError> {
    if binding(tx, id)?.is_some() {
        Err(conflict(
            "engineering obligations require the specialised supervision command",
        ))
    } else {
        Ok(())
    }
}
fn binding(connection: &Connection, id: &str) -> Result<Option<String>, StoreError> {
    Ok(connection
        .query_row(
            "SELECT outcome_id FROM engineering_bindings WHERE obligation_id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?)
}
pub(super) fn validate_renewal(
    tx: &Transaction<'_>,
    claim: &Claim,
    now: i64,
    lease_seconds: i64,
) -> Result<(), StoreError> {
    if let Some(id) = binding(tx, &claim.obligation_id)? {
        let state = load(tx, &id)?;
        let execution = state
            .executions
            .iter()
            .find(|e| {
                e.obligation_id == claim.obligation_id
                    && e.claim.lease_generation == claim.lease_generation
            })
            .ok_or(StoreError::Fenced)?;
        if execution.fenced
            || execution.contract_revision != state.contract_revision
            || state.cancellation_requested
        {
            return Err(StoreError::Fenced);
        }
        let started: i64 = tx.query_row(
            "SELECT claimed_at FROM attempts WHERE obligation_id = ?1 AND lease_generation = ?2",
            params![claim.obligation_id, claim.lease_generation],
            |r| r.get(0),
        )?;
        let turn_seconds = execution
            .package_id
            .as_ref()
            .and_then(|id| state.packages.iter().find(|p| p.id == *id))
            .map_or(state.contract().budget.turn_seconds, |p| {
                p.input.budget.turn_seconds
            });
        if now.saturating_add(lease_seconds)
            > started
                .saturating_add(turn_seconds)
                .min(state.contract().budget.deadline)
        {
            return Err(conflict("lease renewal exceeds the execution time budget"));
        }
    }
    Ok(())
}
pub(super) fn recover_expired(
    tx: &Transaction<'_>,
    id: &str,
    now: i64,
) -> Result<bool, StoreError> {
    let Some(outcome_id) = binding(tx, id)? else {
        return Ok(false);
    };
    let mut state = load(tx, &outcome_id)?;
    for execution in state
        .executions
        .iter_mut()
        .filter(|e| e.obligation_id == id && !e.fenced)
    {
        execution.fenced = true;
        execution.recovery_required = true;
    }
    apply_engineering_transition(
        tx,
        id,
        EngineeringTransition::Attention {
            reason: "lease expired; adapter must reconcile execution and retained writer ownership",
        },
        now,
    )?;
    wake(tx, &state, now)?;
    save(tx, &mut state, now, "engineering_lease_expired")?;
    Ok(true)
}
fn wake(
    tx: &Transaction<'_>,
    state: &EngineeringOutcomeSnapshot,
    now: i64,
) -> Result<(), StoreError> {
    apply_engineering_transition(tx, &state.root.id, EngineeringTransition::Wake, now)
}
fn validate_actor(
    tx: &Transaction<'_>,
    state: &EngineeringOutcomeSnapshot,
    actor: &EngineeringActor,
    now: i64,
) -> Result<(), StoreError> {
    if let EngineeringActor::Supervisor {
        execution_id,
        claim,
    }
    | EngineeringActor::Worker {
        execution_id,
        claim,
    } = actor
    {
        let execution = state
            .executions
            .iter()
            .find(|e| e.id == *execution_id)
            .ok_or(StoreError::Fenced)?;
        if execution.fenced
            || execution.contract_revision != state.contract_revision
            || claim != &execution.claim
            || (matches!(actor, EngineeringActor::Supervisor { .. })
                != (execution.role == EngineeringRole::Supervisor))
        {
            return Err(StoreError::Fenced);
        }
        let obligation = require_obligation(tx, &claim.obligation_id)?;
        verify_claim(&obligation, claim, now)?;
    }
    Ok(())
}

impl Store {
    pub fn engineering_outcome(
        &self,
        id: &str,
    ) -> Result<Option<EngineeringOutcomeSnapshot>, StoreError> {
        let read = |store: &Self| match load(&store.connection, id) {
            Ok(s) => Ok(Some(s)),
            Err(StoreError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        };
        if self.connection.is_autocommit() {
            self.with_deferred_read(read)
        } else {
            read(self)
        }
    }
    pub fn engineering_history_page(
        &self,
        id: &str,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<crate::ReadPage<AuditEvent>, StoreError> {
        let root: String = self
            .connection
            .query_row(
                "SELECT root_obligation_id FROM engineering_outcomes WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(id.into()))?;
        self.audit_event_page(&root, cursor, None, limit)
    }
    /// Replay the external plain-intent command using its original configured contract.
    pub fn engineering_operator_intake_receipt(
        &self,
        command_id: &str,
        intent: &str,
        operator_name: &str,
    ) -> Result<Option<EngineeringCommandReceipt>, StoreError> {
        identifier(command_id)?;
        identifier(operator_name)?;
        validate_bounded_text("engineering intent", intent, 16_384, false)?;
        let row: Option<(String, String, String)> = self.connection.query_row("SELECT envelope_json, actor_json, receipt_json FROM engineering_receipts WHERE command_id = ?1", [command_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
        let Some((raw, actor, receipt)) = row else {
            return Ok(None);
        };
        let envelope: EngineeringCommandEnvelope = decode(&raw)?;
        if actor
            != encode(&EngineeringActor::Operator {
                name: operator_name.into(),
            })?
            || !matches!(envelope.command, EngineeringCommand::CreateOutcome { contract } if contract.intent == intent)
        {
            return Err(conflict(
                "intake command ID already used for a different intent or actor",
            ));
        }
        Ok(Some(decode(&receipt)?))
    }
    pub fn engineering_outcome_ids(&self, limit: usize) -> Result<Vec<String>, StoreError> {
        if limit > 500 {
            return Err(StoreError::Invalid("at most 500 outcomes per read".into()));
        }
        let mut statement = self
            .connection
            .prepare("SELECT id FROM engineering_outcomes ORDER BY id LIMIT ?1")?;
        Ok(statement
            .query_map([limit as i64], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?)
    }
    pub fn engineering_binding(
        &self,
        id: &str,
    ) -> Result<Option<(String, Option<String>, EngineeringRole)>, StoreError> {
        let row: Option<(String, Option<String>, String)> = self.connection.query_row("SELECT outcome_id, package_id, role FROM engineering_bindings WHERE obligation_id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
        Ok(row.map(|(outcome, package, role)| {
            (
                outcome,
                package,
                if role == "supervisor" {
                    EngineeringRole::Supervisor
                } else {
                    EngineeringRole::Worker
                },
            )
        }))
    }
    pub fn is_engineering_obligation(&self, id: &str) -> Result<bool, StoreError> {
        Ok(binding(&self.connection, id)?.is_some())
    }
    pub fn engineering_command(
        &mut self,
        actor: EngineeringActor,
        envelope: EngineeringCommandEnvelope,
        now: i64,
    ) -> Result<EngineeringCommandReceipt, StoreError> {
        validate_envelope(&envelope, &actor)?;
        let payload_digest = digest(&(1_u32, &actor, &envelope))?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let receipt: Option<(String, String)> = tx.query_row("SELECT payload_digest, receipt_json FROM engineering_receipts WHERE command_id = ?1", [&envelope.command_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        if let Some((stored, raw)) = receipt {
            if stored != payload_digest {
                return Err(conflict(
                    "command ID reused with different payload, actor or precondition",
                ));
            }
            return decode(&raw);
        }
        let mut state = if let EngineeringCommand::CreateOutcome { contract } = &envelope.command {
            operator(&actor)?;
            if envelope.expected.is_some() {
                return Err(conflict("creation must not name an existing outcome"));
            }
            validate_contract(contract, now)?;
            let id = fresh_id();
            let root_id = fresh_id();
            create_obligation(&tx, &root_id, &contract.intent, now)?;
            tx.execute("INSERT INTO engineering_outcomes(id, root_obligation_id, state_revision) VALUES (?1, ?2, 1)", params![id, root_id])?;
            tx.execute("INSERT INTO engineering_bindings(obligation_id, outcome_id, role) VALUES (?1, ?2, 'supervisor')", params![root_id, id])?;
            EngineeringOutcomeSnapshot {
                id,
                root: require_obligation(&tx, &root_id)?,
                state_revision: 0,
                contract_revision: 1,
                contracts: vec![EngineeringContractRevision {
                    revision: 1,
                    contract: contract.clone(),
                    actor: actor_name(&actor),
                    at: now,
                }],
                messages: vec![EngineeringMessage {
                    text: contract.intent.clone(),
                    actor: actor_name(&actor),
                    at: now,
                }],
                processed_message_count: 0,
                packages: vec![],
                executions: vec![],
                questions: vec![],
                submissions: vec![],
                assessments: vec![],
                repairs: vec![],
                reconciliations: vec![],
                turns_used: 0,
                recoveries_used: 0,
                cancellation_requested: false,
                acceptance: None,
            }
        } else {
            let expected = envelope
                .expected
                .as_ref()
                .ok_or_else(|| conflict("outcome precondition required"))?;
            let state = load(&tx, &expected.outcome_id)?;
            if state.contract_revision != expected.contract_revision
                || state.state_revision != expected.state_revision
            {
                return Err(StoreError::Fenced);
            }
            validate_actor(&tx, &state, &actor, now)?;
            if state.root.state.is_terminal()
                && !matches!(&envelope.command,
                EngineeringCommand::RecordReconciliation(input) if state.acceptance.as_ref().is_some_and(|a| a.assessor_execution_id == input.execution_id) && input.recovered_submission.is_none())
            {
                return Err(conflict("outcome is terminal"));
            }
            state
        };
        let record_id = apply_command(&tx, &mut state, &actor, &envelope.command, now)?;
        let event_sequence = save(&tx, &mut state, now, "engineering_command")?;
        let receipt = EngineeringCommandReceipt {
            command_id: envelope.command_id.clone(),
            payload_digest,
            outcome_id: state.id.clone(),
            contract_revision: state.contract_revision,
            state_revision: state.state_revision,
            record_id,
            event_sequence,
        };
        tx.execute("INSERT INTO engineering_receipts(command_id,payload_digest,envelope_json,actor_json,receipt_json,outcome_id) VALUES (?1,?2,?3,?4,?5,?6)", params![envelope.command_id, receipt.payload_digest, encode(&envelope)?, encode(&actor)?, encode(&receipt)?, state.id])?;
        tx.commit()?;
        Ok(receipt)
    }

    pub fn claim_due_engineering(
        &mut self,
        role: EngineeringRole,
        now: i64,
        lease_seconds: i64,
        limit: usize,
    ) -> Result<Vec<EngineeringClaim>, StoreError> {
        if !(1..=3600).contains(&lease_seconds) || limit > 64 {
            return Err(StoreError::Invalid(
                "engineering claim bounds exceeded".into(),
            ));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        recover_expired_in_transaction(&tx, now)?;
        let candidates = {
            let mut statement = tx.prepare("SELECT b.outcome_id, b.obligation_id, b.package_id FROM engineering_bindings b JOIN obligations o ON o.id = b.obligation_id WHERE b.role = ?1 AND o.state = 'pending' AND o.next_wake_at <= ?2 ORDER BY o.next_wake_at, o.id LIMIT 500")?;
            statement
                .query_map(params![role_name(role), now], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut claims = vec![];
        for (outcome_id, obligation_id, package_id) in candidates {
            if claims.len() == limit {
                break;
            }
            let mut state = load(&tx, &outcome_id)?;
            if state.cancellation_requested
                || state.root.state.is_terminal()
                || (role == EngineeringRole::Worker
                    && (state.root.state == ObligationState::Attention
                        || missing_information_question(&state).is_some()))
            {
                continue;
            }
            let budget = state.contract().budget.clone();
            if state.turns_used >= budget.max_turns || now >= budget.deadline {
                attention(
                    &tx,
                    &state,
                    "execution budget exhausted; operator must revise the contract",
                    now,
                )?;
                save(&tx, &mut state, now, "engineering_budget_exhausted")?;
                continue;
            }
            let mut workspace = None;
            let mut turn_seconds = budget.turn_seconds;
            if let Some(package_id) = &package_id {
                let package = state
                    .packages
                    .iter()
                    .find(|p| p.id == *package_id)
                    .ok_or_else(|| conflict("package binding missing"))?;
                if package.contract_revision != state.contract_revision
                    || package.cancellation_requested
                    || package.superseded_by.is_some()
                    || state
                        .submissions
                        .iter()
                        .any(|s| s.package_id == *package_id)
                {
                    defer(&tx, &obligation_id, now)?;
                    continue;
                }
                let used = state
                    .executions
                    .iter()
                    .filter(|e| e.package_id.as_ref() == Some(package_id))
                    .count();
                if used >= package.input.budget.max_turns as usize
                    || now >= package.input.budget.deadline
                {
                    apply_engineering_transition(
                        &tx,
                        &obligation_id,
                        EngineeringTransition::Attention {
                            reason: "package execution budget exhausted",
                        },
                        now,
                    )?;
                    attention(
                        &tx,
                        &state,
                        "package execution budget exhausted; supervisor needs a contract decision",
                        now,
                    )?;
                    save(&tx, &mut state, now, "engineering_budget_exhausted")?;
                    continue;
                }
                if !dependencies_ready(&tx, &state, package)? {
                    defer(&tx, &obligation_id, now)?;
                    continue;
                }
                let running: u32 = tx.query_row("SELECT COUNT(*) FROM engineering_writers w JOIN engineering_dispatches d ON d.execution_id = w.execution_id WHERE d.outcome_id = ?1", [&state.id], |r| r.get(0))?;
                let occupied: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM engineering_writers WHERE workspace = ?1)",
                    [&package.input.workspace],
                    |r| r.get(0),
                )?;
                if occupied || running >= budget.max_concurrent_workers {
                    defer(&tx, &obligation_id, now)?;
                    continue;
                }
                workspace = Some(package.input.workspace.clone());
                turn_seconds = package.input.budget.turn_seconds;
            } else if state
                .executions
                .iter()
                .any(|e| e.role == EngineeringRole::Supervisor && !e.cessation_verified)
            {
                // Supervisors can also launch tools; their containment must be reconciled.
                attention(
                    &tx,
                    &state,
                    "previous supervisor must be reconciled before the next turn",
                    now,
                )?;
                save(&tx, &mut state, now, "engineering_reconciliation_required")?;
                continue;
            }
            let claim = apply_transition(
                &tx,
                Transition::Claim {
                    id: &obligation_id,
                    now,
                    lease_seconds: lease_seconds.min(turn_seconds).min(budget.deadline - now),
                },
            )?
            .claim
            .expect("claim transition");
            let execution_id = fresh_id();
            let instructions = match role {
                EngineeringRole::Supervisor => state.contract().supervisor.clone(),
                EngineeringRole::Worker => state.contract().worker.clone(),
            };
            let dispatch_key = fresh_id();
            let execution = EngineeringExecution {
                id: execution_id.clone(),
                obligation_id: obligation_id.clone(),
                package_id: package_id.clone(),
                contract_revision: state.contract_revision,
                role,
                claim: claim.clone(),
                dispatch_key: dispatch_key.clone(),
                instructions: instructions.clone(),
                workspace: workspace.clone(),
                fenced: false,
                cessation_verified: false,
                recovery_required: false,
                recovery_charged: false,
                checkpoints: vec![],
            };
            tx.execute("INSERT INTO engineering_dispatches(execution_id,outcome_id,obligation_id,lease_generation,dispatch_json) VALUES (?1,?2,?3,?4,?5)", params![execution_id, state.id, obligation_id, claim.lease_generation, encode(&execution)?])?;
            if let Some(workspace) = &workspace {
                tx.execute(
                    "INSERT INTO engineering_writers(workspace, execution_id) VALUES (?1, ?2)",
                    params![workspace, execution_id],
                )?;
            }
            state.executions.push(execution);
            state.turns_used += 1;
            save(&tx, &mut state, now, "engineering_dispatch_intent")?;
            claims.push(EngineeringClaim {
                claim,
                execution_id,
                outcome_id,
                package_id,
                contract_revision: state.contract_revision,
                state_revision: state.state_revision,
                instructions,
                workspace,
                dispatch_key,
                remaining_turns: budget.max_turns - state.turns_used,
            });
        }
        tx.commit()?;
        Ok(claims)
    }
}

fn defer(tx: &Transaction<'_>, id: &str, now: i64) -> Result<(), StoreError> {
    apply_engineering_transition(
        tx,
        id,
        EngineeringTransition::Pending {
            at: now.saturating_add(60),
            reason: "supervision prerequisite, assessment or writer ownership recheck",
        },
        now,
    )
}
fn create_obligation(
    tx: &Transaction<'_>,
    id: &str,
    description: &str,
    now: i64,
) -> Result<(), StoreError> {
    let new = NewObligation {
        id: id.into(),
        description: description.into(),
        scheduled_at: now,
        recurrence: None,
        approval_required: false,
        retry: crate::RetryPolicy::default(),
    };
    validate_new(&new)?;
    apply_transition(tx, Transition::Create { new, now })?;
    Ok(())
}
fn missing_information_question(
    state: &EngineeringOutcomeSnapshot,
) -> Option<&EngineeringQuestion> {
    state.questions.iter().find(|q| {
        q.kind == EngineeringQuestionKind::MissingInformation
            && q.contract_revision == state.contract_revision
            && q.resolution.is_none()
    })
}
fn waiting_for_operator(state: &EngineeringOutcomeSnapshot) -> bool {
    state
        .questions
        .iter()
        .any(|q| q.kind == EngineeringQuestionKind::NewAuthority && q.resolution.is_none())
        || (missing_information_question(state).is_some()
            && state.messages.len() <= state.processed_message_count)
}
fn missing_information_attention(
    tx: &Transaction<'_>,
    state: &EngineeringOutcomeSnapshot,
    now: i64,
) -> Result<(), StoreError> {
    let question = missing_information_question(state)
        .ok_or_else(|| conflict("missing information question is absent"))?;
    apply_engineering_transition(
        tx,
        &state.root.id,
        EngineeringTransition::HumanDecision {
            reason: &question.prompt,
        },
        now,
    )
}
fn attention(
    tx: &Transaction<'_>,
    state: &EngineeringOutcomeSnapshot,
    reason: &str,
    now: i64,
) -> Result<(), StoreError> {
    apply_engineering_transition(
        tx,
        &state.root.id,
        EngineeringTransition::Attention { reason },
        now,
    )
}
fn package_accepted(state: &EngineeringOutcomeSnapshot, package: &str) -> bool {
    state.assessments.iter().any(|a| {
        a.contract_revision == state.contract_revision
            && a.input.verdict == EngineeringVerdict::Accept
            && state.submissions.iter().any(|s| {
                s.id == a.input.submission_id
                    && s.package_id == package
                    && s.contract_revision == state.contract_revision
            })
    })
}
fn dependencies_ready(
    tx: &Transaction<'_>,
    state: &EngineeringOutcomeSnapshot,
    package: &EngineeringPackage,
) -> Result<bool, StoreError> {
    for dependency in &package.input.dependencies {
        let prerequisite = state
            .packages
            .iter()
            .find(|p| p.id == *dependency)
            .ok_or_else(|| conflict("missing prerequisite"))?;
        let obligation = require_obligation(tx, &prerequisite.obligation_id)?;
        if !package_accepted(state, dependency) || obligation.state != ObligationState::Completed {
            return Ok(false);
        }
    }
    Ok(true)
}
fn children_accepted(state: &EngineeringOutcomeSnapshot, parent: &str) -> bool {
    state
        .packages
        .iter()
        .filter(|p| p.input.parent_id.as_deref() == Some(parent))
        .all(|p| {
            package_accepted(state, &p.id)
                || p.superseded_by
                    .as_ref()
                    .is_some_and(|id| package_accepted(state, id))
        })
}
fn has_writer(tx: &Transaction<'_>, execution: &str) -> Result<bool, StoreError> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM engineering_writers WHERE execution_id = ?1)",
        [execution],
        |r| r.get(0),
    )?)
}
fn make_package(
    tx: &Transaction<'_>,
    state: &mut EngineeringOutcomeSnapshot,
    input: &NewEngineeringPackage,
    now: i64,
) -> Result<String, StoreError> {
    validate_package(input, state, now)?;
    if state.packages.len() >= state.contract().budget.max_packages as usize {
        return Err(conflict("package budget exhausted"));
    }
    // New packages can reference existing packages only. This construction order
    // makes cycles impossible without exposing a mutable edge operation.
    for id in input.dependencies.iter().chain(input.parent_id.iter()) {
        if !state.packages.iter().any(|p| {
            p.id == *id
                && p.contract_revision == state.contract_revision
                && !p.cancellation_requested
                && p.superseded_by.is_none()
        }) {
            return Err(conflict(
                "relationship must name a current package in this outcome",
            ));
        }
    }
    let id = fresh_id();
    let obligation_id = fresh_id();
    create_obligation(tx, &obligation_id, &input.instructions, now)?;
    tx.execute("INSERT INTO engineering_bindings(obligation_id,outcome_id,package_id,role) VALUES (?1,?2,?3,'worker')", params![obligation_id, state.id, id])?;
    state.packages.push(EngineeringPackage {
        id: id.clone(),
        obligation_id,
        contract_revision: state.contract_revision,
        input: input.clone(),
        superseded_by: None,
        cancellation_requested: false,
    });
    Ok(id)
}
fn submit(
    tx: &Transaction<'_>,
    state: &mut EngineeringOutcomeSnapshot,
    execution_id: &str,
    input: &EngineeringSubmissionInput,
    recovered: bool,
    now: i64,
) -> Result<String, StoreError> {
    validate_submission(input)?;
    let execution = state
        .executions
        .iter()
        .find(|e| e.id == execution_id)
        .ok_or(StoreError::Fenced)?
        .clone();
    let package_id = execution
        .package_id
        .as_ref()
        .ok_or_else(|| conflict("only workers submit package results"))?;
    let package = state
        .packages
        .iter()
        .find(|p| p.id == *package_id)
        .ok_or(StoreError::Fenced)?;
    if execution.contract_revision != state.contract_revision
        || state.cancellation_requested
        || package.cancellation_requested
        || package.superseded_by.is_some()
        || state
            .submissions
            .iter()
            .any(|s| s.package_id == *package_id)
    {
        return Err(StoreError::Fenced);
    }
    if !input
        .evidence
        .iter()
        .all(|e| package.input.criteria.contains(&e.criterion_id))
    {
        return Err(conflict(
            "submission evidence names a criterion outside its package",
        ));
    }
    let id = fresh_id();
    let submission_digest = digest(&(execution_id, package_id, state.contract_revision, input))?;
    state.submissions.push(EngineeringSubmission {
        id: id.clone(),
        execution_id: execution_id.into(),
        package_id: package_id.clone(),
        contract_revision: state.contract_revision,
        digest: submission_digest,
        input: input.clone(),
        recovered,
    });
    state
        .executions
        .iter_mut()
        .find(|e| e.id == execution_id)
        .expect("execution found")
        .fenced = true;
    apply_engineering_transition(
        tx,
        &execution.obligation_id,
        EngineeringTransition::Pending {
            at: now.saturating_add(60),
            reason: "result saved; supervisor acceptance pending",
        },
        now,
    )?;
    wake(tx, state, now)?;
    Ok(id)
}
fn apply_command(
    tx: &Transaction<'_>,
    state: &mut EngineeringOutcomeSnapshot,
    actor: &EngineeringActor,
    command: &EngineeringCommand,
    now: i64,
) -> Result<Option<String>, StoreError> {
    if state.cancellation_requested
        && !matches!(
            command,
            EngineeringCommand::RecordReconciliation(_)
                | EngineeringCommand::ReviseContract { .. }
                | EngineeringCommand::RequestCancellation { .. }
                | EngineeringCommand::FollowUp { .. }
        )
    {
        return Err(StoreError::Fenced);
    }
    match command {
        EngineeringCommand::CreateOutcome { .. } => {}
        EngineeringCommand::FollowUp { text } => {
            operator(actor)?;
            if state.messages.len() >= 64 {
                return Err(conflict("conversation message budget exhausted"));
            }
            state.messages.push(EngineeringMessage {
                text: text.clone(),
                actor: actor_name(actor),
                at: now,
            });
            if missing_information_question(state).is_some()
                && !waiting_for_operator(state)
                && !state
                    .executions
                    .iter()
                    .any(|e| e.role == EngineeringRole::Supervisor && !e.cessation_verified)
            {
                apply_engineering_transition(
                    tx,
                    &state.root.id,
                    EngineeringTransition::Pending {
                        at: now,
                        reason: "new user information requires supervisor reconsideration",
                    },
                    now,
                )?;
            }
            wake(tx, state, now)?;
        }
        EngineeringCommand::ReviseContract { contract }
        | EngineeringCommand::FormaliseContract { contract } => {
            validate_contract(contract, now)?;
            if matches!(command, EngineeringCommand::FormaliseContract { .. }) {
                supervisor(actor)?;
                let old = state.contract();
                if old.authority != contract.authority
                    || old.permitted_scope != contract.permitted_scope
                    || old.prohibited_effects != contract.prohibited_effects
                    || old.budget != contract.budget
                    || old.supervisor != contract.supervisor
                    || old.worker != contract.worker
                    || old.intent != contract.intent
                {
                    return Err(conflict(
                        "supervisor formalisation may only define criteria within the operator contract",
                    ));
                }
            } else {
                operator(actor)?;
            }
            if state.contracts.len() >= 64 {
                return Err(conflict("contract revision bound reached"));
            }
            state.contract_revision += 1;
            state.contracts.push(EngineeringContractRevision {
                revision: state.contract_revision,
                contract: contract.clone(),
                actor: actor_name(actor),
                at: now,
            });
            for execution in &mut state.executions {
                execution.fenced = true;
                if !execution.cessation_verified {
                    apply_engineering_transition(
                        tx,
                        &execution.obligation_id,
                        EngineeringTransition::Attention {
                            reason: "contract revised; previous execution requires reconciliation",
                        },
                        now,
                    )?;
                }
            }
            for package in &state.packages {
                let obligation = require_obligation(tx, &package.obligation_id)?;
                if !obligation.state.is_terminal() {
                    apply_engineering_transition(
                        tx,
                        &package.obligation_id,
                        EngineeringTransition::Attention {
                            reason: "package belongs to a superseded contract; supervisor must replan",
                        },
                        now,
                    )?;
                }
            }
            if state
                .executions
                .iter()
                .any(|e| e.role == EngineeringRole::Supervisor && !e.cessation_verified)
            {
                attention(
                    tx,
                    state,
                    "contract revised; reconcile prior supervisor before replacement",
                    now,
                )?;
            } else {
                apply_engineering_transition(
                    tx,
                    &state.root.id,
                    EngineeringTransition::Pending {
                        at: now,
                        reason: "contract revised; supervisor must replan",
                    },
                    now,
                )?;
            }
            if state.cancellation_requested {
                settle_cancellation(tx, state, now)?;
            }
        }
        EngineeringCommand::CreatePackage(input) => {
            supervisor(actor)?;
            if state.contract().criteria.is_empty() {
                return Err(conflict(
                    "formalise acceptance criteria before creating packages",
                ));
            }
            if state.packages.len() >= state.contract().budget.max_packages as usize {
                attention(
                    tx,
                    state,
                    "package budget exhausted; operator must revise contract",
                    now,
                )?;
            } else {
                return Ok(Some(make_package(tx, state, input, now)?));
            }
        }
        EngineeringCommand::RecordCheckpoint {
            runtime_identity,
            request_identity,
            cursor,
            summary,
            evidence_digest,
        } => {
            let id =
                actor_execution(actor).ok_or_else(|| conflict("execution authority required"))?;
            let count: usize = state.executions.iter().map(|e| e.checkpoints.len()).sum();
            if count >= state.contract().budget.max_checkpoints as usize {
                attention(tx, state, "checkpoint budget exhausted", now)?;
            } else {
                let execution = state
                    .executions
                    .iter_mut()
                    .find(|e| e.id == id)
                    .ok_or(StoreError::Fenced)?;
                if execution
                    .checkpoints
                    .first()
                    .is_some_and(|c| c.runtime_identity != *runtime_identity)
                {
                    return Err(conflict("runtime identity is immutable for an execution"));
                }
                execution.checkpoints.push(EngineeringCheckpoint {
                    sequence: execution.checkpoints.len() as u64 + 1,
                    runtime_identity: runtime_identity.clone(),
                    request_identity: request_identity.clone(),
                    cursor: cursor.clone(),
                    summary: summary.clone(),
                    evidence_digest: evidence_digest.clone(),
                    at: now,
                });
            }
        }
        EngineeringCommand::AskQuestion {
            request_key,
            kind,
            prompt,
            options,
        } => {
            if *kind == EngineeringQuestionKind::MissingInformation {
                supervisor(actor)?;
            }
            let execution_id = actor_execution(actor)
                .ok_or_else(|| conflict("execution authority required"))?
                .to_owned();
            if state
                .questions
                .iter()
                .any(|q| q.execution_id == execution_id && q.request_key == *request_key)
            {
                return Err(conflict(
                    "runtime question already recorded; replay its command receipt",
                ));
            }
            if state.questions.len() >= state.contract().budget.max_questions as usize {
                attention(tx, state, "question budget exhausted", now)?;
            } else {
                let id = fresh_id();
                state.questions.push(EngineeringQuestion {
                    id: id.clone(),
                    execution_id,
                    contract_revision: state.contract_revision,
                    request_key: request_key.clone(),
                    kind: *kind,
                    prompt: prompt.clone(),
                    options: options.clone(),
                    resolution: None,
                });
                if *kind == EngineeringQuestionKind::MissingInformation {
                    let execution = state
                        .executions
                        .iter_mut()
                        .find(|e| {
                            e.id == state.questions.last().expect("saved question").execution_id
                        })
                        .expect("supervisor execution");
                    execution.fenced = true;
                    state.processed_message_count = state.messages.len();
                    missing_information_attention(tx, state, now)?;
                } else if *kind == EngineeringQuestionKind::NewAuthority {
                    let execution = state
                        .executions
                        .iter_mut()
                        .find(|e| {
                            e.id == state.questions.last().expect("saved question").execution_id
                        })
                        .expect("actor execution");
                    execution.fenced = true;
                    apply_engineering_transition(
                        tx,
                        &execution.obligation_id,
                        EngineeringTransition::Attention {
                            reason: "new authority requested; reconcile paused execution",
                        },
                        now,
                    )?;
                    for execution in state
                        .executions
                        .iter_mut()
                        .filter(|e| e.role == EngineeringRole::Supervisor)
                    {
                        execution.fenced = true;
                    }
                    attention(
                        tx,
                        state,
                        "new authority requested; operator contract decision required",
                        now,
                    )?;
                } else {
                    wake(tx, state, now)?;
                }
                return Ok(Some(id));
            }
        }
        EngineeringCommand::ResolveQuestion {
            question_id,
            answer,
            authority_grants,
        } => {
            let question = state
                .questions
                .iter()
                .find(|q| q.id == *question_id)
                .ok_or_else(|| conflict("unknown question"))?
                .clone();
            if question.resolution.is_some() {
                return Err(conflict("question already resolved"));
            }
            if question.kind == EngineeringQuestionKind::MissingInformation {
                operator(actor)?;
                if question.contract_revision != state.contract_revision {
                    return Err(StoreError::Fenced);
                }
                if !authority_grants.is_empty() {
                    return Err(conflict(
                        "missing information answers cannot grant authority",
                    ));
                }
            } else if question.kind == EngineeringQuestionKind::NewAuthority {
                operator(actor)?;
                if !authority_grants.is_empty()
                    && state.contract_revision <= question.contract_revision
                {
                    return Err(conflict(
                        "new authority requires a new operator contract revision before resolution",
                    ));
                }
            } else {
                supervisor(actor)?;
                if question.contract_revision != state.contract_revision {
                    return Err(StoreError::Fenced);
                }
            }
            if authority_grants
                .iter()
                .any(|g| !state.contract().authority.iter().any(|a| a.grant == *g))
            {
                return Err(conflict(
                    "resolution names authority outside current contract",
                ));
            }
            state
                .questions
                .iter_mut()
                .find(|q| q.id == *question_id)
                .expect("question found")
                .resolution = Some(EngineeringResolution {
                answer: answer.clone(),
                actor: actor_name(actor),
                authority_grants: authority_grants.clone(),
                contract_revision: state.contract_revision,
                at: now,
            });
            if question.kind.needs_operator()
                && !state.questions.iter().any(|q| {
                    q.kind.needs_operator()
                        && q.resolution.is_none()
                        && (q.kind == EngineeringQuestionKind::NewAuthority
                            || q.contract_revision == state.contract_revision)
                })
                && !state
                    .executions
                    .iter()
                    .any(|e| e.role == EngineeringRole::Supervisor && !e.cessation_verified)
            {
                apply_engineering_transition(
                    tx,
                    &state.root.id,
                    EngineeringTransition::Pending {
                        at: now,
                        reason: "operator answer retained; supervisor decision due within existing contract",
                    },
                    now,
                )?;
            }
            wake(tx, state, now)?;
        }
        EngineeringCommand::SubmitResult(input) => {
            let EngineeringActor::Worker { execution_id, .. } = actor else {
                return Err(conflict("worker authority required"));
            };
            return Ok(Some(submit(tx, state, execution_id, input, false, now)?));
        }
        EngineeringCommand::AssessResult(input) => {
            let assessor = supervisor(actor)?;
            let submission = state
                .submissions
                .iter()
                .find(|s| s.id == input.submission_id)
                .ok_or_else(|| conflict("unknown submission"))?
                .clone();
            if submission.contract_revision != state.contract_revision
                || submission.digest != input.submission_digest
                || submission.execution_id == assessor
            {
                return Err(StoreError::Fenced);
            }
            if state
                .assessments
                .iter()
                .any(|a| a.input.submission_id == submission.id)
            {
                return Err(conflict("submission already assessed"));
            }
            let package = state
                .packages
                .iter()
                .find(|p| p.id == submission.package_id)
                .ok_or(StoreError::Fenced)?
                .clone();
            if package.cancellation_requested || package.superseded_by.is_some() {
                return Err(StoreError::Fenced);
            }
            validate_review(
                &input.review,
                &submission.input.artefacts,
                &[&submission.execution_id, assessor],
            )?;
            if input.verdict == EngineeringVerdict::Accept {
                if !input.unmet_criteria.is_empty() || !submission.input.limitations.is_empty() {
                    return Err(conflict(
                        "acceptance cannot retain unmet criteria or limitations",
                    ));
                }
                if has_writer(tx, &submission.execution_id)?
                    || !state
                        .executions
                        .iter()
                        .find(|e| e.id == submission.execution_id)
                        .is_some_and(|e| e.cessation_verified)
                {
                    return Err(conflict("submission writer cessation is unverified"));
                }
                for criterion in &package.input.criteria {
                    if !submission
                        .input
                        .evidence
                        .iter()
                        .any(|e| e.criterion_id == *criterion && e.exit_code == 0)
                    {
                        return Err(conflict(
                            "acceptance lacks successful evidence for every package criterion",
                        ));
                    }
                }
                if !children_accepted(state, &package.id)
                    || !dependencies_ready(tx, state, &package)?
                {
                    return Err(conflict(
                        "required children or prerequisites remain unaccepted",
                    ));
                }
                apply_engineering_transition(
                    tx,
                    &package.obligation_id,
                    EngineeringTransition::Accepted,
                    now,
                )?;
            } else if input.unmet_criteria.is_empty()
                || input
                    .unmet_criteria
                    .iter()
                    .any(|c| !package.input.criteria.contains(c))
            {
                return Err(conflict("repair must name precise package criteria"));
            }
            let id = fresh_id();
            state.assessments.push(EngineeringAssessment {
                id: id.clone(),
                assessor_execution_id: assessor.into(),
                contract_revision: state.contract_revision,
                input: input.clone(),
            });
            wake(tx, state, now)?;
            return Ok(Some(id));
        }
        EngineeringCommand::CreateRepair {
            assessment_id,
            replacement,
        } => {
            supervisor(actor)?;
            let assessment = state
                .assessments
                .iter()
                .find(|a| a.id == *assessment_id)
                .ok_or_else(|| conflict("unknown assessment"))?
                .clone();
            if assessment.contract_revision != state.contract_revision
                || assessment.input.verdict != EngineeringVerdict::Repair
                || state
                    .repairs
                    .iter()
                    .any(|r| r.assessment_id == *assessment_id)
            {
                return Err(conflict(
                    "repair requires an unconsumed current repair assessment",
                ));
            }
            let submission = state
                .submissions
                .iter()
                .find(|s| s.id == assessment.input.submission_id)
                .expect("assessed submission")
                .clone();
            let package = state
                .packages
                .iter()
                .find(|p| p.id == submission.package_id)
                .expect("submitted package")
                .clone();
            if replacement.criteria != package.input.criteria
                || replacement.parent_id != package.input.parent_id
                || replacement.dependencies != package.input.dependencies
            {
                return Err(conflict(
                    "repair must preserve all package criteria, containment and prerequisites",
                ));
            }
            if state.repairs.len() >= state.contract().budget.max_repairs as usize
                || state.packages.len() >= state.contract().budget.max_packages as usize
            {
                attention(
                    tx,
                    state,
                    "repair budget exhausted; operator contract decision required",
                    now,
                )?;
            } else {
                if has_writer(tx, &submission.execution_id)? {
                    return Err(conflict(
                        "reconcile previous writer before commissioning repair",
                    ));
                }
                let id = make_package(tx, state, replacement, now)?;
                state
                    .packages
                    .iter_mut()
                    .find(|p| p.id == package.id)
                    .expect("package found")
                    .superseded_by = Some(id.clone());
                state.repairs.push(EngineeringRepair {
                    assessment_id: assessment_id.clone(),
                    replaced_package_id: package.id,
                    replacement_package_id: id.clone(),
                });
                apply_engineering_transition(
                    tx,
                    &package.obligation_id,
                    EngineeringTransition::Cancelled,
                    now,
                )?;
                return Ok(Some(id));
            }
        }
        EngineeringCommand::YieldSupervisor {
            next_wake_at,
            reason,
            processed_message_count,
        } => {
            let execution_id = supervisor(actor)?;
            if *next_wake_at < now || *next_wake_at > now.saturating_add(3600) {
                return Err(conflict(
                    "supervisor continuation must wake within one hour",
                ));
            }
            if *processed_message_count != state.messages.len() {
                return Err(StoreError::Fenced);
            }
            state.processed_message_count = *processed_message_count;
            state
                .executions
                .iter_mut()
                .find(|e| e.id == execution_id)
                .expect("validated execution")
                .fenced = true;
            if missing_information_question(state).is_some() && waiting_for_operator(state) {
                missing_information_attention(tx, state, now)?;
                return Ok(None);
            }
            let incoming =
                state.questions.iter().any(|q| {
                    q.resolution.is_none() && q.contract_revision == state.contract_revision
                }) || state.submissions.iter().any(|s| {
                    s.contract_revision == state.contract_revision
                        && !state
                            .assessments
                            .iter()
                            .any(|a| a.input.submission_id == s.id)
                });
            apply_engineering_transition(
                tx,
                &state.root.id,
                EngineeringTransition::Pending {
                    at: if incoming { now } else { *next_wake_at },
                    reason,
                },
                now,
            )?;
        }
        EngineeringCommand::RequestCancellation { package_id } => {
            if !matches!(
                actor,
                EngineeringActor::Operator { .. } | EngineeringActor::Supervisor { .. }
            ) {
                return Err(conflict("operator or supervisor authority required"));
            }
            cancel_packages(tx, state, package_id.as_deref(), now)?;
        }
        EngineeringCommand::RecordReconciliation(input) => {
            let EngineeringActor::Reconciler { adapter_id } = actor else {
                return Err(conflict("trusted reconciler required"));
            };
            let execution = state
                .executions
                .iter()
                .find(|e| e.id == input.execution_id)
                .ok_or(StoreError::Fenced)?
                .clone();
            if execution.instructions.adapter_id != *adapter_id {
                return Err(StoreError::Fenced);
            }
            if execution
                .checkpoints
                .first()
                .is_some_and(|c| c.runtime_identity != input.runtime_identity)
            {
                return Err(conflict(
                    "reconciliation runtime identity differs from dispatch checkpoint",
                ));
            }
            if state.reconciliations.len() >= 512 {
                return Err(conflict("reconciliation record bound reached"));
            }
            let charge_recovery = execution.recovery_required && !execution.recovery_charged;
            if charge_recovery && state.recoveries_used >= state.contract().budget.max_recoveries {
                attention(
                    tx,
                    state,
                    "recovery budget exhausted; retained writers require operator budget decision",
                    now,
                )?;
            } else {
                if charge_recovery {
                    state.recoveries_used += 1;
                    state
                        .executions
                        .iter_mut()
                        .find(|e| e.id == execution.id)
                        .expect("execution found")
                        .recovery_charged = true;
                }
                state.reconciliations.push(EngineeringReconciliation {
                    adapter_id: adapter_id.clone(),
                    input: input.clone(),
                    at: now,
                });
                if input.reaped_boundary.is_some() {
                    let position = state
                        .executions
                        .iter()
                        .position(|e| e.id == input.execution_id)
                        .expect("execution found");
                    state.executions[position].cessation_verified = true;
                    state.executions[position].fenced = true;
                    tx.execute(
                        "DELETE FROM engineering_writers WHERE execution_id = ?1",
                        [&input.execution_id],
                    )?;
                    if let Some(submission) = &input.recovered_submission {
                        // Offline import is a reconciler operation, never stale worker authority.
                        if state.executions.iter().any(|e| {
                            e.obligation_id == execution.obligation_id
                                && e.claim.lease_generation > execution.claim.lease_generation
                        }) {
                            return Err(StoreError::Fenced);
                        }
                        let id = submit(tx, state, &input.execution_id, submission, true, now)?;
                        settle_cancellation(tx, state, now)?;
                        return Ok(Some(id));
                    }
                    let cancelled = state.cancellation_requested
                        || execution.package_id.as_ref().is_some_and(|id| {
                            state
                                .packages
                                .iter()
                                .any(|p| p.id == *id && p.cancellation_requested)
                        });
                    let obligation = require_obligation(tx, &execution.obligation_id)?;
                    let newest = !state.executions.iter().any(|e| {
                        e.obligation_id == execution.obligation_id
                            && e.claim.lease_generation > execution.claim.lease_generation
                    });
                    if !cancelled && newest && !obligation.state.is_terminal() {
                        if execution.role == EngineeringRole::Supervisor {
                            if !waiting_for_operator(state) {
                                apply_engineering_transition(
                                    tx,
                                    &execution.obligation_id,
                                    EngineeringTransition::Pending {
                                        at: obligation.next_wake_at.unwrap_or(now),
                                        reason: "previous supervisor cessation verified; next decision due",
                                    },
                                    now,
                                )?;
                            }
                        } else if execution.contract_revision == state.contract_revision {
                            apply_engineering_transition(
                                tx,
                                &execution.obligation_id,
                                EngineeringTransition::Pending {
                                    at: now,
                                    reason: "writer cessation verified; supervisor owns next action",
                                },
                                now,
                            )?;
                        }
                    }
                    settle_cancellation(tx, state, now)?;
                } else if input.recovered_submission.is_some() {
                    return Err(conflict("offline import requires verified cessation"));
                }
                if execution.role == EngineeringRole::Worker || execution.recovery_required {
                    wake(tx, state, now)?;
                }
            }
        }
        EngineeringCommand::FinishOutcome {
            assessment_ids,
            review,
            processed_message_count,
        } => {
            let assessor = supervisor(actor)?;
            if state.contract().criteria.is_empty()
                || *processed_message_count != state.messages.len()
            {
                return Err(conflict(
                    "outcome contract or conversation is not fully assessed",
                ));
            }
            if state
                .questions
                .iter()
                .any(|q| q.resolution.is_none() && q.contract_revision == state.contract_revision)
            {
                return Err(conflict("unresolved question blocks acceptance"));
            }
            if state
                .executions
                .iter()
                .any(|e| !e.cessation_verified && e.id != assessor)
            {
                return Err(conflict("unreconciled execution blocks acceptance"));
            }
            if state.packages.iter().any(|p| {
                p.contract_revision == state.contract_revision
                    && p.superseded_by.is_none()
                    && !package_accepted(state, &p.id)
            }) {
                return Err(conflict("required package remains unaccepted"));
            }
            let mut covered = BTreeSet::new();
            let mut artefacts = vec![];
            let mut excluded = vec![assessor];
            for id in assessment_ids {
                let assessment = state
                    .assessments
                    .iter()
                    .find(|a| {
                        a.id == *id
                            && a.contract_revision == state.contract_revision
                            && a.input.verdict == EngineeringVerdict::Accept
                    })
                    .ok_or_else(|| {
                        conflict("final acceptance requires exact current accepted assessments")
                    })?;
                let submission = state
                    .submissions
                    .iter()
                    .find(|s| s.id == assessment.input.submission_id)
                    .expect("assessed submission");
                excluded.push(&submission.execution_id);
                for evidence in &submission.input.evidence {
                    if evidence.exit_code == 0 {
                        covered.insert(evidence.criterion_id.as_str());
                    }
                }
                for artefact in &submission.input.artefacts {
                    if !artefacts.contains(artefact) {
                        artefacts.push(artefact.clone());
                    }
                }
            }
            if state
                .contract()
                .criteria
                .iter()
                .any(|c| !covered.contains(c.id.as_str()))
            {
                return Err(conflict(
                    "final acceptance lacks every current contract criterion",
                ));
            }
            validate_review(review, &artefacts, &excluded)?;
            state.acceptance = Some(EngineeringAcceptance {
                assessor_execution_id: assessor.into(),
                contract_revision: state.contract_revision,
                assessment_ids: assessment_ids.clone(),
                review: review.clone(),
                at: now,
            });
            state.processed_message_count = *processed_message_count;
            apply_engineering_transition(tx, &state.root.id, EngineeringTransition::Accepted, now)?;
        }
    }
    Ok(None)
}

fn cancel_packages(
    tx: &Transaction<'_>,
    state: &mut EngineeringOutcomeSnapshot,
    package_id: Option<&str>,
    now: i64,
) -> Result<(), StoreError> {
    let mut owned = BTreeSet::new();
    if let Some(id) = package_id {
        if !state.packages.iter().any(|p| p.id == id) {
            return Err(conflict("unknown cancellation package"));
        }
        owned.insert(id.to_owned());
        loop {
            let before = owned.len();
            for package in &state.packages {
                if package
                    .input
                    .parent_id
                    .as_ref()
                    .is_some_and(|p| owned.contains(p))
                {
                    owned.insert(package.id.clone());
                }
            }
            if owned.len() == before {
                break;
            }
        }
    } else {
        state.cancellation_requested = true;
        owned.extend(state.packages.iter().map(|p| p.id.clone()));
    }
    for package in &mut state.packages {
        if owned.contains(&package.id) {
            package.cancellation_requested = true;
        }
    }
    for execution in &mut state.executions {
        if state.cancellation_requested
            || execution
                .package_id
                .as_ref()
                .is_some_and(|p| owned.contains(p))
        {
            execution.fenced = true;
        }
    }
    settle_cancellation(tx, state, now)?;
    wake(tx, state, now)
}
fn settle_cancellation(
    tx: &Transaction<'_>,
    state: &EngineeringOutcomeSnapshot,
    now: i64,
) -> Result<(), StoreError> {
    for package in state.packages.iter().filter(|p| p.cancellation_requested) {
        let unresolved = state
            .executions
            .iter()
            .any(|e| e.package_id.as_ref() == Some(&package.id) && !e.cessation_verified);
        apply_engineering_transition(
            tx,
            &package.obligation_id,
            if unresolved {
                EngineeringTransition::Attention {
                    reason: "cancellation requested; adapter must prove writer cessation",
                }
            } else {
                EngineeringTransition::Cancelled
            },
            now,
        )?;
    }
    if state.cancellation_requested {
        let unresolved = state.executions.iter().any(|e| !e.cessation_verified);
        apply_engineering_transition(
            tx,
            &state.root.id,
            if unresolved {
                EngineeringTransition::Attention {
                    reason: "outcome cancellation awaits execution and descendant reconciliation",
                }
            } else {
                EngineeringTransition::Cancelled
            },
            now,
        )?;
    }
    Ok(())
}
fn hash(value: &str) -> Result<(), StoreError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(StoreError::Invalid(
            "SHA-256 must be 64 lowercase hexadecimal digits".into(),
        ));
    }
    Ok(())
}
fn identifier(value: &str) -> Result<(), StoreError> {
    validate_bounded_text("engineering identity", value, 256, false)
}
fn budget(value: &EngineeringBudget, now: i64) -> Result<(), StoreError> {
    if value.max_turns == 0
        || value.max_turns > 256
        || value.max_packages == 0
        || value.max_packages > 64
        || value.max_repairs > 64
        || value.max_recoveries == 0
        || value.max_recoveries > 512
        || value.max_checkpoints == 0
        || value.max_checkpoints > 512
        || value.max_questions == 0
        || value.max_questions > 64
        || value.max_concurrent_workers == 0
        || value.max_concurrent_workers > 16
        || !(1..=3600).contains(&value.turn_seconds)
        || value.deadline <= now
        || value.deadline > now.saturating_add(31_536_000)
    {
        return Err(StoreError::Invalid(
            "finite engineering budget is outside supported bounds".into(),
        ));
    }
    Ok(())
}
fn instructions(value: &EngineeringInstructions) -> Result<(), StoreError> {
    validate_bounded_text("instructions", &value.text, 16_384, false)?;
    hash(&value.digest)?;
    hash(&value.profile_digest)?;
    identifier(&value.adapter_id)?;
    if value.digest != format!("{:x}", Sha256::digest(value.text.as_bytes())) {
        return Err(StoreError::Invalid(
            "instruction digest does not match text".into(),
        ));
    }
    for digest in &value.context_digests {
        hash(digest)?;
    }
    Ok(())
}
fn validate_contract(value: &EngineeringContract, now: i64) -> Result<(), StoreError> {
    validate_bounded_text("engineering intent", &value.intent, 16_384, false)?;
    budget(&value.budget, now)?;
    instructions(&value.supervisor)?;
    instructions(&value.worker)?;
    let mut ids = BTreeSet::new();
    for criterion in &value.criteria {
        identifier(&criterion.id)?;
        validate_bounded_text("criterion", &criterion.description, 16_384, false)?;
        if !ids.insert(&criterion.id) {
            return Err(StoreError::Invalid("duplicate criterion ID".into()));
        }
    }
    let mut grants = BTreeSet::new();
    for grant in &value.authority {
        identifier(&grant.grant)?;
        hash(&grant.evidence_digest)?;
        if !grants.insert(&grant.grant) {
            return Err(StoreError::Invalid("duplicate authority grant".into()));
        }
    }
    Ok(())
}
fn validate_artefact(value: &EngineeringArtefact) -> Result<(), StoreError> {
    match value {
        EngineeringArtefact::Git {
            repository,
            commit,
            tree,
        } => {
            identifier(repository)?;
            for hash in [commit, tree] {
                if ![40, 64].contains(&hash.len())
                    || !hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(StoreError::Invalid(
                        "Git artefact needs exact hexadecimal commit and tree identities".into(),
                    ));
                }
            }
        }
        EngineeringArtefact::File {
            store,
            path,
            sha256,
            ..
        } => {
            identifier(store)?;
            identifier(path)?;
            hash(sha256)?;
            if path.starts_with('/')
                || path.contains('\\')
                || path
                    .split('/')
                    .any(|p| p.is_empty() || p == "." || p == "..")
            {
                return Err(StoreError::Invalid(
                    "file artefact needs a relative path within its approved store".into(),
                ));
            }
        }
    }
    Ok(())
}
fn validate_package(
    value: &NewEngineeringPackage,
    state: &EngineeringOutcomeSnapshot,
    now: i64,
) -> Result<(), StoreError> {
    identifier(&value.workspace)?;
    budget(&value.budget, now)?;
    if !state.contract().permitted_scope.contains(&value.workspace) {
        return Err(conflict(
            "package workspace is outside the operator contract scope",
        ));
    }
    if value.criteria.is_empty()
        || value
            .criteria
            .iter()
            .any(|id| !state.contract().criteria.iter().any(|c| c.id == *id))
    {
        return Err(conflict("package must name current contract criteria"));
    }
    if value.budget.deadline > state.contract().budget.deadline
        || value.budget.max_turns > state.contract().budget.max_turns
    {
        return Err(conflict("package budget exceeds outcome budget"));
    }
    if value.dependencies.iter().collect::<BTreeSet<_>>().len() != value.dependencies.len()
        || value.criteria.iter().collect::<BTreeSet<_>>().len() != value.criteria.len()
    {
        return Err(conflict("duplicate package reference"));
    }
    for artefact in &value.inputs {
        validate_artefact(artefact)?;
    }
    Ok(())
}
fn validate_submission(value: &EngineeringSubmissionInput) -> Result<(), StoreError> {
    if value.artefacts.is_empty() {
        return Err(conflict("submission requires immutable artefacts"));
    }
    for artefact in &value.artefacts {
        validate_artefact(artefact)?;
    }
    let mut criteria = BTreeSet::new();
    for evidence in &value.evidence {
        identifier(&evidence.criterion_id)?;
        validate_artefact(&evidence.artefact)?;
        hash(&evidence.command_digest)?;
        hash(&evidence.output_digest)?;
        if !value.artefacts.contains(&evidence.artefact) || !criteria.insert(&evidence.criterion_id)
        {
            return Err(conflict(
                "criterion evidence must uniquely name a submitted exact artefact",
            ));
        }
    }
    Ok(())
}
fn validate_review(
    review: &EngineeringReviewEvidence,
    artefacts: &[EngineeringArtefact],
    excluded: &[&str],
) -> Result<(), StoreError> {
    identifier(&review.reviewer_identity)?;
    hash(&review.evidence_digest)?;
    if excluded.contains(&review.reviewer_identity.as_str())
        || review.artefacts.len() != artefacts.len()
        || artefacts.iter().any(|a| !review.artefacts.contains(a))
    {
        return Err(conflict(
            "independent review must cover the exact submitted artefacts",
        ));
    }
    for artefact in &review.artefacts {
        validate_artefact(artefact)?;
    }
    Ok(())
}
fn validate_envelope(
    envelope: &EngineeringCommandEnvelope,
    actor: &EngineeringActor,
) -> Result<(), StoreError> {
    identifier(&envelope.command_id)?;
    // A deterministic struct encoding, versioned in the receipt digest. Reject
    // overlong strings, NUL and excessive collections before opening SQLite.
    let encoded = encode(&(actor, envelope))?;
    if encoded.len() > 262_144 {
        return Err(StoreError::Invalid(
            "engineering command exceeds 256 KiB".into(),
        ));
    }
    fn walk(value: &serde_json::Value) -> Result<(), StoreError> {
        match value {
            serde_json::Value::String(s) => {
                validate_bounded_text("engineering command text", s, 16_384, true)
            }
            serde_json::Value::Array(values) => {
                if values.len() > 64 {
                    return Err(StoreError::Invalid(
                        "at most 64 references per command".into(),
                    ));
                }
                for value in values {
                    walk(value)?;
                }
                Ok(())
            }
            serde_json::Value::Object(values) => {
                for value in values.values() {
                    walk(value)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    walk(&serde_json::from_str(&encoded).map_err(|e| StoreError::Invalid(e.to_string()))?)?;
    match actor {
        EngineeringActor::Operator { name } => identifier(name)?,
        EngineeringActor::Reconciler { adapter_id } => identifier(adapter_id)?,
        EngineeringActor::Worker { execution_id, .. }
        | EngineeringActor::Supervisor { execution_id, .. } => identifier(execution_id)?,
    }
    if let Some(expected) = &envelope.expected {
        identifier(&expected.outcome_id)?;
    }
    match &envelope.command {
        EngineeringCommand::RecordCheckpoint {
            runtime_identity,
            request_identity,
            cursor,
            summary,
            evidence_digest,
        } => {
            identifier(runtime_identity)?;
            identifier(cursor)?;
            if let Some(id) = request_identity {
                identifier(id)?;
            }
            validate_bounded_text("checkpoint summary", summary, 4096, true)?;
            hash(evidence_digest)?;
        }
        EngineeringCommand::AskQuestion {
            request_key,
            prompt,
            ..
        } => {
            identifier(request_key)?;
            validate_bounded_text("question prompt", prompt, 16_384, false)?;
        }
        EngineeringCommand::RecordReconciliation(input) => {
            identifier(&input.execution_id)?;
            identifier(&input.runtime_identity)?;
            hash(&input.evidence_digest)?;
            if let Some(boundary) = &input.reaped_boundary {
                identifier(boundary)?;
            }
            if let Some(submission) = &input.recovered_submission {
                validate_submission(submission)?;
            }
        }
        EngineeringCommand::SubmitResult(input) => validate_submission(input)?,
        EngineeringCommand::FollowUp { text } => {
            validate_bounded_text("follow-up", text, 16_384, false)?
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sha(text: &str) -> String {
        format!("{:x}", Sha256::digest(text.as_bytes()))
    }
    fn instructions() -> EngineeringInstructions {
        EngineeringInstructions {
            text: "bounded engineering".into(),
            digest: sha("bounded engineering"),
            context_digests: vec![sha("guidance")],
            profile_digest: sha("profile"),
            adapter_id: "test-adapter".into(),
        }
    }
    fn budget() -> EngineeringBudget {
        EngineeringBudget {
            max_turns: 20,
            max_packages: 16,
            max_repairs: 4,
            max_recoveries: 32,
            max_checkpoints: 32,
            max_questions: 16,
            max_concurrent_workers: 2,
            turn_seconds: 600,
            deadline: 10_000,
        }
    }
    fn contract() -> EngineeringContract {
        EngineeringContract {
            intent: "Build the fixture".into(),
            criteria: vec![EngineeringCriterion {
                id: "works".into(),
                description: "fixture works".into(),
            }],
            permitted_scope: vec!["fixture-workspace".into(), "other-workspace".into()],
            prohibited_effects: vec!["publication".into()],
            authority: vec![EngineeringAuthority {
                grant: "local-engineering".into(),
                evidence_digest: sha("operator decision"),
            }],
            supervisor: instructions(),
            worker: instructions(),
            budget: budget(),
        }
    }
    fn op() -> EngineeringActor {
        EngineeringActor::Operator {
            name: "operator".into(),
        }
    }
    fn actor(claim: &EngineeringClaim) -> EngineeringActor {
        if claim.package_id.is_some() {
            EngineeringActor::Worker {
                execution_id: claim.execution_id.clone(),
                claim: claim.claim.clone(),
            }
        } else {
            EngineeringActor::Supervisor {
                execution_id: claim.execution_id.clone(),
                claim: claim.claim.clone(),
            }
        }
    }
    fn envelope(
        store: &Store,
        id: &str,
        command: EngineeringCommand,
    ) -> EngineeringCommandEnvelope {
        EngineeringCommandEnvelope {
            command_id: fresh_id(),
            expected: Some(
                store
                    .engineering_outcome(id)
                    .unwrap()
                    .unwrap()
                    .precondition(),
            ),
            command,
        }
    }
    fn command(
        store: &mut Store,
        id: &str,
        actor: EngineeringActor,
        command: EngineeringCommand,
        now: i64,
    ) -> EngineeringCommandReceipt {
        let envelope = envelope(store, id, command);
        store.engineering_command(actor, envelope, now).unwrap()
    }
    fn create(store: &mut Store) -> String {
        store
            .engineering_command(
                op(),
                EngineeringCommandEnvelope {
                    command_id: fresh_id(),
                    expected: None,
                    command: EngineeringCommand::CreateOutcome {
                        contract: contract(),
                    },
                },
                100,
            )
            .unwrap()
            .outcome_id
    }
    fn claim(store: &mut Store, role: EngineeringRole, now: i64) -> EngineeringClaim {
        store
            .claim_due_engineering(role, now, 600, 1)
            .unwrap()
            .pop()
            .unwrap()
    }
    fn package() -> NewEngineeringPackage {
        NewEngineeringPackage {
            parent_id: None,
            dependencies: vec![],
            instructions: "Implement fixture".into(),
            criteria: vec!["works".into()],
            inputs: vec![],
            workspace: "fixture-workspace".into(),
            budget: budget(),
        }
    }
    fn new_package(store: &mut Store, root: &EngineeringClaim, now: i64) -> String {
        command(
            store,
            &root.outcome_id,
            actor(root),
            EngineeringCommand::CreatePackage(package()),
            now,
        )
        .record_id
        .unwrap()
    }
    fn artefact() -> EngineeringArtefact {
        EngineeringArtefact::File {
            store: "approved-fixture".into(),
            path: "result.md".into(),
            bytes: 6,
            sha256: sha("result"),
        }
    }
    fn submission() -> EngineeringSubmissionInput {
        EngineeringSubmissionInput {
            artefacts: vec![artefact()],
            evidence: vec![EngineeringCriterionEvidence {
                criterion_id: "works".into(),
                artefact: artefact(),
                command_digest: sha("check"),
                output_digest: sha("passed"),
                exit_code: 0,
            }],
            limitations: String::new(),
        }
    }
    fn review() -> EngineeringReviewEvidence {
        EngineeringReviewEvidence {
            reviewer_identity: "independent-review-execution".into(),
            artefacts: vec![artefact()],
            evidence_digest: sha("review"),
        }
    }
    fn reconcile(
        store: &mut Store,
        c: &EngineeringClaim,
        result: Option<EngineeringSubmissionInput>,
        now: i64,
    ) -> EngineeringCommandReceipt {
        command(
            store,
            &c.outcome_id,
            EngineeringActor::Reconciler {
                adapter_id: "test-adapter".into(),
            },
            EngineeringCommand::RecordReconciliation(EngineeringReconciliationInput {
                execution_id: c.execution_id.clone(),
                runtime_identity: "runtime-1".into(),
                observation: "private PID namespace reaped".into(),
                evidence_digest: sha("waitpid proof"),
                reaped_boundary: Some("boundary-1".into()),
                recovered_submission: result,
            }),
            now,
        )
    }
    fn assessment(
        store: &Store,
        id: &str,
        verdict: EngineeringVerdict,
    ) -> EngineeringAssessmentInput {
        let snapshot = store.engineering_outcome(id).unwrap().unwrap();
        let submission = snapshot.submissions.last().unwrap();
        EngineeringAssessmentInput {
            submission_id: submission.id.clone(),
            submission_digest: submission.digest.clone(),
            verdict,
            unmet_criteria: if verdict == EngineeringVerdict::Repair {
                vec!["works".into()]
            } else {
                vec![]
            },
            review: review(),
        }
    }
    #[test]
    fn replay_survives_reopen_and_checks_full_actor_payload_and_preconditions() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("state.sqlite");
        let mut store = Store::open(&path).unwrap();
        let envelope = EngineeringCommandEnvelope {
            command_id: "stable-create".into(),
            expected: None,
            command: EngineeringCommand::CreateOutcome {
                contract: contract(),
            },
        };
        let receipt = store
            .engineering_command(op(), envelope.clone(), 100)
            .unwrap();
        drop(store);
        let mut store = Store::open(&path).unwrap();
        assert_eq!(
            store
                .engineering_command(op(), envelope.clone(), 200)
                .unwrap()
                .outcome_id,
            receipt.outcome_id
        );
        assert!(matches!(
            store.engineering_command(
                EngineeringActor::Operator {
                    name: "different".into()
                },
                envelope.clone(),
                200
            ),
            Err(StoreError::Conflict(_))
        ));
        let mut changed = envelope;
        changed.command = EngineeringCommand::CreateOutcome {
            contract: EngineeringContract {
                intent: "different".into(),
                ..contract()
            },
        };
        assert!(matches!(
            store.engineering_command(op(), changed, 200),
            Err(StoreError::Conflict(_))
        ));
        assert_eq!(store.engineering_outcome_ids(20).unwrap().len(), 1);
        assert_eq!(
            store
                .engineering_operator_intake_receipt(
                    "stable-create",
                    "Build the fixture",
                    "operator"
                )
                .unwrap()
                .unwrap()
                .outcome_id,
            receipt.outcome_id
        );
        assert!(
            store
                .engineering_operator_intake_receipt(
                    "stable-create",
                    "Different intent",
                    "operator"
                )
                .is_err()
        );
        assert!(
            store
                .engineering_operator_intake_receipt(
                    "stable-create",
                    "Build the fixture",
                    "other operator"
                )
                .is_err()
        );
        let forbidden = store
            .connection
            .execute("UPDATE engineering_versions SET snapshot_json = '{}'", []);
        assert!(forbidden.is_err());
    }
    #[test]
    fn fake_generic_routes_and_worker_self_acceptance_are_fenced() {
        let mut store = Store::open_in_memory().unwrap();
        let id = create(&mut store);
        let snapshot = store.engineering_outcome(&id).unwrap().unwrap();
        assert!(store.claim_due(100, 100, 5).unwrap().is_empty());
        assert!(store.claim_due_gardener(100, 100, 5).unwrap().is_empty());
        assert!(store.cancel(&snapshot.root.id, 100).is_err());
        assert!(store.retry_attention(&snapshot.root.id, 100).is_err());
        assert!(
            store
                .decide_approval(
                    &snapshot.root.id,
                    ApprovalDecision::Approved,
                    "operator",
                    None,
                    100
                )
                .is_err()
        );
        let root = claim(&mut store, EngineeringRole::Supervisor, 100);
        assert!(
            store
                .complete(&root.claim, Completion::Succeeded { evidence: None }, 101)
                .is_err()
        );
        new_package(&mut store, &root, 101);
        let worker = claim(&mut store, EngineeringRole::Worker, 102);
        let finish = envelope(
            &store,
            &id,
            EngineeringCommand::FinishOutcome {
                assessment_ids: vec![],
                review: review(),
                processed_message_count: 1,
            },
        );
        assert!(
            store
                .engineering_command(actor(&worker), finish, 103)
                .is_err()
        );
    }
    #[test]
    fn submission_handover_is_atomic_pending_until_independent_exact_assessment() {
        let mut store = Store::open_in_memory().unwrap();
        let id = create(&mut store);
        let root = claim(&mut store, EngineeringRole::Supervisor, 100);
        let package_id = new_package(&mut store, &root, 101);
        let worker = claim(&mut store, EngineeringRole::Worker, 102);
        command(
            &mut store,
            &id,
            actor(&worker),
            EngineeringCommand::SubmitResult(submission()),
            103,
        );
        assert_eq!(
            store
                .get(&worker.claim.obligation_id)
                .unwrap()
                .unwrap()
                .state,
            ObligationState::Pending
        );
        assert_eq!(
            store.get(&root.claim.obligation_id).unwrap().unwrap().state,
            ObligationState::Running
        );
        assert!(
            store
                .claim_due_engineering(EngineeringRole::Worker, 200, 600, 5)
                .unwrap()
                .is_empty()
        );
        let accept = assessment(&store, &id, EngineeringVerdict::Accept);
        let env = envelope(
            &store,
            &id,
            EngineeringCommand::AssessResult(accept.clone()),
        );
        assert!(store.engineering_command(actor(&root), env, 104).is_err());
        reconcile(&mut store, &worker, None, 104);
        let mut wrong = accept.clone();
        if let EngineeringArtefact::File { sha256, .. } = &mut wrong.review.artefacts[0] {
            *sha256 = sha("wrong bytes");
        }
        let env = envelope(&store, &id, EngineeringCommand::AssessResult(wrong));
        assert!(store.engineering_command(actor(&root), env, 105).is_err());
        let accepted = command(
            &mut store,
            &id,
            actor(&root),
            EngineeringCommand::AssessResult(accept),
            105,
        )
        .record_id
        .unwrap();
        assert!(package_accepted(
            &store.engineering_outcome(&id).unwrap().unwrap(),
            &package_id
        ));
        command(
            &mut store,
            &id,
            actor(&root),
            EngineeringCommand::FinishOutcome {
                assessment_ids: vec![accepted],
                review: review(),
                processed_message_count: 1,
            },
            106,
        );
        assert_eq!(
            store.get(&root.claim.obligation_id).unwrap().unwrap().state,
            ObligationState::Completed
        );
        reconcile(&mut store, &root, None, 107);
        let state = store.engineering_outcome(&id).unwrap().unwrap();
        assert_eq!(state.root.state, ObligationState::Completed);
        assert_eq!(state.recoveries_used, 0);
        assert!(state.executions.iter().all(|e| e.cessation_verified));
    }
    #[test]
    fn expired_lease_retains_writer_and_only_trusted_reconciler_imports_offline_result() {
        let mut store = Store::open_in_memory().unwrap();
        let id = create(&mut store);
        let root = claim(&mut store, EngineeringRole::Supervisor, 100);
        new_package(&mut store, &root, 101);
        let worker = claim(&mut store, EngineeringRole::Worker, 102);
        assert_eq!(store.recover_expired_leases(703).unwrap(), 2);
        assert_eq!(
            store
                .get(&worker.claim.obligation_id)
                .unwrap()
                .unwrap()
                .state,
            ObligationState::Attention
        );
        let tx = store.connection.unchecked_transaction().unwrap();
        assert!(has_writer(&tx, &worker.execution_id).unwrap());
        tx.commit().unwrap();
        assert!(
            store
                .claim_due_engineering(EngineeringRole::Worker, 703, 600, 10)
                .unwrap()
                .is_empty()
        );
        let env = envelope(&store, &id, EngineeringCommand::SubmitResult(submission()));
        assert!(matches!(
            store.engineering_command(actor(&worker), env, 704),
            Err(StoreError::Fenced)
        ));
        reconcile(&mut store, &worker, Some(submission()), 704);
        let state = store.engineering_outcome(&id).unwrap().unwrap();
        assert!(state.submissions[0].recovered);
        assert_eq!(state.root.state, ObligationState::Attention);
        reconcile(&mut store, &root, None, 705);
        assert_eq!(
            store
                .engineering_outcome(&id)
                .unwrap()
                .unwrap()
                .root
                .next_wake_at,
            Some(705)
        );
    }
    #[test]
    fn contract_and_message_watermarks_reject_stale_decisions_and_results() {
        let mut store = Store::open_in_memory().unwrap();
        let id = create(&mut store);
        let root = claim(&mut store, EngineeringRole::Supervisor, 100);
        new_package(&mut store, &root, 101);
        let worker = claim(&mut store, EngineeringRole::Worker, 102);
        let stale = envelope(
            &store,
            &id,
            EngineeringCommand::YieldSupervisor {
                next_wake_at: 200,
                reason: "waiting".into(),
                processed_message_count: 1,
            },
        );
        command(
            &mut store,
            &id,
            op(),
            EngineeringCommand::FollowUp {
                text: "also support Unicode".into(),
            },
            103,
        );
        assert!(matches!(
            store.engineering_command(actor(&root), stale, 104),
            Err(StoreError::Fenced)
        ));
        let mut revision = contract();
        revision.criteria[0].description = "new acceptance".into();
        command(
            &mut store,
            &id,
            op(),
            EngineeringCommand::ReviseContract { contract: revision },
            105,
        );
        let env = envelope(&store, &id, EngineeringCommand::SubmitResult(submission()));
        assert!(matches!(
            store.engineering_command(actor(&worker), env, 106),
            Err(StoreError::Fenced)
        ));
        assert!(store.renew_lease(&worker.claim, 106, 100).is_err());
        let env = envelope(
            &store,
            &id,
            EngineeringCommand::RecordReconciliation(EngineeringReconciliationInput {
                execution_id: worker.execution_id.clone(),
                runtime_identity: "runtime-1".into(),
                observation: "done".into(),
                evidence_digest: sha("proof"),
                reaped_boundary: Some("boundary".into()),
                recovered_submission: Some(submission()),
            }),
        );
        assert!(matches!(
            store.engineering_command(
                EngineeringActor::Reconciler {
                    adapter_id: "test-adapter".into()
                },
                env,
                106
            ),
            Err(StoreError::Fenced)
        ));
    }
    #[test]
    fn cancellation_owns_descendants_not_dependency_neighbours_and_retains_writers() {
        let mut store = Store::open_in_memory().unwrap();
        let id = create(&mut store);
        let root = claim(&mut store, EngineeringRole::Supervisor, 100);
        let parent = new_package(&mut store, &root, 101);
        let mut child = package();
        child.parent_id = Some(parent.clone());
        child.workspace = "other-workspace".into();
        let child_id = command(
            &mut store,
            &id,
            actor(&root),
            EngineeringCommand::CreatePackage(child),
            102,
        )
        .record_id
        .unwrap();
        let mut dependent = package();
        dependent.dependencies = vec![parent.clone()];
        let dependent_id = command(
            &mut store,
            &id,
            actor(&root),
            EngineeringCommand::CreatePackage(dependent),
            103,
        )
        .record_id
        .unwrap();
        let workers = store
            .claim_due_engineering(EngineeringRole::Worker, 104, 600, 2)
            .unwrap();
        assert_eq!(workers.len(), 2);
        command(
            &mut store,
            &id,
            op(),
            EngineeringCommand::RequestCancellation {
                package_id: Some(parent.clone()),
            },
            105,
        );
        let state = store.engineering_outcome(&id).unwrap().unwrap();
        assert!(
            state
                .packages
                .iter()
                .find(|p| p.id == child_id)
                .unwrap()
                .cancellation_requested
        );
        assert!(
            !state
                .packages
                .iter()
                .find(|p| p.id == dependent_id)
                .unwrap()
                .cancellation_requested
        );
        for worker in &workers {
            assert_eq!(
                store
                    .get(&worker.claim.obligation_id)
                    .unwrap()
                    .unwrap()
                    .state,
                ObligationState::Attention
            );
            reconcile(&mut store, worker, None, 106);
        }
        assert!(
            store
                .claim_due_engineering(EngineeringRole::Worker, 107, 600, 2)
                .unwrap()
                .is_empty()
        );
        command(
            &mut store,
            &id,
            op(),
            EngineeringCommand::RequestCancellation { package_id: None },
            108,
        );
        assert_eq!(
            store.engineering_outcome(&id).unwrap().unwrap().root.state,
            ObligationState::Attention
        );
        reconcile(&mut store, &root, None, 109);
        assert_eq!(
            store.engineering_outcome(&id).unwrap().unwrap().root.state,
            ObligationState::Cancelled
        );
    }
    #[test]
    fn repair_is_atomic_deduplicated_and_cannot_drop_criteria() {
        let mut store = Store::open_in_memory().unwrap();
        let id = create(&mut store);
        let root = claim(&mut store, EngineeringRole::Supervisor, 100);
        new_package(&mut store, &root, 101);
        let worker = claim(&mut store, EngineeringRole::Worker, 102);
        let mut incomplete = submission();
        incomplete.evidence.clear();
        incomplete.limitations = "missing check".into();
        reconcile(&mut store, &worker, Some(incomplete), 103);
        let assessment = assessment(&store, &id, EngineeringVerdict::Repair);
        let assessment_id = command(
            &mut store,
            &id,
            actor(&root),
            EngineeringCommand::AssessResult(assessment),
            104,
        )
        .record_id
        .unwrap();
        let mut bad = package();
        bad.criteria.clear();
        let env = envelope(
            &store,
            &id,
            EngineeringCommand::CreateRepair {
                assessment_id: assessment_id.clone(),
                replacement: bad,
            },
        );
        assert!(store.engineering_command(actor(&root), env, 105).is_err());
        let env = envelope(
            &store,
            &id,
            EngineeringCommand::CreateRepair {
                assessment_id: assessment_id.clone(),
                replacement: package(),
            },
        );
        let receipt = store
            .engineering_command(actor(&root), env.clone(), 105)
            .unwrap();
        assert_eq!(
            store
                .engineering_command(actor(&root), env, 106)
                .unwrap()
                .record_id,
            receipt.record_id
        );
        let state = store.engineering_outcome(&id).unwrap().unwrap();
        assert_eq!(state.repairs.len(), 1);
        assert_eq!(state.packages.len(), 2);
        let env = envelope(
            &store,
            &id,
            EngineeringCommand::CreateRepair {
                assessment_id,
                replacement: package(),
            },
        );
        assert!(store.engineering_command(actor(&root), env, 106).is_err());
        assert_eq!(
            store
                .claim_due_engineering(EngineeringRole::Worker, 106, 600, 2)
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn package_relationships_require_current_same_outcome_and_budget_exhaustion_is_durable() {
        let mut store = Store::open_in_memory().unwrap();
        let id = create(&mut store);
        let root = claim(&mut store, EngineeringRole::Supervisor, 100);
        let mut missing = package();
        missing.dependencies = vec!["missing-or-self".into()];
        let env = envelope(&store, &id, EngineeringCommand::CreatePackage(missing));
        assert!(store.engineering_command(actor(&root), env, 101).is_err());
        let mut outside = package();
        outside.workspace = "unapproved-workspace".into();
        let env = envelope(&store, &id, EngineeringCommand::CreatePackage(outside));
        assert!(store.engineering_command(actor(&root), env, 101).is_err());
        let mut revised = contract();
        revised.budget.max_turns = 1;
        command(
            &mut store,
            &id,
            op(),
            EngineeringCommand::ReviseContract { contract: revised },
            102,
        );
        reconcile(&mut store, &root, None, 103);
        assert!(
            store
                .claim_due_engineering(EngineeringRole::Supervisor, 104, 600, 1)
                .unwrap()
                .is_empty()
        );
        let state = store.engineering_outcome(&id).unwrap().unwrap();
        assert_eq!(state.root.state, ObligationState::Attention);
        assert!(state.root.last_error.unwrap().contains("budget exhausted"));
        assert_eq!(state.turns_used, 1);
    }
    #[test]
    fn routine_question_wakes_supervisor_and_answer_receipt_is_exact_and_replayable() {
        let mut store = Store::open_in_memory().unwrap();
        let id = create(&mut store);
        let root = claim(&mut store, EngineeringRole::Supervisor, 100);
        new_package(&mut store, &root, 101);
        command(
            &mut store,
            &id,
            actor(&root),
            EngineeringCommand::YieldSupervisor {
                next_wake_at: 500,
                reason: "waiting for worker".into(),
                processed_message_count: 1,
            },
            102,
        );
        reconcile(&mut store, &root, None, 103);
        assert_eq!(
            store
                .engineering_outcome(&id)
                .unwrap()
                .unwrap()
                .root
                .next_wake_at,
            Some(500)
        );
        let worker = claim(&mut store, EngineeringRole::Worker, 104);
        let question_id = command(
            &mut store,
            &id,
            actor(&worker),
            EngineeringCommand::AskQuestion {
                request_key: "broker-1/thread-1/turn-1/item-1/request-0".into(),
                kind: EngineeringQuestionKind::Routine,
                prompt: "Which fixture?".into(),
                options: vec!["local fixture".into()],
            },
            105,
        )
        .record_id
        .unwrap();
        let supervisor = claim(&mut store, EngineeringRole::Supervisor, 105);
        let env = envelope(
            &store,
            &id,
            EngineeringCommand::ResolveQuestion {
                question_id: question_id.clone(),
                answer: "Use the local fixture".into(),
                authority_grants: vec!["local-engineering".into()],
            },
        );
        let receipt = store
            .engineering_command(actor(&supervisor), env.clone(), 106)
            .unwrap();
        assert_eq!(
            store
                .engineering_command(actor(&supervisor), env, 107)
                .unwrap()
                .event_sequence,
            receipt.event_sequence
        );
        let env = envelope(
            &store,
            &id,
            EngineeringCommand::ResolveQuestion {
                question_id,
                answer: "another answer".into(),
                authority_grants: vec![],
            },
        );
        assert!(
            store
                .engineering_command(actor(&supervisor), env, 107)
                .is_err()
        );
        assert_eq!(
            store
                .engineering_outcome(&id)
                .unwrap()
                .unwrap()
                .questions
                .len(),
            1
        );
    }
    #[test]
    fn authority_question_fences_writers_and_operator_reply_cannot_grant_authority() {
        let mut store = Store::open_in_memory().unwrap();
        let id = create(&mut store);
        let root = claim(&mut store, EngineeringRole::Supervisor, 100);
        new_package(&mut store, &root, 101);
        let worker = claim(&mut store, EngineeringRole::Worker, 102);
        let question_id = command(
            &mut store,
            &id,
            actor(&worker),
            EngineeringCommand::AskQuestion {
                request_key: "broker/thread/turn/item/request".into(),
                kind: EngineeringQuestionKind::NewAuthority,
                prompt: "May I deploy?".into(),
                options: vec![],
            },
            103,
        )
        .record_id
        .unwrap();
        assert_eq!(
            store.engineering_outcome(&id).unwrap().unwrap().root.state,
            ObligationState::Attention
        );
        assert!(store.renew_lease(&worker.claim, 104, 100).is_err());
        reconcile(&mut store, &worker, None, 104);
        reconcile(&mut store, &root, None, 105);
        let env = envelope(
            &store,
            &id,
            EngineeringCommand::ResolveQuestion {
                question_id: question_id.clone(),
                answer: "Deploy".into(),
                authority_grants: vec!["deployment".into()],
            },
        );
        assert!(store.engineering_command(op(), env, 106).is_err());
        command(
            &mut store,
            &id,
            op(),
            EngineeringCommand::ResolveQuestion {
                question_id,
                answer: "Stay local".into(),
                authority_grants: vec![],
            },
            106,
        );
        let state = store.engineering_outcome(&id).unwrap().unwrap();
        assert_eq!(state.contract_revision, 1);
        assert_eq!(state.contract().authority, contract().authority);
        assert_eq!(state.root.state, ObligationState::Pending);
    }
    #[test]
    fn ceased_execution_can_be_replaced_but_old_worker_and_offline_import_remain_fenced() {
        let mut store = Store::open_in_memory().unwrap();
        let id = create(&mut store);
        let root = claim(&mut store, EngineeringRole::Supervisor, 100);
        new_package(&mut store, &root, 101);
        let worker = claim(&mut store, EngineeringRole::Worker, 102);
        assert!(store.renew_lease(&worker.claim, 103, 600).is_err());
        reconcile(&mut store, &worker, None, 104);
        let replacement = claim(&mut store, EngineeringRole::Worker, 105);
        assert_ne!(worker.execution_id, replacement.execution_id);
        let env = envelope(&store, &id, EngineeringCommand::SubmitResult(submission()));
        assert!(matches!(
            store.engineering_command(actor(&worker), env, 106),
            Err(StoreError::Fenced)
        ));
        let env = envelope(
            &store,
            &id,
            EngineeringCommand::RecordReconciliation(EngineeringReconciliationInput {
                execution_id: worker.execution_id,
                runtime_identity: "runtime-1".into(),
                observation: "late old result".into(),
                evidence_digest: sha("proof"),
                reaped_boundary: Some("old-boundary".into()),
                recovered_submission: Some(submission()),
            }),
        );
        assert!(matches!(
            store.engineering_command(
                EngineeringActor::Reconciler {
                    adapter_id: "test-adapter".into()
                },
                env,
                106
            ),
            Err(StoreError::Fenced)
        ));
        assert!(
            store
                .engineering_outcome(&id)
                .unwrap()
                .unwrap()
                .submissions
                .is_empty()
        );
        let tx = store.connection.unchecked_transaction().unwrap();
        assert!(has_writer(&tx, &replacement.execution_id).unwrap());
        tx.commit().unwrap();
    }
    #[test]
    fn cancellation_recovery_budget_can_be_increased_without_reviving_authority() {
        let mut store = Store::open_in_memory().unwrap();
        let mut original = contract();
        original.budget.max_recoveries = 1;
        let id = store
            .engineering_command(
                op(),
                EngineeringCommandEnvelope {
                    command_id: fresh_id(),
                    expected: None,
                    command: EngineeringCommand::CreateOutcome {
                        contract: original.clone(),
                    },
                },
                100,
            )
            .unwrap()
            .outcome_id;
        let root = claim(&mut store, EngineeringRole::Supervisor, 100);
        new_package(&mut store, &root, 101);
        let worker = claim(&mut store, EngineeringRole::Worker, 102);
        store.recover_expired_leases(703).unwrap();
        reconcile(&mut store, &worker, None, 704);
        command(
            &mut store,
            &id,
            op(),
            EngineeringCommand::RequestCancellation { package_id: None },
            705,
        );
        reconcile(&mut store, &root, None, 706);
        assert_eq!(
            store.engineering_outcome(&id).unwrap().unwrap().root.state,
            ObligationState::Attention
        );
        original.budget.max_recoveries = 2;
        command(
            &mut store,
            &id,
            op(),
            EngineeringCommand::ReviseContract { contract: original },
            707,
        );
        assert!(
            store
                .claim_due_engineering(EngineeringRole::Supervisor, 708, 600, 1)
                .unwrap()
                .is_empty()
        );
        reconcile(&mut store, &root, None, 708);
        let state = store.engineering_outcome(&id).unwrap().unwrap();
        assert_eq!(state.root.state, ObligationState::Cancelled);
        assert_eq!(state.recoveries_used, 2);
    }
    #[test]
    fn missing_information_waits_for_operator_and_fresh_followup_gets_one_reconsideration() {
        let mut store = Store::open_in_memory().unwrap();
        let id = create(&mut store);
        let root = claim(&mut store, EngineeringRole::Supervisor, 100);
        let question_id = command(
            &mut store,
            &id,
            actor(&root),
            EngineeringCommand::AskQuestion {
                request_key: "supervisor/missing-source-location".into(),
                kind: EngineeringQuestionKind::MissingInformation,
                prompt: "Which local directory contains the source notes?".into(),
                options: vec![],
            },
            101,
        )
        .record_id
        .unwrap();
        let state = store.engineering_outcome(&id).unwrap().unwrap();
        assert_eq!(state.root.state, ObligationState::Attention);
        assert_eq!(
            state.root.failure_disposition,
            Some(FailureDisposition::HumanDecision)
        );
        assert_eq!(
            state.root.last_error.as_deref(),
            Some("Which local directory contains the source notes?")
        );
        assert_eq!(
            state.questions[0].kind,
            EngineeringQuestionKind::MissingInformation
        );
        assert_eq!(
            serde_json::to_string(&state.questions[0].kind).unwrap(),
            "\"missing_information\""
        );
        reconcile(&mut store, &root, None, 102);
        for now in [103, 104] {
            assert!(
                store
                    .claim_due_engineering(EngineeringRole::Supervisor, now, 600, 1)
                    .unwrap()
                    .is_empty()
            );
        }
        assert_eq!(
            store.engineering_outcome(&id).unwrap().unwrap().turns_used,
            1
        );
        command(
            &mut store,
            &id,
            op(),
            EngineeringCommand::FollowUp {
                text: "The source is a local notes directory, not a remote service".into(),
            },
            105,
        );
        let reconsider = claim(&mut store, EngineeringRole::Supervisor, 106);
        command(
            &mut store,
            &id,
            actor(&reconsider),
            EngineeringCommand::YieldSupervisor {
                next_wake_at: 200,
                reason: "Still need the exact directory".into(),
                processed_message_count: 2,
            },
            107,
        );
        reconcile(&mut store, &reconsider, None, 108);
        let state = store.engineering_outcome(&id).unwrap().unwrap();
        assert_eq!(state.root.state, ObligationState::Attention);
        assert_eq!(
            state.root.failure_disposition,
            Some(FailureDisposition::HumanDecision)
        );
        assert!(
            store
                .claim_due_engineering(EngineeringRole::Supervisor, 109, 600, 1)
                .unwrap()
                .is_empty()
        );
        let widened = envelope(
            &store,
            &id,
            EngineeringCommand::ResolveQuestion {
                question_id: question_id.clone(),
                answer: "Use the fixture".into(),
                authority_grants: vec!["local-engineering".into()],
            },
        );
        assert!(store.engineering_command(op(), widened, 110).is_err());
        command(
            &mut store,
            &id,
            op(),
            EngineeringCommand::ResolveQuestion {
                question_id,
                answer: "Use fixture-workspace".into(),
                authority_grants: vec![],
            },
            111,
        );
        let state = store.engineering_outcome(&id).unwrap().unwrap();
        assert_eq!(state.root.state, ObligationState::Pending);
        assert_eq!(state.contract_revision, 1);
        assert_eq!(state.contract().authority, contract().authority);
        assert_eq!(
            state.questions[0].resolution.as_ref().unwrap().answer,
            "Use fixture-workspace"
        );
        assert_eq!(
            store
                .claim_due_engineering(EngineeringRole::Supervisor, 112, 600, 1)
                .unwrap()
                .len(),
            1
        );
    }
}
