//! Authenticated host observations reconcile one immutable managed execution.
use super::*;
use crate::workspace::{WorkspaceHost, bounded_text, validate_limits};
use bokkie_operator_api::*;
use serde::{Serialize, de::DeserializeOwned};
use std::collections::{BTreeMap, BTreeSet};

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

/// Sort every object explicitly, including when another dependency enables
/// serde_json's preserve_order feature. Strings retain their compact UTF-8 form.
fn canonical_digest(value: &impl Serialize) -> Result<String, StoreError> {
    fn sorted(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(object) => {
                let entries: BTreeMap<_, _> = object.into_iter().collect();
                serde_json::Value::Object(
                    entries
                        .into_iter()
                        .map(|(key, value)| (key, sorted(value)))
                        .collect(),
                )
            }
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(sorted).collect())
            }
            scalar => scalar,
        }
    }
    let value = serde_json::to_value(value).map_err(|e| invalid(&e.to_string()))?;
    let bytes = serde_json::to_vec(&sorted(value)).map_err(|e| invalid(&e.to_string()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
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
    if assignment.repository_scope.len() > 32
        || assignment
            .repository_scope
            .iter()
            .any(|r| !repository_name(r))
        || assignment
            .repository_scope
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            != assignment.repository_scope.len()
    {
        return Err(invalid("select distinct bounded repository identities"));
    }
    if !assignment.result_contract.is_delivery()
        && (assignment.repository_scope.is_empty()
            || assignment.review_retained_work.is_some()
            || assignment
                .permitted_actions
                .iter()
                .any(|a| !matches!(a.as_str(), "inspect" | "verify")))
    {
        return Err(invalid(
            "evidence reports require an explicit repository scope and only inspect/verify actions",
        ));
    }
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
    if let Some(review) = &assignment.review_retained_work {
        if !matches!(def.trigger, ManagedTrigger::Immediate)
            || assignment
                .permitted_actions
                .iter()
                .any(|action| !matches!(action.as_str(), "inspect" | "verify"))
        {
            return Err(invalid(
                "retained evidence review is immediate and permits only inspection and verification",
            ));
        }
        text(&review.source.execution_id, 200)?;
        if !sha256(&review.source.result_digest) {
            return Err(invalid(
                "retained evidence source requires a canonical result digest",
            ));
        }
        text(&review.summary, def.max_output_chars as usize)?;
        if review.criteria.len() != assignment.criteria.len() {
            return Err(invalid(
                "retained evidence review must map every new completion criterion exactly",
            ));
        }
        let mut mapped = BTreeSet::new();
        for criterion in &review.criteria {
            text(&criterion.id, 200)?;
            list(&criterion.evidence, 32, true)?;
            if !ids.contains(&criterion.id) || !mapped.insert(&criterion.id) {
                return Err(invalid(
                    "retained evidence review has an unknown or repeated completion criterion",
                ));
            }
        }
    }
    if encode(def)?.len() > 262144 {
        return Err(invalid("workspace definition exceeds 256 KiB"));
    }
    Ok(())
}

/// The immutable definition is the binding; the authoritative source report
/// supplies all delivery identities. No browser or model delivery replacement
/// enters the review mode, and the original obligation is never closed here.
fn retained_review_result(
    conn: &Connection,
    task_id: &str,
    assignment: &WorkspaceTaskDefinition,
    host_id: Option<&str>,
) -> Result<Option<WorkspaceResult>, StoreError> {
    let Some(review) = &assignment.review_retained_work else {
        return Ok(None);
    };
    let source = execution(conn, &review.source.execution_id)?;
    let obligation = require_obligation(conn, &source.dispatch.obligation_id)?;
    if source.dispatch.task_id != task_id
        || source.dispatch.assignment.project != assignment.project
        || source.dispatch.assignment.review_retained_work.is_some()
        || host_id.is_some_and(|id| source.host_id != id)
        || !source.stopped
        || !obligation.state.is_terminal()
        || original_cessation(conn, &review.source.execution_id)?.kind != "descendants_reaped"
    {
        return Err(conflict(
            "retained evidence review requires an explicitly closed, unchained source from this exact task, project and host",
        ));
    }
    let result = source
        .result
        .ok_or_else(|| conflict("retained evidence source has no authoritative report"))?;
    if canonical_digest(&result)? != review.source.result_digest {
        return Err(conflict(
            "retained evidence source result digest changed or is incorrect",
        ));
    }
    if result.deliveries.is_empty() {
        return Err(conflict(
            "retained evidence source has no attributable deliveries to review",
        ));
    }
    Ok(Some(WorkspaceResult {
        report: None,
        summary: review.summary.clone(),
        criteria: review.criteria.clone(),
        deliveries: result.deliveries,
        limitations: vec![],
    }))
}

pub(super) fn validate_retained_review(
    conn: &Connection,
    task_id: &str,
    def: &ManagedTaskDefinition,
) -> Result<(), StoreError> {
    if let Some(assignment) = &def.workspace {
        retained_review_result(conn, task_id, assignment, None)?;
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
    recovery: Option<WorkspaceRecoveryProvenance>,
}
fn execution(conn: &Connection, id: &str) -> Result<Execution, StoreError> {
    let row:(String,String,String,String,i64,bool,bool,Option<String>)=conn.query_row(
        "SELECT dispatch_json,host_id,status,progress,last_event_sequence,cancellation_requested,cessation_verified,result_json FROM workspace_executions WHERE execution_id=?1",
        [id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?)))
        .optional()?.ok_or_else(||StoreError::NotFound(format!("workspace execution {id}")))?;
    let original: Option<WorkspaceResult> = row.7.map(|s| decode(&s)).transpose()?;
    let recovery:Option<(String,String)>=conn.query_row("SELECT result_json,provenance_json FROM workspace_execution_recoveries WHERE execution_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let (result, recovery) = match recovery {
        Some((result, provenance)) if original.is_none() => {
            (Some(decode(&result)?), Some(decode(&provenance)?))
        }
        Some(_) => {
            return Err(invalid(
                "workspace execution cannot have both an original and recovered result",
            ));
        }
        None => (original, None),
    };
    Ok(Execution {
        dispatch: decode(&row.0)?,
        host_id: row.1,
        status: row.2,
        progress: row.3,
        cursor: row.4,
        cancel: row.5,
        stopped: row.6,
        result,
        recovery,
    })
}
fn question(conn: &Connection, id: &str) -> Result<Option<WorkspaceQuestion>, StoreError> {
    let raw:Option<String>=conn.query_row("SELECT q.question_json FROM workspace_execution_questions q
        WHERE q.execution_id=?1 AND NOT EXISTS(SELECT 1 FROM workspace_execution_answers a WHERE a.execution_id=q.execution_id AND a.question_id=q.question_id)
        ORDER BY q.sequence DESC LIMIT 1",[id],|r|r.get(0)).optional()?;
    raw.map(|s| decode(&s)).transpose()
}

fn latest_verification(
    conn: &Connection,
    id: &str,
) -> Result<Option<WorkspaceVerification>, StoreError> {
    let raw:Option<String>=conn.query_row("SELECT event_json FROM workspace_execution_events WHERE execution_id=?1 AND json_extract(event_json,'$.kind') IN ('stopped','recovered_result') ORDER BY sequence DESC LIMIT 1",[id],|r|r.get(0)).optional()?;
    match raw.map(|raw| decode::<WorkspaceEvent>(&raw)).transpose()? {
        Some(
            WorkspaceEvent::Stopped { verification, .. }
            | WorkspaceEvent::RecoveredResult { verification, .. },
        ) => Ok(verification),
        _ => Ok(None),
    }
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
                recovery: e.recovery,
                verification: latest_verification(conn, &id)?,
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

fn validate_cessation(cessation: &WorkspaceCessation) -> Result<(), StoreError> {
    text(&cessation.boundary_id, 256)?;
    text(&cessation.evidence, 16384)?;
    if !matches!(
        cessation.kind.as_str(),
        "not_started" | "descendants_reaped"
    ) {
        return Err(invalid("unsupported workspace cessation proof kind"));
    }
    Ok(())
}

fn validate_result(result: &WorkspaceResult) -> Result<(), StoreError> {
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
    if let Some(report) = &result.report {
        text(&report.markdown, 32768)?;
        if report.format != "evidence-report-v1"
            || report.sources.is_empty()
            || report.sources.len() > 32
            || report.digest.len() != 64
            || !sha(&report.digest)
            || report.source_manifest_digest
                != canonical_digest(
                    &serde_json::json!({"format":"evidence-source-manifest-v1","sources":report.sources}),
                )?
            || report.digest
                != canonical_digest(
                    &serde_json::json!({"format":report.format,"markdown":report.markdown,"source_manifest_digest":report.source_manifest_digest}),
                )?
        {
            return Err(invalid(
                "evidence report content and sources must match their bounded digests",
            ));
        }
        let mut ids = BTreeSet::new();
        for source in &report.sources {
            text(&source.url, 2048)?;
            if source.id.len() != 64
                || !sha(&source.id)
                || !ids.insert(&source.id)
                || source.content_digest.len() != 64
                || !sha(&source.content_digest)
                || source.observed_at < 0
                || !(1..=262_144).contains(&source.bytes)
            {
                return Err(invalid(
                    "evidence report sources require distinct retained identities",
                ));
            }
        }
    }
    Ok(())
}

fn validate_verification(verification: Option<&WorkspaceVerification>) -> Result<(), StoreError> {
    if let Some(verification) = verification {
        list(&verification.evidence, 64, false)?;
    }
    Ok(())
}

fn validate_provenance(provenance: &WorkspaceRecoveryProvenance) -> Result<(), StoreError> {
    if provenance.origin != "host_reconciliation"
        || provenance.algorithm != "retained-delivery-v1"
        || provenance.recovered_at < 0
        || provenance.sources.is_empty()
        || provenance.sources.len() > 16
        || [
            &provenance.dispatch_digest,
            &provenance.admission_digest,
            &provenance.result_digest,
        ]
        .into_iter()
        .any(|digest| !sha256(digest))
    {
        return Err(invalid(
            "workspace recovery requires bounded host provenance and canonical SHA-256 identities",
        ));
    }
    validate_cessation(&provenance.cessation)?;
    if provenance.cessation.kind != "descendants_reaped" {
        return Err(invalid(
            "workspace result recovery requires verified descendant cessation",
        ));
    }
    for source in &provenance.sources {
        text(&source.kind, 200)?;
        if !sha256(&source.sha256) {
            return Err(invalid(
                "workspace recovery source requires a canonical SHA-256 identity",
            ));
        }
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
        WorkspaceEvent::Checkpoint { checkpoint } => {
            text(&checkpoint.stage, 200)?;
            text(&checkpoint.summary, 4096)?;
            text(&checkpoint.next_action, 2048)?;
            list(&checkpoint.evidence, 32, false)?;
            if encode(checkpoint)?.len() > 16384 {
                return Err(invalid(
                    "workspace checkpoint exceeds its retained output budget",
                ));
            }
            if !matches!(
                checkpoint.assessment.as_str(),
                "progress" | "passed" | "failed" | "inconclusive"
            ) {
                return Err(invalid("unsupported workspace checkpoint assessment"));
            }
        }
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
            validate_cessation(cessation)?;
            text(reason, 4096)?;
            if let Some(result) = result {
                validate_result(result)?;
            }
            validate_verification(verification.as_ref())?;
        }
        WorkspaceEvent::RecoveredResult {
            result,
            provenance,
            verification,
        } => {
            validate_result(result)?;
            validate_provenance(provenance)?;
            validate_verification(verification.as_ref())?;
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
    let complete = verification.passed
        && !verification.evidence.is_empty()
        && result.criteria.len() == dispatch.assignment.criteria.len()
        && dispatch.assignment.criteria.iter().all(|wanted| {
            result.criteria.iter().any(|actual| {
                actual.id == wanted.id && actual.satisfied && !actual.evidence.is_empty()
            })
        });
    if !complete {
        return false;
    }
    if !dispatch.assignment.result_contract.is_delivery() {
        // Subject evidence limits may be the completed assessment's findings.
        // Exact admitted criteria and trusted verification still own acceptance.
        return result.deliveries.is_empty()
            && result.report.as_ref().is_some_and(|report| {
                report.sources.iter().all(|source| {
                    dispatch.assignment.repository_scope.iter().any(|repo| {
                        source
                            .url
                            .starts_with(&format!("https://github.com/{repo}/"))
                    })
                })
            });
    }
    result.limitations.is_empty()
        && result.report.is_none()
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

fn repository_name(value: &str) -> bool {
    value.len() <= 256
        && value.split('/').count() == 2
        && value.split('/').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        })
}

fn original_cessation(conn: &Connection, id: &str) -> Result<WorkspaceCessation, StoreError> {
    let saved:Option<String>=conn.query_row("SELECT event_json FROM workspace_execution_events WHERE execution_id=?1 AND json_extract(event_json,'$.kind')='stopped' ORDER BY sequence LIMIT 1",[id],|r|r.get(0)).optional()?;
    match saved
        .map(|raw| decode::<WorkspaceEvent>(&raw))
        .transpose()?
    {
        Some(WorkspaceEvent::Stopped { cessation, .. }) => Ok(cessation),
        _ => Err(conflict(
            "workspace execution has no retained cessation observation",
        )),
    }
}

struct StoppedObservation<'a> {
    cessation: &'a WorkspaceCessation,
    result: Option<&'a WorkspaceResult>,
    verification: Option<&'a WorkspaceVerification>,
    reason: &'a str,
    save_original: bool,
}

/// Both an original result and an additive recovery use the same acceptance and
/// lifecycle gates. Verification cannot change either immutable result record.
fn reconcile_stopped(
    tx: &Transaction<'_>,
    e: &Execution,
    observation: StoppedObservation<'_>,
    now: i64,
) -> Result<(), StoreError> {
    let id = &e.dispatch.obligation_id;
    let max_output:u32=tx.query_row(
        "SELECT json_extract(d.definition_json,'$.max_output_chars') FROM managed_bindings b JOIN managed_definitions d ON d.task_id=b.task_id AND d.revision=b.definition_revision WHERE b.obligation_id=?1",
        [id],|r|r.get(0))?;
    let review_result = retained_review_result(
        tx,
        &e.dispatch.task_id,
        &e.dispatch.assignment,
        Some(&e.host_id),
    )?;
    if review_result
        .as_ref()
        .zip(observation.result)
        .is_some_and(|(expected, actual)| expected != actual)
    {
        return Err(conflict(
            "retained evidence review result must preserve the admitted summary, criterion mapping and authoritative source deliveries",
        ));
    }
    let accept = !e.cancel
        && (observation.cessation.kind == "descendants_reaped"
            || observation.cessation.kind == "not_started" && review_result.is_some())
        && question(tx, &e.dispatch.execution_id)?.is_none()
        && observation
            .result
            .is_some_and(|result| result.summary.chars().count() <= max_output as usize)
        && accepted(&e.dispatch, observation.result, observation.verification);
    let status = if e.cancel {
        "cancelled"
    } else if accept {
        "completed"
    } else {
        "attention"
    };
    if observation.save_original {
        tx.execute("UPDATE workspace_executions SET cessation_verified=1,result_json=?2,status=?3,progress=?4 WHERE execution_id=?1",
            params![e.dispatch.execution_id,observation.result.map(encode).transpose()?,status,observation.reason])?;
    } else {
        tx.execute(
            "UPDATE workspace_executions SET status=?2,progress=?3 WHERE execution_id=?1",
            params![e.dispatch.execution_id, status, observation.reason],
        )?;
    }
    if e.cancel {
        apply_workspace_transition(
            tx,
            id,
            WorkspaceTransition::Cancelled {
                evidence: &observation.cessation.evidence,
            },
            now,
        )?;
        schedule_after_cancellation(tx, &e.dispatch.task_id, now)?;
    } else if accept {
        let result = observation.result.expect("accepted result");
        tx.execute(
            "INSERT INTO managed_results(obligation_id,result,created_at) VALUES (?1,?2,?3)",
            params![id, result.summary, now],
        )?;
        apply_workspace_transition(
            tx,
            id,
            WorkspaceTransition::Accepted {
                evidence: &observation.cessation.evidence,
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
    Ok(())
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
        let cessation = original_cessation(tx, &event.execution_id)?;
        let permitted = match &event.event {
            WorkspaceEvent::Stopped {
                cessation: new,
                result,
                ..
            } => cessation == *new && e.result == *result,
            WorkspaceEvent::RecoveredResult { provenance, .. } => {
                e.result.is_none()
                    && e.recovery.is_none()
                    && cessation.kind == "descendants_reaped"
                    && cessation == provenance.cessation
                    && question(tx, &event.execution_id)?.is_none()
            }
            _ => false,
        };
        if e.status != "attention" || e.cancel || !permitted {
            return Err(conflict(
                "stopped workspace execution permits only bounded recovery or verification of its identical retained result and cessation",
            ));
        }
    }
    if e.cancel
        && !matches!(
            event.event,
            WorkspaceEvent::Stopped { .. }
                | WorkspaceEvent::Started { .. }
                | WorkspaceEvent::Progress { .. }
                | WorkspaceEvent::Checkpoint { .. }
                | WorkspaceEvent::Question { .. }
                | WorkspaceEvent::Attention { .. }
        )
    {
        return Err(conflict(
            "workspace cancellation permits only cessation reconciliation",
        ));
    }
    let id = &e.dispatch.obligation_id;
    // The host may have durably queued observations before it receives the
    // cancellation control. Acknowledge their original sequence and payload so
    // cessation can follow, while cancellation retains all responsibility.
    let audit_only = e.cancel && !matches!(event.event, WorkspaceEvent::Stopped { .. });
    match &event.event {
        _ if audit_only => {}
        WorkspaceEvent::Started { .. } => {
            if e.dispatch.assignment.review_retained_work.is_some() {
                return Err(conflict(
                    "retained evidence review cannot start a workspace implementation runtime",
                ));
            }
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
        WorkspaceEvent::Checkpoint { checkpoint } => {
            let progress = format!(
                "{} · {}\n{}\nEvidence: {}\nNext: {}",
                checkpoint.stage,
                checkpoint.assessment,
                checkpoint.summary,
                checkpoint.evidence.join("; "),
                checkpoint.next_action
            );
            tx.execute(
                "UPDATE workspace_executions SET progress=?2 WHERE execution_id=?1",
                params![event.execution_id, progress],
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
            reconcile_stopped(
                tx,
                &e,
                StoppedObservation {
                    cessation,
                    result: result.as_ref(),
                    verification: verification.as_ref(),
                    reason,
                    save_original: e.recovery.is_none(),
                },
                now,
            )?;
        }
        WorkspaceEvent::RecoveredResult {
            result,
            provenance,
            verification,
        } => {
            let obligation = require_obligation(tx, id)?;
            if !e.stopped
                || e.status != "attention"
                || obligation.state != ObligationState::Attention
                || e.cancel
                || e.result.is_some()
                || e.recovery.is_some()
                || question(tx, &event.execution_id)?.is_some()
            {
                return Err(conflict(
                    "workspace result recovery requires stopped attention with no original result, recovery, cancellation or unresolved question",
                ));
            }
            let cessation = original_cessation(tx, &event.execution_id)?;
            if cessation.kind != "descendants_reaped"
                || cessation != provenance.cessation
                || provenance.recovered_at < e.dispatch.admitted_at
                || provenance.recovered_at > now
                || canonical_digest(&e.dispatch)? != provenance.dispatch_digest
                || canonical_digest(result)? != provenance.result_digest
            {
                return Err(conflict(
                    "workspace recovered result does not match its original dispatch, result digest, observation time or cessation",
                ));
            }
            tx.execute("INSERT INTO workspace_execution_recoveries(execution_id,event_sequence,result_json,provenance_json,recorded_at) VALUES (?1,?2,?3,?4,?5)",
            params![event.execution_id,event.sequence,encode(result)?,encode(provenance)?,now])?;
            reconcile_stopped(
                tx,
                &e,
                StoppedObservation {
                    cessation: &provenance.cessation,
                    result: Some(result),
                    verification: verification.as_ref(),
                    reason: "Workspace result recovered from retained host delivery evidence",
                    save_original: false,
                },
                now,
            )?;
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
            retained_review_result(
                &tx,
                &task_id,
                definition
                    .workspace
                    .as_ref()
                    .expect("validated workspace assignment"),
                Some(&host.id),
            )?;
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
