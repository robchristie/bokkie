//! Bounded, globally ordered reads over the authoritative append-only events.

use rusqlite::{OptionalExtension, Row, Transaction, types::Type};

use crate::{Store, StoreError};

pub const MAX_CHANGE_PAGE_SIZE: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventProvenance {
    /// Deterministic replay order for events that predate the global envelope.
    /// This order must not be interpreted as historical transaction causality.
    LegacyNonCausal,
    /// Authoritative cross-stream insertion order assigned in the source write.
    LiveAppend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventSource {
    AuditEvent { sequence: i64 },
    GardenerEvent { sequence: i64 },
    GardenerRunEvent { sequence: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EventEnvelope {
    pub sequence: i64,
    pub provenance: EventProvenance,
    pub source: EventSource,
}

/// One incremental projection invalidation with both its authoritative event
/// identity and the durable domain identities that event can affect.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChangeRecord {
    pub envelope: EventEnvelope,
    pub event_type: String,
    pub occurred_at: i64,
    pub details_json: String,
    pub obligation_id: Option<String>,
    pub occurrence: Option<u32>,
    pub repository: Option<String>,
    pub inspection_id: Option<String>,
    pub proposal_fingerprint: Option<String>,
    pub proposal_instance_id: Option<String>,
    pub run_id: Option<String>,
}

/// A bounded slice of one stable event-envelope snapshot.
///
/// Pass `next_after` back as `after` with the same `through` watermark while it
/// is present. When it is absent, polling can resume after `through` with no
/// missed or duplicated envelope.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub after: i64,
    pub through: i64,
    pub next_after: Option<i64>,
}

impl Store {
    /// Read a bounded global change page from one deferred SQLite snapshot.
    ///
    /// With no `through`, the method captures the exact current watermark in
    /// the same transaction as the page. Supplying that returned watermark on
    /// later pages pins the walk even while WAL writers continue appending.
    pub fn change_page(
        &self,
        after: i64,
        through: Option<i64>,
        limit: usize,
    ) -> Result<Page<ChangeRecord>, StoreError> {
        if !(1..=MAX_CHANGE_PAGE_SIZE).contains(&limit) {
            return Err(StoreError::Invalid(format!(
                "event page limit {limit} is outside 1..={MAX_CHANGE_PAGE_SIZE}"
            )));
        }
        validate_cursor(after)?;
        if let Some(through) = through {
            validate_cursor(through)?;
        }

        let transaction = self.connection.unchecked_transaction()?;
        let current = transaction.query_row(
            "SELECT coalesce(max(sequence), 0) FROM event_envelopes",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        let through = through.unwrap_or(current);
        if through > current {
            return Err(cursor_after_watermark(through, current));
        }
        require_retained_cursor(&transaction, through)?;
        if after > through {
            return Err(cursor_after_watermark(after, through));
        }
        require_retained_cursor(&transaction, after)?;

        let mut items = query_changes(&transaction, after, through, limit + 1)?;
        let next_after = if items.len() > limit {
            items.truncate(limit);
            items.last().map(|item| item.envelope.sequence)
        } else {
            None
        };
        transaction.commit()?;
        Ok(Page {
            items,
            after,
            through,
            next_after,
        })
    }
}

fn validate_cursor(cursor: i64) -> Result<(), StoreError> {
    if cursor < 0 {
        Err(StoreError::Invalid(format!(
            "event cursor {cursor} must not be negative"
        )))
    } else {
        Ok(())
    }
}

fn require_retained_cursor(transaction: &Transaction<'_>, cursor: i64) -> Result<(), StoreError> {
    if cursor == 0 {
        return Ok(());
    }
    let exists = transaction
        .query_row(
            "SELECT 1 FROM event_envelopes WHERE sequence = ?1",
            [cursor],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if exists {
        Ok(())
    } else {
        Err(StoreError::ProjectionGap(format!(
            "event envelope cursor {cursor}"
        )))
    }
}

fn cursor_after_watermark(after: i64, watermark: i64) -> StoreError {
    StoreError::ProjectionGap(format!(
        "event cursor {after} is after watermark {watermark}"
    ))
}

fn query_changes(
    transaction: &Transaction<'_>,
    after: i64,
    through: i64,
    limit: usize,
) -> Result<Vec<ChangeRecord>, StoreError> {
    let mut statement = transaction.prepare(
        "SELECT e.sequence, e.provenance, e.source_kind,
                coalesce(e.audit_event_sequence, e.gardener_event_sequence,
                         e.gardener_run_event_sequence) AS source_sequence,
                coalesce(a.event_type, g.event_type, re.event_type) AS event_type,
                coalesce(a.occurred_at, g.occurred_at, re.occurred_at) AS occurred_at,
                coalesce(a.details_json, g.details_json, re.details_json) AS details_json,
                coalesce(a.obligation_id, gpi.implementation_obligation_id,
                         gi.obligation_id, run.obligation_id) AS obligation_id,
                coalesce(a.occurrence, gi.occurrence, run.occurrence) AS occurrence,
                coalesce(g.repository, run.repository, ap.repository) AS repository,
                g.inspection_id,
                coalesce(g.proposal_fingerprint, run.proposal_fingerprint,
                         api.proposal_fingerprint) AS proposal_fingerprint,
                coalesce(gpi.id, ri.instance_id, api.id) AS proposal_instance_id,
                re.run_id
         FROM event_envelopes e
         LEFT JOIN audit_events a ON a.sequence = e.audit_event_sequence
         LEFT JOIN gardener_events g ON g.sequence = e.gardener_event_sequence
         LEFT JOIN gardener_run_events re ON re.sequence = e.gardener_run_event_sequence
         LEFT JOIN gardener_inspections gi ON gi.id = g.inspection_id
         LEFT JOIN gardener_proposal_observations gpo
           ON gpo.inspection_id = g.inspection_id
          AND gpo.proposal_fingerprint = g.proposal_fingerprint
         LEFT JOIN gardener_proposal_observation_instances gpoi
           ON gpoi.observation_id = gpo.id
         LEFT JOIN gardener_proposal_instances gpi ON gpi.id = gpoi.instance_id
         LEFT JOIN gardener_implementation_runs run ON run.id = re.run_id
         LEFT JOIN gardener_implementation_run_instances ri ON ri.run_id = run.id
         LEFT JOIN gardener_proposal_instances api
           ON api.implementation_obligation_id = a.obligation_id
         LEFT JOIN gardener_proposals ap
           ON ap.fingerprint = api.proposal_fingerprint
         WHERE e.sequence > ?1 AND e.sequence <= ?2
         ORDER BY e.sequence
         LIMIT ?3",
    )?;
    let rows = statement.query_map(
        rusqlite::params![
            after,
            through,
            i64::try_from(limit).expect("bounded page limit")
        ],
        change_record_from_row,
    )?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn change_record_from_row(row: &Row<'_>) -> rusqlite::Result<ChangeRecord> {
    let provenance = match row.get::<_, String>(1)?.as_str() {
        "legacy_non_causal" => EventProvenance::LegacyNonCausal,
        "live_append" => EventProvenance::LiveAppend,
        other => return Err(invalid_event_value(1, "event provenance", other)),
    };
    let source_sequence = row.get(3)?;
    let source = match row.get::<_, String>(2)?.as_str() {
        "audit_event" => EventSource::AuditEvent {
            sequence: source_sequence,
        },
        "gardener_event" => EventSource::GardenerEvent {
            sequence: source_sequence,
        },
        "gardener_run_event" => EventSource::GardenerRunEvent {
            sequence: source_sequence,
        },
        other => return Err(invalid_event_value(2, "event source kind", other)),
    };
    Ok(ChangeRecord {
        envelope: EventEnvelope {
            sequence: row.get(0)?,
            provenance,
            source,
        },
        event_type: row.get(4)?,
        occurred_at: row.get(5)?,
        details_json: row.get(6)?,
        obligation_id: row.get(7)?,
        occurrence: row.get(8)?,
        repository: row.get(9)?,
        inspection_id: row.get(10)?,
        proposal_fingerprint: row.get(11)?,
        proposal_instance_id: row.get(12)?,
        run_id: row.get(13)?,
    })
}

fn invalid_event_value(column: usize, field: &str, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        Type::Text,
        format!("unknown {field} {value:?}").into(),
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use rusqlite::{Connection, params};
    use tempfile::TempDir;

    use super::*;
    use crate::migrations::MIGRATIONS;

    fn create_v8(path: &Path) -> Connection {
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
        for migration in &MIGRATIONS[..6] {
            connection.execute_batch(migration.sql).unwrap();
            connection
                .execute(
                    "INSERT INTO schema_migrations(version, name) VALUES (?1, ?2)",
                    params![migration.version, migration.name],
                )
                .unwrap();
        }
        for migration in &MIGRATIONS[6..8] {
            connection.execute_batch(migration.sql).unwrap();
            connection
                .execute(
                    "INSERT INTO schema_migrations(version, name, sha256)
                     VALUES (?1, ?2, ?3)",
                    params![migration.version, migration.name, migration.sha256],
                )
                .unwrap();
        }
        connection
    }

    fn insert_domain_fixture(connection: &Connection) {
        connection
            .execute_batch(
                "INSERT INTO obligations(
                    id, description, state, occurrence, scheduled_at, next_wake_at,
                    approval_required, attempts_made, max_attempts, retry_base_seconds,
                    retry_max_seconds, lease_generation, created_at, updated_at
                 ) VALUES
                    ('inspection-obligation', 'inspect', 'pending', 1, 10, 10,
                     0, 0, 3, 60, 3600, 0, 10, 10),
                    ('implementation-obligation', 'implement', 'awaiting_approval', 1, 10, NULL,
                     1, 0, 1, 60, 3600, 0, 10, 10);

                 INSERT INTO gardener_obligation_bindings(obligation_id, kind, created_at)
                 VALUES ('inspection-obligation', 'inspection', 10),
                        ('implementation-obligation', 'implementation', 10);

                 INSERT INTO gardener_repositories(
                    repository, default_branch, checkout_path, inspection_cron,
                    inspection_timezone, first_inspection_at,
                    inspection_obligation_id, created_at, updated_at
                 ) VALUES (
                    'robchristie/bokkie', 'main', '/srv/bokkie', '* * * * *',
                    'Australia/Adelaide', 10, 'inspection-obligation', 10, 10
                 );

                 INSERT INTO gardener_inspections(
                    id, repository, obligation_id, occurrence, lease_generation,
                    lease_token, source_commit, worktree_path, prompt_digest,
                    result_json, started_at, completed_at
                 ) VALUES (
                    'inspection-1', 'robchristie/bokkie', 'inspection-obligation', 1, 1,
                    'lease', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', '/tmp/inspection-1',
                    'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                    '{}', 10, 11
                 );

                 INSERT INTO gardener_proposals(
                    fingerprint, repository, prompt, implementation_obligation_id, created_at
                 ) VALUES (
                    'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff',
                    'robchristie/bokkie', 'Improve one thing',
                    'implementation-obligation', 11
                 );

                 INSERT INTO gardener_proposal_observations(
                    proposal_fingerprint, inspection_id, source_commit, observed_at
                 ) VALUES (
                    'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff',
                    'inspection-1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 11
                 );

                 INSERT INTO gardener_proposal_instances(
                    id, proposal_fingerprint, source_commit, source_observation_id,
                    source_inspection_id, generation, implementation_obligation_id, created_at
                 ) VALUES (
                    'proposal-instance-1',
                    'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff',
                    'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1, 'inspection-1', 1,
                    'implementation-obligation', 11
                 );

                 INSERT INTO gardener_proposal_observation_instances(
                    observation_id, instance_id, mapped_at
                 ) VALUES (1, 'proposal-instance-1', 11);

                 INSERT INTO gardener_implementation_runs(
                    id, repository, proposal_fingerprint, obligation_id, occurrence,
                    attempt_number, lease_generation, lease_token, source_commit,
                    implementation_worktree_path, branch, phase, created_at, updated_at
                 ) VALUES (
                    'run-1', 'robchristie/bokkie',
                    'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff',
                    'implementation-obligation', 1, 1, 1, 'run-lease',
                    'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', '/tmp/run-1',
                    'codex/gardener-run-1', 'created', 12, 12
                 );

                 INSERT INTO gardener_implementation_run_instances(
                    run_id, instance_id, proposal_fingerprint, source_commit, generation, mapped_at
                 ) VALUES (
                    'run-1', 'proposal-instance-1',
                    'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff',
                    'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1, 12
                 );",
            )
            .unwrap();
    }

    fn insert_audit(connection: &Connection, event_type: &str, occurred_at: i64) {
        connection
            .execute(
                "INSERT INTO audit_events(
                    obligation_id, occurrence, event_type, occurred_at, to_state, details_json
                 ) VALUES ('implementation-obligation', 1, ?1, ?2,
                           'awaiting_approval', '{}')",
                params![event_type, occurred_at],
            )
            .unwrap();
    }

    fn insert_gardener(connection: &Connection, event_type: &str, occurred_at: i64) {
        connection
            .execute(
                "INSERT INTO gardener_events(
                    repository, inspection_id, proposal_fingerprint,
                    event_type, occurred_at, details_json
                 ) VALUES (
                    'robchristie/bokkie', 'inspection-1',
                    'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff',
                    ?1, ?2, '{}'
                 )",
                params![event_type, occurred_at],
            )
            .unwrap();
    }

    fn insert_run_event(connection: &Connection, event_type: &str, occurred_at: i64) {
        connection
            .execute(
                "INSERT INTO gardener_run_events(run_id, event_type, occurred_at, details_json)
                 VALUES ('run-1', ?1, ?2, '{}')",
                params![event_type, occurred_at],
            )
            .unwrap();
    }

    #[test]
    fn legacy_backfill_is_complete_deterministic_non_causal_and_survives_reopen() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("legacy-v8.sqlite");
        let connection = create_v8(&path);
        insert_domain_fixture(&connection);
        insert_run_event(&connection, "legacy-run", 20);
        insert_gardener(&connection, "legacy-gardener", 20);
        insert_audit(&connection, "legacy-audit-late", 30);
        insert_audit(&connection, "legacy-audit", 20);
        drop(connection);

        let store = Store::open(&path).unwrap();
        let page = store.change_page(0, None, 10).unwrap();
        assert_eq!(page.through, 4);
        assert_eq!(page.next_after, None);
        assert_eq!(
            page.items
                .iter()
                .map(|change| change.event_type.as_str())
                .collect::<Vec<_>>(),
            [
                "legacy-audit",
                "legacy-gardener",
                "legacy-run",
                "legacy-audit-late"
            ]
        );
        assert!(
            page.items
                .iter()
                .all(|change| { change.envelope.provenance == EventProvenance::LegacyNonCausal })
        );
        drop(store);

        let reopened = Store::open_compatible(&path).unwrap();
        assert_eq!(reopened.change_page(0, None, 10).unwrap(), page);
    }

    #[test]
    fn same_second_live_events_follow_authoritative_source_insertion_sequence() {
        let store = Store::open_in_memory().unwrap();
        insert_domain_fixture(&store.connection);
        insert_run_event(&store.connection, "run-first", 100);
        insert_audit(&store.connection, "audit-second", 100);
        insert_gardener(&store.connection, "gardener-third", 100);

        let page = store.change_page(0, None, 10).unwrap();
        assert_eq!(
            page.items
                .iter()
                .map(|change| change.event_type.as_str())
                .collect::<Vec<_>>(),
            ["run-first", "audit-second", "gardener-third"]
        );
        assert!(
            page.items
                .iter()
                .all(|change| change.envelope.provenance == EventProvenance::LiveAppend)
        );
        assert!(matches!(
            page.items[0].envelope.source,
            EventSource::GardenerRunEvent { sequence: 1 }
        ));
        assert!(matches!(
            page.items[1].envelope.source,
            EventSource::AuditEvent { sequence: 1 }
        ));
        assert!(matches!(
            page.items[2].envelope.source,
            EventSource::GardenerEvent { sequence: 1 }
        ));
    }

    #[test]
    fn changes_derive_scoped_obligation_proposal_and_run_identities() {
        let store = Store::open_in_memory().unwrap();
        insert_domain_fixture(&store.connection);
        insert_audit(&store.connection, "audit", 100);
        insert_gardener(&store.connection, "gardener", 101);
        insert_run_event(&store.connection, "run", 102);

        let items = store.change_page(0, None, 10).unwrap().items;
        for item in &items {
            assert_eq!(
                item.proposal_fingerprint.as_deref(),
                Some("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff")
            );
            assert_eq!(
                item.proposal_instance_id.as_deref(),
                Some("proposal-instance-1")
            );
        }
        assert_eq!(
            items[0].obligation_id.as_deref(),
            Some("implementation-obligation")
        );
        assert_eq!(items[0].run_id, None);
        assert_eq!(items[1].inspection_id.as_deref(), Some("inspection-1"));
        assert_eq!(items[2].run_id.as_deref(), Some("run-1"));
        assert_eq!(
            items[2].obligation_id.as_deref(),
            Some("implementation-obligation")
        );
    }

    #[test]
    fn envelope_rejects_orphans_duplicates_mismatches_and_mutation() {
        let store = Store::open_in_memory().unwrap();
        insert_domain_fixture(&store.connection);
        insert_audit(&store.connection, "audit", 100);

        let orphan = store.connection.execute(
            "INSERT INTO event_envelopes(
                provenance, source_kind, audit_event_sequence
             ) VALUES ('live_append', 'audit_event', 999)",
            [],
        );
        assert!(orphan.is_err());
        let duplicate = store.connection.execute(
            "INSERT INTO event_envelopes(
                provenance, source_kind, audit_event_sequence
             ) VALUES ('live_append', 'audit_event', 1)",
            [],
        );
        assert!(duplicate.is_err());
        let mismatch = store.connection.execute(
            "INSERT INTO event_envelopes(
                provenance, source_kind, audit_event_sequence
             ) VALUES ('live_append', 'gardener_event', 999)",
            [],
        );
        assert!(
            mismatch
                .unwrap_err()
                .to_string()
                .contains("CHECK constraint")
        );
        assert!(
            store
                .connection
                .execute("UPDATE event_envelopes SET provenance = provenance", [])
                .unwrap_err()
                .to_string()
                .contains("append-only")
        );
        assert!(
            store
                .connection
                .execute("DELETE FROM event_envelopes", [])
                .unwrap_err()
                .to_string()
                .contains("append-only")
        );
    }

    #[test]
    fn source_and_envelope_roll_back_as_one_transaction() {
        let store = Store::open_in_memory().unwrap();
        insert_domain_fixture(&store.connection);
        let transaction = store.connection.unchecked_transaction().unwrap();
        insert_audit(&transaction, "rolled-back", 100);
        assert_eq!(
            transaction
                .query_row("SELECT count(*) FROM event_envelopes", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        transaction.rollback().unwrap();
        assert_eq!(
            store
                .connection
                .query_row("SELECT count(*) FROM audit_events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(store.change_page(0, None, 10).unwrap().through, 0);
    }

    #[test]
    fn cursor_validation_distinguishes_invalid_ahead_and_gap_inputs() {
        let store = Store::open_in_memory().unwrap();
        insert_domain_fixture(&store.connection);
        insert_audit(&store.connection, "first", 100);

        assert!(matches!(
            store.change_page(0, None, 0),
            Err(StoreError::Invalid(message)) if message.contains("page limit")
        ));
        assert!(matches!(
            store.change_page(-1, None, 1),
            Err(StoreError::Invalid(message)) if message.contains("must not be negative")
        ));
        assert!(matches!(
            store.change_page(2, None, 1),
            Err(StoreError::ProjectionGap(message)) if message.contains("after watermark")
        ));

        store
            .connection
            .execute_batch("DROP TRIGGER audit_events_global_envelope;")
            .unwrap();
        insert_audit(&store.connection, "tenth", 101);
        store
            .connection
            .execute(
                "INSERT INTO event_envelopes(
                    sequence, provenance, source_kind, audit_event_sequence
                 ) VALUES (10, 'live_append', 'audit_event', 2)",
                [],
            )
            .unwrap();
        assert!(matches!(
            store.change_page(5, Some(10), 1),
            Err(StoreError::ProjectionGap(identity)) if identity.contains("cursor 5")
        ));
    }

    #[test]
    fn wal_writes_between_pages_do_not_cross_a_captured_watermark() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("wal-pages.sqlite");
        let reader = Store::open(&path).unwrap();
        let writer = Store::open_compatible(&path).unwrap();
        insert_domain_fixture(&reader.connection);
        for index in 1..=3 {
            insert_audit(&reader.connection, &format!("before-{index}"), 100);
        }

        let first = reader.change_page(0, None, 2).unwrap();
        assert_eq!(first.through, 3);
        assert_eq!(first.next_after, Some(2));
        insert_audit(&writer.connection, "after-4", 100);
        insert_audit(&writer.connection, "after-5", 100);

        let second = reader
            .change_page(first.next_after.unwrap(), Some(first.through), 2)
            .unwrap();
        assert_eq!(
            second
                .items
                .iter()
                .map(|item| item.envelope.sequence)
                .collect::<Vec<_>>(),
            [3]
        );
        let next_poll = reader.change_page(first.through, None, 2).unwrap();
        assert_eq!(next_poll.through, 5);
        assert_eq!(
            next_poll
                .items
                .iter()
                .map(|item| item.envelope.sequence)
                .collect::<Vec<_>>(),
            [4, 5]
        );
    }

    #[test]
    fn large_history_is_consumed_only_in_bounded_pages() {
        let store = Store::open_in_memory().unwrap();
        insert_domain_fixture(&store.connection);
        let transaction = store.connection.unchecked_transaction().unwrap();
        for index in 0..5_003 {
            insert_audit(&transaction, &format!("event-{index}"), 100);
        }
        transaction.commit().unwrap();

        let mut after = 0;
        let mut through = None;
        let mut seen = Vec::new();
        loop {
            let page = store.change_page(after, through, 137).unwrap();
            assert!(page.items.len() <= 137);
            through = Some(page.through);
            seen.extend(page.items.iter().map(|item| item.envelope.sequence));
            match page.next_after {
                Some(next) => after = next,
                None => break,
            }
        }
        assert_eq!(seen.len(), 5_003);
        assert_eq!(seen, (1..=5_003).collect::<Vec<_>>());
    }
}
