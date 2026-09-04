//! Immutable, append-only SQLite schema migration manifest.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::StoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationManifestEntry {
    pub version: i64,
    pub name: &'static str,
    pub sha256: &'static str,
    pub(crate) sql: &'static str,
}

pub fn migration_manifest() -> &'static [MigrationManifestEntry] {
    MIGRATIONS
}

pub(crate) const MIGRATIONS: &[MigrationManifestEntry] = &[
    MigrationManifestEntry {
        version: 1,
        name: "0001_obligation_kernel.sql",
        sha256: "8f90ef761cc47bfa6d48d9be6d13504231457764c8efc51d3cf65ba0bb337c66",
        sql: include_str!("../migrations/0001_obligation_kernel.sql"),
    },
    MigrationManifestEntry {
        version: 2,
        name: "0002_append_only_guards.sql",
        sql: include_str!("../migrations/0002_append_only_guards.sql"),
        sha256: "5fe0a4fed78e12fc11d722203cc678ba0e55a0d25abc48c5c0f34b4ad95284ec",
    },
    MigrationManifestEntry {
        version: 3,
        name: "0003_coding_gardener_state.sql",
        sql: include_str!("../migrations/0003_coding_gardener_state.sql"),
        sha256: "331c82be7c8b09e23eccceed3a41971c9cd2f54c29c36ae4e1bebbc812c73399",
    },
    MigrationManifestEntry {
        version: 4,
        name: "0004_coding_gardener_runs.sql",
        sql: include_str!("../migrations/0004_coding_gardener_runs.sql"),
        sha256: "b786240b81dc5ff615aa6f25e80249dda971bdd8001efc3bc04c21b0855e0f08",
    },
    MigrationManifestEntry {
        version: 5,
        name: "0005_gardener_trust_publication.sql",
        sql: include_str!("../migrations/0005_gardener_trust_publication.sql"),
        sha256: "1958764d5c8f9180a5bf13f2be66f46b8ab1e8034252aebe061c020f967f0b56",
    },
    MigrationManifestEntry {
        version: 6,
        name: "0006_source_bound_proposal_generations.sql",
        sql: include_str!("../migrations/0006_source_bound_proposal_generations.sql"),
        sha256: "17edad3e23b121895c919a9f22f3607b110765660a49a076f29a027f1083b26f",
    },
    MigrationManifestEntry {
        version: 7,
        name: "0007_immutable_migration_manifest.sql",
        sql: include_str!("../migrations/0007_immutable_migration_manifest.sql"),
        sha256: "df27eb40756ce50294e25a38c0e1ff54bcfbbe41cc448208123685bc52f98841",
    },
    MigrationManifestEntry {
        version: 8,
        name: "0008_typed_failure_dispositions.sql",
        sql: include_str!("../migrations/0008_typed_failure_dispositions.sql"),
        sha256: "36637c61b6f731274c0afa2302e93324d66fbeb17343da6a9e155092ac0ba17e",
    },
];

pub(crate) fn migrate(connection: &mut Connection) -> Result<(), StoreError> {
    validate_compiled_manifest()?;
    let applied = validate_prefix(connection, true)?;
    if applied.is_none() {
        connection.execute_batch(
            "CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                applied_at INTEGER NOT NULL DEFAULT (unixepoch())
            );",
        )?;
    }

    for migration in &MIGRATIONS[applied.unwrap_or(0)..] {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(migration.sql)?;
        if migration.version < 7 {
            transaction.execute(
                "INSERT INTO schema_migrations(version, name) VALUES (?1, ?2)",
                params![migration.version, migration.name],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO schema_migrations(version, name, sha256) VALUES (?1, ?2, ?3)",
                params![migration.version, migration.name, migration.sha256],
            )?;
        }
        transaction.commit()?;
    }

    validate_prefix(connection, false)?;
    Ok(())
}

pub(crate) fn validate_current(connection: &Connection) -> Result<(), StoreError> {
    validate_compiled_manifest()?;
    let applied = validate_prefix(connection, false)?;
    match applied {
        Some(count) if count == MIGRATIONS.len() => Ok(()),
        Some(count) => Err(StoreError::Invalid(format!(
            "database schema is at migration {count}, expected {}; startup migration is required",
            MIGRATIONS.len()
        ))),
        None => Err(StoreError::Invalid(
            "database has not been initialised; startup migration is required".to_owned(),
        )),
    }
}

/// Validate the exact contiguous applied prefix. With `allow_legacy`, a v1-v6
/// manifest without digests is accepted solely so migration 7 can bootstrap
/// the known hashes. No domain or audit row is rewritten by that bootstrap.
fn validate_prefix(
    connection: &Connection,
    allow_legacy: bool,
) -> Result<Option<usize>, StoreError> {
    let manifest_exists = connection
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'schema_migrations'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !manifest_exists {
        let other_tables: i64 = connection.query_row(
            "SELECT count(*) FROM sqlite_schema
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )?;
        if other_tables != 0 {
            return Err(StoreError::Invalid(
                "database contains tables but has no schema migration manifest".to_owned(),
            ));
        }
        return Ok(None);
    }

    let has_digest = {
        let mut statement = connection.prepare("PRAGMA table_info(schema_migrations)")?;
        let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
        columns
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|name| name == "sha256")
    };
    let sql = if has_digest {
        "SELECT version, name, sha256 FROM schema_migrations ORDER BY version"
    } else {
        "SELECT version, name, NULL FROM schema_migrations ORDER BY version"
    };
    let mut statement = connection.prepare(sql)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    if rows.len() > MIGRATIONS.len() {
        return Err(StoreError::Invalid(format!(
            "database schema has newer migration {}, but this build supports through {}",
            rows.last().map_or(0, |row| row.0),
            MIGRATIONS.len()
        )));
    }
    for (index, (version, name, digest)) in rows.iter().enumerate() {
        let expected = &MIGRATIONS[index];
        if *version != expected.version {
            return Err(StoreError::Invalid(format!(
                "migration manifest is not contiguous: found version {version} where {} was expected",
                expected.version
            )));
        }
        if name != expected.name {
            return Err(StoreError::Invalid(format!(
                "migration {version} is recorded as {name:?}, expected {:?}",
                expected.name
            )));
        }
        if has_digest && digest.as_deref() != Some(expected.sha256) {
            return Err(StoreError::Invalid(format!(
                "migration {version} digest does not match the immutable manifest"
            )));
        }
    }

    if has_digest && rows.len() < 7 {
        return Err(StoreError::Invalid(
            "migration digest column exists before the digest bootstrap migration".to_owned(),
        ));
    }
    if !has_digest && (!allow_legacy || rows.len() > 6) {
        return Err(StoreError::Invalid(
            "migration manifest has no immutable digest records".to_owned(),
        ));
    }
    Ok(Some(rows.len()))
}

fn sha256(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn validate_compiled_manifest() -> Result<(), StoreError> {
    for migration in MIGRATIONS {
        if sha256(migration.sql) != migration.sha256 {
            return Err(StoreError::Invalid(format!(
                "compiled migration {} does not match its immutable SHA-256 digest",
                migration.name
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use rusqlite::{Connection, params};
    use tempfile::TempDir;

    use super::*;
    use crate::Store;

    fn legacy_v6(path: &Path) -> Connection {
        let connection = Connection::open(path).unwrap();
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
        connection
    }

    #[test]
    fn fresh_database_records_exact_digests_and_reopens_without_migration() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("fresh.sqlite");
        drop(Store::open(&path).unwrap());
        let connection = Connection::open(&path).unwrap();
        let records = connection
            .prepare("SELECT version, name, sha256 FROM schema_migrations ORDER BY version")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(records.len(), MIGRATIONS.len());
        for (record, expected) in records.iter().zip(MIGRATIONS) {
            assert_eq!(record.0, expected.version);
            assert_eq!(record.1, expected.name);
            assert_eq!(record.2, expected.sha256);
            assert_eq!(record.2, sha256(expected.sql));
        }
        drop(connection);
        assert!(Store::open_compatible(path).is_ok());
    }

    #[test]
    fn compatible_open_requires_wal_without_changing_persistent_mode() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("non-wal.sqlite");
        drop(Store::open(&path).unwrap());
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "journal_mode", "DELETE")
            .unwrap();
        drop(connection);

        let error = match Store::open_compatible(&path) {
            Ok(_) => panic!("compatible open unexpectedly changed journal mode"),
            Err(error) => error,
        };
        assert!(matches!(error, StoreError::Invalid(message) if message.contains("journal mode")));
        let connection = Connection::open(path).unwrap();
        let journal_mode: String = connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "delete");
    }

    #[test]
    fn v6_digest_bootstrap_preserves_domain_and_audit_evidence() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("v6.sqlite");
        let connection = legacy_v6(&path);
        connection
            .execute(
                "INSERT INTO obligations(
                    id, description, state, occurrence, scheduled_at, next_wake_at,
                    approval_required, attempts_made, max_attempts, retry_base_seconds,
                    retry_max_seconds, lease_generation, created_at, updated_at
                 ) VALUES ('legacy', 'untouched', 'pending', 1, 10, 10, 0, 0, 3, 60,
                           3600, 0, 10, 10)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO audit_events(
                    obligation_id, occurrence, event_type, occurred_at, to_state, details_json
                 ) VALUES ('legacy', 1, 'created', 10, 'pending', '{\"legacy\":true}')",
                [],
            )
            .unwrap();
        drop(connection);

        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.get("legacy").unwrap().unwrap().description,
            "untouched"
        );
        assert_eq!(
            store.events("legacy").unwrap()[0].details_json,
            r#"{"legacy":true}"#
        );
        drop(store);
        assert!(Store::open_compatible(path).is_ok());
    }

    #[test]
    fn v8_backfills_only_unambiguous_legacy_failure_dispositions() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("v7.sqlite");
        let connection = legacy_v6(&path);
        connection.execute_batch(MIGRATIONS[6].sql).unwrap();
        connection
            .execute(
                "INSERT INTO schema_migrations(version, name, sha256) VALUES (7, ?1, ?2)",
                params![MIGRATIONS[6].name, MIGRATIONS[6].sha256],
            )
            .unwrap();
        for (id, state) in [
            ("retry", "retry_scheduled"),
            ("terminal", "attention"),
            ("expired", "attention"),
            ("ambiguous", "attention"),
            ("cancelled", "cancelled"),
        ] {
            connection
                .execute(
                    "INSERT INTO obligations(
                        id, description, state, occurrence, scheduled_at, next_wake_at,
                        approval_required, attempts_made, max_attempts, retry_base_seconds,
                        retry_max_seconds, lease_generation, created_at, updated_at
                     ) VALUES (?1, 'legacy', ?2, 1, 10,
                        CASE WHEN ?2 = 'retry_scheduled' THEN 20 ELSE NULL END,
                        0, 1, 3, 10, 60, 1, 10, 10)",
                    params![id, state],
                )
                .unwrap();
        }
        for (id, outcome, retryable) in [
            ("retry", "failed", Some(true)),
            ("terminal", "failed", Some(false)),
            ("expired", "lease_expired", Some(true)),
            ("ambiguous", "failed", None),
        ] {
            connection
                .execute(
                    "INSERT INTO attempts(
                        obligation_id, occurrence, attempt_number, lease_generation,
                        lease_token, claimed_at, completed_at, outcome, retryable
                     ) VALUES (?1, 1, 1, 1, ?1 || '-lease', 10, 11, ?2, ?3)",
                    params![id, outcome, retryable],
                )
                .unwrap();
        }
        drop(connection);

        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.get("retry").unwrap().unwrap().failure_disposition,
            Some(crate::FailureDisposition::RetrySafe)
        );
        assert_eq!(
            store.get("terminal").unwrap().unwrap().failure_disposition,
            Some(crate::FailureDisposition::Terminal)
        );
        assert_eq!(
            store.get("expired").unwrap().unwrap().failure_disposition,
            Some(crate::FailureDisposition::RetrySafe)
        );
        assert_eq!(
            store.get("ambiguous").unwrap().unwrap().failure_disposition,
            None
        );
        assert_eq!(
            store.get("cancelled").unwrap().unwrap().failure_disposition,
            Some(crate::FailureDisposition::Cancelled)
        );
        assert_eq!(
            store.attempts("ambiguous").unwrap()[0].failure_disposition,
            None
        );
    }

    fn remove_manifest_guards(connection: &Connection) {
        connection
            .execute_batch(
                "DROP TRIGGER schema_migrations_immutable_update;
                 DROP TRIGGER schema_migrations_immutable_delete;",
            )
            .unwrap();
    }

    #[test]
    fn incompatible_manifest_is_rejected_before_any_pending_migration_write() {
        for corruption in ["gap", "name", "digest", "newer"] {
            let directory = TempDir::new().unwrap();
            let path = directory.path().join(format!("{corruption}.sqlite"));
            drop(Store::open(&path).unwrap());
            let connection = Connection::open(&path).unwrap();
            remove_manifest_guards(&connection);
            match corruption {
                "gap" => {
                    connection
                        .execute("DELETE FROM schema_migrations WHERE version = 3", [])
                        .unwrap();
                }
                "name" => {
                    connection
                        .execute(
                            "UPDATE schema_migrations SET name = 'tampered.sql' WHERE version = 3",
                            [],
                        )
                        .unwrap();
                }
                "digest" => {
                    connection
                        .execute(
                            "UPDATE schema_migrations SET sha256 = ?1 WHERE version = 3",
                            ["0".repeat(64)],
                        )
                        .unwrap();
                }
                "newer" => {
                    connection
                        .execute(
                            "INSERT INTO schema_migrations(version, name, sha256)
                             VALUES (9, '0009_future.sql', ?1)",
                            ["9".repeat(64)],
                        )
                        .unwrap();
                }
                _ => unreachable!(),
            }
            connection
                .execute_batch("CREATE TABLE prevalidation_sentinel(value TEXT NOT NULL);")
                .unwrap();
            drop(connection);

            let error = match Store::open(&path) {
                Ok(_) => panic!("{corruption} manifest unexpectedly opened"),
                Err(error) => error,
            };
            assert!(
                matches!(error, StoreError::Invalid(_)),
                "{corruption}: {error}"
            );
            let connection = Connection::open(path).unwrap();
            assert_eq!(
                connection
                    .query_row("SELECT count(*) FROM prevalidation_sentinel", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap(),
                0
            );
        }
    }

    #[test]
    fn applied_manifest_rows_are_immutable() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrate(&mut connection).unwrap();
        for sql in [
            "UPDATE schema_migrations SET name = name WHERE version = 1",
            "DELETE FROM schema_migrations WHERE version = 1",
        ] {
            let error = connection.execute(sql, []).unwrap_err();
            assert!(error.to_string().contains("immutable"));
        }
    }
}
