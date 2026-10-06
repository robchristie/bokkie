//! Notification responsibility is independent of local result completion.
use super::*;
use crate::notifications::{NotificationIntent, NotificationOutcome, NotificationTransport};
use bokkie_operator_api::{
    ManagedDelivery, ManagedDeliveryAttempt, ManagedTaskDefinition, NotificationRecovery,
    NotificationRecoveryRequest,
};

pub(super) fn create_intent(
    tx: &Transaction<'_>,
    task: &str,
    source: &Claim,
    definition: &ManagedTaskDefinition,
    result: &str,
    now: i64,
) -> Result<(), StoreError> {
    let is_push =
        crate::notifications::push::push_device_id(&definition.profile_revision).is_some();
    if !is_push {
        crate::notifications::validate_address(&definition.destination)
            .map_err(StoreError::Invalid)?;
    }
    if (!is_push && definition.profile_revision != "reminder-v1")
        || definition.effects != ["store_local_result", "send_notification"]
    {
        return Err(StoreError::Conflict(
            "reminder requires its exact installed adapter".into(),
        ));
    }
    let id = format!("delivery-{}", Uuid::new_v4());
    let new = NewObligation {
        id: id.clone(),
        description: format!("Notification: {}", definition.name),
        scheduled_at: now,
        recurrence: None,
        approval_required: false,
        retry: crate::RetryPolicy {
            max_attempts: definition.max_attempts,
            ..Default::default()
        },
    };
    validate_new(&new)?;
    apply_transition(tx, Transition::Create { new, now })?;
    tx.execute("INSERT INTO notification_deliveries(id,task_id,source_obligation_id,destination,subject,body,message_id,created_at)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8)", params![id,task,source.obligation_id,definition.destination,
        definition.name,result,format!("<{id}@bokkie.local>"),now])?;
    if is_push {
        super::push::save_intent(tx, &id, definition, now)?;
    }
    append_event(
        tx,
        &id,
        1,
        "notification_intent_saved",
        now,
        Some(ObligationState::Pending),
        ObligationState::Pending,
        json!({"task_id":task,"source_obligation_id":source.obligation_id,"destination":definition.destination}),
    )?;
    Ok(())
}

fn intent(conn: &Connection, id: &str) -> Result<Option<NotificationIntent>, StoreError> {
    let mut intent = conn.query_row("SELECT id,task_id,source_obligation_id,destination,subject,body,message_id,created_at FROM notification_deliveries WHERE id=?1",
        [id], |r| Ok(NotificationIntent { id:r.get(0)?,task_id:r.get(1)?,source_obligation_id:r.get(2)?,destination:r.get(3)?,
            subject:r.get(4)?,body:r.get(5)?,message_id:r.get(6)?,created_at:r.get(7)?,transport:None,push:None })).optional()?;
    if let Some(intent) = &mut intent {
        intent.push = super::push::intent(conn, id)?;
        let raw:Option<String> = conn.query_row("SELECT details_json FROM audit_events WHERE obligation_id=?1 AND event_type='notification_transport_bound' ORDER BY sequence LIMIT 1",
            [id],|r|r.get(0)).optional()?;
        intent.transport = raw
            .map(|raw| serde_json::from_str(&raw).map_err(|e| StoreError::Invalid(e.to_string())))
            .transpose()?;
    }
    Ok(intent)
}
pub(super) fn delivery_for_source(
    conn: &Connection,
    source: &str,
) -> Result<Option<ManagedDelivery>, StoreError> {
    let id: Option<String> = conn
        .query_row(
            "SELECT id FROM notification_deliveries WHERE source_obligation_id=?1",
            [source],
            |r| r.get(0),
        )
        .optional()?;
    id.map(|id| delivery(conn, &id)).transpose()
}
pub(crate) fn delivery(conn: &Connection, id: &str) -> Result<ManagedDelivery, StoreError> {
    let data = intent(conn, id)?.ok_or_else(|| StoreError::NotFound(id.into()))?;
    let obligation = require_obligation(conn, id)?;
    let (started, reconciled): (Option<i64>, Option<i64>) = conn.query_row(
        "SELECT dispatch_started_at,reconciled_at FROM notification_deliveries WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let uncertain = obligation.state == ObligationState::Attention && started.is_some();
    let push = super::push::projection(conn, id)?;
    let status = match obligation.state {
        _ if reconciled.is_some() => "reconciled",
        ObligationState::Completed if push.is_some() => "accepted_by_push_service",
        ObligationState::Completed => "accepted_by_relay",
        ObligationState::Running => "sending",
        ObligationState::RetryScheduled => "retry_scheduled",
        ObligationState::Attention if uncertain => "uncertain",
        ObligationState::Attention => "needs_attention",
        _ => "pending",
    }
    .to_string();
    let detail = if reconciled.is_some() {
        "Operator acknowledged this delivery as reconciled; acceptance is not proved by that decision".into()
    } else {
        obligation
            .last_error
            .clone()
            .or(obligation.last_evidence.clone())
            .unwrap_or_else(|| match status.as_str() {
                "sending" => "Delivery attempt in progress".into(),
                _ => "Notification intent saved; waiting for the delivery worker".into(),
            })
    };
    let attempts = conn.prepare("SELECT attempt_number,claimed_at,completed_at,outcome,error,evidence FROM attempts WHERE obligation_id=?1 ORDER BY id DESC LIMIT 20")?
        .query_map([id],|r|Ok(ManagedDeliveryAttempt { attempt_number:r.get(0)?,started_at:r.get(1)?,completed_at:r.get(2)?,outcome:r.get(3)?,
            detail:r.get::<_,Option<String>>(4)?.or(r.get(5)?) }))?.collect::<Result<Vec<_>,_>>()?;
    let recovery =
        if uncertain || (push.is_some() && obligation.state == ObligationState::Attention) {
            Some(ActionPrecondition {
                obligation_id: id.into(),
                occurrence: obligation.occurrence,
                state_revision: conn.query_row(
                    "SELECT max(sequence) FROM audit_events WHERE obligation_id=?1",
                    [id],
                    |r| r.get(0),
                )?,
                gardener_fingerprint: None,
                gardener_proposal_instance_id: None,
                gardener_source_commit: None,
                gardener_source_observation_id: None,
                gardener_source_inspection_id: None,
                gardener_generation: None,
            })
        } else {
            None
        };
    Ok(ManagedDelivery {
        id: id.into(),
        status,
        detail,
        destination: data.destination,
        subject: data.subject,
        body: data.body,
        next_retry_at: obligation.next_wake_at,
        attempts,
        recovery,
        push,
    })
}

pub(super) fn reject_generic(tx: &Transaction<'_>, id: &str) -> Result<(), StoreError> {
    if intent(tx, id)?.is_some() {
        return Err(StoreError::Conflict(
            "notification delivery requires its designated fenced recovery action".into(),
        ));
    }
    Ok(())
}
pub(super) fn validate_fenced_retry(tx: &Transaction<'_>, id: &str) -> Result<(), StoreError> {
    if let Some(intent) = intent(tx, id)? {
        if intent.push.is_some() {
            return Err(StoreError::Conflict("Use the saved push delivery's explicit recovery review; its destination and expiry cannot be changed by retry".into()));
        }
        let started: Option<i64> = tx.query_row(
            "SELECT dispatch_started_at FROM notification_deliveries WHERE id=?1",
            [id],
            |r| r.get(0),
        )?;
        if started.is_some() {
            return Err(StoreError::Conflict("uncertain notification delivery requires explicit duplicate-risk acknowledgement or reconciliation".into()));
        }
    }
    Ok(())
}

pub(super) fn recover_expired(
    tx: &Transaction<'_>,
    id: &str,
    now: i64,
) -> Result<bool, StoreError> {
    if intent(tx, id)?.is_none() {
        return Ok(false);
    }
    let started: Option<i64> = tx.query_row(
        "SELECT dispatch_started_at FROM notification_deliveries WHERE id=?1",
        [id],
        |r| r.get(0),
    )?;
    if started.is_none() {
        return Ok(false);
    }
    let obligation = require_obligation(tx, id)?;
    let error = "Delivery worker lease expired after possible dispatch; check delivery evidence before retrying";
    let changed = tx.execute("UPDATE attempts SET completed_at=?3,outcome='lease_expired',retryable=0,failure_disposition='needs_reconciliation',error=?4
        WHERE obligation_id=?1 AND lease_generation=?2 AND completed_at IS NULL",params![id,obligation.lease_generation,now,error])?;
    if changed != 1 {
        return Err(StoreError::Fenced);
    }
    schedule_failure(
        tx,
        &obligation,
        now,
        FailureDisposition::NeedsReconciliation,
        error,
        None,
        "notification_lease_expired",
    )?;
    Ok(true)
}

impl Store {
    pub fn notification_intent(&self, id: &str) -> Result<NotificationIntent, StoreError> {
        intent(&self.connection, id)?.ok_or_else(|| StoreError::NotFound(id.into()))
    }
    pub fn notification_delivery(&self, id: &str) -> Result<ManagedDelivery, StoreError> {
        delivery(&self.connection, id)
    }
    pub fn notification_task(&self, id: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .connection
            .query_row(
                "SELECT task_id FROM notification_deliveries WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub fn claim_due_notifications(
        &mut self,
        now: i64,
        lease_seconds: i64,
        limit: usize,
    ) -> Result<Vec<Claim>, StoreError> {
        if lease_seconds <= 0 || limit > 100 {
            return Err(StoreError::Invalid(
                "delivery claim lease must be positive and batch at most 100".into(),
            ));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        recover_expired_in_transaction(&tx, now)?;
        let ids = tx.prepare("SELECT o.id FROM obligations o JOIN notification_deliveries d ON d.id=o.id
            WHERE o.state IN ('pending','retry_scheduled') AND o.next_wake_at<=?1 AND d.dispatch_started_at IS NULL
            ORDER BY o.next_wake_at,o.id LIMIT ?2")?.query_map(params![now,limit as i64],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        let mut claims = Vec::new();
        for id in ids {
            claims.push(
                apply_transition(
                    &tx,
                    Transition::Claim {
                        id: &id,
                        now,
                        lease_seconds,
                    },
                )?
                .claim
                .expect("claim transition"),
            );
        }
        tx.commit()?;
        Ok(claims)
    }
    /// Commit the conservative possible-dispatch marker before any network use.
    /// One claim can dispatch once; replay never invokes the sender again.
    pub fn begin_notification_send(
        &mut self,
        claim: &Claim,
        now: i64,
    ) -> Result<NotificationIntent, StoreError> {
        self.begin_notification_send_with_transport(claim, None, now)
    }
    pub fn begin_notification_send_with_transport(
        &mut self,
        claim: &Claim,
        transport: Option<&NotificationTransport>,
        now: i64,
    ) -> Result<NotificationIntent, StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        verify_claim(&require_obligation(&tx, &claim.obligation_id)?, claim, now)?;
        let mut data = intent(&tx, &claim.obligation_id)?
            .ok_or_else(|| StoreError::NotFound(claim.obligation_id.clone()))?;
        if data.transport.is_none() && data.push.is_none() {
            if let Some(transport) = transport.filter(|t| t.destination == data.destination) {
                append_event(
                    &tx,
                    &claim.obligation_id,
                    claim.occurrence,
                    "notification_transport_bound",
                    now,
                    Some(ObligationState::Running),
                    ObligationState::Running,
                    serde_json::to_value(transport)
                        .map_err(|e| StoreError::Invalid(e.to_string()))?,
                )?;
                data.transport = Some(transport.clone());
            }
        }
        let changed = tx.execute("UPDATE notification_deliveries SET dispatch_started_at=?2 WHERE id=?1 AND dispatch_started_at IS NULL",
            params![claim.obligation_id,now])?;
        if changed != 1 {
            return Err(StoreError::Conflict(
                "this delivery may already have been dispatched".into(),
            ));
        }
        append_event(
            &tx,
            &claim.obligation_id,
            claim.occurrence,
            "notification_dispatch_started",
            now,
            Some(ObligationState::Running),
            ObligationState::Running,
            json!({"message_id":data.message_id}),
        )?;
        tx.commit()?;
        Ok(data)
    }
    pub fn complete_notification_send(
        &mut self,
        claim: &Claim,
        outcome: NotificationOutcome,
        now: i64,
    ) -> Result<(), StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        verify_claim(&require_obligation(&tx, &claim.obligation_id)?, claim, now)?;
        let started: Option<i64> = tx.query_row(
            "SELECT dispatch_started_at FROM notification_deliveries WHERE id=?1",
            [&claim.obligation_id],
            |r| r.get(0),
        )?;
        if started.is_none() {
            return Err(StoreError::Conflict(
                "delivery has no committed dispatch marker".into(),
            ));
        }
        let bounded = |s: String| s.chars().take(4096).collect::<String>();
        let completion = match outcome {
            NotificationOutcome::Accepted { detail } => Completion::Succeeded {
                evidence: Some(bounded(detail)),
            },
            NotificationOutcome::PushAccepted {
                detail,
                ttl_seconds,
            } => {
                let changed = tx.execute(
                    "UPDATE push_deliveries SET accepted_ttl_seconds=?2 WHERE id=?1",
                    params![claim.obligation_id, ttl_seconds],
                )?;
                if changed != 1 {
                    return Err(StoreError::Conflict(
                        "Push acceptance requires a saved push intent".into(),
                    ));
                }
                Completion::Succeeded {
                    evidence: Some(bounded(detail)),
                }
            }
            NotificationOutcome::SubscriptionExpired { detail } => {
                tx.execute(
                    "UPDATE notification_deliveries SET dispatch_started_at=NULL WHERE id=?1",
                    [&claim.obligation_id],
                )?;
                tx.execute("UPDATE push_configuration SET active=0,revision=revision+1 WHERE active=1 AND device_id=(SELECT device_id FROM push_deliveries WHERE id=?1)",[&claim.obligation_id])?;
                Completion::Failed {disposition:FailureDisposition::HumanDecision,error:bounded(detail),evidence:Some("The push service rejected the saved subscription; explicitly enrol a device and review future destinations".into())}
            }
            NotificationOutcome::Rejected { retryable, detail } => {
                tx.execute(
                    "UPDATE notification_deliveries SET dispatch_started_at=NULL WHERE id=?1",
                    [&claim.obligation_id],
                )?;
                Completion::Failed { disposition:if retryable { FailureDisposition::RetrySafe } else { FailureDisposition::HumanDecision },
                    error:bounded(detail),evidence:Some("The delivery service did not accept this message; retry cannot duplicate an accepted send".into()) }
            }
            NotificationOutcome::Uncertain { detail } => Completion::Failed {
                disposition: FailureDisposition::NeedsReconciliation,
                error: bounded(detail),
                evidence: Some("Acceptance is uncertain; automatic retry is blocked".into()),
            },
        };
        validate_completion(&completion)?;
        apply_transition(
            &tx,
            Transition::Complete {
                claim,
                completion,
                now,
            },
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn recover_notification_if_current(
        &mut self,
        id: &str,
        request: &NotificationRecoveryRequest,
        now: i64,
    ) -> Result<ManagedDelivery, StoreError> {
        if request
            .note
            .as_ref()
            .is_some_and(|n| n.contains('\0') || n.chars().count() > 4096)
        {
            return Err(StoreError::Invalid(
                "reconciliation note exceeds bounds".into(),
            ));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_action_precondition(&tx, id, &request.precondition, None, None)?;
        let before = delivery(&tx, id)?;
        if before.recovery.is_none() {
            return Err(StoreError::Conflict(
                "notification is not awaiting delivery reconciliation".into(),
            ));
        }
        match request.action {
            NotificationRecovery::MarkReconciled => {
                apply_transition(&tx, Transition::Cancel { id, now })?;
                tx.execute(
                    "UPDATE notification_deliveries SET reconciled_at=?2 WHERE id=?1",
                    params![id, now],
                )?;
            }
            NotificationRecovery::RetryAcknowledgingDuplicateRisk => {
                if before.push.as_ref().is_some_and(|p| now >= p.expires_at) {
                    return Err(StoreError::Conflict("The saved notification has expired. Resolve it without resending and explicitly review a new reminder if one is still needed".into()));
                }
                tx.execute(
                    "UPDATE notification_deliveries SET dispatch_started_at=NULL WHERE id=?1",
                    [id],
                )?;
                apply_transition(&tx, Transition::RetryAttention { id, now })?;
            }
        }
        let obligation = require_obligation(&tx, id)?;
        append_event(
            &tx,
            id,
            obligation.occurrence,
            "notification_operator_recovery",
            now,
            Some(ObligationState::Attention),
            obligation.state,
            json!({"action":request.action,"note":request.note,"relay_acceptance_proved":false}),
        )?;
        let result = delivery(&tx, id)?;
        tx.commit()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bokkie_operator_api::{ManagedCapabilityProfile, ManagedTrigger, OperatorTaskKind};
    use tempfile::TempDir;

    fn activate(store: &mut Store, recurring: bool) -> String {
        let mut definition =
            ManagedTaskDefinition::reminder("Review queue", "Read the queue", "reader@example.org");
        if recurring {
            definition.trigger = ManagedTrigger::Recurring {
                cron: "* * * * *".into(),
                timezone: "UTC".into(),
            };
        }
        let id = store
            .managed_create(&Uuid::new_v4().to_string(), &definition, 0)
            .unwrap()
            .task_id;
        let profiles = [ManagedCapabilityProfile::reminder("reader@example.org")];
        let preview = store.managed_preview(&id, "session", &profiles, 0).unwrap();
        assert!(preview.blockers.is_empty(), "{:?}", preview.blockers);
        store
            .managed_activate(
                &Uuid::new_v4().to_string(),
                &preview,
                "session",
                &profiles,
                0,
            )
            .unwrap();
        id
    }
    fn complete_reminder(store: &mut Store, now: i64) -> (String, String) {
        let claim = store
            .claim_due_reminders(now, 30, 1)
            .unwrap()
            .pop()
            .unwrap();
        store
            .complete_managed_note(&claim, "Read the queue", now)
            .unwrap();
        // Lost local completion response cannot insert another delivery.
        store
            .complete_managed_note(&claim, "Read the queue", now + 1)
            .unwrap();
        let delivery = delivery_for_source(&store.connection, &claim.obligation_id)
            .unwrap()
            .unwrap();
        (claim.obligation_id, delivery.id)
    }
    #[test]
    fn intent_and_result_commit_atomically_and_incompatible_workers_cannot_claim() {
        let mut store = Store::open_in_memory().unwrap();
        let task = activate(&mut store, false);
        assert!(store.claim_due(0, 30, 10).unwrap().is_empty());
        assert!(store.claim_due_gardener(0, 30, 10).unwrap().is_empty());
        assert!(store.claim_due_notes(0, 30, 10).unwrap().is_empty());
        let (source, id) = complete_reminder(&mut store, 0);
        assert_eq!(
            store.managed_detail(&task).unwrap().runs[0]
                .delivery
                .as_ref()
                .unwrap()
                .id,
            id
        );
        let payload = store.notification_intent(&id).unwrap();
        assert_eq!(payload.source_obligation_id, source);
        assert_eq!(payload.destination, "reader@example.org");
        assert_eq!(payload.body, "Read the queue");
        assert!(store.claim_due(1, 30, 10).unwrap().is_empty());
        assert!(store.claim_due_gardener(1, 30, 10).unwrap().is_empty());
        assert!(store.claim_due_notes(1, 30, 10).unwrap().is_empty());
        let count: i64 = store
            .connection
            .query_row("SELECT count(*) FROM notification_deliveries", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
        assert!(
            store
                .connection
                .execute(
                    "UPDATE notification_deliveries SET body='changed' WHERE id=?1",
                    [&id]
                )
                .is_err()
        );
    }
    #[test]
    fn payload_creation_failure_rolls_back_local_result_and_kernel_completion() {
        let mut store = Store::open_in_memory().unwrap();
        activate(&mut store, false);
        let claim = store.claim_due_reminders(0, 30, 1).unwrap().pop().unwrap();
        store.connection.execute_batch("CREATE TRIGGER reject_intent BEFORE INSERT ON notification_deliveries BEGIN SELECT RAISE(ABORT,'synthetic storage failure'); END;").unwrap();
        assert!(
            store
                .complete_managed_note(&claim, "Read the queue", 0)
                .is_err()
        );
        assert_eq!(
            store.get(&claim.obligation_id).unwrap().unwrap().state,
            ObligationState::Running
        );
        let result: bool = store
            .connection
            .query_row("SELECT EXISTS(SELECT 1 FROM managed_results)", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(!result);
        let count: i64 = store
            .connection
            .query_row("SELECT count(*) FROM notification_deliveries", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }
    #[test]
    fn restart_before_dispatch_safely_retries_but_possible_dispatch_requires_attention() {
        let temp = TempDir::new().unwrap();
        let db = temp.path().join("reminders.sqlite");
        let mut store = Store::open(&db).unwrap();
        activate(&mut store, false);
        let (_, id) = complete_reminder(&mut store, 0);
        let first = store
            .claim_due_notifications(0, 5, 1)
            .unwrap()
            .pop()
            .unwrap();
        drop(store);
        let mut store = Store::open(&db).unwrap();
        store.recover_expired_leases(5).unwrap();
        let wake = store.get(&id).unwrap().unwrap().next_wake_at.unwrap();
        let next = store
            .claim_due_notifications(wake, 5, 1)
            .unwrap()
            .pop()
            .unwrap();
        assert_ne!(first.lease_token, next.lease_token);
        let intent = store.begin_notification_send(&next, wake).unwrap();
        assert_eq!(intent.id, id);
        assert!(store.begin_notification_send(&next, wake).is_err());
        drop(store);
        let mut store = Store::open(&db).unwrap();
        store.recover_expired_leases(wake + 5).unwrap();
        let projected = store.notification_delivery(&id).unwrap();
        assert_eq!(projected.status, "uncertain");
        assert!(projected.next_retry_at.is_none());
        assert!(projected.recovery.is_some());
        assert!(
            store
                .claim_due_notifications(wake + 1000, 30, 1)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .complete_notification_send(
                    &next,
                    NotificationOutcome::Accepted {
                        detail: "late acceptance".into()
                    },
                    wake + 5
                )
                .is_err()
        );
        assert!(store.retry_attention(&id, wake + 6).is_err());
        assert!(
            store
                .retry_attention_if_current(&id, projected.recovery.as_ref().unwrap(), wake + 6)
                .is_err()
        );
        assert!(
            store
                .cancel_if_current(&id, projected.recovery.as_ref().unwrap(), wake + 6)
                .is_err()
        );
        assert_eq!(
            store
                .attempts(&id)
                .unwrap()
                .last()
                .unwrap()
                .failure_disposition,
            Some(FailureDisposition::NeedsReconciliation)
        );
    }
    #[test]
    fn proved_rejection_retries_same_payload_and_acceptance_never_replays() {
        let mut store = Store::open_in_memory().unwrap();
        activate(&mut store, false);
        let (_, id) = complete_reminder(&mut store, 0);
        let claim = store
            .claim_due_notifications(0, 30, 1)
            .unwrap()
            .pop()
            .unwrap();
        let first = store.begin_notification_send(&claim, 0).unwrap();
        store
            .complete_notification_send(
                &claim,
                NotificationOutcome::Rejected {
                    retryable: true,
                    detail: "relay unavailable before DATA".into(),
                },
                0,
            )
            .unwrap();
        assert_eq!(
            store.notification_delivery(&id).unwrap().status,
            "retry_scheduled"
        );
        let wake = store.get(&id).unwrap().unwrap().next_wake_at.unwrap();
        let retry = store
            .claim_due_notifications(wake, 30, 1)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(store.begin_notification_send(&retry, wake).unwrap(), first);
        store
            .complete_notification_send(
                &retry,
                NotificationOutcome::Accepted {
                    detail: "Relay accepted message; downstream delivery is not proved".into(),
                },
                wake,
            )
            .unwrap();
        assert_eq!(
            store.notification_delivery(&id).unwrap().status,
            "accepted_by_relay"
        );
        assert!(
            store
                .claim_due_notifications(wake + 1000, 30, 1)
                .unwrap()
                .is_empty()
        );
        assert!(store.begin_notification_send(&retry, wake).is_err());
    }
    #[test]
    fn explicit_recovery_is_fenced_and_acknowledgement_does_not_claim_acceptance() {
        let mut store = Store::open_in_memory().unwrap();
        activate(&mut store, false);
        let (_, id) = complete_reminder(&mut store, 0);
        let claim = store
            .claim_due_notifications(0, 30, 1)
            .unwrap()
            .pop()
            .unwrap();
        store.begin_notification_send(&claim, 0).unwrap();
        store
            .complete_notification_send(
                &claim,
                NotificationOutcome::Uncertain {
                    detail: "lost reply after DATA".into(),
                },
                0,
            )
            .unwrap();
        let precondition = store.notification_delivery(&id).unwrap().recovery.unwrap();
        let retry = NotificationRecoveryRequest {
            precondition: precondition.clone(),
            action: NotificationRecovery::RetryAcknowledgingDuplicateRisk,
            note: Some("Operator accepts duplicate risk".into()),
        };
        assert_eq!(
            store
                .recover_notification_if_current(&id, &retry, 1)
                .unwrap()
                .status,
            "pending"
        );
        assert!(
            store
                .recover_notification_if_current(&id, &retry, 1)
                .is_err()
        );
        let claim = store
            .claim_due_notifications(1, 30, 1)
            .unwrap()
            .pop()
            .unwrap();
        store.begin_notification_send(&claim, 1).unwrap();
        store
            .complete_notification_send(
                &claim,
                NotificationOutcome::Uncertain {
                    detail: "lost second reply".into(),
                },
                1,
            )
            .unwrap();
        let mut reconcile = retry;
        reconcile.action = NotificationRecovery::MarkReconciled;
        assert!(
            store
                .recover_notification_if_current(&id, &reconcile, 2)
                .is_err()
        );
        reconcile.precondition = store.notification_delivery(&id).unwrap().recovery.unwrap();
        let result = store
            .recover_notification_if_current(&id, &reconcile, 2)
            .unwrap();
        assert_eq!(result.status, "reconciled");
        assert!(result.detail.contains("acceptance is not proved"));
        assert!(
            store
                .claim_due_notifications(1000, 30, 1)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn proved_permanent_rejection_can_use_normal_fenced_retry_of_the_same_intent() {
        let mut store = Store::open_in_memory().unwrap();
        activate(&mut store, false);
        let (_, id) = complete_reminder(&mut store, 0);
        let claim = store
            .claim_due_notifications(0, 30, 1)
            .unwrap()
            .pop()
            .unwrap();
        let original = store.begin_notification_send(&claim, 0).unwrap();
        store
            .complete_notification_send(
                &claim,
                NotificationOutcome::Rejected {
                    retryable: false,
                    detail: "550 recipient temporarily misconfigured; operator correction required"
                        .into(),
                },
                0,
            )
            .unwrap();
        let projection = store.operator_obligation(&id, 1).unwrap();
        assert!(projection.capabilities.retry.available);
        assert!(
            projection
                .task
                .as_ref()
                .unwrap()
                .notification
                .as_ref()
                .unwrap()
                .recovery
                .is_none()
        );
        let precondition = projection.capabilities.retry.precondition.unwrap();
        assert!(store.retry_attention(&id, 1).is_err());
        store
            .retry_attention_if_current(&id, &precondition, 1)
            .unwrap();
        assert!(
            store
                .retry_attention_if_current(&id, &precondition, 1)
                .is_err()
        );
        let claim = store
            .claim_due_notifications(1, 30, 1)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(store.begin_notification_send(&claim, 1).unwrap(), original);
    }

    #[test]
    fn temporary_rejection_exhaustion_retains_visible_attention_and_one_retryable_intent() {
        let mut store = Store::open_in_memory().unwrap();
        let task = activate(&mut store, false);
        let (_, id) = complete_reminder(&mut store, 0);
        let original = store.notification_intent(&id).unwrap();
        let maximum = store.get(&id).unwrap().unwrap().max_attempts;
        assert_eq!(maximum, 3);
        let mut now = 0;
        for attempt in 1..=maximum {
            let claim = store
                .claim_due_notifications(now, 30, 1)
                .unwrap()
                .pop()
                .unwrap();
            assert_eq!(claim.attempt_number, attempt);
            assert_eq!(
                store.begin_notification_send(&claim, now).unwrap(),
                original
            );
            store
                .complete_notification_send(
                    &claim,
                    NotificationOutcome::Rejected {
                        retryable: true,
                        detail:
                            "Synthetic 451 response proves the relay did not accept this message"
                                .into(),
                    },
                    now,
                )
                .unwrap();
            let obligation = store.get(&id).unwrap().unwrap();
            if attempt < maximum {
                assert_eq!(obligation.state, ObligationState::RetryScheduled);
                now = obligation.next_wake_at.unwrap();
            } else {
                assert_eq!(obligation.state, ObligationState::Attention);
                assert!(obligation.next_wake_at.is_none());
            }
        }
        now += 1000;
        assert!(
            store
                .claim_due_notifications(now, 30, 1)
                .unwrap()
                .is_empty()
        );
        let delivery = store.notification_delivery(&id).unwrap();
        assert_eq!(delivery.status, "needs_attention");
        assert_eq!(delivery.attempts.len(), maximum as usize);
        assert!(delivery.recovery.is_none());
        assert_eq!(
            store.managed_detail(&task).unwrap().status,
            bokkie_operator_api::ManagedTaskStatus::Completed
        );
        let catalogue = store
            .managed_catalogue_view("", None, 20, "input", now, "Australia/Adelaide")
            .unwrap();
        let parent = catalogue.items.iter().find(|item| item.id == task).unwrap();
        assert_eq!(parent.status, "completed");
        assert!(parent.summary.as_ref().unwrap().needs_input);
        let snapshot = store.operator_snapshot(now).unwrap();
        let attention = snapshot
            .obligations
            .iter()
            .find(|obligation| obligation.id == id)
            .unwrap();
        assert_eq!(
            attention.task.as_ref().unwrap().kind,
            OperatorTaskKind::NotificationDelivery
        );
        assert_eq!(
            attention.task.as_ref().unwrap().parent_task_id.as_deref(),
            Some(task.as_str())
        );
        assert!(attention.capabilities.retry.available);
        let precondition = attention.capabilities.retry.precondition.as_ref().unwrap();
        assert_eq!(precondition.obligation_id, id);
        assert_eq!(store.notification_intent(&id).unwrap(), original);
        store
            .retry_attention_if_current(&id, precondition, now)
            .unwrap();
        let retry = store
            .claim_due_notifications(now, 30, 1)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(retry.obligation_id, id);
        assert_eq!(
            store.begin_notification_send(&retry, now).unwrap(),
            original
        );
        let (results, intents): (i64, i64) = store.connection.query_row(
            "SELECT (SELECT count(*) FROM managed_results), (SELECT count(*) FROM notification_deliveries)",
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!((results, intents), (1, 1));
    }
    #[test]
    fn retry_and_restart_keep_the_first_sender_and_relay_binding() {
        use crate::notifications::{NotificationConfig, NotificationSender};
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("pinned-transport.sqlite");
        let mut store = Store::open(&database).unwrap();
        activate(&mut store, false);
        let (_, id) = complete_reminder(&mut store, 0);
        let first = NotificationTransport {
            relay_host: "smtp-relay".into(),
            relay_port: 25,
            from_address: "bokkie@example.org".into(),
            destination: "reader@example.org".into(),
        };
        let claim = store
            .claim_due_notifications(0, 30, 1)
            .unwrap()
            .pop()
            .unwrap();
        let intent = store
            .begin_notification_send_with_transport(&claim, Some(&first), 0)
            .unwrap();
        assert_eq!(intent.transport, Some(first.clone()));
        store
            .complete_notification_send(
                &claim,
                NotificationOutcome::Rejected {
                    retryable: true,
                    detail: "synthetic pre-DATA failure".into(),
                },
                0,
            )
            .unwrap();
        drop(store);
        let mut store = Store::open(&database).unwrap();
        let wake = store.get(&id).unwrap().unwrap().next_wake_at.unwrap();
        let claim = store
            .claim_due_notifications(wake, 30, 1)
            .unwrap()
            .pop()
            .unwrap();
        let changed = NotificationConfig {
            relay_host: "127.0.0.1".into(),
            relay_port: 1,
            from_address: "other@example.org".into(),
            destination: "reader@example.org".into(),
            timeout_ms: 100,
        };
        let restored = store
            .begin_notification_send_with_transport(&claim, changed.transport().as_ref(), wake)
            .unwrap();
        assert_eq!(restored, intent);
        assert!(matches!(
            changed.send(&restored),
            NotificationOutcome::Rejected {
                retryable: false,
                ..
            }
        ));
        let count:i64=store.connection.query_row("SELECT count(*) FROM audit_events WHERE obligation_id=?1 AND event_type='notification_transport_bound'",[&id],|r|r.get(0)).unwrap();
        assert_eq!(count, 1);
    }
    #[test]
    fn legacy_local_note_still_stores_only_a_result() {
        let mut store = Store::open_in_memory().unwrap();
        let definition = ManagedTaskDefinition::local_note("Original note", "Local text");
        let task = store
            .managed_create("old-note", &definition, 0)
            .unwrap()
            .task_id;
        let profiles = [ManagedCapabilityProfile::local_note()];
        let preview = store
            .managed_preview(&task, "session", &profiles, 0)
            .unwrap();
        store
            .managed_activate("activate-old-note", &preview, "session", &profiles, 0)
            .unwrap();
        assert!(store.claim_due_reminders(0, 30, 1).unwrap().is_empty());
        assert!(crate::managed::run_one_note(&mut store, 0).unwrap());
        let detail = store.managed_detail(&task).unwrap();
        assert_eq!(detail.runs[0].result.as_deref(), Some("Local text"));
        assert!(detail.runs[0].delivery.is_none());
        assert!(store.claim_due_notifications(0, 30, 1).unwrap().is_empty());
    }

    #[test]
    fn dated_once_emits_one_result_and_intent_across_ticks_and_cannot_resume() {
        let mut store = Store::open_in_memory().unwrap();
        let profiles = [ManagedCapabilityProfile::reminder("reader@example.org")];
        let mut definition =
            ManagedTaskDefinition::reminder("Dated reminder", "One result", "reader@example.org");
        definition.trigger = ManagedTrigger::Once {
            local_datetime: "1970-01-01T00:01:00".into(),
            timezone: "UTC".into(),
        };
        let task = store
            .managed_create("create-once", &definition, 0)
            .unwrap()
            .task_id;
        let review = store
            .managed_preview(&task, "session", &profiles, 0)
            .unwrap();
        store
            .managed_activate("activate-once", &review, "session", &profiles, 0)
            .unwrap();
        assert!(!crate::managed::run_one_reminder(&mut store, 59).unwrap());
        assert!(crate::managed::run_one_reminder(&mut store, 60).unwrap());
        let claim = store
            .claim_due_notifications(60, 30, 1)
            .unwrap()
            .pop()
            .unwrap();
        let intent = store.begin_notification_send(&claim, 60).unwrap();
        store
            .complete_notification_send(
                &claim,
                NotificationOutcome::Accepted {
                    detail: "Synthetic relay acceptance".into(),
                },
                60,
            )
            .unwrap();
        for tick in [60, 61, 120, 3600] {
            assert!(!crate::managed::run_one_reminder(&mut store, tick).unwrap());
            assert!(
                store
                    .claim_due_notifications(tick, 30, 1)
                    .unwrap()
                    .is_empty()
            );
        }
        let state = store.managed_detail(&task).unwrap();
        assert_eq!(
            state.status,
            bokkie_operator_api::ManagedTaskStatus::Completed
        );
        assert_eq!(state.runs.len(), 1);
        assert_eq!(state.runs[0].result.as_deref(), Some("One result"));
        assert_eq!(state.runs[0].delivery.as_ref().unwrap().id, intent.id);
        assert!(state.next_wake_at.is_none());
        assert!(
            store
                .managed_resume(
                    "resume-once",
                    &task,
                    state.configuration_revision,
                    &profiles,
                    3600
                )
                .is_err()
        );
        let count: i64 = store
            .connection
            .query_row("SELECT count(*) FROM notification_deliveries", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn reminder_recurrence_emits_distinct_intents_across_adelaide_dst() {
        let mut store = Store::open_in_memory().unwrap();
        let profiles = [ManagedCapabilityProfile::reminder("reader@example.org")];
        let mut definition =
            ManagedTaskDefinition::reminder("Daily reminder", "DST text", "reader@example.org");
        definition.trigger = ManagedTrigger::Recurring {
            cron: "30 9 * * *".into(),
            timezone: "Australia/Adelaide".into(),
        };
        let before = chrono::DateTime::parse_from_rfc3339("2026-10-02T22:30:00Z")
            .unwrap()
            .timestamp();
        let first = chrono::DateTime::parse_from_rfc3339("2026-10-03T00:00:00Z")
            .unwrap()
            .timestamp();
        let second = chrono::DateTime::parse_from_rfc3339("2026-10-03T23:00:00Z")
            .unwrap()
            .timestamp();
        let task = store
            .managed_create("create-dst", &definition, before)
            .unwrap()
            .task_id;
        let review = store
            .managed_preview(&task, "session", &profiles, before)
            .unwrap();
        assert_eq!(&review.occurrences[..2], &[first, second]);
        assert_eq!(second - first, 23 * 3600);
        store
            .managed_activate("activate-dst", &review, "session", &profiles, before)
            .unwrap();
        assert!(crate::managed::run_one_reminder(&mut store, first).unwrap());
        let initial = store.managed_detail(&task).unwrap();
        assert_eq!(initial.next_wake_at, Some(second));
        let first_id = initial
            .runs
            .iter()
            .find(|r| r.result.is_some())
            .unwrap()
            .delivery
            .as_ref()
            .unwrap()
            .id
            .clone();
        assert!(!crate::managed::run_one_reminder(&mut store, second - 1).unwrap());
        assert!(crate::managed::run_one_reminder(&mut store, second).unwrap());
        let final_state = store.managed_detail(&task).unwrap();
        let completed: Vec<_> = final_state
            .runs
            .iter()
            .filter(|r| r.result.is_some())
            .collect();
        assert_eq!(completed.len(), 2);
        assert!(
            completed
                .iter()
                .all(|r| r.timezone == "Australia/Adelaide"
                    && r.result.as_deref() == Some("DST text"))
        );
        assert!(
            completed
                .iter()
                .any(|r| r.delivery.as_ref().unwrap().id != first_id)
        );
        let count: i64 = store
            .connection
            .query_row("SELECT count(*) FROM notification_deliveries", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn edit_and_pause_preserve_an_admitted_delivery_and_its_original_payload() {
        let mut store = Store::open_in_memory().unwrap();
        let task = activate(&mut store, true);
        let (_, id) = complete_reminder(&mut store, 60);
        let claim = store
            .claim_due_notifications(60, 30, 1)
            .unwrap()
            .pop()
            .unwrap();
        let original = store.begin_notification_send(&claim, 60).unwrap();
        let state = store.managed_detail(&task).unwrap();
        let mut revised = state.active.unwrap().definition;
        revised.instructions = "Revised future text".into();
        revised.destination = "new-reader@example.org".into();
        store
            .managed_revise(
                "revise-future",
                &task,
                state.configuration_revision,
                &revised,
                61,
            )
            .unwrap();
        let profiles = [ManagedCapabilityProfile::reminder("new-reader@example.org")];
        let review = store
            .managed_preview(&task, "session", &profiles, 61)
            .unwrap();
        store
            .managed_activate("activate-future", &review, "session", &profiles, 61)
            .unwrap();
        let active = store.managed_detail(&task).unwrap();
        store
            .managed_pause("pause-future", &task, active.configuration_revision, 62)
            .unwrap();
        assert_eq!(store.notification_intent(&id).unwrap(), original);
        assert_eq!(
            store.get(&id).unwrap().unwrap().lease_token.as_deref(),
            Some(claim.lease_token.as_str())
        );
        store
            .complete_notification_send(
                &claim,
                NotificationOutcome::Accepted {
                    detail: "Synthetic relay acceptance".into(),
                },
                63,
            )
            .unwrap();
        let paused = store.managed_detail(&task).unwrap();
        assert!(paused.next_wake_at.is_none());
        assert_eq!(store.notification_intent(&id).unwrap(), original);
        assert_eq!(original.destination, "reader@example.org");
        assert_eq!(original.body, "Read the queue");
        store
            .managed_resume(
                "resume-future",
                &task,
                paused.configuration_revision,
                &profiles,
                600,
            )
            .unwrap();
        assert_eq!(store.managed_detail(&task).unwrap().next_wake_at, Some(660));
        assert!(crate::managed::run_one_reminder(&mut store, 660).unwrap());
        let future = store
            .managed_detail(&task)
            .unwrap()
            .runs
            .into_iter()
            .find(|r| r.result.as_deref() == Some("Revised future text"))
            .unwrap()
            .delivery
            .unwrap();
        assert_eq!(future.destination, "new-reader@example.org");
        assert_ne!(future.id, id);
    }
    #[test]
    fn historical_run_keeps_its_definition_zone_after_schedule_revision() {
        let mut store = Store::open_in_memory().unwrap();
        let task = activate(&mut store, true);
        let claim = store.claim_due_reminders(60, 30, 1).unwrap().pop().unwrap();
        let state = store.managed_detail(&task).unwrap();
        let mut revised = state.active.unwrap().definition;
        revised.trigger = ManagedTrigger::Recurring {
            cron: "* * * * *".into(),
            timezone: "Australia/Adelaide".into(),
        };
        store
            .managed_revise(
                "new-zone",
                &task,
                state.configuration_revision,
                &revised,
                60,
            )
            .unwrap();
        let profiles = [ManagedCapabilityProfile::reminder("reader@example.org")];
        let preview = store
            .managed_preview(&task, "session", &profiles, 60)
            .unwrap();
        store
            .managed_activate("activate-zone", &preview, "session", &profiles, 60)
            .unwrap();
        store
            .complete_managed_note(&claim, "Read the queue", 61)
            .unwrap();
        let detail = store.managed_detail(&task).unwrap();
        assert_eq!(
            detail
                .runs
                .iter()
                .find(|r| r.obligation_id == claim.obligation_id)
                .unwrap()
                .timezone,
            "UTC"
        );
        assert_eq!(
            detail
                .runs
                .iter()
                .find(|r| r.admitted_at.is_none())
                .unwrap()
                .timezone,
            "Australia/Adelaide"
        );
    }
    #[test]
    fn old_uncertain_delivery_remains_attention_after_twenty_new_recurring_results() {
        let mut store = Store::open_in_memory().unwrap();
        let task = activate(&mut store, true);
        let (source, id) = complete_reminder(&mut store, 60);
        let claim = store
            .claim_due_notifications(60, 30, 1)
            .unwrap()
            .pop()
            .unwrap();
        store.begin_notification_send(&claim, 60).unwrap();
        store
            .complete_notification_send(
                &claim,
                NotificationOutcome::Uncertain {
                    detail: "reply lost".into(),
                },
                60,
            )
            .unwrap();
        for n in 2..=23 {
            complete_reminder(&mut store, n * 60);
        }
        let detail = store.managed_detail(&task).unwrap();
        assert_eq!(detail.runs.len(), 20);
        assert!(detail.runs.iter().all(|r| r.obligation_id != source));
        assert_eq!(detail.next_wake_at, Some(24 * 60));
        let snapshot = store.operator_snapshot(23 * 60).unwrap();
        let row = snapshot.obligations.iter().find(|o| o.id == id).unwrap();
        let projected = row.task.as_ref().unwrap();
        assert_eq!(projected.kind, OperatorTaskKind::NotificationDelivery);
        assert_eq!(projected.parent_task_id.as_deref(), Some(task.as_str()));
        assert_eq!(projected.notification.as_ref().unwrap().status, "uncertain");
        assert!(!row.capabilities.retry.available);
        assert!(!row.capabilities.cancel.available);
    }
}
