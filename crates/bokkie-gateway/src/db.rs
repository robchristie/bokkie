//! The SQLite database and its migrations.

use std::path::Path;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

/// The migrations under `migrations/`, applied in order at startup.
pub static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Opens (creating if needed) the database at `path` and applies every
/// pending migration. The file is created private to the gateway's user.
pub async fn open(path: &Path) -> Result<SqlitePool, sqlx::Error> {
    if !path.exists() {
        create_private(path)?;
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await?;
    MIGRATIONS.run(&pool).await?;
    Ok(pool)
}

fn create_private(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map(drop)
}

/// Starts a write transaction. `BEGIN IMMEDIATE` takes the write lock at the
/// start, waiting out the busy timeout. A deferred transaction that reads
/// and then writes can instead fail at once (`SQLITE_BUSY_SNAPSHOT`) when
/// another connection wrote in between.
pub async fn write(
    db: &SqlitePool,
) -> Result<sqlx::Transaction<'static, sqlx::Sqlite>, sqlx::Error> {
    db.begin_with("BEGIN IMMEDIATE").await
}
