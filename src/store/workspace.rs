//! Authenticated host observations reconcile one immutable managed execution.
use super::*;
use crate::workspace::{WorkspaceHost, bounded_text, validate_limits};
use bokkie_operator_api::*;
use serde::{Serialize, de::DeserializeOwned};
use std::collections::BTreeSet;

const LEASE_SECONDS: i64 = 90;
const MAX_BATCH: usize = 100;

fn invalid(message: &str) -> StoreError {
    StoreError::Invalid(message.into())
}
fn conflict(message: &str) -> StoreError {
    StoreError::Conflict(message.into())
}
fn encode(value: &impl Serialize) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|e| invalid(&e.to_string()))
}
fn decode<T: DeserializeOwned>(raw: &str) -> Result<T, StoreError> {
    serde_json::from_str(raw).map_err(|e| invalid(&e.to_string()))
}
fn text(value: &str, max: usize) -> Result<(), StoreError> {
    if !bounded_text(value, max, true) {
        return Err(invalid(
            "workspace observation has an empty, invalid or oversized field",
        ));
    }
    Ok(())
}
fn list(values: &[String], max: usize, required: bool) -> Result<(), StoreError> {
    if values.len() > max || required && values.is_empty() {
        return Err(invalid(
            "workspace evidence list exceeds bounds or is empty",
        ));
    }
    for value in values {
        text(value, 4096)?;
    }
    Ok(())
}

pub(super) fn validate_definition(def: &ManagedTaskDefinition) -> Result<(), StoreError> {
    let Some(assignment) = &def.workspace else {
        return if def.capability == "workspace" {
            Err(invalid(
                "workspace definition requires its pinned assignment",
            ))
        } else {
            Ok(())
        };
    };
    if def.capability != "workspace"
        || def.max_attempts != 1
        || def.destination != format!("workspace:{}", assignment.project.id)
        || def.effects != ["workspace_execution", "store_local_result"]
    {
        return Err(invalid(
            "workspace assignment requires its closed execution capability",
        ));
    }
    text(&assignment.project.id, 200)?;
    if assignment.project.revision <= 0 {
        return Err(invalid("invalid workspace project revision"));
    }
    crate::handoffs::validate_registration(&assignment.project.registration)?;
    crate::handoffs::validate_brief(&assignment.brief)?;
    validate_limits(&assignment.limits)?;
    text(&assignment.decision_rules, 4096)?;
    if assignment.criteria.is_empty()
        || assignment.criteria.len() > 64
        || assignment.permitted_actions.is_empty()
        || assignment.permitted_actions.len() > 32
    {
        return Err(invalid(
            "workspace assignment requires bounded criteria and permitted actions",
        ));
    }
    let mut ids = BTreeSet::new();
    for criterion in &assignment.criteria {
        text(&criterion.id, 200)?;
        text(&criterion.description, 4096)?;
        if !ids.insert(&criterion.id) {
            return Err(invalid("workspace criterion identities must be distinct"));
        }
    }
    let mut actions = BTreeSet::new();
    for action in &assignment.permitted_actions {
        text(action, 200)?;
        if !actions.insert(action) {
            return Err(invalid("workspace permitted actions must be distinct"));
        }
    }
    if encode(def)?.len() > 262144 {
        return Err(invalid("workspace definition exceeds 256 KiB"));
    }
    Ok(())
}

pub(super) fn validate_current_project(
    conn: &Connection,
    def: &ManagedTaskDefinition,
) -> Result<(), StoreError> {
    if let Some(assignment) = &def.workspace {
        let current: Option<(i64, String)> = conn
            .query_row(
                "SELECT revision,registration_json FROM workspace_projects WHERE id=?1",
                [&assignment.project.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((revision, raw)) = current else {
            return Err(conflict("workspace project is no longer registered"));
        };
        if revision != assignment.project.revision
            || decode::<ProjectRegistration>(&raw)? != assignment.project.registration
        {
            return Err(conflict(
                "workspace project changed since the saved definition; review a new snapshot",
            ));
        }
    }
    Ok(())
}

fn host_allows(host: &WorkspaceHost, def: &ManagedTaskDefinition) -> bool {
    let Some(assignment) = &def.workspace else {
        return false;
    };
    host.name == assignment.project.registration.host
        && host.projects.iter().any(|p| {
            p.project_id == assignment.project.id
                && p.profile_revision == def.profile_revision
                && assignment
                    .permitted_actions
                    .iter()
                    .all(|a| p.permitted_actions.contains(a))
                && assignment.limits.max_seconds <= p.limits.max_seconds
                && assignment.limits.max_turns <= p.limits.max_turns
                && assignment.limits.max_tokens <= p.limits.max_tokens
                && p.capability_profile(&host.name).effects == def.effects
                && p.capability_profile(&host.name).destination == def.destination
        })
}

pub(super) fn profile_allows(
    profile: &ManagedCapabilityProfile,
    definition: &ManagedTaskDefinition,
) -> bool {
    let (Some(policy), Some(assignment)) = (&profile.workspace_policy, &definition.workspace)
    else {
        return false;
    };
    profile.capability == "workspace"
        && profile.max_attempts == 1
        && policy.host_name == assignment.project.registration.host
        && policy.project_id == assignment.project.id
        && !policy.permitted_actions.is_empty()
        && assignment
            .permitted_actions
            .iter()
            .all(|a| policy.permitted_actions.contains(a))
        && assignment.limits.max_seconds <= policy.limits.max_seconds
        && assignment.limits.max_turns <= policy.limits.max_turns
        && assignment.limits.max_tokens <= policy.limits.max_tokens
}

struct Execution {
    dispatch: WorkspaceDispatch,
    host_id: String,
    status: String,
    progress: String,
    cursor: i64,
    cancel: bool,
    stopped: bool,
    result: Option<WorkspaceResult>,
}
fn execution(conn: &Connection, id: &str) -> Result<Execution, StoreError> {
    let row:(String,String,String,String,i64,bool,bool,Option<String>)=conn.query_row(
        "SELECT dispatch_json,host_id,status,progress,last_event_sequence,cancellation_requested,cessation_verified,result_json FROM workspace_executions WHERE execution_id=?1",
        [id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?)))
        .optional()?.ok_or_else(||StoreError::NotFound(format!("workspace execution {id}")))?;
    Ok(Execution {
        dispatch: decode(&row.0)?,
        host_id: row.1,
        status: row.2,
        progress: row.3,
        cursor: row.4,
        cancel: row.5,
        stopped: row.6,
        result: row.7.map(|s| decode(&s)).transpose()?,
    })
}
fn question(conn: &Connection, id: &str) -> Result<Option<WorkspaceQuestion>, StoreError> {
    let raw:Option<String>=conn.query_row("SELECT q.question_json FROM workspace_execution_questions q
        WHERE q.execution_id=?1 AND NOT EXISTS(SELECT 1 FROM workspace_execution_answers a WHERE a.execution_id=q.execution_id AND a.question_id=q.question_id)
        ORDER BY q.sequence DESC LIMIT 1",[id],|r|r.get(0)).optional()?;
    raw.map(|s| decode(&s)).transpose()
}
pub(super) fn run_for_obligation(
    conn: &Connection,
    id: &str,
) -> Result<Option<WorkspaceRun>, StoreError> {
    let execution_id: Option<String> = conn
        .query_row(
            "SELECT execution_id FROM workspace_executions WHERE obligation_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    execution_id
        .map(|id| {
            let e = execution(conn, &id)?;
            Ok(WorkspaceRun {
                execution_id: id.clone(),
                status: e.status,
                progress: e.progress,
                question: question(conn, &id)?,
                result: e.result,
                cessation_verified: e.stopped,
                cancellation_requested: e.cancel,
                last_event_sequence: e.cursor,
            })
        })
        .transpose()
}

pub(super) fn recover_expired(
    tx: &Transaction<'_>,
    id: &str,
    now: i64,
) -> Result<bool, StoreError> {
    let execution_id: Option<String> = tx
        .query_row(
            "SELECT execution_id FROM workspace_executions WHERE obligation_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(execution_id) = execution_id else {
        return Ok(false);
    };
    tx.execute("UPDATE workspace_executions SET status=CASE WHEN cancellation_requested THEN 'cancelling' ELSE 'attention' END,
        progress='Host lease expired; reconcile the same retained execution before accepting a result' WHERE execution_id=?1",[&execution_id])?;
    apply_workspace_transition(
        tx,
        id,
        WorkspaceTransition::Attention {
            reason: "Workspace host lease expired; execution ownership is retained and requires reconciliation",
            expired: true,
        },
        now,
    )?;
    Ok(true)
}

pub(super) fn recover_reconciliation_wakes(
    tx: &Transaction<'_>,
    now: i64,
) -> Result<(), StoreError> {
    let ids=tx.prepare("SELECT e.obligation_id FROM workspace_executions e JOIN obligations o ON o.id=e.obligation_id
        WHERE e.cessation_verified=0 AND o.state='pending' AND o.next_wake_at<=?1")?
        .query_map([now],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
    for id in ids {
        tx.execute("UPDATE workspace_executions SET status=CASE WHEN cancellation_requested THEN 'cancelling' ELSE 'attention' END,
            progress='Host reconciliation observation is overdue; ownership remains retained' WHERE obligation_id=?1",[&id])?;
        apply_workspace_transition(
            tx,
            &id,
            WorkspaceTransition::Attention {
                reason: "Workspace host reconciliation observation is overdue; retained execution needs attention",
                expired: false,
            },
            now,
        )?;
    }
    let deadlines=tx.prepare("SELECT e.execution_id,e.obligation_id FROM workspace_executions e JOIN obligations o ON o.id=e.obligation_id
        WHERE e.cessation_verified=0 AND e.cancellation_requested=0 AND e.deadline_at<=?1 AND o.state NOT IN ('completed','cancelled')")?
        .query_map([now],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
    for (execution_id, id) in deadlines {
        tx.execute("UPDATE workspace_executions SET cancellation_requested=1,status='cancelling',progress='Execution deadline reached; await verified cessation' WHERE execution_id=?1",[&execution_id])?;
        apply_workspace_transition(
            tx,
            &id,
            WorkspaceTransition::Attention {
                reason: "Workspace execution deadline reached; host must stop and prove cessation",
                expired: false,
            },
            now,
        )?;
    }
    Ok(())
}

fn validate_event(event: &WorkspaceEvent) -> Result<(), StoreError> {
    if encode(event)?.len() > 262144 {
        return Err(invalid("workspace event exceeds 256 KiB"));
    }
    match event {
        WorkspaceEvent::Started {
            runtime_id,
            instruction_sources,
        } => {
            text(runtime_id, 256)?;
            list(instruction_sources, 32, true)?;
        }
        WorkspaceEvent::Progress { summary } => text(summary, 4096)?,
        WorkspaceEvent::Question { question } => {
            text(&question.id, 200)?;
            text(&question.prompt, 4096)?;
            list(&question.options, 16, false)?;
            if !matches!(
                question.kind.as_str(),
                "routine" | "missing_information" | "inconclusive" | "new_authority"
            ) {
                return Err(invalid("unsupported workspace question kind"));
            }
        }
        WorkspaceEvent::Attention { reason } => text(reason, 4096)?,
        WorkspaceEvent::Stopped {
            cessation,
            result,
            verification,
            reason,
        } => {
            text(&cessation.boundary_id, 256)?;
            text(&cessation.evidence, 16384)?;
            text(reason, 4096)?;
            if !matches!(
                cessation.kind.as_str(),
                "not_started" | "descendants_reaped"
            ) {
                return Err(invalid("unsupported workspace cessation proof kind"));
            }
            if let Some(result) = result {
                text(&result.summary, 16384)?;
                list(&result.limitations, 32, false)?;
                if result.criteria.len() > 64 || result.deliveries.len() > 32 {
                    return Err(invalid(
                        "workspace result exceeds criterion or delivery bounds",
                    ));
                }
                let mut ids = BTreeSet::new();
                for criterion in &result.criteria {
                    text(&criterion.id, 200)?;
                    list(&criterion.evidence, 32, false)?;
                    if !ids.insert(&criterion.id) {
                        return Err(invalid("workspace result repeats a criterion identity"));
                    }
                }
                for delivery in &result.deliveries {
                    for (value, bound) in [
                        (&delivery.repository, 256),
                        (&delivery.pull_request, 2048),
                        (&delivery.reviewed_head, 64),
                        (&delivery.merge_revision, 64),
                        (&delivery.tree, 64),
                    ] {
                        if !bounded_text(value, bound, false) {
                            return Err(invalid(
                                "workspace delivery observation exceeds field bounds",
                            ));
                        }
                    }
                    list(&delivery.checks, 32, false)?;
                }
            }
            if let Some(verification) = verification {
                list(&verification.evidence, 64, false)?;
            }
        }
    }
    Ok(())
}
fn sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn schedule_after_cancellation(
    tx: &Transaction<'_>,
    task_id: &str,
    now: i64,
) -> Result<(), StoreError> {
    let recurring:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM managed_tasks t JOIN managed_definitions d ON d.task_id=t.id AND d.revision=t.active_revision WHERE t.id=?1 AND t.status='active' AND json_extract(d.definition_json,'$.trigger.kind')='recurring')",[task_id],|r|r.get(0))?;
    if recurring {
        super::managed::schedule(tx, task_id, now)?;
    }
    Ok(())
}
fn accepted(
    dispatch: &WorkspaceDispatch,
    result: Option<&WorkspaceResult>,
    verification: Option<&WorkspaceVerification>,
) -> bool {
    let (Some(result), Some(verification)) = (result, verification) else {
        return false;
    };
    verification.passed
        && !verification.evidence.is_empty()
        && result.limitations.is_empty()
        && result.criteria.len() == dispatch.assignment.criteria.len()
        && dispatch.assignment.criteria.iter().all(|wanted| {
            result.criteria.iter().any(|actual| {
                actual.id == wanted.id && actual.satisfied && !actual.evidence.is_empty()
            })
        })
        && !result.deliveries.is_empty()
        && result.deliveries.iter().all(|d| {
            sha(&d.reviewed_head)
                && sha(&d.merge_revision)
                && sha(&d.tree)
                && !d.checks.is_empty()
                && d.repository.split('/').count() == 2
                && d.repository.split('/').all(|part| {
                    !part.is_empty()
                        && part.bytes().all(|b| {
                            b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.'
                        })
                })
                && d.pull_request
                    .strip_prefix(&format!("https://github.com/{}/pull/", d.repository))
                    .is_some_and(|number| number.parse::<u64>().is_ok_and(|n| n > 0))
        })
}

fn reconcile_event(
    tx: &Transaction<'_>,
    host: &WorkspaceHost,
    event: &WorkspaceHostEvent,
    now: i64,
) -> Result<(), StoreError> {
    text(&event.execution_id, 200)?;
    if event.sequence <= 0 {
        return Err(invalid("workspace event sequence must be positive"));
    }
    validate_event(&event.event)?;
    let e = execution(tx, &event.execution_id)?;
    if e.host_id != host.id {
        return Err(conflict(
            "workspace execution belongs to another authenticated host",
        ));
    }
    let raw = encode(&event.event)?;
    if event.sequence <= e.cursor {
        let saved:Option<String>=tx.query_row("SELECT event_json FROM workspace_execution_events WHERE execution_id=?1 AND sequence=?2",params![event.execution_id,event.sequence],|r|r.get(0)).optional()?;
        return if saved.as_deref() == Some(&raw) {
            Ok(())
        } else {
            Err(conflict(
                "workspace event sequence replay changed its payload",
            ))
        };
    }
    if event.sequence != e.cursor + 1 {
        return Err(conflict(
            "workspace event sequence has a gap or arrived out of order",
        ));
    }
    if e.stopped {
        let saved:String=tx.query_row("SELECT event_json FROM workspace_execution_events WHERE execution_id=?1 AND json_extract(event_json,'$.kind')='stopped' ORDER BY sequence LIMIT 1",[&event.execution_id],|r|r.get(0))?;
        match (decode::<WorkspaceEvent>(&saved)?, &event.event) {
            (
                WorkspaceEvent::Stopped {
                    cessation: old_cessation,
                    result: old_result,
                    ..
                },
                WorkspaceEvent::Stopped {
                    cessation, result, ..
                },
            ) if e.status == "attention"
                && !e.cancel
                && old_cessation == *cessation
                && old_result == *result => {}
            _ => {
                return Err(conflict(
                    "stopped workspace execution permits only verification of the identical retained result and cessation",
                ));
            }
        }
    }
    if e.cancel
        && !matches!(
            event.event,
            WorkspaceEvent::Stopped { .. } | WorkspaceEvent::Attention { .. }
        )
    {
        return Err(conflict(
            "workspace cancellation permits only cessation reconciliation",
        ));
    }
    let id = &e.dispatch.obligation_id;
    match &event.event {
        WorkspaceEvent::Started { .. } => {
            let already:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM workspace_execution_events WHERE execution_id=?1 AND json_extract(event_json,'$.kind')='started')",[&event.execution_id],|r|r.get(0))?;
            if already {
                return Err(conflict("workspace execution already started"));
            }
            tx.execute("UPDATE workspace_executions SET status='running',progress='Workspace runtime started' WHERE execution_id=?1",[&event.execution_id])?;
        }
        WorkspaceEvent::Progress { summary } => {
            tx.execute(
                "UPDATE workspace_executions SET progress=?2 WHERE execution_id=?1",
                params![event.execution_id, summary],
            )?;
        }
        WorkspaceEvent::Question { question: new } => {
            let existing:Option<String>=tx.query_row("SELECT question_json FROM workspace_execution_questions WHERE execution_id=?1 AND question_id=?2",params![event.execution_id,new.id],|r|r.get(0)).optional()?;
            if existing.is_some() {
                return Err(conflict("workspace question identity is already retained"));
            }
            if question(tx, &event.execution_id)?.is_some() {
                return Err(conflict(
                    "resolve the existing workspace question before opening another",
                ));
            }
            let count: i64 = tx.query_row(
                "SELECT count(*) FROM workspace_execution_questions WHERE execution_id=?1",
                [&event.execution_id],
                |r| r.get(0),
            )?;
            if count >= 64 {
                return Err(conflict("workspace question budget exhausted"));
            }
            tx.execute("INSERT INTO workspace_execution_questions(execution_id,question_id,question_json,sequence) VALUES (?1,?2,?3,?4)",params![event.execution_id,new.id,encode(new)?,event.sequence])?;
            tx.execute("UPDATE workspace_executions SET status='waiting',progress=?2 WHERE execution_id=?1",params![event.execution_id,new.prompt])?;
            if new.kind == "new_authority" {
                apply_workspace_transition(
                    tx,
                    id,
                    WorkspaceTransition::Attention {
                        reason: "Workspace needs new authority; ordinary answers cannot extend the admitted scope",
                        expired: false,
                    },
                    now,
                )?;
            }
        }
        WorkspaceEvent::Attention { reason } => {
            tx.execute("UPDATE workspace_executions SET status=CASE WHEN cancellation_requested THEN 'cancelling' ELSE 'attention' END,progress=?2 WHERE execution_id=?1",params![event.execution_id,reason])?;
            apply_workspace_transition(
                tx,
                id,
                WorkspaceTransition::Attention {
                    reason,
                    expired: false,
                },
                now,
            )?;
        }
        WorkspaceEvent::Stopped {
            cessation,
            result,
            verification,
            reason,
        } => {
            let max_output:u32=tx.query_row(
                "SELECT json_extract(d.definition_json,'$.max_output_chars') FROM managed_bindings b JOIN managed_definitions d ON d.task_id=b.task_id AND d.revision=b.definition_revision WHERE b.obligation_id=?1",
                [id],|r|r.get(0))?;
            let accept = !e.cancel
                && cessation.kind == "descendants_reaped"
                && question(tx, &event.execution_id)?.is_none()
                && result
                    .as_ref()
                    .is_some_and(|result| result.summary.chars().count() <= max_output as usize)
                && accepted(&e.dispatch, result.as_ref(), verification.as_ref());
            let status = if e.cancel {
                "cancelled"
            } else if accept {
                "completed"
            } else {
                "attention"
            };
            tx.execute("UPDATE workspace_executions SET cessation_verified=1,result_json=?2,status=?3,progress=?4 WHERE execution_id=?1",
                params![event.execution_id,result.as_ref().map(encode).transpose()?,status,reason])?;
            if e.cancel {
                apply_workspace_transition(
                    tx,
                    id,
                    WorkspaceTransition::Cancelled {
                        evidence: &cessation.evidence,
                    },
                    now,
                )?;
                schedule_after_cancellation(tx, &e.dispatch.task_id, now)?;
            } else if accept {
                let result = result.as_ref().expect("accepted result");
                tx.execute("INSERT INTO managed_results(obligation_id,result,created_at) VALUES (?1,?2,?3)",params![id,result.summary,now])?;
                apply_workspace_transition(
                    tx,
                    id,
                    WorkspaceTransition::Accepted {
                        evidence: &cessation.evidence,
                    },
                    now,
                )?;
                super::managed::schedule(tx, &e.dispatch.task_id, now)?;
            } else {
                apply_workspace_transition(
                    tx,
                    id,
                    WorkspaceTransition::Attention {
                        reason: "Workspace stopped with retained results but acceptance evidence is incomplete; review the unmet criteria and delivery verification",
                        expired: false,
                    },
                    now,
                )?;
            }
        }
    }
    tx.execute("INSERT INTO workspace_execution_events(execution_id,sequence,event_json,observed_at) VALUES (?1,?2,?3,?4)",params![event.execution_id,event.sequence,raw,now])?;
    tx.execute(
        "UPDATE workspace_executions SET last_event_sequence=?2 WHERE execution_id=?1",
        params![event.execution_id, event.sequence],
    )?;
    let obligation = require_obligation(tx, id)?;
    append_event(
        tx,
        id,
        obligation.occurrence,
        "workspace_host_observation",
        now,
        Some(obligation.state),
        obligation.state,
        json!({"execution_id":event.execution_id,"sequence":event.sequence,"kind":serde_json::to_value(&event.event).map_err(|e|invalid(&e.to_string()))?["kind"]}),
    )?;
    super::managed::domain_event(
        tx,
        &e.dispatch.task_id,
        "workspace_execution_changed",
        now,
        &json!({"execution_id":event.execution_id,"sequence":event.sequence}),
    )?;
    Ok(())
}

fn heartbeat(
    tx: &Transaction<'_>,
    host: &WorkspaceHost,
    id: &str,
    now: i64,
) -> Result<(), StoreError> {
    text(id, 200)?;
    let e = execution(tx, id)?;
    if e.host_id != host.id {
        return Err(conflict(
            "workspace heartbeat belongs to another authenticated host",
        ));
    }
    if e.stopped || e.cancel || now >= e.dispatch.deadline_at {
        return Ok(());
    }
    let obligation = require_obligation(tx, &e.dispatch.obligation_id)?;
    let until = now
        .saturating_add(LEASE_SECONDS)
        .min(e.dispatch.deadline_at);
    if obligation.state == ObligationState::Running {
        let claim = Claim {
            obligation_id: obligation.id,
            occurrence: obligation.occurrence,
            attempt_number: obligation.attempts_made,
            lease_token: obligation.lease_token.ok_or(StoreError::Fenced)?,
            lease_generation: obligation.lease_generation,
            lease_expires_at: obligation.lease_expires_at.ok_or(StoreError::Fenced)?,
            description: obligation.description,
        };
        apply_transition(
            tx,
            Transition::Renew {
                claim: &claim,
                now,
                lease_seconds: until - now,
            },
        )?;
    } else if question(tx, id)?.is_none_or(|q| q.kind != "new_authority")
        && (obligation.state==ObligationState::Pending || obligation.last_error.as_deref().is_some_and(|reason|reason=="Workspace host lease expired; execution ownership is retained and requires reconciliation" || reason=="Workspace host reconciliation observation is overdue; retained execution needs attention"))
        && matches!(
            e.status.as_str(),
            "dispatching" | "running" | "waiting" | "attention"
        )
    {
        if obligation.state != ObligationState::Pending || obligation.next_wake_at != Some(until) {
            apply_workspace_transition(
                tx,
                &e.dispatch.obligation_id,
                WorkspaceTransition::ReconciliationWake { at: until },
                now,
            )?;
        }
        tx.execute("UPDATE workspace_executions SET status=CASE WHEN EXISTS(SELECT 1 FROM workspace_execution_questions q WHERE q.execution_id=?1 AND NOT EXISTS(SELECT 1 FROM workspace_execution_answers a WHERE a.execution_id=q.execution_id AND a.question_id=q.question_id)) THEN 'waiting' ELSE 'running' END WHERE execution_id=?1",[id])?;
    }
    Ok(())
}

impl Store {
    pub fn workspace_run(&self, id: &str) -> Result<WorkspaceRun, StoreError> {
        let e = execution(&self.connection, id)?;
        run_for_obligation(&self.connection, &e.dispatch.obligation_id)?
            .ok_or_else(|| StoreError::NotFound(id.into()))
    }

    /// Caller identity comes solely from the dedicated authenticated host route.
    /// The transaction saves dispatch intent and a lease before returning it.
    pub fn workspace_exchange(
        &mut self,
        host: &WorkspaceHost,
        request: &WorkspaceExchangeRequest,
        now: i64,
    ) -> Result<WorkspaceExchangeResponse, StoreError> {
        host.validate()?;
        if request.events.len() > MAX_BATCH
            || request.heartbeats.len() > MAX_BATCH
            || encode(request)?.len() > 1_048_576
        {
            return Err(invalid(
                "workspace exchange exceeds bounded events, heartbeats or payload",
            ));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        recover_expired_in_transaction(&tx, now)?;
        let mut acknowledgements = Vec::new();
        for event in &request.events {
            reconcile_event(&tx, host, event, now)?;
            acknowledgements.push(WorkspaceAcknowledgement {
                execution_id: event.execution_id.clone(),
                sequence: event.sequence,
            });
        }
        for id in &request.heartbeats {
            heartbeat(&tx, host, id, now)?;
        }
        let due=tx.prepare("SELECT o.id,b.task_id,b.definition_revision,b.profile_revision,d.definition_json FROM obligations o
            JOIN managed_bindings b ON b.obligation_id=o.id JOIN managed_tasks t ON t.id=b.task_id
            JOIN managed_definitions d ON d.task_id=b.task_id AND d.revision=b.definition_revision
            WHERE o.state='pending' AND o.next_wake_at<=?1 AND b.admitted_at IS NULL
            AND t.status='active' AND t.active_revision=b.definition_revision AND json_extract(d.definition_json,'$.capability')='workspace'
            AND json_extract(d.definition_json,'$.workspace.project.registration.host')=?2
            AND EXISTS(SELECT 1 FROM json_each(?3) p WHERE json_extract(p.value,'$.project_id')=json_extract(d.definition_json,'$.workspace.project.id') AND json_extract(p.value,'$.profile_revision')=b.profile_revision)
            AND NOT EXISTS(SELECT 1 FROM workspace_executions e WHERE e.obligation_id=o.id)
            ORDER BY o.next_wake_at,o.id LIMIT 100")?
            .query_map(params![now,host.name,encode(&host.projects)?],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?)))?.collect::<Result<Vec<_>,_>>()?;
        for (id, task_id, definition_revision, profile_revision, raw) in due {
            let definition: ManagedTaskDefinition = decode(&raw)?;
            validate_definition(&definition)?;
            if !host_allows(host, &definition) {
                continue;
            }
            let assignment = definition
                .workspace
                .expect("validated workspace assignment");
            let deadline_at = now
                .checked_add(i64::from(assignment.limits.max_seconds))
                .ok_or_else(|| invalid("workspace execution deadline overflows"))?;
            apply_transition(
                &tx,
                Transition::Claim {
                    id: &id,
                    now,
                    lease_seconds: LEASE_SECONDS.min(deadline_at - now),
                },
            )?;
            let dispatch = WorkspaceDispatch {
                execution_id: Uuid::new_v4().to_string(),
                task_id: task_id.clone(),
                obligation_id: id.clone(),
                definition_revision,
                profile_revision,
                admitted_at: now,
                deadline_at,
                assignment,
            };
            tx.execute("INSERT INTO workspace_executions(execution_id,obligation_id,host_id,dispatch_json,status,admitted_at,deadline_at) VALUES (?1,?2,?3,?4,'dispatching',?5,?6)",params![dispatch.execution_id,id,host.id,encode(&dispatch)?,now,deadline_at])?;
            tx.execute(
                "UPDATE managed_bindings SET admitted_at=?2 WHERE obligation_id=?1",
                params![id, now],
            )?;
            super::managed::domain_event(
                &tx,
                &task_id,
                "workspace_execution_admitted",
                now,
                &json!({"execution_id":dispatch.execution_id,"obligation_id":id}),
            )?;
        }
        let ids=tx.prepare("SELECT execution_id FROM workspace_executions WHERE host_id=?1 AND cessation_verified=0 ORDER BY admitted_at,execution_id LIMIT 100")?
            .query_map([&host.id],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        let mut dispatches = Vec::new();
        let mut controls = Vec::new();
        for id in ids {
            let e = execution(&tx, &id)?;
            let answers=tx.prepare("SELECT answer_json FROM workspace_execution_answers WHERE execution_id=?1 ORDER BY answered_at,question_id")?
                .query_map([&id],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?.into_iter().map(|s|decode(&s)).collect::<Result<Vec<WorkspaceAnswer>,_>>()?;
            controls.push(WorkspaceControl {
                execution_id: id,
                cancel: e.cancel,
                answers,
            });
            dispatches.push(e.dispatch);
        }
        tx.commit()?;
        Ok(WorkspaceExchangeResponse {
            acknowledgements,
            dispatches,
            controls,
        })
    }

    pub fn workspace_run_action(
        &mut self,
        request: &WorkspaceRunActionRequest,
        now: i64,
    ) -> Result<(), StoreError> {
        text(&request.command_id, 200)?;
        text(&request.execution_id, 200)?;
        text(&request.conversation_id, 200)?;
        if request.expected_event_sequence < 0 {
            return Err(invalid("workspace action sequence must be non-negative"));
        }
        if request.cancel == request.answer.is_some() {
            return Err(invalid(
                "choose one workspace answer or cancellation action",
            ));
        }
        let raw = encode(request)?;
        if raw.len() > 32768 {
            return Err(invalid("workspace action exceeds 32 KiB"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let replay: Option<String> = tx
            .query_row(
                "SELECT request_json FROM workspace_execution_commands WHERE command_id=?1",
                [&request.command_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(replay) = replay {
            return if replay == raw {
                Ok(())
            } else {
                Err(conflict(
                    "workspace command identity reused with changed payload",
                ))
            };
        }
        let e = execution(&tx, &request.execution_id)?;
        if request.expected_event_sequence != e.cursor {
            return Err(conflict("workspace run changed since review"));
        }
        if e.stopped && !(request.cancel && e.status == "attention") {
            return Err(conflict("workspace execution already stopped"));
        }
        if request.cancel {
            if !e.cancel {
                tx.execute("UPDATE workspace_executions SET cancellation_requested=1,status=CASE WHEN cessation_verified THEN 'cancelled' ELSE 'cancelling' END,progress=CASE WHEN cessation_verified THEN 'Cancellation reconciled against retained cessation proof' ELSE 'Cancellation requested; await verified cessation' END WHERE execution_id=?1",[&request.execution_id])?;
                if e.stopped {
                    let saved:String=tx.query_row("SELECT event_json FROM workspace_execution_events WHERE execution_id=?1 AND json_extract(event_json,'$.kind')='stopped' ORDER BY sequence LIMIT 1",[&request.execution_id],|r|r.get(0))?;
                    let WorkspaceEvent::Stopped { cessation, .. } =
                        decode::<WorkspaceEvent>(&saved)?
                    else {
                        return Err(conflict("verified cessation record missing"));
                    };
                    apply_workspace_transition(
                        &tx,
                        &e.dispatch.obligation_id,
                        WorkspaceTransition::Cancelled {
                            evidence: &cessation.evidence,
                        },
                        now,
                    )?;
                    schedule_after_cancellation(&tx, &e.dispatch.task_id, now)?;
                } else {
                    apply_workspace_transition(
                        &tx,
                        &e.dispatch.obligation_id,
                        WorkspaceTransition::Attention {
                            reason: "Workspace cancellation requested; authenticated host must prove cessation",
                            expired: false,
                        },
                        now,
                    )?;
                }
            }
        } else if let Some(answer) = &request.answer {
            if e.cancel {
                return Err(conflict(
                    "cancelled workspace authority cannot receive answers",
                ));
            }
            text(&answer.question_id, 200)?;
            text(&answer.text, 4096)?;
            let raw_question:Option<String>=tx.query_row("SELECT question_json FROM workspace_execution_questions WHERE execution_id=?1 AND question_id=?2",params![request.execution_id,answer.question_id],|r|r.get(0)).optional()?;
            let q: WorkspaceQuestion =
                decode(&raw_question.ok_or_else(|| {
                    conflict("workspace question is not owned by this execution")
                })?)?;
            if q.kind == "new_authority" {
                return Err(conflict(
                    "new authority requires a new reviewed task definition; ordinary answers cannot widen scope",
                ));
            }
            let old:Option<String>=tx.query_row("SELECT answer_json FROM workspace_execution_answers WHERE execution_id=?1 AND question_id=?2",params![request.execution_id,answer.question_id],|r|r.get(0)).optional()?;
            let answer_raw = encode(answer)?;
            match old {
                Some(old) if old != answer_raw => {
                    return Err(conflict(
                        "workspace question already has an immutable answer",
                    ));
                }
                Some(_) => {}
                None => {
                    tx.execute("INSERT INTO workspace_execution_answers(execution_id,question_id,answer_json,answered_at) VALUES (?1,?2,?3,?4)",params![request.execution_id,answer.question_id,answer_raw,now])?;
                    tx.execute("UPDATE workspace_executions SET status='running',progress='Answer retained for delivery to the workspace host' WHERE execution_id=?1",[&request.execution_id])?;
                }
            }
        }
        tx.execute(
            "INSERT INTO workspace_execution_commands(command_id,request_json) VALUES (?1,?2)",
            params![request.command_id, raw],
        )?;
        super::managed::domain_event(
            &tx,
            &e.dispatch.task_id,
            "workspace_control_changed",
            now,
            &json!({"execution_id":request.execution_id,"cancel":request.cancel}),
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
