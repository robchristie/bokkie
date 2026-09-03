//! Authoritative read-only operator projections over the existing durable store.

use std::collections::{BTreeMap, BTreeSet};

use bokkie_operator_api::{
    ActionCapability, ActionConsequence, ApprovalSubject, AttentionCause, DisabledReason,
    DurableLiveness, ExceptionReason, ObligationTopic, OperatorCapabilities, OperatorObligation,
    OperatorObligationState, OperatorSnapshot, TopicItem, TopicSource,
};
use rusqlite::params;
use serde::Serialize;

use crate::{
    ApprovalDecision, GardenerImplementationRun, GardenerVerificationVerdict, Obligation,
    ObligationState, Proposal, Store, StoreError,
    store::{
        approval_transition_is_legal, cancel_transition_is_legal,
        gardener_proposal_transition_is_legal, generic_approval_transition_is_legal,
        retry_transition_is_legal,
    },
};

#[derive(Debug, Serialize)]
struct ApprovalEvidence {
    id: i64,
    obligation_id: String,
    occurrence: u32,
    decision: ApprovalDecision,
    actor: String,
    note: Option<String>,
    decided_at: i64,
}

impl Store {
    pub fn operator_snapshot(&self, captured_at: i64) -> Result<OperatorSnapshot, StoreError> {
        let proposals = self
            .gardener_proposals()?
            .into_iter()
            .map(|proposal| (proposal.implementation_obligation_id.clone(), proposal))
            .collect::<BTreeMap<_, _>>();
        let mut obligations = self
            .list()?
            .into_iter()
            .map(|obligation| {
                self.project_operator_obligation(&obligation, proposals.get(&obligation.id))
            })
            .collect::<Result<Vec<_>, _>>()?;
        obligations.sort_by_key(operator_sort_key);
        Ok(OperatorSnapshot {
            captured_at,
            obligations,
        })
    }

    pub fn operator_topic(
        &self,
        obligation_id: &str,
        captured_at: i64,
    ) -> Result<ObligationTopic, StoreError> {
        let obligation = self
            .get(obligation_id)?
            .ok_or_else(|| StoreError::NotFound(obligation_id.to_owned()))?;
        let all_proposals = self.gardener_proposals()?;
        let mut all_observations = Vec::new();
        for proposal in &all_proposals {
            all_observations.extend(self.proposal_observations(&proposal.fingerprint)?);
        }
        let direct_fingerprints = all_proposals
            .iter()
            .filter(|item| item.implementation_obligation_id == obligation_id)
            .map(|item| item.fingerprint.clone())
            .collect::<BTreeSet<_>>();
        let mut inspection_ids = self
            .gardener_inspections()?
            .into_iter()
            .filter(|item| item.obligation_id == obligation_id)
            .map(|item| item.id)
            .collect::<BTreeSet<_>>();
        inspection_ids.extend(
            all_observations
                .iter()
                .filter(|item| direct_fingerprints.contains(&item.proposal_fingerprint))
                .map(|item| item.inspection_id.clone()),
        );
        let proposal_fingerprints = if direct_fingerprints.is_empty() {
            all_observations
                .iter()
                .filter(|item| inspection_ids.contains(&item.inspection_id))
                .map(|item| item.proposal_fingerprint.clone())
                .collect::<BTreeSet<_>>()
        } else {
            direct_fingerprints
        };
        let proposals = all_proposals
            .into_iter()
            .filter(|item| proposal_fingerprints.contains(&item.fingerprint))
            .collect::<Vec<_>>();
        let observations = all_observations
            .drain(..)
            .filter(|item| proposal_fingerprints.contains(&item.proposal_fingerprint))
            .collect::<Vec<_>>();
        let inspections = self
            .gardener_inspections()?
            .into_iter()
            .filter(|item| inspection_ids.contains(&item.id))
            .collect::<Vec<_>>();
        let runs = self.gardener_implementation_runs_for_obligation(obligation_id)?;
        let run_ids = runs
            .iter()
            .map(|item| item.id.clone())
            .collect::<BTreeSet<_>>();
        let mut items = Vec::new();

        for event in self.events(obligation_id)? {
            items.push(topic_item(
                event.occurred_at,
                TopicSource::AuditEvent,
                event.sequence.to_string(),
                format!("audit:{}", event.sequence),
                Some(event.occurrence),
                event.event_type.clone(),
                &event,
            )?);
        }
        for approval in self.approval_evidence(obligation_id)? {
            items.push(topic_item(
                approval.decided_at,
                TopicSource::ApprovalDecision,
                approval.id.to_string(),
                format!("approval:{}", approval.id),
                Some(approval.occurrence),
                approval.decision.to_string(),
                &approval,
            )?);
        }
        for attempt in self.attempts(obligation_id)? {
            items.push(topic_item(
                attempt.claimed_at,
                TopicSource::Attempt,
                attempt.id.to_string(),
                format!("attempt:{}", attempt.id),
                Some(attempt.occurrence),
                format!("attempt_{}", attempt.outcome),
                &attempt,
            )?);
        }
        for inspection in inspections {
            items.push(topic_item(
                inspection.started_at,
                TopicSource::GardenerInspection,
                inspection.id.clone(),
                format!("gardener-inspection:{}", inspection.id),
                Some(inspection.occurrence),
                "gardener_inspection".to_owned(),
                &inspection,
            )?);
        }
        for proposal in proposals {
            items.push(topic_item(
                proposal.created_at,
                TopicSource::GardenerProposal,
                proposal.fingerprint.clone(),
                format!("gardener-proposal:{}", proposal.fingerprint),
                self.get(&proposal.implementation_obligation_id)?
                    .map(|item| item.occurrence)
                    .or(Some(obligation.occurrence)),
                "gardener_proposal".to_owned(),
                &proposal,
            )?);
        }
        for observation in observations {
            items.push(topic_item(
                observation.observed_at,
                TopicSource::GardenerObservation,
                observation.id.to_string(),
                format!("gardener-observation:{}", observation.id),
                None,
                "gardener_proposal_observed".to_owned(),
                &observation,
            )?);
        }
        for event in self.gardener_events()? {
            if event
                .inspection_id
                .as_deref()
                .is_some_and(|id| inspection_ids.contains(id))
                || event
                    .proposal_fingerprint
                    .as_ref()
                    .is_some_and(|fingerprint| proposal_fingerprints.contains(fingerprint))
            {
                items.push(topic_item(
                    event.occurred_at,
                    TopicSource::GardenerEvent,
                    event.sequence.to_string(),
                    format!("gardener-event:{}", event.sequence),
                    None,
                    event.event_type.clone(),
                    &event,
                )?);
            }
        }
        for run in runs {
            items.push(topic_item(
                run.created_at,
                TopicSource::GardenerImplementationRun,
                run.id.clone(),
                format!("gardener-run:{}", run.id),
                Some(run.occurrence),
                format!("gardener_run_{}", run.phase),
                &run,
            )?);
        }
        for run_id in run_ids {
            for event in self.gardener_run_events(&run_id)? {
                items.push(topic_item(
                    event.occurred_at,
                    TopicSource::GardenerRunEvent,
                    event.sequence.to_string(),
                    format!("gardener-run-event:{}", event.sequence),
                    None,
                    event.event_type.clone(),
                    &event,
                )?);
            }
        }
        items.sort_by(|left, right| {
            (
                left.occurred_at,
                left.source,
                &left.source_sequence,
                &left.stable_id,
            )
                .cmp(&(
                    right.occurred_at,
                    right.source,
                    &right.source_sequence,
                    &right.stable_id,
                ))
        });
        Ok(ObligationTopic {
            captured_at,
            obligation_id: obligation_id.to_owned(),
            items,
        })
    }

    fn project_operator_obligation(
        &self,
        obligation: &Obligation,
        proposal: Option<&Proposal>,
    ) -> Result<OperatorObligation, StoreError> {
        let exception = self.exception_reason(obligation, proposal)?;
        let liveness = match obligation.state {
            state if state.is_terminal() => None,
            ObligationState::Pending | ObligationState::RetryScheduled => {
                Some(DurableLiveness::FutureWake {
                    wake_at: obligation.next_wake_at.ok_or_else(|| {
                        StoreError::Invalid(format!(
                            "non-terminal obligation {:?} lacks a durable wake",
                            obligation.id
                        ))
                    })?,
                })
            }
            ObligationState::Running => Some(DurableLiveness::ActiveLease {
                token: obligation.lease_token.clone().ok_or_else(|| {
                    StoreError::Invalid(format!(
                        "running obligation {:?} lacks a lease",
                        obligation.id
                    ))
                })?,
                generation: obligation.lease_generation,
                expires_at: obligation.lease_expires_at.ok_or_else(|| {
                    StoreError::Invalid(format!(
                        "running obligation {:?} lacks lease expiry",
                        obligation.id
                    ))
                })?,
            }),
            ObligationState::AwaitingApproval | ObligationState::Attention => {
                Some(DurableLiveness::HumanAttention {
                    reason: exception.clone().ok_or_else(|| {
                        StoreError::Invalid(format!(
                            "human-attention obligation {:?} lacks an exception reason",
                            obligation.id
                        ))
                    })?,
                })
            }
            ObligationState::Completed | ObligationState::Cancelled => unreachable!(),
        };
        Ok(OperatorObligation {
            id: obligation.id.clone(),
            description: obligation.description.clone(),
            state: obligation.state.into(),
            occurrence: obligation.occurrence,
            scheduled_at: obligation.scheduled_at,
            next_wake_at: obligation.next_wake_at,
            recurrence_cron: obligation.recurrence_cron.clone(),
            recurrence_timezone: obligation.recurrence_timezone.clone(),
            approval_required: obligation.approval_required,
            attempts_made: obligation.attempts_made,
            max_attempts: obligation.max_attempts,
            retry_base_seconds: obligation.retry_base_seconds,
            retry_max_seconds: obligation.retry_max_seconds,
            last_error: obligation.last_error.clone(),
            last_evidence: obligation.last_evidence.clone(),
            created_at: obligation.created_at,
            updated_at: obligation.updated_at,
            exception,
            liveness,
            capabilities: capabilities(obligation, proposal.is_some()),
        })
    }

    fn exception_reason(
        &self,
        obligation: &Obligation,
        proposal: Option<&Proposal>,
    ) -> Result<Option<ExceptionReason>, StoreError> {
        if obligation.state == ObligationState::AwaitingApproval {
            let subject = proposal.map_or(ApprovalSubject::Generic, |proposal| {
                ApprovalSubject::GardenerProposal {
                    repository: proposal.repository.clone(),
                    fingerprint: proposal.fingerprint.clone(),
                    prompt: proposal.prompt.clone(),
                    obligation_id: obligation.id.clone(),
                    occurrence: obligation.occurrence,
                }
            });
            return Ok(Some(ExceptionReason::AwaitingApproval { subject }));
        }
        if obligation.state != ObligationState::Attention {
            return Ok(None);
        }
        let latest_approval = self.approval_evidence(&obligation.id)?.pop();
        let cause = if let Some(approval) = latest_approval
            .filter(|item| item.occurrence == obligation.occurrence)
            .filter(|item| item.decision == ApprovalDecision::Rejected)
        {
            AttentionCause::Rejected {
                actor: approval.actor,
                note: approval.note,
            }
        } else if let Some(run) = self.latest_gardener_run(&obligation.id)? {
            match run.verification_verdict {
                Some(GardenerVerificationVerdict::Blocking) => {
                    AttentionCause::GardenerVerificationBlocking {
                        summary: run.verification_summary.unwrap_or_default(),
                    }
                }
                Some(GardenerVerificationVerdict::Inconclusive) => {
                    AttentionCause::GardenerVerificationInconclusive {
                        summary: run.verification_summary.unwrap_or_default(),
                    }
                }
                _ => failure_cause(
                    obligation,
                    &self.events(&obligation.id)?,
                    self.attempts(&obligation.id)?.last(),
                ),
            }
        } else {
            failure_cause(
                obligation,
                &self.events(&obligation.id)?,
                self.attempts(&obligation.id)?.last(),
            )
        };
        Ok(Some(ExceptionReason::Attention {
            cause,
            error: obligation.last_error.clone(),
            evidence: obligation.last_evidence.clone(),
        }))
    }

    fn latest_gardener_run(
        &self,
        obligation_id: &str,
    ) -> Result<Option<GardenerImplementationRun>, StoreError> {
        Ok(self
            .gardener_implementation_runs_for_obligation(obligation_id)?
            .into_iter()
            .max_by_key(|run| (run.updated_at, run.id.clone())))
    }

    fn approval_evidence(&self, obligation_id: &str) -> Result<Vec<ApprovalEvidence>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id, obligation_id, occurrence, decision, actor, note, decided_at
             FROM approvals WHERE obligation_id = ?1 ORDER BY id",
        )?;
        let rows = statement.query_map(params![obligation_id], |row| {
            let decision = row.get::<_, String>(3)?.parse().map_err(|error: String| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
                )
            })?;
            Ok(ApprovalEvidence {
                id: row.get(0)?,
                obligation_id: row.get(1)?,
                occurrence: row.get(2)?,
                decision,
                actor: row.get(4)?,
                note: row.get(5)?,
                decided_at: row.get(6)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }
}

fn failure_cause(
    obligation: &Obligation,
    events: &[crate::AuditEvent],
    latest_attempt: Option<&crate::Attempt>,
) -> AttentionCause {
    let event_type = events
        .last()
        .map(|event| event.event_type.as_str())
        .unwrap_or("");
    if event_type.starts_with("recurrence_") && event_type.ends_with("_attention") {
        AttentionCause::RecurrenceFailure
    } else {
        match latest_attempt.and_then(|attempt| attempt.retryable) {
            Some(false) => AttentionCause::NonRetryableFailure,
            Some(true) if obligation.attempts_made >= obligation.max_attempts => {
                AttentionCause::AttemptsExhausted
            }
            _ => AttentionCause::PersistedFailure,
        }
    }
}

fn capabilities(obligation: &Obligation, is_proposal: bool) -> OperatorCapabilities {
    let approval_legal = approval_transition_is_legal(obligation);
    let generic_approval_legal = generic_approval_transition_is_legal(obligation, is_proposal);
    let proposal_approval_legal = gardener_proposal_transition_is_legal(obligation, is_proposal);
    let approve_reason = if is_proposal && approval_legal {
        Some(DisabledReason::GardenerProposalRequiresExactDecision)
    } else if generic_approval_legal {
        None
    } else {
        Some(state_disabled_reason(obligation))
    };
    let proposal_reason = if !is_proposal {
        Some(DisabledReason::NotGardenerProposal)
    } else if proposal_approval_legal {
        None
    } else {
        Some(state_disabled_reason(obligation))
    };
    let retry_reason =
        (!retry_transition_is_legal(obligation)).then(|| state_disabled_reason(obligation));
    let cancel_reason =
        (!cancel_transition_is_legal(obligation)).then(|| state_disabled_reason(obligation));
    OperatorCapabilities {
        approve: capability(approve_reason, ActionConsequence::ScheduleCurrentOccurrence),
        reject: capability(approve_reason, ActionConsequence::MoveToAttention),
        retry: capability(retry_reason, ActionConsequence::ReopenForRetry),
        cancel: capability(cancel_reason, ActionConsequence::CancelObligation),
        approve_gardener_proposal: capability(
            proposal_reason,
            ActionConsequence::ScheduleExactGardenerProposal,
        ),
        reject_gardener_proposal: capability(
            proposal_reason,
            ActionConsequence::RejectExactGardenerProposal,
        ),
    }
}

fn capability(reason: Option<DisabledReason>, consequence: ActionConsequence) -> ActionCapability {
    ActionCapability {
        available: reason.is_none(),
        disabled_reason: reason,
        consequence,
    }
}

fn state_disabled_reason(obligation: &Obligation) -> DisabledReason {
    if obligation.state.is_terminal() {
        DisabledReason::TerminalObligation
    } else if obligation.state == ObligationState::Running {
        DisabledReason::RunningClaimOwnsObligation
    } else {
        DisabledReason::StateDoesNotPermit
    }
}

fn operator_sort_key(item: &OperatorObligation) -> (u8, u8, i64, i64, String) {
    let exception = u8::from(item.exception.is_none());
    let state = match item.state {
        OperatorObligationState::AwaitingApproval => 0,
        OperatorObligationState::Attention => 1,
        OperatorObligationState::Running => 2,
        OperatorObligationState::RetryScheduled => 3,
        OperatorObligationState::Pending => 4,
        OperatorObligationState::Completed => 5,
        OperatorObligationState::Cancelled => 6,
    };
    (
        exception,
        state,
        item.next_wake_at.unwrap_or(i64::MAX),
        item.updated_at,
        item.id.clone(),
    )
}

fn topic_item(
    occurred_at: i64,
    source: TopicSource,
    source_sequence: String,
    stable_id: String,
    occurrence: Option<u32>,
    event_type: String,
    evidence: &impl Serialize,
) -> Result<TopicItem, StoreError> {
    Ok(TopicItem {
        occurred_at,
        source,
        source_sequence,
        stable_id,
        occurrence,
        event_type,
        evidence: serde_json::to_value(evidence)
            .map_err(|error| StoreError::Invalid(format!("operator evidence failed: {error}")))?,
    })
}

impl From<ObligationState> for OperatorObligationState {
    fn from(value: ObligationState) -> Self {
        match value {
            ObligationState::Pending => Self::Pending,
            ObligationState::AwaitingApproval => Self::AwaitingApproval,
            ObligationState::Running => Self::Running,
            ObligationState::RetryScheduled => Self::RetryScheduled,
            ObligationState::Attention => Self::Attention,
            ObligationState::Completed => Self::Completed,
            ObligationState::Cancelled => Self::Cancelled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Completion, InspectionResult, NewGardenerImplementationRun, NewGardenerInspection,
        NewObligation, NewRepositoryRegistration, Recurrence, RetryPolicy,
    };

    fn new(id: &str, approval_required: bool) -> NewObligation {
        NewObligation {
            id: id.to_owned(),
            description: format!("Test {id}"),
            scheduled_at: 100,
            recurrence: None,
            approval_required,
            retry: RetryPolicy {
                max_attempts: 2,
                base_delay_seconds: 10,
                max_delay_seconds: 20,
            },
        }
    }

    fn finish_gardener_run(
        store: &mut Store,
        claim: &crate::Claim,
        run_id: &str,
        verdict: GardenerVerificationVerdict,
        start: i64,
    ) {
        let implementation_path = format!("/tmp/{run_id}-implementation");
        let verification_path = format!("/tmp/{run_id}-verification");
        let head = if run_id == "run-1" {
            "c".repeat(40)
        } else {
            "d".repeat(40)
        };
        store
            .create_gardener_implementation_run(
                claim,
                NewGardenerImplementationRun {
                    id: run_id.to_owned(),
                    implementation_worktree_path: implementation_path,
                    branch: format!("codex/gardener-{run_id}"),
                },
                start,
            )
            .unwrap();
        store
            .record_implementation_codex_thread(
                claim,
                run_id,
                &format!("{run_id}-implementation-thread"),
                start + 1,
            )
            .unwrap();
        store
            .record_implementation_codex_turn(
                claim,
                run_id,
                &format!("{run_id}-implementation-turn"),
                start + 2,
            )
            .unwrap();
        store
            .finish_gardener_implementation(
                claim,
                run_id,
                r#"{"summary":"implemented"}"#,
                start + 3,
            )
            .unwrap();
        store
            .record_gardener_git_commit(claim, run_id, &head, start + 4)
            .unwrap();
        store
            .record_gardener_push_observation(claim, run_id, &head, start + 5)
            .unwrap();
        store
            .record_gardener_ready_pull_request(
                claim,
                run_id,
                43,
                "https://github.com/robchristie/bokkie/pull/43",
                &head,
                start + 6,
            )
            .unwrap();
        store
            .start_gardener_verification(claim, run_id, &verification_path, &head, start + 7)
            .unwrap();
        store
            .record_verification_codex_thread(
                claim,
                run_id,
                &format!("{run_id}-verification-thread"),
                start + 8,
            )
            .unwrap();
        store
            .record_verification_codex_turn(
                claim,
                run_id,
                &format!("{run_id}-verification-turn"),
                start + 9,
            )
            .unwrap();
        store
            .finish_gardener_verification(
                claim,
                run_id,
                verdict,
                &head,
                match verdict {
                    GardenerVerificationVerdict::Blocking => "A blocking issue remains",
                    GardenerVerificationVerdict::Inconclusive => "Evidence is inconclusive",
                    GardenerVerificationVerdict::Pass => "Exact head passed",
                },
                start + 10,
            )
            .unwrap();
    }

    #[test]
    fn snapshot_owns_exception_liveness_order_and_exact_transition_capabilities() {
        let mut store = Store::open_in_memory().unwrap();
        store.create(new("approval", true), 90).unwrap();
        let mut future = new("future", false);
        future.scheduled_at = 200;
        store.create(future, 90).unwrap();
        store.create(new("pending", false), 90).unwrap();
        store.create(new("running", false), 90).unwrap();
        let running = store.claim_due(100, 60, 1).unwrap();
        assert_eq!(running[0].obligation_id, "pending");
        // The remaining due ordinary obligation becomes the running fixture.
        let running = store.claim_due(100, 60, 1).unwrap().pop().unwrap();
        assert_eq!(running.obligation_id, "running");

        store.create(new("retry", false), 90).unwrap();
        let retry = store.claim_due(100, 60, 1).unwrap().pop().unwrap();
        store
            .complete(
                &retry,
                Completion::Failed {
                    retryable: true,
                    error: "temporary".to_owned(),
                    evidence: Some("retry evidence".to_owned()),
                },
                101,
            )
            .unwrap();
        store.create(new("complete", false), 90).unwrap();
        let complete = store.claim_due(100, 60, 1).unwrap().pop().unwrap();
        store
            .complete(
                &complete,
                Completion::Succeeded {
                    evidence: Some("done".to_owned()),
                },
                102,
            )
            .unwrap();
        store.create(new("cancelled", false), 90).unwrap();
        store.cancel("cancelled", 103).unwrap();
        store.create(new("rejected", true), 90).unwrap();
        store
            .decide_approval(
                "rejected",
                ApprovalDecision::Rejected,
                "operator",
                Some("unsafe"),
                104,
            )
            .unwrap();

        let snapshot = store.operator_snapshot(105).unwrap();
        assert_eq!(snapshot.captured_at, 105);
        assert_eq!(snapshot.obligations[0].id, "approval");
        assert_eq!(snapshot.obligations[1].id, "rejected");
        let by_id = snapshot
            .obligations
            .iter()
            .map(|item| (item.id.as_str(), item))
            .collect::<BTreeMap<_, _>>();
        assert!(matches!(
            by_id["approval"].exception,
            Some(ExceptionReason::AwaitingApproval {
                subject: ApprovalSubject::Generic
            })
        ));
        assert!(matches!(
            by_id["approval"].liveness,
            Some(DurableLiveness::HumanAttention { .. })
        ));
        assert!(by_id["approval"].capabilities.approve.available);
        assert!(
            !by_id["approval"]
                .capabilities
                .approve_gardener_proposal
                .available
        );
        assert_eq!(
            by_id["approval"]
                .capabilities
                .approve_gardener_proposal
                .disabled_reason,
            Some(DisabledReason::NotGardenerProposal)
        );
        assert!(matches!(
            by_id["pending"].liveness,
            Some(DurableLiveness::ActiveLease { .. })
        ));
        assert!(matches!(
            by_id["future"].liveness,
            Some(DurableLiveness::FutureWake { wake_at: 200 })
        ));
        assert!(matches!(
            by_id["running"].liveness,
            Some(DurableLiveness::ActiveLease { .. })
        ));
        assert_eq!(
            by_id["running"].capabilities.cancel.disabled_reason,
            Some(DisabledReason::RunningClaimOwnsObligation)
        );
        assert!(matches!(
            by_id["retry"].liveness,
            Some(DurableLiveness::FutureWake { wake_at: 111 })
        ));
        assert!(matches!(
            by_id["rejected"].exception,
            Some(ExceptionReason::Attention {
                cause: AttentionCause::Rejected { .. },
                ..
            })
        ));
        assert!(by_id["rejected"].capabilities.retry.available);
        assert_eq!(
            by_id["complete"].capabilities.cancel.disabled_reason,
            Some(DisabledReason::TerminalObligation)
        );
        assert!(by_id["complete"].liveness.is_none());
        assert!(by_id["cancelled"].liveness.is_none());

        store.retry_attention("rejected", 106).unwrap();
        let retried = store
            .operator_snapshot(106)
            .unwrap()
            .obligations
            .into_iter()
            .find(|item| item.id == "rejected")
            .unwrap();
        assert_eq!(retried.state, OperatorObligationState::AwaitingApproval);
        assert!(retried.capabilities.approve.available);
    }

    #[test]
    fn gardener_proposal_requires_exact_action_and_topic_is_deterministic() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .register_gardener_repository(
                NewRepositoryRegistration {
                    repository: crate::CANONICAL_REPOSITORY.to_owned(),
                    default_branch: crate::CANONICAL_DEFAULT_BRANCH.to_owned(),
                    checkout_path: "/tmp/bokkie-operator-test".to_owned(),
                    inspection_recurrence: Recurrence::new("0 0 * * *", "UTC").unwrap(),
                    first_inspection_at: 100,
                },
                100,
            )
            .unwrap();
        let claim = store.claim_due_gardener(100, 60, 1).unwrap().pop().unwrap();
        store
            .start_gardener_inspection(
                &claim,
                NewGardenerInspection {
                    id: "inspection-1".to_owned(),
                    source_commit: "a".repeat(40),
                    worktree_path: "/tmp/bokkie-operator-test/inspection".to_owned(),
                    prompt_digest: "b".repeat(64),
                },
                100,
            )
            .unwrap();
        store
            .record_inspection_codex_thread(&claim, "inspection-1", "thread-1", 100)
            .unwrap();
        store
            .record_inspection_codex_turn(&claim, "inspection-1", "turn-1", 100)
            .unwrap();
        let proposal = store
            .finish_gardener_inspection(
                &claim,
                "inspection-1",
                &InspectionResult {
                    summary: "One useful change".to_owned(),
                    proposed_goal_prompts: vec!["Implement the exact safe change".to_owned()],
                },
                100,
            )
            .unwrap()
            .pop()
            .unwrap();

        let projected = store
            .operator_snapshot(100)
            .unwrap()
            .obligations
            .into_iter()
            .find(|item| item.id == proposal.implementation_obligation_id)
            .unwrap();
        assert!(!projected.capabilities.approve.available);
        assert_eq!(
            projected.capabilities.approve.disabled_reason,
            Some(DisabledReason::GardenerProposalRequiresExactDecision)
        );
        assert!(projected.capabilities.approve_gardener_proposal.available);
        assert!(matches!(
            projected.exception,
            Some(ExceptionReason::AwaitingApproval {
                subject: ApprovalSubject::GardenerProposal {
                    ref fingerprint,
                    ref prompt,
                    occurrence: 1,
                    ..
                }
            }) if fingerprint == &proposal.fingerprint && prompt == "Implement the exact safe change"
        ));

        let topic = store
            .operator_topic(&proposal.implementation_obligation_id, 101)
            .unwrap();
        assert_eq!(topic.captured_at, 101);
        assert!(topic.items.windows(2).all(|items| {
            (
                items[0].occurred_at,
                items[0].source,
                &items[0].source_sequence,
                &items[0].stable_id,
            ) <= (
                items[1].occurred_at,
                items[1].source,
                &items[1].source_sequence,
                &items[1].stable_id,
            )
        }));
        assert!(topic.items.iter().any(|item| {
            item.source == TopicSource::GardenerProposal
                && item.evidence["fingerprint"] == proposal.fingerprint
                && item.evidence["prompt"] == "Implement the exact safe change"
        }));
        assert!(topic.items.iter().any(|item| {
            item.source == TopicSource::GardenerInspection
                && item.evidence["codex_thread_id"] == "thread-1"
                && item.evidence["codex_turn_id"] == "turn-1"
                && item.evidence["source_commit"] == "a".repeat(40)
        }));
        assert!(topic.items.iter().any(|item| {
            item.source == TopicSource::GardenerEvent && item.event_type == "proposal_created"
        }));
        let inspection_topic = store
            .operator_topic("gardener:inspect:robchristie/bokkie", 101)
            .unwrap();
        assert!(inspection_topic.items.iter().any(|item| {
            item.source == TopicSource::GardenerProposal
                && item.evidence["fingerprint"] == proposal.fingerprint
                && item.evidence["prompt"] == "Implement the exact safe change"
        }));
        assert!(matches!(
            store.decide_approval(
                &proposal.implementation_obligation_id,
                ApprovalDecision::Approved,
                "operator",
                None,
                101,
            ),
            Err(StoreError::Conflict(_))
        ));
        store
            .decide_gardener_proposal(
                &proposal.fingerprint,
                ApprovalDecision::Approved,
                "operator",
                Some("exact prompt accepted"),
                101,
            )
            .unwrap();
        let claim = store
            .claim_due_gardener(101, 100, 10)
            .unwrap()
            .into_iter()
            .find(|claim| claim.obligation_id == proposal.implementation_obligation_id)
            .unwrap();
        store
            .create_gardener_implementation_run(
                &claim,
                NewGardenerImplementationRun {
                    id: "run-1".to_owned(),
                    implementation_worktree_path: "/tmp/run-1-implementation".to_owned(),
                    branch: "codex/gardener-run-1".to_owned(),
                },
                102,
            )
            .unwrap();
        store
            .record_implementation_codex_thread(&claim, "run-1", "implementation-thread", 103)
            .unwrap();
        store
            .record_implementation_codex_turn(&claim, "run-1", "implementation-turn", 104)
            .unwrap();
        store
            .finish_gardener_implementation(&claim, "run-1", r#"{"summary":"implemented"}"#, 105)
            .unwrap();
        let head = "c".repeat(40);
        store
            .record_gardener_git_commit(&claim, "run-1", &head, 106)
            .unwrap();
        store
            .record_gardener_push_observation(&claim, "run-1", &head, 107)
            .unwrap();
        store
            .record_gardener_ready_pull_request(
                &claim,
                "run-1",
                42,
                "https://github.com/robchristie/bokkie/pull/42",
                &head,
                108,
            )
            .unwrap();
        store
            .start_gardener_verification(&claim, "run-1", "/tmp/run-1-verification", &head, 109)
            .unwrap();
        store
            .record_verification_codex_thread(&claim, "run-1", "verification-thread", 110)
            .unwrap();
        store
            .record_verification_codex_turn(&claim, "run-1", "verification-turn", 111)
            .unwrap();
        store
            .finish_gardener_verification(
                &claim,
                "run-1",
                GardenerVerificationVerdict::Blocking,
                &head,
                "A blocking issue remains",
                112,
            )
            .unwrap();
        store
            .complete(
                &claim,
                Completion::Failed {
                    retryable: false,
                    error: "independent verification returned blocking".to_owned(),
                    evidence: Some("ready PR retained".to_owned()),
                },
                113,
            )
            .unwrap();
        let attention = store
            .operator_snapshot(114)
            .unwrap()
            .obligations
            .into_iter()
            .find(|item| item.id == proposal.implementation_obligation_id)
            .unwrap();
        assert!(matches!(
            attention.exception,
            Some(ExceptionReason::Attention {
                cause: AttentionCause::GardenerVerificationBlocking { ref summary },
                ..
            }) if summary == "A blocking issue remains"
        ));
        let completed_topic = store
            .operator_topic(&proposal.implementation_obligation_id, 114)
            .unwrap();
        let run = completed_topic
            .items
            .iter()
            .find(|item| item.source == TopicSource::GardenerImplementationRun)
            .unwrap();
        assert_eq!(run.evidence["source_commit"], "a".repeat(40));
        assert_eq!(run.evidence["git_commit"], head);
        assert_eq!(
            run.evidence["pull_request_url"],
            "https://github.com/robchristie/bokkie/pull/42"
        );
        assert_eq!(run.evidence["verification_verdict"], "blocking");
        assert!(
            completed_topic
                .items
                .iter()
                .any(|item| item.source == TopicSource::GardenerRunEvent)
        );
        store
            .retry_attention(&proposal.implementation_obligation_id, 115)
            .unwrap();
        store
            .decide_gardener_proposal(
                &proposal.fingerprint,
                ApprovalDecision::Approved,
                "operator",
                Some("retry exact prompt"),
                116,
            )
            .unwrap();
        let second_claim = store
            .claim_due_gardener(116, 100, 10)
            .unwrap()
            .into_iter()
            .find(|claim| claim.obligation_id == proposal.implementation_obligation_id)
            .unwrap();
        finish_gardener_run(
            &mut store,
            &second_claim,
            "run-2",
            GardenerVerificationVerdict::Inconclusive,
            117,
        );
        store
            .complete(
                &second_claim,
                Completion::Failed {
                    retryable: false,
                    error: "independent verification returned inconclusive".to_owned(),
                    evidence: Some("second ready PR retained".to_owned()),
                },
                128,
            )
            .unwrap();
        let inconclusive = store
            .operator_snapshot(129)
            .unwrap()
            .obligations
            .into_iter()
            .find(|item| item.id == proposal.implementation_obligation_id)
            .unwrap();
        assert!(matches!(
            inconclusive.exception,
            Some(ExceptionReason::Attention {
                cause: AttentionCause::GardenerVerificationInconclusive { ref summary },
                ..
            }) if summary == "Evidence is inconclusive"
        ));
        assert!(matches!(
            store.operator_topic("missing", 100),
            Err(StoreError::NotFound(_))
        ));
    }
}
