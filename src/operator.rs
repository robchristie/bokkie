//! Authoritative read-only operator projections over the existing durable store.

#[cfg(test)]
use std::collections::BTreeSet;

use bokkie_operator_api::{
    ActionCapability, ActionConsequence, ActionPrecondition, ApprovalSubject, AttentionCause,
    DisabledReason, DurableLiveness, ExceptionReason, ObligationTopic, OperatorCapabilities,
    OperatorFailureDisposition, OperatorObligation, OperatorObligationState, OperatorSnapshot,
    TopicItem, TopicSource,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    ApprovalDecision, AuditEvent, GardenerEvent, GardenerImplementationRun, GardenerRunEvent,
    GardenerVerificationVerdict, Obligation, ObligationState, ProposalObservation, Store,
    StoreError,
    gardener::ProposalInstance,
    store::{
        approval_transition_is_legal, attempt_from_row, audit_from_row, cancel_transition_is_legal,
        gardener_proposal_transition_is_legal, generic_approval_transition_is_legal,
        obligation_from_row, proposal_instance_for_obligation, retry_transition_is_legal,
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

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SnapshotCursorKey {
    captured_at: i64,
    exception_rank: i64,
    state_rank: i64,
    wake_rank: i64,
    updated_at: i64,
    id: String,
}

/// API-v1 topic ordering is observable: wall-clock time, source category,
/// source-local sequence, then stable source identity. The watermark makes
/// this mutable multi-table projection an exact, restart-on-change walk.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct TopicCursorKey {
    captured_at: i64,
    occurred_at: i64,
    source_rank: i64,
    source_sequence: String,
    stable_id: String,
}

#[derive(Debug)]
struct TopicCandidate {
    key: TopicCursorKey,
    source: TopicSource,
    source_identity: String,
    occurrence: Option<u32>,
    event_type: String,
}

const TOPIC_CANDIDATES_SQL: &str = "
    WITH direct_fingerprints(fingerprint) AS (
        SELECT DISTINCT proposal_fingerprint
          FROM gardener_proposal_instances
         WHERE implementation_obligation_id = ?1
    ), related_inspections(id) AS (
        SELECT id FROM gardener_inspections WHERE obligation_id = ?1
        UNION
        SELECT source_inspection_id FROM gardener_proposal_instances
         WHERE implementation_obligation_id = ?1
        UNION
        SELECT po.inspection_id
          FROM gardener_proposal_observations po
         WHERE po.proposal_fingerprint IN (SELECT fingerprint FROM direct_fingerprints)
    ), related_fingerprints(fingerprint) AS (
        SELECT fingerprint FROM direct_fingerprints
        UNION
        SELECT DISTINCT po.proposal_fingerprint
          FROM gardener_proposal_observations po
         WHERE NOT EXISTS (SELECT 1 FROM direct_fingerprints)
           AND po.inspection_id IN (SELECT id FROM related_inspections)
    ), topic_items(
        occurred_at, source_rank, source_sequence, stable_id,
        source_kind, source_identity, occurrence, event_type
    ) AS (
        SELECT occurred_at, 0, CAST(sequence AS TEXT), 'audit:' || sequence,
               'audit', CAST(sequence AS TEXT), occurrence, event_type
          FROM audit_events WHERE obligation_id = ?1
        UNION ALL
        SELECT decided_at, 1, CAST(id AS TEXT), 'approval:' || id,
               'approval', CAST(id AS TEXT), occurrence, decision
          FROM approvals WHERE obligation_id = ?1
        UNION ALL
        SELECT claimed_at, 2, CAST(id AS TEXT), 'attempt:' || id,
               'attempt', CAST(id AS TEXT), occurrence, 'attempt_' || outcome
          FROM attempts WHERE obligation_id = ?1
        UNION ALL
        SELECT started_at, 3, id, 'gardener-inspection:' || id,
               'inspection', id, occurrence, 'gardener_inspection'
          FROM gardener_inspections WHERE id IN (SELECT id FROM related_inspections)
        UNION ALL
        SELECT p.created_at, 4, p.fingerprint, 'gardener-proposal:' || p.fingerprint,
               'proposal', p.fingerprint, o.occurrence, 'gardener_proposal'
          FROM gardener_proposals p
          JOIN obligations o ON o.id = p.implementation_obligation_id
         WHERE p.fingerprint IN (SELECT fingerprint FROM related_fingerprints)
        UNION ALL
        SELECT pi.created_at, 5, CAST(pi.generation AS TEXT) || ':' || pi.id,
               'gardener-proposal-instance:' || pi.id,
               'instance', pi.id, o.occurrence, 'gardener_proposal_instance'
          FROM gardener_proposal_instances pi
          JOIN obligations o ON o.id = pi.implementation_obligation_id
         WHERE pi.proposal_fingerprint IN (SELECT fingerprint FROM related_fingerprints)
        UNION ALL
        SELECT po.observed_at, 6, CAST(po.id AS TEXT), 'gardener-observation:' || po.id,
               'observation', CAST(po.id AS TEXT), NULL, 'gardener_proposal_observed'
          FROM gardener_proposal_observations po
         WHERE po.proposal_fingerprint IN (SELECT fingerprint FROM related_fingerprints)
        UNION ALL
        SELECT g.occurred_at, 7, CAST(g.sequence AS TEXT), 'gardener-event:' || g.sequence,
               'gardener_event', CAST(g.sequence AS TEXT), NULL, g.event_type
          FROM gardener_events g
         WHERE g.inspection_id IN (SELECT id FROM related_inspections)
            OR g.proposal_fingerprint IN (SELECT fingerprint FROM related_fingerprints)
        UNION ALL
        SELECT r.created_at, 8, r.id, 'gardener-run:' || r.id,
               'run', r.id, r.occurrence, 'gardener_run_' || r.phase
          FROM gardener_implementation_runs r WHERE r.obligation_id = ?1
        UNION ALL
        SELECT re.occurred_at, 9, CAST(re.sequence AS TEXT),
               'gardener-run-event:' || re.sequence,
               'run_event', CAST(re.sequence AS TEXT), NULL, re.event_type
          FROM gardener_run_events re
          JOIN gardener_implementation_runs r ON r.id = re.run_id
         WHERE r.obligation_id = ?1
    )";

fn approval_evidence_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApprovalEvidence> {
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
}

impl Store {
    pub fn operator_obligation(
        &self,
        obligation_id: &str,
        captured_at: i64,
    ) -> Result<OperatorObligation, StoreError> {
        self.operator_obligation_with_watermark(obligation_id, captured_at)
            .map(|(_, obligation)| obligation)
    }

    pub(crate) fn operator_obligation_with_watermark(
        &self,
        obligation_id: &str,
        captured_at: i64,
    ) -> Result<(i64, OperatorObligation), StoreError> {
        self.with_deferred_read(|store| {
            let watermark = crate::pagination::watermark(&store.connection)?;
            let (obligation, state_revision) = store
                .connection
                .query_row(
                    "SELECT o.*,
                        (SELECT sequence FROM audit_events a
                         WHERE a.obligation_id = o.id ORDER BY sequence DESC LIMIT 1)
                         AS state_revision
                 FROM obligations o WHERE o.id = ?1",
                    [obligation_id],
                    |row| {
                        Ok((
                            obligation_from_row(row)?,
                            row.get::<_, i64>("state_revision")?,
                        ))
                    },
                )
                .optional()?
                .ok_or_else(|| StoreError::NotFound(obligation_id.to_owned()))?;
            let proposal = proposal_instance_for_obligation(&store.connection, obligation_id)?;
            let projection = store.project_operator_obligation(
                &obligation,
                proposal.as_ref(),
                state_revision,
                captured_at,
            )?;
            Ok((watermark, projection))
        })
    }

    pub fn operator_snapshot(&self, captured_at: i64) -> Result<OperatorSnapshot, StoreError> {
        self.operator_snapshot_page(captured_at, None, None, None)
    }

    pub fn operator_snapshot_page(
        &self,
        captured_at: i64,
        cursor: Option<&str>,
        requested_watermark: Option<i64>,
        requested_limit: Option<usize>,
    ) -> Result<OperatorSnapshot, StoreError> {
        self.with_deferred_read(|store| {
            store.operator_snapshot_page_inner(
                captured_at,
                cursor,
                requested_watermark,
                requested_limit,
            )
        })
    }

    fn operator_snapshot_page_inner(
        &self,
        captured_at: i64,
        cursor: Option<&str>,
        requested_watermark: Option<i64>,
        requested_limit: Option<usize>,
    ) -> Result<OperatorSnapshot, StoreError> {
        let scope = "operator_snapshot";
        let limit = crate::page_limit(requested_limit)?;
        let current = crate::pagination::watermark(&self.connection)?;
        let (watermark, after, upper, projection_at) = if let Some(cursor) = cursor {
            let state = crate::pagination::decode_cursor_exact::<SnapshotCursorKey>(
                cursor,
                scope,
                requested_watermark,
                current,
            )?;
            if state.after.captured_at != state.upper.captured_at {
                return Err(StoreError::Invalid(
                    "invalid page cursor: snapshot time is inconsistent".to_owned(),
                ));
            }
            let projection_at = state.after.captured_at;
            (state.watermark, state.after, state.upper, projection_at)
        } else {
            (
                crate::pagination::initial_watermark(current, requested_watermark)?,
                SnapshotCursorKey {
                    captured_at,
                    exception_rank: i64::MIN,
                    state_rank: i64::MIN,
                    wake_rank: i64::MIN,
                    updated_at: i64::MIN,
                    id: String::new(),
                },
                self.connection
                    .query_row(
                        "SELECT
                            CASE WHEN state IN ('awaiting_approval', 'attention')
                                      OR (state = 'running' AND lease_expires_at <= ?1)
                                 THEN 0 ELSE 1 END AS exception_rank,
                            CASE state
                              WHEN 'awaiting_approval' THEN 0 WHEN 'attention' THEN 1
                              WHEN 'running' THEN 2 WHEN 'retry_scheduled' THEN 3
                              WHEN 'pending' THEN 4 WHEN 'completed' THEN 5 ELSE 6 END AS state_rank,
                            coalesce(next_wake_at, 9223372036854775807), updated_at, id
                         FROM obligations
                         ORDER BY exception_rank DESC, state_rank DESC,
                                  coalesce(next_wake_at, 9223372036854775807) DESC,
                                  updated_at DESC, id DESC LIMIT 1",
                        [captured_at],
                        |row| {
                            Ok(SnapshotCursorKey {
                                captured_at,
                                exception_rank: row.get(0)?,
                                state_rank: row.get(1)?,
                                wake_rank: row.get(2)?,
                                updated_at: row.get(3)?,
                                id: row.get(4)?,
                            })
                        },
                    )
                    .optional()?
                    .unwrap_or(SnapshotCursorKey {
                        captured_at,
                        exception_rank: i64::MIN,
                        state_rank: i64::MIN,
                        wake_rank: i64::MIN,
                        updated_at: i64::MIN,
                        id: String::new(),
                    }),
                captured_at,
            )
        };
        let mut statement = self.connection.prepare(
            "SELECT o.*,
                    (SELECT sequence FROM audit_events a
                     WHERE a.obligation_id = o.id ORDER BY sequence DESC LIMIT 1)
                     AS state_revision,
                    CASE WHEN o.state IN ('awaiting_approval', 'attention')
                              OR (o.state = 'running' AND o.lease_expires_at <= ?1)
                         THEN 0 ELSE 1 END AS exception_rank,
                    CASE o.state
                      WHEN 'awaiting_approval' THEN 0 WHEN 'attention' THEN 1
                      WHEN 'running' THEN 2 WHEN 'retry_scheduled' THEN 3
                      WHEN 'pending' THEN 4 WHEN 'completed' THEN 5 ELSE 6 END AS state_rank,
                    coalesce(o.next_wake_at, 9223372036854775807) AS wake_rank
             FROM obligations o
             WHERE (exception_rank, state_rank, wake_rank, o.updated_at, o.id)
                     > (?2, ?3, ?4, ?5, ?6)
               AND (exception_rank, state_rank, wake_rank, o.updated_at, o.id)
                     <= (?7, ?8, ?9, ?10, ?11)
             ORDER BY exception_rank, state_rank, wake_rank, o.updated_at, o.id LIMIT ?12",
        )?;
        let rows = statement.query_map(
            params![
                projection_at,
                after.exception_rank,
                after.state_rank,
                after.wake_rank,
                after.updated_at,
                after.id,
                upper.exception_rank,
                upper.state_rank,
                upper.wake_rank,
                upper.updated_at,
                upper.id,
                i64::try_from(limit + 1).expect("bounded limit")
            ],
            |row| {
                Ok((
                    obligation_from_row(row)?,
                    row.get::<_, i64>("state_revision")?,
                ))
            },
        )?;
        let mut raw = rows.collect::<Result<Vec<_>, _>>()?;
        let has_more = raw.len() > limit;
        raw.truncate(limit);
        let next_cursor = if has_more {
            Some(crate::pagination::encode_cursor(
                scope,
                watermark,
                snapshot_cursor_key(&raw.last().expect("non-empty page").0, projection_at),
                upper,
            )?)
        } else {
            None
        };
        let obligations = raw
            .into_iter()
            .map(|(obligation, state_revision)| {
                let proposal = proposal_instance_for_obligation(&self.connection, &obligation.id)?;
                self.project_operator_obligation(
                    &obligation,
                    proposal.as_ref(),
                    state_revision,
                    projection_at,
                )
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(OperatorSnapshot {
            captured_at: projection_at,
            service: None,
            next_cursor,
            watermark,
            obligations,
        })
    }

    pub fn operator_topic(
        &self,
        obligation_id: &str,
        captured_at: i64,
    ) -> Result<ObligationTopic, StoreError> {
        self.operator_topic_page(obligation_id, captured_at, None, None, None)
    }

    /// Bounded selected-topic projection preserving the observable API-v1
    /// order: occurrence time, source category, source-local sequence and
    /// stable source identity. The exact global watermark protects the
    /// multi-table projection from mixing revisions between pages; it is not
    /// substituted for the source-local sequence exposed on each item.
    pub fn operator_topic_page(
        &self,
        obligation_id: &str,
        captured_at: i64,
        cursor: Option<&str>,
        requested_watermark: Option<i64>,
        requested_limit: Option<usize>,
    ) -> Result<ObligationTopic, StoreError> {
        self.with_deferred_read(|store| {
            store.operator_topic_page_inner(
                obligation_id,
                captured_at,
                cursor,
                requested_watermark,
                requested_limit,
            )
        })
    }

    fn operator_topic_page_inner(
        &self,
        obligation_id: &str,
        captured_at: i64,
        cursor: Option<&str>,
        requested_watermark: Option<i64>,
        requested_limit: Option<usize>,
    ) -> Result<ObligationTopic, StoreError> {
        self.get(obligation_id)?
            .ok_or_else(|| StoreError::NotFound(obligation_id.to_owned()))?;
        let scope = format!("operator_topic:{obligation_id}");
        let limit = crate::page_limit(requested_limit)?;
        let current = crate::pagination::watermark(&self.connection)?;
        let (watermark, after, upper, projection_at) = if let Some(cursor) = cursor {
            let state = crate::pagination::decode_cursor_exact::<TopicCursorKey>(
                cursor,
                &scope,
                requested_watermark,
                current,
            )?;
            if state.after.captured_at != state.upper.captured_at {
                return Err(StoreError::Invalid(
                    "invalid page cursor: topic time is inconsistent".to_owned(),
                ));
            }
            let projection_at = state.after.captured_at;
            (state.watermark, state.after, state.upper, projection_at)
        } else {
            let upper = self
                .topic_candidate_upper(obligation_id, captured_at)?
                .unwrap_or_else(|| topic_cursor_min(captured_at));
            (
                crate::pagination::initial_watermark(current, requested_watermark)?,
                topic_cursor_min(captured_at),
                upper,
                captured_at,
            )
        };
        let page_query = format!(
            "{TOPIC_CANDIDATES_SQL}
             SELECT occurred_at, source_rank, source_sequence, stable_id,
                    source_kind, source_identity, occurrence, event_type
               FROM topic_items
              WHERE (occurred_at, source_rank, source_sequence, stable_id)
                    > (?2, ?3, ?4, ?5)
                AND (occurred_at, source_rank, source_sequence, stable_id)
                    <= (?6, ?7, ?8, ?9)
              ORDER BY occurred_at, source_rank, source_sequence, stable_id
              LIMIT ?10"
        );
        let mut statement = self.connection.prepare(&page_query)?;
        let rows = statement.query_map(
            params![
                obligation_id,
                after.occurred_at,
                after.source_rank,
                after.source_sequence,
                after.stable_id,
                upper.occurred_at,
                upper.source_rank,
                upper.source_sequence,
                upper.stable_id,
                i64::try_from(limit + 1).expect("bounded limit")
            ],
            |row| topic_candidate_from_row(row, projection_at),
        )?;
        let mut candidates = rows.collect::<Result<Vec<_>, _>>()?;
        let next_cursor = if candidates.len() > limit {
            candidates.truncate(limit);
            Some(crate::pagination::encode_cursor(
                &scope,
                watermark,
                candidates.last().expect("non-empty page").key.clone(),
                upper,
            )?)
        } else {
            None
        };
        drop(statement);
        let items = candidates
            .into_iter()
            .map(|candidate| self.topic_item_from_candidate(candidate))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ObligationTopic {
            captured_at: projection_at,
            obligation_id: obligation_id.to_owned(),
            service: None,
            next_cursor,
            watermark,
            items,
        })
    }

    fn topic_candidate_upper(
        &self,
        obligation_id: &str,
        captured_at: i64,
    ) -> Result<Option<TopicCursorKey>, StoreError> {
        let query = format!(
            "{TOPIC_CANDIDATES_SQL}
             SELECT occurred_at, source_rank, source_sequence, stable_id
               FROM topic_items
              ORDER BY occurred_at DESC, source_rank DESC,
                       source_sequence DESC, stable_id DESC
              LIMIT 1"
        );
        self.connection
            .query_row(&query, [obligation_id], |row| {
                Ok(TopicCursorKey {
                    captured_at,
                    occurred_at: row.get(0)?,
                    source_rank: row.get(1)?,
                    source_sequence: row.get(2)?,
                    stable_id: row.get(3)?,
                })
            })
            .optional()
            .map_err(StoreError::from)
    }

    fn topic_item_from_candidate(
        &self,
        candidate: TopicCandidate,
    ) -> Result<TopicItem, StoreError> {
        let evidence = match candidate.source {
            TopicSource::AuditEvent => self.topic_audit_evidence(&candidate.source_identity)?,
            TopicSource::ApprovalDecision => {
                self.topic_approval_evidence(&candidate.source_identity)?
            }
            TopicSource::Attempt => self.topic_attempt_evidence(&candidate.source_identity)?,
            TopicSource::GardenerInspection => self
                .gardener_inspection(&candidate.source_identity)?
                .map(serde_json::to_value)
                .transpose()
                .map_err(topic_serialisation_error)?
                .ok_or_else(|| missing_topic_source("inspection", &candidate.source_identity))?,
            TopicSource::GardenerProposal => self
                .gardener_proposal(&candidate.source_identity)?
                .map(serde_json::to_value)
                .transpose()
                .map_err(topic_serialisation_error)?
                .ok_or_else(|| missing_topic_source("proposal", &candidate.source_identity))?,
            TopicSource::GardenerProposalInstance => self
                .gardener_proposal_instance(&candidate.source_identity)?
                .map(serde_json::to_value)
                .transpose()
                .map_err(topic_serialisation_error)?
                .ok_or_else(|| {
                    missing_topic_source("proposal instance", &candidate.source_identity)
                })?,
            TopicSource::GardenerObservation => {
                self.topic_observation_evidence(&candidate.source_identity)?
            }
            TopicSource::GardenerEvent => {
                self.topic_gardener_event_evidence(&candidate.source_identity)?
            }
            TopicSource::GardenerImplementationRun => self
                .gardener_implementation_run(&candidate.source_identity)?
                .map(serde_json::to_value)
                .transpose()
                .map_err(topic_serialisation_error)?
                .ok_or_else(|| missing_topic_source("run", &candidate.source_identity))?,
            TopicSource::GardenerRunEvent => {
                self.topic_run_event_evidence(&candidate.source_identity)?
            }
        };
        Ok(TopicItem {
            occurred_at: candidate.key.occurred_at,
            source: candidate.source,
            source_sequence: candidate.key.source_sequence,
            stable_id: candidate.key.stable_id,
            occurrence: candidate.occurrence,
            event_type: candidate.event_type,
            evidence,
        })
    }

    fn topic_audit_evidence(&self, identity: &str) -> Result<serde_json::Value, StoreError> {
        let sequence = topic_numeric_identity("audit event", identity)?;
        let event: AuditEvent = self
            .connection
            .query_row(
                "SELECT sequence, obligation_id, occurrence, event_type, occurred_at,
                        from_state, to_state, details_json
                   FROM audit_events WHERE sequence = ?1",
                [sequence],
                audit_from_row,
            )
            .optional()?
            .ok_or_else(|| missing_topic_source("audit event", identity))?;
        serde_json::to_value(event).map_err(topic_serialisation_error)
    }

    fn topic_approval_evidence(&self, identity: &str) -> Result<serde_json::Value, StoreError> {
        let id = topic_numeric_identity("approval", identity)?;
        let approval = self
            .connection
            .query_row(
                "SELECT id, obligation_id, occurrence, decision, actor, note, decided_at
                   FROM approvals WHERE id = ?1",
                [id],
                approval_evidence_from_row,
            )
            .optional()?
            .ok_or_else(|| missing_topic_source("approval", identity))?;
        serde_json::to_value(approval).map_err(topic_serialisation_error)
    }

    fn topic_attempt_evidence(&self, identity: &str) -> Result<serde_json::Value, StoreError> {
        let id = topic_numeric_identity("attempt", identity)?;
        let attempt = self
            .connection
            .query_row(
                "SELECT id, obligation_id, occurrence, attempt_number, lease_generation,
                        lease_token, claimed_at, completed_at, outcome, retryable,
                        failure_disposition, error, evidence
                   FROM attempts WHERE id = ?1",
                [id],
                attempt_from_row,
            )
            .optional()?
            .ok_or_else(|| missing_topic_source("attempt", identity))?;
        serde_json::to_value(attempt).map_err(topic_serialisation_error)
    }

    fn topic_observation_evidence(&self, identity: &str) -> Result<serde_json::Value, StoreError> {
        let id = topic_numeric_identity("proposal observation", identity)?;
        let observation: ProposalObservation = self
            .connection
            .query_row(
                "SELECT id, proposal_fingerprint, inspection_id, source_commit, observed_at
                   FROM gardener_proposal_observations WHERE id = ?1",
                [id],
                |row| {
                    Ok(ProposalObservation {
                        id: row.get(0)?,
                        proposal_fingerprint: row.get(1)?,
                        inspection_id: row.get(2)?,
                        source_commit: row.get(3)?,
                        observed_at: row.get(4)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| missing_topic_source("proposal observation", identity))?;
        serde_json::to_value(observation).map_err(topic_serialisation_error)
    }

    fn topic_gardener_event_evidence(
        &self,
        identity: &str,
    ) -> Result<serde_json::Value, StoreError> {
        let sequence = topic_numeric_identity("gardener event", identity)?;
        let event: GardenerEvent = self
            .connection
            .query_row(
                "SELECT sequence, repository, inspection_id, proposal_fingerprint,
                        event_type, occurred_at, details_json
                   FROM gardener_events WHERE sequence = ?1",
                [sequence],
                |row| {
                    Ok(GardenerEvent {
                        sequence: row.get(0)?,
                        repository: row.get(1)?,
                        inspection_id: row.get(2)?,
                        proposal_fingerprint: row.get(3)?,
                        event_type: row.get(4)?,
                        occurred_at: row.get(5)?,
                        details_json: row.get(6)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| missing_topic_source("gardener event", identity))?;
        serde_json::to_value(event).map_err(topic_serialisation_error)
    }

    fn topic_run_event_evidence(&self, identity: &str) -> Result<serde_json::Value, StoreError> {
        let sequence = topic_numeric_identity("gardener run event", identity)?;
        let event: GardenerRunEvent = self
            .connection
            .query_row(
                "SELECT sequence, run_id, event_type, occurred_at, details_json
                   FROM gardener_run_events WHERE sequence = ?1",
                [sequence],
                |row| {
                    Ok(GardenerRunEvent {
                        sequence: row.get(0)?,
                        run_id: row.get(1)?,
                        event_type: row.get(2)?,
                        occurred_at: row.get(3)?,
                        details_json: row.get(4)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| missing_topic_source("gardener run event", identity))?;
        serde_json::to_value(event).map_err(topic_serialisation_error)
    }

    #[cfg(test)]
    fn operator_topic_legacy_for_test(
        &self,
        obligation_id: &str,
        captured_at: i64,
    ) -> Result<ObligationTopic, StoreError> {
        let obligation = self
            .get(obligation_id)?
            .ok_or_else(|| StoreError::NotFound(obligation_id.to_owned()))?;
        let all_proposals = self.gardener_proposals()?;
        let mut all_instances = Vec::new();
        for proposal in &all_proposals {
            all_instances.extend(self.gardener_proposal_instances(&proposal.fingerprint)?);
        }
        let mut all_observations = Vec::new();
        for proposal in &all_proposals {
            all_observations.extend(self.proposal_observations(&proposal.fingerprint)?);
        }
        let direct_instances = all_instances
            .iter()
            .filter(|item| item.implementation_obligation_id == obligation_id)
            .collect::<Vec<_>>();
        let direct_fingerprints = direct_instances
            .iter()
            .map(|item| item.proposal_fingerprint.clone())
            .collect::<BTreeSet<_>>();
        let mut inspection_ids = self
            .gardener_inspections()?
            .into_iter()
            .filter(|item| item.obligation_id == obligation_id)
            .map(|item| item.id)
            .collect::<BTreeSet<_>>();
        inspection_ids.extend(
            direct_instances
                .iter()
                .map(|item| item.source_inspection_id.clone()),
        );
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
        let proposal_instances = all_instances
            .into_iter()
            .filter(|item| proposal_fingerprints.contains(&item.proposal_fingerprint))
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
        for instance in proposal_instances {
            items.push(topic_item(
                instance.created_at,
                TopicSource::GardenerProposalInstance,
                format!("{}:{}", instance.generation, instance.id),
                format!("gardener-proposal-instance:{}", instance.id),
                self.get(&instance.implementation_obligation_id)?
                    .map(|item| item.occurrence)
                    .or(Some(obligation.occurrence)),
                "gardener_proposal_instance".to_owned(),
                &instance,
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
            service: None,
            next_cursor: None,
            watermark: crate::pagination::watermark(&self.connection)?,
            items,
        })
    }

    fn project_operator_obligation(
        &self,
        obligation: &Obligation,
        proposal: Option<&ProposalInstance>,
        state_revision: i64,
        captured_at: i64,
    ) -> Result<OperatorObligation, StoreError> {
        let exception = self.exception_reason(obligation, proposal, captured_at)?;
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
            ObligationState::Running => {
                let token = obligation.lease_token.clone().ok_or_else(|| {
                    StoreError::Invalid(format!(
                        "running obligation {:?} lacks a lease",
                        obligation.id
                    ))
                })?;
                let expires_at = obligation.lease_expires_at.ok_or_else(|| {
                    StoreError::Invalid(format!(
                        "running obligation {:?} lacks lease expiry",
                        obligation.id
                    ))
                })?;
                if expires_at <= captured_at {
                    Some(DurableLiveness::HumanAttention {
                        reason: exception.clone().ok_or_else(|| {
                            StoreError::Invalid(format!(
                                "expired running obligation {:?} lacks an exception reason",
                                obligation.id
                            ))
                        })?,
                    })
                } else {
                    Some(DurableLiveness::ActiveLease {
                        token,
                        generation: obligation.lease_generation,
                        expires_at,
                    })
                }
            }
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
            failure_disposition: obligation.failure_disposition.map(Into::into),
            created_at: obligation.created_at,
            updated_at: obligation.updated_at,
            exception,
            liveness,
            capabilities: capabilities(obligation, proposal, state_revision),
        })
    }

    fn exception_reason(
        &self,
        obligation: &Obligation,
        proposal: Option<&ProposalInstance>,
        captured_at: i64,
    ) -> Result<Option<ExceptionReason>, StoreError> {
        if obligation.state == ObligationState::Running {
            let expires_at = obligation.lease_expires_at.ok_or_else(|| {
                StoreError::Invalid(format!(
                    "running obligation {:?} lacks lease expiry",
                    obligation.id
                ))
            })?;
            if expires_at <= captured_at {
                return Ok(Some(ExceptionReason::ExpiredLease {
                    token: obligation.lease_token.clone().ok_or_else(|| {
                        StoreError::Invalid(format!(
                            "running obligation {:?} lacks a lease",
                            obligation.id
                        ))
                    })?,
                    generation: obligation.lease_generation,
                    expires_at,
                }));
            }
        }
        if obligation.state == ObligationState::AwaitingApproval {
            let subject = proposal.map_or(ApprovalSubject::Generic, |proposal| {
                ApprovalSubject::GardenerProposal {
                    repository: proposal.repository.clone(),
                    fingerprint: proposal.proposal_fingerprint.clone(),
                    instance_id: proposal.id.clone(),
                    generation: proposal.generation,
                    source_commit: proposal.source_commit.clone(),
                    source_observation_id: proposal.source_observation_id,
                    source_inspection_id: proposal.source_inspection_id.clone(),
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
        let latest_approval = self.latest_approval_evidence(&obligation.id)?;
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
                    self.latest_audit_event(&obligation.id)?.as_ref(),
                    self.latest_attempt(&obligation.id)?.as_ref(),
                ),
            }
        } else {
            failure_cause(
                obligation,
                self.latest_audit_event(&obligation.id)?.as_ref(),
                self.latest_attempt(&obligation.id)?.as_ref(),
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
        crate::store::latest_gardener_implementation_run(&self.connection, obligation_id)
    }

    fn latest_approval_evidence(
        &self,
        obligation_id: &str,
    ) -> Result<Option<ApprovalEvidence>, StoreError> {
        self.connection
            .query_row(
                "SELECT id, obligation_id, occurrence, decision, actor, note, decided_at
                 FROM approvals WHERE obligation_id = ?1 ORDER BY id DESC LIMIT 1",
                [obligation_id],
                approval_evidence_from_row,
            )
            .optional()
            .map_err(StoreError::from)
    }

    #[cfg(test)]
    fn approval_evidence(&self, obligation_id: &str) -> Result<Vec<ApprovalEvidence>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id, obligation_id, occurrence, decision, actor, note, decided_at
             FROM approvals WHERE obligation_id = ?1 ORDER BY id LIMIT 100",
        )?;
        let rows = statement.query_map(params![obligation_id], approval_evidence_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }
}

impl From<crate::FailureDisposition> for OperatorFailureDisposition {
    fn from(value: crate::FailureDisposition) -> Self {
        match value {
            crate::FailureDisposition::RetrySafe => Self::RetrySafe,
            crate::FailureDisposition::NeedsReconciliation => Self::NeedsReconciliation,
            crate::FailureDisposition::HumanDecision => Self::HumanDecision,
            crate::FailureDisposition::Terminal => Self::Terminal,
            crate::FailureDisposition::Cancelled => Self::Cancelled,
        }
    }
}

fn failure_cause(
    obligation: &Obligation,
    latest_event: Option<&crate::AuditEvent>,
    latest_attempt: Option<&crate::Attempt>,
) -> AttentionCause {
    let event_type = latest_event
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

fn capabilities(
    obligation: &Obligation,
    proposal: Option<&ProposalInstance>,
    state_revision: i64,
) -> OperatorCapabilities {
    let is_proposal = proposal.is_some();
    let is_current_proposal = proposal.is_some_and(|proposal| proposal.superseded_by.is_none());
    let approval_legal = approval_transition_is_legal(obligation);
    let generic_approval_legal = generic_approval_transition_is_legal(obligation, is_proposal);
    let proposal_approval_legal =
        gardener_proposal_transition_is_legal(obligation, is_current_proposal);
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
    let retry_reason = (!retry_transition_is_legal(obligation)
        || (is_proposal && !is_current_proposal))
        .then(|| state_disabled_reason(obligation));
    let cancel_reason =
        (!cancel_transition_is_legal(obligation)).then(|| state_disabled_reason(obligation));
    let ordinary_precondition = ActionPrecondition {
        obligation_id: obligation.id.clone(),
        occurrence: obligation.occurrence,
        state_revision,
        gardener_fingerprint: None,
        gardener_proposal_instance_id: None,
        gardener_source_commit: None,
        gardener_source_observation_id: None,
        gardener_source_inspection_id: None,
        gardener_generation: None,
    };
    let gardener_precondition = proposal.map(|proposal| ActionPrecondition {
        gardener_fingerprint: Some(proposal.proposal_fingerprint.clone()),
        gardener_proposal_instance_id: Some(proposal.id.clone()),
        gardener_source_commit: Some(proposal.source_commit.clone()),
        gardener_source_observation_id: Some(proposal.source_observation_id),
        gardener_source_inspection_id: Some(proposal.source_inspection_id.clone()),
        gardener_generation: Some(proposal.generation),
        ..ordinary_precondition.clone()
    });
    OperatorCapabilities {
        approve: capability(
            approve_reason,
            ActionConsequence::ScheduleCurrentOccurrence,
            Some(ordinary_precondition.clone()),
        ),
        reject: capability(
            approve_reason,
            ActionConsequence::MoveToAttention,
            Some(ordinary_precondition.clone()),
        ),
        retry: capability(
            retry_reason,
            ActionConsequence::ReopenForRetry,
            Some(ordinary_precondition.clone()),
        ),
        cancel: capability(
            cancel_reason,
            ActionConsequence::CancelObligation,
            Some(ordinary_precondition),
        ),
        approve_gardener_proposal: capability(
            proposal_reason,
            ActionConsequence::ScheduleExactGardenerProposal,
            gardener_precondition.clone(),
        ),
        reject_gardener_proposal: capability(
            proposal_reason,
            ActionConsequence::RejectExactGardenerProposal,
            gardener_precondition,
        ),
    }
}

fn capability(
    reason: Option<DisabledReason>,
    consequence: ActionConsequence,
    precondition: Option<ActionPrecondition>,
) -> ActionCapability {
    ActionCapability {
        available: reason.is_none(),
        disabled_reason: reason,
        consequence,
        precondition: reason.is_none().then_some(precondition).flatten(),
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

#[cfg(test)]
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

fn operator_obligation_sort_key(
    item: &Obligation,
    captured_at: i64,
) -> (i64, i64, i64, i64, String) {
    let exception = if matches!(
        item.state,
        ObligationState::AwaitingApproval | ObligationState::Attention
    ) || (item.state == ObligationState::Running
        && item
            .lease_expires_at
            .is_some_and(|expiry| expiry <= captured_at))
    {
        0
    } else {
        1
    };
    let state = match item.state {
        ObligationState::AwaitingApproval => 0,
        ObligationState::Attention => 1,
        ObligationState::Running => 2,
        ObligationState::RetryScheduled => 3,
        ObligationState::Pending => 4,
        ObligationState::Completed => 5,
        ObligationState::Cancelled => 6,
    };
    (
        exception,
        state,
        item.next_wake_at.unwrap_or(i64::MAX),
        item.updated_at,
        item.id.clone(),
    )
}

fn snapshot_cursor_key(item: &Obligation, captured_at: i64) -> SnapshotCursorKey {
    let key = operator_obligation_sort_key(item, captured_at);
    SnapshotCursorKey {
        captured_at,
        exception_rank: key.0,
        state_rank: key.1,
        wake_rank: key.2,
        updated_at: key.3,
        id: key.4,
    }
}

fn topic_cursor_min(captured_at: i64) -> TopicCursorKey {
    TopicCursorKey {
        captured_at,
        occurred_at: i64::MIN,
        source_rank: i64::MIN,
        source_sequence: String::new(),
        stable_id: String::new(),
    }
}

fn topic_candidate_from_row(
    row: &rusqlite::Row<'_>,
    captured_at: i64,
) -> rusqlite::Result<TopicCandidate> {
    let source_rank: i64 = row.get(1)?;
    let source = match source_rank {
        0 => TopicSource::AuditEvent,
        1 => TopicSource::ApprovalDecision,
        2 => TopicSource::Attempt,
        3 => TopicSource::GardenerInspection,
        4 => TopicSource::GardenerProposal,
        5 => TopicSource::GardenerProposalInstance,
        6 => TopicSource::GardenerObservation,
        7 => TopicSource::GardenerEvent,
        8 => TopicSource::GardenerImplementationRun,
        9 => TopicSource::GardenerRunEvent,
        value => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Integer,
                format!("unknown topic source rank {value}").into(),
            ));
        }
    };
    let source_kind: String = row.get(4)?;
    if source_kind != topic_source_kind(source) {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            format!("topic source rank {source_rank} conflicts with {source_kind:?}").into(),
        ));
    }
    Ok(TopicCandidate {
        key: TopicCursorKey {
            captured_at,
            occurred_at: row.get(0)?,
            source_rank,
            source_sequence: row.get(2)?,
            stable_id: row.get(3)?,
        },
        source,
        source_identity: row.get(5)?,
        occurrence: row.get(6)?,
        event_type: row.get(7)?,
    })
}

fn topic_source_kind(source: TopicSource) -> &'static str {
    match source {
        TopicSource::AuditEvent => "audit",
        TopicSource::ApprovalDecision => "approval",
        TopicSource::Attempt => "attempt",
        TopicSource::GardenerInspection => "inspection",
        TopicSource::GardenerProposal => "proposal",
        TopicSource::GardenerProposalInstance => "instance",
        TopicSource::GardenerObservation => "observation",
        TopicSource::GardenerEvent => "gardener_event",
        TopicSource::GardenerImplementationRun => "run",
        TopicSource::GardenerRunEvent => "run_event",
    }
}

fn topic_numeric_identity(kind: &str, identity: &str) -> Result<i64, StoreError> {
    identity.parse::<i64>().map_err(|error| {
        StoreError::Invalid(format!(
            "invalid {kind} topic identity {identity:?}: {error}"
        ))
    })
}

fn missing_topic_source(kind: &str, identity: &str) -> StoreError {
    StoreError::Invalid(format!(
        "{kind} topic source {identity:?} disappeared inside a stable read snapshot"
    ))
}

#[cfg(test)]
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

fn topic_serialisation_error(error: serde_json::Error) -> StoreError {
    StoreError::Invalid(format!("operator evidence failed: {error}"))
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
    use std::collections::BTreeMap;

    use super::*;
    use crate::{
        Completion, GardenerCandidateQualification, InspectionResult, NewGardenerImplementationRun,
        NewGardenerInspection, NewObligation, NewRepositoryRegistration, Recurrence, RetryPolicy,
    };
    use tempfile::TempDir;

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

    #[test]
    fn expired_running_lease_is_projected_as_attention_until_store_recovers_it() {
        let mut store = Store::open_in_memory().unwrap();
        store.create(new("lease", false), 100).unwrap();
        store.claim_due(100, 10, 1).unwrap();

        let before_expiry = store.operator_snapshot(109).unwrap();
        let before_expiry = before_expiry
            .obligations
            .into_iter()
            .find(|item| item.id == "lease")
            .unwrap();
        assert!(before_expiry.exception.is_none());
        assert!(matches!(
            before_expiry.liveness,
            Some(DurableLiveness::ActiveLease {
                expires_at: 110,
                ..
            })
        ));

        for captured_at in [110, 111] {
            let expired = store
                .operator_snapshot(captured_at)
                .unwrap()
                .obligations
                .into_iter()
                .find(|item| item.id == "lease")
                .unwrap();
            assert_eq!(expired.state, OperatorObligationState::Running);
            assert!(matches!(
                expired.exception,
                Some(ExceptionReason::ExpiredLease {
                    generation: 1,
                    expires_at: 110,
                    ..
                })
            ));
            assert!(matches!(
                expired.liveness,
                Some(DurableLiveness::HumanAttention {
                    reason: ExceptionReason::ExpiredLease {
                        expires_at: 110,
                        ..
                    }
                })
            ));
        }

        assert_eq!(store.recover_expired_leases(110).unwrap(), 1);
        let recovered = store
            .operator_snapshot(111)
            .unwrap()
            .obligations
            .into_iter()
            .find(|item| item.id == "lease")
            .unwrap();
        assert_eq!(recovered.state, OperatorObligationState::RetryScheduled);
        assert!(recovered.exception.is_none());
        assert!(matches!(
            recovered.liveness,
            Some(DurableLiveness::FutureWake { wake_at: 120 })
        ));
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
                r#"{"summary":"implemented","changed_paths":[],"checks":[]}"#,
                start + 3,
            )
            .unwrap();
        store
            .record_gardener_git_commit(claim, run_id, &head, start + 4)
            .unwrap();
        store
            .record_gardener_candidate_qualification(
                claim,
                &GardenerCandidateQualification {
                    run_id: run_id.to_owned(),
                    head: head.clone(),
                    diff_manifest_json: "[]".to_owned(),
                    tree_manifest_json: "[]".to_owned(),
                    checks_json: r#"[{"executable":{"role":"candidate_check"},"arguments":["test"],"duration_millis":1,"status":{"kind":"passed"},"evidence":{}}]"#.to_owned(),
                    duration_ms: 1,
                    qualified_at: start + 4,
                },
                start + 4,
            )
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
                    disposition: crate::FailureDisposition::RetrySafe,
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
        let approval_precondition = by_id["approval"]
            .capabilities
            .approve
            .precondition
            .as_ref()
            .unwrap();
        assert_eq!(approval_precondition.obligation_id, "approval");
        assert_eq!(approval_precondition.occurrence, 1);
        assert!(approval_precondition.state_revision > 0);
        assert!(approval_precondition.gardener_fingerprint.is_none());
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
        let exact_precondition = projected
            .capabilities
            .approve_gardener_proposal
            .precondition
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(
            exact_precondition.gardener_fingerprint.as_deref(),
            Some(proposal.fingerprint.as_str())
        );
        let instance = store
            .gardener_proposal_instances(&proposal.fingerprint)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(
            exact_precondition.gardener_proposal_instance_id.as_deref(),
            Some(instance.id.as_str())
        );
        assert_eq!(
            exact_precondition.gardener_source_commit.as_deref(),
            Some(instance.source_commit.as_str())
        );
        assert_eq!(
            exact_precondition.gardener_source_observation_id,
            Some(instance.source_observation_id)
        );
        assert_eq!(
            exact_precondition.gardener_source_inspection_id.as_deref(),
            Some(instance.source_inspection_id.as_str())
        );
        assert_eq!(
            exact_precondition.gardener_generation,
            Some(instance.generation)
        );
        assert!(matches!(
            projected.exception,
            Some(ExceptionReason::AwaitingApproval {
                subject: ApprovalSubject::GardenerProposal {
                    ref fingerprint,
                    ref instance_id,
                    generation,
                    ref source_commit,
                    source_observation_id,
                    ref source_inspection_id,
                    ref prompt,
                    occurrence: 1,
                    ..
                }
            }) if fingerprint == &proposal.fingerprint
                && instance_id == &instance.id
                && generation == instance.generation
                && source_commit == &instance.source_commit
                && source_observation_id == instance.source_observation_id
                && source_inspection_id == &instance.source_inspection_id
                && prompt == "Implement the exact safe change"
        ));

        let mut wrong_fingerprint = exact_precondition.clone();
        wrong_fingerprint.gardener_fingerprint = Some("different".to_owned());
        let error = store
            .decide_gardener_proposal_if_current(
                &proposal.fingerprint,
                ApprovalDecision::Approved,
                "operator",
                None,
                &wrong_fingerprint,
                101,
            )
            .unwrap_err();
        assert!(matches!(error, StoreError::Conflict(message) if message.contains("fingerprint")));
        let mut wrong_generation = exact_precondition;
        wrong_generation.gardener_generation = Some(instance.generation + 1);
        let error = store
            .decide_gardener_proposal_instance_if_current(
                &instance.id,
                ApprovalDecision::Approved,
                "operator",
                None,
                &wrong_generation,
                101,
            )
            .unwrap_err();
        assert!(matches!(error, StoreError::Conflict(message) if message.contains("source-bound")));

        let topic = store
            .operator_topic(&proposal.implementation_obligation_id, 101)
            .unwrap();
        let legacy_items = store
            .operator_topic_legacy_for_test(&proposal.implementation_obligation_id, 101)
            .unwrap()
            .items;
        assert_eq!(topic.items, legacy_items);
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
            item.source == TopicSource::GardenerProposalInstance
                && item.evidence["id"] == instance.id
                && item.evidence["generation"] == instance.generation
                && item.evidence["source_commit"] == instance.source_commit
                && item.evidence["source_observation_id"] == instance.source_observation_id
                && item.evidence["source_inspection_id"] == instance.source_inspection_id
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
            .finish_gardener_implementation(
                &claim,
                "run-1",
                r#"{"summary":"implemented","changed_paths":[],"checks":[]}"#,
                105,
            )
            .unwrap();
        let head = "c".repeat(40);
        store
            .record_gardener_git_commit(&claim, "run-1", &head, 106)
            .unwrap();
        store
            .record_gardener_candidate_qualification(
                &claim,
                &GardenerCandidateQualification {
                    run_id: "run-1".to_owned(),
                    head: head.clone(),
                    diff_manifest_json: "[]".to_owned(),
                    tree_manifest_json: "[]".to_owned(),
                    checks_json: r#"[{"executable":{"role":"candidate_check"},"arguments":["test"],"duration_millis":1,"status":{"kind":"passed"},"evidence":{}}]"#.to_owned(),
                    duration_ms: 1,
                    qualified_at: 106,
                },
                106,
            )
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
                    disposition: crate::FailureDisposition::Terminal,
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
        for source in [
            TopicSource::AuditEvent,
            TopicSource::ApprovalDecision,
            TopicSource::Attempt,
            TopicSource::GardenerInspection,
            TopicSource::GardenerProposal,
            TopicSource::GardenerProposalInstance,
            TopicSource::GardenerObservation,
            TopicSource::GardenerEvent,
            TopicSource::GardenerImplementationRun,
            TopicSource::GardenerRunEvent,
        ] {
            assert!(
                completed_topic
                    .items
                    .iter()
                    .any(|item| item.source == source),
                "missing production topic source {source:?}"
            );
        }
        for item in &completed_topic.items {
            let typed_source_sequence = match item.source {
                TopicSource::AuditEvent | TopicSource::GardenerEvent => item.evidence["sequence"]
                    .as_i64()
                    .map(|value| value.to_string()),
                TopicSource::ApprovalDecision
                | TopicSource::Attempt
                | TopicSource::GardenerObservation => {
                    item.evidence["id"].as_i64().map(|value| value.to_string())
                }
                TopicSource::GardenerRunEvent => item.evidence["sequence"]
                    .as_i64()
                    .map(|value| value.to_string()),
                TopicSource::GardenerInspection | TopicSource::GardenerImplementationRun => {
                    item.evidence["id"].as_str().map(str::to_owned)
                }
                TopicSource::GardenerProposal => {
                    item.evidence["fingerprint"].as_str().map(str::to_owned)
                }
                TopicSource::GardenerProposalInstance => Some(format!(
                    "{}:{}",
                    item.evidence["generation"].as_u64().unwrap(),
                    item.evidence["id"].as_str().unwrap()
                )),
            }
            .expect("complete typed source evidence");
            assert_eq!(item.source_sequence, typed_source_sequence);
        }
        let mut paged_items = Vec::new();
        let mut cursor = None;
        let mut watermark = None;
        loop {
            let page = store
                .operator_topic_page(
                    &proposal.implementation_obligation_id,
                    114,
                    cursor.as_deref(),
                    watermark,
                    Some(3),
                )
                .unwrap();
            watermark.get_or_insert(page.watermark);
            paged_items.extend(page.items);
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(paged_items, completed_topic.items);
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
                    disposition: crate::FailureDisposition::Terminal,
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

    #[test]
    fn later_occurrence_rejects_action_confirmed_for_an_earlier_occurrence() {
        let mut store = Store::open_in_memory().unwrap();
        let mut recurring = new("recurring-approval", true);
        recurring.recurrence = Some(Recurrence::new("* * * * *", "UTC").unwrap());
        store.create(recurring, 90).unwrap();
        let stale = store
            .operator_snapshot(90)
            .unwrap()
            .obligations
            .pop()
            .unwrap()
            .capabilities
            .approve
            .precondition
            .unwrap();

        store
            .decide_approval_if_current(
                "recurring-approval",
                ApprovalDecision::Approved,
                "operator",
                None,
                &stale,
                100,
            )
            .unwrap();
        let claim = store.claim_due(100, 60, 1).unwrap().pop().unwrap();
        store
            .complete(&claim, Completion::Succeeded { evidence: None }, 101)
            .unwrap();
        assert_eq!(
            store.get("recurring-approval").unwrap().unwrap().occurrence,
            2
        );

        let error = store
            .decide_approval_if_current(
                "recurring-approval",
                ApprovalDecision::Approved,
                "operator",
                None,
                &stale,
                102,
            )
            .unwrap_err();
        assert!(matches!(error, StoreError::Conflict(message) if message.contains("stale")));
        assert_eq!(
            store.get("recurring-approval").unwrap().unwrap().state,
            ObligationState::AwaitingApproval
        );
    }

    #[test]
    fn same_occurrence_state_cycle_rejects_the_original_confirmation() {
        let mut store = Store::open_in_memory().unwrap();
        store.create(new("cycled-approval", true), 90).unwrap();
        let stale = store
            .operator_snapshot(90)
            .unwrap()
            .obligations
            .pop()
            .unwrap()
            .capabilities
            .approve
            .precondition
            .unwrap();

        store
            .decide_approval(
                "cycled-approval",
                ApprovalDecision::Rejected,
                "operator",
                None,
                100,
            )
            .unwrap();
        store.retry_attention("cycled-approval", 101).unwrap();
        let current = store.get("cycled-approval").unwrap().unwrap();
        assert_eq!(current.occurrence, stale.occurrence);
        assert_eq!(current.state, ObligationState::AwaitingApproval);

        let error = store
            .decide_approval_if_current(
                "cycled-approval",
                ApprovalDecision::Approved,
                "operator",
                None,
                &stale,
                102,
            )
            .unwrap_err();
        assert!(matches!(error, StoreError::Conflict(message) if message.contains("revision")));
        assert_eq!(
            store.get("cycled-approval").unwrap().unwrap().state,
            ObligationState::AwaitingApproval
        );
    }

    #[test]
    fn deferred_operator_read_keeps_one_snapshot_across_owner_queries() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("operator-snapshot.sqlite");
        let reader = Store::open(&path).unwrap();
        let mut writer = Store::open_compatible(&path).unwrap();

        let (before, during) = reader
            .with_deferred_read(|owner| {
                let before = owner.list()?.len();
                writer.create(new("committed-during-read", false), 100)?;
                let during = owner.list()?.len();
                Ok((before, during))
            })
            .unwrap();
        assert_eq!((before, during), (0, 0));
        assert_eq!(reader.list().unwrap().len(), 1);
    }

    #[test]
    fn operator_snapshot_pages_keep_semantic_order_without_duplicates() {
        let mut store = Store::open_in_memory().unwrap();
        for index in 0..205 {
            store
                .create(
                    new(&format!("snapshot-{index:03}"), index % 7 == 0),
                    100 + index,
                )
                .unwrap();
        }
        let mut cursor = None;
        let mut watermark = None;
        let mut items = Vec::new();
        loop {
            let page = store
                .operator_snapshot_page(1_000, cursor.as_deref(), watermark, Some(31))
                .unwrap();
            watermark.get_or_insert(page.watermark);
            items.extend(page.obligations);
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(items.len(), 205);
        assert!(
            items
                .windows(2)
                .all(|pair| operator_sort_key(&pair[0]) <= operator_sort_key(&pair[1]))
        );
        assert_eq!(
            items
                .iter()
                .map(|item| &item.id)
                .collect::<BTreeSet<_>>()
                .len(),
            items.len()
        );
    }

    #[test]
    fn selected_topic_pages_over_large_scoped_history_without_global_materialisation() {
        let mut store = Store::open_in_memory().unwrap();
        store.create(new("selected", false), 100).unwrap();
        store.create(new("unrelated", false), 100).unwrap();
        for index in 0..5_001_i64 {
            for obligation_id in ["selected", "unrelated"] {
                store
                    .connection
                    .execute(
                        "INSERT INTO audit_events(
                        obligation_id, occurrence, event_type, occurred_at,
                        from_state, to_state, details_json
                     ) VALUES (?1, 1, 'fixture', ?2, 'pending', 'pending', '{}')",
                        params![obligation_id, 101 + index],
                    )
                    .unwrap();
            }
        }

        let mut cursor = None;
        let mut watermark = None;
        let mut revisions = Vec::new();
        loop {
            let page = store
                .operator_topic_page("selected", 10_000, cursor.as_deref(), watermark, Some(113))
                .unwrap();
            assert!(page.service.is_none());
            watermark.get_or_insert(page.watermark);
            revisions.extend(page.items.iter().map(|item| {
                item.stable_id
                    .strip_prefix("audit:")
                    .unwrap()
                    .parse::<i64>()
                    .unwrap()
            }));
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(revisions.len(), 5_002);
        assert!(revisions.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(
            revisions.iter().collect::<BTreeSet<_>>().len(),
            revisions.len()
        );
    }
}
