//! Managed definitions own admission policy; executions use kernel transitions.
use super::*;
use bokkie_operator_api::{
    ManagedCapabilityProfile, ManagedCatalogueEntry, ManagedCataloguePage,
    ManagedDefinitionRevision, ManagedRun, ManagedTaskDefinition, ManagedTaskDetail,
    ManagedTaskPreview, ManagedTaskReceipt, ManagedTaskStatus, ManagedTrigger,
};
use chrono::{NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use serde::{Serialize, de::DeserializeOwned};

fn encode<T: Serialize>(value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|e| StoreError::Invalid(e.to_string()))
}
fn decode<T: DeserializeOwned>(value: &str) -> Result<T, StoreError> {
    serde_json::from_str(value).map_err(|e| StoreError::Invalid(e.to_string()))
}
fn invalid(message: &str) -> StoreError {
    StoreError::Invalid(message.into())
}
fn conflict(message: &str) -> StoreError {
    StoreError::Conflict(message.into())
}
fn bounded(value: &str, max: usize) -> bool {
    !value.contains('\0') && value.chars().count() <= max
}
fn validate(def: &ManagedTaskDefinition) -> Result<(), StoreError> {
    if def.name.trim().is_empty()
        || !bounded(&def.name, 200)
        || def.purpose.trim().is_empty()
        || !bounded(&def.purpose, 4096)
        || !bounded(&def.instructions, 16384)
        || !bounded(&def.capability, 100)
        || !bounded(&def.profile_revision, 200)
        || !bounded(&def.destination, 200)
        || def.context_refs.len() > 20
        || def.context_refs.iter().any(|v| !bounded(v, 2048))
        || def.effects.len() > 20
        || def.effects.iter().any(|v| !bounded(v, 200))
        || !(1..=5).contains(&def.max_attempts)
        || !(1..=16384).contains(&def.max_output_chars)
    {
        return Err(invalid(
            "task definition exceeds finite field or execution bounds",
        ));
    }
    match &def.trigger {
        ManagedTrigger::Immediate => {}
        ManagedTrigger::Once {
            local_datetime,
            timezone,
        } => {
            once_timestamp(local_datetime, timezone)?;
        }
        ManagedTrigger::Recurring { cron, timezone } => {
            Recurrence::new(cron, timezone)?;
        }
    }
    Ok(())
}
fn once_timestamp(local: &str, timezone: &str) -> Result<i64, StoreError> {
    if !bounded(local, 32) || !bounded(timezone, 100) {
        return Err(invalid("invalid local time or timezone"));
    }
    let zone: Tz = timezone
        .parse()
        .map_err(|_| invalid("unknown IANA timezone"))?;
    let date = NaiveDateTime::parse_from_str(local, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(local, "%Y-%m-%dT%H:%M"))
        .map_err(|_| invalid("local datetime must be YYYY-MM-DDTHH:MM[:SS]"))?;
    zone.from_local_datetime(&date)
        .single()
        .map(|v| v.timestamp())
        .ok_or_else(|| invalid("local datetime is ambiguous or does not exist in its timezone"))
}
fn next_time(def: &ManagedTaskDefinition, now: i64) -> Result<i64, StoreError> {
    match &def.trigger {
        ManagedTrigger::Immediate => Ok(now),
        ManagedTrigger::Once {
            local_datetime,
            timezone,
        } => {
            let at = once_timestamp(local_datetime, timezone)?;
            if at < now {
                Err(invalid("one-off time is in the past"))
            } else {
                Ok(at)
            }
        }
        ManagedTrigger::Recurring { cron, timezone } => {
            Ok(Recurrence::new(cron, timezone)?.next_after(now)?)
        }
    }
}
fn profile_for<'a>(
    def: &ManagedTaskDefinition,
    profiles: &'a [ManagedCapabilityProfile],
) -> Option<&'a ManagedCapabilityProfile> {
    profiles
        .iter()
        .find(|p| p.capability == def.capability && p.revision == def.profile_revision)
}
fn capability_blockers(
    def: &ManagedTaskDefinition,
    profiles: &[ManagedCapabilityProfile],
) -> Vec<String> {
    let mut reasons = Vec::new();
    if def.capability == "local_note" && def.instructions.trim().is_empty() {
        reasons.push("Local note instructions are required".into());
    }
    // The available runner set is deliberately closed, even if a caller advertises another profile.
    if def.capability != "local_note" || def.profile_revision != "local-note-v1" {
        reasons.push(format!(
            "Capability {} has no installed execution adapter",
            def.capability
        ));
    }
    match profile_for(def, profiles) {
        None => reasons.push("The exact capability profile is unavailable".into()),
        Some(p) => {
            if !p.available {
                reasons.push("The capability is disabled by the operator".into());
            }
            if def.effects != p.effects
                || def.destination != p.destination
                || def.max_attempts > p.max_attempts
                || def.max_output_chars > p.max_output_chars
            {
                reasons.push(
                    "Requested effects, destination or bounds exceed the capability profile".into(),
                );
            }
        }
    }
    if def.capability == "local_note"
        && (def.effects != ["store_local_result"] || def.destination != "task_results")
    {
        reasons.push("Local notes can only store a result in this task".into());
    }
    reasons
}
fn blockers(
    def: &ManagedTaskDefinition,
    profiles: &[ManagedCapabilityProfile],
    now: i64,
) -> Vec<String> {
    let mut reasons = capability_blockers(def, profiles);
    if let Err(error) = next_time(def, now) {
        reasons.push(error.to_string());
    }
    reasons
}
fn retained_admission(conn: &Connection, id: &str) -> Result<Option<(String, i64)>, StoreError> {
    Ok(conn.query_row("SELECT b.obligation_id,o.scheduled_at FROM managed_bindings b JOIN obligations o ON o.id=b.obligation_id
        WHERE b.task_id=?1 AND b.admitted_at IS NOT NULL AND o.state NOT IN ('completed','cancelled') LIMIT 1",
        [id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?)
}
fn resume_blockers(
    def: &ManagedTaskDefinition,
    profiles: &[ManagedCapabilityProfile],
    now: i64,
    retained: bool,
) -> Vec<String> {
    let mut reasons = capability_blockers(def, profiles);
    if !retained {
        match next_time(def,now) {
            Err(_) if matches!(def.trigger,ManagedTrigger::Once {..}) => reasons.push(
                "The paused one-off time has passed without admission; revise its date and review activation before resuming".into()),
            Err(error) => reasons.push(error.to_string()),
            Ok(_) => {},
        }
    }
    reasons
}
fn preview_occurrences(def: &ManagedTaskDefinition, now: i64) -> Result<Vec<i64>, StoreError> {
    let mut occurrences = Vec::new();
    if let Ok(at) = next_time(def, now) {
        occurrences.push(at);
        if let ManagedTrigger::Recurring { cron, timezone } = &def.trigger {
            let recurrence = Recurrence::new(cron, timezone)?;
            for _ in 0..4 {
                match recurrence.next_after(*occurrences.last().unwrap()) {
                    Ok(at) => occurrences.push(at),
                    Err(_) => break,
                }
            }
        }
    }
    Ok(occurrences)
}

fn revision(
    conn: &Connection,
    id: &str,
    rev: Option<i64>,
) -> Result<Option<ManagedDefinitionRevision>, StoreError> {
    rev.map(|revision| {
        let (raw, created_at): (String, i64) = conn.query_row(
            "SELECT definition_json, created_at FROM managed_definitions WHERE task_id=?1 AND revision=?2",
            params![id, revision], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(ManagedDefinitionRevision { revision, definition: decode(&raw)?, created_at })
    }).transpose()
}
fn detail(conn: &Connection, id: &str) -> Result<ManagedTaskDetail, StoreError> {
    let (configuration_revision, status, active, candidate): (i64, String, Option<i64>, Option<i64>) = conn.query_row(
        "SELECT configuration_revision,status,active_revision,candidate_revision FROM managed_tasks WHERE id=?1",
        [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?.ok_or_else(|| StoreError::NotFound(id.into()))?;
    let status = match status.as_str() {
        "draft" => ManagedTaskStatus::Draft,
        "active" => ManagedTaskStatus::Active,
        "paused" => ManagedTaskStatus::Paused,
        "completed" => ManagedTaskStatus::Completed,
        _ => return Err(invalid("unknown managed task status")),
    };
    let runs = conn.prepare("SELECT b.obligation_id,b.definition_revision,b.profile_revision,o.scheduled_at,b.admitted_at,o.state,r.result
        FROM managed_bindings b JOIN obligations o ON o.id=b.obligation_id LEFT JOIN managed_results r ON r.obligation_id=o.id
        WHERE b.task_id=?1 ORDER BY o.created_at DESC,o.rowid DESC LIMIT 20")?
        .query_map([id], |r| Ok(ManagedRun { obligation_id:r.get(0)?, definition_revision:r.get(1)?,profile_revision:r.get(2)?,
            scheduled_at:r.get(3)?,admitted_at:r.get(4)?,state:r.get(5)?,result:r.get(6)? }))?.collect::<Result<Vec<_>,_>>()?;
    let next_wake_at = conn.query_row("SELECT min(o.next_wake_at) FROM managed_bindings b JOIN obligations o ON o.id=b.obligation_id
        WHERE b.task_id=?1 AND o.state NOT IN ('completed','cancelled')", [id], |r| r.get(0))?;
    Ok(ManagedTaskDetail {
        id: id.into(),
        configuration_revision,
        status,
        active: revision(conn, id, active)?,
        candidate: revision(conn, id, candidate)?,
        next_wake_at,
        runs,
    })
}
fn ensure_revision(state: &ManagedTaskDetail, expected: i64) -> Result<(), StoreError> {
    if state.configuration_revision != expected {
        Err(conflict("task configuration changed since review"))
    } else {
        Ok(())
    }
}
fn replay(
    tx: &Transaction<'_>,
    command: &str,
    request: &str,
) -> Result<Option<ManagedTaskReceipt>, StoreError> {
    if command.trim().is_empty() || !bounded(command, 200) {
        return Err(invalid("invalid command identity"));
    }
    let saved: Option<(String, String)> = tx
        .query_row(
            "SELECT request_json,receipt_json FROM managed_receipts WHERE command_id=?1",
            [command],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match saved {
        Some((old, raw)) if old == request => Ok(Some(decode(&raw)?)),
        Some(_) => Err(conflict("command identity was reused with changed content")),
        None => Ok(None),
    }
}
fn receipt(
    tx: &Transaction<'_>,
    command: &str,
    request: &str,
    id: &str,
    now: i64,
    event: &str,
) -> Result<ManagedTaskReceipt, StoreError> {
    let result = ManagedTaskReceipt {
        command_id: command.into(),
        task_id: id.into(),
        configuration_revision: tx.query_row(
            "SELECT configuration_revision FROM managed_tasks WHERE id=?1",
            [id],
            |r| r.get(0),
        )?,
    };
    tx.execute(
        "INSERT INTO managed_receipts(command_id,request_json,receipt_json) VALUES (?1,?2,?3)",
        params![command, request, encode(&result)?],
    )?;
    domain_event(
        tx,
        id,
        event,
        now,
        &json!({"command_id":command,"configuration_revision":result.configuration_revision}),
    )?;
    Ok(result)
}
fn domain_event(
    tx: &Transaction<'_>,
    id: &str,
    event: &str,
    now: i64,
    details: &serde_json::Value,
) -> Result<(), StoreError> {
    tx.execute("INSERT INTO domain_events(entity_kind,entity_id,event_type,occurred_at,details_json) VALUES ('managed_task',?1,?2,?3,?4)",
        params![id,event,now,encode(details)?])?;
    Ok(())
}
fn cancel_unadmitted(tx: &Transaction<'_>, id: &str, now: i64) -> Result<(), StoreError> {
    let ids = tx.prepare("SELECT b.obligation_id FROM managed_bindings b JOIN obligations o ON o.id=b.obligation_id
        WHERE b.task_id=?1 AND b.admitted_at IS NULL AND o.state NOT IN ('completed','cancelled')")?
        .query_map([id],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
    for id in ids {
        apply_transition(tx, Transition::Cancel { id: &id, now })?;
    }
    Ok(())
}
fn schedule(tx: &Transaction<'_>, id: &str, now: i64) -> Result<(), StoreError> {
    let state = detail(tx, id)?;
    if state.status != ManagedTaskStatus::Active {
        return Ok(());
    }
    let outstanding: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM managed_bindings b JOIN obligations o ON o.id=b.obligation_id
        WHERE b.task_id=?1 AND o.state NOT IN ('completed','cancelled'))",
        [id],
        |r| r.get(0),
    )?;
    if outstanding {
        return Ok(());
    }
    let active = state
        .active
        .ok_or_else(|| invalid("active task has no active definition"))?;
    // A completed one-off cannot become a second occurrence through pause/resume or edits.
    let completed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM managed_bindings b JOIN obligations o ON o.id=b.obligation_id
        WHERE b.task_id=?1 AND b.definition_revision=?2 AND o.state='completed')",
        params![id, active.revision],
        |r| r.get(0),
    )?;
    if completed && !matches!(active.definition.trigger, ManagedTrigger::Recurring { .. }) {
        tx.execute("UPDATE managed_tasks SET status='completed',configuration_revision=configuration_revision+1,updated_at=?2 WHERE id=?1",params![id,now])?;
        domain_event(tx, id, "managed_completed", now, &json!({}))?;
        return Ok(());
    }
    // An already accepted dated occurrence can become overdue while an older
    // admitted revision finishes. Keep that exact occurrence rather than losing it.
    let scheduled = match &active.definition.trigger {
        ManagedTrigger::Once {
            local_datetime,
            timezone,
        } => once_timestamp(local_datetime, timezone),
        _ => next_time(&active.definition, now),
    };
    let at = match scheduled {
        Ok(at) => at,
        Err(StoreError::Recurrence(RecurrenceError::Exhausted)) => {
            // Exhaustion is successful completion of a finite schedule. Preserve
            // the result and kernel completion already written in this transaction.
            tx.execute("UPDATE managed_tasks SET status='completed',configuration_revision=configuration_revision+1,updated_at=?2 WHERE id=?1",params![id,now])?;
            domain_event(
                tx,
                id,
                "managed_schedule_exhausted",
                now,
                &json!({"definition_revision":active.revision,"reason":"recurrence_exhausted"}),
            )?;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let obligation_id = format!("note-{}", Uuid::new_v4());
    let new = NewObligation {
        id: obligation_id.clone(),
        description: active.definition.name.clone(),
        scheduled_at: at,
        recurrence: None,
        approval_required: false,
        retry: crate::RetryPolicy {
            max_attempts: active.definition.max_attempts,
            ..Default::default()
        },
    };
    validate_new(&new)?;
    apply_transition(tx, Transition::Create { new, now })?;
    tx.execute("INSERT INTO managed_bindings(obligation_id,task_id,definition_revision,profile_revision) VALUES (?1,?2,?3,?4)",
        params![obligation_id,id,active.revision,active.definition.profile_revision])?;
    let prior_due:Option<i64>=tx.query_row("SELECT max(o.scheduled_at) FROM managed_bindings b JOIN obligations o ON o.id=b.obligation_id
        WHERE b.task_id=?1 AND o.id!=?2 AND o.scheduled_at<=?3",params![id,obligation_id,now],|r|r.get(0))?;
    domain_event(
        tx,
        id,
        "managed_occurrence_scheduled",
        now,
        &json!({"obligation_id":obligation_id,"definition_revision":active.revision,"scheduled_at":at,
        "missed_tick_policy":"retain_one_due_occurrence_then_future", "coalesced_window_start":prior_due,"coalesced_through":now}),
    )?;
    Ok(())
}

impl Store {
    pub fn managed_create(
        &mut self,
        command: &str,
        definition: &ManagedTaskDefinition,
        now: i64,
    ) -> Result<ManagedTaskReceipt, StoreError> {
        validate(definition)?;
        let request = encode(&("create", definition))?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(receipt) = replay(&tx, command, &request)? {
            return Ok(receipt);
        }
        let id = format!("task-{}", Uuid::new_v4());
        tx.execute("INSERT INTO managed_tasks(id,configuration_revision,candidate_revision,status,created_at,updated_at) VALUES (?1,1,1,'draft',?2,?2)",params![id,now])?;
        tx.execute("INSERT INTO managed_definitions(task_id,revision,definition_json,created_at) VALUES (?1,1,?2,?3)",params![id,encode(definition)?,now])?;
        let result = receipt(&tx, command, &request, &id, now, "managed_draft_created")?;
        tx.commit()?;
        Ok(result)
    }
    pub fn managed_revise(
        &mut self,
        command: &str,
        id: &str,
        expected: i64,
        definition: &ManagedTaskDefinition,
        now: i64,
    ) -> Result<ManagedTaskReceipt, StoreError> {
        validate(definition)?;
        let request = encode(&("revise", id, expected, definition))?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(receipt) = replay(&tx, command, &request)? {
            return Ok(receipt);
        }
        let state = detail(&tx, id)?;
        ensure_revision(&state, expected)?;
        let rev: i64 = tx.query_row(
            "SELECT coalesce(max(revision),0)+1 FROM managed_definitions WHERE task_id=?1",
            [id],
            |r| r.get(0),
        )?;
        tx.execute("INSERT INTO managed_definitions(task_id,revision,definition_json,created_at) VALUES (?1,?2,?3,?4)",params![id,rev,encode(definition)?,now])?;
        tx.execute("UPDATE managed_tasks SET candidate_revision=?2,configuration_revision=configuration_revision+1,updated_at=?3 WHERE id=?1",params![id,rev,now])?;
        let result = receipt(&tx, command, &request, id, now, "managed_candidate_revised")?;
        tx.commit()?;
        Ok(result)
    }
    pub(crate) fn managed_detail_in_read(&self, id: &str) -> Result<ManagedTaskDetail, StoreError> {
        detail(&self.connection, id)
    }
    pub fn managed_detail(&self, id: &str) -> Result<ManagedTaskDetail, StoreError> {
        let tx = self.connection.unchecked_transaction()?;
        let result = detail(&tx, id)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn managed_preview(
        &self,
        id: &str,
        session: &str,
        profiles: &[ManagedCapabilityProfile],
        now: i64,
    ) -> Result<ManagedTaskPreview, StoreError> {
        let state = self.managed_detail(id)?;
        let candidate = state
            .candidate
            .as_ref()
            .or(state.active.as_ref())
            .ok_or_else(|| conflict("task has no definition"))?;
        let definition = &candidate.definition;
        let mut reasons = blockers(definition, profiles, now);
        if state.candidate.is_none() {
            reasons.push("No proposed revision to activate".into());
        }
        if state.status == ManagedTaskStatus::Completed {
            reasons.push("Completed one-off tasks cannot be activated again".into());
        }
        let occurrences = preview_occurrences(definition, now)?;
        let changes = match &state.active {
            None => vec!["Create and activate this task".into()],
            Some(active) => definition_changes(&active.definition, definition)?,
        };
        Ok(ManagedTaskPreview {
            task_id: id.into(),
            configuration_revision: state.configuration_revision,
            candidate_revision: candidate.revision,
            session_id: session.into(),
            profile_revision: definition.profile_revision.clone(),
            profile: profile_for(definition, profiles).cloned(),
            definition: definition.clone(),
            blockers: reasons,
            changes,
            occurrences,
        })
    }
    /// Resume reviews always describe the effective active revision. An unrelated
    /// draft cannot replace the already accepted responsibility or its authority.
    pub fn managed_resume_preview(
        &self,
        id: &str,
        session: &str,
        profiles: &[ManagedCapabilityProfile],
        now: i64,
    ) -> Result<ManagedTaskPreview, StoreError> {
        let tx = self.connection.unchecked_transaction()?;
        let state = detail(&tx, id)?;
        let active = state
            .active
            .as_ref()
            .ok_or_else(|| conflict("task has no active definition"))?;
        let retained = retained_admission(&tx, id)?;
        let mut reasons = resume_blockers(&active.definition, profiles, now, retained.is_some());
        if state.status != ManagedTaskStatus::Paused {
            reasons.push("Only paused tasks can be resumed".into());
        }
        let mut occurrences = if state.status == ManagedTaskStatus::Completed {
            Vec::new()
        } else {
            preview_occurrences(&active.definition, now)?
        };
        let changes = if let Some((obligation_id, scheduled_at)) = &retained {
            if matches!(active.definition.trigger, ManagedTrigger::Once { .. }) {
                occurrences = vec![*scheduled_at];
            }
            vec![format!(
                "Retain admitted run {obligation_id}; its original definition, retry and reconciliation responsibility continue"
            )]
        } else {
            vec!["Resume future admissions using the active definition".into()]
        };
        let preview = ManagedTaskPreview {
            task_id: id.into(),
            configuration_revision: state.configuration_revision,
            candidate_revision: active.revision,
            session_id: session.into(),
            profile_revision: active.definition.profile_revision.clone(),
            profile: profile_for(&active.definition, profiles).cloned(),
            definition: active.definition.clone(),
            blockers: reasons,
            changes,
            occurrences,
        };
        tx.commit()?;
        Ok(preview)
    }
    pub fn managed_activate(
        &mut self,
        command: &str,
        reviewed: &ManagedTaskPreview,
        session: &str,
        profiles: &[ManagedCapabilityProfile],
        now: i64,
    ) -> Result<ManagedTaskReceipt, StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = activate_in_transaction(&tx, command, reviewed, session, profiles, now)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn managed_pause(
        &mut self,
        command: &str,
        id: &str,
        expected: i64,
        now: i64,
    ) -> Result<ManagedTaskReceipt, StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = pause_in_transaction(&tx, command, id, expected, now)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn managed_resume(
        &mut self,
        command: &str,
        id: &str,
        expected: i64,
        profiles: &[ManagedCapabilityProfile],
        now: i64,
    ) -> Result<ManagedTaskReceipt, StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = resume_in_transaction(&tx, command, id, expected, profiles, now)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn managed_task_for_obligation(&self, id: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .connection
            .query_row(
                "SELECT task_id FROM managed_bindings WHERE obligation_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub fn is_managed_obligation(&self, id: &str) -> Result<bool, StoreError> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM managed_bindings WHERE obligation_id=?1)",
            [id],
            |r| r.get(0),
        )?)
    }
    pub fn claim_due_notes(
        &mut self,
        now: i64,
        lease_seconds: i64,
        limit: usize,
    ) -> Result<Vec<Claim>, StoreError> {
        if lease_seconds <= 0 || limit > 100 {
            return Err(invalid(
                "note claim lease must be positive and batch at most 100",
            ));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        recover_expired_in_transaction(&tx, now)?;
        let ids=tx.prepare("SELECT o.id FROM obligations o JOIN managed_bindings b ON b.obligation_id=o.id
            JOIN managed_tasks t ON t.id=b.task_id JOIN managed_definitions d ON d.task_id=b.task_id AND d.revision=b.definition_revision
            WHERE o.state IN ('pending','retry_scheduled') AND o.next_wake_at<=?1
              AND b.profile_revision='local-note-v1' AND json_extract(d.definition_json,'$.capability')='local_note'
              AND (b.admitted_at IS NOT NULL OR (t.status='active' AND t.active_revision=b.definition_revision))
            ORDER BY o.next_wake_at,o.id LIMIT ?2")?.query_map(params![now,limit as i64],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        let mut claims = Vec::new();
        for id in ids {
            let claim = apply_transition(
                &tx,
                Transition::Claim {
                    id: &id,
                    now,
                    lease_seconds,
                },
            )?
            .claim
            .expect("claim transition");
            tx.execute("UPDATE managed_bindings SET admitted_at=coalesce(admitted_at,?2) WHERE obligation_id=?1",params![id,now])?;
            claims.push(claim);
        }
        tx.commit()?;
        Ok(claims)
    }
    pub fn managed_note_definition(
        &self,
        id: &str,
    ) -> Result<ManagedDefinitionRevision, StoreError> {
        let (task, rev): (String, i64) = self.connection.query_row(
            "SELECT task_id,definition_revision FROM managed_bindings WHERE obligation_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        revision(&self.connection, &task, Some(rev))?.ok_or_else(|| StoreError::NotFound(id.into()))
    }
    pub fn complete_managed_note(
        &mut self,
        claim: &Claim,
        result: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (task, rev): (String, i64) = tx.query_row(
            "SELECT task_id,definition_revision FROM managed_bindings WHERE obligation_id=?1",
            [&claim.obligation_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let def =
            revision(&tx, &task, Some(rev))?.ok_or_else(|| invalid("bound definition missing"))?;
        if !bounded(result, def.definition.max_output_chars as usize) {
            return Err(invalid("note result exceeds approved output bound"));
        }
        let existing: Option<String> = tx
            .query_row(
                "SELECT result FROM managed_results WHERE obligation_id=?1",
                [&claim.obligation_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing != result {
                return Err(conflict("completed note result differs from replay"));
            }
            let obligation = require_obligation(&tx, &claim.obligation_id)?;
            if obligation.state == ObligationState::Completed {
                return Ok(());
            }
        }
        let obligation = require_obligation(&tx, &claim.obligation_id)?;
        verify_claim(&obligation, claim, now)?;
        tx.execute(
            "INSERT INTO managed_results(obligation_id,result,created_at) VALUES (?1,?2,?3)",
            params![claim.obligation_id, result, now],
        )?;
        apply_transition(
            &tx,
            Transition::Complete {
                claim,
                completion: Completion::Succeeded {
                    evidence: Some(format!("Local result saved for {}", claim.obligation_id)),
                },
                now,
            },
        )?;
        let state = detail(&tx, &task)?;
        if state.active.as_ref().is_some_and(|a| {
            a.revision == rev && !matches!(a.definition.trigger, ManagedTrigger::Recurring { .. })
        }) {
            tx.execute("UPDATE managed_tasks SET status='completed',configuration_revision=configuration_revision+1,updated_at=?2 WHERE id=?1",params![task,now])?;
        } else {
            schedule(&tx, &task, now)?;
        }
        domain_event(
            &tx,
            &task,
            "managed_note_completed",
            now,
            &json!({"obligation_id":claim.obligation_id,"definition_revision":rev}),
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn managed_catalogue(
        &self,
        query: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<ManagedCataloguePage, StoreError> {
        if !bounded(query, 200)
            || after.is_some_and(|value| !bounded(value, 300))
            || !(1..=100).contains(&limit)
        {
            return Err(invalid("catalogue search or page exceeds bounds"));
        }
        let terms: Vec<_> = query.split_whitespace().collect();
        let terms_json = encode(&terms)?;
        let tx = self.connection.unchecked_transaction()?;
        let items=tx.prepare("WITH catalogue AS (
            SELECT t.id AS id,'managed' AS kind,json_extract(d.definition_json,'$.name') AS name,
                   json_extract(d.definition_json,'$.purpose') AS description,t.status AS status,t.id AS search_identity,json_extract(d.definition_json,'$.instructions') || ' ' || CASE json_extract(d.definition_json,'$.capability') WHEN 'local_note' THEN 'local note reminder' ELSE replace(json_extract(d.definition_json,'$.capability'),'_',' ') END AS search_text
            FROM managed_tasks t JOIN managed_definitions d ON d.task_id=t.id AND d.revision=coalesce(t.active_revision,t.candidate_revision)
            UNION ALL
            SELECT o.id,'gardener','Garden Bokkie',o.description,o.state,g.repository,g.repository FROM gardener_repositories g JOIN obligations o ON o.id=g.inspection_obligation_id
            UNION ALL
            SELECT o.id,'engineering',o.description,o.description,o.state,e.id,'' FROM engineering_outcomes e JOIN obligations o ON o.id=e.root_obligation_id
        ) SELECT id,kind,name,description,status FROM catalogue
        WHERE kind||':'||id>?1 AND (instr(lower(id),lower(?4))>0 OR instr(lower(search_identity),lower(?4))>0 OR NOT EXISTS (SELECT 1 FROM json_each(?2) term WHERE instr(lower(catalogue.kind || ' task ' || catalogue.name || ' ' || catalogue.description || ' ' || catalogue.search_text),lower(term.value))=0))
        ORDER BY kind,id LIMIT ?3")?.query_map(params![after.unwrap_or(""),terms_json,limit as i64+1,query],|r|Ok(ManagedCatalogueEntry {
            id:r.get(0)?,kind:r.get(1)?,name:r.get(2)?,description:r.get(3)?,status:r.get(4)? }))?.collect::<Result<Vec<_>,_>>()?;
        let mut items = items;
        let next_after = if items.len() > limit {
            items.truncate(limit);
            items.last().map(|i| format!("{}:{}", i.kind, i.id))
        } else {
            None
        };
        tx.commit()?;
        Ok(ManagedCataloguePage { items, next_after })
    }
}
/// Only a trusted, revision-fenced operator retry can recover a local note.
/// It reuses the admitted occurrence; it cannot change configuration or release
/// a lease. Other adapters must define their own recovery authority.
pub(super) fn validate_fenced_retry(tx: &Transaction<'_>, id: &str) -> Result<(), StoreError> {
    let binding: Option<(Option<i64>, String, String)> = tx.query_row(
        "SELECT b.admitted_at,b.profile_revision,json_extract(d.definition_json,'$.capability')
         FROM managed_bindings b JOIN managed_definitions d ON d.task_id=b.task_id AND d.revision=b.definition_revision WHERE b.obligation_id=?1",
        [id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    ).optional()?;
    if let Some((admitted, profile, capability)) = binding {
        if admitted.is_none() || profile != "local-note-v1" || capability != "local_note" {
            return Err(conflict(
                "this managed occurrence requires its designated recovery adapter",
            ));
        }
    }
    Ok(())
}
pub(super) fn reject_generic(tx: &Transaction<'_>, id: &str) -> Result<(), StoreError> {
    let managed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM managed_bindings WHERE obligation_id=?1)",
        [id],
        |r| r.get(0),
    )?;
    if managed {
        Err(conflict(
            "managed task executions require managed task commands",
        ))
    } else {
        Ok(())
    }
}
fn definition_changes(
    old: &ManagedTaskDefinition,
    new: &ManagedTaskDefinition,
) -> Result<Vec<String>, StoreError> {
    let old = serde_json::to_value(old).map_err(|e| invalid(&e.to_string()))?;
    let new = serde_json::to_value(new).map_err(|e| invalid(&e.to_string()))?;
    Ok(new
        .as_object()
        .expect("definition object")
        .iter()
        .filter(|(k, v)| old.get(*k) != Some(*v))
        .map(|(key, value)| {
            format!(
                "{}: {} → {}",
                key.replace('_', " "),
                old.get(key).unwrap_or(&serde_json::Value::Null),
                value
            )
        })
        .collect())
}

pub(crate) fn activate_in_transaction(
    tx: &Transaction<'_>,
    command: &str,
    reviewed: &ManagedTaskPreview,
    session: &str,
    profiles: &[ManagedCapabilityProfile],
    now: i64,
) -> Result<ManagedTaskReceipt, StoreError> {
    if reviewed.session_id != session {
        return Err(conflict("review belongs to another service session"));
    }
    let request = encode(&("activate", reviewed))?;
    if let Some(receipt) = replay(tx, command, &request)? {
        return Ok(receipt);
    }
    let state = detail(tx, &reviewed.task_id)?;
    ensure_revision(&state, reviewed.configuration_revision)?;
    let candidate = state
        .candidate
        .ok_or_else(|| conflict("task has no candidate definition"))?;
    if candidate.revision != reviewed.candidate_revision
        || candidate.definition != reviewed.definition
        || reviewed.profile_revision != candidate.definition.profile_revision
        || profile_for(&candidate.definition, profiles) != reviewed.profile.as_ref()
    {
        return Err(conflict(
            "reviewed definition or capability profile changed",
        ));
    }
    if state.status == ManagedTaskStatus::Completed {
        return Err(conflict("completed one-off cannot be activated again"));
    }
    let reasons = blockers(&candidate.definition, profiles, now);
    if !reviewed.blockers.is_empty() || !reasons.is_empty() {
        return Err(conflict(&format!(
            "activation blocked: {}",
            reasons.join("; ")
        )));
    }
    cancel_unadmitted(tx, &state.id, now)?;
    tx.execute("UPDATE managed_tasks SET active_revision=?2,candidate_revision=NULL,status='active',configuration_revision=configuration_revision+1,updated_at=?3 WHERE id=?1",
            params![state.id,candidate.revision,now])?;
    schedule(tx, &state.id, now)?;
    let result = receipt(tx, command, &request, &state.id, now, "managed_activated")?;
    Ok(result)
}

pub(crate) fn pause_in_transaction(
    tx: &Transaction<'_>,
    command: &str,
    id: &str,
    expected: i64,
    now: i64,
) -> Result<ManagedTaskReceipt, StoreError> {
    let request = encode(&("pause", id, expected))?;
    if let Some(receipt) = replay(tx, command, &request)? {
        return Ok(receipt);
    }
    let state = detail(tx, id)?;
    ensure_revision(&state, expected)?;
    if state.status != ManagedTaskStatus::Active {
        return Err(conflict("only active tasks can be paused"));
    }
    cancel_unadmitted(tx, id, now)?;
    tx.execute("UPDATE managed_tasks SET status='paused',configuration_revision=configuration_revision+1,updated_at=?2 WHERE id=?1",params![id,now])?;
    let result = receipt(tx, command, &request, id, now, "managed_paused")?;
    Ok(result)
}

pub(crate) fn resume_in_transaction(
    tx: &Transaction<'_>,
    command: &str,
    id: &str,
    expected: i64,
    profiles: &[ManagedCapabilityProfile],
    now: i64,
) -> Result<ManagedTaskReceipt, StoreError> {
    let request = encode(&("resume", id, expected, profiles))?;
    if let Some(receipt) = replay(tx, command, &request)? {
        return Ok(receipt);
    }
    let state = detail(tx, id)?;
    ensure_revision(&state, expected)?;
    if state.status != ManagedTaskStatus::Paused {
        return Err(conflict("only paused tasks can be resumed"));
    }
    let active = state
        .active
        .ok_or_else(|| conflict("task has no active definition"))?;
    let retained = retained_admission(tx, id)?.is_some();
    let reasons = resume_blockers(&active.definition, profiles, now, retained);
    if !reasons.is_empty() {
        return Err(conflict(&format!("resume blocked: {}", reasons.join("; "))));
    }
    tx.execute("UPDATE managed_tasks SET status='active',configuration_revision=configuration_revision+1,updated_at=?2 WHERE id=?1",params![id,now])?;
    schedule(tx, id, now)?;
    let result = receipt(tx, command, &request, id, now, "managed_resumed")?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    fn profiles() -> Vec<ManagedCapabilityProfile> {
        vec![ManagedCapabilityProfile::local_note()]
    }
    fn create(store: &mut Store, definition: &ManagedTaskDefinition, now: i64) -> String {
        let receipt = store
            .managed_create(&Uuid::new_v4().to_string(), definition, now)
            .unwrap();
        receipt.task_id
    }
    fn activate(store: &mut Store, id: &str, now: i64) {
        let preview = store
            .managed_preview(id, "session", &profiles(), now)
            .unwrap();
        store
            .managed_activate(
                &Uuid::new_v4().to_string(),
                &preview,
                "session",
                &profiles(),
                now,
            )
            .unwrap();
    }
    fn recurring(instructions: &str) -> ManagedTaskDefinition {
        let mut def = ManagedTaskDefinition::local_note("Daily note", instructions);
        def.trigger = ManagedTrigger::Recurring {
            cron: "* * * * *".into(),
            timezone: "UTC".into(),
        };
        def
    }
    #[test]
    fn draft_and_preview_never_create_obligations_and_changed_replays_conflict() {
        let mut store = Store::open_in_memory().unwrap();
        let def = ManagedTaskDefinition::local_note("A", "first");
        let receipt = store.managed_create("create", &def, 0).unwrap();
        assert_eq!(store.managed_create("create", &def, 50).unwrap(), receipt);
        let mut altered = def.clone();
        altered.instructions = "changed".into();
        assert!(matches!(
            store.managed_create("create", &altered, 0),
            Err(StoreError::Conflict(_))
        ));
        let watermark = store.change_page(0, None, 1000).unwrap().through;
        for _ in 0..3 {
            assert!(
                store
                    .managed_preview(&receipt.task_id, "session", &profiles(), 0)
                    .unwrap()
                    .blockers
                    .is_empty()
            );
        }
        assert_eq!(watermark, store.change_page(0, None, 1000).unwrap().through);
        assert!(store.list().unwrap().is_empty());
        let changes = store.change_page(0, None, 100).unwrap();
        assert!(matches!(
            changes.items[0].envelope.source,
            crate::EventSource::DomainEvent { .. }
        ));
        assert_eq!(
            changes.items[0].entity_id.as_deref(),
            Some(receipt.task_id.as_str())
        );
    }
    #[test]
    fn unavailable_capabilities_and_changed_session_profile_or_definition_cannot_activate() {
        let mut store = Store::open_in_memory().unwrap();
        let mut def = ManagedTaskDefinition::local_note("Research", "Read sites and email me");
        def.capability = "research_email".into();
        let id = create(&mut store, &def, 0);
        let preview = store
            .managed_preview(&id, "session", &profiles(), 0)
            .unwrap();
        assert!(!preview.blockers.is_empty());
        assert!(
            store
                .managed_activate("bad", &preview, "session", &profiles(), 0)
                .is_err()
        );
        let id = create(
            &mut store,
            &ManagedTaskDefinition::local_note("note", "hello"),
            0,
        );
        let preview = store
            .managed_preview(&id, "session", &profiles(), 0)
            .unwrap();
        assert!(
            store
                .managed_activate("bad-session", &preview, "restarted", &profiles(), 0)
                .is_err()
        );
        let mut changed = profiles();
        changed[0].max_attempts = 4;
        assert!(
            store
                .managed_activate("changed-profile", &preview, "session", &changed, 0)
                .is_err()
        );
        store
            .managed_revise(
                "revise",
                &id,
                1,
                &ManagedTaskDefinition::local_note("note", "new"),
                0,
            )
            .unwrap();
        assert!(
            store
                .managed_activate("stale", &preview, "session", &profiles(), 0)
                .is_err()
        );
        assert!(store.list().unwrap().is_empty());
    }
    #[test]
    fn active_candidate_does_not_pause_and_admission_keeps_original_revision() {
        let mut store = Store::open_in_memory().unwrap();
        let original = recurring("old result");
        let id = create(&mut store, &original, 0);
        activate(&mut store, &id, 0);
        let claim = store.claim_due_notes(60, 30, 1).unwrap().pop().unwrap();
        assert!(store.claim_due(60, 30, 100).unwrap().is_empty());
        assert!(store.claim_due_gardener(60, 30, 100).unwrap().is_empty());
        assert!(store.cancel(&claim.obligation_id, 60).is_err());
        assert!(
            store
                .complete(&claim, Completion::Succeeded { evidence: None }, 60)
                .is_err()
        );
        store
            .managed_revise("edit", &id, 2, &recurring("new result"), 60)
            .unwrap();
        assert_eq!(
            store.managed_detail(&id).unwrap().status,
            ManagedTaskStatus::Active
        );
        activate(&mut store, &id, 60);
        assert_eq!(
            store
                .managed_note_definition(&claim.obligation_id)
                .unwrap()
                .definition
                .instructions,
            "old result"
        );
        assert!(store.claim_due_notes(61, 30, 10).unwrap().is_empty());
        store
            .complete_managed_note(&claim, "old result", 65)
            .unwrap();
        let state = store.managed_detail(&id).unwrap();
        assert_eq!(state.next_wake_at, Some(120));
        assert_eq!(
            state.runs.iter().filter(|r| r.state == "pending").count(),
            1
        );
        assert_eq!(state.runs[0].definition_revision, 2);
        let claim = store.claim_due_notes(120, 30, 1).unwrap().pop().unwrap();
        assert_eq!(
            store
                .managed_note_definition(&claim.obligation_id)
                .unwrap()
                .definition
                .instructions,
            "new result"
        );
    }
    #[test]
    fn edits_replace_only_unadmitted_wake_and_pause_blocks_new_admission() {
        let mut store = Store::open_in_memory().unwrap();
        let id = create(&mut store, &recurring("old"), 0);
        activate(&mut store, &id, 0);
        let old = store.managed_detail(&id).unwrap().runs[0]
            .obligation_id
            .clone();
        let mut new = recurring("new");
        new.trigger = ManagedTrigger::Recurring {
            cron: "*/5 * * * *".into(),
            timezone: "UTC".into(),
        };
        store.managed_revise("revise", &id, 2, &new, 10).unwrap();
        activate(&mut store, &id, 10);
        assert_eq!(
            store.get(&old).unwrap().unwrap().state,
            ObligationState::Cancelled
        );
        assert_eq!(store.managed_detail(&id).unwrap().next_wake_at, Some(300));
        store.managed_pause("pause", &id, 4, 20).unwrap();
        assert!(store.claim_due_notes(600, 30, 1).unwrap().is_empty());
        assert!(store.managed_resume("disabled", &id, 5, &[], 600).is_err());
        store
            .managed_resume("resume", &id, 5, &profiles(), 600)
            .unwrap();
        assert_eq!(store.managed_detail(&id).unwrap().next_wake_at, Some(900));
    }
    #[test]
    fn paused_admitted_work_retries_after_restart_and_oneoff_never_resumes() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("notes.sqlite");
        let mut store = Store::open(&path).unwrap();
        let id = create(
            &mut store,
            &ManagedTaskDefinition::local_note("note", "result"),
            0,
        );
        activate(&mut store, &id, 0);
        let claim = store.claim_due_notes(0, 10, 1).unwrap().pop().unwrap();
        store.managed_pause("pause", &id, 2, 1).unwrap();
        drop(store);
        let mut store = Store::open(&path).unwrap();
        assert!(store.claim_due_notes(10, 30, 1).unwrap().is_empty()); // recovery schedules retry at 40
        let retry = store.claim_due_notes(40, 30, 1).unwrap().pop().unwrap();
        assert!(matches!(
            store.complete_managed_note(&claim, "result", 40),
            Err(StoreError::Fenced)
        ));
        store.complete_managed_note(&retry, "result", 41).unwrap();
        store.complete_managed_note(&retry, "result", 100).unwrap(); // exact duplicate completion
        assert!(
            store
                .complete_managed_note(&retry, "different", 100)
                .is_err()
        );
        let state = store.managed_detail(&id).unwrap();
        assert_eq!(state.status, ManagedTaskStatus::Completed);
        assert_eq!(state.runs.len(), 1);
        assert_eq!(state.runs[0].result.as_deref(), Some("result"));
        assert!(
            store
                .managed_resume(
                    "resume",
                    &id,
                    state.configuration_revision,
                    &profiles(),
                    100
                )
                .is_err()
        );
        assert!(store.claim_due_notes(1000, 30, 1).unwrap().is_empty());
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.managed_detail(&id).unwrap(), state);
    }
    #[test]
    fn exhausted_notes_recover_only_with_fenced_operator_retry_and_keep_original_binding() {
        for pause in [false, true] {
            let directory = TempDir::new().unwrap();
            let path = directory.path().join("notes.sqlite");
            let mut store = Store::open(&path).unwrap();
            let id = create(&mut store, &recurring("original text"), 0);
            activate(&mut store, &id, 0);
            let mut now = 60;
            let mut obligation_id = String::new();
            for _ in 0..3 {
                let claim = store.claim_due_notes(now, 1, 1).unwrap().pop().unwrap();
                obligation_id = claim.obligation_id;
                now += 1;
                assert!(store.claim_due_notes(now, 1, 1).unwrap().is_empty());
                if let Some(next) = store.get(&obligation_id).unwrap().unwrap().next_wake_at {
                    now = next;
                }
            }
            assert_eq!(
                store.get(&obligation_id).unwrap().unwrap().state,
                ObligationState::Attention
            );
            store
                .managed_revise("future", &id, 2, &recurring("future text"), now)
                .unwrap();
            activate(&mut store, &id, now);
            if pause {
                let expected = store.managed_detail(&id).unwrap().configuration_revision;
                store.managed_pause("pause", &id, expected, now).unwrap();
            }
            drop(store);
            let mut store = Store::open(&path).unwrap();
            let snapshot = store.operator_snapshot(now).unwrap();
            let occurrence = snapshot
                .obligations
                .iter()
                .find(|o| o.id == obligation_id)
                .unwrap();
            assert!(occurrence.capabilities.retry.available);
            assert!(!occurrence.capabilities.cancel.available);
            let fence = occurrence.capabilities.retry.precondition.clone().unwrap();
            assert!(store.retry_attention(&obligation_id, now).is_err());
            store
                .retry_attention_if_current(&obligation_id, &fence, now)
                .unwrap();
            assert!(
                store
                    .retry_attention_if_current(&obligation_id, &fence, now)
                    .is_err()
            );
            assert!(store.claim_due(now, 30, 10).unwrap().is_empty());
            let claim = store.claim_due_notes(now, 30, 1).unwrap().pop().unwrap();
            assert_eq!(claim.obligation_id, obligation_id);
            assert_eq!(
                store
                    .managed_note_definition(&obligation_id)
                    .unwrap()
                    .revision,
                1
            );
            store
                .complete_managed_note(&claim, "original text", now)
                .unwrap();
            store
                .complete_managed_note(&claim, "original text", now)
                .unwrap();
            let state = store.managed_detail(&id).unwrap();
            assert_eq!(state.active.unwrap().revision, 2);
            assert_eq!(
                state.runs.iter().filter(|run| run.result.is_some()).count(),
                1
            );
            if pause {
                assert_eq!(state.status, ManagedTaskStatus::Paused);
                assert!(state.next_wake_at.is_none());
            } else {
                assert!(state.next_wake_at.unwrap() > now);
            }
        }
    }
    #[test]
    fn overdue_recurrence_coalesces_backlog_strictly_after_completion() {
        let mut store = Store::open_in_memory().unwrap();
        let id = create(&mut store, &recurring("note"), 0);
        activate(&mut store, &id, 0);
        let claim = store.claim_due_notes(3600, 30, 1).unwrap().pop().unwrap();
        store.complete_managed_note(&claim, "note", 3605).unwrap();
        assert_eq!(store.managed_detail(&id).unwrap().next_wake_at, Some(3660));
        assert!(store.claim_due_notes(3605, 30, 1).unwrap().is_empty());
    }
    #[test]
    fn named_timezone_preview_rejects_dst_gaps_and_folds_and_shifts_utc() {
        assert!(once_timestamp("2026-10-04T02:30", "Australia/Adelaide").is_err());
        assert!(once_timestamp("2026-04-05T02:30", "Australia/Adelaide").is_err());
        let mut store = Store::open_in_memory().unwrap();
        let mut def = recurring("daily");
        def.trigger = ManagedTrigger::Recurring {
            cron: "30 9 * * *".into(),
            timezone: "Australia/Adelaide".into(),
        };
        let id = create(&mut store, &def, 0);
        let before = once_timestamp("2026-10-03T08:00", "Australia/Adelaide").unwrap();
        let preview = store
            .managed_preview(&id, "s", &profiles(), before)
            .unwrap();
        assert_eq!(preview.occurrences[1] - preview.occurrences[0], 23 * 3600);
        let mut once = def;
        once.trigger = ManagedTrigger::Once {
            local_datetime: "2026-10-03T09:30".into(),
            timezone: "Australia/Adelaide".into(),
        };
        let id = create(&mut store, &once, before);
        let once_preview = store
            .managed_preview(&id, "s", &profiles(), before)
            .unwrap();
        assert_eq!(once_preview.occurrences, vec![preview.occurrences[0]]);
    }
    #[test]
    fn catalogue_search_runs_in_database_beyond_first_page() {
        let mut store = Store::open_in_memory().unwrap();
        let mut ids = Vec::new();
        for index in 0..110 {
            ids.push(create(
                &mut store,
                &ManagedTaskDefinition::local_note(format!("Task {index}"), "note"),
                0,
            ));
        }
        let first = store.managed_catalogue("", None, 50).unwrap();
        assert_eq!(first.items.len(), 50);
        assert!(first.next_after.is_some());
        let second = store
            .managed_catalogue("", first.next_after.as_deref(), 50)
            .unwrap();
        let third = store
            .managed_catalogue("", second.next_after.as_deref(), 50)
            .unwrap();
        assert_eq!(third.items.len(), 10);
        assert!(third.next_after.is_none());
        let unique: std::collections::BTreeSet<_> = first
            .items
            .iter()
            .chain(second.items.iter())
            .chain(third.items.iter())
            .map(|i| &i.id)
            .collect();
        assert_eq!(unique.len(), 110);
        let wanted = ids.iter().max().unwrap();
        assert!(!first.items.iter().any(|item| &item.id == wanted));
        let result = store.managed_catalogue(wanted, None, 50).unwrap();
        assert_eq!(result.items.len(), 1);
        assert_eq!(&result.items[0].id, wanted);
        assert_eq!(
            store
                .managed_catalogue("Task 109", None, 50)
                .unwrap()
                .items
                .len(),
            1
        );
    }
    #[test]
    fn catalogue_matches_separate_words_and_note_kind_without_literal_name_phrase() {
        let mut store = Store::open_in_memory().unwrap();
        let mut definition =
            ManagedTaskDefinition::local_note("Review research queue", "Choose one paper to read.");
        definition.purpose = "Remind the operator to review their research queue.".into();
        let id = create(&mut store, &definition, 0);
        for query in [
            "research queue reminder",
            "REMINDER research",
            "paper queue",
            "research    queue",
        ] {
            let matches = store.managed_catalogue(query, None, 20).unwrap();
            assert_eq!(matches.items.len(), 1, "{query}");
            assert_eq!(matches.items[0].id, id);
        }
        assert!(
            store
                .managed_catalogue("research unrelated", None, 20)
                .unwrap()
                .items
                .is_empty()
        );
        assert!(
            store
                .managed_catalogue("%", None, 20)
                .unwrap()
                .items
                .is_empty()
        );
    }
    #[test]
    fn competing_pause_and_claim_admit_at_most_one_and_preserve_original_work() {
        for _ in 0..5 {
            let dir = TempDir::new().unwrap();
            let path = dir.path().join("race.sqlite");
            let mut store = Store::open(&path).unwrap();
            let id = create(
                &mut store,
                &ManagedTaskDefinition::local_note("race", "note"),
                0,
            );
            activate(&mut store, &id, 0);
            let mut other = Store::open(&path).unwrap();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let ready = barrier.clone();
            let worker = std::thread::spawn(move || {
                ready.wait();
                other.claim_due_notes(0, 30, 100).unwrap()
            });
            barrier.wait();
            store.managed_pause("pause", &id, 2, 0).unwrap();
            let claims = worker.join().unwrap();
            assert!(claims.len() <= 1);
            assert!(store.claim_due_notes(0, 30, 100).unwrap().is_empty());
            if let Some(claim) = claims.first() {
                store.complete_managed_note(claim, "note", 1).unwrap();
            }
            let state = store.managed_detail(&id).unwrap();
            assert!(
                state
                    .runs
                    .iter()
                    .all(|r| r.state == "cancelled" || r.state == "completed")
            );
        }
    }
    #[test]
    fn changed_current_once_waits_for_original_admission_and_keeps_accepted_date() {
        let mut store = Store::open_in_memory().unwrap();
        let id = create(&mut store, &recurring("old"), 0);
        activate(&mut store, &id, 0);
        let claim = store.claim_due_notes(60, 300, 1).unwrap().pop().unwrap();
        let mut once = ManagedTaskDefinition::local_note("new dated note", "new");
        once.trigger = ManagedTrigger::Once {
            local_datetime: "1970-01-01T00:02:00".into(),
            timezone: "UTC".into(),
        };
        store.managed_revise("edit", &id, 2, &once, 61).unwrap();
        activate(&mut store, &id, 61);
        assert!(store.claim_due_notes(121, 30, 1).unwrap().is_empty());
        store.complete_managed_note(&claim, "old", 125).unwrap();
        let state = store.managed_detail(&id).unwrap();
        assert_eq!(state.status, ManagedTaskStatus::Active);
        assert_eq!(state.next_wake_at, Some(120));
        let claim = store.claim_due_notes(125, 30, 1).unwrap().pop().unwrap();
        store.complete_managed_note(&claim, "new", 126).unwrap();
        assert_eq!(
            store.managed_detail(&id).unwrap().status,
            ManagedTaskStatus::Completed
        );
    }
    #[test]
    fn paused_admitted_recurrence_finishes_without_new_wake_then_resumes_future() {
        let mut store = Store::open_in_memory().unwrap();
        let id = create(&mut store, &recurring("note"), 0);
        activate(&mut store, &id, 0);
        let claim = store.claim_due_notes(60, 30, 1).unwrap().pop().unwrap();
        store.managed_pause("pause", &id, 2, 61).unwrap();
        store.complete_managed_note(&claim, "note", 62).unwrap();
        let state = store.managed_detail(&id).unwrap();
        assert_eq!(state.status, ManagedTaskStatus::Paused);
        assert!(state.next_wake_at.is_none());
        store
            .managed_resume("resume", &id, 3, &profiles(), 600)
            .unwrap();
        assert_eq!(store.managed_detail(&id).unwrap().next_wake_at, Some(660));
        let changes = store.change_page(0, None, 100).unwrap();
        assert!(
            changes
                .items
                .iter()
                .any(|event| event.event_type == "managed_occurrence_scheduled"
                    && event.details_json.contains("coalesced_window_start")
                    && event.details_json.contains("660"))
        );
    }
    #[test]
    fn resume_preview_preserves_admitted_dated_retry_and_ignores_candidate() {
        let mut store = Store::open_in_memory().unwrap();
        let mut once = ManagedTaskDefinition::local_note("Dated note", "accepted");
        once.trigger = ManagedTrigger::Once {
            local_datetime: "1970-01-01T00:01:00".into(),
            timezone: "UTC".into(),
        };
        let id = create(&mut store, &once, 0);
        activate(&mut store, &id, 0);
        let original = store.claim_due_notes(60, 10, 1).unwrap().pop().unwrap();
        store.managed_pause("pause", &id, 2, 61).unwrap();
        let mut unrelated = recurring("candidate instructions");
        unrelated.capability = "research_email".into();
        store
            .managed_revise("candidate", &id, 3, &unrelated, 62)
            .unwrap();
        assert!(store.claim_due_notes(70, 30, 1).unwrap().is_empty());
        let preview = store
            .managed_resume_preview(&id, "session", &profiles(), 75)
            .unwrap();
        assert!(preview.blockers.is_empty());
        assert_eq!(preview.definition, once);
        assert_eq!(preview.candidate_revision, 1);
        assert_eq!(preview.occurrences, vec![60]);
        assert!(preview.changes[0].contains(&original.obligation_id));
        store
            .managed_resume("resume", &id, 4, &profiles(), 75)
            .unwrap();
        let state = store.managed_detail(&id).unwrap();
        assert_eq!(state.runs.len(), 1);
        assert_eq!(state.next_wake_at, Some(100));
        let retry = store.claim_due_notes(100, 30, 1).unwrap().pop().unwrap();
        assert_eq!(retry.obligation_id, original.obligation_id);
        store
            .complete_managed_note(&retry, "accepted", 101)
            .unwrap();
        let preview = store
            .managed_resume_preview(&id, "session", &profiles(), 102)
            .unwrap();
        assert!(!preview.blockers.is_empty());
        assert!(preview.occurrences.is_empty());
    }
    #[test]
    fn overdue_unadmitted_once_requires_explicit_new_date_but_recurrence_previews_future() {
        let mut store = Store::open_in_memory().unwrap();
        let mut once = ManagedTaskDefinition::local_note("Dated note", "unadmitted");
        once.trigger = ManagedTrigger::Once {
            local_datetime: "1970-01-01T00:01:00".into(),
            timezone: "UTC".into(),
        };
        let id = create(&mut store, &once, 0);
        activate(&mut store, &id, 0);
        store.managed_pause("pause", &id, 2, 30).unwrap();
        let preview = store
            .managed_resume_preview(&id, "session", &profiles(), 100)
            .unwrap();
        assert!(preview.occurrences.is_empty());
        assert!(
            preview
                .blockers
                .iter()
                .any(|reason| reason.contains("without admission"))
        );
        assert!(
            store
                .managed_resume("late", &id, 3, &profiles(), 100)
                .is_err()
        );
        let id = create(&mut store, &recurring("note"), 0);
        activate(&mut store, &id, 0);
        store.managed_pause("pause-recurring", &id, 2, 30).unwrap();
        let preview = store
            .managed_resume_preview(&id, "session", &profiles(), 600)
            .unwrap();
        assert!(preview.blockers.is_empty());
        assert_eq!(preview.occurrences, vec![660, 720, 780, 840, 900]);
        let disabled = store
            .managed_resume_preview(&id, "session", &[], 600)
            .unwrap();
        assert!(!disabled.blockers.is_empty());
    }
    #[test]
    fn catalogue_engineering_uses_navigable_root_and_searches_outcome_identity() {
        use crate::engineering::{
            EngineeringActor, EngineeringBudget, EngineeringCommand, EngineeringCommandEnvelope,
            EngineeringContract, EngineeringInstructions,
        };
        let mut store = Store::open_in_memory().unwrap();
        let instructions = EngineeringInstructions {
            text: "bounded engineering".into(),
            digest: format!("{:x}", Sha256::digest(b"bounded engineering")),
            context_digests: vec![],
            profile_digest: "a".repeat(64),
            adapter_id: "test-adapter".into(),
        };
        let contract = EngineeringContract {
            intent: "Build catalogue fixture".into(),
            criteria: vec![],
            permitted_scope: vec!["fixture-workspace".into()],
            prohibited_effects: vec!["publication".into()],
            authority: vec![],
            supervisor: instructions.clone(),
            worker: instructions,
            budget: EngineeringBudget {
                max_turns: 2,
                max_packages: 1,
                max_repairs: 0,
                max_recoveries: 1,
                max_checkpoints: 1,
                max_questions: 1,
                max_concurrent_workers: 1,
                turn_seconds: 60,
                deadline: 1000,
            },
        };
        let receipt = store
            .engineering_command(
                EngineeringActor::Operator {
                    name: "operator".into(),
                },
                EngineeringCommandEnvelope {
                    command_id: "engineering-fixture".into(),
                    expected: None,
                    command: EngineeringCommand::CreateOutcome { contract },
                },
                0,
            )
            .unwrap();
        let outcome = store
            .engineering_outcome(&receipt.outcome_id)
            .unwrap()
            .unwrap();
        assert_ne!(outcome.root.id, outcome.id);
        for query in [&outcome.id, &outcome.root.id, "Build catalogue fixture"] {
            let page = store.managed_catalogue(query, None, 10).unwrap();
            assert_eq!(page.items.len(), 1);
            assert_eq!(page.items[0].id, outcome.root.id);
            assert!(store.get(&page.items[0].id).unwrap().is_some());
        }
    }
    #[test]
    fn final_finite_recurrence_keeps_result_and_completes_schedule_durably() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("finite.sqlite");
        let mut store = Store::open(&database).unwrap();
        let at = once_timestamp("2026-01-01T00:00:00", "UTC").unwrap();
        let mut definition =
            ManagedTaskDefinition::local_note("Annual finite note", "final result");
        definition.trigger = ManagedTrigger::Recurring {
            cron: "0 0 0 1 1 * 2026".into(),
            timezone: "UTC".into(),
        };
        let id = create(&mut store, &definition, at - 1);
        activate(&mut store, &id, at - 1);
        let claim = store.claim_due_notes(at, 30, 1).unwrap().pop().unwrap();
        store
            .complete_managed_note(&claim, "final result", at + 1)
            .unwrap();
        let state = store.managed_detail(&id).unwrap();
        assert_eq!(state.status, ManagedTaskStatus::Completed);
        assert!(state.next_wake_at.is_none());
        assert_eq!(state.runs.len(), 1);
        assert_eq!(state.runs[0].state, "completed");
        assert_eq!(state.runs[0].result.as_deref(), Some("final result"));
        let attempts = store.attempts(&claim.obligation_id).unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].outcome, AttemptOutcome::Succeeded);
        let events = store.change_page(0, None, 100).unwrap();
        assert_eq!(
            events
                .items
                .iter()
                .filter(|event| event.event_type == "managed_schedule_exhausted"
                    && event.entity_id.as_deref() == Some(id.as_str()))
                .count(),
            1
        );
        let watermark = events.through;
        store
            .complete_managed_note(&claim, "final result", at + 2)
            .unwrap();
        assert_eq!(store.change_page(0, None, 100).unwrap().through, watermark);
        drop(store);
        let mut store = Store::open(&database).unwrap();
        assert_eq!(store.managed_detail(&id).unwrap(), state);
        assert!(store.claim_due_notes(at + 3600, 30, 1).unwrap().is_empty());
    }
}
